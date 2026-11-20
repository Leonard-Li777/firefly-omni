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

    // 1. 模型 B 唯一路径：物化路径已外迁至微型字典 tag_paths，file_tag_relations 仅保留 4B 外键 path_id。
    // 热路径不再做 PRAGMA table_info(file_tag_relations) / sqlite_master 模型探测（Spec §2.5.3）。
    // 缺 tag_paths 表属 schema 缺失，必须 tracing::warn! 显式上报并返回错误，严禁静默吞异常降级为空树。
    if !table_exists(master_conn, "tag_paths") {
        tracing::warn!(
            "[TaxonomyTreeFast] 业务主库缺少 tag_paths 表（模型 B 唯一路径字典），无法建树"
        );
        anyhow::bail!("[TaxonomyTreeFast] 业务主库缺少 tag_paths 表（模型 B 必需）");
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

    // Seam B · 模型 B 唯一路径收敛：物化路径一律经 `ftr.path_id JOIN tag_paths tp` 取 `tp.code_path`。
    // 已彻底删除模型 A 的 has_code_path / has_name_path 分支、ftr.code_path 直读与
    // COALESCE(ftr.code_path) 兜底；展示名改由「语义包 tag_aliases_{lang} + 业务主库
    // file_tags_private」按 code_path 段序实时回译（不依赖已删除的 ftr.name_path）。

    // Seam B · 模型 B 清 rowid（防 WITHOUT ROWID 报错 & VACUUM 错位）：
    // has_wf 分支取 wf.id（文件级唯一）；!has_wf 分支强制 JOIN files 取 f.id，与 has_wf 语义对齐。
    // 模型 B 的 file_tag_relations 为 WITHOUT ROWID，ftr.rowid 物理不存在，故彻底删除该回退分支，
    // 主库既无 workspace_files 又无 files 时显式 warn + 返回错误（禁静默吞）。
    if !has_wf && !table_exists(master_conn, "files") {
        tracing::warn!(
            "[TaxonomyTreeFast] 主库缺少 workspace_files 与 files 表，无法取文件级 id（模型 B 禁用 ftr.rowid）"
        );
        anyhow::bail!("[TaxonomyTreeFast] 主库缺少 workspace_files 与 files 表（模型 B 必需）");
    }

    // 统一列序：path_id, code_path, 文件级 id, file_fingerprint。
    // 仅对唯一 path_id 建 Trie，故不再逐行取 tag_code（由 code_path 末段即 leaf_code 推导）。
    let select_sql = if has_wf && !where_clauses.is_empty() {
        format!(
            "SELECT ftr.path_id, tp.code_path, wf.id, ftr.file_fingerprint \
             FROM file_tag_relations ftr \
             JOIN tag_paths tp ON tp.path_id = ftr.path_id \
             JOIN workspace_files wf ON wf.file_fingerprint = ftr.file_fingerprint \
             WHERE {}",
            where_clauses.join(" AND ")
        )
    } else if has_wf {
        format!(
            "SELECT ftr.path_id, tp.code_path, wf.id, ftr.file_fingerprint \
             FROM file_tag_relations ftr \
             JOIN tag_paths tp ON tp.path_id = ftr.path_id \
             JOIN workspace_files wf ON wf.file_fingerprint = ftr.file_fingerprint"
        )
    } else {
        format!(
            "SELECT ftr.path_id, tp.code_path, f.id, ftr.file_fingerprint \
             FROM file_tag_relations ftr \
             JOIN tag_paths tp ON tp.path_id = ftr.path_id \
             JOIN files f ON f.file_fingerprint = ftr.file_fingerprint"
        )
    };

    let mut stmt = master_conn.prepare(&select_sql)?;
    let param_refs: Vec<&dyn rusqlite::ToSql> = params.iter().map(|p| p as &dyn rusqlite::ToSql).collect();
    let rows = stmt.query_map(param_refs.as_slice(), |row| {
        let path_id: i64 = row.get(0)?;
        let code_path: String = row.get(1)?;
        let raw_id: i64 = row.get(2)?;
        let fp: String = row.get(3)?;
        Ok((path_id, code_path, raw_id, fp))
    })?;

    // 2.1 按唯一 path_id 预聚合（模型 B 核心提速）：几十万关系行先压缩为数千个唯一 path_id，
    // 每行的文件级 id 并入对应 RoaringBitmap，file_fingerprint 单独保序收集（供 include_files 输出）。
    let mut path_agg: HashMap<i64, (String, roaring::RoaringBitmap)> = HashMap::new();
    let mut path_fps: HashMap<i64, HashSet<String>> = HashMap::new();
    let mut all_path_codes: HashSet<String> = HashSet::new();
    for r in rows.flatten() {
        let (path_id, code_path, raw_id, fp) = r;
        if fp.trim().is_empty() {
            continue;
        }
        let entry = path_agg
            .entry(path_id)
            .or_insert_with(|| (code_path.clone(), roaring::RoaringBitmap::new()));
        if raw_id >= 0 && raw_id <= (u32::MAX as i64) {
            entry.1.insert(raw_id as u32);
        } else {
            tracing::error!("[Omni] 检测到非法文件整型 ID（path_id={}）: {}", path_id, raw_id);
        }
        path_fps.entry(path_id).or_default().insert(fp);
        for seg in code_path.split('/').filter(|s| !s.is_empty()) {
            all_path_codes.insert(seg.to_string());
        }
    }

    // 2.2 展示名映射（三层级联，对齐 alias_query.rs slice 03 契约，杜绝向 UI 暴露裸 omw.*/hownet.* code）：
    //   ① 目标语言分表 tag_aliases_{locale}（is_canonical 优先，次选同表高频词形）
    //   ② 业务主库 file_tags_private（非受控 _ext.*/user.* 兜底）
    //   ③ 英文母本 tag_aliases_en_US 展示级联（仅对①②仍未命中的 code 生效，避免 OMW 上位词缺中译时露出裸 code）
    let mut resolved_names: HashMap<String, String> = HashMap::new();
    let target_table = omni_pro::resolve_tag_aliases_table(locale);
    let all_path_codes: Vec<String> = all_path_codes.into_iter().collect();
    if !all_path_codes.is_empty() {
        let fill_from_alias_table = |table_name: &str,
                                     codes: &[String],
                                     out: &mut HashMap<String, String>| {
            if codes.is_empty() || !table_exists(conn, table_name) {
                return;
            }
            for chunk in codes.chunks(200) {
                let placeholders = vec!["?"; chunk.len()].join(",");
                let sql = format!(
                    "SELECT tag_code, lemma FROM {table_name} \
                     WHERE tag_code IN ({placeholders}) \
                     ORDER BY is_canonical DESC, count DESC, lemma ASC"
                );
                match conn.prepare(&sql) {
                    Ok(mut stmt) => {
                        let params: Vec<&dyn rusqlite::ToSql> =
                            chunk.iter().map(|c| c as &dyn rusqlite::ToSql).collect();
                        match stmt.query_map(params.as_slice(), |row| {
                            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                        }) {
                            Ok(rows) => {
                                for (c, l) in rows.flatten() {
                                    if !l.trim().is_empty() {
                                        out.entry(c).or_insert(l);
                                    }
                                }
                            }
                            Err(e) => {
                                tracing::warn!(
                                    "[TaxonomyTreeFast] 查询展示名分表 {table_name} 失败: {e}"
                                );
                            }
                        }
                    }
                    Err(e) => {
                        tracing::warn!(
                            "[TaxonomyTreeFast] 预编译展示名分表 {table_name} SQL 失败: {e}"
                        );
                    }
                }
            }
        };

        // ① 目标语言分表
        fill_from_alias_table(target_table, &all_path_codes, &mut resolved_names);

        // ② 展示级联兜底：目标语言缺失词条时优先回退语义包 en-US 母本，杜绝 UI 暴露裸 omw.* / hownet.* 代码
        //    （置身于 file_tags_private 之前，防止历史脏行把裸 code 写入 file_tags_private.name 截胡 en-US 兜底）
        const EN_ALIAS_TABLE: &str = "tag_aliases_en_US";
        if target_table != EN_ALIAS_TABLE {
            let missing_after_target: Vec<String> = all_path_codes
                .iter()
                .filter(|c| !resolved_names.contains_key(*c))
                .cloned()
                .collect();
            fill_from_alias_table(EN_ALIAS_TABLE, &missing_after_target, &mut resolved_names);
        }

        // ③ 非受控 _ext.* / user.* 展示名统一查 file_tags_private（不依赖已删除的 ftr.name_path）
        let still_missing: Vec<String> = all_path_codes
            .iter()
            .filter(|c| !resolved_names.contains_key(*c))
            .cloned()
            .collect();
        if !still_missing.is_empty() && table_exists(master_conn, "file_tags_private") {
            for chunk in still_missing.chunks(200) {
                let placeholders = vec!["?"; chunk.len()].join(",");
                let sql = format!(
                    "SELECT code, name FROM file_tags_private WHERE code IN ({placeholders})"
                );
                match master_conn.prepare(&sql) {
                    Ok(mut stmt) => {
                        let params: Vec<&dyn rusqlite::ToSql> =
                            chunk.iter().map(|c| c as &dyn rusqlite::ToSql).collect();
                        match stmt.query_map(params.as_slice(), |row| {
                            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                        }) {
                            Ok(rows) => {
                                for (c, n) in rows.flatten() {
                                    let trimmed = n.trim();
                                    if !trimmed.is_empty() && trimmed != c {
                                        resolved_names
                                            .entry(c)
                                            .or_insert_with(|| trimmed.to_string());
                                    }
                                }
                            }
                            Err(e) => {
                                tracing::warn!(
                                    "[TaxonomyTreeFast] 查询 file_tags_private 展示名失败: {e}"
                                );
                            }
                        }
                    }
                    Err(e) => {
                        tracing::warn!(
                            "[TaxonomyTreeFast] 预编译 file_tags_private 查询失败: {e}"
                        );
                    }
                }
            }
        }
    }

    let mut node_map: HashMap<String, TrieNode> = HashMap::new();

    // 2. 仅对唯一 path_id 各做一次 code_path.split('/') 建 Trie（模型 B 核心提速）：
    // path_agg 已把几十万关系行压成数千条唯一 path_id，逐行切串开销从 O(关系行数) 降为 O(唯一 path_id 数)。
    // 按 path_id 升序保证装配顺序确定（与写入顺序一致，避免 HashMap 随机序影响同码辩论归属）。
    let mut agg_entries: Vec<_> = path_agg.iter().collect();
    agg_entries.sort_by_key(|(path_id, _)| **path_id);
    for (path_id, (raw_code_path, bitmap)) in agg_entries {
        let fps = path_fps.get(path_id);
        // code_path 末段即 leaf_code（模型 B tag_paths.leaf_code 锚点），无需逐行读 ftr.tag_code
        let tag_code = raw_code_path.rsplit('/').next().unwrap_or("").to_string();
        if tag_code.trim().is_empty() {
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

        let name_geo_location = omni_core::concepts::Concept::地理位置.zh_name();
        let name_author = omni_core::concepts::Concept::作者.zh_name();
        let name_file_type = omni_core::concepts::Concept::文件类型.zh_name();
        let name_document = omni_core::concepts::Concept::文档.zh_name();

        // 悲观数据兜底 (M-3)：若 code_path 为空，回退虚拟挂载至 /{code_content_tags}/{tag_code}
        let eff_code_path = if raw_code_path.trim().is_empty() {
            format!("/{}/{}", code_content_tags, tag_code)
        } else {
            raw_code_path.clone()
        };

        let mut code_segs: Vec<String> = eff_code_path
            .split('/')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();

        // 模型 B 无 ftr.name_path：按 code_path 段序从 resolved_names（语义包 + file_tags_private）实时回译展示名
        let mut name_segs: Vec<String> = code_segs
            .iter()
            .map(|seg| resolved_names.get(seg).cloned().unwrap_or_else(|| seg.clone()))
            .collect();

        if code_segs.is_empty() {
            code_segs.push(code_content_tags.to_string());
            code_segs.push(tag_code.clone());
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
            } else if seg_code.starts_with("hownet.") {
                "hownet".to_string()
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
            // 唯有路径末级命中节点才并入该 path_id 预聚合的整块 RoaringBitmap（一次 OR，非逐关系行 insert）
            if i == seg_len - 1 {
                if let Some(target_node) = node_map.get_mut(&curr_code_path) {
                    target_node.direct_bitmap |= bitmap;
                    if include_files {
                        if let Some(fps) = fps {
                            target_node.direct_files.extend(fps.iter().cloned());
                        }
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

    // 3. 展示名已在 2.2 阶段按唯一 path_id 的 code_path 段序批量回译完毕
    // （语义包 tag_aliases_{lang} canonical + 业务主库 file_tags_private），无需再按 all_codes 二次查词。

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
