use rusqlite::Connection;
use super::types::*;
use super::inspect::load_pack_semantic_meta_for_codes;
use super::tree_assemble::{TrieNode, build_trie_subtree, count_nodes};
use super::utils::*;
use std::collections::{HashMap, HashSet};

pub fn query_taxonomy_tree_fast(
    conn: &Connection,
    locale: &str,
    root_filter: Option<&str>,
    master_conn: &Connection,
    include_files: bool,
    workspace_id: Option<i64>,
    directory_prefix: Option<&str>,
) -> anyhow::Result<TaxonomyTreeResponse> {
    let t_start = std::time::Instant::now();
    let lang = locale.split('-').next().unwrap_or("zh");

    // 1. 检查主库中表与列的存在性
    let has_ftr: bool = master_conn
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'file_tag_relations'",
            [],
            |_| Ok(true),
        )
        .unwrap_or(false);

    if !has_ftr {
        return Ok(TaxonomyTreeResponse {
            locale: locale.to_string(),
            root_nodes: Vec::new(),
            total_nodes: 0,
        });
    }

    let mut has_code_path = false;
    let mut has_name_path = false;
    if let Ok(mut col_stmt) = master_conn.prepare("PRAGMA table_info(file_tag_relations)") {
        if let Ok(rows) = col_stmt.query_map([], |r| r.get::<_, String>(1)) {
            for col in rows.flatten() {
                if col == "code_path" {
                    has_code_path = true;
                } else if col == "name_path" {
                    has_name_path = true;
                }
            }
        }
    }

    let has_wf: bool = master_conn
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'workspace_files'",
            [],
            |_| Ok(true),
        )
        .unwrap_or(false);

    let mut where_clauses = Vec::new();
    let mut params: Vec<rusqlite::types::Value> = Vec::new();

    if has_wf {
        where_clauses.push("wf.status = 1".to_string());
    }

    if let Some(ws_id) = workspace_id {
        if has_wf {
            where_clauses.push("wf.workspace_id = ?".to_string());
            params.push(rusqlite::types::Value::Integer(ws_id));
        }
    }

    if let Some(prefix) = directory_prefix {
        let clean_prefix = prefix.trim();
        if !clean_prefix.is_empty() && has_wf {
            #[cfg(windows)]
            let norm_prefix = clean_prefix.replace('/', "\\");
            #[cfg(not(windows))]
            let norm_prefix = clean_prefix.replace('\\', "/");

            let sep = if norm_prefix.contains('\\') { "\\" } else { "/" };
            let base = norm_prefix.trim_end_matches(['/', '\\']);
            let base_with_sep = format!("{}{}", base, sep);
            // 安全转义：使用 | 作为转义符，原样保留原生反斜杠 \ (Spec §3.1.1)
            let escaped_prefix = base_with_sep
                .replace('|', "||")
                .replace('%', "|%")
                .replace('_', "|_");
            let prefix_pattern = format!("{}%", escaped_prefix);

            where_clauses.push("(wf.path = ? OR wf.path LIKE ? ESCAPE '|')".to_string());
            params.push(rusqlite::types::Value::Text(base.to_string()));
            params.push(rusqlite::types::Value::Text(prefix_pattern));
        }
    }

    // Seam B · tag_paths 快路探测（工单08 · 消费方改造，只读端）：
    // 目标态下完整物化路径存微型字典 tag_paths.code_path（模型 B）。写入端 ensureTagPath 属独立
    // 「路径模型 A→B 迁移」工单，本函数仅做只读消费，不触碰写入端（避免两套并行写入）。
    //
    // 快路生效条件（三者同时满足）：① tag_paths 表存在；② 且有数据；③ file_tag_relations 含 `path_id` 列。
    // 生效时用 `LEFT JOIN tag_paths` 经 `ftr.path_id` 取 `tp.code_path`（混合/部分迁移行在 `has_code_path` 时回退 `ftr.code_path`），
    // 路径字典通常仅数千行，JOIN 代价远低于逐行重算路径；空表或无 `path_id` 列时安全回退模型 A。
    let tag_paths_populated: bool = master_conn
        .query_row(
            "SELECT CASE WHEN (SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='tag_paths') > 0 \
             THEN (SELECT COUNT(*) FROM tag_paths) > 0 ELSE 0 END",
            [],
            |r| r.get::<_, i64>(0),
        )
        .map(|v| v > 0)
        .unwrap_or(false);

    // 快路：tag_paths 有数据时，路径改由 JOIN tag_paths 经 ftr.path_id 取（模型 B）；
    // 回退：否则用 ftr.code_path 直读（模型 A）。ftr.path_id 在目标态为外键，存量库无此列时快路自然关闭。
    let use_tag_paths_fast = tag_paths_populated
        && master_conn
            .prepare("PRAGMA table_info(file_tag_relations)")
            .ok()
            .and_then(|mut s| {
                s.query_map([], |r| r.get::<_, String>(1))
                    .ok()
                    .map(|rows| rows.flatten().any(|c| c == "path_id"))
            })
            .unwrap_or(false);
    if use_tag_paths_fast {
        tracing::info!(
            "[TaxonomyTreeFast] tag_paths 快路生效（模型 B：ftr.path_id JOIN tag_paths.code_path）"
        );
    } else if tag_paths_populated {
        tracing::warn!(
            "[TaxonomyTreeFast] tag_paths 有数据但 file_tag_relations 缺 path_id 列，回退 code_path 直读（模型 A）"
        );
    }

    // 路径列表达式 + JOIN 子句：快路（模型 B）经 ftr.path_id LEFT JOIN tag_paths 取 code_path
    // （当混合库同时存在 code_path 列时优先 tp.code_path、否则回退 ftr.code_path，避免 path_id NULL 漏行）；
    // 纯模型 A 用 ftr.code_path 直读。name_path 在模型 B 的 ftr 中已移除，统一取 ''（展示名后续由 tag_aliases 解析）。
    let code_path_expr = if use_tag_paths_fast {
        if has_code_path {
            "COALESCE(tp.code_path, ftr.code_path, '')".to_string()
        } else {
            "COALESCE(tp.code_path, '')".to_string()
        }
    } else if has_code_path {
        "ftr.code_path".to_string()
    } else {
        "''".to_string()
    };
    let name_path_expr = if use_tag_paths_fast || !has_name_path {
        "''".to_string()
    } else {
        "ftr.name_path".to_string()
    };
    let tag_paths_join = if use_tag_paths_fast {
        " LEFT JOIN tag_paths tp ON tp.path_id = ftr.path_id "
    } else {
        " "
    };

    // Seam B · !has_wf 分支 rowid 修复：
    // 旧实现取 `ftr.rowid`（file_tag_relations 主键含 tag_code/via_parent_code → 同一文件多 tag
    // 产生多行不同 rowid），使 RoaringBitmap 按「关系行」计数而非按「文件」计数，导致 file_count 虚高。
    // 当主库存在 files 表时 JOIN files 取 `f.rowid`（文件级唯一），与 has_wf 分支的 `wf.rowid` 语义对齐；
    // 仅当精简测试夹具既无 workspace_files 又无 files 表时 warn 后回退 `ftr.rowid`。
    let has_files: bool = !has_wf && table_exists(master_conn, "files");
    if !has_wf && !has_files {
        tracing::warn!(
            "[TaxonomyTreeFast] 主库缺少 workspace_files 与 files 表，回退 ftr.rowid（仅兼容精简测试夹具）"
        );
    }
    let select_sql = if has_wf && !where_clauses.is_empty() {
        format!(
            "SELECT DISTINCT wf.rowid, ftr.file_fingerprint, ftr.tag_code, {}, {} \
             FROM file_tag_relations ftr{}\
             JOIN workspace_files wf ON wf.file_fingerprint = ftr.file_fingerprint \
             WHERE {}",
            code_path_expr,
            name_path_expr,
            tag_paths_join,
            where_clauses.join(" AND ")
        )
    } else if has_wf {
        format!(
            "SELECT DISTINCT wf.rowid, ftr.file_fingerprint, ftr.tag_code, {}, {} \
             FROM file_tag_relations ftr{}\
             JOIN workspace_files wf ON wf.file_fingerprint = ftr.file_fingerprint",
            code_path_expr,
            name_path_expr,
            tag_paths_join
        )
    } else if has_files {
        format!(
            "SELECT DISTINCT f.rowid, ftr.file_fingerprint, ftr.tag_code, {}, {} \
             FROM file_tag_relations ftr{}\
             JOIN files f ON f.file_fingerprint = ftr.file_fingerprint",
            code_path_expr,
            name_path_expr,
            tag_paths_join
        )
    } else {
        format!(
            "SELECT ftr.rowid, ftr.file_fingerprint, ftr.tag_code, {}, {} \
             FROM file_tag_relations ftr{}",
            code_path_expr,
            name_path_expr,
            tag_paths_join
        )
    };

    let mut stmt = master_conn.prepare(&select_sql)?;
    let param_refs: Vec<&dyn rusqlite::ToSql> = params.iter().map(|p| p as &dyn rusqlite::ToSql).collect();
    let rows = stmt.query_map(param_refs.as_slice(), |row| {
        let raw_id: i64 = row.get(0)?;
        let fp: String = row.get(1)?;
        let tag_code: String = row.get(2)?;
        let code_path: String = row.get(3).unwrap_or_default();
        let name_path: String = row.get(4).unwrap_or_default();
        Ok((raw_id, fp, tag_code, code_path, name_path))
    })?;

    let mut node_map: HashMap<String, TrieNode> = HashMap::new();

    // 2. 遍历活跃记录，自顶向下构建前缀树 Trie
    for r in rows.flatten() {
        let (raw_id, fp, tag_code, raw_code_path, raw_name_path) = r;
        if fp.trim().is_empty() || tag_code.trim().is_empty() {
            continue;
        }

        let code_content_tags = omni_core::concepts::Concept::内容标签.code();
        let code_file_type = omni_core::concepts::Concept::文件类型.code();
        let code_lang_seg = omni_core::concepts::Concept::语言细分.code();
        let code_sec_level = omni_core::concepts::Concept::安全等级.code();
        let code_proc_status = omni_core::concepts::Concept::处理状态.code();
        let code_photo_cat = omni_core::concepts::Concept::摄影照片细分.code();
        let code_text_density = omni_core::concepts::Concept::文字密度.code();
        let code_geo_location = omni_core::concepts::Concept::地理位置.code();
        let code_author = omni_core::concepts::Concept::作者.code();

        let name_content_tags = omni_core::concepts::Concept::内容标签.zh_name();
        let name_geo_location = omni_core::concepts::Concept::地理位置.zh_name();
        let name_author = omni_core::concepts::Concept::作者.zh_name();
        let name_file_type = omni_core::concepts::Concept::文件类型.zh_name();
        let name_document = omni_core::concepts::Concept::文档.zh_name();

        // 悲观数据兜底 (M-3)：若 code_path 为空，回退虚拟挂载至 /{code_content_tags}/{tag_code}
        let (eff_code_path, eff_name_path) = if raw_code_path.trim().is_empty() {
            (
                format!("/{}/{}", code_content_tags, tag_code),
                format!("/{}/{}", name_content_tags, tag_code),
            )
        } else {
            (raw_code_path, raw_name_path)
        };

        let mut code_segs: Vec<String> = eff_code_path
            .split('/')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();

        let mut name_segs: Vec<String> = eff_name_path
            .split('/')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();

        if code_segs.is_empty() {
            code_segs.push(code_content_tags.to_string());
            code_segs.push(tag_code.clone());
            name_segs.push(name_content_tags.to_string());
            name_segs.push(tag_code.clone());
        }

        // 历史驼峰归一化兼容
        if code_segs[0] == "builtin.fileType" {
            code_segs[0] = code_file_type.to_string();
        }

        // 历史兼并副码自愈归一（ISS-01）：将历史 omw.06367879.n 直接归入正统受控码 omw.03833065.n
        for seg in &mut code_segs {
            if seg == "omw.06367879.n" { // lint-ignore: raw-code
                *seg = "omw.03833065.n".to_string(); // lint-ignore: raw-code
            }
        }

        // 作者受控码统一展示名为正统受控名
        for (i, seg) in code_segs.iter_mut().enumerate() {
            if seg == code_author {
                if i < name_segs.len() && (name_segs[i].is_empty() || name_segs[i] == "未归类") {
                    name_segs[i] = name_author.to_string();
                }
            }
        }

        // 拓扑防环（ISS-01）：若路径中出现重复节点（如 A/.../A 隔代同码回环），截除回环冗余段
        let mut seen_codes = HashSet::new();
        let mut dedup_code_segs = Vec::new();
        let mut dedup_name_segs = Vec::new();
        for (idx, seg) in code_segs.into_iter().enumerate() {
            if seen_codes.insert(seg.clone()) {
                dedup_code_segs.push(seg);
                if idx < name_segs.len() {
                    dedup_name_segs.push(name_segs[idx].clone());
                }
            }
        }
        code_segs = dedup_code_segs;
        name_segs = dedup_name_segs;

        // 历史存量数据自愈：若前缀误被包装为 /{code_content_tags}/<subdim>/... 或 /{code_content_tags}/<root_dim>/...，
        // 门禁：只要第二段为受控维度代码（如摄影照片细分、文字密度、语言细分等）或一级根维度代码，绝对严禁作为 content_tags 的子节点，直接剥除伪 content_tags 前缀！
        if code_segs.len() > 1 && code_segs[0] == code_content_tags {
            let is_subdim_or_root = omni_core::is_root_dimension(&code_segs[1])
                || omni_core::get_dimension_policy(&code_segs[1]).is_some()
                || omni_core::tag_identity::concept_from_code(&code_segs[1])
                    .map_or(false, |c_concept| omni_core::get_dimension_policy(c_concept.code()).is_some())
                || code_segs[1] == code_lang_seg
                || code_segs[1] == "builtin.language_segmentation"
                || code_segs[1] == code_sec_level
                || code_segs[1] == "builtin.security_level"
                || code_segs[1] == code_proc_status
                || code_segs[1] == "builtin.processing_status"
                || code_segs[1] == code_photo_cat
                || code_segs[1] == code_text_density;
            if is_subdim_or_root {
                code_segs.remove(0);
                if !name_segs.is_empty() {
                    name_segs.remove(0);
                }
            }
        }

        // 历史错误拓扑自愈：地理位置为一级独立根维度，若历史脏数据中被误挂在其他维度（如文件类型->图片->地理位置），
        // 自动切除前缀，将地理位置提升为根节点
        if let Some(pos) = code_segs.iter().position(|s| s == code_geo_location) {
            if pos > 0 {
                code_segs.drain(0..pos);
                if let Some(name_pos) = name_segs.iter().position(|s| s == name_geo_location) {
                    name_segs.drain(0..name_pos);
                } else if name_segs.len() > pos {
                    name_segs.drain(0..pos);
                }
            }
        }

        // 历史错误拓扑自愈：作者为一级独立根维度，若历史脏数据中被误挂在其他维度（如内容标签->人->作者 或 文件类型->作者），
        // 自动切除前缀，将作者提升为根节点
        if let Some(pos) = code_segs.iter().position(|s| s == code_author) {
            if pos > 0 {
                code_segs.drain(0..pos);
                if let Some(name_pos) = name_segs.iter().position(|s| s == name_author || s == "作者") {
                    name_segs.drain(0..name_pos);
                } else if name_segs.len() > pos {
                    name_segs.drain(0..pos);
                }
            }
        }

        // 历史截断短路径自愈：子维度若缺失顶层祖先，自动根据本体拓扑补齐真实父级路径
        if code_segs[0] == code_lang_seg || code_segs[0] == "builtin.language_segmentation" {
            // 语言细分属于 文件类型 -> 文档
            code_segs.insert(0, "builtin.document".to_string());
            code_segs.insert(0, code_file_type.to_string());
            if !name_segs.is_empty() {
                name_segs.insert(0, name_document.to_string());
                name_segs.insert(0, name_file_type.to_string());
            }
        } else if code_segs[0] == code_sec_level
            || code_segs[0] == "builtin.security_level"
            || code_segs[0] == code_proc_status
            || code_segs[0] == "builtin.processing_status"
        {
            // 安全等级 / 处理状态属于 文件用途 -> 工作文档
            code_segs.insert(0, omni_core::concepts::Concept::工作文档.code().to_string());
            code_segs.insert(0, omni_core::concepts::Concept::文件用途.code().to_string());
            if !name_segs.is_empty() {
                name_segs.insert(0, "工作文档".to_string());
                name_segs.insert(0, "文件用途".to_string());
            }
        }

        // 受控根维度封闭性与排他门禁 (Spec v2.0 不变式 1 & 不变式 2)
        if !omni_core::is_root_dimension(&code_segs[0]) && code_segs[0] != code_content_tags {
            let is_subdim = omni_core::get_dimension_policy(&code_segs[0]).is_some()
                || omni_core::tag_identity::concept_from_code(&code_segs[0])
                    .map_or(false, |c_concept| omni_core::get_dimension_policy(c_concept.code()).is_some())
                || code_segs[0] == code_photo_cat
                || code_segs[0] == code_text_density;
            if is_subdim {
                // 已归入受控细分体系，排他门禁生效：严禁强插内容标签！
                // 前置补齐所属文件类型一级根
                let root_code = code_file_type.to_string();
                let root_name = "文件类型".to_string();
                code_segs.insert(0, root_code);
                name_segs.insert(0, root_name);
            } else {
                // 仅对非受控的开放自由标签或未声明父级的受控泛标签，收敛兜底至内容标签
                code_segs.insert(0, code_content_tags.to_string());
                name_segs.insert(0, "内容标签".to_string());
            }
        }

        // 自顶向下装配路径节点
        let seg_len = code_segs.len();
        for i in 0..seg_len {
            let seg_code = code_segs[i].clone();
            let seg_name = if i < name_segs.len() && !name_segs[i].is_empty() {
                name_segs[i].clone()
            } else {
                seg_code.clone()
            };
            let curr_code_path = format!("/{}", code_segs[0..=i].join("/"));
            let curr_name_path = if i < name_segs.len() {
                format!("/{}", name_segs[0..=i].join("/"))
            } else {
                curr_code_path.clone()
            };
            let parent_code_path = if i > 0 {
                Some(format!("/{}", code_segs[0..i].join("/")))
            } else {
                None
            };

            let source = if seg_code.starts_with("builtin.") {
                "builtin".to_string()
            } else if seg_code.starts_with("omw.") {
                "omw".to_string()
            } else if seg_code.starts_with("_ext.") {
                "_ext".to_string()
            } else {
                "user".to_string()
            };

            let sort_order = if seg_code == omni_core::concepts::Concept::内容标签.code() {
                9999
            } else if seg_code == omni_core::concepts::Concept::文件类型.code() {
                1
            } else if seg_code == omni_core::concepts::Concept::文件用途.code() {
                2
            } else if seg_code == omni_core::concepts::Concept::文件来源.code() {
                3
            } else if seg_code == code_author {
                4
            } else {
                0
            };

            // 检查或插入当前路径节点 (以物化 code_path 为键保证前缀树各分支绝对独立)
            if !node_map.contains_key(&curr_code_path) {
                node_map.insert(
                    curr_code_path.clone(),
                    TrieNode {
                        code: seg_code.clone(),
                        name: seg_name,
                        code_path: curr_code_path.clone(),
                        name_path: curr_name_path,
                        parent_path: parent_code_path.clone(),
                        source,
                        sort_order,
                        direct_files: HashSet::new(),
                        direct_bitmap: roaring::RoaringBitmap::new(),
                        children_paths: Vec::new(),
                        pos: None,
                        lexfile: None,
                        sememe_category: None,
                    },
                );
            }

            // 父节点维护子节点路径列表
            if let Some(ref p_path) = parent_code_path {
                if let Some(p_node) = node_map.get_mut(p_path) {
                    if !p_node.children_paths.contains(&curr_code_path) {
                        p_node.children_paths.push(curr_code_path.clone());
                    }
                }
            }

            // 【捍卫真实父与逻辑父文件计数公理 (M-1)】
            // 中间祖先节点 (逻辑父) direct_files 初始化为 ∅
            // 唯有路径末级命中节点才累加 direct_files
            if i == seg_len - 1 {
                if let Some(target_node) = node_map.get_mut(&curr_code_path) {
                    if raw_id >= 0 && raw_id <= (u32::MAX as i64) {
                        target_node.direct_bitmap.insert(raw_id as u32);
                    } else {
                        tracing::error!("[Omni] 检测到非法文件整型 ID: {}", raw_id);
                    }
                    if include_files {
                        target_node.direct_files.insert(fp.clone());
                    }
                }
            }
        }
    }

    if node_map.is_empty() {
        return Ok(TaxonomyTreeResponse {
            locale: locale.to_string(),
            root_nodes: Vec::new(),
            total_nodes: 0,
        });
    }

    let all_codes: Vec<String> = {
        let mut s = HashSet::new();
        for n in node_map.values() {
            s.insert(n.code.clone());
        }
        s.into_iter().collect()
    };

    // 2.5 按需回填语义包 file_tags 元数据（sort_order, pos, lexfile, sememe_category）：
    // 性能优化：仅对当前树出现的 all_codes 通过 PRIMARY KEY B-Tree 索引批量点查，耗时 < 0.1ms；
    // 特例：builtin.content_tags 维持硬编码 9999 恒定垫底，不被 pack 值覆盖。
    let pack_semantic_meta = load_pack_semantic_meta_for_codes(conn, &all_codes);
    for node in node_map.values_mut() {
        if let Some(meta) = pack_semantic_meta.get(&node.code) {
            if node.code != omni_core::concepts::Concept::内容标签.code() && meta.sort_order > 0 {
                node.sort_order = meta.sort_order;
            }
            node.pos = meta.pos.clone();
            node.lexfile = meta.lexfile.clone();
            node.sememe_category = meta.sememe_category;
        }
        // 若数据库未直接提供 pos，从 code 后缀做权威词性推导
        if node.pos.is_none() {
            if node.code.starts_with("omw.") {
                if node.code.ends_with(".v") {
                    node.pos = Some("v".to_string());
                } else if node.code.ends_with(".n") {
                    node.pos = Some("n".to_string());
                } else if node.code.ends_with(".a") || node.code.ends_with(".s") {
                    node.pos = Some("a".to_string());
                } else if node.code.ends_with(".r") {
                    node.pos = Some("r".to_string());
                }
            } else if node.code.starts_with("hownet.") && node.code.ends_with(".n") {
                node.pos = Some("n".to_string());
            }
        }
    }

    // 3. 按需查词 (P95 <= 5ms)：仅对 Trie 树涉及的活跃节点批量向 tag_aliases_{lang} 查词
    let mut resolved_names: HashMap<String, String> = HashMap::new();
    let target_table = omni_pro::resolve_tag_aliases_table(locale);
    let has_target_table = table_exists(conn, target_table);

    if has_target_table && !all_codes.is_empty() {
        for chunk in all_codes.chunks(200) {
            let placeholders = vec!["?"; chunk.len()].join(",");
            let sql = format!(
                "SELECT tag_code, lemma FROM {target_table} WHERE is_canonical = 1 AND tag_code IN ({placeholders})"
            );
            if let Ok(mut stmt) = conn.prepare(&sql) {
                let params: Vec<&dyn rusqlite::ToSql> = chunk.iter().map(|c| c as &dyn rusqlite::ToSql).collect();
                if let Ok(rows) = stmt.query_map(params.as_slice(), |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                }) {
                    for (c, l) in rows.flatten() {
                        resolved_names.insert(c, l);
                    }
                }
            }
        }
    }

    // 5. 顶层根节点过滤与排序输出（节点结构转换与子树装配委托 tree_assemble）
    let mut roots = Vec::new();
    let mut visited = HashSet::new();

    let root_paths: Vec<String> = node_map
        .values()
        .filter(|n| n.parent_path.is_none())
        .map(|n| n.code_path.clone())
        .collect();

    if let Some(target_root) = root_filter {
        let target_norm = if target_root.starts_with('/') {
            target_root.to_string()
        } else {
            format!("/{}", target_root)
        };
        for r_path in &root_paths {
            if let Some(r_node) = node_map.get(r_path) {
                if r_node.code == target_root || r_node.code_path == target_norm {
                    if let Some((root_nodes, _, _)) = build_trie_subtree(
                        r_path,
                        &node_map,
                        &resolved_names,
                        lang,
                        include_files,
                        false,
                        true, // is_root = true
                        &mut visited,
                    ) {
                        for (root_node, _) in root_nodes {
                            if !root_node.children.is_empty() || root_node.file_count > 0 {
                                roots.push(root_node);
                            }
                        }
                    }
                }
            }
        }
    } else {
        let mut sorted_root_paths = root_paths;
        sorted_root_paths.sort_by(|a, b| {
            let node_a = node_map.get(a);
            let node_b = node_map.get(b);
            let order_a = node_a.map(|n| if n.sort_order > 0 { n.sort_order } else { i64::MAX }).unwrap_or(i64::MAX);
            let order_b = node_b.map(|n| if n.sort_order > 0 { n.sort_order } else { i64::MAX }).unwrap_or(i64::MAX);
            if order_a != order_b {
                order_a.cmp(&order_b)
            } else {
                a.cmp(b)
            }
        });

        for r_path in sorted_root_paths {
            if let Some((root_nodes, _, _)) = build_trie_subtree(
                &r_path,
                &node_map,
                &resolved_names,
                lang,
                include_files,
                false,
                true, // is_root = true
                &mut visited,
            ) {
                for (root_node, _) in root_nodes {
                    if !root_node.children.is_empty() || root_node.file_count > 0 {
                        roots.push(root_node);
                    }
                }
            }
        }
    }

        // 顶层根节点排序：sort_order (>0 升序) 为主判据，其次 ROOT_DIMENSION_CONCEPTS 规范概念序，再次 code 字典序。
        // 主判据与回退路径 (L1300 一带) 及 desktop 服务端 (TagTreeQuery) 的 `order > 0` 语义保持一致。
        roots.sort_by(|a, b| {
            let root_rank = |code: &str| -> usize {
                omni_core::ROOT_DIMENSION_CONCEPTS
                    .iter()
                    .position(|concept_item| concept_item.code() == code)
                    .unwrap_or(usize::MAX)
            };
            let order_a = if a.sort_order > 0 { a.sort_order } else { i64::MAX };
            let order_b = if b.sort_order > 0 { b.sort_order } else { i64::MAX };
            if order_a != order_b {
                order_a.cmp(&order_b)
            } else {
                let rank_a = root_rank(&a.code);
                let rank_b = root_rank(&b.code);
                if rank_a != rank_b {
                    rank_a.cmp(&rank_b)
                } else {
                    a.code.cmp(&b.code)
                }
            }
        });

    let total_nodes = count_nodes(&roots);
    let elapsed = t_start.elapsed();
    tracing::info!(
        "[TaxonomyTreeFast] 倒排 Trie 树装配完成: roots={}, total_nodes={}, 耗时={:?}",
        roots.len(),
        total_nodes,
        elapsed
    );

    Ok(TaxonomyTreeResponse {
        locale: locale.to_string(),
        root_nodes: roots,
        total_nodes,
    })
}
