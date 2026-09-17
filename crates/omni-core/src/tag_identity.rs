//! Builtin 受控标签跨语言身份（omni-core）
//!
//! Spec: issue-omni-i18n-tag-identity-spec
//! - 多别名（zh/en/...）→ 同一 code（期望复用）
//! - code 源为英文规范名：builtin.{en_slug}（方案 B，无 hash；闭环登记 + 别名复用）
//! - 未命中别名时由调用方派生 _ext，禁止向量模糊还原

use std::collections::HashMap;
use std::sync::OnceLock;

/// 与 Desktop `enBuiltinCode` / `contentHash8` 算法对齐（SHA-256 hex 前 8 位）
pub fn content_hash8(input: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(input.as_bytes());
    let hex = format!("{:x}", hasher.finalize());
    hex[..8.min(hex.len())].to_string()
}

fn sanitize_en_slug(raw: &str) -> String {
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
    trimmed
}

/// 以英文规范名派生 builtin code（方案 B：builtin.{en_slug}，无 hash）
/// slug 为空时过渡 h_{hash}；唯一性由构建门禁与别名表保证。
pub fn en_builtin_code(en_name: &str) -> String {
    let trimmed = en_name.trim();
    let slug = sanitize_en_slug(trimmed);
    if slug.is_empty() {
        let hash = content_hash8(trimmed);
        format!("builtin.h_{}", &hash[..hash.len().min(10)])
    } else {
        format!("builtin.{}", slug)
    }
}

/// 开放集扩展标签 code：_ext.{slug}.{hash8(原串)}
pub fn derive_ext_tag_code(tag: &str) -> String {
    let tag_clean = tag.trim();
    let hash8 = content_hash8(tag_clean);
    let slug = sanitize_en_slug(tag_clean);
    if slug.is_empty() {
        format!("_ext.h_{}", &hash8[..hash8.len().min(10)])
    } else {
        format!("_ext.{}.{}", slug, hash8)
    }
}

/// 规范化 lemma：NFKC + trim + lowercase（与 TS normalizeLemma 对齐）
pub fn normalize_lemma(lemma: &str) -> String {
    use unicode_normalization::UnicodeNormalization;
    lemma.nfkc().collect::<String>().trim().to_lowercase()
}

/// 受控别名表：(别名, 英文规范名)
/// 同一概念的多语言别名必须指向同一 en 规范名。
const BUILTIN_ALIASES: &[(&str, &str)] = &[
    // 文件类型 / 版面等规则高频项
    ("文本", "Text"),
    ("纯文字", "Plain Text"),
    ("文档", "Document"),
    ("图片", "Image"),
    ("视频", "Video"),
    ("音频", "Audio"),
    ("压缩包", "Compressed Archive"),
    ("源代码", "Source Code"),
    ("程序", "Executable Program"),
    ("字体", "Font"),
    ("模型", "3D Model"),
    ("系统文件", "System Files"),
    ("数据库", "Database"),
    ("磁盘映像", "Disk Image"),
    ("应用数据", "Application Data"),
    ("电子书", "E-Book"),
    // 闭环视觉规则高频项
    ("截图", "Screenshot"),
    ("设计稿", "Design Draft"),
    ("图纸", "Blueprint"),
    ("表情包", "Meme"),
    ("证照", "ID Document"),
    ("合同票据", "Contract or Invoice"),
    ("海报宣发", "Poster Promo"),
    ("医学影像", "Medical Imaging"),
    ("网页长截图", "Long Webpage Screenshot"),
    ("UI界面截图", "UI Screenshot"),
    ("聊天截图", "Chat Screenshot"),
    ("代码截图", "Code Screenshot"),
    ("游戏截图", "Game Screenshot"),
    ("有字图", "Image With Text"),
    ("无字图", "Image Without Text"),
    ("全彩", "Full Color"),
    ("黑白", "Black And White"),
    ("摄影照片", "Photo"),
    ("人像写真", "Portrait"),
    ("人物照", "Person Photo"),
    ("私房写真", "Boudoir Photo"),
    ("婚纱照", "Wedding Photo"),
    ("静物照", "Still Life Photo"),
    ("自然景观", "Natural Landscape"),
    ("城市建筑", "City Architecture"),
    ("旅行照", "Travel Photo"),
    ("风景照", "Scenery Photo"),
    ("宠物照", "Pet Photo"),
    ("二次元", "Anime Style"),
    ("动漫", "Anime"),
    ("插画", "Illustration"),
    ("漫画", "Comic"),
    ("手绘", "Hand Drawn"),
    ("代码", "Code"),
    ("保密", "Confidential"),
    // 视觉门控补充
    ("名人", "Celebrity"),
    ("社会名流", "Celebrity"),
    ("历史遗迹", "Historic Site"),
    ("户外活动", "Outdoor Activity"),
    ("夜景照", "Night Photo"),
    ("怀旧", "Nostalgic"),
    ("建筑摄影", "Architecture Photo"),
    ("微距摄影", "Macro Photo"),
    ("街拍抓拍", "Street Photo"),
    ("航空航拍", "Aerial Photo"),
    ("青年漫", "Youth Comic"),
    ("少年漫", "Boys Comic"),
    ("少女漫", "Girls Comic"),
    ("成人漫", "Adult Comic"),
    ("实拍", "Real Shot"),
    ("CG渲染", "CG Render"),
    ("AI生成", "AI Generated"),
    ("绘画", "Painting"),
    ("应用图标", "App Icon"),
    ("高饱和鲜艳", "High Saturation"),
    ("暖色调", "Warm Tone"),
    ("冷色调", "Cool Tone"),
    ("中性黑白灰", "Neutral Gray"),
    ("透明背景", "Transparent Background"),
    ("实景背景", "Real Scene Background"),
    ("人物主体", "Human Subject"),
    ("动物宠物", "Animal Pet"),
    ("植物花草", "Plant Flower"),
    ("静物商品", "Still Life Product"),
    ("环境建筑", "Environment Building"),
    ("无人空镜", "Empty Scene"),
    ("单人", "Single Person"),
    ("双人", "Two Persons"),
    ("多人合影", "Group Photo"),
    ("色情", "Pornography"),
    ("血腥", "Gore"),
    ("涉政", "Political Sensitive"),
    ("违规", "Violation"),
    ("高ISO噪点", "High ISO Noise"),
    ("逆光死白", "Backlight Blowout"),
    ("暗光欠曝", "Underexposed"),
    // 英文模型直出别名（归一到同一概念）
    ("text", "Text"),
    ("document", "Document"),
    ("image", "Image"),
    ("screenshot", "Screenshot"),
    ("screen capture", "Screenshot"),
    ("design draft", "Design Draft"),
    ("blueprint", "Blueprint"),
    ("meme", "Meme"),
    ("id document", "ID Document"),
    ("invoice", "Invoice"),
    ("contract", "Contract"),
    ("full color", "Full Color"),
    ("black and white", "Black And White"),
    ("black_and_white", "Black And White"),
    ("image with text", "Image With Text"),
    ("image without text", "Image Without Text"),
    ("text image", "Image With Text"),
    ("anime", "Anime"),
    ("illustration", "Illustration"),
    ("comic", "Comic"),
    ("portrait", "Portrait"),
    ("person photo", "Person Photo"),
    ("still life photo", "Still Life Photo"),
    ("natural landscape", "Natural Landscape"),
    ("scenery photo", "Scenery Photo"),
    ("travel photo", "Travel Photo"),
    ("night photo", "Night Photo"),
    ("real shot", "Real Shot"),
    ("cg render", "CG Render"),
    ("painting", "Painting"),
    ("photo", "Photo"),
    ("black and white", "Black And White"),
    ("full color", "Full Color"),
    ("pornography", "Pornography"),
    ("image with text", "Image With Text"),
    ("image without text", "Image Without Text"),
    ("ui screenshot", "UI Screenshot"),
    ("landscape", "Natural Landscape"),
    ("code screenshot", "Code Screenshot"),
    ("ui screenshot", "UI Screenshot"),
    ("chat screenshot", "Chat Screenshot"),
    ("confidential", "Confidential"),
    ("source code", "Source Code"),
];

fn alias_map() -> &'static HashMap<String, String> {
    static MAP: OnceLock<HashMap<String, String>> = OnceLock::new();
    MAP.get_or_init(|| {
        let mut m = HashMap::new();
        for (alias, en) in BUILTIN_ALIASES {
            m.insert(normalize_lemma(alias), (*en).to_string());
            m.insert(normalize_lemma(en), (*en).to_string());
        }
        m
    })
}

fn en_to_code() -> &'static HashMap<String, String> {
    static MAP: OnceLock<HashMap<String, String>> = OnceLock::new();
    MAP.get_or_init(|| {
        let mut m = HashMap::new();
        for (_, en) in BUILTIN_ALIASES {
            m.insert((*en).to_string(), en_builtin_code(en));
        }
        m
    })
}

/// 别名/规范名 → builtin code（精确字典，非向量）
pub fn builtin_tag_code(tag: &str) -> Option<&'static str> {
    let key = normalize_lemma(tag);
    let en = alias_map().get(&key)?;
    // 再取 code 的 'static 引用
    en_to_code().get(en.as_str()).map(|s| s.as_str())
}

/// 是否与规范概念同 code（规则层用）
pub fn tag_matches_concept(tag: &str, canonical_zh_or_en: &str) -> bool {
    match (builtin_tag_code(tag), builtin_tag_code(canonical_zh_or_en)) {
        (Some(a), Some(b)) => a == b,
        _ => tag.trim() == canonical_zh_or_en.trim(),
    }
}

/// 标签串归一：命中别名 → builtin code；未命中 → _ext 派生
pub fn normalize_tag_to_code(tag: &str) -> String {
    if let Some(code) = builtin_tag_code(tag) {
        return code.to_string();
    }
    // 已是合法 code 形态则原样返回
    let t = tag.trim();
    if t.starts_with("builtin.") || t.starts_with("_ext.") || t.starts_with("omw.") {
        return t.to_string();
    }
    derive_ext_tag_code(t)
}

/// 将标签列表归一为 code 集合（P0 幂等契约：zh/en 别名输入应产出相同集合）
pub fn normalize_tag_set_to_codes(tags: &[String]) -> Vec<String> {
    let mut codes: Vec<String> = tags.iter().map(|t| normalize_tag_to_code(t)).collect();
    codes.sort();
    codes.dedup();
    codes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn en_builtin_code_matches_ts_algorithm_shape() {
        let code = en_builtin_code("Screenshot");
        // 方案 B：无 hash
        assert_eq!(code, "builtin.screenshot");
        assert_eq!(code, en_builtin_code("Screenshot"));
        assert_eq!(en_builtin_code("Source Code"), "builtin.source_code");
    }

    #[test]
    fn zh_and_en_aliases_share_same_code() {
        let zh = builtin_tag_code("截图").expect("截图");
        let en = builtin_tag_code("Screenshot").expect("Screenshot");
        let en2 = builtin_tag_code("screen capture").expect("screen capture");
        assert_eq!(zh, en);
        assert_eq!(en, en2);
        assert_eq!(zh, en_builtin_code("Screenshot"));
    }

    #[test]
    fn tag_matches_concept_works_across_languages() {
        assert!(tag_matches_concept("Screenshot", "截图"));
        assert!(tag_matches_concept("有字图", "Image With Text"));
        assert!(!tag_matches_concept("Screenshot", "设计稿"));
    }

    #[test]
    fn normalize_tag_set_is_idempotent_across_locales() {
        let zh_set = vec![
            "截图".to_string(),
            "设计稿".to_string(),
            "有字图".to_string(),
        ];
        let en_set = vec![
            "Screenshot".to_string(),
            "Design Draft".to_string(),
            "Image With Text".to_string(),
        ];
        let a = normalize_tag_set_to_codes(&zh_set);
        let b = normalize_tag_set_to_codes(&en_set);
        assert_eq!(a, b);
        assert_eq!(a.len(), 3);
    }

    #[test]
    fn unknown_tag_falls_back_to_ext() {
        let code = normalize_tag_to_code("深度强化学习");
        assert!(code.starts_with("_ext."));
        // 不同原串同 slug 时 hash 不同
        let c2 = normalize_tag_to_code("Deep Reinforcement Learning");
        assert_ne!(code, c2);
    }

    #[test]
    fn p0_vision_gating_concept_matching() {
        // 英文模型输出与中文规则 canonical 对齐（vision 门控语义）
        assert!(tag_matches_concept("Screenshot", "截图"));
        assert!(tag_matches_concept("Portrait", "人像写真"));
        assert!(tag_matches_concept("Person Photo", "人物照"));
        assert!(tag_matches_concept("Natural Landscape", "自然景观"));
        assert!(tag_matches_concept("Image Without Text", "无字图"));
        assert!(tag_matches_concept("Design Draft", "设计稿"));
        assert!(!tag_matches_concept("Screenshot", "人像写真"));

        // zh/en 候选集经归一后 code 集合一致（P0）
        let zh = vec![
            "截图".to_string(),
            "人像写真".to_string(),
            "无字图".to_string(),
        ];
        let en = vec![
            "Screenshot".to_string(),
            "Portrait".to_string(),
            "Image Without Text".to_string(),
        ];
        assert_eq!(normalize_tag_set_to_codes(&zh), normalize_tag_set_to_codes(&en));
        assert_eq!(en_builtin_code("Screenshot"), "builtin.screenshot");
    }

    #[test]
    fn ext_same_slug_different_raw_are_distinct() {
        let a = derive_ext_tag_code("Go");
        let b = derive_ext_tag_code("go");
        assert_ne!(a, b);
        assert!(a.starts_with("_ext."));
    }
}
