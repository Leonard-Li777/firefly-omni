#![deny(clippy::all)]

use napi_derive::napi;
use omni_core::{OmniConfig, OmniExtractionResult};
use omni_extract::OmniExtractor;
use omni_vision::OmniVisionEngine;
use std::collections::HashMap;

#[napi]
pub fn compute_phash_native(path_str: String) -> Option<String> {
    OmniExtractionResult::compute_phash(&path_str)
}

#[napi]
pub fn check_corrupted_native(path_str: String) -> bool {
    OmniExtractionResult::check_corrupted(&path_str)
}

#[napi]
pub fn detect_mime_native(path_str: String) -> Result<String, napi::Error> {
    let mime = OmniVisionEngine::detect_mime_type(&path_str)
        .map_err(|e| napi::Error::from_reason(format!("MIME detection failed: {}", e)))?;
    Ok(mime)
}

#[napi]
pub async fn extract_file_info_async(
    path_str: String,
    config_json: Option<String>,
) -> Result<String, napi::Error> {
    let config: OmniConfig = if let Some(json_str) = config_json {
        serde_json::from_str(&json_str)
            .map_err(|e| napi::Error::from_reason(format!("Invalid config JSON: {}", e)))?
    } else {
        OmniConfig::default()
    };

    let result = OmniExtractor::extract(&path_str, &config)
        .await
        .map_err(|e| napi::Error::from_reason(format!("Extraction failed: {}", e)))?;

    let json_output = serde_json::to_string(&result)
        .map_err(|e| napi::Error::from_reason(format!("Failed to serialize result: {}", e)))?;

    Ok(json_output)
}

#[napi]
pub fn replace_embedded_image_ocr_native(markdown: String, ocr_map_json: String) -> Result<String, napi::Error> {
    let map: HashMap<String, String> = serde_json::from_str(&ocr_map_json)
        .map_err(|e| napi::Error::from_reason(format!("Invalid OCR map JSON: {}", e)))?;

    Ok(OmniExtractor::replace_embedded_image_ocr(&markdown, &map))
}

