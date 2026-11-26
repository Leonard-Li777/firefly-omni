//! 静态只读资源布局、单一启动引导信封与二进制自描述契约。
//!
//! 对齐 `docs/specs/multi-product-resource-placement-and-assembly-spec.md` §3.3 / §3.4：
//! 1. `ResourceLayout` 由 `static_resources_root` 纯函数派生全部只读资产路径，不读写环境变量；
//! 2. `OmniBootstrapConfig`（`schemaVersion = 1`）作为宿主与引擎间唯一引导接缝；
//! 3. `BinaryContract` 由编译期特性图决定，供打包门禁校验 edition 与随包必需文件。

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

/// 进程级注册的权威 ResourceLayout（由 `--bootstrap` 或 `StandaloneCliAdapter` 在启动时注入）
static GLOBAL_RESOURCE_LAYOUT: OnceLock<Mutex<Option<ResourceLayout>>> = OnceLock::new();

/// 注册进程级 `ResourceLayout`（供跨 crate 统一读取，消除环境变量与多级目录探测）
pub fn set_global_resource_layout(layout: Option<ResourceLayout>) {
    let slot = GLOBAL_RESOURCE_LAYOUT.get_or_init(|| Mutex::new(None));
    if let Ok(mut guard) = slot.lock() {
        *guard = layout;
    }
}

/// 读取进程级已注册的 `ResourceLayout`
pub fn global_resource_layout() -> Option<ResourceLayout> {
    GLOBAL_RESOURCE_LAYOUT
        .get()
        .and_then(|m| m.lock().ok())
        .and_then(|g| g.clone())
}

/// 统一媒体工具定位（合并全仓 4 处重复 `resolve_ffmpeg` / `ffprobe` 逻辑）：
/// 1) 优先显式注入的 `&ResourceLayout` 或进程全局 `ResourceLayout`；
/// 2) 仅在未注入布局的孤立单测/CLI 场景下，最小化回退可执行文件相对 `resources/bin/<tool>` 与系统 PATH。
pub fn resolve_media_tool_with_layout(
    tool: &str,
    layout: Option<&ResourceLayout>,
) -> Option<PathBuf> {
    let owned_global = if layout.is_none() {
        global_resource_layout()
    } else {
        None
    };
    let effective = layout.or(owned_global.as_ref());

    if let Some(l) = effective {
        let p = match tool {
            "ffmpeg" => l.ffmpeg_bin(),
            "ffprobe" => l.ffprobe_bin(),
            "exiftool" => l.exiftool_bin(),
            other => {
                let exe = if cfg!(target_os = "windows") {
                    format!("{other}.exe")
                } else {
                    other.to_string()
                };
                l.static_root().join("bin").join(other).join(exe)
            }
        };
        if p.is_file() {
            return Some(p);
        }
    }

    let exe_name = if cfg!(target_os = "windows") {
        format!("{tool}.exe")
    } else {
        tool.to_string()
    };

    if let Ok(exe_path) = std::env::current_exe() {
        if let Some(exe_dir) = exe_path.parent() {
            let candidates = [
                exe_dir.join("resources").join("bin").join(tool).join(&exe_name),
                exe_dir.join("..").join(tool).join(&exe_name),
                exe_dir.join("..").join("..").join("bin").join(tool).join(&exe_name),
            ];
            for cand in candidates {
                if cand.is_file() {
                    return Some(cand);
                }
            }
        }
    }

    // 末位兜底：系统 PATH（严格排在随包 ResourceLayout 之后，杜绝被系统旧版劫持）
    if let Ok(path_env) = std::env::var("PATH") {
        let sep = if cfg!(target_os = "windows") { ';' } else { ':' };
        for dir in path_env.split(sep) {
            if dir.trim().is_empty() {
                continue;
            }
            let cand = Path::new(dir).join(&exe_name);
            if cand.is_file() {
                return Some(cand);
            }
        }
    }
    None
}

pub fn resolve_ffmpeg_with_layout(layout: Option<&ResourceLayout>) -> Option<PathBuf> {
    resolve_media_tool_with_layout("ffmpeg", layout)
}

pub fn resolve_ffprobe_with_layout(layout: Option<&ResourceLayout>) -> Option<PathBuf> {
    resolve_media_tool_with_layout("ffprobe", layout)
}

pub const BOOTSTRAP_SCHEMA_VERSION: u32 = 1;
pub const CONTRACT_SCHEMA_VERSION: u32 = 1;

/// 只读静态资源强类型布局（Accept Dependencies, Don't Create Them）
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResourceLayout {
    static_root: PathBuf,
}

impl ResourceLayout {
    pub fn from_root(static_root: PathBuf) -> Self {
        Self { static_root }
    }

    pub fn static_root(&self) -> &Path {
        &self.static_root
    }

    pub fn semantic_pack_file(&self) -> PathBuf {
        self.static_root.join("semantic").join("semantic.pack")
    }

    pub fn taxonomy_dir(&self) -> PathBuf {
        self.static_root.join("taxonomy")
    }

    pub fn tag_admissibility_file(&self) -> PathBuf {
        self.taxonomy_dir().join("tag-admissibility.json")
    }

    pub fn domain_tag_matrix_file(&self) -> PathBuf {
        self.taxonomy_dir().join("domain_tag_matrix.json")
    }

    pub fn geo_data_file(&self) -> PathBuf {
        self.static_root.join("geo").join("geonames-compact.json")
    }

    pub fn ppocr_dir(&self) -> PathBuf {
        self.static_root.join("models").join("PP-OCRv6")
    }

    pub fn magika_dir(&self) -> PathBuf {
        self.static_root.join("models").join("magika")
    }

    pub fn text_taxonomy_embeddings_file(&self) -> PathBuf {
        self.static_root
            .join("models")
            .join("tags_text_embeddings.bin")
    }

    pub fn native_onnx_lib_dir(&self) -> PathBuf {
        self.static_root.join("bin").join("omni")
    }

    pub fn exiftool_bin(&self) -> PathBuf {
        let exe = if cfg!(target_os = "windows") {
            "exiftool.exe"
        } else {
            "exiftool"
        };
        self.static_root.join("bin").join("exiftool").join(exe)
    }

    pub fn ffmpeg_bin(&self) -> PathBuf {
        let exe = if cfg!(target_os = "windows") {
            "ffmpeg.exe"
        } else {
            "ffmpeg"
        };
        self.static_root.join("bin").join("ffmpeg").join(exe)
    }

    pub fn ffprobe_bin(&self) -> PathBuf {
        let exe = if cfg!(target_os = "windows") {
            "ffprobe.exe"
        } else {
            "ffprobe"
        };
        self.static_root.join("bin").join("ffprobe").join(exe)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct BootstrapStorageConfig {
    pub db_path: Option<PathBuf>,
    pub vectors_dir: Option<PathBuf>,
    pub run_dir: Option<PathBuf>,
    pub user_data_dir: Option<PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct BootstrapRuntimeConfig {
    #[serde(default = "default_embedding_profile")]
    pub embedding_profile: String,
    #[serde(default = "default_mrl_dimension")]
    pub mrl_dimension: usize,
    #[serde(default)]
    pub disable_direct_ml: bool,
    #[serde(default)]
    pub watch_stdin: bool,
    #[serde(default)]
    pub dev_mode: bool,
    #[serde(default)]
    pub eager_tts_warmup: bool,
}

fn default_embedding_profile() -> String {
    "classic_light".to_string()
}

fn default_mrl_dimension() -> usize {
    512
}

impl Default for BootstrapRuntimeConfig {
    fn default() -> Self {
        Self {
            embedding_profile: default_embedding_profile(),
            mrl_dimension: default_mrl_dimension(),
            disable_direct_ml: false,
            watch_stdin: false,
            dev_mode: false,
            eager_tts_warmup: false,
        }
    }
}

/// 宿主与 Omni 子进程之间唯一的启动引导配置信封（Spec §3.3）
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct OmniBootstrapConfig {
    pub schema_version: u32,
    pub listen_addr: String,
    #[serde(default)]
    pub auth_token_file: Option<PathBuf>,
    pub static_resources_root: PathBuf,
    pub storage: BootstrapStorageConfig,
    #[serde(default)]
    pub runtime: BootstrapRuntimeConfig,
    #[serde(default)]
    pub plugin_mounts_file: Option<PathBuf>,
}

impl OmniBootstrapConfig {
    /// 从磁盘加载并严格校验引导配置
    pub fn load_from_file(path: &Path) -> Result<Self> {
        let raw = fs::read_to_string(path)
            .with_context(|| format!("无法读取 Omni 引导配置: {}", path.display()))?;
        let cfg: Self = serde_json::from_str(&raw)
            .with_context(|| format!("解析 Omni 引导配置失败: {}", path.display()))?;
        cfg.validate()?;
        Ok(cfg)
    }

    pub fn validate(&self) -> Result<()> {
        if self.schema_version != BOOTSTRAP_SCHEMA_VERSION {
            bail!(
                "OmniBootstrapConfig schemaVersion 不匹配: 期望 {}, 实际 {}",
                BOOTSTRAP_SCHEMA_VERSION,
                self.schema_version
            );
        }
        if self.listen_addr.trim().is_empty() {
            bail!("OmniBootstrapConfig.listenAddr 不能为空");
        }
        if self.static_resources_root.as_os_str().is_empty() {
            bail!("OmniBootstrapConfig.staticResourcesRoot 不能为空");
        }
        Ok(())
    }

    pub fn resource_layout(&self) -> ResourceLayout {
        ResourceLayout::from_root(self.static_resources_root.clone())
    }
}

/// 二进制编译期自描述契约（`firefly-omni contract --json`）
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct BinaryContract {
    pub contract_schema: u32,
    pub binary: String,
    pub version: String,
    pub git_sha: String,
    pub edition: String,
    pub bootstrap_schema: u32,
    pub required_static_paths: Vec<String>,
    pub required_runtime_libs: BTreeMap<String, Vec<String>>,
    pub forbidden_paths: Vec<String>,
}

impl BinaryContract {
    pub fn for_edition(is_pro: bool, version: &str, git_sha: &str) -> Self {
        let common_forbidden = vec![
            "presetResources".to_string(),
            "visual-vocab".to_string(),
            "libmupdf".to_string(),
            "libclang".to_string(),
            "models/ram".to_string(),
            "models/tagger".to_string(),
            "models/clip/tags_text_zh.bin".to_string(),
            "models/clip/tags_text_zh.txt".to_string(),
        ];

        if is_pro {
            let mut runtime_libs = BTreeMap::new();
            runtime_libs.insert(
                "win32".to_string(),
                vec![
                    "bin/omni/onnxruntime.dll".to_string(),
                    "bin/omni/onnxruntime_dml.dll".to_string(),
                    "bin/omni/DirectML.dll".to_string(),
                    "bin/omni/sherpa-onnx-c-api.dll".to_string(),
                ],
            );
            runtime_libs.insert(
                "darwin".to_string(),
                vec![
                    "bin/omni/libonnxruntime.dylib".to_string(),
                    "bin/omni/libsherpa-onnx-c-api.dylib".to_string(),
                ],
            );
            runtime_libs.insert(
                "linux".to_string(),
                vec![
                    "bin/omni/libonnxruntime.so".to_string(),
                    "bin/omni/libsherpa-onnx-c-api.so".to_string(),
                ],
            );

            Self {
                contract_schema: CONTRACT_SCHEMA_VERSION,
                binary: "firefly-omni".to_string(),
                version: version.to_string(),
                git_sha: git_sha.to_string(),
                edition: "pro".to_string(),
                bootstrap_schema: BOOTSTRAP_SCHEMA_VERSION,
                required_static_paths: vec![
                    "semantic/semantic.pack".to_string(),
                    "taxonomy/tag-admissibility.json".to_string(),
                    "taxonomy/domain_tag_matrix.json".to_string(),
                    "geo/geonames-compact.json".to_string(),
                    "models/PP-OCRv6".to_string(),
                    "models/magika".to_string(),
                    "models/tags_text_embeddings.bin".to_string(),
                    "bin/exiftool".to_string(),
                ],
                required_runtime_libs: runtime_libs,
                forbidden_paths: common_forbidden,
            }
        } else {
            let mut ce_forbidden = common_forbidden;
            ce_forbidden.extend([
                "semantic".to_string(),
                "taxonomy".to_string(),
                "geo".to_string(),
                "models".to_string(),
                "bin/omni/onnxruntime.dll".to_string(),
                "bin/omni/onnxruntime_dml.dll".to_string(),
                "bin/omni/DirectML.dll".to_string(),
                "bin/omni/sherpa-onnx-c-api.dll".to_string(),
            ]);
            Self {
                contract_schema: CONTRACT_SCHEMA_VERSION,
                binary: "firefly-omni".to_string(),
                version: version.to_string(),
                git_sha: git_sha.to_string(),
                edition: "ce".to_string(),
                bootstrap_schema: BOOTSTRAP_SCHEMA_VERSION,
                required_static_paths: vec!["bin/exiftool".to_string()],
                required_runtime_libs: BTreeMap::new(),
                forbidden_paths: ce_forbidden,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn unique_tmp_file(name: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("omni_rl_{nanos}_{name}"))
    }

    #[test]
    fn resource_layout_derives_expected_paths() {
        let root = PathBuf::from("/opt/firefly/resources");
        let layout = ResourceLayout::from_root(root.clone());
        assert_eq!(layout.semantic_pack_file(), root.join("semantic/semantic.pack"));
        assert_eq!(layout.tag_admissibility_file(), root.join("taxonomy/tag-admissibility.json"));
        assert_eq!(layout.domain_tag_matrix_file(), root.join("taxonomy/domain_tag_matrix.json"));
        assert_eq!(layout.magika_dir(), root.join("models/magika"));
        assert_eq!(
            layout.text_taxonomy_embeddings_file(),
            root.join("models/tags_text_embeddings.bin")
        );
        assert_eq!(layout.native_onnx_lib_dir(), root.join("bin/omni"));
    }

    #[test]
    fn binary_contract_separates_pro_and_ce_editions() {
        let pro = BinaryContract::for_edition(true, "0.1.9", "abc123");
        assert_eq!(pro.edition, "pro");
        assert!(pro.required_static_paths.contains(&"semantic/semantic.pack".to_string()));
        assert!(pro.required_runtime_libs.contains_key("win32"));
        assert!(pro.forbidden_paths.contains(&"models/tagger".to_string()));

        let ce = BinaryContract::for_edition(false, "0.1.9", "abc123");
        assert_eq!(ce.edition, "ce");
        assert_eq!(ce.required_static_paths, vec!["bin/exiftool".to_string()]);
        assert!(ce.required_runtime_libs.is_empty());
        assert!(ce.forbidden_paths.contains(&"semantic".to_string()));
    }

    #[test]
    fn bootstrap_config_validates_schema_and_loads_json() {
        let file = unique_tmp_file("bootstrap.json");
        let json = serde_json::json!({
            "schemaVersion": 1,
            "listenAddr": "127.0.0.1:38200",
            "staticResourcesRoot": "C:/app/resources",
            "storage": {
                "dbPath": "C:/data/firefly-ai-folder.db",
                "vectorsDir": "C:/data/vectors"
            },
            "runtime": {
                "embeddingProfile": "gemma_unified",
                "mrlDimension": 512,
                "disableDirectMl": true,
                "watchStdin": true
            }
        });
        fs::write(&file, serde_json::to_string(&json).unwrap()).unwrap();
        let cfg = OmniBootstrapConfig::load_from_file(&file).unwrap();
        assert_eq!(cfg.runtime.embedding_profile, "gemma_unified");
        assert!(cfg.runtime.disable_direct_ml);
        assert_eq!(cfg.resource_layout().static_root(), Path::new("C:/app/resources"));

        let bad_json = serde_json::json!({
            "schemaVersion": 99,
            "listenAddr": "127.0.0.1:38200",
            "staticResourcesRoot": "C:/app/resources",
            "storage": {}
        });
        fs::write(&file, serde_json::to_string(&bad_json).unwrap()).unwrap();
        assert!(OmniBootstrapConfig::load_from_file(&file).is_err());
        let _ = fs::remove_file(&file);
    }
}
