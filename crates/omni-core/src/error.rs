use std::fmt;
use std::path::PathBuf;

/// Omni 系统统一强类型错误枚举 (SPEC §3 / Ticket 07)
///
/// 取代跨层 anyhow 弱类型字符串匹配，为上层路由、指标采集与硬件两阶段降级
/// 提供确定性匹配语义与固定分类标识。
#[derive(Debug, Clone)]
pub enum OmniError {
    /// 磁盘物理权重缺失（离线开发态唯一合法识别点）
    ModelMissing { path: PathBuf },

    /// 权重存在但 Session 构建/初始化失败（文件损坏、ORT 初始化失败、DLL 契约断裂等）
    ModelLoadFailed { path: PathBuf, source: String },

    /// 硬件加速运行期推理前向失败（触发两阶段回退与同帧 CPU 重建）
    DmlRuntimeFailed {
        encoder: &'static str,
        source: String,
    },

    /// 同帧 CPU 重建或降级后重跑仍失败（该帧彻底失败，阻断伪向量入库）
    CpuFallbackFailed {
        encoder: &'static str,
        source: String,
    },

    /// 输入参数非法（维度不符、空内容、类型越界等）
    InvalidInput {
        field: &'static str,
        reason: String,
    },

    /// 通用运行时未捕获错误
    Internal(String),
}

impl fmt::Display for OmniError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ModelMissing { path } => write!(f, "模型权重文件缺失: {path:?}"),
            Self::ModelLoadFailed { path, source } => {
                write!(f, "模型装载失败 ({path:?}): {source}")
            }
            Self::DmlRuntimeFailed { encoder, source } => {
                write!(f, "编码器 [{encoder}] 运行期推理失败: {source}")
            }
            Self::CpuFallbackFailed { encoder, source } => {
                write!(f, "编码器 [{encoder}] CPU 降级回退重跑失败: {source}")
            }
            Self::InvalidInput { field, reason } => {
                write!(f, "入参校验失败 [{field}]: {reason}")
            }
            Self::Internal(msg) => write!(f, "内部运行时错误: {msg}"),
        }
    }
}

impl std::error::Error for OmniError {}

impl OmniError {
    /// 结构化类型标识（SSOT），供日志打点、监控指标与 HTTP 错误码契约反查，严禁调用方做字符串比对
    pub fn kind(&self) -> &'static str {
        match self {
            Self::ModelMissing { .. } => "model_missing",
            Self::ModelLoadFailed { .. } => "model_load_failed",
            Self::DmlRuntimeFailed { .. } => "dml_runtime_failed",
            Self::CpuFallbackFailed { .. } => "cpu_fallback_failed",
            Self::InvalidInput { .. } => "invalid_input",
            Self::Internal(_) => "internal_error",
        }
    }

    /// 对应的标准 HTTP 状态码数值 (400, 404, 500, 503)
    pub fn http_status_code(&self) -> u16 {
        match self {
            Self::ModelMissing { .. } => 404,
            Self::ModelLoadFailed { .. } => 503,
            Self::DmlRuntimeFailed { .. } => 500,
            Self::CpuFallbackFailed { .. } => 500,
            Self::InvalidInput { .. } => 400,
            Self::Internal(_) => 500,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_omni_error_kinds_are_stable() {
        let e1 = OmniError::ModelMissing {
            path: PathBuf::from("fake.onnx"),
        };
        assert_eq!(e1.kind(), "model_missing");
        assert_eq!(e1.http_status_code(), 404);

        let e2 = OmniError::ModelLoadFailed {
            path: PathBuf::from("fake.onnx"),
            source: "corrupt".into(),
        };
        assert_eq!(e2.kind(), "model_load_failed");
        assert_eq!(e2.http_status_code(), 503);

        let e3 = OmniError::DmlRuntimeFailed {
            encoder: "gemma.vision",
            source: "dxgi reset".into(),
        };
        assert_eq!(e3.kind(), "dml_runtime_failed");
        assert_eq!(e3.http_status_code(), 500);

        let e4 = OmniError::CpuFallbackFailed {
            encoder: "gemma.vision",
            source: "oom".into(),
        };
        assert_eq!(e4.kind(), "cpu_fallback_failed");
        assert_eq!(e4.http_status_code(), 500);

        let e5 = OmniError::InvalidInput {
            field: "vector",
            reason: "dim != 512".into(),
        };
        assert_eq!(e5.kind(), "invalid_input");
        assert_eq!(e5.http_status_code(), 400);
    }

    #[test]
    fn test_omni_error_into_anyhow_preserves_message() {
        let err = OmniError::InvalidInput {
            field: "text",
            reason: "empty text".into(),
        };
        let anyhow_err: anyhow::Error = err.into();
        assert!(anyhow_err.to_string().contains("empty text"));
    }
}
