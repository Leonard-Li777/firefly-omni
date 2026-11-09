use serde::{Deserialize, Serialize};

/// 会话生命周期状态
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SessionLifecycle {
    Unloaded,
    Loading,
    Ready,
    Draining,
    Failed(String),
}

/// 并发准入状态
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AdmissionState {
    Accepting,
    Draining,
    Closed,
}

/// 停止原因契约 (四态闭合，除 error 外)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FinishReason {
    Stop,
    Length,
    ContentFilter,
    Cancelled,
}

/// 任务类型
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum GenerativeTaskId {
    ShortName,
    Tag,
    QualityScore,
    DirectoryPlan,
}

impl GenerativeTaskId {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::ShortName => "short-name",
            Self::Tag => "tag",
            Self::QualityScore => "quality-score",
            Self::DirectoryPlan => "directory-plan",
        }
    }
}

// ============================================================================
// HTTP 端点请求与响应数据契约
// ============================================================================

/// GET /v1/health 响应
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EngineHealthResponse {
    pub status: String,
    pub pid: u32,
    pub uptime_seconds: u64,
    pub active_tasks: usize,
    pub session: CpuSessionSnapshot,
}

/// CPU Session 详细状态快照
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CpuSessionSnapshot {
    pub lifecycle: SessionLifecycle,
    pub admission: AdmissionState,
    pub model_id: String,
    pub resident_bytes: u64,
    pub peak_rss_bytes: u64,
    pub active_tasks: usize,
    pub last_active_timestamp: u64,
    pub crash_count: u32,
    pub session_epoch: u64,
}

/// POST /v1/engines/cpu/session 请求
#[derive(Debug, Clone, Default, Deserialize)]
pub struct SessionPreheatRequest {
    #[serde(default)]
    pub model_id: Option<String>,
    #[serde(default = "default_wait_true")]
    pub wait: bool,
    #[serde(default)]
    pub reset_crash_count: bool,
}

fn default_wait_true() -> bool {
    true
}

/// POST /v1/engines/cpu/session 响应
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionPreheatResponse {
    pub ok: bool,
    pub lifecycle: SessionLifecycle,
    pub admission: AdmissionState,
    pub resident_bytes: u64,
    pub model_id: String,
}

/// DELETE /v1/engines/cpu/session Query 参数
#[derive(Debug, Clone, Deserialize)]
pub struct SessionDrainQuery {
    #[serde(default = "default_wait_drain_ms")]
    pub wait_drain_ms: u64,
}

fn default_wait_drain_ms() -> u64 {
    5000
}

/// DELETE /v1/engines/cpu/session 响应
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionDrainResponse {
    pub ok: bool,
    pub lifecycle: SessionLifecycle,
    pub drained: bool,
    pub released_bytes: u64,
}

/// POST /v1/tokenize 请求
#[derive(Debug, Clone, Deserialize)]
pub struct TokenizeRequest {
    #[serde(default)]
    pub model: Option<String>,
    pub texts: Vec<String>,
}

/// POST /v1/tokenize 响应
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenizeResponse {
    pub tokens: Vec<Vec<u32>>,
    pub counts: Vec<usize>,
    pub total_tokens: usize,
}

/// 对话消息项
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatCompletionMessage {
    pub role: String,
    pub content: String,
}

/// POST /v1/chat/completions 请求
#[derive(Debug, Clone, Deserialize)]
pub struct ChatCompletionRequest {
    pub model: String,
    #[serde(default)]
    pub task_id: Option<String>,
    pub messages: Vec<ChatCompletionMessage>,
    #[serde(default)]
    pub temperature: Option<f32>,
    #[serde(default)]
    pub max_tokens: Option<usize>,
}

/// POST /v1/chat/completions 响应 Choice
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatCompletionChoice {
    pub index: usize,
    pub message: ChatCompletionMessage,
    pub finish_reason: FinishReason,
}

/// POST /v1/chat/completions 响应 Usage
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatCompletionUsage {
    pub prompt_tokens: usize,
    pub completion_tokens: usize,
    pub total_tokens: usize,
}

/// POST /v1/chat/completions 响应
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatCompletionResponse {
    pub id: String,
    pub object: &'static str,
    pub created: u64,
    pub model: String,
    pub choices: Vec<ChatCompletionChoice>,
    pub usage: ChatCompletionUsage,
}

/// 统一错误响应结构
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorDetail {
    pub code: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub param: Option<String>,
    pub r#type: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiErrorResponse {
    pub error: ErrorDetail,
}

impl ApiErrorResponse {
    pub fn new(code: impl Into<String>, message: impl Into<String>, r#type: impl Into<String>) -> Self {
        Self {
            error: ErrorDetail {
                code: code.into(),
                message: message.into(),
                param: None,
                r#type: r#type.into(),
            },
        }
    }
}
