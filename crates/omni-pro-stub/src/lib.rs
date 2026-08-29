use omni_core::{DuplicateScanRequest, DuplicateScanResponse, OmniDuplicateGroup};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

pub fn is_pro_enabled() -> bool {
    false
}

pub mod geo {
    use super::*;

    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub struct GeoQueryPoint {
        pub lat: f64,
        pub lng: f64,
    }

    #[derive(Debug, Clone, Serialize, Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub struct ReverseOutcome {
        pub available: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub dataset_version: Option<u32>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub results: Option<Vec<serde_json::Value>>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub reason: Option<String>,
    }

    pub struct GeoService;

    impl GeoService {
        pub fn unavailable() -> Self {
            Self
        }
        pub fn from_path<P: AsRef<std::path::Path>>(_path: P) -> Self {
            Self
        }
        pub fn is_available(&self) -> bool {
            false
        }
        pub fn reverse(
            &self,
            _points: &[GeoQueryPoint],
            _lang: Option<&str>,
            _max_city_km: Option<f64>,
            _max_any_km: Option<f64>,
        ) -> ReverseOutcome {
            ReverseOutcome {
                available: false,
                dataset_version: None,
                results: None,
                reason: Some(String::from("Open-core mode: omni-pro not present")),
            }
        }
    }

    pub fn discover_dataset_path() -> Option<std::path::PathBuf> {
        None
    }
}

pub mod cleanup {
    use super::*;

    pub struct OmniCleanup;

    impl OmniCleanup {
        pub fn init() {}

        pub fn scan(
            _req: &DuplicateScanRequest,
            _stop_flag: &Arc<AtomicBool>,
        ) -> DuplicateScanResponse {
            DuplicateScanResponse {
                success: false,
                total_scanned: 0,
                duplicate_groups: Vec::new(),
                total_redundant_files: 0,
                total_freed_bytes: 0,
                duration_ms: 0,
            }
        }

        pub fn scan_streaming<FG, FP>(
            _req: &DuplicateScanRequest,
            _stop_flag: &Arc<AtomicBool>,
            _on_group: FG,
            _on_progress: FP,
        ) -> DuplicateScanResponse
        where
            FG: Fn(&OmniDuplicateGroup) + Send + Sync + 'static,
            FP: Fn(usize, usize, &str) + Send + Sync + 'static,
        {
            DuplicateScanResponse {
                success: false,
                total_scanned: 0,
                duplicate_groups: Vec::new(),
                total_redundant_files: 0,
                total_freed_bytes: 0,
                duration_ms: 0,
            }
        }

        pub fn execute_fix(
            _action: &str,
            _paths: Vec<String>,
        ) -> (usize, usize, Vec<String>, Vec<String>) {
            (0, 0, Vec::new(), vec!["Open-core mode: omni-cleanup not present".to_string()])
        }
    }

    pub type CzkawkaBridge = OmniCleanup;
}

pub mod cover {
    pub struct CoverRenderer;

    impl CoverRenderer {
        pub fn render_cover<P: AsRef<std::path::Path>>(_file_path: P) -> anyhow::Result<Vec<u8>> {
            anyhow::bail!("Open-core mode: cover extraction requires omni-pro");
        }

        pub fn render_cover_with_options<P: AsRef<std::path::Path>>(_file_path: P, _allow_libreoffice: bool) -> anyhow::Result<Vec<u8>> {
            anyhow::bail!("Open-core mode: cover extraction requires omni-pro");
        }

        pub fn render_pdf_page_png_buffers<P: AsRef<std::path::Path>>(_file_path: P, _max_pages: usize) -> anyhow::Result<Vec<Vec<u8>>> {
            Ok(Vec::new())
        }
    }
}

pub use cover::CoverRenderer;