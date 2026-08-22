use napi_derive::napi;

#[napi]
pub fn extract_file_info(file_path: String) -> String {
    format!("{{\"path\": \"{}\", \"status\": \"ok\"}}", file_path)
}
