use serde::{Deserialize, Serialize};

/// 受控标签跨语言身份（别名 → code）
pub mod tag_identity;

/// AOT 受控概念强类型枚举（PRD-0050）
pub mod concepts;

/// (WP5) 图片标签链路阈值集中定义（CLIP/RAM/互斥组 margin/出口 conf），单点可调
pub mod tag_thresholds;

/// 标签准入闸门 · G1 词形层（取码前，零本体依赖；单一数据源 `taxonomy/tag-admissibility.json`）
pub mod tag_admissibility;

/// (DEC-03) 维度执行策略与多选/互斥裁决保底定义
pub mod dimension_policies_generated;
pub use dimension_policies_generated::*;

/// ONNX Runtime 统一动态底座、EP 两阶段路由与分级看门狗 (Ticket 2)
pub mod ort_runtime;
pub use ort_runtime::*;
pub use ort;

/// 编译/运行期受控标签宏：开发时书写中文或英文直观名称，自动解析为系统标准 tag_code
///
/// 示例：`tag_code!("截图")` → `"builtin.screenshot"`
#[macro_export]
macro_rules! tag_code {
    ($name:expr) => {{
        // 受控反查：动态别名（omw.* 优先）→ 静态 builtin
        $crate::tag_identity::resolve_controlled_tag_code($name)
            .or_else(|| $crate::tag_identity::builtin_tag_code($name))
            .unwrap_or($crate::tag_identity::UNKNOWN_TAG_CODE)
    }};
}

/// 运行时受控标签多语言展示宏：将 tag_code 转化为指定母语的自然展示词
///
/// 示例：`tag_display!("builtin.screenshot", "en")` → `"Screenshot"`
#[macro_export]
macro_rules! tag_display {
    ($code:expr, $lang:expr) => {{
        $crate::tag_identity::tag_display($code, $lang)
    }};
}

/// Omni 核心引擎版本号
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// 基础分析与配置规范 (对齐 Desktop ConfigKey)
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct OmniConfig {
    pub enable_office_cover: bool,
    /// 文档 OCR 识别数量上限（Office 内嵌图数 / PDF 页数），0 表示不识别，-1 表示不限
    pub max_document_ocr_items: i32,
    pub enable_image_ocr: bool,
    /// OCR 识别模型精度/尺寸 ('tiny' | 'small' | 'medium')
    pub ocr_model_size: String,
    pub max_content_size_kb: usize,
    pub max_file_size_mb: u64,
    /// 分析模式: 'simple' (极速分类) | 'document' (快速文档摘要) | 'full' (标准 AI 分析)
    pub analysis_mode: String,
    /// 是否复用已有基础分析数据 (跳过已有提取)
    pub reuse_basic_analysis_data: bool,
    /// 音频/视频分析截取时长（秒，默认 30 秒）
    #[serde(default = "default_audio_analysis_duration")]
    pub audio_analysis_duration: u32,
    /// 全局忽略/排除受保护项目名单（用于 czkawka 查重清理原生排除保护）
    #[serde(default)]
    pub excluded_items: Vec<String>,
    /// 是否启用 Tier 1 端侧文本分析（#615~#618 分块/关键词/向量/槽位全流程，默认开启）
    #[serde(default = "default_true")]
    pub enable_text_analysis: bool,
    /// 当前用户界面语言（BCP-47，如 zh-CN），由桌面端 /api/config 同步
    #[serde(default)]
    pub language: Option<String>,
    /// 激活的嵌入画像档位 ('classic_light' | 'gemma_unified'，缺省 'classic_light')
    #[serde(default = "default_embedding_profile")]
    pub embedding_profile: String,
}

fn default_audio_analysis_duration() -> u32 {
    30
}

fn default_true() -> bool {
    true
}

fn default_embedding_profile() -> String {
    "classic_light".to_string()
}

impl Default for OmniConfig {
    fn default() -> Self {
        Self {
            enable_office_cover: false,
            max_document_ocr_items: 0,
            enable_image_ocr: false,
            ocr_model_size: "tiny".to_string(),
            max_content_size_kb: 30,
            max_file_size_mb: 100,
            analysis_mode: "full".to_string(),
            reuse_basic_analysis_data: true,
            audio_analysis_duration: 30,
            excluded_items: Vec::new(),
            enable_text_analysis: true,
            language: None,
            embedding_profile: "classic_light".to_string(),
        }
    }
}

/// Omni 引擎内容提取细分耗时基准统计（精确到毫秒）
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct OmniBenchmark {
    pub total_ms: u64,
    pub magika_ms: Option<u64>,
    pub metadata_ms: Option<u64>,
    pub text_ms: Option<u64>,
    pub document_ms: Option<u64>,
    pub ocr_ms: Option<u64>,
    pub html_ms: Option<u64>,
    pub thumbnail_ms: Option<u64>,
}

/// 全量文件提取结果元数据
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct OmniExtractionResult {
    pub file_path: String,
    pub mime_type: String,
    pub file_size: u64,
    pub markdown_content: String,
    pub metadata: serde_json::Value,
    pub phash: Option<String>,
    pub is_corrupted: bool,
    pub benchmark: Option<OmniBenchmark>,
}

/// 原生多模态感知请求
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OmniPerceptionRequest {
    pub file_path: String,
    #[serde(default)]
    pub language: Option<String>,
    #[serde(default)]
    pub enable_visual_tags: Option<bool>,
    /// 是否启用音视频 ASR 语音转录（`enable_audio_transcript` 为历史别名，保留兼容）
    #[serde(default, alias = "enable_audio_transcript")]
    pub enable_asr: Option<bool>,
    #[serde(default)]
    pub enable_geo_reverse: Option<bool>,
    /// 是否启用 Tier 1 端侧文本分析（可覆盖 OmniConfig.enable_text_analysis，缺省沿用全局配置）
    #[serde(default)]
    pub enable_text_analysis: Option<bool>,
    #[serde(default)]
    pub max_content_size_kb: Option<usize>,
    /// 自定义音频截取时长（秒，缺省时读取 OmniConfig.audio_analysis_duration）
    #[serde(default)]
    pub audio_analysis_duration: Option<u32>,
}

/// 原生多模态感知细分耗时
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct OmniPerceptionBenchmark {
    pub total_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extract_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ads_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vision_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub audio_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub geo_ms: Option<u64>,

    // 细分任务动态指标字典 (通过 serde(flatten) 在 JSON 顶层平铺展开)
    // 自动兼容 clip_ms, nsfw_ms, ram_ms, text_detect_ms 等现有与未来任意新增子任务
    #[serde(flatten)]
    pub subtasks: std::collections::BTreeMap<String, u64>,
}

impl OmniPerceptionBenchmark {
    pub fn new() -> Self {
        Self::default()
    }

    /// 记录某项细分子任务耗时（毫秒）
    pub fn record(&mut self, name: impl Into<String>, ms: u64) {
        self.subtasks.insert(name.into(), ms);
    }

    /// 记录可选耗时值（仅当 Some 时记录）
    pub fn record_opt(&mut self, name: impl Into<String>, ms: Option<u64>) {
        if let Some(val) = ms {
            self.subtasks.insert(name.into(), val);
        }
    }

    /// 获取某项细分子任务耗时
    pub fn get(&self, name: &str) -> Option<u64> {
        self.subtasks.get(name).copied()
    }

    /// 批量合并/扩展细分子任务耗时
    pub fn extend_subtasks<I>(&mut self, iter: I)
    where
        I: IntoIterator<Item = (String, u64)>,
    {
        self.subtasks.extend(iter);
    }
}

/// 通用子任务测量工具：执行闭包，返回 (结果, 任务标识名, 耗时ms)
#[inline]
pub fn measure_subtask<T, F: FnOnce() -> T>(name: &'static str, f: F) -> (T, &'static str, u64) {
    let start = std::time::Instant::now();
    let res = f();
    let elapsed = start.elapsed().as_millis() as u64;
    (res, name, elapsed)
}

/// 快捷宏：在 thread scope 中度量执行，返回 (res, elapsed_ms)
/// 与视觉流水线 join 处的 (结果, 耗时) 二元组解构约定保持一致
#[macro_export]
macro_rules! timed_spawn {
    ($scope:expr, $task:expr) => {
        $scope.spawn(|| {
            let start = std::time::Instant::now();
            let res = $task;
            (res, start.elapsed().as_millis() as u64)
        })
    };
}

/// 强类型媒体领域枚举 (多父通用拓扑仲裁，Task 1 / ADR-0030)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MediaDomain {
    Video,
    Audio,
    Ebook,
    Manga,
    Image,
    Archive,
    Code,
    Document,
    General,
}

impl MediaDomain {
    pub fn from_str_loose(s: &str) -> Self {
        match s.to_ascii_lowercase().as_str() {
            "video" | "影视" | "视频" => Self::Video,
            "audio" | "音频" | "音乐" => Self::Audio,
            "ebook" | "e_book" | "book" | "电子书" | "小说" => Self::Ebook,
            "manga" | "comic" | "漫画" => Self::Manga,
            "image" | "picture" | "photo" | "图片" | "图像" => Self::Image,
            "archive" | "zip" | "压缩包" => Self::Archive,
            "code" | "source_code" | "源代码" | "代码" => Self::Code,
            "document" | "doc" | "文档" => Self::Document,
            _ => Self::General,
        }
    }
}

/// 统一多模态标签链项 (融合语义数据库 file_tags 全字段属性)
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct TagChainItem {
    pub code: String,
    #[serde(alias = "tag")]
    pub name: String,
    pub confidence: f32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub parent_codes: Vec<String>,
    /// 本次标注实际经由的消歧父级 code（概念 B 消歧锚；真根为 None 或空串，与概念 A parent_codes 标签树多父数组正交，ADR-0047）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub via_parent_code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub materialized_paths: Option<String>,
    /// 完整物化代码路径 (如 "/builtin.file_type/builtin.image/builtin.subject_type/omw.01846331.n")
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code_path: Option<String>,
    /// 完整物化展示名路径 (如 "/文件类型/图片/主体类型/鸭子")
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name_path: Option<String>,
    // 创世字段治理：parent_name_chain / parent_code_chain 已彻底废除，由 name_path / code_path 取代；
    // pack 来源列已收敛为 source（dimension/tag/hownet/omw），废除 category
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sememe: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slot_role: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sort_order: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// 打标引擎来源 (WP2a 来源标记，与 pack 语义的 source 字段正交):
    /// "clip" | "ram" | "physical" | "mutual_group" | "ocr" | "nsfw" | "quality" | "rule" | "metadata"
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub engine: Option<String>,
    /// 词性标识（封闭单字母枚举，ADR-0052 §5，GH #712 S7）：
    ///
    /// | 字母 | HowNet 原生        | 含义         | 准入策略                     |
    /// |------|--------------------|--------------|------------------------------|
    /// | `n`  | noun               | 名词         | G2 放行，可标注              |
    /// | `v`  | verb               | 动词         | 白名单豁免后可标注           |
    /// | `a`  | adj                | 形容词       | G2 放行，可标注              |
    /// | `r`  | adv                | 副词         | 白名单豁免后可标注           |
    /// | `m`  | num                | 数词         | 保留词库，禁标注（造句需要） |
    /// | `q`  | classifier         | 量词         | 保留词库，禁标注（造句需要） |
    /// | `p`  | prep               | 介词         | 保留词库，禁标注（造句需要） |
    /// | `o`  | echo               | 拟声词       | 保留词库，禁标注（造句需要） |
    /// | `y`  | pron               | 代词         | 保留词库，禁标注（造句需要） |
    /// | `c`  | conj+coor          | 连词         | 保留词库，禁标注（造句需要） |
    /// | `u`  | aux+stru           | 助词/结构助词| 保留词库，禁标注（造句需要） |
    /// | `h`  | wh                 | 疑问词       | 保留词库，禁标注（造句需要） |
    /// | `w`  | pun                | 标点         | 清洗阶段删除                 |
    /// | `x`  | letter+prefix/expr | 字母前缀/表达式 | 清洗阶段删除/重归类        |
    /// | `b`  | char               | 单字         | 清洗阶段删除                 |
    ///
    /// 热集未覆盖（约 15%）时字段为 `None`（冷集降级，有计数告警，非静默）。
    /// **禁止从 code 尾段推导 pos**（ADR-0052 §5 硬约束 3）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pos: Option<String>,

    /// 44大类语义范畴 (noun.artifact等)，具象实体与虚词判据
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lexfile: Option<String>,
    /// 概念树层级深度 (Min BFS Depth，具象度评分与虚词熔断)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub depth: Option<u32>,
    /// 专名个体标识 (true=专名个体，受 R1 保护不被折叠)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_instance: Option<bool>,
}

impl TagChainItem {
    pub fn new(code: impl Into<String>, name: impl Into<String>, confidence: f32) -> Self {
        Self {
            code: code.into(),
            name: name.into(),
            confidence,
            ..Default::default()
        }
    }

    pub fn with_parent(mut self, parent_code: impl Into<String>) -> Self {
        self.parent_codes = vec![parent_code.into()];
        self
    }

    /// 链式设置本次实际经由的消歧父级 code（概念 B）
    pub fn with_via_parent(mut self, via_parent_code: impl Into<String>) -> Self {
        self.via_parent_code = Some(via_parent_code.into());
        self
    }
}

/// 兼容别名: RamTagItem 指向统一 TagChainItem
pub type RamTagItem = TagChainItem;

/// 全量原生多模态感知结果 (收拢元数据、频域算子、视觉标签、语音转录与物理事实)
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct OmniPerceptionResult {
    pub file_path: String,
    pub mime_type: String,
    pub file_size: u64,
    pub category: Option<String>,
    pub markdown_content: String,
    pub ocr_text: Option<String>,
    pub metadata: serde_json::Value,

    // 物理事实特征
    pub file_source: Option<String>,
    /// 来源代码: "downloaded" | "intranet" | "local" | "system" (语言中立，对应维度 ID: 3)
    pub file_source_code: Option<String>,
    pub source_url: Option<String>,
    pub workflow_state: Option<String>,
    /// 工作流处理状态代码: "draft" | "reviewing" | "completed" | "archived" | "unarchived" (对应维度 ID: 18)
    pub workflow_state_code: Option<String>,
    pub security_level: Option<String>,
    /// 安全密级代码: "top_secret" | "confidential" | "internal" | "public" (对应维度 ID: 17)
    pub security_level_code: Option<String>,
    pub has_watermark: Option<bool>,
    /// 水印等级: 0 (无水印), 1 (轻水印), 2 (有水印) (对应维度 ID: 125 tags 下标)
    pub watermark_level: Option<u8>,
    pub watermark_status: Option<String>,
    pub has_mosaic: Option<bool>,
    /// 打码等级: 0 (无码), 1 (薄码), 2 (有码) (对应维度 ID: 124 tags 下标)
    pub mosaic_level: Option<u8>,
    pub mosaic_status: Option<String>,
    /// 图片是否含有文本特征
    pub has_text: Option<bool>,
    /// 图像美学与画质综合得分 (1.0 ~ 10.0)
    pub aesthetic_score: Option<f32>,
    /// 文件质量评分 (1.0 ~ 10.0)
    pub quality_score: Option<f32>,
    /// 图像形态细分分类 (如: 聊天截图 / 合同票据 / 证照 / 摄影照片 等)
    pub photo_type: Option<String>,
    /// 照片技术与画质状态评估标签 (如: 优质精选 / 暗光欠曝 / 逆光死白 / 高ISO噪点 / 模糊废片 等)
    pub quality_issues: Vec<String>,

    // 多模态直出字段与各大引擎标签 (统一走标签链输出)
    pub visual_tags: Vec<TagChainItem>,
    pub morphology_tags: Vec<String>,
    pub clip_tags: Vec<String>,
    pub nsfw_tags: Vec<String>,
    /// 文本/文档路径的敏感内容标签（FastText 分类器 + 关键词匹配，与视觉 NSFW 标签分流透出）
    /// 图片/视频路径此字段为空，文本/文档路径 `nsfw_tags` 为空此字段有值
    #[serde(default)]
    pub nsfw_text_tags: Vec<String>,
    #[serde(default)]
    pub ram_tags: Vec<String>,
    #[serde(default, skip_serializing)]
    pub morphology_high_confidence_tags: Vec<String>,
    #[serde(default, alias = "raw_clip_tags", alias = "clip_raw_tags")]
    pub clip_high_confidence_tags: Vec<String>,
    #[serde(default, skip_serializing, alias = "raw_nsfw_tags", alias = "nsfw_raw_tags")]
    pub nsfw_high_confidence_tags: Vec<String>,
    pub sensitive_types: Vec<String>,
    pub content_rating: Option<String>,
    /// ASR 语音转录文本（Issue 0046 §1：由 `audio_transcript` 标准化更名，废弃旧命名）
    #[serde(default)]
    pub asr: Option<String>,
    /// 是否存在有效 ASR 转录文本（去空白后非空）
    #[serde(default)]
    pub has_asr: bool,
    /// ASR 转录文本字符长度（Unicode 标量计数，非字节数）
    #[serde(default)]
    pub asr_length: usize,
    #[serde(default)]
    pub lrc: Option<String>,
    pub audio_events: Vec<String>,
    pub geo_address: Option<String>,

    // 级联提示词合成与语义仲裁终局结果 (新增统一透出)
    #[serde(default)]
    pub candidate_hypotheses: Vec<CandidateHypothesisItem>,
    #[serde(default)]
    pub winning_hypothesis: Option<WinningHypothesisItem>,
    #[serde(default)]
    pub activated_dimension_tags: Vec<TagChainItem>,
    /// 第三阶段双锚点交叉向量验证与互斥门禁融合出的终极完美标签集 (第一落库权威)
    #[serde(default)]
    pub fused_tags: Vec<TagChainItem>,
    /// 原生事实标签直出链 (`fact` 组)：元数据直读 (ExifTool/音频/文档/图像) + 下沉既有物理事实
    ///
    /// 原名 `meta_tags`，因其中相当一部分标签并非来自元数据（来源/状态/密级/质量/语言为下沉物理事实），
    /// 与「标签来源分组」概念对齐后更名为 `fact_tags`（见 ADR-0045 §Decision 1）。
    /// 置信度**扁平统一 0.90**（物理直读与规则推导同级），`engine` 统一标记为 "metadata"。
    /// Desktop 端作为第一权威事实无损落库。
    #[serde(default)]
    pub fact_tags: Vec<TagChainItem>,
    #[serde(default)]
    pub smart_name: Option<String>,
    #[serde(default)]
    pub content_description: Option<String>,
    #[serde(default)]
    pub pruned_ambiguous_words: Vec<String>,

    // Tier 1 端侧纯 CPU 确定性文本特征与向量
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub keywords: Vec<String>,
    #[serde(default)]
    pub entities: Vec<serde_json::Value>,
    #[serde(default)]
    pub structured_summary: Option<serde_json::Value>,
    #[serde(default)]
    pub one_sentence_desc: Option<String>,
    #[serde(default)]
    pub name_slots: Option<serde_json::Value>,
    #[serde(default)]
    pub embedding_dense: Option<Vec<f32>>,

    pub phash: Option<String>,
    pub is_corrupted: bool,
    pub benchmark: Option<OmniPerceptionBenchmark>,
}

/// 级联提示词候选假设项 (用于 CLIP 文本向量仲裁)
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CandidateHypothesisItem {
    pub id: String,
    pub prompt_text: String,
    pub confidence: f32,
    pub is_winner: bool,
    pub slots: serde_json::Value,
    #[serde(default)]
    pub bound_tags: Vec<TagChainItem>,
}

/// 胜出的最佳语义假设项
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct WinningHypothesisItem {
    pub prompt_text: String,
    pub confidence: f32,
}

/// 标准多模态语义上下文结构 (供第三阶段融合仲裁使用)
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MultimodalContext {
    pub file_path: String,
    pub file_name: String,
    pub mime_type: String,
    pub document_text: Option<String>,
    pub ocr_text: Option<String>,
    /// 语音事实：ASR 转录文本（外部契约字段名为 `asr`，此处沿用内部历史命名以兼容融合引擎）
    pub audio_transcript: Option<String>,
    pub lrc_text: Option<String>,
    pub visual_tags: Vec<TagChainItem>,
    pub exif_metadata: serde_json::Value,
    pub is_image: bool,
    pub is_document: bool,
    pub is_audio_or_video: bool,
    pub language: Option<String>,
    /// 动态维度执行策略字典 (dim_code -> policy)
    #[serde(default)]
    pub dimension_policies: Option<std::collections::HashMap<String, DimensionExecutionPolicy>>,
}

/// 解析「可参与事实推导的文本正文」（Issue 0046 §1）。
///
/// 优先级：复合文档正文（markdown_content）> 纯图片 OCR 文本 > 音视频 ASR 转录。
///
/// 纯图片/音视频不再把 OCR/ASR 写入 `markdown_content`（保持正文结构纯粹），
/// 但文字存在性判定、Tier1 文本分析与混合检索索引仍必须消费这些语义，
/// 故统一经本函数解析出「有效文本」。抽为纯函数便于单测与跨调用点复用，避免口径漂移。
pub fn resolve_effective_text(
    markdown_content: &str,
    ocr_text: Option<&str>,
    asr: Option<&str>,
) -> String {
    if !markdown_content.trim().is_empty() {
        return markdown_content.to_string();
    }
    if let Some(ocr) = ocr_text.filter(|s| !s.trim().is_empty()) {
        return ocr.to_string();
    }
    asr.unwrap_or_default().to_string()
}

/// 单指标音频转录请求: POST /api/audio/transcribe
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AudioTranscribeRequest {    pub file_path: String,
    #[serde(default)]
    pub language: Option<String>,
    /// 自定义截取转录时长（秒）
    #[serde(default)]
    pub duration_seconds: Option<u32>,
}

/// 单指标音频转录响应
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AudioTranscribeResponse {
    pub file_path: String,
    pub transcript: Option<String>,
    pub events: Vec<String>,
    pub language: Option<String>,
    pub duration_ms: u64,
}

/// 音频转标准格式请求 (16kHz Mono PCM WAV + 降噪): POST /api/audio/convert
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AudioConvertRequest {
    pub file_path: String,
    #[serde(default)]
    pub duration_seconds: Option<u32>,
}

/// 音频转标准格式响应
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AudioConvertResponse {
    pub file_path: String,
    pub output_path: String,
    pub duration_seconds: u32,
    pub duration_ms: u64,
}

/// 单指标视觉标签请求: POST /api/vision/tags
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VisionTagsRequest {
    #[serde(alias = "file_path")]
    pub file_path: String,
    #[serde(default)]
    pub language: Option<String>,
    #[serde(default, alias = "top_k")]
    pub top_k: Option<usize>,
}

/// 单指标视觉标签响应
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct VisionTagsResponse {
    #[serde(alias = "file_path")]
    pub file_path: String,
    pub tags: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scored_tags: Option<Vec<(String, f32)>>,
    #[serde(alias = "duration_ms")]
    pub duration_ms: u64,
}

/// 单指标图像频域特征检测请求: POST /api/vision/inspect
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VisionInspectRequest {
    pub file_path: String,
}

/// 单指标图像频域特征检测响应
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct VisionInspectResponse {
    pub file_path: String,
    pub has_watermark: bool,
    pub watermark_level: u8,
    pub watermark_status: String,
    pub has_mosaic: bool,
    pub mosaic_level: u8,
    pub mosaic_status: String,
    pub duration_ms: u64,
}

/// 单指标文件系统 ADS 来源检测请求: POST /api/fs/ads
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FsAdsRequest {
    pub file_path: String,
}

/// 单指标文件系统 ADS 来源检测响应
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct FsAdsResponse {
    pub file_path: String,
    pub file_source: Option<String>,
    pub file_source_code: Option<String>,
    pub source_url: Option<String>,
    pub duration_ms: u64,
}

/// 查重扫描请求
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DuplicateScanRequest {
    pub paths: Vec<String>,
    pub strategies: Option<Vec<String>>,
    pub min_similarity: Option<f32>,
    pub check_video: Option<bool>,
    /// 异常命名检测模式: 'multilingual' (默认: 保留中文/日韩等多语言合规文件名，仅检查首尾空格、非法控制字符等) | 'strict_ascii' (严格纯ASCII模式，非ASCII转写拼音)
    pub name_issues_mode: Option<String>,
    /// 排除/受保护的目录或文件项名单（如 .VirtualDirectory, node_modules, .git 等，czkawka 原生跳过）
    pub excluded_items: Option<Vec<String>>,
}

/// 查重文件项
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OmniDuplicateFileItem {
    pub path: String,
    pub name: String,
    pub size: u64,
    pub modified_at: String,
    pub fingerprint: String,
    pub similarity_score: Option<f32>,
}

/// 查重聚合组
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OmniDuplicateGroup {
    pub group_id: String,
    pub strategy: String,
    pub similarity_percentage: f32,
    /// 当前组实际踩线的最低相似度阈值（若相似度低于此组阈值则无法匹配入组）
    pub group_threshold: Option<f32>,
    pub description: String,
    pub files: Vec<OmniDuplicateFileItem>,
    pub potential_freed_bytes: u64,
}

/// 查重扫描响应
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DuplicateScanResponse {
    pub success: bool,
    pub total_scanned: usize,
    pub duplicate_groups: Vec<OmniDuplicateGroup>,
    pub total_redundant_files: usize,
    pub total_freed_bytes: u64,
    pub duration_ms: u64,
}

/// 查重/清理修复请求 (Exif 擦除 / 视频优化转码)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DuplicateFixRequest {
    pub action: String,
    pub paths: Vec<String>,
}

/// 查重/清理修复响应
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DuplicateFixResponse {
    pub success: bool,
    pub action: String,
    pub success_count: usize,
    pub failed_count: usize,
    pub processed_paths: Vec<String>,
    pub errors: Vec<String>,
}

impl OmniExtractionResult {
    /// 计算图像/音频/视频 感知指纹 (czkawka_core / omni pHash)
    pub fn compute_phash<P: AsRef<std::path::Path>>(path: P) -> Option<String> {
        let p = path.as_ref();
        let metadata = std::fs::metadata(p).ok()?;
        let len = metadata.len();
        if len == 0 {
            return None;
        }

        use std::io::{Read, Seek, SeekFrom};
        let mut file = std::fs::File::open(p).ok()?;
        let mut hasher: u64 = len.wrapping_mul(31);

        // 1. 读取文件头部 64KB 数据
        let mut head_buf = [0u8; 65536];
        if let Ok(n) = file.read(&mut head_buf) {
            for b in &head_buf[..n] {
                hasher = hasher.wrapping_mul(31).wrapping_add(*b as u64);
            }
        }

        // 2. 如果文件大于 128KB，读取文件尾部 64KB 数据
        if len > 131072 {
            if file.seek(SeekFrom::End(-65536)).is_ok() {
                let mut tail_buf = [0u8; 65536];
                if let Ok(n) = file.read(&mut tail_buf) {
                    for b in &tail_buf[..n] {
                        hasher = hasher.wrapping_mul(31).wrapping_add(*b as u64);
                    }
                }
            }
        }

        Some(format!("{:016x}", hasher))
    }

    /// 检测破损文件 (基本空文件及可读性检查)
    pub fn check_corrupted<P: AsRef<std::path::Path>>(path: P) -> bool {
        let p = path.as_ref();
        if let Ok(metadata) = std::fs::metadata(p) {
            return metadata.len() == 0;
        }
        true
    }
}

/// 将 Path/PathBuf 统一转换为符合当前操作系统原生标准的路径字符串（Windows 下为 \，Unix/macOS 下为 /）
#[inline]
pub fn to_native_path_str<P: AsRef<std::path::Path>>(path: P) -> String {
    let raw = path.as_ref().to_string_lossy().to_string();
    if cfg!(target_os = "windows") {
        raw.replace('/', "\\")
    } else {
        raw.replace('\\', "/")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 票 05 契约测试：TagChainItem A/B 两轴语义正交分离（漂移④终结）
    /// - A 轴：parent_codes 树状 DAG 多父数组
    /// - B 轴：via_parent_code 本次经由单值消歧锚
    #[test]
    fn test_tag_chain_item_ab_axis_separation() {
        let item = TagChainItem::new("builtin.image.cat", "猫", 0.95)
            .with_parent("builtin.image.animal")
            .with_via_parent("builtin.image.mammal");

        // 断言 A 轴与 B 轴取值独立，不再发生「一次值双写」
        assert_eq!(item.parent_codes, vec!["builtin.image.animal"]);
        assert_eq!(item.via_parent_code.as_deref(), Some("builtin.image.mammal"));

        // 序列化与反序列化验证
        let json_str = serde_json::to_string(&item).expect("序列化失败");
        assert!(json_str.contains("\"via_parent_code\":\"builtin.image.mammal\""));
        assert!(json_str.contains("\"parent_codes\":[\"builtin.image.animal\"]"));

        let deserialized: TagChainItem = serde_json::from_str(&json_str).expect("反序列化失败");
        assert_eq!(deserialized.via_parent_code, Some("builtin.image.mammal".to_string()));
        assert_eq!(deserialized.parent_codes, vec!["builtin.image.animal".to_string()]);
    }
}

/// 依据受控 code 反查中文权威展示名 (Canonical Lemma)
///
/// 经 [`tag_identity::concept_from_code`] 桥接：`Concept` 注册表与别名表对同一概念
/// 存在 code 分歧（GH #719），直查 `from_code` 会静默落空、展示名退化成裸 slug。
#[inline(always)]
pub fn get_canonical_concept_name(code: &str) -> Option<&'static str> {
    tag_identity::concept_from_code(code).map(|item| item.zh_name())
}

/// 核心受控根维度强类型枚举集合
pub const ROOT_DIMENSION_CONCEPTS: &[concepts::Concept] = &[
    concepts::Concept::文件类型,
    concepts::Concept::文件用途,
    concepts::Concept::文件来源,
    concepts::Concept::作者,
    concepts::Concept::文件质量,
    concepts::Concept::安全等级,
    concepts::Concept::语言细分,
    concepts::Concept::处理状态,
    concepts::Concept::内容尺度,
    concepts::Concept::内容标签,
];

/// 判定指定 code 是否为受控根维度 (内置 builtin.author / builtin.fileType 别名收敛)
#[inline]
pub fn is_root_dimension(code: &str) -> bool {
    code == "builtin.author" || code == "builtin.fileType" || ROOT_DIMENSION_CONCEPTS.iter().any(|item| item.code() == code)
}



