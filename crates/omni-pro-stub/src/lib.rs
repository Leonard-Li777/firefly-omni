use omni_core::{DuplicateScanRequest, DuplicateScanResponse, OmniDuplicateGroup};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

pub fn is_pro_enabled() -> bool {
    false
}

pub mod geo {
    use super::*;

    #[derive(Debug, Clone, Copy, Serialize, Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub struct GeoQueryPoint {
        pub latitude: f64,
        pub longitude: f64,
    }

    #[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
    #[serde(rename_all = "camelCase")]
    pub struct GeoPlaceResult {
        pub found: bool,
        pub country: Option<String>,
        pub province: Option<String>,
        pub city: Option<String>,
        pub distance_km: Option<f64>,
    }

    #[derive(Debug, Clone, Serialize, Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub struct ReverseOutcome {
        pub available: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub dataset_version: Option<u32>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub results: Option<Vec<GeoPlaceResult>>,
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

        pub fn render_pdf_page_images<P: AsRef<std::path::Path>>(_file_path: P, _max_pages: usize) -> anyhow::Result<Vec<image::DynamicImage>> {
            Ok(Vec::new())
        }

        pub fn render_pdf_page_png_buffers<P: AsRef<std::path::Path>>(_file_path: P, _max_pages: usize) -> anyhow::Result<Vec<Vec<u8>>> {
            Ok(Vec::new())
        }
    }

    pub fn raw_to_webp(_samples: &[u8], _width: u32, _height: u32, _channels: u8) -> anyhow::Result<Vec<u8>> {
        anyhow::bail!("Open-core mode: cover extraction requires omni-pro");
    }
}

pub use cover::{raw_to_webp, CoverRenderer};

pub mod perceive {
    use std::path::Path;

    pub fn detect_watermark_level(_img: &image::DynamicImage) -> u8 {
        0
    }

    pub fn detect_watermark_status(_img: &image::DynamicImage) -> &'static str {
        "none"
    }

    pub fn detect_mosaic_level(_img: &image::DynamicImage) -> u8 {
        0
    }

    pub fn detect_mosaic_status(_img: &image::DynamicImage) -> &'static str {
        "none"
    }

    pub fn detect_exif_orientation_fast<P: AsRef<Path>>(_path: P) -> Option<String> {
        None
    }

    pub fn detect_ntfs_zone_identifier<P: AsRef<Path>>(_path: P) -> (Option<String>, Option<String>, Option<String>) {
        (None, None, None)
    }

    pub fn detect_ntfs_zone_identifier_with_lang<P: AsRef<Path>>(_path: P, _lang: Option<&str>) -> (Option<String>, Option<String>, Option<String>) {
        (None, None, None)
    }

    pub fn detect_workflow_state(_path_str: &str, _metadata: &serde_json::Value) -> String {
        "unarchived".to_string()
    }

    pub fn detect_security_level(_path_str: &str, _content_preview: &str) -> String {
        "public".to_string()
    }

    pub fn evaluate_image_aesthetic_and_quality(_img: &image::DynamicImage, _exif_orientation: Option<&str>) -> (f32, Vec<String>) {
        (7.5, Vec::new())
    }

    pub fn infer_image_modal_type(
        _img: &image::DynamicImage,
        _mobilenet_tags: &[String],
        _clip_tags: &[String],
        _nsfw_tags: &[String],
        _has_text: bool,
        _file_name: &str,
    ) -> String {
        "摄影照片".to_string()
    }
}

pub mod vision {
    use std::path::Path;

    #[derive(Debug, Clone)]
    pub struct OCRBoxResult {
        pub box_rect: [u32; 4],
        pub text: String,
        pub confidence: f32,
    }

    pub struct OmniVisionEngine;

    impl OmniVisionEngine {
        pub fn detect_mime_type<P: AsRef<Path>>(path: P) -> anyhow::Result<String> {
            let p = path.as_ref();
            if let Some(ext) = p.extension().and_then(|e| e.to_str()) {
                let mime = match ext.to_lowercase().as_str() {
                    "png" => "image/png",
                    "jpg" | "jpeg" => "image/jpeg",
                    "gif" => "image/gif",
                    "webp" => "image/webp",
                    "bmp" => "image/bmp",
                    "pdf" => "application/pdf",
                    "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
                    "pptx" => "application/vnd.openxmlformats-officedocument.presentationml.presentation",
                    "xlsx" => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
                    "mp3" => "audio/mpeg",
                    "mp4" => "video/mp4",
                    "json" => "application/json",
                    "txt" | "md" => "text/plain",
                    _ => "application/octet-stream",
                };
                return Ok(mime.to_string());
            }
            Ok("application/octet-stream".to_string())
        }

        pub fn group_boxes_into_lines(_boxes: Vec<OCRBoxResult>) -> String {
            String::new()
        }

        pub fn ctc_decode(_preds: &[f32], _char_list: &[&str], _seq_len: usize, _num_classes: usize) -> (String, f32) {
            (String::new(), 0.0)
        }

        pub fn fast_detect_has_text(_img: &image::DynamicImage) -> bool {
            true
        }

        pub fn recognize_ocr_text_with_size<P: AsRef<Path>>(_image_path: P, _model_size: &str) -> anyhow::Result<String> {
            Ok(String::new())
        }

        pub fn recognize_ocr_text<P: AsRef<Path>>(_image_path: P) -> anyhow::Result<String> {
            Ok(String::new())
        }

        pub fn recognize_ocr_dynamic_image(_img: &image::DynamicImage, _model_size: &str) -> anyhow::Result<String> {
            Ok(String::new())
        }

        pub fn recognize_ocr_image_bytes(_bytes: &[u8], _model_size: &str) -> anyhow::Result<String> {
            Ok(String::new())
        }

        pub fn extract_clip_visual_tags_from_image(
            _img: &image::DynamicImage,
            _lang: Option<&str>,
            _top_k: usize,
        ) -> Vec<String> {
            Vec::new()
        }

        pub fn extract_ram_tags(
            _img: &image::DynamicImage,
            _lang: Option<&str>,
            _top_k: usize,
        ) -> Vec<omni_core::TagChainItem> {
            Vec::new()
        }

        pub fn resolve_tag_to_chain_item(tag: &str, confidence: f32) -> omni_core::TagChainItem {
            omni_core::TagChainItem {
                tag: tag.to_string(),
                confidence,
                dimension_id: 28,
                dimension_name: "内容标签".to_string(),
                logic_pan_dimension: tag.to_string(),
            }
        }

        pub fn extract_clip_visual_tags_from_image_with_hint(
            _img: &image::DynamicImage,
            _path_hint: Option<&str>,
            _lang: Option<&str>,
            _top_k: usize,
        ) -> Vec<String> {
            Vec::new()
        }

        pub fn extract_clip_visual_tags<P: AsRef<Path>>(
            _image_path: P,
            _lang: Option<&str>,
            _top_k: usize,
        ) -> Vec<String> {
            Vec::new()
        }

        pub fn extract_clip_high_confidence_tags(
            _img: &image::DynamicImage,
            _path_hint: Option<&str>,
            _lang: Option<&str>,
        ) -> Vec<String> {
            Vec::new()
        }

        pub fn extract_mobilenet_tags(
            _img: &image::DynamicImage,
            _has_text: bool,
        ) -> Vec<String> {
            Vec::new()
        }

        pub fn extract_mobilenet_high_confidence_tags(
            _img: &image::DynamicImage,
            _has_text: bool,
        ) -> Vec<String> {
            Vec::new()
        }

        pub fn detect_nsfw_tags_and_rating(
            _img: &image::DynamicImage,
            _ocr_text: &str,
            _clip_tags: &[String],
        ) -> (Vec<String>, Vec<String>, Option<String>) {
            (Vec::new(), Vec::new(), None)
        }

        pub fn detect_is_black_and_white(_img: &image::DynamicImage) -> bool {
            false
        }

        pub fn derive_mobilenet_tags(
            _aspect_ratio: f32,
            _has_text: bool,
            _is_bw: bool,
        ) -> (Vec<String>, Vec<String>) {
            (Vec::new(), Vec::new())
        }

        pub fn run_nsfw_model(_img: &image::DynamicImage) -> Option<[f32; 5]> {
            None
        }

        pub fn derive_nsfw_tags_and_rating_from_probs(
            _probs_opt: Option<[f32; 5]>,
            _ocr_text: &str,
            _clip_tags: &[String],
        ) -> (Vec<String>, Vec<String>, Option<String>) {
            (Vec::new(), Vec::new(), None)
        }

        pub fn derive_nsfw_high_confidence_tags(
            _tags: &[String],
            _rating: Option<&str>,
        ) -> Vec<String> {
            Vec::new()
        }
    }
}

pub use vision::{OCRBoxResult, OmniVisionEngine};