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
        format!("builtin.{}", &hash[..hash.len().min(10)])
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
        format!("_ext.{}", &hash8[..hash8.len().min(10)])
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
    ("同人本", "Doujinshi"),
    ("剧情", "Story"),
    ("二次元色情", "Anime Pornography"),
    ("软色情", "Soft Pornography"),
    ("性感", "Sexy"),
    ("擦边诱惑", "Borderline Tease"),
    ("血腥暴力", "Bloody Violence"),
    ("断头斩首", "Beheading"),
    ("肢解碎尸", "Dismemberment"),
    ("血腥虐杀", "Bloody Massacre"),
    ("露骨", "Explicit"),
    ("露骨性行为", "Explicit Sexual Act"),
    ("安全", "Safe"),
    ("全年龄", "All Ages"),
    ("水彩水墨", "Watercolor Ink"),
    ("四格漫", "Four Panel Comic"),
    ("页漫", "Page Comic"),
    ("条漫", "Webtoon"),
    ("欢乐", "Joyful"),
    ("悲伤", "Sad"),
    ("紧张", "Tense"),
    ("兴奋", "Excited"),
    ("压抑", "Depressed"),
    ("历史", "History"),
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
    ("长图", "Long Image"),
    ("正方形图", "Square Image"),
    ("横图", "Horizontal Image"),
    ("竖图", "Vertical Image"),
    ("横屏", "Landscape"),
    ("竖屏", "Portrait"),
    ("主题内容", "Topic"),
    ("topic", "Topic"),
    ("long image", "Long Image"),
    ("square image", "Square Image"),
    ("horizontal image", "Horizontal Image"),
    ("vertical image", "Vertical Image"),
    // 视觉细分与票据证照 (无 hash 规范化)
    ("合同", "Contract"),
    ("发票", "Invoice"),
    ("红头文件", "Official Document"),
    ("论文", "Paper"),
    ("收据", "Receipt"),
    ("对账单", "Statement"),
    ("银行回单", "Bank Receipt"),
    ("采购订单", "Purchase Order"),
    ("报销凭证", "Expense Voucher"),
    ("保函协议", "Letter Of Guarantee"),
    ("投标文件", "Bidding Document"),
    ("身份证", "ID Card"),
    ("房产证", "Property Certificate"),
    ("护照", "Passport"),
    ("驾驶证", "Driver License"),
    ("行驶证", "Vehicle License"),
    ("车牌", "License Plate"),
    ("银行卡", "Bank Card"),
    ("营业执照", "Business License"),
    ("户口本", "Household Register"),
    ("毕业证", "Diploma"),
    ("学位证", "Degree Certificate"),
    ("工作证", "Work Permit"),
    ("结婚证", "Marriage Certificate"),
    ("社保卡", "Social Security Card"),
    ("特写", "Close Up"),
    ("半身", "Medium Shot"),
    ("全身", "Full Shot"),
    ("全景", "Panoramic Shot"),
    ("微距", "Macro"),
    ("日光", "Sunlight"),
    ("日落", "Sunset"),
    ("夜景", "Night Scene"),
    ("暗光", "Low Light"),
    ("室内光", "Indoor Light"),
    ("纯色背景", "Solid Background"),
    ("平视", "Eye Level"),
    ("俯视", "Top Down View"),
    ("仰视", "Low Angle View"),
    ("第一人称", "First Person"),
    ("图表为主", "Chart Dominant"),
    ("图文混合", "Text And Graphic"),
    ("手写笔记", "Handwritten Notes"),
    ("Windows截图", "Windows Screenshot"),
    ("macOS截图", "macOS Screenshot"),
    ("iOS截图", "iOS Screenshot"),
    ("Android截图", "Android Screenshot"),
    ("Linux截图", "Linux Screenshot"),
    ("正方形", "Square"),
    ("超宽长条", "Ultra Wide"),
    ("微量文本", "Minimal Text"),
    ("图文标题", "Graphic Title"),
    ("密集排版", "Dense Layout"),
    ("扁平极简", "Flat Minimalist"),
    ("写实拟真", "Photorealistic"),
    ("复古胶片", "Vintage Film"),
    ("赛博朋克", "Cyberpunk"),
    ("春季花景", "Spring Bloom"),
    ("夏季绿荫", "Summer Shade"),
    ("秋季金黄", "Autumn Foliage"),
    ("冬季雪景", "Winter Snow"),
    ("红色", "Red"),
    ("橙色", "Orange"),
    ("黄色", "Yellow"),
    ("绿色", "Green"),
    ("青色", "Cyan"),
    ("蓝色", "Blue"),
    ("紫色", "Purple"),
    ("粉色", "Pink"),
    ("棕色", "Brown"),
    ("白色", "White"),
    ("黑色", "Black"),
    ("灰色", "Gray"),
    ("商业志", "Commercial Manga"),
    ("虚焦", "Out Of Focus"),
    ("抖动", "Motion Blur"),
    ("曝光良好", "Good Exposure"),
    ("轻微欠曝", "Slight Underexposure"),
    ("轻微过曝", "Slight Overexposure"),
    ("模糊废片", "Blurry Wastage"),
    ("脱焦", "Defocused"),
    ("运动抖动", "Motion Shake"),
    ("曝光正常", "Normal Exposure"),
    ("无码", "Uncensored"),
    ("薄码", "Light Mosaic"),
    ("有码", "Censored"),
    ("无水印", "No Watermark"),
    ("轻水印", "Light Watermark"),
    ("有水印", "Watermarked"),
    ("高质量", "High Quality"),
    ("中等质量", "Medium Quality"),
    ("低质量", "Low Quality"),
    ("R-15", "R-15"),
    ("R-18", "R-18"),
    ("R-18G", "R-18G"),
    ("暴恐", "Violence Terror"),
    ("生殖器暴露", "Genital Exposure"),
    ("性虐调教", "Sadomasochism"),
    ("乱伦淫秽", "Incest Obscene"),
    ("强奸轮奸", "Rape Assault"),
    ("自慰高潮", "Masturbation"),
    ("调教拘束", "Bondage"),
    ("情色文娱", "Erotic Entertainment"),
    ("暴露走光", "Exposure Wardrobe Malfunction"),
    ("残肢断臂", "Severed Limbs"),
    ("尸体残骸", "Corpse Remains"),
    ("酷刑折磨", "Torture"),
    ("重口猎奇", "Hardcore Bizarre"),
    ("暴恐惨案", "Terror Tragedy"),
    ("自残放血", "Self Harm Bleeding"),
    ("血肉模糊", "Flesh Mutilation"),
    ("涉政违规", "Political Sensitive Violation"),
    ("反党反政", "Subversive Politics"),
    ("颠覆政权", "Subversion"),
    ("邪教分裂", "Cult Separatism"),
    ("邪教组织", "Cult Organization"),
    ("暴乱动乱", "Riots Turmoil"),
    ("违法违规", "Illegal Violation"),
    ("毒品交易", "Drug Trafficking"),
    ("走私贩私", "Smuggling"),
    ("网络赌博", "Online Gambling"),
    ("洗钱诈骗", "Money Laundering Fraud"),
    ("暗网黑产", "Darknet Underground"),
    ("洗钱黑产", "Laundering Industry"),
    ("电信诈骗", "Telecom Fraud"),
    ("各地美食", "Local Delicacy"),
    ("治愈", "Healing"),
    ("致郁", "Depressing"),
    ("轻松", "Relaxed"),
    ("感动", "Touching"),
    ("欲望", "Desire"),
    ("罪恶感", "Guilt"),
    ("背德感", "Transgression"),
    ("羞耻", "Shame"),
    ("支配", "Dominance"),
    ("写实", "Realistic"),
    ("内容标签", "Content Tag"),
    ("图片细分", "Image Subdivision"),
];

use std::sync::RwLock;

static DYNAMIC_ALIASES: OnceLock<RwLock<HashMap<String, &'static str>>> = OnceLock::new();

fn dynamic_aliases() -> &'static RwLock<HashMap<String, &'static str>> {
    DYNAMIC_ALIASES.get_or_init(|| RwLock::new(HashMap::new()))
}

/// 动态批量载入受控标签别名（由 SQLite 连接时读取 tag_aliases 表注入，或从 json 载入）
/// 传入 (lemma, tag_code)
///
/// 冲突规则：同一 lemma 同时命中 `omw.*` 与 `builtin.*` 时 **omw.* 优先**（中文概念优先反映射 OMW，禁止无脑 builtin/_ext）。
pub fn load_aliases_from_entries<I, S1, S2>(entries: I)
where
    I: IntoIterator<Item = (S1, S2)>,
    S1: AsRef<str>,
    S2: AsRef<str>,
{
    if let Ok(mut map) = dynamic_aliases().write() {
        for (lemma, code) in entries {
            let norm_lemma = normalize_lemma(lemma.as_ref());
            if norm_lemma.is_empty() {
                continue;
            }
            let code_ref = code.as_ref();
            let should_insert = match map.get(&norm_lemma) {
                Some(existing) => {
                    let existing_is_omw = existing.starts_with("omw.");
                    let incoming_is_omw = code_ref.starts_with("omw.");
                    let existing_is_builtin = existing.starts_with("builtin.");
                    let incoming_is_builtin = code_ref.starts_with("builtin.");
                    if incoming_is_omw && existing_is_builtin {
                        true
                    } else if existing_is_omw && incoming_is_builtin {
                        false
                    } else {
                        // 同前缀或其它受控形态：后写覆盖
                        true
                    }
                }
                None => true,
            };
            if should_insert {
                let leaked_code: &'static str = Box::leak(code_ref.to_string().into_boxed_str());
                map.insert(norm_lemma, leaked_code);
            }
        }
    }
}

/// 受控标签 code 反查（运行时归一入口）
///
/// 优先级：
/// 1. 动态别名字典（semantic.pack tag_aliases_{lang} 热载；同 lemma 时 omw.* 已优先）
/// 2. 静态 builtin 离线字典
///
/// 未命中返回 `None`，由调用方派生 `_ext.{slug}.{hash8}`。
pub fn resolve_controlled_tag_code(tag: &str) -> Option<&'static str> {
    let key = normalize_lemma(tag);
    if key.is_empty() {
        return None;
    }
    // 1. 动态轨（含 omw.*）
    if let Ok(dyn_map) = dynamic_aliases().read() {
        if let Some(&code) = dyn_map.get(&key) {
            return Some(code);
        }
    }
    // 2. 静态 builtin 字典（不经过 dynamic 再查，避免语义重复）
    let en = alias_map().get(&key)?;
    en_to_code().get(en.as_str()).map(|s| s.as_str())
}

/// 清空动态别名字典（用于热切换或断开连接时）
pub fn clear_dynamic_aliases() {
    if let Ok(mut map) = dynamic_aliases().write() {
        map.clear();
    }
}

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

fn code_to_canonical_zh() -> &'static HashMap<String, String> {
    static MAP: OnceLock<HashMap<String, String>> = OnceLock::new();
    MAP.get_or_init(|| {
        let mut m = HashMap::new();
        for (zh, en) in BUILTIN_ALIASES {
            let code = en_builtin_code(en);
            m.entry(code).or_insert_with(|| (*zh).to_string());
        }
        m
    })
}

fn code_to_canonical_en() -> &'static HashMap<String, String> {
    static MAP: OnceLock<HashMap<String, String>> = OnceLock::new();
    MAP.get_or_init(|| {
        let mut m = HashMap::new();
        for (_, en) in BUILTIN_ALIASES {
            let code = en_builtin_code(en);
            m.entry(code).or_insert_with(|| (*en).to_string());
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

/// 将标准 tag_code 反查对应语言的规范展示名称 (若未查到则提取 slug 兜底)
pub fn tag_display(code: &str, lang: &str) -> String {
    let clean_code = code.trim();
    let l = lang.to_lowercase();
    let is_zh = l.starts_with("zh");

    if is_zh {
        if let Some(zh) = code_to_canonical_zh().get(clean_code) {
            return zh.clone();
        }
    } else {
        if let Some(en) = code_to_canonical_en().get(clean_code) {
            return en.clone();
        }
    }

    // 针对 _ext.slug.hash 或 builtin.slug 提取人类可读部分
    if let Some(stripped) = clean_code.strip_prefix("builtin.") {
        return stripped.replace('_', " ");
    }
    if let Some(stripped) = clean_code.strip_prefix("_ext.") {
        if let Some(slug) = stripped.split('.').next() {
            if !slug.is_empty() {
                return slug.replace('_', " ");
            }
        }
    }

    clean_code.to_string()
}

/// 别名/规范名 → builtin code（精确字典，非向量）
pub fn builtin_tag_code(tag: &str) -> Option<&'static str> {
    let key = normalize_lemma(tag);
    // 1. 优先查动态载入字典（DB/JSON 热载入轨）
    if let Ok(dyn_map) = dynamic_aliases().read() {
        if let Some(&code) = dyn_map.get(&key) {
            return Some(code);
        }
    }
    // 2. 回退查静态内置别名字典（静态底座保底轨）
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

    #[test]
    fn dynamic_aliases_injection_and_lookup() {
        clear_dynamic_aliases();
        load_aliases_from_entries(vec![
            ("特种发票", "builtin.invoice"),
            ("Special Tax Invoice", "builtin.invoice"),
        ]);
        assert_eq!(builtin_tag_code("特种发票"), Some("builtin.invoice"));
        assert_eq!(builtin_tag_code("Special Tax Invoice"), Some("builtin.invoice"));
        assert_eq!(normalize_tag_to_code("特种发票"), "builtin.invoice");
        clear_dynamic_aliases();
    }

    #[test]
    fn dynamic_aliases_prefer_omw_over_builtin_for_same_lemma() {
        clear_dynamic_aliases();
        // 同 lemma：builtin 先写入，omw 后写入 → 反查必须得到 omw.*
        load_aliases_from_entries(vec![
            ("科幻", "builtin.science_fiction"),
            ("科幻", "omw.00012345.n"),
        ]);
        assert_eq!(resolve_controlled_tag_code("科幻"), Some("omw.00012345.n"));
        // 反向写入顺序：omw 先、builtin 后，仍保持 omw 优先
        clear_dynamic_aliases();
        load_aliases_from_entries(vec![
            ("科幻", "omw.00012345.n"),
            ("科幻", "builtin.science_fiction"),
        ]);
        assert_eq!(resolve_controlled_tag_code("科幻"), Some("omw.00012345.n"));
        // 静态 builtin 词仍可解析
        assert!(resolve_controlled_tag_code("截图").unwrap().starts_with("builtin."));
        // 未知词返回 None，交由调用方派生 _ext
        assert_eq!(resolve_controlled_tag_code("完全未知的新标签XYZQ"), None);
        clear_dynamic_aliases();
    }
}
