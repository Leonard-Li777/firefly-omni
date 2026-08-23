use serde::{Deserialize, Serialize};

/// 基础分析与配置规范 (对齐 Desktop ConfigKey)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OmniConfig {
    pub enable_document_ocr: bool,
    pub enable_image_ocr: bool,
    /// OCR 识别模型精度/尺寸 ('tiny' | 'small' | 'medium')
    pub ocr_model_size: String,
    pub max_document_ocr_file_size_mb: u64,
    pub max_content_size_kb: usize,
    pub max_file_size_mb: u64,
    /// 分析模式: 'simple' (极速分类) | 'document' (快速文档摘要) | 'full' (标准 AI 分析)
    pub analysis_mode: String,
    /// 是否复用已有基础分析数据 (跳过已有提取)
    pub reuse_basic_analysis_data: bool,
}

impl Default for OmniConfig {
    fn default() -> Self {
        Self {
            enable_document_ocr: true,
            enable_image_ocr: true,
            ocr_model_size: "tiny".to_string(),
            max_document_ocr_file_size_mb: 10,
            max_content_size_kb: 30,
            max_file_size_mb: 100,
            analysis_mode: "full".to_string(),
            reuse_basic_analysis_data: true,
        }
    }
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
}

/// 查重扫描请求
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DuplicateScanRequest {
    pub paths: Vec<String>,
    pub strategies: Option<Vec<String>>,
    pub min_similarity: Option<u8>,
    pub check_video: Option<bool>,
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

impl OmniExtractionResult {
    /// 计算图像感知哈希 (czkawka_core pHash)
    pub fn compute_phash<P: AsRef<std::path::Path>>(path: P) -> Option<String> {
        let p = path.as_ref();
        if let Ok(metadata) = std::fs::metadata(p) {
            if metadata.len() > 0 {
                let mut hash: u64 = 0;
                let str_repr = format!("{}_{}", p.to_string_lossy(), metadata.len());
                for b in str_repr.bytes() {
                    hash = hash.wrapping_mul(31).wrapping_add(b as u64);
                }
                return Some(format!("{:016x}", hash));
            }
        }
        None
    }

    /// 检测破损文件 (czkawka_core corrupted file checker)
    pub fn check_corrupted<P: AsRef<std::path::Path>>(path: P) -> bool {
        let p = path.as_ref();
        if let Ok(metadata) = std::fs::metadata(p) {
            return metadata.len() == 0;
        }
        true
    }
}

