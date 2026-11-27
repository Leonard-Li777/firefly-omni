//! 运行期规则资产统一加载（fail-closed）。
//!
//! 背景见 `docs/specs/omni-failure-path-fail-closed-spec.md`：规则资产缺失/损坏时**禁止**
//! 回退编译期内置副本，也禁止逐字段补默认（会产出"资产字段 + 内置字段"的混血配置）。
//! 本模块提供四类失败语义、严格 JSON 读取、以及"只缓存成功值 + 有界负缓存"的资产槽。
//!
//! 设计要点：
//! 1. `AssetSlot` 只缓存**成功**的 `Arc<T>`；失败按 `NEGATIVE_CACHE_TTL` 做有界负缓存，
//!    避免热路径（逐标签 G1 判定）在资产损坏时放大磁盘 IO，但失败不会被当成正常态永久固化。
//! 2. `require()` 在资产不可用时**直接 panic**（装配错误不允许静默降级）；启动装配应优先
//!    调用各模块的 `init_*` 拿到 `Result`，让进程在服务前就明确失败。
//! 3. 路径解析收敛在此处：显式环境变量 > 固定候选 > 可执行文件向上 8 层；
//!    **删除**工作目录（CWD）逐级向上 walk —— 同一资产在不同工作目录命中不同文件本身就是漂移源。

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::Serialize;

/// 规则资产种类（强类型，禁止字符串判定）
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AssetKind {
    TagAdmissibility,
    NsfwConfig,
    NsfwRules,
    VocabLayers,
    DomainMatrix,
    SemanticPack,
    Tagger,
}

impl AssetKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            AssetKind::TagAdmissibility => "tag_admissibility",
            AssetKind::NsfwConfig => "nsfw_config",
            AssetKind::NsfwRules => "nsfw_rules",
            AssetKind::VocabLayers => "vocab_layers",
            AssetKind::DomainMatrix => "domain_matrix",
            AssetKind::SemanticPack => "semantic_pack",
            AssetKind::Tagger => "tagger",
        }
    }
}

/// 资产加载失败语义（Spec §2.2 & Visual Tagger v2.12 §3.3）
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AssetError {
    /// 应随包分发但找不到
    AssetMissing {
        kind: AssetKind,
        path: Option<PathBuf>,
    },
    /// 找到但解码/校验失败（含半字段、版本错配、空判据集合）
    AssetCorrupt {
        kind: AssetKind,
        path: Option<PathBuf>,
        reason: String,
    },
    /// 挂载陈旧、模型/schema 版本不兼容或摘要失配（HTTP 409 ENGINE_NOT_READY）
    Incompatible {
        kind: AssetKind,
        path: Option<PathBuf>,
        reason: String,
    },
    /// 该能力/资产本就未安装（可选插件资产）
    NotProvisioned { kind: AssetKind },
}

impl AssetError {
    pub fn kind(&self) -> AssetKind {
        match self {
            AssetError::AssetMissing { kind, .. } => *kind,
            AssetError::AssetCorrupt { kind, .. } => *kind,
            AssetError::Incompatible { kind, .. } => *kind,
            AssetError::NotProvisioned { kind } => *kind,
        }
    }

    /// HTTP 层与日志共用的稳定错误码
    pub fn code(&self) -> &'static str {
        match self {
            AssetError::AssetMissing { .. } => "ASSET_MISSING",
            AssetError::AssetCorrupt { .. } => "ASSET_CORRUPT",
            AssetError::Incompatible { .. } => "ENGINE_NOT_READY",
            AssetError::NotProvisioned { .. } => "NOT_PROVISIONED",
        }
    }

    pub fn status(&self) -> AssetStatus {
        match self {
            AssetError::AssetMissing { .. } => AssetStatus::Missing,
            AssetError::AssetCorrupt { .. } => AssetStatus::Corrupt,
            AssetError::Incompatible { .. } => AssetStatus::Stale,
            AssetError::NotProvisioned { .. } => AssetStatus::NotProvisioned,
        }
    }

    pub fn path(&self) -> Option<&Path> {
        match self {
            AssetError::AssetMissing { path, .. } => path.as_deref(),
            AssetError::AssetCorrupt { path, .. } => path.as_deref(),
            AssetError::Incompatible { path, .. } => path.as_deref(),
            AssetError::NotProvisioned { .. } => None,
        }
    }

    pub fn reason_str(&self) -> Option<&str> {
        match self {
            AssetError::AssetCorrupt { reason, .. } => Some(reason.as_str()),
            AssetError::Incompatible { reason, .. } => Some(reason.as_str()),
            _ => None,
        }
    }

    fn reason_text(&self) -> Option<String> {
        match self {
            AssetError::AssetCorrupt { reason, .. } => Some(reason.clone()),
            AssetError::Incompatible { reason, .. } => Some(reason.clone()),
            AssetError::AssetMissing { .. } => Some("资产文件缺失".to_string()),
            AssetError::NotProvisioned { .. } => None,
        }
    }
}

impl std::fmt::Display for AssetError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AssetError::AssetMissing { kind, path } => match path {
                Some(p) => write!(f, "{}: {}（{} 缺失）", self.code(), kind.as_str(), p.display()),
                None => write!(f, "{}: {}（未找到资产）", self.code(), kind.as_str()),
            },
            AssetError::AssetCorrupt { kind, path, reason } => match path {
                Some(p) => write!(
                    f,
                    "{}: {}（{}）: {}",
                    self.code(),
                    kind.as_str(),
                    p.display(),
                    reason
                ),
                None => write!(f, "{}: {}: {}", self.code(), kind.as_str(), reason),
            },
            AssetError::Incompatible { kind, path, reason } => match path {
                Some(p) => write!(
                    f,
                    "{}: {}（{}）: {}",
                    self.code(),
                    kind.as_str(),
                    p.display(),
                    reason
                ),
                None => write!(f, "{}: {}: {}", self.code(), kind.as_str(), reason),
            },
            AssetError::NotProvisioned { kind } => {
                write!(f, "{}: {}（能力未安装）", self.code(), kind.as_str())
            }
        }
    }
}

impl std::error::Error for AssetError {}

/// 资产状态（健康端点闭包值）
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AssetStatus {
    Ok,
    Missing,
    Corrupt,
    Stale,
    NotProvisioned,
}

impl AssetStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            AssetStatus::Ok => "ok",
            AssetStatus::Missing => "missing",
            AssetStatus::Corrupt => "corrupt",
            AssetStatus::Stale => "stale",
            AssetStatus::NotProvisioned => "not_provisioned",
        }
    }
}

/// 资产健康快照（供 `/health.assets` 与诊断面板）
#[derive(Debug, Clone, Serialize)]
pub struct AssetHealth {
    pub status: AssetStatus,
    /// 资产来源：**仅允许** `asset`（内置来源已在本规格中删除）
    pub source: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<PathBuf>,
    /// 资产自身声明的版本号；缺失即视为 corrupt（严禁"无版本视为兼容"）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_failure_at_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retry_after_ms: Option<u64>,
}

impl AssetHealth {
    /// 成功态（`source` 恒为 `asset`）
    pub fn ok_asset(path: Option<PathBuf>, version: Option<String>) -> Self {
        AssetHealth {
            status: AssetStatus::Ok,
            source: "asset",
            path,
            version,
            reason: None,
            last_failure_at_ms: None,
            retry_after_ms: None,
        }
    }

    /// 失败态（供未接入 `AssetSlot` 的模块做只读探测时构造）
    pub fn from_error(err: &AssetError) -> Self {
        AssetHealth::failed(err, None)
    }

    fn ok(path: Option<PathBuf>, version: Option<String>) -> Self {
        AssetHealth::ok_asset(path, version)
    }

    fn failed(err: &AssetError, retry_after_ms: Option<u64>) -> Self {
        AssetHealth {
            status: err.status(),
            source: "asset",
            path: err.path().map(|p| p.to_path_buf()),
            version: None,
            reason: err.reason_text(),
            last_failure_at_ms: Some(now_ms()),
            retry_after_ms,
        }
    }
}

impl Default for AssetHealth {
    fn default() -> Self {
        AssetHealth {
            status: AssetStatus::Missing,
            source: "asset",
            path: None,
            version: None,
            reason: Some("尚未加载".to_string()),
            last_failure_at_ms: None,
            retry_after_ms: None,
        }
    }
}

/// 损坏态重复打盘的下限间隔（规格书 §4.7：≤ 1 次 / 30s）
pub const NEGATIVE_CACHE_TTL: Duration = Duration::from_secs(30);

/// 一次成功加载的结果（值 + 来源路径 + 资产声明版本）
pub struct LoadedAsset<T> {
    pub value: Arc<T>,
    pub path: Option<PathBuf>,
    pub version: Option<String>,
}

/// 资产槽：只缓存成功值；失败走有界负缓存，可 `reload()` 热恢复
pub struct AssetSlot<T: 'static> {
    value: RwLock<Option<Arc<T>>>,
    health: RwLock<AssetHealth>,
    last_error: Mutex<Option<AssetError>>,
    retry_at: Mutex<Option<Instant>>,
}

impl<T: 'static> Default for AssetSlot<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T: 'static> AssetSlot<T> {
    pub const fn new() -> Self {
        AssetSlot {
            value: RwLock::new(None),
            health: RwLock::new(AssetHealth {
                status: AssetStatus::Missing,
                source: "asset",
                path: None,
                version: None,
                reason: None,
                last_failure_at_ms: None,
                retry_after_ms: None,
            }),
            last_error: Mutex::new(None),
            retry_at: Mutex::new(None),
        }
    }

    /// 取当前成功快照（未加载或上次失败则为 None）
    pub fn snapshot(&self) -> Option<Arc<T>> {
        self.value.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub fn health(&self) -> AssetHealth {
        self.health
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// 取快照；不可用时**panic**（装配错误必须炸出来，禁止回退内置值）
    pub fn require(&self, kind: AssetKind) -> Arc<T> {
        match self.snapshot() {
            Some(v) => v,
            None => {
                let detail = self
                    .last_error
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .clone()
                    .map(|e| e.to_string())
                    .unwrap_or_else(|| "尚未初始化".to_string());
                panic!(
                    "规则资产不可用（装配错误，禁止回退内置默认）: kind={} detail={}；\
                     请检查安装包 extraResources 是否完整，或显式设置 FFP_TAXONOMY_DIR",
                    kind.as_str(),
                    detail
                )
            }
        }
    }

    /// 加载（或复用已有成功值）。
    ///
    /// - `force=true` 即 `reload()` 语义：忽略已有值与负缓存，重新走 Resolve→Read→Decode→Validate；
    /// - `force=false` 时：已有成功值直接返回；处于负缓存窗口内直接返回上次错误（不再打盘）。
    pub fn load_or_get<F>(&self, force: bool, kind: AssetKind, loader: F) -> Result<Arc<T>, AssetError>
    where
        F: FnOnce() -> Result<LoadedAsset<T>, AssetError>,
    {
        if !force {
            if let Some(v) = self.snapshot() {
                return Ok(v);
            }
            let mut guard = self.retry_at.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(at) = *guard {
                if Instant::now() < at {
                    if let Some(err) = self
                        .last_error
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .clone()
                    {
                        return Err(err);
                    }
                }
                *guard = None;
            }
        }

        match loader() {
            Ok(loaded) => {
                let arc = Arc::clone(&loaded.value);
                *self.value.write().unwrap_or_else(|e| e.into_inner()) = Some(arc.clone());
                *self.health.write().unwrap_or_else(|e| e.into_inner()) =
                    AssetHealth::ok(loaded.path, loaded.version);
                *self.last_error.lock().unwrap_or_else(|e| e.into_inner()) = None;
                *self.retry_at.lock().unwrap_or_else(|e| e.into_inner()) = None;
                tracing::info!(kind = kind.as_str(), "规则资产加载成功");
                Ok(arc)
            }
            Err(err) => {
                self.record_failure(&err);
                tracing::error!(kind = kind.as_str(), error = %err, "规则资产加载失败（fail-closed）");
                Err(err)
            }
        }
    }

    /// 显式写入槽位失败状态与有界负缓存，并清空旧快照（用于加载后编译失败的 Fail-Closed 隔离）
    pub fn record_failure(&self, err: &AssetError) {
        *self.value.write().unwrap_or_else(|e| e.into_inner()) = None;
        *self.health.write().unwrap_or_else(|e| e.into_inner()) = AssetHealth::failed(
            err,
            Some(NEGATIVE_CACHE_TTL.as_millis() as u64),
        );
        *self.last_error.lock().unwrap_or_else(|e| e.into_inner()) = Some(err.clone());
        *self.retry_at.lock().unwrap_or_else(|e| e.into_inner()) =
            Some(Instant::now() + NEGATIVE_CACHE_TTL);
    }
}

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// 读取并解析 JSON 资产（严格：IO 失败分类、语法错误即 corrupt）
pub fn read_json(kind: AssetKind, path: &Path) -> Result<serde_json::Value, AssetError> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(AssetError::AssetMissing {
                kind,
                path: Some(path.to_path_buf()),
            })
        }
        Err(e) => {
            return Err(AssetError::AssetCorrupt {
                kind,
                path: Some(path.to_path_buf()),
                reason: format!("读取失败: {e}"),
            })
        }
    };
    serde_json::from_str::<serde_json::Value>(&text).map_err(|e| AssetError::AssetCorrupt {
        kind,
        path: Some(path.to_path_buf()),
        reason: format!("JSON 解析失败: {e}"),
    })
}

/// 取必需子对象（缺失即 corrupt）
pub fn require_object<'a>(
    parent: &'a serde_json::Value,
    kind: AssetKind,
    path: &Path,
    key: &str,
) -> Result<&'a serde_json::Value, AssetError> {
    parent.get(key).ok_or_else(|| AssetError::AssetCorrupt {
        kind,
        path: Some(path.to_path_buf()),
        reason: format!("缺少必需字段 `{key}`"),
    })
}

/// 取必需字符串数组（缺失/空数组/含空串 均 corrupt）
pub fn require_str_array(
    parent: &serde_json::Value,
    kind: AssetKind,
    path: &Path,
    key: &str,
    lowercase: bool,
) -> Result<Vec<String>, AssetError> {
    let arr = parent
        .get(key)
        .and_then(|v| v.as_array())
        .ok_or_else(|| AssetError::AssetCorrupt {
            kind,
            path: Some(path.to_path_buf()),
            reason: format!("字段 `{key}` 缺失或不是数组"),
        })?;
    let mut out: Vec<String> = Vec::with_capacity(arr.len());
    for item in arr {
        let Some(s) = item.as_str() else {
            return Err(AssetError::AssetCorrupt {
                kind,
                path: Some(path.to_path_buf()),
                reason: format!("字段 `{key}` 含非字符串元素"),
            });
        };
        let s = s.trim();
        if s.is_empty() {
            return Err(AssetError::AssetCorrupt {
                kind,
                path: Some(path.to_path_buf()),
                reason: format!("字段 `{key}` 含空字符串"),
            });
        }
        out.push(if lowercase { s.to_lowercase() } else { s.to_string() });
    }
    if out.is_empty() {
        // 空判据集合 = 非法资源（否则判据静默 fail-open）
        return Err(AssetError::AssetCorrupt {
            kind,
            path: Some(path.to_path_buf()),
            reason: format!("字段 `{key}` 为空数组（空判据集合不得视为合法资源）"),
        });
    }
    Ok(out)
}

/// 取必需的双语（zh/en）字符串数组（如 stopwords / banned_lemmas / shell_nouns）
pub fn require_bilingual_str_array(
    parent: &serde_json::Value,
    kind: AssetKind,
    path: &Path,
    key: &str,
) -> Result<Vec<String>, AssetError> {
    let obj = parent
        .get(key)
        .and_then(|v| v.as_object())
        .ok_or_else(|| AssetError::AssetCorrupt {
            kind,
            path: Some(path.to_path_buf()),
            reason: format!("字段 `{key}` 缺失或不是对象（应为 zh/en 双语数组）"),
        })?;
    let mut all: Vec<String> = Vec::new();
    for lang in ["zh", "en"] {
        let arr = obj
            .get(lang)
            .and_then(|v| v.as_array())
            .ok_or_else(|| AssetError::AssetCorrupt {
                kind,
                path: Some(path.to_path_buf()),
                reason: format!("字段 `{key}.{lang}` 缺失或不是数组"),
            })?;
        if arr.is_empty() {
            return Err(AssetError::AssetCorrupt {
                kind,
                path: Some(path.to_path_buf()),
                reason: format!("字段 `{key}.{lang}` 为空数组（空判据集合不得视为合法资源）"),
            });
        }
        for item in arr {
            let Some(s) = item.as_str() else {
                return Err(AssetError::AssetCorrupt {
                    kind,
                    path: Some(path.to_path_buf()),
                    reason: format!("字段 `{key}.{lang}` 含非字符串元素"),
                });
            };
            let s = s.trim();
            if s.is_empty() {
                return Err(AssetError::AssetCorrupt {
                    kind,
                    path: Some(path.to_path_buf()),
                    reason: format!("字段 `{key}.{lang}` 含空字符串"),
                });
            }
            all.push(s.to_lowercase());
        }
    }
    if all.is_empty() {
        return Err(AssetError::AssetCorrupt {
            kind,
            path: Some(path.to_path_buf()),
            reason: format!("字段 `{key}` 双语数组均为空"),
        });
    }
    Ok(all)
}

/// 取并校验资产版本号（必填；缺失或非标量即 corrupt）
pub fn require_version(
    root: &serde_json::Value,
    kind: AssetKind,
    path: &Path,
) -> Result<String, AssetError> {
    let v = root.get("version").ok_or_else(|| AssetError::AssetCorrupt {
        kind,
        path: Some(path.to_path_buf()),
        reason: "缺少必需字段 `version`（严禁无版本视为兼容）".to_string(),
    })?;
    let ver = match v {
        serde_json::Value::String(s) if !s.trim().is_empty() => s.trim().to_string(),
        serde_json::Value::Number(n) => n.to_string(),
        _ => {
            return Err(AssetError::AssetCorrupt {
                kind,
                path: Some(path.to_path_buf()),
                reason: "字段 `version` 必须是字符串或数字".to_string(),
            })
        }
    };
    Ok(ver)
}

/// 基于显式 `&ResourceLayout` 定位 taxonomy 规则资产目录
pub fn resolve_taxonomy_dir_with_layout(
    layout: &crate::ResourceLayout,
    required_file: &str,
) -> Option<PathBuf> {
    let dir = layout.taxonomy_dir();
    if dir.is_dir() && dir.join(required_file).is_file() {
        Some(dir)
    } else {
        None
    }
}

/// 定位 taxonomy 规则资产目录（**与工作目录完全无关**）。
///
/// 优先级：已注入的 `ResourceLayout` > 可执行文件相对布局（独立测试兜底）。
/// `required_file` 用于避免"目录存在但装的是别的资产集"导致的跨资产漂移。
pub fn resolve_taxonomy_dir(required_file: &str) -> Option<PathBuf> {
    if let Some(layout) = crate::global_resource_layout() {
        // 已由 --bootstrap 注入全局 ResourceLayout 时严格收口，不再回退环境变量或 8 层目录猜谜
        return resolve_taxonomy_dir_with_layout(&layout, required_file);
    }

    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Ok(raw) = std::env::var("FFP_TAXONOMY_DIR") {
        let trimmed = raw.trim();
        if !trimmed.is_empty() {
            candidates.push(PathBuf::from(trimmed));
        }
    }

    if let Ok(exe_path) = std::env::current_exe() {
        if let Some(mut current) = exe_path.parent() {
            for _ in 0..8 {
                candidates.push(current.join("taxonomy"));
                candidates.push(current.join("build/extraResources/taxonomy"));
                candidates.push(current.join("apps/desktop/build/extraResources/taxonomy"));
                candidates.push(current.join("apps/desktop/pro/build/extraResources/taxonomy"));
                match current.parent() {
                    Some(p) => current = p,
                    None => break,
                }
            }
        }
    }

    for cand in candidates {
        if cand.is_dir() && cand.join(required_file).is_file() {
            return Some(cand);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// §3.3 健康块契约：`status ∈ ok|missing|corrupt|not_provisioned`、`source` 恒为 `asset`，
    /// 成功态必须带资产自身 `version`。该测试同时锁定 `json!({"assets": AssetHealth})` 的嵌入形态。
    #[test]
    fn health_json_shape_matches_spec() {
        let ok = serde_json::json!({
            "assets": AssetHealth::ok_asset(
                Some(PathBuf::from("apps/desktop/build/extraResources/taxonomy/vocab_layers.json")),
                Some("1".to_string())
            )
        });
        assert_eq!(ok["assets"]["status"], "ok");
        assert_eq!(ok["assets"]["source"], "asset");
        assert_eq!(ok["assets"]["version"], "1");

        let missing = AssetError::AssetMissing {
            kind: AssetKind::NsfwRules,
            path: None,
        };
        let bad = serde_json::json!({ "assets": AssetHealth::from_error(&missing) });
        assert_eq!(bad["assets"]["status"], "missing");
        assert_eq!(bad["assets"]["source"], "asset");
        assert!(
            bad["assets"].get("version").is_none(),
            "失败态不得伪造 version"
        );

        let stale = AssetError::Incompatible {
            kind: AssetKind::Tagger,
            path: None,
            reason: "visual_vocab_stale".to_string(),
        };
        assert_eq!(stale.code(), "ENGINE_NOT_READY");
        assert_eq!(stale.status(), AssetStatus::Stale);
        assert_eq!(stale.kind().as_str(), "tagger");
        let stale_health = serde_json::json!({ "assets": AssetHealth::from_error(&stale) });
        assert_eq!(stale_health["assets"]["status"], "stale");
        assert_eq!(stale_health["assets"]["reason"], "visual_vocab_stale");
    }
}

static E2E_VISUAL_VOCAB_READ_COUNT: AtomicU64 = AtomicU64::new(0);
static E2E_ASSETS_RELOAD_COUNT: AtomicU64 = AtomicU64::new(0);
static E2E_LAST_RELOAD_AT_MS: AtomicU64 = AtomicU64::new(0);
static E2E_SNAPSHOT_REVISION: AtomicU64 = AtomicU64::new(0);

/// 记录一次真实的视觉词表磁盘读取（仅用于 E2E 诊断验证无逐请求读盘）
#[inline]
pub fn record_visual_vocab_disk_read() {
    E2E_VISUAL_VOCAB_READ_COUNT.fetch_add(1, Ordering::SeqCst);
}

/// 读取累计视觉词表磁盘读取次数
#[inline]
pub fn visual_vocab_disk_read_count() -> u64 {
    E2E_VISUAL_VOCAB_READ_COUNT.load(Ordering::SeqCst)
}

/// 记录一次 `/api/v1/assets/reload` 完成事件
#[inline]
pub fn record_assets_reload(at_ms: u64) {
    E2E_ASSETS_RELOAD_COUNT.fetch_add(1, Ordering::SeqCst);
    E2E_LAST_RELOAD_AT_MS.store(at_ms, Ordering::SeqCst);
}

/// 返回 `(assets_reload_count, last_reload_at_ms)`
#[inline]
pub fn assets_reload_stats() -> (u64, Option<u64>) {
    let count = E2E_ASSETS_RELOAD_COUNT.load(Ordering::SeqCst);
    let last = E2E_LAST_RELOAD_AT_MS.load(Ordering::SeqCst);
    (count, if last == 0 { None } else { Some(last) })
}

/// 单调递增 `VisualRuntimeSnapshot` 修订号
#[inline]
pub fn bump_visual_snapshot_revision() -> u64 {
    E2E_SNAPSHOT_REVISION.fetch_add(1, Ordering::SeqCst) + 1
}

/// 读取当前 `VisualRuntimeSnapshot` 修订号
#[inline]
pub fn current_visual_snapshot_revision() -> u64 {
    E2E_SNAPSHOT_REVISION.load(Ordering::SeqCst)
}

/// 单请求视觉前向真实执行计数器（挂载于请求工作线程局部作用域）
#[derive(Debug, Default)]
pub struct RequestVisionCounters {
    pub image_vision_encoding_calls: AtomicU32,
    pub vision_session_calls: AtomicU32,
    pub multimodal_projection_calls: AtomicU32,
    pub tagging_prompt_text_encoding_calls: AtomicU32,
    pub synthetic_hypothesis_text_encoding_calls: AtomicU32,
}

impl RequestVisionCounters {
    pub fn snapshot_diagnostics(&self) -> crate::VisualPipelineDiagnostics {
        crate::VisualPipelineDiagnostics {
            image_vision_encoding_calls: self.image_vision_encoding_calls.load(Ordering::SeqCst),
            vision_session_calls: self.vision_session_calls.load(Ordering::SeqCst),
            multimodal_projection_calls: self.multimodal_projection_calls.load(Ordering::SeqCst),
            tagging_prompt_text_encoding_calls: self
                .tagging_prompt_text_encoding_calls
                .load(Ordering::SeqCst),
            synthetic_hypothesis_text_encoding_calls: self
                .synthetic_hypothesis_text_encoding_calls
                .load(Ordering::SeqCst),
            visual_vocab_read_count: visual_vocab_disk_read_count(),
        }
    }
}

thread_local! {
    static ACTIVE_REQUEST_VISION_COUNTERS: RefCell<Option<Arc<RequestVisionCounters>>> = const { RefCell::new(None) };
}

pub struct RequestVisionCountersGuard {
    prev: Option<Arc<RequestVisionCounters>>,
}

impl Drop for RequestVisionCountersGuard {
    fn drop(&mut self) {
        ACTIVE_REQUEST_VISION_COUNTERS.with(|cell| {
            *cell.borrow_mut() = self.prev.take();
        });
    }
}

/// 在当前同步执行线程作用域内挂载请求级视觉计数器（RAII 自动还原）
pub fn attach_request_vision_counters(
    counters: Arc<RequestVisionCounters>,
) -> RequestVisionCountersGuard {
    let prev = ACTIVE_REQUEST_VISION_COUNTERS.with(|cell| cell.borrow_mut().replace(counters));
    RequestVisionCountersGuard { prev }
}

#[inline]
pub fn record_image_vision_encoding_call() {
    ACTIVE_REQUEST_VISION_COUNTERS.with(|cell| {
        if let Some(c) = cell.borrow().as_ref() {
            c.image_vision_encoding_calls.fetch_add(1, Ordering::SeqCst);
        }
    });
}

#[inline]
pub fn record_vision_session_call() {
    ACTIVE_REQUEST_VISION_COUNTERS.with(|cell| {
        if let Some(c) = cell.borrow().as_ref() {
            c.vision_session_calls.fetch_add(1, Ordering::SeqCst);
        }
    });
}

#[inline]
pub fn record_multimodal_projection_call() {
    ACTIVE_REQUEST_VISION_COUNTERS.with(|cell| {
        if let Some(c) = cell.borrow().as_ref() {
            c.multimodal_projection_calls.fetch_add(1, Ordering::SeqCst);
        }
    });
}

#[inline]
pub fn record_tagging_prompt_text_encoding_call() {
    ACTIVE_REQUEST_VISION_COUNTERS.with(|cell| {
        if let Some(c) = cell.borrow().as_ref() {
            c.tagging_prompt_text_encoding_calls
                .fetch_add(1, Ordering::SeqCst);
        }
    });
}

#[inline]
pub fn record_synthetic_hypothesis_text_encoding_call() {
    ACTIVE_REQUEST_VISION_COUNTERS.with(|cell| {
        if let Some(c) = cell.borrow().as_ref() {
            c.synthetic_hypothesis_text_encoding_calls
                .fetch_add(1, Ordering::SeqCst);
        }
    });
}

