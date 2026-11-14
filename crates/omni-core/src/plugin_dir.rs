//! 全系统模型目录解析与激活挂载清单单轨门禁 (SSOT Mount Guard)
//!
//! 对齐 `docs/specs/plugin-shared-ssot-convergence-spec.md` §2.3 & [ISS-01]/[ISS-03]/[ISS-04]：
//! 1. **单轨契约**：当设置 `FIREFLY_ACTIVE_MODEL_MOUNTS_FILE` 时，仅从该 JSON 挂载清单解析激活且通过物理门禁的模型目录，严禁回退读取任意散落环境变量或默认探测目录。
//! 2. **严格 Fail-Closed**：当 `FIREFLY_STRICT_ACTIVE_PLUGINS=1|true` 且挂载清单缺失、不可读、JSON 损坏或 `FIREFLY_SHARED_DIR` 非法时，显式拒绝挂载一切受控插件资源。
//! 3. **Rust 侧最小物理断言**：校验每个 mount 项经过 `canonicalize` 后严格位于规范 `_shared` 根目录内、命中至少一组 `requiredFileGroups`、组内首个主权重字节数满足硬底线（如 `bekko-a8m >= 50_000_000`）且伴随文件非零。

use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::env;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::UNIX_EPOCH;

/// 挂载清单环境变量名
pub const ENV_ACTIVE_MODEL_MOUNTS_FILE: &str = "FIREFLY_ACTIVE_MODEL_MOUNTS_FILE";
/// 严格插件挂载门禁开关环境变量名
pub const ENV_STRICT_ACTIVE_PLUGINS: &str = "FIREFLY_STRICT_ACTIVE_PLUGINS";
/// 全局受控 `_shared` 共享根目录环境变量名
pub const ENV_SHARED_DIR: &str = "FIREFLY_SHARED_DIR";

/// MobileCLIP 英文视觉词表运行期校验预期元数据（随挂载清单由主进程单一权威下发）
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct MountVocabMeta {
    pub file_name: String,
    pub text_file_name: String,
    pub vocab_version: String,
    pub vocab_sha256: String,
    pub vocab_text_sha256: String,
    pub vocab_entry_count: usize,
    pub embed_dim: usize,
}

/// 单个共享资源的激活挂载项（由主进程按 SSOT `BUILTIN_PLUGIN_CATALOG_META` 与激活状态生成）
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ActiveMountEntry {
    pub resource_id: String,
    #[serde(default)]
    pub module_id: String,
    #[serde(default)]
    pub capability_slot: String,
    pub dir: String,
    #[serde(default)]
    pub env_var: String,
    #[serde(default)]
    pub env_value: String,
    #[serde(default)]
    pub required_file_groups: Vec<Vec<String>>,
    #[serde(default)]
    pub min_primary_file_bytes: u64,
    #[serde(default)]
    pub tower: Option<String>,
    #[serde(default)]
    pub dim: Option<usize>,
    #[serde(default)]
    pub encoder_id: Option<u32>,
    #[serde(default)]
    pub vocab_meta: Option<MountVocabMeta>,
}

/// `active-mounts-*.json` 根结构
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ActiveMountsManifest {
    pub version: u32,
    #[serde(default)]
    pub generated_at: u64,
    #[serde(default)]
    pub pid: u32,
    #[serde(default)]
    pub shared_dir: String,
    #[serde(default)]
    pub strict: bool,
    #[serde(default)]
    pub mounts: BTreeMap<String, ActiveMountEntry>,
}

/// 已通过 Rust 最小物理断言的运行时有效挂载项
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ValidatedMount {
    pub resource_id: String,
    pub module_id: String,
    pub capability_slot: String,
    pub dir: PathBuf,
    pub primary_file: Option<PathBuf>,
    pub primary_file_bytes: u64,
    pub env_value_path: PathBuf,
    pub validation_level: String,
    pub tower: Option<String>,
    pub dim: Option<usize>,
    pub encoder_id: Option<u32>,
    pub vocab_meta: Option<MountVocabMeta>,
}

/// 被拒绝的挂载项（供 `/api/v1/engine/status` 自愈诊断）
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RejectedMount {
    pub resource_id: String,
    pub dir: String,
    pub reason: String,
}

/// `/api/v1/engine/status` 回显的挂载清单遥测快照
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct MountManifestStatus {
    /// `manifest_single_track` | `manifest_error` | `legacy_env_fallback` | `strict_missing_manifest`
    pub mode: String,
    pub strict: bool,
    pub manifest_path: Option<String>,
    pub shared_dir: Option<String>,
    pub active_resource_ids: Vec<String>,
    pub active_mounts: BTreeMap<String, ValidatedMountSummary>,
    pub rejected_mounts: Vec<RejectedMount>,
    pub validation_level: String,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ValidatedMountSummary {
    pub module_id: String,
    pub capability_slot: String,
    pub dir: String,
    pub primary_file_bytes: u64,
    pub validation_level: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vocab_version: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ResolvedMountState {
    pub status: MountManifestStatus,
    pub valid_mounts: HashMap<String, ValidatedMount>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CacheKey {
    manifest_path: Option<String>,
    strict_env: Option<String>,
    shared_dir_env: Option<String>,
    mtime_nanos: Option<u128>,
    file_len: Option<u64>,
}

static MANIFEST_CACHE: OnceLock<Mutex<Option<(CacheKey, ResolvedMountState)>>> = OnceLock::new();

fn strict_flag_enabled(val: Option<&str>) -> bool {
    matches!(
        val.map(|s| s.trim().to_ascii_lowercase()).as_deref(),
        Some("1" | "true" | "yes" | "on")
    )
}

/// 内建硬底线：防止篡改清单将 `minPrimaryFileBytes` 调零绕过 Rust 物理检查
pub fn builtin_hard_floor_bytes(resource_id: &str) -> u64 {
    match resource_id {
        "bekko-a8m" => 50_000_000,
        "pocket-cpu-int8-onnx" => 10_000_000,
        "chinese-clip" | "mobile-clip" | "nsfw-mobilenetv2" | "nsfw-l3" | "sensevoice-onnx"
        | "moss-tts" | "m5-tts" => 1_000_000,
        "embeddinggemma-2-onnx" => 100_000,
        _ => 1,
    }
}

/// 仅限非 manifest 独立测试/CLI 运行时的旧环境变量名映射
pub fn builtin_legacy_env_var(resource_id: &str) -> Option<&'static str> {
    match resource_id {
        "bekko-a8m" => Some("BEKKO_MODEL_PATH"),
        "chinese-clip" => Some("CLIP_MODEL_PATH"),
        "mobile-clip" => Some("MOBILECLIP_MODEL_PATH"),
        "embeddinggemma-2-onnx" => Some("GEMMA_MODEL_PATH"),
        "pocket-cpu-int8-onnx" => Some("FIREFLY_POCKET_CPU_MODEL_DIR"),
        "nsfw-mobilenetv2" => Some("NSFW_MODEL_PATH"),
        "nsfw-l3" => Some("NSFW_L3_MODEL_PATH"),
        "sensevoice-onnx" => Some("FIREFLY_SHERPA_SENSEVOICE_DIR"),
        "moss-tts" => Some("OMNI_MOSS_MODEL_PATH"),
        "m5-tts" => Some("OMNI_M5_MODEL_PATH"),
        _ => None,
    }
}

fn canonicalize_existing_dir(p: &Path) -> Result<PathBuf, String> {
    let meta = fs::metadata(p).map_err(|e| format!("目录不可访问 ({}): {}", p.display(), e))?;
    if !meta.is_dir() {
        return Err(format!("路径不是目录: {}", p.display()));
    }
    fs::canonicalize(p).map_err(|e| format!("canonicalize 失败 ({}): {}", p.display(), e))
}

fn is_safe_relative_path(rel: &str) -> bool {
    if rel.trim().is_empty() {
        return false;
    }
    let path = Path::new(rel);
    if path.is_absolute() {
        return false;
    }
    !path
        .components()
        .any(|c| matches!(c, Component::ParentDir | Component::RootDir | Component::Prefix(_)))
}

fn validate_single_mount(
    resource_id: &str,
    entry: &ActiveMountEntry,
    canon_shared_dir: &Path,
) -> Result<ValidatedMount, String> {
    if entry.resource_id != resource_id {
        return Err(format!(
            "resourceId 键名不一致: key={}, entry={}",
            resource_id, entry.resource_id
        ));
    }

    let raw_dir = PathBuf::from(&entry.dir);
    let canon_dir = canonicalize_existing_dir(&raw_dir)?;

    if !canon_dir.starts_with(canon_shared_dir) || canon_dir == canon_shared_dir {
        return Err(format!(
            "越界挂载路径被拒绝: dir={} 未落于 sharedDir={} 之下",
            canon_dir.display(),
            canon_shared_dir.display()
        ));
    }

    let effective_min_bytes = entry
        .min_primary_file_bytes
        .max(builtin_hard_floor_bytes(resource_id));

    let mut matched_primary: Option<(PathBuf, u64)> = None;
    let mut group_errors = Vec::new();

    if entry.required_file_groups.is_empty() {
        group_errors.push("requiredFileGroups 为空".to_string());
    } else {
        for group in &entry.required_file_groups {
            if group.is_empty() {
                continue;
            }
            let mut ok = true;
            let mut primary_candidate: Option<(PathBuf, u64)> = None;
            for (idx, rel) in group.iter().enumerate() {
                if !is_safe_relative_path(rel) {
                    group_errors.push(format!("非法相对路径: {}", rel));
                    ok = false;
                    break;
                }
                let candidate = canon_dir.join(rel);
                let canon_file = match fs::canonicalize(&candidate) {
                    Ok(cf) => cf,
                    Err(e) => {
                        group_errors.push(format!("缺少文件 {} ({})", rel, e));
                        ok = false;
                        break;
                    }
                };
                if !canon_file.starts_with(&canon_dir) {
                    group_errors.push(format!("符号链接越界逃逸: {}", rel));
                    ok = false;
                    break;
                }
                let meta = match fs::metadata(&canon_file) {
                    Ok(m) if m.is_file() => m,
                    Ok(_) => {
                        group_errors.push(format!("非普通文件: {}", rel));
                        ok = false;
                        break;
                    }
                    Err(e) => {
                        group_errors.push(format!("无法读取元数据 {} ({})", rel, e));
                        ok = false;
                        break;
                    }
                };
                let mut fsize = meta.len();
                if rel == "current.json" {
                    // 若首文件声明为 current.json 指针，穿透解析其引用的加密容器 (.ffpc) 体积进行权威硬底线核验
                    if let Ok(raw_json) = fs::read_to_string(&canon_file) {
                        if let Ok(val) = serde_json::from_str::<serde_json::Value>(&raw_json) {
                            let mut container_bytes = 0u64;
                            let mut resolved_containers = Vec::new();
                            if let Some(act) = val.get("activeFile").and_then(|v| v.as_str()) {
                                resolved_containers.push(act.to_string());
                            } else if let Some(map) = val.get("activeFiles").and_then(|v| v.as_object()) {
                                for v in map.values() {
                                    if let Some(s) = v.as_str() {
                                        resolved_containers.push(s.to_string());
                                    }
                                }
                            }
                            for c_rel in &resolved_containers {
                                if is_safe_relative_path(c_rel) {
                                    let c_path = canon_dir.join(c_rel);
                                    if let Ok(c_meta) = fs::metadata(&c_path) {
                                        if c_meta.is_file() {
                                            container_bytes = container_bytes.saturating_add(c_meta.len());
                                        }
                                    }
                                }
                            }
                            if container_bytes > 0 {
                                fsize = container_bytes;
                            }
                        }
                    }
                }
                if idx == 0 {
                    if fsize < effective_min_bytes {
                        group_errors.push(format!(
                            "主文件 {} 体积 {} < 硬底线 {}",
                            rel, fsize, effective_min_bytes
                        ));
                        ok = false;
                        break;
                    }
                    primary_candidate = Some((canon_file, fsize));
                } else if fsize == 0 {
                    group_errors.push(format!("伴随文件为空 (0B): {}", rel));
                    ok = false;
                    break;
                }
            }
            if ok {
                matched_primary = primary_candidate;
                break;
            }
        }
    }

    let (primary_file, primary_bytes) = match matched_primary {
        Some((p, b)) => (Some(p), b),
        None => {
            return Err(format!(
                "物理文件断言失败: {}",
                group_errors
                    .into_iter()
                    .take(3)
                    .collect::<Vec<_>>()
                    .join("; ")
            ));
        }
    };

    // 检查 envValue（若提供）：如果是文件路径（例如 nsfw-mobilenetv2），确认其 canonicalize 后仍在 canon_dir 之下
    let env_value_path = if entry.env_value.trim().is_empty() {
        canon_dir.clone()
    } else {
        let candidate_env = PathBuf::from(&entry.env_value);
        let canon_env = fs::canonicalize(&candidate_env).map_err(|e| {
            format!(
                "envValue 不可访问 ({}): {}",
                candidate_env.display(),
                e
            )
        })?;
        if !canon_env.starts_with(&canon_dir) {
            return Err(format!(
                "envValue {} 超出挂载目录 {}",
                canon_env.display(),
                canon_dir.display()
            ));
        }
        canon_env
    };

    Ok(ValidatedMount {
        resource_id: resource_id.to_string(),
        module_id: entry.module_id.clone(),
        capability_slot: entry.capability_slot.clone(),
        dir: canon_dir,
        primary_file,
        primary_file_bytes: primary_bytes,
        env_value_path,
        validation_level: "omni_runtime_physical_v1".to_string(),
        tower: entry.tower.clone(),
        dim: entry.dim,
        encoder_id: entry.encoder_id,
        vocab_meta: entry.vocab_meta.clone(),
    })
}

/// 纯参数化清单加载与验证（零全局状态污染，直接供单元测试与环境解析器调用）
pub fn load_manifest_from_path(
    manifest_path: &Path,
    shared_dir_override: Option<&Path>,
    strict: bool,
) -> ResolvedMountState {
    let raw = match fs::read_to_string(manifest_path) {
        Ok(s) => s,
        Err(e) => {
            return ResolvedMountState {
                status: MountManifestStatus {
                    mode: "manifest_error".to_string(),
                    strict,
                    manifest_path: Some(manifest_path.to_string_lossy().to_string()),
                    shared_dir: shared_dir_override.map(|p| p.to_string_lossy().to_string()),
                    active_resource_ids: Vec::new(),
                    active_mounts: BTreeMap::new(),
                    rejected_mounts: Vec::new(),
                    validation_level: "none".to_string(),
                    error: Some(format!("读取挂载清单失败: {}", e)),
                },
                valid_mounts: HashMap::new(),
            };
        }
    };

    let manifest: ActiveMountsManifest = match serde_json::from_str(&raw) {
        Ok(m) => m,
        Err(e) => {
            return ResolvedMountState {
                status: MountManifestStatus {
                    mode: "manifest_error".to_string(),
                    strict,
                    manifest_path: Some(manifest_path.to_string_lossy().to_string()),
                    shared_dir: shared_dir_override.map(|p| p.to_string_lossy().to_string()),
                    active_resource_ids: Vec::new(),
                    active_mounts: BTreeMap::new(),
                    rejected_mounts: Vec::new(),
                    validation_level: "none".to_string(),
                    error: Some(format!("解析挂载清单 JSON 失败: {}", e)),
                },
                valid_mounts: HashMap::new(),
            };
        }
    };

    if manifest.version != 1 {
        return ResolvedMountState {
            status: MountManifestStatus {
                mode: "manifest_error".to_string(),
                strict,
                manifest_path: Some(manifest_path.to_string_lossy().to_string()),
                shared_dir: Some(manifest.shared_dir),
                active_resource_ids: Vec::new(),
                active_mounts: BTreeMap::new(),
                rejected_mounts: Vec::new(),
                validation_level: "none".to_string(),
                error: Some(format!("不支持的挂载清单版本: {}", manifest.version)),
            },
            valid_mounts: HashMap::new(),
        };
    }

    let shared_path_candidate = shared_dir_override
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
        .or_else(|| {
            let trimmed = manifest.shared_dir.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(PathBuf::from(trimmed))
            }
        });

    let canon_shared = match shared_path_candidate {
        Some(p) => match canonicalize_existing_dir(&p) {
            Ok(c) => c,
            Err(e) => {
                return ResolvedMountState {
                    status: MountManifestStatus {
                        mode: "manifest_error".to_string(),
                        strict,
                        manifest_path: Some(manifest_path.to_string_lossy().to_string()),
                        shared_dir: Some(p.to_string_lossy().to_string()),
                        active_resource_ids: Vec::new(),
                        active_mounts: BTreeMap::new(),
                        rejected_mounts: Vec::new(),
                        validation_level: "none".to_string(),
                        error: Some(format!("FIREFLY_SHARED_DIR 校验失败: {}", e)),
                    },
                    valid_mounts: HashMap::new(),
                };
            }
        },
        None => {
            return ResolvedMountState {
                status: MountManifestStatus {
                    mode: "manifest_error".to_string(),
                    strict,
                    manifest_path: Some(manifest_path.to_string_lossy().to_string()),
                    shared_dir: None,
                    active_resource_ids: Vec::new(),
                    active_mounts: BTreeMap::new(),
                    rejected_mounts: Vec::new(),
                    validation_level: "none".to_string(),
                    error: Some("缺少 FIREFLY_SHARED_DIR 与 manifest.sharedDir".to_string()),
                },
                valid_mounts: HashMap::new(),
            };
        }
    };

    // 若同时设置了 FIREFLY_SHARED_DIR 与 manifest.sharedDir，二者 canonicalize 必须一致
    if shared_dir_override.is_some() && !manifest.shared_dir.trim().is_empty() {
        match canonicalize_existing_dir(Path::new(manifest.shared_dir.trim())) {
            Ok(manifest_shared_canon) if manifest_shared_canon == canon_shared => {}
            Ok(manifest_shared_canon) => {
                return ResolvedMountState {
                    status: MountManifestStatus {
                        mode: "manifest_error".to_string(),
                        strict,
                        manifest_path: Some(manifest_path.to_string_lossy().to_string()),
                        shared_dir: Some(canon_shared.to_string_lossy().to_string()),
                        active_resource_ids: Vec::new(),
                        active_mounts: BTreeMap::new(),
                        rejected_mounts: Vec::new(),
                        validation_level: "none".to_string(),
                        error: Some(format!(
                            "manifest.sharedDir ({}) 与 FIREFLY_SHARED_DIR ({}) 不一致",
                            manifest_shared_canon.display(),
                            canon_shared.display()
                        )),
                    },
                    valid_mounts: HashMap::new(),
                };
            }
            Err(e) => {
                return ResolvedMountState {
                    status: MountManifestStatus {
                        mode: "manifest_error".to_string(),
                        strict,
                        manifest_path: Some(manifest_path.to_string_lossy().to_string()),
                        shared_dir: Some(canon_shared.to_string_lossy().to_string()),
                        active_resource_ids: Vec::new(),
                        active_mounts: BTreeMap::new(),
                        rejected_mounts: Vec::new(),
                        validation_level: "none".to_string(),
                        error: Some(format!("manifest.sharedDir 非法: {}", e)),
                    },
                    valid_mounts: HashMap::new(),
                };
            }
        }
    }

    let mut valid_mounts = HashMap::new();
    let mut active_mounts_summary = BTreeMap::new();
    let mut active_resource_ids = Vec::new();
    let mut rejected_mounts = Vec::new();

    for (rid, entry) in &manifest.mounts {
        match validate_single_mount(rid, entry, &canon_shared) {
            Ok(vm) => {
                active_resource_ids.push(rid.clone());
                active_mounts_summary.insert(
                    rid.clone(),
                    ValidatedMountSummary {
                        module_id: vm.module_id.clone(),
                        capability_slot: vm.capability_slot.clone(),
                        dir: vm.dir.to_string_lossy().to_string(),
                        primary_file_bytes: vm.primary_file_bytes,
                        validation_level: vm.validation_level.clone(),
                        vocab_version: vm.vocab_meta.as_ref().map(|v| v.vocab_version.clone()),
                    },
                );
                valid_mounts.insert(rid.clone(), vm);
            }
            Err(reason) => {
                tracing::warn!(
                    "[PluginDir] 挂载清单条目 {} 被 Rust 最小物理断言拒绝: {}",
                    rid,
                    reason
                );
                rejected_mounts.push(RejectedMount {
                    resource_id: rid.clone(),
                    dir: entry.dir.clone(),
                    reason,
                });
            }
        }
    }

    ResolvedMountState {
        status: MountManifestStatus {
            mode: "manifest_single_track".to_string(),
            strict: strict || manifest.strict,
            manifest_path: Some(manifest_path.to_string_lossy().to_string()),
            shared_dir: Some(canon_shared.to_string_lossy().to_string()),
            active_resource_ids,
            active_mounts: active_mounts_summary,
            rejected_mounts,
            validation_level: "omni_runtime_physical_v1".to_string(),
            error: None,
        },
        valid_mounts,
    }
}

fn current_cache_key() -> CacheKey {
    let manifest_path = env::var(ENV_ACTIVE_MODEL_MOUNTS_FILE)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let strict_env = env::var(ENV_STRICT_ACTIVE_PLUGINS).ok();
    let shared_dir_env = env::var(ENV_SHARED_DIR)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());

    let (mtime_nanos, file_len) = manifest_path
        .as_deref()
        .and_then(|p| fs::metadata(p).ok())
        .map(|m| {
            let ns = m
                .modified()
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map(|d| d.as_nanos());
            (ns, Some(m.len()))
        })
        .unwrap_or((None, None));

    CacheKey {
        manifest_path,
        strict_env,
        shared_dir_env,
        mtime_nanos,
        file_len,
    }
}

/// 读取当前进程挂载状态（基于环境变量与 mtime 自动失效缓存）
pub fn current_mount_state() -> ResolvedMountState {
    let key = current_cache_key();
    let cache_mutex = MANIFEST_CACHE.get_or_init(|| Mutex::new(None));
    if let Ok(guard) = cache_mutex.lock() {
        if let Some((cached_key, cached_state)) = guard.as_ref() {
            if cached_key == &key {
                return cached_state.clone();
            }
        }
    }

    let strict = strict_flag_enabled(key.strict_env.as_deref());
    let fresh = match key.manifest_path.as_deref() {
        Some(path_str) => {
            let shared_override = key.shared_dir_env.as_deref().map(Path::new);
            load_manifest_from_path(Path::new(path_str), shared_override, strict)
        }
        None if strict => ResolvedMountState {
            status: MountManifestStatus {
                mode: "strict_missing_manifest".to_string(),
                strict: true,
                manifest_path: None,
                shared_dir: key.shared_dir_env.clone(),
                active_resource_ids: Vec::new(),
                active_mounts: BTreeMap::new(),
                rejected_mounts: Vec::new(),
                validation_level: "none".to_string(),
                error: Some(format!(
                    "已开启 {} 但未设置 {}，严格禁止回退散落环境变量或默认探测路径",
                    ENV_STRICT_ACTIVE_PLUGINS, ENV_ACTIVE_MODEL_MOUNTS_FILE
                )),
            },
            valid_mounts: HashMap::new(),
        },
        None => ResolvedMountState {
            status: MountManifestStatus {
                mode: "legacy_env_fallback".to_string(),
                strict: false,
                manifest_path: None,
                shared_dir: key.shared_dir_env.clone(),
                active_resource_ids: Vec::new(),
                active_mounts: BTreeMap::new(),
                rejected_mounts: Vec::new(),
                validation_level: "legacy_env_only".to_string(),
                error: None,
            },
            valid_mounts: HashMap::new(),
        },
    };

    if let Ok(mut guard) = cache_mutex.lock() {
        *guard = Some((key, fresh.clone()));
    }
    fresh
}

/// 清空进程内挂载清单缓存（测试辅助）
pub fn invalidate_mount_cache() {
    if let Some(m) = MANIFEST_CACHE.get() {
        if let Ok(mut guard) = m.lock() {
            *guard = None;
        }
    }
}

/// 获取 `/api/v1/engine/status` 使用的挂载清单遥测快照
pub fn mount_manifest_status() -> MountManifestStatus {
    current_mount_state().status
}

/// 获取通过门禁校验的激活挂载详情
pub fn get_validated_mount(resource_id: &str) -> Result<Option<ValidatedMount>> {
    let state = current_mount_state();
    match state.status.mode.as_str() {
        "strict_missing_manifest" => Err(anyhow!(
            "{}",
            state
                .status
                .error
                .unwrap_or_else(|| "strict 模式缺少挂载清单".to_string())
        )),
        "manifest_error" => Err(anyhow!(
            "{}",
            state
                .status
                .error
                .unwrap_or_else(|| "挂载清单解析或根目录校验失败".to_string())
        )),
        "manifest_single_track" => {
            if let Some(vm) = state.valid_mounts.get(resource_id) {
                return Ok(Some(vm.clone()));
            }
            if let Some(rej) = state
                .status
                .rejected_mounts
                .iter()
                .find(|r| r.resource_id == resource_id)
            {
                return Err(anyhow!(
                    "共享资源 {} 挂载校验未通过: {}",
                    resource_id,
                    rej.reason
                ));
            }
            // 未出现在激活清单中：明确返回 None（严禁回退散落环境变量！）
            Ok(None)
        }
        _ => {
            // 非 strict 且未提供清单时（如独立 cargo test / CLI）：仅认对应显式环境变量，不回退硬编码探测路径
            let Some(env_key) = builtin_legacy_env_var(resource_id) else {
                return Ok(None);
            };
            let Ok(raw_val) = env::var(env_key) else {
                return Ok(None);
            };
            let trimmed = raw_val.trim();
            if trimmed.is_empty() {
                return Ok(None);
            }
            let p = PathBuf::from(trimmed);
            if !p.exists() {
                return Err(anyhow!(
                    "环境变量 {} 指向路径不存在: {}",
                    env_key,
                    p.display()
                ));
            }
            let dir = if p.is_file() {
                p.parent()
                    .map(Path::to_path_buf)
                    .ok_or_else(|| anyhow!("无法解析 {} 父目录", p.display()))?
            } else {
                p.clone()
            };
            Ok(Some(ValidatedMount {
                resource_id: resource_id.to_string(),
                module_id: String::new(),
                capability_slot: String::new(),
                dir,
                primary_file: if p.is_file() { Some(p.clone()) } else { None },
                primary_file_bytes: 0,
                env_value_path: p,
                validation_level: "legacy_env_only".to_string(),
                tower: None,
                dim: None,
                encoder_id: None,
                vocab_meta: None,
            }))
        }
    }
}

/// 解析已激活挂载目录（单轨 manifest 模式下绝不回退环境变量）
pub fn resolve_mounted_dir(resource_id: &str) -> Result<Option<PathBuf>> {
    Ok(get_validated_mount(resource_id)?.map(|m| m.dir))
}

/// 解析已激活挂载值路径（对文件型环境变量如 `NSFW_MODEL_PATH` 返回具体文件路径，否则返回目录）
pub fn resolve_mounted_path(resource_id: &str) -> Result<Option<PathBuf>> {
    Ok(get_validated_mount(resource_id)?.map(|m| m.env_value_path))
}

/// 强制要求某资源当前已激活且通过物理门禁
pub fn require_mounted_dir(resource_id: &str) -> Result<PathBuf> {
    resolve_mounted_dir(resource_id)?
        .ok_or_else(|| anyhow!("共享资源 {} 未激活或未通过挂载清单门禁", resource_id))
}

/// 读取清单注入的 MobileCLIP / CLIP 预期词表权威元数据
pub fn get_mount_vocab_meta(resource_id: &str) -> Option<MountVocabMeta> {
    get_validated_mount(resource_id)
        .ok()
        .flatten()
        .and_then(|m| m.vocab_meta)
}

fn read_current_json(dir: &Path) -> Option<serde_json::Value> {
    let raw = fs::read_to_string(dir.join("current.json")).ok()?;
    serde_json::from_str(&raw).ok()
}

fn join_safe_ffpc_rel(dir: &Path, rel: &str) -> Option<PathBuf> {
    if !is_safe_relative_path(rel) {
        return None;
    }
    let path = dir.join(rel);
    if path.is_file() {
        Some(path)
    } else {
        None
    }
}

/// 从 `current.json` 的 `activeFiles[key]` 解析激活的加密容器路径
pub fn resolve_ffpc_active_file(dir: &Path, key: &str) -> Option<PathBuf> {
    let val = read_current_json(dir)?;
    let rel = val.get("activeFiles")?.get(key)?.as_str()?;
    join_safe_ffpc_rel(dir, rel)
}

/// 从 `current.json` 的单文件指针 `activeFile` 解析激活的加密容器路径
pub fn resolve_ffpc_single_file(dir: &Path) -> Option<PathBuf> {
    let val = read_current_json(dir)?;
    let rel = val.get("activeFile")?.as_str()?;
    join_safe_ffpc_rel(dir, rel)
}

/// 轻量读取 `.ffpc` 头部 AAD 元数据，判断容器内是否已打包内嵌分词器（不解密整包载荷）
pub fn ffpc_has_embedded_tokenizer(ffpc_path: &Path) -> bool {
    use std::io::Read;
    let Ok(mut f) = fs::File::open(ffpc_path) else {
        return false;
    };
    let mut prefix = [0u8; crate::ffp_crypto::header::FIXED_PREFIX_LEN];
    if f.read_exact(&mut prefix).is_err() {
        return false;
    }
    if prefix[0..4] != crate::ffp_crypto::header::MAGIC_FFPC {
        return false;
    }
    let aad_len = u32::from_le_bytes([prefix[20], prefix[21], prefix[22], prefix[23]]) as usize;
    if aad_len == 0 || aad_len > crate::ffp_crypto::header::MAX_AAD_LEN {
        return false;
    }
    let mut aad_buf = vec![0u8; aad_len];
    if f.read_exact(&mut aad_buf).is_err() {
        return false;
    }
    serde_json::from_slice::<crate::ffp_crypto::AadMetadata>(&aad_buf)
        .map(|meta| meta.tok_len > 0)
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn make_temp_root(prefix: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time")
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "omni_plugin_dir_test_{}_{}_{}",
            prefix,
            std::process::id(),
            nanos
        ));
        fs::create_dir_all(&root).expect("create temp root");
        root
    }

    fn write_bytes(path: &Path, len: usize) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create parent");
        }
        let mut f = fs::File::create(path).expect("create file");
        let chunk = vec![0xAAu8; len];
        f.write_all(&chunk).expect("write bytes");
    }

    #[test]
    fn rejects_symlink_or_traversal_outside_shared_dir() {
        let root = make_temp_root("traversal");
        let shared = root.join("plugins").join("_shared");
        let outside = root.join("outside_evil");
        fs::create_dir_all(&shared).unwrap();
        write_bytes(&outside.join("model.onnx"), 1024);

        let manifest_path = root.join("active-mounts.json");
        let outside_str = outside.to_string_lossy().replace('\\', "\\\\");
        let shared_str = shared.to_string_lossy().replace('\\', "\\\\");
        let json = format!(
            r#"{{
                "version": 1,
                "sharedDir": "{shared_str}",
                "strict": true,
                "mounts": {{
                    "embeddinggemma-2-onnx": {{
                        "resourceId": "embeddinggemma-2-onnx",
                        "moduleId": "engine_gemma_unified",
                        "capabilitySlot": "base_embedding",
                        "dir": "{outside_str}",
                        "requiredFileGroups": [["model.onnx"]],
                        "minPrimaryFileBytes": 100
                    }}
                }}
            }}"#
        );
        fs::write(&manifest_path, json).unwrap();

        let state = load_manifest_from_path(&manifest_path, Some(&shared), true);
        assert_eq!(state.status.mode, "manifest_single_track");
        assert!(state.valid_mounts.is_empty());
        assert_eq!(state.status.rejected_mounts.len(), 1);
        assert!(state.status.rejected_mounts[0].reason.contains("越界挂载路径被拒绝"));

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn enforces_bekko_hard_floor_even_if_manifest_attempts_downgrade() {
        let root = make_temp_root("hardfloor");
        let shared = root.join("_shared");
        let bekko = shared.join("bekko-a8m");
        write_bytes(&bekko.join("model.onnx"), 4096); // 仅 4KB，远低于 50MB 硬底线

        let manifest_path = root.join("active-mounts.json");
        let bekko_str = bekko.to_string_lossy().replace('\\', "\\\\");
        let shared_str = shared.to_string_lossy().replace('\\', "\\\\");
        let json = format!(
            r#"{{
                "version": 1,
                "sharedDir": "{shared_str}",
                "strict": true,
                "mounts": {{
                    "bekko-a8m": {{
                        "resourceId": "bekko-a8m",
                        "moduleId": "engine_classic_light",
                        "capabilitySlot": "base_embedding",
                        "dir": "{bekko_str}",
                        "requiredFileGroups": [["model.onnx"]],
                        "minPrimaryFileBytes": 1
                    }}
                }}
            }}"#
        );
        fs::write(&manifest_path, json).unwrap();

        let state = load_manifest_from_path(&manifest_path, Some(&shared), true);
        assert!(state.valid_mounts.is_empty());
        assert_eq!(state.status.rejected_mounts.len(), 1);
        assert!(state.status.rejected_mounts[0].reason.contains("50000000"));

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn accepts_valid_multi_file_mount_and_zero_byte_companion_is_rejected() {
        let root = make_temp_root("companion");
        let shared = root.join("_shared");
        let m5_dir = shared.join("m5-tts");
        let sub = m5_dir.join("models/audio/m5");
        write_bytes(&sub.join("m5_backbone_step_int8.onnx"), 1_050_000);
        write_bytes(&sub.join("c0_head_fp32.onnx"), 1024);
        write_bytes(&sub.join("tokenizer.json"), 0); // 零字节伴随文件必须被拒绝

        let manifest_path = root.join("active-mounts.json");
        let m5_str = m5_dir.to_string_lossy().replace('\\', "\\\\");
        let shared_str = shared.to_string_lossy().replace('\\', "\\\\");
        let json = format!(
            r#"{{
                "version": 1,
                "sharedDir": "{shared_str}",
                "strict": true,
                "mounts": {{
                    "m5-tts": {{
                        "resourceId": "m5-tts",
                        "moduleId": "tts_m5_onnx",
                        "capabilitySlot": "voice_tts",
                        "dir": "{m5_str}",
                        "requiredFileGroups": [[
                            "models/audio/m5/m5_backbone_step_int8.onnx",
                            "models/audio/m5/c0_head_fp32.onnx",
                            "models/audio/m5/tokenizer.json"
                        ]],
                        "minPrimaryFileBytes": 1000000
                    }}
                }}
            }}"#
        );
        fs::write(&manifest_path, json).unwrap();

        let state = load_manifest_from_path(&manifest_path, Some(&shared), true);
        assert!(state.valid_mounts.is_empty());
        assert!(state.status.rejected_mounts[0].reason.contains("0B"));

        // 修复伴随文件为非零后通过
        write_bytes(&sub.join("tokenizer.json"), 512);
        let state_ok = load_manifest_from_path(&manifest_path, Some(&shared), true);
        assert_eq!(state_ok.status.active_resource_ids, vec!["m5-tts"]);
        assert!(state_ok.valid_mounts.contains_key("m5-tts"));

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn shared_dir_mismatch_fails_closed() {
        let root = make_temp_root("mismatch");
        let shared_a = root.join("_shared_a");
        let shared_b = root.join("_shared_b");
        fs::create_dir_all(&shared_a).unwrap();
        fs::create_dir_all(&shared_b).unwrap();
        let manifest_path = root.join("active-mounts.json");
        let shared_a_str = shared_a.to_string_lossy().replace('\\', "\\\\");
        let json = format!(r#"{{"version":1,"sharedDir":"{shared_a_str}","mounts":{{}}}}"#);
        fs::write(&manifest_path, json).unwrap();

        let state = load_manifest_from_path(&manifest_path, Some(&shared_b), true);
        assert_eq!(state.status.mode, "manifest_error");
        assert!(state.status.error.unwrap().contains("不一致"));

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn accepts_current_json_pointing_to_valid_encrypted_container_and_rejects_empty() {
        let root = make_temp_root("current_json");
        let shared = root.join("_shared");
        let bekko = shared.join("bekko-a8m");
        fs::create_dir_all(&bekko).unwrap();

        // 写入一个 55MB 的加密容器文件
        let container_name = "model.test_hash.ffpc";
        write_bytes(&bekko.join(container_name), 55_000_000);

        // 写入指向该容器的 current.json
        let current_json_path = bekko.join("current.json");
        let current_json_content = format!(
            r#"{{"activeFile":"{}","version":"1.0.0"}}"#,
            container_name
        );
        fs::write(&current_json_path, current_json_content).unwrap();

        let manifest_path = root.join("active-mounts.json");
        let bekko_str = bekko.to_string_lossy().replace('\\', "\\\\");
        let shared_str = shared.to_string_lossy().replace('\\', "\\\\");
        let json = format!(
            r#"{{
                "version": 1,
                "sharedDir": "{shared_str}",
                "strict": true,
                "mounts": {{
                    "bekko-a8m": {{
                        "resourceId": "bekko-a8m",
                        "moduleId": "engine_classic_light",
                        "capabilitySlot": "base_embedding",
                        "dir": "{bekko_str}",
                        "requiredFileGroups": [["current.json"]],
                        "minPrimaryFileBytes": 100
                    }}
                }}
            }}"#
        );
        fs::write(&manifest_path, json).unwrap();

        // 验证：通过 current.json 穿透解析到 55MB 容器，满足 50MB 硬底线，挂载成功
        let state_ok = load_manifest_from_path(&manifest_path, Some(&shared), true);
        assert_eq!(state_ok.status.active_resource_ids, vec!["bekko-a8m"]);
        assert!(state_ok.valid_mounts.contains_key("bekko-a8m"));
        assert_eq!(state_ok.valid_mounts["bekko-a8m"].primary_file_bytes, 55_000_000);

        // 篡改测试：将 current.json 指向一个仅 10KB 的伪造容器，必须被 50MB 硬底线精准拒绝
        let fake_container = "model.fake.ffpc";
        write_bytes(&bekko.join(fake_container), 10_000);
        fs::write(
            &current_json_path,
            format!(r#"{{"activeFile":"{}","version":"1.0.0"}}"#, fake_container),
        ).unwrap();

        let state_tampered = load_manifest_from_path(&manifest_path, Some(&shared), true);
        assert!(state_tampered.valid_mounts.is_empty());
        assert_eq!(state_tampered.status.rejected_mounts.len(), 1);
        assert!(state_tampered.status.rejected_mounts[0].reason.contains("50000000"));

        let _ = fs::remove_dir_all(&root);
    }
}
