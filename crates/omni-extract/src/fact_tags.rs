//! 原生事实标签抽取引擎 (Task 2，原「元数据标签抽取」)
//!
//! 产出**全部** `fact` 组标签（`fact_tags`）——既包含元数据直读，也包含下沉的既有物理事实。
//! 之所以不再叫 `meta_tags`：这些事实标签**并不都来自元数据**，名称需与「标签来源分组」对齐
//! （见 ADR-0045 §Decision 1 与根目录 `CONTEXT.md`「标签来源分组」）。
//!
//! - 音频维度：Artist / Album / Genre → 作者/歌手、专辑、音乐流派
//! - 视觉器材与工具维度：Make / Model / Software → 相机品牌、器材型号、创作软件
//! - 文档维度：Author / Company / Organization → 作者、出品机构
//! - 下沉既有物理事实：file_source / workflow_state / security_level / 质量等级 / 语言细分
//!
//! 所有产出统一封装为 [`omni_core::TagChainItem`]，置信度**扁平统一为 0.90**
//! （物理直读与规则推导同级，历史各自的取值一律作废，见 ADR-0045 §Decision 1/2）；
//! `engine` 统一标记为 `"metadata"`（产出引擎标识，与 `tag_group` 正交）。

use omni_core::tag_thresholds::EXIT_CONFIDENCE_THRESHOLD;
#[cfg(test)]
use omni_core::tag_thresholds::LAYER_FALLBACK_PHYSICAL;
use omni_core::TagChainItem;
use serde_json::Value;

/// 事实标签打标引擎标识（`TagChainItem.engine` 的封闭词表成员之一，与来源分组 `fact` 正交）
pub const ENGINE_METADATA: &str = "metadata";

/// 事实标签置信度基准：**扁平 0.90**（ADR-0045 §Decision 1/2）。
///
/// 由 1.0 降为 0.90 —— 程序检测无法保证 100% 正确；且「物理事实绝对覆盖律」已改由
/// **标签来源分组的优先序**（`fact > fused > visual > ai`）保证，不再依赖置信度数值大小。
/// 事实组内不再区分物理直读与规则推导，故只需一个常量。
const CONF_FACT: f32 = 0.90;

/// 事实标签抽取上下文：承载元数据 JSON 与已确诊的下沉物理事实
#[derive(Debug, Clone, Default)]
pub struct FactTagContext<'a> {
    /// 完整元数据 JSON (含 exiftool / audio / document / image 等子树)
    pub metadata: &'a Value,
    /// 文件来源展示名 (网络下载 / 局域网共享 / 本地磁盘 / 系统文件)
    pub file_source: Option<String>,
    /// 处理状态展示名 (草稿 / 待审核 / 已完成 / 已归档 / 未归档)
    pub workflow_state: Option<String>,
    /// 安全密级展示名 (绝密 / 机密 / 内部 / 公开)
    pub security_level: Option<String>,
    /// 文件质量评分 (1.0 ~ 10.0)，用于映射质量等级
    pub quality_score: Option<f32>,
    /// 语言细分展示名 (中文 / 英文)
    pub language_label: Option<String>,
}

/// 原生事实标签抽取引擎
pub struct OmniFactTagExtractor;

impl OmniFactTagExtractor {
    /// 从元数据 JSON 与下沉物理事实中抽取全套 `fact_tags`
    ///
    /// 输出顺序稳定：先元数据实体维度（类别/作者/专辑/流派/器材/软件/机构），
    /// 再下沉物理事实维度（来源/状态/密级/质量/语言），保证 Desktop 侧展示与落库顺序可预期。
    pub fn extract(ctx: &FactTagContext<'_>) -> Vec<TagChainItem> {
        let mut tags: Vec<TagChainItem> = Vec::new();

        // 1. 元数据实体维度抽取 (音频 / 视觉器材 / 文档)
        Self::extract_audio_dimensions(ctx.metadata, &mut tags);
        Self::extract_visual_equipment_dimensions(ctx.metadata, &mut tags);
        Self::extract_document_dimensions(ctx.metadata, &mut tags);

        // 2. 下沉既有物理事实 (桌面端 Node.js 预检管线既有确诊维度)
        Self::push_fact(&mut tags, "文件来源", ctx.file_source.as_deref(), "dim.file_source", CONF_FACT);
        Self::push_fact(&mut tags, "处理状态", ctx.workflow_state.as_deref(), "dim.workflow_state", CONF_FACT);
        Self::push_fact(&mut tags, "安全等级", ctx.security_level.as_deref(), "dim.security_level", CONF_FACT);
        Self::push_quality_grade(&mut tags, ctx.quality_score);
        Self::push_fact(&mut tags, "语言", ctx.language_label.as_deref(), "dim.language", CONF_FACT);

        // 出口一致性保障：fact_tags 为确定性物理事实，仍统一校验置信度不低于出口门限
        tags.retain(|t| t.confidence >= EXIT_CONFIDENCE_THRESHOLD);

        tracing::info!(
            "[事实标签抽取:fact_tags] 抽取完成，标签总数: {}, 明细: {:?}",
            tags.len(),
            tags.iter().map(|t| (t.name.as_str(), t.code.as_str())).collect::<Vec<_>>()
        );
        tags
    }

    /// 音频维度：Artist → 作者/歌手；Album → 专辑；Genre → 音乐流派
    fn extract_audio_dimensions(metadata: &Value, out: &mut Vec<TagChainItem>) {
        // 元数据可能位于 metadata["audio"] 或 metadata["exiftool"] 子树
        for scope in Self::metadata_scopes(metadata) {
            // Artist / Author / Composer → 歌手 / 作者
            for key in ["Artist", "artist", "AlbumArtist", "Author", "composer", "Composer"] {
                if let Some(val) = Self::get_str(scope, key) {
                    // 复用既有作者清洗规则，过滤 admin / unknown 等噪声签名
                    let (joined, list) = crate::clean_and_split_authors(&val);
                    if list.is_empty() {
                        continue;
                    }
                    let display = if joined.is_empty() { val.trim().to_string() } else { joined };
                    Self::push_tag(
                        out,
                        "作者",
                        Self::ext_code("creator", &display),
                        &display,
                        CONF_FACT,
                        Some(key),
                        &val,
                    );
                }
            }
            // Album → 专辑
            for key in ["Album", "album"] {
                if let Some(val) = Self::get_str(scope, key) {
                    Self::push_tag(
                        out,
                        "专辑",
                        Self::ext_code("album", &val),
                        &val,
                        CONF_FACT,
                        Some(key),
                        &val,
                    );
                }
            }
            // Genre → 音乐流派 (受控维度优先，未受控时降级为扩展 code)
            for key in ["Genre", "genre"] {
                if let Some(val) = Self::get_str(scope, key) {
                    let code = omni_core::tag_identity::resolve_controlled_tag_code(&val)
                        .map(|c| c.to_string())
                        .unwrap_or_else(|| Self::ext_code("music_genre", &val));
                    Self::push_tag(out, "音乐流派", code, &val, CONF_FACT, Some(key), &val);
                }
            }
        }
    }

    /// 视觉器材与工具维度：Make/Model → 相机品牌与器材型号；Software → 创作软件
    fn extract_visual_equipment_dimensions(metadata: &Value, out: &mut Vec<TagChainItem>) {
        for scope in Self::metadata_scopes(metadata) {
            // Make → 相机品牌
            for key in ["Make", "make"] {
                if let Some(val) = Self::get_str(scope, key) {
                    Self::push_tag(
                        out,
                        "相机品牌",
                        Self::ext_code("camera_brand", &val),
                        &val,
                        CONF_FACT,
                        Some(key),
                        &val,
                    );
                }
            }
            // Model → 器材型号 (跳过与 Make 重复的简陋值)
            for key in ["Model", "model"] {
                if let Some(val) = Self::get_str(scope, key) {
                    if val.chars().count() < 2 {
                        continue;
                    }
                    Self::push_tag(
                        out,
                        "器材型号",
                        Self::ext_code("camera_model", &val),
                        &val,
                        CONF_FACT,
                        Some(key),
                        &val,
                    );
                }
            }
            // Software / CreatorTool → 创作软件 (受控反查 Photoshop / Blender 等)
            // Producer 字段常混入机构名，交由文档维度（出品机构）处理，此处不消费
            for key in ["Software", "software", "CreatorTool", "creator_tool"] {
                if let Some(val) = Self::get_str(scope, key) {
                    let name = Self::normalize_software_name(&val);
                    if name.is_empty() {
                        continue;
                    }
                    let code = omni_core::tag_identity::resolve_controlled_tag_code(&name)
                        .map(|c| c.to_string())
                        .unwrap_or_else(|| Self::ext_code("creation_tool", &name));
                    Self::push_tag(out, "创作软件", code, &name, CONF_FACT, Some(key), &val);
                }
            }
        }
    }

    /// 文档维度：Author → 文档作者；Company / Organization / Producer → 出品机构
    fn extract_document_dimensions(metadata: &Value, out: &mut Vec<TagChainItem>) {
        for scope in Self::metadata_scopes(metadata) {
            // Company / Organization / Producer → 出品机构
            for key in ["Company", "company", "Organization", "organization", "Producer"] {
                if let Some(val) = Self::get_str(scope, key) {
                    if val.chars().count() < 2 {
                        continue;
                    }
                    Self::push_tag(
                        out,
                        "出品机构",
                        Self::ext_code("organization", &val),
                        &val,
                        CONF_FACT,
                        Some(key),
                        &val,
                    );
                }
            }
        }
    }

    /// 文件质量评分 → 质量等级 (高质量 / 中等质量 / 低质量)
    fn push_quality_grade(out: &mut Vec<TagChainItem>, quality_score: Option<f32>) {
        let Some(score) = quality_score else { return };
        let grade = if score >= 8.0 {
            "高质量"
        } else if score >= 5.0 {
            "中等质量"
        } else {
            "低质量"
        };
        Self::push_tag(
            out,
            "文件质量",
            Self::ext_code("file_quality", grade),
            grade,
            CONF_FACT,
            Some("quality_score"),
            &score.to_string(),
        );
    }

    /// 统一标签入池：去重 + 结构化 Tracing 日志
    #[allow(clippy::too_many_arguments)]
    fn push_tag(
        out: &mut Vec<TagChainItem>,
        dimension_name: &str,
        code: String,
        name: &str,
        confidence: f32,
        source_key: Option<&str>,
        raw_val: &str,
    ) {
        let clean_name = name.trim();
        if clean_name.is_empty() {
            return;
        }
        // 同名标签去重（后到者不覆盖已确诊事实）
        if out.iter().any(|t| t.name == clean_name) {
            return;
        }
        let item = TagChainItem {
            code,
            name: clean_name.to_string(),
            confidence,
            engine: Some(ENGINE_METADATA.to_string()),
            source: Some(ENGINE_METADATA.to_string()),
            ..Default::default()
        };
        tracing::info!(
            "[事实标签抽取:fact_tags] key={}, val={} -> tag={}({}), code={}, conf={:.2}",
            source_key.unwrap_or("-"),
            raw_val,
            item.name,
            dimension_name,
            item.code,
            item.confidence
        );
        out.push(item);
    }

    /// 下沉物理事实入池（展示名已由上层确诊，此处仅做承载）
    fn push_fact(
        out: &mut Vec<TagChainItem>,
        dimension_name: &str,
        value: Option<&str>,
        code: &str,
        confidence: f32,
    ) {
        let Some(val) = value else { return };
        Self::push_tag(
            out,
            dimension_name,
            format!("{}.{}", code, Self::slug(val)),
            val,
            confidence,
            Some(dimension_name),
            val,
        );
    }

    /// 元数据候选作用域：优先各类型子树，兜底直接读顶层 (扁平 exiftool 映射)
    fn metadata_scopes(metadata: &Value) -> Vec<&Value> {
        const SUBTREES: [&str; 6] = ["exiftool", "audio", "document", "image", "video", "metadata"];
        let mut scopes: Vec<&Value> = Vec::new();
        for key in SUBTREES {
            if let Some(v) = metadata.get(key) {
                if v.is_object() {
                    scopes.push(v);
                }
            }
        }
        if metadata.is_object() {
            scopes.push(metadata);
        }
        scopes
    }

    /// 读取字符串字段（大小写敏感的直接命中 + 大小写不敏感的兜底扫描）
    fn get_str(scope: &Value, key: &str) -> Option<String> {
        if let Some(v) = scope.get(key) {
            if let Some(s) = v.as_str() {
                let trimmed = s.trim();
                if !trimmed.is_empty() && trimmed != "0" {
                    return Some(trimmed.to_string());
                }
            }
        }
        let obj = scope.as_object()?;
        let key_lower = key.to_ascii_lowercase();
        for (k, v) in obj {
            if k.to_ascii_lowercase() == key_lower {
                if let Some(s) = v.as_str() {
                    let trimmed = s.trim();
                    if !trimmed.is_empty() && trimmed != "0" {
                        return Some(trimmed.to_string());
                    }
                }
            }
        }
        None
    }

    /// 创作软件名归一化：剥离版本号与厂商前缀，保留可识别的工具主体名
    ///
    /// 例："Adobe Photoshop 25.0 (Windows)" → "Adobe Photoshop"
    fn normalize_software_name(raw: &str) -> String {
        let mut s = raw.trim().to_string();
        if s.is_empty() {
            return s;
        }
        // 剥离括号注释
        if let Some(idx) = s.find(" (") {
            s.truncate(idx);
        }
        // 剥离尾部版本号 (如 " 25.0" / " v3.1.4")
        // 仅当 "v/V" 后紧跟数字时才视为版本前缀剥离，避免误吞词尾字母 (如 "AV" → "A")
        let mut end = s.len();
        let bytes: Vec<char> = s.chars().collect();
        let mut i = bytes.len();
        while i > 0 {
            let c = bytes[i - 1];
            let is_version_char =
                c.is_ascii_digit() || c == '.' || ((c == 'v' || c == 'V') && i < bytes.len() && bytes[i].is_ascii_digit());
            if is_version_char {
                i -= 1;
            } else {
                break;
            }
        }
        if i < bytes.len() && i > 0 {
            // 仅当剥离后仍留有字母主体时才截断
            let head: String = bytes[..i].iter().collect();
            if head.chars().any(|c| c.is_alphabetic()) {
                end = head.len();
            }
        }
        s.truncate(end);
        s.trim().to_string()
    }

    /// 生成稳定 slug（用于扩展 code 规范化，保证同一实体 code 幂等）
    ///
    /// 仅保留 ASCII 字母数字，其余（含 CJK）折叠为下划线；
    /// 若折叠后为空（如纯中文实体名），回退为确定性 `hash8` 前缀，避免 code 冲突。
    fn slug(raw: &str) -> String {
        let lower = raw.trim().to_lowercase();
        let mut out = String::with_capacity(lower.len());
        let mut last_us = false;
        for ch in lower.chars() {
            if ch.is_ascii_alphanumeric() {
                out.push(ch);
                last_us = false;
            } else if !last_us && !out.is_empty() {
                out.push('_');
                last_us = true;
            }
        }
        let trimmed = out.trim_matches('_').to_string();
        if trimmed.is_empty() {
            omni_core::tag_identity::content_hash8(raw.trim())
        } else {
            trimmed
        }
    }

    /// 为开放集实体派生 `_ext.{slug}.{hash8}` 形式的扩展 code（与 Desktop 端约定对齐）
    fn ext_code(prefix: &str, name: &str) -> String {
        let clean = name.trim();
        format!(
            "_ext.{}.{}.{}",
            prefix,
            Self::slug(clean),
            omni_core::tag_identity::content_hash8(clean)
        )
    }
}

/// 便捷入口：基于元数据 JSON 抽取 `fact_tags`（无下沉物理事实）
pub fn extract_fact_tags(metadata: &Value) -> Vec<TagChainItem> {
    OmniFactTagExtractor::extract(&FactTagContext {
        metadata,
        ..Default::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// 音频元数据：Artist / Album / Genre 应全部产出
    #[test]
    fn test_extract_audio_metadata_tags() {
        let metadata = json!({
            "exiftool": {
                "Artist": "Beyond",
                "Album": "乐与怒",
                "Genre": "摇滚"
            }
        });
        let tags = extract_fact_tags(&metadata);
        let names: Vec<&str> = tags.iter().map(|t| t.name.as_str()).collect();

        assert!(names.contains(&"Beyond"), "应提取歌手 Artist，实际: {:?}", names);
        assert!(names.contains(&"乐与怒"), "应提取专辑 Album，实际: {:?}", names);
        assert!(names.contains(&"摇滚"), "应提取音乐流派 Genre，实际: {:?}", names);

        for tag in &tags {
            assert_eq!(tag.engine.as_deref(), Some(ENGINE_METADATA), "engine 应统一为 metadata");
            assert_eq!(tag.confidence, 0.90, "事实标签置信度应为 0.90: {:?}", tag);
            assert!(!tag.code.is_empty(), "标签必须携带 code: {:?}", tag);
        }
    }

    /// 图像元数据：Make / Model / Software 应全部产出，且软件名剥离版本号
    #[test]
    fn test_extract_image_equipment_tags() {
        let metadata = json!({
            "exiftool": {
                "Make": "Canon",
                "Model": "Canon EOS R5",
                "Software": "Adobe Photoshop 25.0 (Windows)"
            }
        });
        let tags = extract_fact_tags(&metadata);
        let names: Vec<&str> = tags.iter().map(|t| t.name.as_str()).collect();

        assert!(names.contains(&"Canon"), "应提取相机品牌 Make，实际: {:?}", names);
        assert!(names.contains(&"Canon EOS R5"), "应提取器材型号 Model，实际: {:?}", names);
        assert!(
            names.contains(&"Adobe Photoshop"),
            "应提取创作软件并剥离版本号，实际: {:?}",
            names
        );
    }

    /// 文档元数据：Author / Company 应全部产出
    #[test]
    fn test_extract_document_metadata_tags() {
        let metadata = json!({
            "document": {
                "Author": "张三",
                "Company": "腾讯科技有限公司"
            }
        });
        let tags = extract_fact_tags(&metadata);
        let names: Vec<&str> = tags.iter().map(|t| t.name.as_str()).collect();

        assert!(names.contains(&"张三"), "应提取文档作者 Author，实际: {:?}", names);
        assert!(
            names.contains(&"腾讯科技有限公司"),
            "应提取出品机构 Company，实际: {:?}",
            names
        );

        let company = tags.iter().find(|t| t.name == "腾讯科技有限公司").unwrap();
        assert_eq!(company.confidence, 0.90, "规则推导事实与物理直读同级，应为 0.90");
    }

    /// 下沉物理事实：来源/状态/密级/质量/语言 应随元数据一并产出
    #[test]
    fn test_downshift_physical_facts_and_quality_grade() {
        let metadata = json!({});
        let tags = OmniFactTagExtractor::extract(&FactTagContext {
            metadata: &metadata,
            file_source: Some("网络下载".to_string()),
            workflow_state: Some("草稿".to_string()),
            security_level: Some("内部".to_string()),
            quality_score: Some(8.7),
            language_label: Some("中文".to_string()),
        });
        let names: Vec<&str> = tags.iter().map(|t| t.name.as_str()).collect();

        for expected in ["网络下载", "草稿", "内部", "高质量", "中文"] {
            assert!(names.contains(&expected), "应下沉物理事实 {}，实际: {:?}", expected, names);
        }
    }

    /// 质量等级阈值边界映射
    #[test]
    fn test_quality_grade_thresholds() {
        let metadata = json!({});
        let grade = |score: f32| -> Option<String> {
            OmniFactTagExtractor::extract(&FactTagContext {
                metadata: &metadata,
                quality_score: Some(score),
                ..Default::default()
            })
            .first()
            .map(|t| t.name.clone())
        };
        assert_eq!(grade(9.0).as_deref(), Some("高质量"));
        assert_eq!(grade(8.0).as_deref(), Some("高质量"));
        assert_eq!(grade(7.9).as_deref(), Some("中等质量"));
        assert_eq!(grade(5.0).as_deref(), Some("中等质量"));
        assert_eq!(grade(4.9).as_deref(), Some("低质量"));
    }

    /// 噪声作者签名 (admin / unknown) 必须被过滤
    #[test]
    fn test_noisy_author_filtered() {
        let metadata = json!({ "document": { "Author": "Administrator" } });
        let tags = extract_fact_tags(&metadata);
        assert!(
            tags.iter().all(|t| t.name != "Administrator"),
            "噪声作者签名应被过滤: {:?}",
            tags
        );
    }

    /// 所有产出标签置信度必须不低于出口门限
    #[test]
    fn test_all_tags_pass_exit_threshold() {
        let metadata = json!({
            "audio": { "artist": "Beyond", "genre": "Rock" },
            "exiftool": { "Model": "Canon EOS R5" }
        });
        let tags = extract_fact_tags(&metadata);
        assert!(!tags.is_empty());
        for tag in &tags {
            assert!(
                tag.confidence >= EXIT_CONFIDENCE_THRESHOLD,
                "fact_tags 置信度必须 >= 出口门限: {:?}",
                tag
            );
        }
        // 与物理层回退分对齐
        assert!(CONF_FACT >= LAYER_FALLBACK_PHYSICAL);
    }
}
