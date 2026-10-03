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
///
/// **本函数是纯生成器，不做任何准入校验**（G1 词形闸装在**调用方**，见
/// [`normalize_tag_to_code`] 的范式 —— spec §6.9.3 原文即写「`derive_ext_tag_code` **调用方**」）。
///
/// 为什么不把闸装在这里：本函数同时服务**可信的策展资产**——如 RAM++ 离线投影表
/// `ram_pan_projection.json` 的 `zh` 字段（4585 条中 103 条是多义项 gloss，形如 `胡同/球道`，
/// 含 `/`）。这些 `zh` 是**词典查找键**而非候选标签词，用 G1 去筛会误删 103 条合法投影。
/// G1 的适用对象是**来自文件名 / OCR / CLIP / RAM 模型输出的候选词**，不是策展资产。
///
/// 新增调用方时，若输入来自不可信来源，**必须先过 G1 闸**再调用本函数。
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

/// 判断 code 是否属于严格受控三分区契约 (builtin.* | omw.* | hownet.*)
/// 开放集扩展标签 _ext.* 以及旧系统 dim.* 不属于受控标签 code
#[inline]
pub fn is_controlled_code(code: &str) -> bool {
    code.starts_with("builtin.") || code.starts_with("omw.") || code.starts_with("hownet.")
}

/// 未知标签兜底代码
pub const UNKNOWN_TAG_CODE: &str = "builtin.unknown";

/// 受控别名表：(别名, 英文规范名)
/// 同一概念的多语言别名必须指向同一 en 规范名。
const BUILTIN_ALIASES: &[(&str, &str)] = &[
    // 文件类型 / 版面等规则高频项
    ("文本", "Text"),
    ("纯文字", "Plain Text"),
    ("文档", "Document"),
    ("文件", "Document"),
    ("图片", "Image"),
    ("图像", "Image"),
    ("画面", "Image"),
    ("照片", "Photo"),
    ("素材", "Material"),
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
    ("涂鸦", "Doodle"),
    ("角色", "Character"),
    ("设计", "Design"),
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
    ("软色情", "Soft Porn"),
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
    ("file", "Document"),
    ("image", "Image"),
    ("picture", "Image"),
    ("pic", "Image"),
    ("img", "Image"),
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
    ("image with no text", "Image Without Text"),
    ("doodle", "Doodle"),
    ("character", "Character"),
    ("design", "Design"),
    ("designated", "Design"),
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
    ("pornography", "Pornography"),
    ("ui screenshot", "UI Screenshot"),
    ("landscape", "Natural Landscape"),
    ("code screenshot", "Code Screenshot"),
    ("chat screenshot", "Chat Screenshot"),
    ("confidential", "Confidential"),
    ("exposure is normal", "Normal Exposure"),
    ("normal exposure", "Normal Exposure"),
    ("good exposure", "Good Exposure"),
    ("slight underexposure", "Slight Underexposure"),
    ("slight overexposure", "Slight Overexposure"),
    ("high quality", "High Quality"),
    ("medium quality", "Medium Quality"),
    ("low quality", "Low Quality"),
    ("all ages", "All Ages"),
    ("text-in-image", "Image With Text"),
    ("text in image", "Image With Text"),
    ("长图", "Long Image"),
    ("正方形图", "Square Image"),
    ("横图", "Horizontal Image"),
    ("竖图", "Vertical Image"),
    ("横屏", "Horizontal Screen"),
    ("Landscape", "Horizontal Screen"),
    ("竖屏", "Vertical Screen"),
    ("Portrait", "Vertical Screen"),
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
    ("macOS截图", "macOS Screenshots"),
    ("macOS Screenshot", "macOS Screenshots"),
    ("iOS截图", "iOS Screenshots"),
    ("iOS Screenshot", "iOS Screenshots"),
    ("Android截图", "Android Screenshot"),
    ("Linux截图", "Linux Screenshot"),
    ("正方形", "Square"),
    ("超宽长条", "Ultra Wide"),
    ("微量文本", "Microtext"),
    ("Minimal Text", "Microtext"),
    ("图文标题", "Picture And Text Title"),
    ("Graphic Title", "Picture And Text Title"),
    ("密集排版", "Dense Typography"),
    ("Dense Layout", "Dense Typography"),
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
    ("暴恐", "Violence Terror"),
    ("生殖器暴露", "Genital Exposure"),
    ("性虐调教", "Sadomasochism"),
    ("乱伦淫秽", "Incest Pornography"),
    ("强奸轮奸", "Rape Assault"),
    ("自慰高潮", "Masturbation"),
    ("调教拘束", "Bondage"),
    ("情色文娱", "Erotic Entertainment"),
    ("暴露走光", "Exposure Wardrobe Malfunction"),
    ("残肢断臂", "Severed Limbs Mutilation"),
    ("尸体残骸", "Corpse Remains"),
    ("酷刑折磨", "Torture"),
    ("重口猎奇", "Hardcore Grotesque"),
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
    ("内容标签", "Content Tags"),
    ("content_tags", "Content Tags"),
    ("图片细分", "Image Subdivision"),
    // 权威事实维度与事实标签别名对齐（统一 builtin.* 严格受控体系）
    ("文件来源", "File Source"),
    ("file_source", "File Source"),
    ("网络下载", "Web Downloads"),
    ("web_downloads", "Web Downloads"),
    ("web_download", "Web Downloads"),
    ("本地创建", "Created Locally"),
    ("created_locally", "Created Locally"),
    ("原创", "Original"),
    ("备份归档", "Backup Archive"),
    ("处理状态", "Processing status"),
    ("processing_status", "Processing status"),
    ("未归档", "Not archived"),
    ("not_archived", "Not archived"),
    ("unarchived", "Not archived"),
    ("已归档", "Archived"),
    ("archived", "Archived"),
    ("草稿", "Draft"),
    ("draft", "Draft"),
    ("安全等级", "Security level"),
    ("security_level", "Security level"),
    ("公开", "Public"),
    ("public", "Public"),
    ("内部", "Internal"),
    ("internal", "Internal"),
    ("语言细分", "Language segmentation"),
    ("language_segmentation", "Language segmentation"),
    ("语言", "Language segmentation"),
    ("中文", "Chinese"),
    ("chinese", "Chinese"),
    ("英文", "English"),
    ("english", "English"),
    ("日语", "Japanese"),
    ("japanese", "Japanese"),
    ("韩语", "Korean"),
    ("korean", "Korean"),
    ("法语", "French"),
    ("french", "French"),
    ("德语", "German"),
    ("german", "German"),
    ("西班牙语", "Spanish"),
    ("spanish", "Spanish"),
    ("俄语", "Russian"),
    ("russian", "Russian"),
    ("文件质量", "Document quality"),
    ("document_quality", "Document quality"),
    ("作者", "Author"),
    ("author", "Author"),
    ("音乐类型", "Music Type"),
    ("音乐流派", "Music Type"),
    ("music_type", "Music Type"),
    ("音频细分", "Audio Segmentation"),
    ("audio_segmentation", "Audio Segmentation"),
    ("摄影照片细分", "Photography Categories"),
    ("photography_categories", "Photography Categories"),
    // 视觉与多模态高频候选词跨语言对齐（避免误被派生为 _ext.* 并确保规范母语本地化）
    //
    // 本段只补**本段专属**的跨语言别名。凡与上文既有条目**同名**者一律不在此重复声明
    // （`设计` / `design` / `designated` / `角色` / `character` 均已在上文登记）——
    // `alias_map()` 用 `HashMap::insert`（**末次胜出**），在此重复会**静默覆盖**上文的规范名，
    // 属死数据 + 隐含行序依赖。裁决口径（方向 B，见
    // docs/issues/issue-builtin-aliases-duplicate-keys.md）：**裸词 → 裸概念**
    // （`design`→`Design`、`character`→`Character`）；复合概念 `Design Draft` /
    // `Human Subject` 由各自**专属**别名承载（`设计稿` / `design draft`、`人物主体` / `人物`）。
    ("艺术", "Painting"),
    ("art", "Painting"),
    ("cartoon", "Comic"),
    ("肖像", "Portrait"),
    ("人物", "Human Subject"),
    ("扁平", "Flat Minimalist"),
    ("flat", "Flat Minimalist"),
    // 敏感内容 / 成人色情 12 大垂直门类与细分子标签跨语言对齐 (PRD 0052)
    // （同上：`调教拘束` / `露骨性行为` / `自慰高潮` 已在上文登记，此处不再重复声明，避免末次胜出覆盖）
    ("家庭乱伦", "Family incest"),
    ("family incest", "Family incest"),
    ("婚外情欲", "Extramarital lust"),
    ("extramarital lust", "Extramarital lust"),
    ("校园师生", "Campus teachers and students"),
    ("campus teachers and students", "Campus teachers and students"),
    ("职场社交", "Workplace social"),
    ("workplace social", "Workplace social"),
    ("多人群交", "Group sex"),
    ("group sex", "Group sex"),
    ("性爱调教", "Sex training"),
    ("sex training", "Sex training"),
    ("窥视暴露", "Peeping exposed"),
    ("peeping exposed", "Peeping exposed"),
    ("职业制服", "Professional uniform"),
    ("professional uniform", "Professional uniform"),
    ("文学题材", "Literary themes"),
    ("literary themes", "Literary themes"),
    ("情欲生理", "Erotic Physiology"),
    ("erotic physiology", "Erotic Physiology"),
    ("非自愿侵犯", "Involuntary assault"),
    ("involuntary assault", "Involuntary assault"),
    ("多元取向", "Multiple orientations"),
    ("multiple orientations", "Multiple orientations"),
    // 重点 L4 细分标签
    ("近亲乱伦", "Incest"),
    ("incest", "Incest"),
    ("母子", "Mother and son"),
    ("mother and son", "Mother and son"),
    ("父女", "Father and daughter"),
    ("father and daughter", "Father and daughter"),
    ("兄妹", "Brother and sister"),
    ("brother and sister", "Brother and sister"),
    ("姐弟", "Siblings"),
    ("伴侣交换", "Partner swap"),
    ("partner swap", "Partner swap"),
    ("换妻", "Wife swapping"),
    ("wife swapping", "Wife swapping"),
    ("偷情出轨", "Cheating"),
    ("cheating", "Cheating"),
    ("人妻美妇", "Beautiful married woman"),
    ("少妇熟女", "Young mature woman"),
    ("3p", "3P"),
    ("群交", "Group sex"),
    ("多人派对", "Multi-person party"),
    ("sm", "SM"),
    ("性奴隶", "Sex slave"),
    ("粗暴性爱", "Rough sex"),
    ("重口变态", "Hardcore Perversion"),
    ("滴蜡皮鞭", "Wax Play & Whip"),
    ("特定恋物", "Specific fetish"),
    ("疯狂暴露", "Extreme Public Exposure"),
    ("公共空间暴露", "Public Exposure"),
    ("户外露出", "Exposed outdoors"),
    ("车震", "Car Sex"),
    ("偷拍窥视", "Covert Photography and Peeping"),
    ("covert photography and peeping", "Covert Photography and Peeping"),
    ("隐私盗摄", "Spy Camera & Peeping"),
    ("privacy camera", "Spy Camera & Peeping"),
    ("spy camera & peeping", "Spy Camera & Peeping"),
    ("洗澡窥视", "Voyeurism"),
    ("走光露底", "Exposed"),
    ("医生护士", "Doctor & Nurse Roleplay"),
    ("空姐", "Airline stewardess"),
    ("女警", "Policewoman"),
    ("商业涉黄", "Commercial pornography"),
    ("援交嫖妓", "Paid Dating and Prostitution"),
    ("paid dating and prostitution", "Paid Dating and Prostitution"),
    ("情色武侠", "Erotic Wuxia"),
    ("古典情色", "Classical Erotica"),
    ("日本情色", "Japanese erotica"),
    ("japanese erotica", "Japanese erotica"),
    ("西洋情色", "Western erotica"),
    ("explicit sexual acts", "Explicit Sexual Acts"),
    ("口交", "Oral sex"),
    ("oral sex", "Oral sex"),
    ("肛交", "Anal sex"),
    ("anal sex", "Anal sex"),
    ("颜射吞精", "Facial & Swallowing"),
    ("强暴强奸", "Rape and Sexual Assault"),
    ("迷奸下药", "Drug-facilitated Sexual Assault"),
    ("违背意愿", "Against one's will"),
    ("轮奸侵犯", "Gang rape assault"),
    ("同性情色", "Homoerotic"),
    ("女同百合", "Lesbian Yuri"),
    ("男同耽美", "Gay Yaoi"),
    ("跨性别", "Transgender"),
    ("transgender", "Transgender"),
    // 维度 123 内容尺度 (Rating Track & Scale Track)
    ("PG-13", "PG-13"),
    ("pg-13", "PG-13"),
    ("pg13", "PG-13"),
    ("R-15", "R-15"),
    ("r-15", "R-15"),
    ("r15", "R-15"),
    ("R-18", "R-18"),
    ("r-18", "R-18"),
    ("r18", "R-18"),
    ("R-18G", "R-18G"),
    ("r-18g", "R-18G"),
    ("r18g", "R-18G"),
    ("soft porn", "Soft Porn"),
    ("soft pornography", "Soft Porn"),
    ("半肉", "Half Meat"),
    ("half meat", "Half Meat"),
    ("纯肉", "Pure Meat"),
    ("pure meat", "Pure Meat"),
    ("特殊XP", "Special XP"),
    ("special xp", "Special XP"),
    ("special_xp", "Special XP"),
    ("重度猎奇", "Extreme Grotesque"),
    ("extreme grotesque", "Extreme Grotesque"),
    ("severe curiosity", "Extreme Grotesque"),
    ("car sex", "Car Sex"),
    ("car shock", "Car Sex"),
    ("rape and sexual assault", "Rape and Sexual Assault"),
    ("rape rape", "Rape and Sexual Assault"),
    ("hardcore perversion", "Hardcore Perversion"),
    ("incest pornography", "Incest Pornography"),
    ("incest obscene", "Incest Pornography"),
    ("severed limbs mutilation", "Severed Limbs Mutilation"),
    ("severed limbs", "Severed Limbs Mutilation"),
    ("hardcore grotesque", "Hardcore Grotesque"),
    ("hardcore bizarre", "Hardcore Grotesque"),
    ("wax play & whip", "Wax Play & Whip"),
    ("dripping wax whip", "Wax Play & Whip"),
    ("extreme public exposure", "Extreme Public Exposure"),
    ("crazy exposure", "Extreme Public Exposure"),
    ("public exposure", "Public Exposure"),
    ("public space exposure", "Public Exposure"),
    ("voyeurism", "Voyeurism"),
    ("bath peeping", "Voyeurism"),
    ("doctor & nurse roleplay", "Doctor & Nurse Roleplay"),
    ("doctor nurse", "Doctor & Nurse Roleplay"),
    ("erotic wuxia", "Erotic Wuxia"),
    ("erotic martial arts", "Erotic Wuxia"),
    ("facial & swallowing", "Facial & Swallowing"),
    ("facial cum swallowing", "Facial & Swallowing"),
    ("drug-facilitated sexual assault", "Drug-facilitated Sexual Assault"),
    ("raped and drugged", "Drug-facilitated Sexual Assault"),
    ("lesbian yuri", "Lesbian Yuri"),
    ("lesbian lily", "Lesbian Yuri"),
    ("gay yaoi", "Gay Yaoi"),
    ("gay beauty", "Gay Yaoi"),
];

use std::sync::RwLock;

static DYNAMIC_ALIASES: OnceLock<RwLock<HashMap<String, &'static str>>> = OnceLock::new();
static DYNAMIC_ALIASES_BY_LANG: OnceLock<RwLock<HashMap<String, HashMap<String, &'static str>>>> = OnceLock::new();
static DYNAMIC_CANONICAL_BY_LANG: OnceLock<RwLock<HashMap<String, HashMap<String, &'static str>>>> = OnceLock::new();

fn dynamic_aliases() -> &'static RwLock<HashMap<String, &'static str>> {
    DYNAMIC_ALIASES.get_or_init(|| RwLock::new(HashMap::new()))
}

fn dynamic_aliases_by_lang() -> &'static RwLock<HashMap<String, HashMap<String, &'static str>>> {
    DYNAMIC_ALIASES_BY_LANG.get_or_init(|| RwLock::new(HashMap::new()))
}

fn dynamic_canonical_by_lang() -> &'static RwLock<HashMap<String, HashMap<String, &'static str>>> {
    DYNAMIC_CANONICAL_BY_LANG.get_or_init(|| RwLock::new(HashMap::new()))
}

/// 规约语言标识（支持 "zh-CN", "zh_CN", "zh" -> "zh", "en-US" -> "en" 等）
pub fn normalize_language_code(lang: &str) -> &'static str {
    let lower = lang.trim().to_lowercase();
    if lower.starts_with("zh") {
        "zh"
    } else if lower.starts_with("ja") {
        "ja"
    } else if lower.starts_with("ko") {
        "ko"
    } else if lower.starts_with("fr") {
        "fr"
    } else if lower.starts_with("de") {
        "de"
    } else if lower.starts_with("es") {
        "es"
    } else if lower.starts_with("ru") {
        "ru"
    } else if lower.starts_with("pt") {
        "pt"
    } else if lower.starts_with("ar") {
        "ar"
    } else {
        "en"
    }
}

/// 极速 Unicode 字符集语言识别 (LID)
/// 对单标签短语执行 100% 确定性零耗时判别 (ADR-0048 / Q1 选项 A)
pub fn detect_tag_language(raw: &str) -> &'static str {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return "en";
    }
    for ch in trimmed.chars() {
        match ch {
            '\u{3040}'..='\u{309F}' | '\u{30A0}'..='\u{30FF}' => return "ja",
            '\u{AC00}'..='\u{D7AF}' | '\u{1100}'..='\u{11FF}' => return "ko",
            '\u{0400}'..='\u{04FF}' => return "ru",
            '\u{0600}'..='\u{06FF}' => return "ar",
            '\u{4E00}'..='\u{9FFF}' => return "zh",
            _ => {}
        }
    }
    "en"
}

/// 按语言分表载入动态别名与权威展示名 (lemma, tag_code, is_canonical)
///
/// 冲突规则：同一 lemma 同时命中 `omw.*` 与 `builtin.*` 时 **omw.* 优先**。
/// 全局表（`dynamic_aliases`，供 `resolve_controlled_tag_code` 等旧接口读取）
/// 与语言分表采用同一 omw.* 优先冲突规则，避免多语言分表热载顺序不同
/// 导致同名 lemma 全局解析结果漂移（ADR-0048 评审 HIGH-2）。
/// 指针复用：同一 (lemma, code) 重复载入时复用已泄漏的 `&'static str`，
/// 不再追加 `Box::leak`（评审 HIGH-3）。
pub fn load_aliases_for_lang<I, S1, S2>(lang: &str, entries: I)
where
    I: IntoIterator<Item = (S1, S2, bool)>,
    S1: AsRef<str>,
    S2: AsRef<str>,
{
    // 受控前缀优先级：omw.* (3) > hownet.* (2) > builtin.* (1) > 其它/_ext.* (0)
    fn code_priority(code: &str) -> u8 {
        if code.starts_with("omw.") {
            3
        } else if code.starts_with("hownet.") {
            2
        } else if code.starts_with("builtin.") {
            1
        } else {
            0
        }
    }

    // 冲突仲裁：返回 true 表示 incoming 应覆盖 existing
    // 由于 SQL 查询已按 is_canonical DESC, count DESC 排序，
    // 最权威、词频最高的概念最先被读取入表，同优先级下保留最先入表的高频词义，
    // 仅当 incoming 体系优先级更高 (omw > hownet > builtin) 时才允许覆盖。
    fn prefer_incoming(existing: &str, incoming: &str) -> bool {
        let p_existing = code_priority(existing);
        let p_incoming = code_priority(incoming);
        p_incoming > p_existing
    }

    let norm_lang = normalize_language_code(lang).to_string();
    let mut by_lang_guard = dynamic_aliases_by_lang().write().unwrap();
    let mut canonical_guard = dynamic_canonical_by_lang().write().unwrap();
    let mut global_guard = dynamic_aliases().write().unwrap();

    let lang_map = by_lang_guard.entry(norm_lang.clone()).or_insert_with(HashMap::new);
    let can_map = canonical_guard.entry(norm_lang).or_insert_with(HashMap::new);

    for (lemma, code, is_canonical) in entries {
        let norm_lemma = normalize_lemma(lemma.as_ref());
        if norm_lemma.is_empty() {
            continue;
        }
        let code_ref = code.as_ref();
        // 门禁：受控别名表严禁载入 _ext.* 非受控扩展代码
        if !is_controlled_code(code_ref) {
            continue;
        }
        let should_insert = match lang_map.get(&norm_lemma) {
            Some(existing) => prefer_incoming(existing, code_ref),
            None => true,
        };

        if should_insert {
            // 指针复用：语言分表已存在相同 code 时直接复用，避免重复 Box::leak
            let leaked_code: &'static str = match lang_map.get(&norm_lemma) {
                Some(&existing) if existing == code_ref => existing,
                _ => Box::leak(code_ref.to_string().into_boxed_str()),
            };
            lang_map.insert(norm_lemma.clone(), leaked_code);

            // 全局表独立仲裁（同名 lemma 跨语言分表可能映射不同 code，
            // 维持 omw.* 优先，其余后写覆盖，与分表规则一致）
            match global_guard.get(&norm_lemma) {
                Some(&existing) if existing == leaked_code => {}
                Some(&existing) if !prefer_incoming(existing, leaked_code) => {}
                _ => {
                    global_guard.insert(norm_lemma.clone(), leaked_code);
                }
            }
        }

        // 规范展示名仲裁 (以 code_ref 为主键，独立于 lemma 映射是否插入):
        // 1. 若 is_canonical 为 true，优先更新覆盖；
        // 2. 若当前分表中尚未记录该 code 的展示名，以首个出现的词形保底记录，
        //    防止因同名 lemma 未更新导致特定 code 的母语展示名被漏登 (Q2 选项 A)。
        if is_canonical || !can_map.contains_key(code_ref) {
            let leaked_lemma: &'static str = match can_map.get(code_ref) {
                Some(&existing) if is_canonical && existing == lemma.as_ref() => existing,
                _ => Box::leak(lemma.as_ref().to_string().into_boxed_str()),
            };
            can_map.insert(code_ref.to_string(), leaked_lemma);
        }
    }
}

/// 动态批量载入受控标签别名（由 SQLite 连接时读取 tag_aliases 表注入，或从 json 载入）
/// 传入 (lemma, tag_code)
///
/// 冲突规则：同一 lemma 同时命中 `omw.*` 与 `builtin.*` 时 **omw.* 优先**。
pub fn load_aliases_from_entries<I, S1, S2>(entries: I)
where
    I: IntoIterator<Item = (S1, S2)>,
    S1: AsRef<str>,
    S2: AsRef<str>,
{
    let mut batch_by_lang: HashMap<&'static str, Vec<(String, String, bool)>> = HashMap::new();
    for (lemma, code) in entries {
        let l_str = lemma.as_ref();
        let lang = detect_tag_language(l_str);
        batch_by_lang.entry(lang).or_default().push((l_str.to_string(), code.as_ref().to_string(), true));
    }
    for (lang, list) in batch_by_lang {
        load_aliases_for_lang(lang, list);
    }
}

/// 受控标签 code 反查（运行时单语言/全局归一入口）
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
    // 1. 动态轨（含 omw.* / hownet.* / builtin.*）
    if let Ok(dyn_map) = dynamic_aliases().read() {
        if let Some(&code) = dyn_map.get(&key) {
            if is_controlled_code(code) {
                return Some(code);
            }
        }
        for cand in english_lemma_candidates(&key) {
            if let Some(&code) = dyn_map.get(&cand) {
                if is_controlled_code(code) {
                    return Some(code);
                }
            }
        }
    }
    // 2. 静态 builtin 字典
    lookup_static_with_en_lemmas(&key)
}

/// 英文形态学词形还原候选词（单复数 s/es/ies 与分词 ed/ing 还原）
pub fn english_lemma_candidates(key: &str) -> Vec<String> {
    let mut candidates = Vec::new();
    let len = key.len();

    // 1. 复数还原
    if key.ends_with("ies") && len > 4 {
        candidates.push(format!("{}y", &key[..len - 3]));
    }
    if key.ends_with("es") && len > 3 {
        // boxes -> box, watches -> watch
        candidates.push(key[..len - 2].to_string());
        // services -> service, tables -> table (es结尾但本体带e)
        candidates.push(key[..len - 1].to_string());
    }
    if key.ends_with('s') && !key.ends_with("ss") && len > 3 {
        candidates.push(key[..len - 1].to_string());
    }

    // 2. 动词分词还原 (ed, ing)
    if key.ends_with("ied") && len > 4 {
        candidates.push(format!("{}y", &key[..len - 3]));
    }
    if key.ends_with("ed") && len > 4 {
        // connected -> connect
        candidates.push(key[..len - 2].to_string());
        // created -> create
        candidates.push(key[..len - 1].to_string());
    }
    if key.ends_with("ing") && len > 5 {
        // connecting -> connect
        candidates.push(key[..len - 3].to_string());
        // creating -> create, organizing -> organize
        candidates.push(format!("{}e", &key[..len - 3]));
    }

    candidates
}

fn lookup_map_with_en_lemmas<'a>(
    map: &'a HashMap<String, &'static str>,
    key: &str,
) -> Option<&'static str> {
    if let Some(&code) = map.get(key) {
        if is_controlled_code(code) {
            return Some(code);
        }
    }
    for cand in english_lemma_candidates(key) {
        if let Some(&code) = map.get(&cand) {
            if is_controlled_code(code) {
                return Some(code);
            }
        }
    }
    None
}

fn lookup_static_with_en_lemmas(key: &str) -> Option<&'static str> {
    if let Some(en) = alias_map().get(key) {
        if let Some(code) = en_to_code().get(en.as_str()) {
            return Some(code.as_str());
        }
    }
    if let Some(code) = en_to_code().get(key) {
        return Some(code.as_str());
    }
    for cand in english_lemma_candidates(key) {
        if let Some(en) = alias_map().get(&cand) {
            if let Some(code) = en_to_code().get(en.as_str()) {
                return Some(code.as_str());
            }
        }
        if let Some(code) = en_to_code().get(&cand) {
            return Some(code.as_str());
        }
    }
    None
}

/// 识别标签两阶段反查入口 (ADR-0048)
///
/// 1. 当前语言分表直查 (tag_aliases_{current_lang})
/// 2. 未命中时执行 Unicode LID 语言识别
/// 3. 若识别语言与当前语言不同，反查对应异语分表 (tag_aliases_{detected_lang})
/// 4. 异语命中后，反向解析当前语言的权威规范名进行就地本地化 (Q2 选项 A)
///
/// 返回: Option<(tag_code, Option<canonical_name_in_current_lang>)>
pub fn resolve_controlled_tag_two_stage(
    tag: &str,
    current_lang: Option<&str>,
) -> Option<(&'static str, Option<String>)> {
    let key = normalize_lemma(tag);
    if key.is_empty() {
        return None;
    }
    let curr_lang = normalize_language_code(current_lang.unwrap_or("zh"));
    let detected_lang = detect_tag_language(tag);

    // 辅助闭包：反查当前系统语言的权威规范展示名 (Q2 选项 A 就地本地化)
    let resolve_canonical = |code: &str| -> Option<String> {
        if let Ok(canon_guard) = dynamic_canonical_by_lang().read() {
            if let Some(cmap) = canon_guard.get(curr_lang) {
                if let Some(&name) = cmap.get(code) {
                    return Some(name.to_string());
                }
            }
        }
        let fallback_name = tag_display(code, curr_lang);
        if fallback_name != code {
            Some(fallback_name)
        } else {
            None
        }
    };

    // ─── 第一阶段：当前语言分表直查 ───
    if detected_lang == curr_lang {
        if let Ok(by_lang) = dynamic_aliases_by_lang().read() {
            if let Some(map) = by_lang.get(curr_lang) {
                if curr_lang == "en" {
                    if let Some(code) = lookup_map_with_en_lemmas(map, &key) {
                        return Some((code, None));
                    }
                } else if let Some(&code) = map.get(&key) {
                    if is_controlled_code(code) {
                        return Some((code, None));
                    }
                }
            }
        }
        // 查当前语言静态底座
        if curr_lang == "zh" {
            if let Some(en) = alias_map().get(&key) {
                if let Some(code) = en_to_code().get(en.as_str()) {
                    return Some((code.as_str(), None));
                }
            }
        } else if curr_lang == "en" {
            if let Some(code) = lookup_static_with_en_lemmas(&key) {
                return Some((code, None));
            }
        }
        // 相同语言未查得，直接判定为未登录词
        return None;
    }

    // ─── 异语分表反查 ───
    let mut matched_code: Option<&'static str> = None;
    if let Ok(by_lang) = dynamic_aliases_by_lang().read() {
        if let Some(map) = by_lang.get(detected_lang) {
            if detected_lang == "en" {
                matched_code = lookup_map_with_en_lemmas(map, &key);
            } else if let Some(&code) = map.get(&key) {
                if is_controlled_code(code) {
                    matched_code = Some(code);
                }
            }
        }
        if matched_code.is_none() && detected_lang != "en" {
            if let Some(map) = by_lang.get("en") {
                matched_code = lookup_map_with_en_lemmas(map, &key);
            }
        }
    }

    // 异语静态底座兜底
    if matched_code.is_none() {
        matched_code = lookup_static_with_en_lemmas(&key);
    }

    if let Some(code) = matched_code {
        return Some((code, resolve_canonical(code)));
    }

    None
}

/// 清空动态别名字典（用于热切换或断开连接时）
pub fn clear_dynamic_aliases() {
    if let Ok(mut map) = dynamic_aliases().write() {
        map.clear();
    }
    if let Ok(mut map) = dynamic_aliases_by_lang().write() {
        map.clear();
    }
    if let Ok(mut map) = dynamic_canonical_by_lang().write() {
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

    // 动态规范词典兜底：静态 BUILTIN_ALIASES 只登记 builtin.* 的规范名，
    // omw.*/hownet.* 的母语规范展示名由 semantic.pack 热载入轨写入 dynamic_canonical_by_lang，
    // 若不查这张表，这两层受控概念的展示名会被降级成裸 code（违背 omw ≻ hownet ≻ builtin 分层）。
    // 命中值等于 code 时视为无效（脏别名表可能把 code 冻结成词形），继续走 slug 兜底。
    if let Ok(canon_guard) = dynamic_canonical_by_lang().read() {
        if let Some(cmap) = canon_guard.get(normalize_language_code(lang)) {
            if let Some(&name) = cmap.get(clean_code) {
                if !name.is_empty() && name != clean_code {
                    return name.to_string();
                }
            }
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
            if is_controlled_code(code) {
                return Some(code);
            }
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
///
/// **G1 词形闸（拒绝先于铸造，spec §6.9.3 / ADR-0052）装在本函数**——它是「候选词 → code」
/// 的主入口（`omni-server` 的 `detected_visual_tags` 归一即走此路）。不合格词返回空串哨兵
/// [`crate::tag_admissibility::REJECTED_CODE`]，调用方需按
/// [`crate::tag_admissibility::is_rejected_code`] 判定并丢弃。
///
/// 注意闸门位置：受控别名命中（`builtin.*` / `omw.*`）与「已是合法 code 形态」两个分支
/// **不**过闸——受控词表本身即白名单，code 形态输入则是幂等短路。
///
/// `dim.*` **有意不进**短路列表（2026-10-02 裁决：dim.* 完全迁移到 builtin.*，见
/// `tag-admissibility-filter-spec.md` §6.9.6 待裁定项 1）——迁移完成后流转路径上不应再出现
/// `dim.*` 输入；迁移完成前的残余 `dim.*` 会落到 G1 闸并派生 `_ext` 形态，属预期行为。
pub fn normalize_tag_to_code(tag: &str) -> String {
    if let Some(code) = builtin_tag_code(tag) {
        return code.to_string();
    }
    // 已是合法 code 形态则原样返回
    let t = tag.trim();
    if t.starts_with("builtin.") || t.starts_with("_ext.") || t.starts_with("omw.") || t.starts_with("hownet.") {
        return t.to_string();
    }
    // G1 词形闸：不合格词不派生 `_ext`，返回空串哨兵
    if crate::tag_admissibility::is_g1_rejected(t) {
        return crate::tag_admissibility::REJECTED_CODE.to_string();
    }
    derive_ext_tag_code(t)
}

/// 将标签列表归一为 code 集合（P0 幂等契约：zh/en 别名输入应产出相同集合）
///
/// **G1 拒绝传播**：被词形闸拒绝的词会归一为空串哨兵，此处统一滤除，
/// 使拒绝沿 `detected_visual_tags` → morphology/clip/ram 回写过滤 → 标签链汇聚全链路自动传播
/// （这些下游一律用 `contains(&code)` 判定，空串不在集合内即被剔除）。
pub fn normalize_tag_set_to_codes(tags: &[String]) -> Vec<String> {
    let mut codes: Vec<String> = tags
        .iter()
        .map(|t| normalize_tag_to_code(t))
        .filter(|c| !crate::tag_admissibility::is_rejected_code(c))
        .collect();
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

    /// **别名表键唯一门禁**。
    ///
    /// `BUILTIN_ALIASES` 是 `alias_map()` 的事实源，后者用 `HashMap::insert`（**末次胜出**）。
    /// 同名别名一旦指向**不同的** en 规范名，靠后条目会**静默覆盖**靠前条目 —— 死数据 +
    /// 隐含行序依赖（改表内行序即静默改语义）。详见
    /// `docs/issues/issue-builtin-aliases-duplicate-keys.md`（已按方向 B 裁决）。
    #[test]
    fn builtin_aliases_have_unique_keys() {
        let mut seen: HashMap<&str, &str> = HashMap::new();
        let mut dups: Vec<String> = Vec::new();
        for (alias, en) in BUILTIN_ALIASES {
            if let Some(prev) = seen.insert(alias, en) {
                dups.push(format!("{alias}: {prev} / {en}"));
            }
        }
        assert!(
            dups.is_empty(),
            "BUILTIN_ALIASES 存在重复别名（末次胜出会静默覆盖）: {dups:?}"
        );
    }

    /// **方向 B 回归**：8 个曾重复的别名一律「**裸词 → 裸概念**」，
    /// 复合概念改由各自**专属**别名承载（未被本裁决波及）。
    #[test]
    fn bare_tokens_resolve_to_bare_concepts() {
        for (alias, expected_code) in [
            ("设计", "builtin.design"),
            ("design", "builtin.design"),
            ("designated", "builtin.design"),
            ("角色", "builtin.character"),
            ("character", "builtin.character"),
            ("露骨性行为", "builtin.explicit_sexual_act"),
            ("自慰高潮", "builtin.masturbation"),
            ("调教拘束", "builtin.bondage"),
        ] {
            assert_eq!(
                resolve_controlled_tag_two_stage(alias, Some("zh")).map(|(c, _)| c),
                Some(expected_code),
                "别名 {alias:?} 的解析结果与方向 B 裁决不符"
            );
        }

        // 复合概念仍可达：专属别名未被方向 B 波及。
        for (alias, expected_code) in [
            ("设计稿", "builtin.design_draft"),
            ("design draft", "builtin.design_draft"),
            ("人物主体", "builtin.human_subject"),
            ("人物", "builtin.human_subject"),
            ("explicit sexual acts", "builtin.explicit_sexual_acts"),
        ] {
            assert_eq!(
                resolve_controlled_tag_two_stage(alias, Some("zh")).map(|(c, _)| c),
                Some(expected_code),
                "复合概念 {alias:?} 应仍可达"
            );
        }
    }

    /// **联合键空间歧义门禁**（`alias_map()` 的 `normalize_lemma` 归一后）。
    ///
    /// `alias_map()` 把 **别名** 与 **en 规范名** 插入**同一张表**：
    ///
    /// ```text
    /// for (alias, en) in BUILTIN_ALIASES {
    ///     m.insert(normalize_lemma(alias), en);
    ///     m.insert(normalize_lemma(en),    en);   // 同一张表
    /// }
    /// ```
    ///
    /// 当 `normalize_lemma(某别名) == normalize_lemma(另一概念的 en 规范名)` 时，两个概念
    /// 会争抢同一个键 —— 而 `HashMap::insert` 是**末次胜出**，「谁赢」取决于**表内行序**，
    /// 属**静默语义**（改行序即改行为）。`builtin_aliases_have_unique_keys` 只覆盖**字面别名**
    /// 重复，覆盖不到这种「别名 vs en 规范名」的**跨键空间**碰撞。
    ///
    /// 现存 2 处已知歧义（**待本体裁决**，见
    /// `docs/issues/issue-alias-map-joint-keyspace-conflicts.md`）：
    /// `landscape`（自然景观 vs 横屏）与 `portrait`（人像写真 vs 竖屏）。
    /// 本用例把它们**冻结**：新增歧义立即失败；改动这 2 处即失败（迫使显式裁决）。
    #[test]
    fn alias_map_joint_keyspace_conflicts_are_frozen() {
        // 复刻 alias_map() 的归一与插入，收集每个键对应的**全部**候选值。
        let mut vals: HashMap<String, Vec<&'static str>> = HashMap::new();
        for (alias, en) in BUILTIN_ALIASES {
            for k in [normalize_lemma(alias), normalize_lemma(en)] {
                let bucket = vals.entry(k).or_default();
                if !bucket.contains(en) {
                    bucket.push(en);
                }
            }
        }
        let mut conflicts: Vec<String> = vals
            .iter()
            .filter(|(_, v)| v.len() > 1)
            .map(|(k, _)| k.clone())
            .collect();
        conflicts.sort();

        assert_eq!(
            conflicts,
            vec!["landscape".to_string(), "portrait".to_string()],
            "alias_map() 联合键空间出现**新的**歧义键（末次胜出 = 隐含行序依赖）。\
             若确为有意新增，请连同 docs/issues/issue-alias-map-joint-keyspace-conflicts.md 一并裁决。"
        );

        // 冻结当前「末次胜出」的实际落点：行序一旦变动即失败，迫使显式决策。
        assert_eq!(
            alias_map().get("landscape").map(String::as_str),
            Some("Horizontal Screen")
        );
        assert_eq!(
            alias_map().get("portrait").map(String::as_str),
            Some("Portrait")
        );
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

    /// G1 词形闸（spec §6.9.3 / 验收 V10）：四类坏名**不得**产出 `_ext.*` 概念。
    ///
    /// 闸门位置 = `normalize_tag_to_code`（「候选词 → code」主入口），而非 `derive_ext_tag_code`
    /// （纯生成器，见其文档注释：策展资产如 RAM 投影表的 `zh` 键含 `/`，不可用 G1 去筛）。
    #[test]
    fn g1_rejects_mojibake_before_minting_ext_code() {
        for bad in [
            "\u{FFFD}\u{FFFD}\u{FFFD}乱码", // U+FFFD 替换字符
            "袁浩, 李晓红编著\u{0}",        // NUL 控制字符
            "Exclude\u{f023}T,",            // 私用区 PUA
            "\u{80}abc",                    // C1 控制字符（U+0080 PAD，非 White_Space）
            "ӉӉӉ汉",                        // 单串多脚本混杂（R-G1-11）
        ] {
            let code = normalize_tag_to_code(bad);
            assert!(
                crate::tag_admissibility::is_rejected_code(&code),
                "坏名不得铸造 code: {bad:?} → {code}"
            );
        }
    }

    /// 幂等短路：已是合法 code 形态的输入必须**原样返回**，不得再走派生。
    ///
    /// 必要性：G1 会拒绝含 `.` 的串（`.` 非白名单文种亦非中性字符），无短路会把
    /// `_ext.foo.12345678` 这类合法 code 先判「不合格」而返回空哨兵——把合法标签误杀。
    /// （S1 #705 第 1 轮审查：该短路此前零测试覆盖。）
    #[test]
    fn idempotent_short_circuit_returns_code_form_as_is() {
        for code in [
            "_ext.foo.12345678",
            "builtin.invoice",
            "omw.01846331.n",
            "hownet.002",
        ] {
            assert_eq!(
                normalize_tag_to_code(code),
                code,
                "code 形态输入应原样返回: {code}"
            );
        }
    }

    /// 闸门不得误伤策展资产：RAM++ 投影表的 `zh` 键含 `/`（多义项 gloss），
    /// 经 `derive_ext_tag_code` 仍须产出合法 `_ext.*`（纯生成器不做准入校验）。
    #[test]
    fn derive_ext_tag_code_stays_pure_for_curated_gloss_keys() {
        for gloss in ["胡同/球道", "检查/支票", "一本/一册"] {
            let code = derive_ext_tag_code(gloss);
            assert!(
                code.starts_with("_ext.") && !code.is_empty(),
                "策展 gloss 键必须仍能派生 code: {gloss} → {code}"
            );
        }
    }

    /// 正例零误杀：合法词仍正常铸造，且拒绝不污染 code 集合。
    #[test]
    fn g1_allows_positive_and_filters_rejected_from_set() {
        assert!(normalize_tag_to_code("手机照片").starts_with("builtin.") || normalize_tag_to_code("手机照片").starts_with("_ext."));
        assert!(normalize_tag_to_code("Документы").starts_with("_ext."));
        let mixed = vec![
            "手机照片".to_string(),
            "\u{FFFD}\u{FFFD}乱码".to_string(),
            "Документы".to_string(),
        ];
        let codes = normalize_tag_set_to_codes(&mixed);
        assert_eq!(codes.len(), 2, "空串哨兵必须被滤除: {codes:?}");
        assert!(codes.iter().all(|c| !c.is_empty()));
    }

    // 注意：dynamic_aliases_* 全局字典为进程级共享状态（static RwLock），
    // 多个测试并行 clear/insert 会互相踩踏（ADR-0048 评审 BLOCKER-1），
    // 因此所有依赖动态别名字典的用例必须合并进这一个测试，保证顺序执行。
    #[test]
    fn dynamic_aliases_lifecycle_all_in_one() {
        // ─── 场景 1：注入与反查 ───
        clear_dynamic_aliases();
        load_aliases_from_entries(vec![
            ("特种发票", "builtin.invoice"),
            ("Special Tax Invoice", "builtin.invoice"),
        ]);
        assert_eq!(builtin_tag_code("特种发票"), Some("builtin.invoice"));
        assert_eq!(builtin_tag_code("Special Tax Invoice"), Some("builtin.invoice"));
        assert_eq!(normalize_tag_to_code("特种发票"), "builtin.invoice");

        // ─── 场景 2：同 lemma omw.* 优先于 builtin.*（双向写入顺序）───
        clear_dynamic_aliases();
        // builtin 先写入，omw 后写入 → 反查必须得到 omw.*
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

        // ─── 场景 3：两阶段跨语言反查与就地本地化 ───
        clear_dynamic_aliases();

        // 3.1 测试 LID 判定
        assert_eq!(detect_tag_language("Connect"), "en");
        assert_eq!(detect_tag_language("Art"), "en");
        assert_eq!(detect_tag_language("艺术"), "zh");
        assert_eq!(detect_tag_language("アニメ"), "ja");
        assert_eq!(detect_tag_language("그림"), "ko");
        assert_eq!(detect_tag_language("рисунок"), "ru");

        // 3.2 模拟从多语言分表热载入别名
        load_aliases_for_lang("zh", vec![
            ("艺术", "omw.01700688-n", true),
            ("连接", "builtin.connect", true),
            ("发票", "builtin.invoice", true),
        ]);
        load_aliases_for_lang("en", vec![
            ("art", "omw.01700688-n", true),
            ("connect", "builtin.connect", true),
            ("invoice", "builtin.invoice", true),
        ]);

        // 3. 在中文环境下识别英文 "Art" -> 跨语言命中受控 code，并就地本地化为 "艺术"
        let outcome_art = resolve_controlled_tag_two_stage("Art", Some("zh-CN"));
        assert!(outcome_art.is_some());
        let (code, canon_name) = outcome_art.unwrap();
        assert_eq!(code, "omw.01700688-n");
        assert_eq!(canon_name, Some("艺术".to_string()));

        // 4. 在中文环境下识别英文 "Connect" -> 就地本地化为 "连接"
        let outcome_conn = resolve_controlled_tag_two_stage("Connect", Some("zh"));
        assert!(outcome_conn.is_some());
        let (code_conn, canon_conn) = outcome_conn.unwrap();
        assert_eq!(code_conn, "builtin.connect");
        assert_eq!(canon_conn, Some("连接".to_string()));

        // 5. 在中文环境下输入未登录的中文生词 -> 判定为 zh，与当前语言相同，不重复查找，返回 None
        let outcome_zh_unknown = resolve_controlled_tag_two_stage("未知冷门新造词汇", Some("zh"));
        assert_eq!(outcome_zh_unknown, None);

        // 6. 在中文环境下输入未登录的英文生词 -> 两阶段均未查得，返回 None
        let outcome_en_unknown = resolve_controlled_tag_two_stage("NonExistentBrandNameX", Some("zh"));
        assert_eq!(outcome_en_unknown, None);

        // ─── 场景 4：核心视觉画质与尺度受控标签 100% 确定性解析与 _ext 防污染 ───
        assert_eq!(resolve_controlled_tag_code("高质量"), Some("builtin.high_quality"));
        assert_eq!(resolve_controlled_tag_code("曝光正常"), Some("builtin.normal_exposure"));
        assert_eq!(resolve_controlled_tag_code("全年龄"), Some("builtin.all_ages"));
        assert_eq!(resolve_controlled_tag_code("有字图"), Some("builtin.image_with_text"));
        assert_eq!(resolve_controlled_tag_code("无字图"), Some("builtin.image_without_text"));
        assert_eq!(normalize_tag_to_code("高质量"), "builtin.high_quality");
        assert_eq!(normalize_tag_to_code("曝光正常"), "builtin.normal_exposure");
        assert_eq!(normalize_tag_to_code("全年龄"), "builtin.all_ages");
        assert_eq!(normalize_tag_to_code("有字图"), "builtin.image_with_text");

        // 验证非法/污染的 _ext.* 绝对无法覆盖已有的受控代码
        load_aliases_from_entries(vec![
            ("高质量", "_ext.gaozhiliang.4973d142"),
            ("曝光正常", "_ext.guang_zhen_chang.ee8a87d9"),
        ]);
        assert_eq!(resolve_controlled_tag_code("高质量"), Some("builtin.high_quality"));
        assert_eq!(resolve_controlled_tag_code("曝光正常"), Some("builtin.normal_exposure"));

        // 验证维度 123 内容尺度全量标签解析
        assert_eq!(resolve_controlled_tag_code("PG-13"), Some("builtin.pg_13"));
        assert_eq!(resolve_controlled_tag_code("pg13"), Some("builtin.pg_13"));
        assert_eq!(resolve_controlled_tag_code("R-15"), Some("builtin.r_15"));
        assert_eq!(resolve_controlled_tag_code("R-18"), Some("builtin.r_18"));
        assert_eq!(resolve_controlled_tag_code("R-18G"), Some("builtin.r_18g"));
        assert_eq!(resolve_controlled_tag_code("软色情"), Some("builtin.soft_porn"));
        assert_eq!(resolve_controlled_tag_code("半肉"), Some("builtin.half_meat"));
        assert_eq!(resolve_controlled_tag_code("纯肉"), Some("builtin.pure_meat"));
        assert_eq!(resolve_controlled_tag_code("特殊XP"), Some("builtin.special_xp"));
        assert_eq!(resolve_controlled_tag_code("重度猎奇"), Some("builtin.extreme_grotesque"));
        assert_eq!(resolve_controlled_tag_code("车震"), Some("builtin.car_sex"));
        assert_eq!(resolve_controlled_tag_code("强暴强奸"), Some("builtin.rape_and_sexual_assault"));
        assert_eq!(resolve_controlled_tag_code("重口变态"), Some("builtin.hardcore_perversion"));
        assert_eq!(resolve_controlled_tag_code("女同百合"), Some("builtin.lesbian_yuri"));
        assert_eq!(resolve_controlled_tag_code("男同耽美"), Some("builtin.gay_yaoi"));
        assert_eq!(resolve_controlled_tag_code("隐私盗摄"), Some("builtin.spy_camera_peeping"));

        // ─── 场景 5：高频英文视觉/文档候选词在中文环境下的受控两阶段反查与就地本地化 ───
        let outcome_portrait = resolve_controlled_tag_two_stage("portrait", Some("zh"));
        assert_eq!(outcome_portrait, Some(("builtin.portrait", Some("人像写真".to_string()))));

        let outcome_draft = resolve_controlled_tag_two_stage("draft", Some("zh"));
        assert_eq!(outcome_draft, Some(("builtin.draft", Some("草稿".to_string()))));

        // 方向 B（见 docs/issues/issue-builtin-aliases-duplicate-keys.md）：裸词 `design` 归**裸概念**
        // `Design`（`builtin.design` / 设计）；复合概念 `Design Draft` 由专属别名 `设计稿` / `design draft` 承载。
        let outcome_design = resolve_controlled_tag_two_stage("design", Some("zh"));
        assert_eq!(outcome_design, Some(("builtin.design", Some("设计".to_string()))));

        let outcome_cartoon = resolve_controlled_tag_two_stage("cartoon", Some("zh"));
        assert_eq!(outcome_cartoon, Some(("builtin.comic", Some("漫画".to_string()))));

        // 同上：裸词 `character` 归 `Character`（`builtin.character` / 角色）；
        // 复合概念 `Human Subject` 由专属别名 `人物主体` / `人物` 承载。
        let outcome_character = resolve_controlled_tag_two_stage("character", Some("zh"));
        assert_eq!(outcome_character, Some(("builtin.character", Some("角色".to_string()))));

        // ─── 场景 6：英文形态学词形还原（复数/分词）两阶段反查与就地本地化 ───
        load_aliases_for_lang("zh", vec![
            ("元素", "omw.05868954.n", true),
            ("表格", "omw.08266235.n", true),
            ("创建", "omw.01617192.v", true),
        ]);
        load_aliases_for_lang("en", vec![
            ("element", "omw.05868954.n", true),
            ("table", "omw.08266235.n", true),
            ("create", "omw.01617192.v", true),
        ]);

        // 复数 Elements -> 原型 element -> omw.05868954.n -> 元素
        let outcome_elements = resolve_controlled_tag_two_stage("Elements", Some("zh"));
        assert_eq!(outcome_elements, Some(("omw.05868954.n", Some("元素".to_string()))));

        // 复数 Tables -> 原型 table -> omw.08266235.n -> 表格
        let outcome_tables = resolve_controlled_tag_two_stage("Tables", Some("zh"));
        assert_eq!(outcome_tables, Some(("omw.08266235.n", Some("表格".to_string()))));

        // 分词 Creating -> 原型 create -> omw.01617192.v -> 创建
        let outcome_creating = resolve_controlled_tag_two_stage("Creating", Some("zh"));
        assert_eq!(outcome_creating, Some(("omw.01617192.v", Some("创建".to_string()))));

        // 过去分词 Connected -> 原型 connect -> builtin.connect -> 连接
        let outcome_connected = resolve_controlled_tag_two_stage("Connected", Some("zh"));
        assert_eq!(outcome_connected, Some(("builtin.connect", Some("连接".to_string()))));

        clear_dynamic_aliases();
    }

    /// 回归守卫：tag_display 必须能解析 omw.*/hownet.* 的母语规范名。
    /// 静态 BUILTIN_ALIASES 只登记 builtin.*，历史上 tag_display 从不查 semantic.pack
    /// 热载入轨写入的 dynamic_canonical_by_lang，导致这两层受控概念的展示名被降级成裸 code。
    #[test]
    fn tag_display_resolves_dynamic_canonical_for_omw_and_hownet() {
        load_aliases_for_lang(
            "zh-CN",
            vec![("测试心智", "omw.05617606.n", true), ("测试义原", "hownet.9999999", true)],
        );
        // locale 与归一短码都必须命中同一规范名
        assert_eq!(tag_display("omw.05617606.n", "zh-CN"), "测试心智");
        assert_eq!(tag_display("omw.05617606.n", "zh"), "测试心智");
        assert_eq!(tag_display("hownet.9999999", "zh-CN"), "测试义原");

        // 脏别名（词形被冻结成 code）不得被当作规范展示名采纳：
        // tag_display 仍回吐 code，弃用责任在调用方词形闸（fusion.rs 三处 canon != code 过滤）
        load_aliases_for_lang("zh-CN", vec![("omw.00202784.v", "omw.00202784.v", true)]);
        assert_eq!(tag_display("omw.00202784.v", "zh-CN"), "omw.00202784.v");

        // 完全未登录的 code 保持旧行为（原样返回）
        assert_eq!(tag_display("omw.99999999.n", "zh-CN"), "omw.99999999.n");

        clear_dynamic_aliases();
    }
}
