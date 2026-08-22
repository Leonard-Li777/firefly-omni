#![deny(clippy::all)]

use napi_derive::napi;
use omni_core::OmniExtractionResult;
use omni_vision::OmniVisionEngine;

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
