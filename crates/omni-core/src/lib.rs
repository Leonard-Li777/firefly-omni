use serde::{Deserialize, Serialize};

/// Omni 核心引擎版本号
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// 基础分析与配置规范 (对齐 Desktop ConfigKey)
#[derive(Debug, Clone, Serialize, Deserialize)]
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
    /// 全局忽略/排除受保护项目名单（用于 czkawka 查重清理原生排除保护）
    #[serde(default)]
    pub excluded_items: Vec<String>,
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
            excluded_items: Vec::new(),
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
    #[serde(default)]
    pub enable_audio_transcript: Option<bool>,
    #[serde(default)]
    pub enable_geo_reverse: Option<bool>,
    #[serde(default)]
    pub max_content_size_kb: Option<usize>,
}

/// 原生多模态感知细分耗时
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct OmniPerceptionBenchmark {
    pub total_ms: u64,
    pub extract_ms: Option<u64>,
    pub ads_ms: Option<u64>,
    pub vision_ms: Option<u64>,
    pub audio_ms: Option<u64>,
    pub geo_ms: Option<u64>,
}

/// 全量原生多模态感知结果 (收拢元数据、频域算子、视觉标签、语音转录与物理事实)
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct OmniPerceptionResult {
    pub file_path: String,
    pub mime_type: String,
    pub file_size: u64,
    pub category: Option<String>,
    pub markdown_content: String,
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

    // 多模态直出字段
    pub visual_tags: Vec<String>,
    pub audio_transcript: Option<String>,
    pub audio_events: Vec<String>,
    pub geo_address: Option<String>,

    pub phash: Option<String>,
    pub is_corrupted: bool,
    pub benchmark: Option<OmniPerceptionBenchmark>,
}

/// 单指标音频转录请求: POST /api/audio/transcribe
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AudioTranscribeRequest {
    pub file_path: String,
    #[serde(default)]
    pub language: Option<String>,
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

/// 单指标视觉标签请求: POST /api/vision/tags
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VisionTagsRequest {
    pub file_path: String,
    #[serde(default)]
    pub language: Option<String>,
    #[serde(default)]
    pub top_k: Option<usize>,
}

/// 单指标视觉标签响应
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct VisionTagsResponse {
    pub file_path: String,
    pub tags: Vec<String>,
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
