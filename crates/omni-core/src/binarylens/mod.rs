use serde::{Deserialize, Serialize};
use std::path::Path;
use thiserror::Error;

/// 二进制静态分析与分类事实结构
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct BinaryAnalysisInfo {
    pub format: String,
    pub architecture: String,
    pub product_name: Option<String>,
    pub company_name: Option<String>,
    pub file_description: Option<String>,
    pub file_version: Option<String>,
    pub legal_copyright: Option<String>,
    pub signer_organization: Option<String>,
    pub detected_framework_code: Option<String>,
    pub capability_codes: Vec<String>,
    pub category_code: Option<String>,
    pub confidence_score: f32,
}

#[derive(Error, Debug)]
pub enum BinaryLensError {
    #[error("文件 I/O 错误: {0}")]
    Io(#[from] std::io::Error),
    #[error("不支持的二进制格式: {0}")]
    UnsupportedFormat(String),
    #[error("解析失败: {0}")]
    ParseFailure(String),
}

/// 开源版存根：二进制深度解析能力位于 omni-pro
pub fn analyze_binary(_path: &Path) -> Result<BinaryAnalysisInfo, BinaryLensError> {
    Err(BinaryLensError::UnsupportedFormat("BinaryLens requires omni-pro".to_string()))
}
