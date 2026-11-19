use rusqlite::Connection;
use std::collections::{BTreeMap, HashMap, HashSet};
use super::types::*;
use super::connection::open_omni_readonly_connection;
use super::alias_query::query_canonical_and_aliases;
use super::tree_fast::query_taxonomy_tree_fast;

/// 内部逻辑：从 SQLite 连接中提取并组装分类树
pub fn query_taxonomy_tree(
    conn: &Connection,
    locale: &str,
    root_filter: Option<&str>,
    master_db_path: Option<&std::path::Path>,
    include_files: bool,
    workspace_id: Option<i64>,
    directory_prefix: Option<&str>,
) -> anyhow::Result<TaxonomyTreeResponse> {
    // 若提供业务主库路径且有效，优先且强制走倒排 Trie 极速分类树引擎 (ADR-0038 / ADR-0053 / P95 <= 5ms)
    if let Some(path) = master_db_path {
        if path.exists() {
            match open_omni_readonly_connection(path) {
                Ok(master_conn) => {
                    return query_taxonomy_tree_fast(
                        conn,
                        locale,
                        root_filter,
                        &master_conn,
                        include_files,
                        workspace_id,
                        directory_prefix,
                    );
                }
                Err(e) => {
                    tracing::warn!(
                        "[TaxonomyTree] 打开业务主库只读连接失败，降级至语义包轻量树构建: {}",
                        e
                    );
                }
            }
        }
    }

    // 1. 检查 file_tags 表是否存在 (用于离线/只读无主库轻量回退)
    let has_file_tags: bool = conn
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'file_tags'",
            [],
            |_| Ok(true),
        )
        .unwrap_or(false);

    if !has_file_tags {
        return Ok(TaxonomyTreeResponse {
            locale: locale.to_string(),
            root_nodes: Vec::new(),
            total_nodes: 0,
        });
    }

    // 2. 预载入别名与规范名映射表 (优化展示名注入)
    let alias_map = match query_canonical_and_aliases(conn, locale) {
        Ok(map) => map,
        Err(e) => {
            tracing::warn!("[TaxonomyTree] 加载别名与规范名映射表失败，降级为空映射: {}", e);
            HashMap::new()
        }
    };

    // 3. 动态探测 file_tags 表列，兼容只读语义包 (含 sort_order / _debug_name_zh, 无 name) 与本地主库 (含 name, 无 sort_order)
    let mut has_name_col = false;
    let mut has_sort_order_col = false;
    let mut has_debug_name_col = false;
    if let Ok(mut info_stmt) = conn.prepare("PRAGMA table_info(file_tags)") {
        if let Ok(rows) = info_stmt.query_map([], |r| r.get::<_, String>(1)) {
            for col_name in rows.flatten() {
                if col_name == "name" {
                    has_name_col = true;
                } else if col_name == "sort_order" {
                    has_sort_order_col = true;
                } else if col_name == "_debug_name_zh" {
                    has_debug_name_col = true;
                }
            }
        }
    }

    let name_expr = if has_name_col {
        "name"
    } else if has_debug_name_col {
        "_debug_name_zh"
    } else {
        "'' AS name"
    };

    let sort_order_expr = if has_sort_order_col {
        "sort_order"
    } else {
        "0 AS sort_order"
    };

    let select_sql = format!(
        "SELECT code, {}, parent_codes, source, {} FROM file_tags WHERE source IN ('dimension', 'tag', 'builtin') ORDER BY code",
        name_expr, sort_order_expr
    );
    let mut stmt = conn.prepare(&select_sql)?;

    struct RawTag {
        code: String,
        default_name: String,
        parent_codes: Vec<String>,
        source: String,
        _pack_source: String,
        sort_order: i64,
    }

    let mut raw_tags: Vec<RawTag> = Vec::new();
    let rows = stmt.query_map([], |row| {
        let code: String = row.get(0)?;
        let name: Option<String> = row.get(1)?;
        let parent_codes_raw: String = row.get(2).unwrap_or_else(|_| "[]".to_string());
        let source: Option<String> = row.get(3)?;
        let sort_order: Option<i64> = row.get(4)?;
        Ok((code, name, parent_codes_raw, source, sort_order))
    })?;

    let mut pack_source_map: HashMap<String, String> = HashMap::new();
    for row in rows {
        let (code, default_name_opt, parent_codes_raw, pack_source, sort_order) = row?;
        let default_name = default_name_opt.unwrap_or_default();
        let parent_codes: Vec<String> = serde_json::from_str(&parent_codes_raw).unwrap_or_default();
        let pack_source_str = pack_source.unwrap_or_default();
        pack_source_map.insert(code.clone(), pack_source_str.clone());

        // TreeNode.source = code 前缀分区（与 pack source 语义正交：前者 code 身份，后者 taxonomy 来源）
        let source = if code.starts_with("builtin.") {
            "builtin"
        } else if code.starts_with("omw.") {
            "omw"
        } else if code.starts_with("_ext.") {
            "_ext"
        } else {
            "user"
        };
        let sort_order = sort_order.unwrap_or(0);
        raw_tags.push(RawTag {
            code,
            default_name,
            parent_codes,
            source: source.to_string(),
            _pack_source: pack_source_str,
            sort_order,
        });
    }

    // 4. 为每个标签确定注入的本地化展示名
    let mut node_map: BTreeMap<String, TaxonomyNode> = BTreeMap::new();
    for raw in raw_tags {
        let display_name = if let Some((canonical, _)) = alias_map.get(&raw.code) {
            canonical.clone()
        } else if !raw.default_name.trim().is_empty() {
            raw.default_name.clone()
        } else {
            raw.code.clone()
        };

        let primary_parent = raw
            .parent_codes
            .iter()
            .find(|p| pack_source_map.contains_key(*p) && *p != &raw.code)
            .cloned()
            .or_else(|| raw.parent_codes.first().cloned());
        // 特例：builtin.content_tags 恒定垫底，与 fast path 的 9999 硬编码口径一致（pack 值 28 不参与根级排序）
        let sort_order = if raw.code == omni_core::concepts::Concept::内容标签.code() {
            9999
        } else {
            raw.sort_order
        };
        node_map.insert(
            raw.code.clone(),
            TaxonomyNode {
                code: raw.code,
                name: display_name,
                parent_code: primary_parent,
                parent_codes: raw.parent_codes,
                source: raw.source,
                sort_order,
                file_count: 0,
                files: Vec::new(),
                children: Vec::new(),
                code_path: None,
                name_path: None,
            },
        );
    }

    // 5. 组装多叉树森林与文件关联 (支持直读桌面主库 file_tag_relations)
    let mut direct_files: HashMap<String, HashSet<String>> = HashMap::new();
    let mut parent_to_children: HashMap<String, Vec<String>> = HashMap::new();
    let mut root_candidates: Vec<String> = Vec::new();

    // 5.1 确保 7 大受控根维度节点完整存在于 node_map，并具备准确的当前母语规范展示名
    let lang = locale.split('-').next().unwrap_or("zh");
    for root_concept in omni_core::ROOT_DIMENSION_CONCEPTS {
        let code = root_concept.code().to_string();
        let display_name = alias_map
            .get(&code)
            .map(|(c, _)| c.clone())
            .unwrap_or_else(|| omni_core::tag_identity::tag_display(&code, lang));

        if let Some(existing_node) = node_map.get_mut(&code) {
            if existing_node.name == code || existing_node.name.is_empty() {
                existing_node.name = display_name;
            }
        } else {
            node_map.insert(
                code.clone(),
                TaxonomyNode {
                    code: code.clone(),
                    name: display_name,
                    parent_code: None,
                    parent_codes: Vec::new(),
                    source: "builtin".to_string(),
                    sort_order: if code == omni_core::concepts::Concept::内容标签.code() { 9999 } else { 0 },
                    file_count: 0,
                    files: Vec::new(),
                    children: Vec::new(),
                    code_path: None,
                    name_path: None,
                },
            );
        }
        pack_source_map.entry(code).or_insert_with(|| "dimension".to_string());
    }

    // 5.2 若提供业务主库路径，经 Seam A 统一只读连接加载 file_tag_relations 中的文件归属与父级链
    if let Some(path) = master_db_path {
        if path.exists() {
            if let Ok(master_conn) = open_omni_readonly_connection(path) {
                // 模型 B 唯一路径：物化路径已外迁至微型字典 tag_paths，ftr 仅保留 path_id。
                // 移除热路径 PRAGMA table_info 与 ftr.code_path / ftr.name_path 直读（Spec §2.5.3）。
                // 缺 tag_paths 表时显式 warn 并跳过该段文件归属装配，严禁静默吞异常。
                if !super::utils::table_exists(&master_conn, "tag_paths") {
                    tracing::warn!(
                        "[TaxonomyTree] 业务主库缺少 tag_paths 表（模型 B 唯一路径字典），跳过 file_tag_relations 文件归属装配"
                    );
                } else {
                    let has_wf: bool = super::utils::table_exists(&master_conn, "workspace_files");

                    let mut where_clauses: Vec<String> = Vec::new();
                    let mut params: Vec<rusqlite::types::Value> = Vec::new();

                    if has_wf {
                        where_clauses.push("wf.status = 1".to_string());
                        if let Some(ws_id) = workspace_id {
                            where_clauses.push("wf.workspace_id = ?".to_string());
                            params.push(rusqlite::types::Value::Integer(ws_id));
                        }

                        if let Some(prefix) = directory_prefix {
                            let clean_prefix = prefix.trim();
                            if !clean_prefix.is_empty() {
                                let sep = if clean_prefix.contains('\\') { '\\' } else { '/' };
                                let prefix_pattern = if clean_prefix.ends_with('/') || clean_prefix.ends_with('\\') {
                                    format!("{}%", clean_prefix)
                                } else {
                                    format!("{}{}%", clean_prefix, sep)
                                };
                                where_clauses.push("(wf.path LIKE ? OR wf.path = ?)".to_string());
                                params.push(rusqlite::types::Value::Text(prefix_pattern));
                                params.push(rusqlite::types::Value::Text(clean_prefix.to_string()));
                            }
                        }
                    }

                    // 展示名由语义包 alias_map（受控）+ 业务主库 file_tags_private（非受控 _ext.*/user.*）实时译出，
                    // 不再依赖已删除的 ftr.name_path。
                    let mut display_map: HashMap<String, String> = alias_map
                        .iter()
                        .map(|(code, (canonical, _))| (code.clone(), canonical.clone()))
                        .collect();
                    if super::utils::table_exists(&master_conn, "file_tags_private") {
                        match master_conn.prepare("SELECT code, name FROM file_tags_private") {
                            Ok(mut pstmt) => match pstmt.query_map([], |row| {
                                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                            }) {
                                Ok(prows) => {
                                    for (code, name) in prows.flatten() {
                                        display_map.entry(code).or_insert(name);
                                    }
                                }
                                Err(e) => tracing::warn!(
                                    "[TaxonomyTree] 读取 file_tags_private 展示名失败: {}",
                                    e
                                ),
                            },
                            Err(e) => tracing::warn!(
                                "[TaxonomyTree] 准备 file_tags_private 查询失败: {}",
                                e
                            ),
                        }
                    }

                    // 统一经 ftr.path_id JOIN tag_paths 取 tp.code_path（模型 B 唯一路径）
                    let select_sql = if has_wf && !where_clauses.is_empty() {
                        format!(
                            "SELECT ftr.file_fingerprint, ftr.tag_code, ftr.via_parent_code, tp.code_path \
                             FROM file_tag_relations ftr \
                             JOIN tag_paths tp ON tp.path_id = ftr.path_id \
                             JOIN workspace_files wf ON wf.file_fingerprint = ftr.file_fingerprint \
                             WHERE {}",
                            where_clauses.join(" AND ")
                        )
                    } else {
                        format!(
                            "SELECT ftr.file_fingerprint, ftr.tag_code, ftr.via_parent_code, tp.code_path \
                             FROM file_tag_relations ftr \
                             JOIN tag_paths tp ON tp.path_id = ftr.path_id"
                        )
                    };

                    match master_conn.prepare(&select_sql) {
                        Ok(mut stmt) => {
                            let param_refs: Vec<&dyn rusqlite::ToSql> = params.iter().map(|p| p as &dyn rusqlite::ToSql).collect();
                            let rows = stmt.query_map(param_refs.as_slice(), |row| {
                                let fp: String = row.get(0)?;
                                let tag_code: String = row.get(1)?;
                                let via: String = row.get(2).unwrap_or_default();
                                let code_chain: String = row.get(3).unwrap_or_default();
                                // 模型 B 无 name_path：name_chain 置空，展示名统一由 display_map 按 code_path 段序译出
                                Ok((fp, tag_code, via, String::new(), code_chain))
                            });

                            match rows {
                                Ok(rows) => {
                                    for r in rows.flatten() {
                                        let (fp, tag_code, via, name_chain, code_chain) = r;
                                        direct_files.entry(tag_code.clone()).or_default().insert(fp.clone());

                                        let has_name_chain = !name_chain.trim().is_empty();
                                        let has_code_chain = !code_chain.trim().is_empty();
                                        if has_name_chain || has_code_chain {
                                            let name_segs: Vec<String> = if has_name_chain {
                                                name_chain
                                                    .split('/')
                                                    .map(|s| s.trim().to_string())
                                                    .filter(|s| !s.is_empty() && s != omni_core::concepts::Concept::内容标签.zh_name() && s != omni_core::concepts::Concept::内容标签.code())
                                                    .collect()
                                            } else {
                                                // 模型 B：按 code_path 段序经 display_map 实时回译展示名
                                                code_chain
                                                    .split('/')
                                                    .map(|s| s.trim())
                                                    .filter(|s| !s.is_empty() && *s != omni_core::concepts::Concept::内容标签.code())
                                                    .map(|s| display_map.get(s).cloned().unwrap_or_else(|| s.to_string()))
                                                    .collect()
                                            };
                                            let code_segs: Vec<String> = if has_code_chain {
                                                code_chain
                                                    .split('/')
                                                    .map(|s| s.trim().to_string())
                                                    .filter(|s| !s.is_empty() && s != omni_core::concepts::Concept::内容标签.zh_name() && s != omni_core::concepts::Concept::内容标签.code())
                                                    .collect()
                                            } else {
                                                Vec::new()
                                            };

                                            let num_segs = code_segs.len().max(name_segs.len());
                                            if num_segs > 0 {
                                                let mut first_seg_code = if !code_segs.is_empty() && !code_segs[0].is_empty() {
                                                    code_segs[0].clone()
                                                } else if num_segs == 1 {
                                                    tag_code.clone()
                                                } else {
                                                    format!("builtin.cat.{}", name_segs[0])
                                                };

                                                if first_seg_code == "builtin.fileType" {
                                                    first_seg_code = omni_core::concepts::Concept::文件类型.code().to_string();
                                                }

                                                let is_root_dim = omni_core::is_root_dimension(&first_seg_code);

                                                let (mut prev_code, start_idx) = if is_root_dim {
                                                    (first_seg_code, 1)
                                                } else {
                                                    (omni_core::concepts::Concept::内容标签.code().to_string(), 0)
                                                };

                                                for i in start_idx..num_segs {
                                                    let seg_code = if i < code_segs.len() && !code_segs[i].is_empty() {
                                                        code_segs[i].clone()
                                                    } else if i == num_segs - 1 {
                                                        tag_code.clone()
                                                    } else if i < name_segs.len() {
                                                        format!("builtin.cat.{}", name_segs[i])
                                                    } else {
                                                        tag_code.clone()
                                                    };

                                                    let seg_name = if i < name_segs.len() && !name_segs[i].is_empty() {
                                                        name_segs[i].clone()
                                                    } else if let Some(n) = node_map.get(&seg_code) {
                                                        n.name.clone()
                                                    } else if let Some(n) = display_map.get(&seg_code) {
                                                        // 语义包 + file_tags_private 实时译出的展示名（模型 B 无 name_path）
                                                        n.clone()
                                                    } else if let Some((canonical, _)) = alias_map.get(&seg_code) {
                                                        canonical.clone()
                                                    } else {
                                                        let lang = locale.split('-').next().unwrap_or("zh");
                                                        omni_core::tag_identity::tag_display(&seg_code, lang)
                                                    };

                                                    if !node_map.contains_key(&seg_code) {
                                                        node_map.insert(
                                                            seg_code.clone(),
                                                            TaxonomyNode {
                                                                code: seg_code.clone(),
                                                                name: seg_name.clone(),
                                                                parent_code: Some(prev_code.clone()),
                                                                parent_codes: vec![prev_code.clone()],
                                                                source: "chain".to_string(),
                                                                sort_order: 9999,
                                                                file_count: 0,
                                                                files: Vec::new(),
                                                                children: Vec::new(),
                                                                code_path: None,
                                                                name_path: None,
                                                            },
                                                        );
                                                    }

                                                    if prev_code != seg_code {
                                                        let children = parent_to_children.entry(prev_code.clone()).or_default();
                                                        if !children.contains(&seg_code) {
                                                            children.push(seg_code.clone());
                                                        }
                                                    }
                                                    prev_code = seg_code;
                                                }
                                            }
                                        } else if !via.trim().is_empty() && via != tag_code {
                                            if !node_map.contains_key(&tag_code) {
                                                let lang = locale.split('-').next().unwrap_or("zh");
                                                let name = display_map
                                                    .get(&tag_code)
                                                    .cloned()
                                                    .or_else(|| alias_map.get(&tag_code).map(|(c, _)| c.clone()))
                                                    .unwrap_or_else(|| omni_core::tag_identity::tag_display(&tag_code, lang));
                                                node_map.insert(
                                                    tag_code.clone(),
                                                    TaxonomyNode {
                                                        code: tag_code.clone(),
                                                        name,
                                                        parent_code: Some(via.clone()),
                                                        parent_codes: vec![via.clone()],
                                                        source: "rel".to_string(),
                                                        sort_order: 9999,
                                                        file_count: 0,
                                                        files: Vec::new(),
                                                        children: Vec::new(),
                                                        code_path: None,
                                                        name_path: None,
                                                    },
                                                );
                                            }
                                            let children = parent_to_children.entry(via.clone()).or_default();
                                            if !children.contains(&tag_code) {
                                                children.push(tag_code.clone());
                                            }
                                        }
                                    }
                                }
                                Err(e) => tracing::warn!(
                                    "[TaxonomyTree] 读取模型 B 标签关系行失败: {}",
                                    e
                                ),
                            }
                        }
                        Err(e) => tracing::warn!(
                            "[TaxonomyTree] 准备模型 B 标签关系查询失败: {}",
                            e
                        ),
                    }
                }
            }
        }
    }

    // 5.3 补齐受控语义库中所有已有节点的父子邻接映射
    for (code, node) in &node_map {
        if let Some(ref p) = node.parent_code {
            if node_map.contains_key(p) && p != code {
                let children = parent_to_children.entry(p.clone()).or_default();
                if !children.contains(code) {
                    children.push(code.clone());
                }
            } else {
                let is_root = omni_core::is_root_dimension(code)
                    || (pack_source_map.get(code).map(|s| s == "dimension").unwrap_or(false)
                        && node.parent_codes.is_empty());
                let code_content_tags = omni_core::concepts::Concept::内容标签.code();
                if is_root && !root_candidates.contains(code) {
                    root_candidates.push(code.clone());
                } else if !is_root && code != code_content_tags {
                    let children = parent_to_children.entry(code_content_tags.to_string()).or_default();
                    if !children.contains(code) {
                        children.push(code.clone());
                    }
                }
            }
        } else {
            let is_root = omni_core::is_root_dimension(code)
                || (pack_source_map.get(code).map(|s| s == "dimension").unwrap_or(false)
                    && node.parent_codes.is_empty());
            let code_content_tags = omni_core::concepts::Concept::内容标签.code();
            if is_root && !root_candidates.contains(code) {
                root_candidates.push(code.clone());
            } else if !is_root && code != code_content_tags {
                let children = parent_to_children.entry(code_content_tags.to_string()).or_default();
                if !children.contains(code) {
                    children.push(code.clone());
                }
            }
        }
    }

    // 递归组装树节点并自底向上后序遍历计算唯一文件合计
    fn build_subtree(
        code: &str,
        node_map: &BTreeMap<String, TaxonomyNode>,
        parent_to_children: &HashMap<String, Vec<String>>,
        visited: &mut HashSet<String>,
        direct_files: &HashMap<String, HashSet<String>>,
        include_files: bool,
    ) -> Option<(TaxonomyNode, HashSet<String>)> {
        if visited.contains(code) {
            return None;
        }
        visited.insert(code.to_string());

        let base_node = node_map.get(code)?;
        let mut children = Vec::new();
        let mut aggregate_files = HashSet::new();

        if let Some(dfs) = direct_files.get(code) {
            aggregate_files.extend(dfs.iter().cloned());
        }
        if let Some(dfs) = direct_files.get(&base_node.name) {
            aggregate_files.extend(dfs.iter().cloned());
        }

        if let Some(child_codes) = parent_to_children.get(code) {
            for child_code in child_codes {
                if let Some((child_node, child_files)) = build_subtree(child_code, node_map, parent_to_children, visited, direct_files, include_files) {
                    aggregate_files.extend(child_files);
                    children.push(child_node);
                }
            }
        }
        children.sort_by(|a, b| {
            let order_a = if a.sort_order > 0 { a.sort_order } else { i64::MAX };
            let order_b = if b.sort_order > 0 { b.sort_order } else { i64::MAX };
            if order_a != order_b {
                order_a.cmp(&order_b)
            } else {
                a.code.cmp(&b.code)
            }
        });

        visited.remove(code);

        let file_count = aggregate_files.len();
        let files = if include_files {
            let mut sorted_files: Vec<String> = aggregate_files.iter().cloned().collect();
            sorted_files.sort();
            sorted_files
        } else {
            Vec::new()
        };

        let node = TaxonomyNode {
            code: base_node.code.clone(),
            name: base_node.name.clone(),
            parent_code: base_node.parent_code.clone(),
            parent_codes: base_node.parent_codes.clone(),
            source: base_node.source.clone(),
            sort_order: base_node.sort_order,
            file_count,
            files,
            children,
            code_path: base_node.code_path.clone(),
            name_path: base_node.name_path.clone(),
        };

        Some((node, aggregate_files))
    }

    let mut roots: Vec<TaxonomyNode> = Vec::new();
    let mut visited = HashSet::new();

    if let Some(target_root) = root_filter {
        if let Some((root_node, _)) = build_subtree(target_root, &node_map, &parent_to_children, &mut visited, &direct_files, include_files) {
            if !root_node.children.is_empty() || root_node.file_count > 0 {
                roots.push(root_node);
            }
        }
    } else {
        root_candidates.retain(|c| omni_core::is_root_dimension(c));
        root_candidates.sort();
        let mut candidate_nodes: Vec<TaxonomyNode> = Vec::new();
        for root_code in root_candidates {
            if let Some((root_node, _)) = build_subtree(&root_code, &node_map, &parent_to_children, &mut visited, &direct_files, include_files) {
                if !root_node.children.is_empty() || root_node.file_count > 0 {
                    candidate_nodes.push(root_node);
                }
            }
        }

        candidate_nodes.sort_by(|a, b| {
            let order_a = if a.sort_order > 0 { a.sort_order } else { i64::MAX };
            let order_b = if b.sort_order > 0 { b.sort_order } else { i64::MAX };
            if order_a != order_b {
                order_a.cmp(&order_b)
            } else {
                let is_builtin_a = a.code.starts_with("builtin.");
                let is_builtin_b = b.code.starts_with("builtin.");
                if is_builtin_a != is_builtin_b {
                    is_builtin_b.cmp(&is_builtin_a)
                } else {
                    a.code.cmp(&b.code)
                }
            }
        });

        let mut seen_root_names = HashSet::new();
        for node in candidate_nodes {
            if !seen_root_names.contains(&node.name) {
                seen_root_names.insert(node.name.clone());
                roots.push(node);
            }
        }
    }

    fn count_nodes(nodes: &[TaxonomyNode]) -> usize {
        let mut count = nodes.len();
        for node in nodes {
            count += count_nodes(&node.children);
        }
        count
    }

    let total_nodes = count_nodes(&roots);

    Ok(TaxonomyTreeResponse {
        locale: locale.to_string(),
        root_nodes: roots,
        total_nodes,
    })
}
