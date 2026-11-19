use super::inspect::{evaluate_node_policy, is_real_verb_or_modifier};
use super::types::*;
use std::collections::{BTreeMap, HashMap, HashSet};

/// Trie 节点结构：唯一键为从根到该节点的物化完整路径 (current_code_path)，捍卫前缀树分支隔离性
pub(super) struct TrieNode {
    pub code: String,
    pub name: String,
    pub code_path: String,
    pub name_path: String,
    pub parent_path: Option<String>,
    pub source: String,
    pub sort_order: i64,
    pub direct_files: HashSet<String>,
    pub direct_bitmap: roaring::RoaringBitmap,
    pub children_paths: Vec<String>,
    pub pos: Option<String>,
    pub lexfile: Option<String>,
    pub sememe_category: Option<u8>,
}

/// 节点结构转换辅助函数
pub(super) fn convert_trie_node_to_taxonomy_node(
    node: &TrieNode,
    node_map: &HashMap<String, TrieNode>,
    resolved_names: &HashMap<String, String>,
    lang: &str,
    include_files: bool,
    file_count: usize,
    files_set: &HashSet<String>,
) -> TaxonomyNode {
    let files = if include_files {
        let mut list: Vec<String> = files_set.iter().cloned().collect();
        list.sort();
        list
    } else {
        Vec::new()
    };

    let display_name = if let Some(n) = resolved_names.get(&node.code) {
        n.clone()
    } else if omni_core::is_root_dimension(&node.code) || node.code.starts_with("builtin.") {
        let td = omni_core::tag_identity::tag_display(&node.code, lang);
        if lang.starts_with("zh")
            && td.is_ascii()
            && !node.name.is_empty()
            && !node.name.is_ascii()
            && node.name != node.code
        {
            node.name.clone()
        } else {
            td
        }
    } else if !node.name.is_empty() && node.name != node.code {
        node.name.clone()
    } else {
        omni_core::tag_identity::tag_display(&node.code, lang)
    };

    let parent_code = node
        .parent_path
        .as_ref()
        .and_then(|p| node_map.get(p).map(|pn| pn.code.clone()));
    let parent_codes = parent_code.as_ref().map(|p| vec![p.clone()]).unwrap_or_default();

    TaxonomyNode {
        code: node.code.clone(),
        name: display_name,
        parent_code,
        parent_codes,
        source: node.source.clone(),
        sort_order: node.sort_order,
        file_count,
        files,
        children: Vec::new(),
        code_path: Some(node.code_path.clone()),
        name_path: Some(node.name_path.clone()),
    }
}

/// 动词分支扁平化子孙收集器 (Spec §3.3)
pub(super) fn collect_action_flattened_children(
    node_path: &str,
    action_root_code: &str,
    node_map: &HashMap<String, TrieNode>,
    resolved_names: &HashMap<String, String>,
    lang: &str,
    include_files: bool,
    visited: &mut HashSet<String>,
) -> (Vec<(TaxonomyNode, roaring::RoaringBitmap)>, roaring::RoaringBitmap, HashSet<String>) {
    let mut result_nodes = Vec::new();
    let mut aggregate_bitmap = roaring::RoaringBitmap::new();
    let mut aggregate_files = HashSet::new();

    if !visited.insert(node_path.to_string()) {
        return (result_nodes, aggregate_bitmap, aggregate_files);
    }

    let node = match node_map.get(node_path) {
        Some(n) => n,
        None => return (result_nodes, aggregate_bitmap, aggregate_files),
    };

    let ctx = NodeEvaluationContext {
        code: &node.code,
        pos: node.pos.as_deref(),
        lexfile: node.lexfile.as_deref(),
        sememe_category: node.sememe_category,
        is_root: false,
        in_action_branch: true,
    };
    if evaluate_node_policy(&ctx) == TaxonomyFoldPolicy::RejectSubtree {
        return (result_nodes, aggregate_bitmap, aggregate_files);
    }

    aggregate_bitmap |= &node.direct_bitmap;
    if include_files {
        aggregate_files.extend(node.direct_files.clone());
    }

    for child_path in &node.children_paths {
        let (sub_nodes, sub_bitmap, sub_files) = collect_action_flattened_children(
            child_path,
            action_root_code,
            node_map,
            resolved_names,
            lang,
            include_files,
            visited,
        );
        aggregate_bitmap |= &sub_bitmap;
        if include_files {
            aggregate_files.extend(sub_files);
        }
        result_nodes.extend(sub_nodes);
    }

    if !node.direct_bitmap.is_empty() {
        let mut cloned = convert_trie_node_to_taxonomy_node(
            node,
            node_map,
            resolved_names,
            lang,
            include_files,
            node.direct_bitmap.len() as usize,
            &node.direct_files,
        );
        cloned.children.clear();
        cloned.file_count = node.direct_bitmap.len() as usize;
        cloned.parent_code = Some(action_root_code.to_string());
        cloned.parent_codes = vec![action_root_code.to_string()];
        result_nodes.push((cloned, node.direct_bitmap.clone()));
    }

    (result_nodes, aggregate_bitmap, aggregate_files)
}

/// 动作分支平铺子节点按同 code 归并与稳定排序 (ISS-01 & ISS-03)
pub(super) fn merge_and_sort_flattened_nodes(
    nodes: Vec<(TaxonomyNode, roaring::RoaringBitmap)>,
    action_root_code: &str,
    include_files: bool,
) -> Vec<TaxonomyNode> {
    let mut map: BTreeMap<String, (TaxonomyNode, roaring::RoaringBitmap)> = BTreeMap::new();
    for (mut node, bm) in nodes {
        node.parent_code = Some(action_root_code.to_string());
        node.parent_codes = vec![action_root_code.to_string()];
        if let Some(existing) = map.get_mut(&node.code) {
            existing.1 |= &bm;
            existing.0.file_count = existing.1.len() as usize;
            if include_files {
                let mut file_set: HashSet<String> = existing.0.files.iter().cloned().collect();
                file_set.extend(node.files);
                let mut sorted_files: Vec<String> = file_set.into_iter().collect();
                sorted_files.sort();
                existing.0.files = sorted_files;
            } else {
                existing.0.files.clear();
            }
        } else {
            map.insert(node.code.clone(), (node, bm));
        }
    }
    let mut result: Vec<TaxonomyNode> = map.into_values().map(|(n, _)| n).collect();
    result.sort_by(|a, b| {
        let order_a = if a.sort_order > 0 { a.sort_order } else { i64::MAX };
        let order_b = if b.sort_order > 0 { b.sort_order } else { i64::MAX };
        if order_a != order_b {
            order_a.cmp(&order_b)
        } else {
            a.code.cmp(&b.code)
        }
    });
    result
}

/// 递归装配 Trie 子树 (四维策略分流)
pub(super) fn build_trie_subtree(
    path: &str,
    node_map: &HashMap<String, TrieNode>,
    resolved_names: &HashMap<String, String>,
    lang: &str,
    include_files: bool,
    in_action_branch: bool,
    is_root: bool,
    visited: &mut HashSet<String>,
) -> Option<(Vec<(TaxonomyNode, roaring::RoaringBitmap)>, roaring::RoaringBitmap, HashSet<String>)> {
    if !visited.insert(path.to_string()) {
        return None;
    }

    let node = match node_map.get(path) {
        Some(n) => n,
        None => {
            visited.remove(path);
            return None;
        }
    };

    let ctx = NodeEvaluationContext {
        code: &node.code,
        pos: node.pos.as_deref(),
        lexfile: node.lexfile.as_deref(),
        sememe_category: node.sememe_category,
        is_root,
        in_action_branch,
    };
    let mut policy = evaluate_node_policy(&ctx);

    // 内容标签分支内单传空目录紧凑坍缩 (Zero-Entropy Node Collapse)
    if policy == TaxonomyFoldPolicy::Keep
        && !is_root
        && !omni_core::is_root_dimension(&node.code)
        && node.code != omni_core::concepts::Concept::内容标签.code()
        && path.starts_with("/builtin.content_tags/")
        && node.direct_bitmap.is_empty()
        && node.children_paths.len() == 1
    {
        policy = TaxonomyFoldPolicy::BypassAndHoist;
    }

    match policy {
        TaxonomyFoldPolicy::RejectSubtree => {
            visited.remove(path);
            None
        }
        TaxonomyFoldPolicy::FlattenToActionRoot => {
            let mut aggregate_bitmap = node.direct_bitmap.clone();
            let mut aggregate_files = if include_files { node.direct_files.clone() } else { HashSet::new() };
            let mut unmerged_children = Vec::new();
            let mut action_visited = HashSet::new();
            action_visited.insert(path.to_string());

            for child_path in &node.children_paths {
                let (child_nodes, child_bm, child_files) = collect_action_flattened_children(
                    child_path,
                    &node.code,
                    node_map,
                    resolved_names,
                    lang,
                    include_files,
                    &mut action_visited,
                );
                aggregate_bitmap |= &child_bm;
                if include_files {
                    aggregate_files.extend(child_files);
                }
                unmerged_children.extend(child_nodes);
            }

            let flattened_children = merge_and_sort_flattened_nodes(unmerged_children, &node.code, include_files);
            let mut tax_node = convert_trie_node_to_taxonomy_node(
                node,
                node_map,
                resolved_names,
                lang,
                include_files,
                aggregate_bitmap.len() as usize,
                &aggregate_files,
            );
            tax_node.children = flattened_children;
            visited.remove(path);
            Some((vec![(tax_node, aggregate_bitmap.clone())], aggregate_bitmap, aggregate_files))
        }
        TaxonomyFoldPolicy::BypassAndHoist => {
            let mut aggregate_bitmap = node.direct_bitmap.clone();
            let mut aggregate_files = if include_files { node.direct_files.clone() } else { HashSet::new() };
            let mut hoisted_nodes = Vec::new();

            for child_path in &node.children_paths {
                if let Some((child_nodes, child_bm, child_files)) = build_trie_subtree(
                    child_path,
                    node_map,
                    resolved_names,
                    lang,
                    include_files,
                    in_action_branch,
                    false,
                    visited,
                ) {
                    aggregate_bitmap |= &child_bm;
                    if include_files {
                        aggregate_files.extend(child_files);
                    }
                    hoisted_nodes.extend(child_nodes);
                }
            }
            visited.remove(path);
            Some((hoisted_nodes, aggregate_bitmap, aggregate_files))
        }
        TaxonomyFoldPolicy::Keep => {
            let mut aggregate_bitmap = node.direct_bitmap.clone();
            let mut aggregate_files = if include_files { node.direct_files.clone() } else { HashSet::new() };
            let mut children_nodes: Vec<(TaxonomyNode, roaring::RoaringBitmap)> = Vec::new();

            let next_in_action_branch = in_action_branch
                || node.code == "builtin.category.action"
                || is_real_verb_or_modifier(node.pos.as_deref(), node.lexfile.as_deref(), &node.code);

            for child_path in &node.children_paths {
                if let Some((child_nodes, child_bm, child_files)) = build_trie_subtree(
                    child_path,
                    node_map,
                    resolved_names,
                    lang,
                    include_files,
                    next_in_action_branch,
                    false,
                    visited,
                ) {
                    aggregate_bitmap |= &child_bm;
                    if include_files {
                        aggregate_files.extend(child_files);
                    }
                    for mut cn in child_nodes {
                        cn.0.parent_code = Some(node.code.clone());
                        cn.0.parent_codes = vec![node.code.clone()];
                        children_nodes.push(cn);
                    }
                }
            }

            let mut tax_node = convert_trie_node_to_taxonomy_node(
                node,
                node_map,
                resolved_names,
                lang,
                include_files,
                aggregate_bitmap.len() as usize,
                &aggregate_files,
            );

            // 父子同名自愈折叠 (Deduplication Axiom & Child Hoisting)
            let mut current_children = children_nodes;
            if !tax_node.name.trim().is_empty() {
                loop {
                    let mut has_duplicate = false;
                    let mut next_children = Vec::new();
                    for (child, child_bm) in current_children {
                        if child.name.trim() == tax_node.name.trim() {
                            has_duplicate = true;
                            if include_files {
                                let mut merged_files: HashSet<String> = tax_node.files.into_iter().collect();
                                merged_files.extend(child.files);
                                let mut sorted_files: Vec<String> = merged_files.into_iter().collect();
                                sorted_files.sort();
                                tax_node.files = sorted_files;
                            }
                            for mut grand_child in child.children {
                                grand_child.parent_code = Some(node.code.clone());
                                grand_child.parent_codes = vec![node.code.clone()];
                                let g_bm = roaring::RoaringBitmap::new();
                                next_children.push((grand_child, g_bm));
                            }
                        } else {
                            next_children.push((child, child_bm));
                        }
                    }
                    current_children = next_children;
                    if !has_duplicate {
                        break;
                    }
                }
            }

            // 同父同名兄弟节点自愈合并 (Sibling Deduplication Axiom / ISS-01)
            // 第三元 `extra`：无位图（祖先折叠重挂）节点贡献的计数，与位图并集计数相加，防混合合并丢计数
            let mut sibling_map: BTreeMap<String, (TaxonomyNode, roaring::RoaringBitmap, usize)> = BTreeMap::new();
            for (child, child_bm) in current_children {
                let key = if !child.name.trim().is_empty() {
                    child.name.trim().to_string()
                } else {
                    child.code.clone()
                };
                if let Some(existing) = sibling_map.get_mut(&key) {
                    if include_files {
                        existing.1 |= &child_bm;
                        let mut merged_files: HashSet<String> = existing.0.files.iter().cloned().collect();
                        merged_files.extend(child.files);
                        existing.0.file_count = merged_files.len();
                        let mut sorted_files: Vec<String> = merged_files.into_iter().collect();
                        sorted_files.sort();
                        existing.0.files = sorted_files;
                    } else {
                        if child_bm.is_empty() {
                            existing.2 = existing.2.saturating_add(child.file_count);
                        } else {
                            existing.1 |= &child_bm;
                        }
                        existing.0.file_count = (existing.1.len() as usize).saturating_add(existing.2);
                        existing.0.files.clear();
                    }
                    existing.0.children.extend(child.children);
                } else {
                    let extra = if child_bm.is_empty() { child.file_count } else { 0 };
                    sibling_map.insert(key, (child, child_bm, extra));
                }
            }
            let mut current_children_clean: Vec<TaxonomyNode> = sibling_map
                .into_values()
                .map(|(mut n, _, _)| {
                    dedup_and_sort_children(&mut n.children, include_files);
                    n
                })
                .collect();

            current_children_clean.sort_by(|a, b| {
                let order_a = if a.sort_order > 0 { a.sort_order } else { i64::MAX };
                let order_b = if b.sort_order > 0 { b.sort_order } else { i64::MAX };
                if order_a != order_b {
                    order_a.cmp(&order_b)
                } else {
                    a.code.cmp(&b.code)
                }
            });

            tax_node.children = current_children_clean;
            visited.remove(path);
            Some((vec![(tax_node, aggregate_bitmap.clone())], aggregate_bitmap, aggregate_files))
        }
    }
}

/// 同父重挂后递归归并同名兄弟节点并保持 `sort_order -> code` 确定性排序
fn dedup_and_sort_children(children: &mut Vec<TaxonomyNode>, include_files: bool) {
    if children.is_empty() {
        return;
    }
    let taken = std::mem::take(children);
    let mut merged: BTreeMap<String, TaxonomyNode> = BTreeMap::new();
    for mut node in taken {
        dedup_and_sort_children(&mut node.children, include_files);
        let key = if !node.name.trim().is_empty() {
            node.name.trim().to_string()
        } else {
            node.code.clone()
        };
        if let Some(existing) = merged.get_mut(&key) {
            if include_files {
                let mut set: HashSet<String> = existing.files.iter().cloned().collect();
                set.extend(node.files);
                existing.file_count = set.len();
                let mut sorted: Vec<String> = set.into_iter().collect();
                sorted.sort();
                existing.files = sorted;
            } else {
                existing.file_count = existing.file_count.saturating_add(node.file_count);
                existing.files.clear();
            }
            existing.children.extend(node.children);
            dedup_and_sort_children(&mut existing.children, include_files);
        } else {
            merged.insert(key, node);
        }
    }
    let mut out: Vec<TaxonomyNode> = merged.into_values().collect();
    out.sort_by(|a, b| {
        let order_a = if a.sort_order > 0 { a.sort_order } else { i64::MAX };
        let order_b = if b.sort_order > 0 { b.sort_order } else { i64::MAX };
        if order_a != order_b {
            order_a.cmp(&order_b)
        } else {
            a.code.cmp(&b.code)
        }
    });
    *children = out;
}

/// 递归统计树节点总数
pub(super) fn count_nodes(nodes: &[TaxonomyNode]) -> usize {
    let mut count = nodes.len();
    for node in nodes {
        count += count_nodes(&node.children);
    }
    count
}
