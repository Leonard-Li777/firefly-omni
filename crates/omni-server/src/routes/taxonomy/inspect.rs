use axum::{extract::State, Json};
use rusqlite::Connection;
use crate::AppState;
use super::types::*;
use super::utils::*;
use std::collections::HashMap;

pub fn inspect_single_tag(
    conn: &Connection,
    tag_name: &str,
    target_parent_code: &str,
    locale: &str,
) -> InspectTagItemResult {
    let target_table = omni_pro::resolve_tag_aliases_table(locale);
    let has_alias_table = table_exists(conn, target_table);
    let has_file_tags = table_exists(conn, "file_tags");
    inspect_single_tag_internal(conn, tag_name, target_parent_code, target_table, has_alias_table, has_file_tags)
}

fn inspect_single_tag_internal(
    conn: &Connection,
    tag_name: &str,
    target_parent_code: &str,
    target_table: &str,
    has_alias_table: bool,
    has_file_tags: bool,
) -> InspectTagItemResult {
    let clean_name = tag_name.trim();
    let clean_parent = target_parent_code.trim();

    if clean_name.is_empty() {
        return InspectTagItemResult {
            tag_name: tag_name.to_string(),
            is_controlled: false,
            matched_parent: false,
            code: None,
            canonical_name: None,
            parent_codes: Vec::new(),
            ancestry_matched_code: None,
        };
    }

    if !has_file_tags {
        return InspectTagItemResult {
            tag_name: tag_name.to_string(),
            is_controlled: false,
            matched_parent: false,
            code: None,
            canonical_name: None,
            parent_codes: Vec::new(),
            ancestry_matched_code: None,
        };
    }

    // 1. 寻找候选受控 tag_code
    struct Candidate {
        code: String,
        canonical_name: String,
    }

    let mut candidates: Vec<Candidate> = Vec::new();

    // 1.1 从别名表中精确匹配 lemma（严格遵循 omw > hownet > builtin 权威优先级排序）
    if has_alias_table {
        let sql = format!(
            "SELECT a.tag_code, a.lemma, \
             (SELECT a2.lemma FROM {target_table} a2 WHERE a2.tag_code = a.tag_code AND a2.is_canonical = 1 LIMIT 1) as canonical_lemma \
             FROM {target_table} a \
             WHERE a.lemma = ?1 COLLATE NOCASE \
             ORDER BY \
               CASE \
                 WHEN a.tag_code LIKE 'omw.%' THEN 3 \
                 WHEN a.tag_code LIKE 'hownet.%' THEN 2 \
                 WHEN a.tag_code LIKE 'builtin.%' THEN 1 \
                 ELSE 0 \
               END DESC, \
               a.is_canonical DESC, a.count DESC"
        );
        if let Ok(mut stmt) = conn.prepare(&sql) {
            if let Ok(rows) = stmt.query_map([clean_name], |row| {
                let code: String = row.get(0)?;
                let lemma: String = row.get(1)?;
                let canonical_lemma: Option<String> = row.get(2)?;
                Ok(Candidate {
                    code,
                    canonical_name: canonical_lemma.unwrap_or(lemma),
                })
            }) {
                for c in rows.flatten() {
                    candidates.push(c);
                }
            }
        }
    }

    // 1.2 若别名表未命中，尝试直接以 code 精确匹配
    if candidates.is_empty() {
        let sql = "SELECT code FROM file_tags WHERE code = ?1";
        if let Ok(code) = conn.query_row(sql, [clean_name], |row| row.get::<_, String>(0)) {
            candidates.push(Candidate {
                code,
                canonical_name: clean_name.to_string(),
            });
        }
    }

    if candidates.is_empty() {
        return InspectTagItemResult {
            tag_name: tag_name.to_string(),
            is_controlled: false,
            matched_parent: false,
            code: None,
            canonical_name: None,
            parent_codes: Vec::new(),
            ancestry_matched_code: None,
        };
    }

    // 2. 针对候选条目拉取直接 parent_codes 并运行祖先闭包检测
    let fetch_parent_codes = |c: &str| -> Vec<String> {
        let sql = "SELECT parent_codes FROM file_tags WHERE code = ?1";
        match conn.query_row(sql, [c], |row| row.get::<_, Option<String>>(0)) {
            Ok(Some(json_str)) => match serde_json::from_str::<Vec<String>>(&json_str) {
                Ok(parsed) => return parsed,
                Err(e) => {
                    tracing::warn!(
                        "[Inspect] 解析 parent_codes JSON 失败 (code={}): {}",
                        c,
                        e
                    );
                }
            },
            Ok(None) => {}
            Err(rusqlite::Error::QueryReturnedNoRows) => {}
            Err(e) => {
                tracing::warn!("[Inspect] 查询 parent_codes 失败 (code={}): {}", c, e);
            }
        }
        Vec::new()
    };

    // 祖先传递闭包算法 (Transitive Ancestor Closure Check)
    let check_ancestry = |start_code: &str, target: &str| -> Option<String> {
        if target.is_empty() {
            return None;
        }
        let mut queue: std::collections::VecDeque<(String, usize)> = std::collections::VecDeque::new();
        let mut visited: std::collections::HashSet<String> = std::collections::HashSet::new();

        let initial_parents = fetch_parent_codes(start_code);
        for p in initial_parents {
            if p == target {
                return Some(target.to_string());
            }
            queue.push_back((p.clone(), 1));
            visited.insert(p);
        }

        // BFS 向上追溯，最大深度限制 5 级防环
        while let Some((curr, depth)) = queue.pop_front() {
            if curr == target {
                return Some(target.to_string());
            }
            if depth >= 5 {
                continue;
            }
            let parents = fetch_parent_codes(&curr);
            for p in parents {
                if p == target {
                    return Some(target.to_string());
                }
                if visited.insert(p.clone()) {
                    queue.push_back((p, depth + 1));
                }
            }
        }
        None
    };

    // 优先选择祖先匹配成功的候选概念
    let mut matched_candidate: Option<(Candidate, Vec<String>, String)> = None;
    let mut fallback_candidate: Option<(Candidate, Vec<String>)> = None;

    for cand in candidates {
        let parents = fetch_parent_codes(&cand.code);
        if let Some(ancestry_hit) = check_ancestry(&cand.code, clean_parent) {
            matched_candidate = Some((cand, parents, ancestry_hit));
            break;
        } else if fallback_candidate.is_none() {
            fallback_candidate = Some((cand, parents));
        }
    }

    if let Some((cand, parents, ancestry_code)) = matched_candidate {
        InspectTagItemResult {
            tag_name: tag_name.to_string(),
            is_controlled: true,
            matched_parent: true,
            code: Some(cand.code),
            canonical_name: Some(cand.canonical_name),
            parent_codes: parents,
            ancestry_matched_code: Some(ancestry_code),
        }
    } else if let Some((cand, parents)) = fallback_candidate {
        InspectTagItemResult {
            tag_name: tag_name.to_string(),
            is_controlled: true,
            matched_parent: false,
            code: Some(cand.code),
            canonical_name: Some(cand.canonical_name),
            parent_codes: parents,
            ancestry_matched_code: None,
        }
    } else {
        InspectTagItemResult {
            tag_name: tag_name.to_string(),
            is_controlled: false,
            matched_parent: false,
            code: None,
            canonical_name: None,
            parent_codes: Vec::new(),
            ancestry_matched_code: None,
        }
    }
}

pub fn inspect_tags_batch(
    conn: &Connection,
    items: &[InspectTagItem],
    locale: &str,
) -> anyhow::Result<Vec<InspectTagItemResult>> {
    let target_table = omni_pro::resolve_tag_aliases_table(locale);
    let has_alias_table = table_exists(conn, target_table);
    let has_file_tags = table_exists(conn, "file_tags");

    Ok(items
        .iter()
        .map(|item| {
            inspect_single_tag_internal(
                conn,
                &item.tag_name,
                &item.target_parent_code,
                target_table,
                has_alias_table,
                has_file_tags,
            )
        })
        .collect())
}

pub fn is_shell_noun_code(code: &str) -> bool {
    SHELL_NOUN_CODES.contains(&code)
}

pub fn is_real_verb_or_modifier(pos: Option<&str>, lexfile: Option<&str>, code: &str) -> bool {
    if let Some(p) = pos {
        let p_lower = p.trim().to_lowercase();
        if p_lower == "v" || p_lower == "a" || p_lower == "s" || p_lower == "r" {
            return true;
        }
    }
    if let Some(lex) = lexfile {
        if lex.trim().starts_with("verb.") {
            return true;
        }
    }
    if code.starts_with("omw.")
        && (code.ends_with(".v") || code.ends_with(".a") || code.ends_with(".r") || code.ends_with(".s"))
    {
        return true;
    }
    false
}

pub fn evaluate_node_policy(ctx: &NodeEvaluationContext) -> TaxonomyFoldPolicy {
    let is_function_word_root = ctx.code == "builtin.category.function_word" || ctx.code == "builtin.category.relational_time";

    if ctx.is_root && !is_function_word_root {
        if ctx.code == "builtin.category.action" || is_real_verb_or_modifier(ctx.pos, ctx.lexfile, ctx.code) {
            return TaxonomyFoldPolicy::FlattenToActionRoot;
        }
        return TaxonomyFoldPolicy::Keep;
    }

    if is_function_word_root {
        return TaxonomyFoldPolicy::RejectSubtree;
    }
    if let Some(p) = ctx.pos {
        let p_trimmed = p.trim();
        if matches!(p_trimmed, "m" | "q" | "p" | "c" | "u" | "y" | "o" | "h" | "w" | "b" | "x")
            || omni_core::tag_admissibility::is_tag_pos_rejected(p_trimmed)
        {
            return TaxonomyFoldPolicy::RejectSubtree;
        }
    }
    if let Some(lex) = ctx.lexfile {
        if omni_core::tag_admissibility::is_banned_lexfile(lex) {
            return TaxonomyFoldPolicy::RejectSubtree;
        }
    }

    if ctx.in_action_branch
        || ctx.code == "builtin.category.action"
        || is_real_verb_or_modifier(ctx.pos, ctx.lexfile, ctx.code)
    {
        return TaxonomyFoldPolicy::FlattenToActionRoot;
    }

    if is_shell_noun_code(ctx.code)
        || ctx.code == "builtin.category.abstract_entity"
        || ctx.lexfile == Some("noun.Tops")
    {
        return TaxonomyFoldPolicy::BypassAndHoist;
    }

    TaxonomyFoldPolicy::Keep
}

pub fn load_pack_semantic_meta_for_codes(conn: &Connection, codes: &[String]) -> HashMap<String, NodeSemanticMeta> {
    if codes.is_empty() || !table_exists(conn, "file_tags") {
        return HashMap::new();
    }

    let mut has_sort_order = false;
    let mut has_pos = false;
    let mut has_lexfile = false;
    let mut has_sememe_category = false;

    if let Ok(mut info_stmt) = conn.prepare("PRAGMA table_info(file_tags)") {
        if let Ok(rows) = info_stmt.query_map([], |r| r.get::<_, String>(1)) {
            for col in rows.flatten() {
                match col.as_str() {
                    "sort_order" => has_sort_order = true,
                    "pos" => has_pos = true,
                    "lexfile" => has_lexfile = true,
                    "sememe_category" => has_sememe_category = true,
                    _ => {}
                }
            }
        }
    }

    let sort_expr = if has_sort_order { "sort_order" } else { "0 AS sort_order" };
    let pos_expr = if has_pos { "pos" } else { "NULL AS pos" };
    let lex_expr = if has_lexfile { "lexfile" } else { "NULL AS lexfile" };
    let sem_expr = if has_sememe_category { "sememe_category" } else { "NULL AS sememe_category" };

    let mut map = HashMap::new();
    for chunk in codes.chunks(200) {
        let placeholders = vec!["?"; chunk.len()].join(",");
        let sql = format!(
            "SELECT code, {sort_expr}, {pos_expr}, {lex_expr}, {sem_expr} FROM file_tags WHERE code IN ({placeholders})"
        );
        if let Ok(mut stmt) = conn.prepare(&sql) {
            let params: Vec<&dyn rusqlite::ToSql> = chunk.iter().map(|c| c as &dyn rusqlite::ToSql).collect();
            if let Ok(rows) = stmt.query_map(params.as_slice(), |row| {
                let code: String = row.get(0)?;
                let sort_order: i64 = row.get(1).unwrap_or(0);
                let pos: Option<String> = row.get(2).ok();
                let lexfile: Option<String> = row.get(3).ok();
                let sememe_category: Option<u8> = row.get(4).ok();
                Ok((code, NodeSemanticMeta {
                    sort_order,
                    pos,
                    lexfile,
                    sememe_category,
                }))
            }) {
                for (code, meta) in rows.flatten() {
                    map.insert(code, meta);
                }
            }
        }
    }
    map
}

pub static SHELL_NOUN_CODES: &[&str] = &[
    "omw.00024720.n", // lint-ignore: raw-code // 状态 (state, status)
    "omw.00026367.n", // lint-ignore: raw-code // 过程 (process)
    "omw.00027167.n", // lint-ignore: raw-code // 方式 (manner, way)
    "omw.00028119.n", // lint-ignore: raw-code // 原因 (reason, cause)
    "omw.00029378.n", // lint-ignore: raw-code // 方法 (method)
    "omw.00024073.n", // lint-ignore: raw-code // 条件 (condition)
    "omw.13941407.n", // lint-ignore: raw-code // 情况 (situation)
    "omw.00026788.n", // lint-ignore: raw-code // 问题 (problem)
    "omw.05834839.n", // lint-ignore: raw-code // 问题
    "omw.05835747.n", // lint-ignore: raw-code // 类型 (type)
    "omw.13939634.n", // lint-ignore: raw-code // 阶段 (phase, stage)
    "omw.05836885.n", // lint-ignore: raw-code // 形式 (form)
    "omw.05837265.n", // lint-ignore: raw-code // 模式 (mode, pattern)
    "builtin.category.abstract_entity", "builtin.status", "builtin.state", "builtin.process", "builtin.way",
    "builtin.reason", "builtin.method", "builtin.condition", "builtin.situation", "builtin.problem",
    "builtin.type", "builtin.stage", "builtin.pattern",
];

pub fn load_dimension_policies_from_db(
    master_db_path: Option<&std::path::Path>,
) -> HashMap<String, omni_core::DimensionExecutionPolicy> {
    let mut policies = omni_core::get_default_dimension_policies();
    let Some(path) = master_db_path else {
        return policies;
    };
    if !path.exists() {
        tracing::warn!(
            "[Inspect] 业务主库路径不存在，维度策略降级为默认值: {}",
            path.display()
        );
        return policies;
    }

    // 工单09 · 连接治理收尾：直连业务主库必须经 Seam A 收口点
    // open_omni_readonly_connection（busy_timeout(5000) + 只读 PRAGMA 套装），
    // 根治 Node 主进程写入时直接抛 SQLITE_BUSY；原裸 Connection::open_with_flags
    // 既绕过收口（红线4 禁各模块重复连接实现）又缺 busy_timeout。
    // 禁 .unwrap()：连接失败降级返回默认策略并 logger.warn 上报，不得 panic。
    let conn = match super::connection::open_omni_readonly_connection(path) {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(
                "[Inspect] 打开业务主库失败，维度策略降级为默认值: {}",
                e
            );
            return policies;
        }
    };

    let val_str: Result<String, _> = conn.query_row(
        "SELECT value FROM system_config WHERE key = 'DIMENSION_POLICIES' LIMIT 1",
        [],
        |row| row.get(0),
    );

    match val_str {
        Ok(raw) => {
            match serde_json::from_str::<HashMap<String, omni_core::DimensionExecutionPolicy>>(&raw) {
                Ok(db_policies) => {
                    for (code, policy) in db_policies {
                        policies.insert(code, policy);
                    }
                }
                Err(e) => {
                    tracing::warn!(
                        "[Inspect] 解析 system_config.DIMENSION_POLICIES JSON 失败，沿用默认策略: {}",
                        e
                    );
                }
            }
        }
        Err(rusqlite::Error::QueryReturnedNoRows) => {
            // 未配置自定义维度策略时正常沿用内置默认策略
        }
        Err(e) => {
            tracing::warn!(
                "[Inspect] 读取 system_config.DIMENSION_POLICIES 失败，沿用默认策略: {}",
                e
            );
        }
    }
    policies
}

pub async fn reload_policies_handler(
    State(state): State<AppState>,
) -> Json<ReloadPoliciesResponse> {
    let master_path = state.master_db_path.lock().ok().and_then(|p| p.clone());
    let new_policies = load_dimension_policies_from_db(master_path.as_deref());
    let count = new_policies.len();
    if let Ok(mut lock) = state.dimension_policies.write() {
        *lock = new_policies;
        drop(lock);
        tracing::info!("[reload_policies_handler] 策略重载成功，当前激活策略数: {}", count);
        state.publish_status_event("ok");
        Json(ReloadPoliciesResponse {
            success: true,
            count,
            error: None,
        })
    } else {
        Json(ReloadPoliciesResponse {
            success: false,
            count: 0,
            error: Some("RwLock poisoned".to_string()),
        })
    }
}
