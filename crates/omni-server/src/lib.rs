use axum::{
    extract::{DefaultBodyLimit, Multipart, State},
    response::sse::{Event, KeepAlive, Sse},
    routing::{get, post},
    Json, Router,
};
use omni_core::{
    DuplicateScanRequest, DuplicateScanResponse, OmniConfig, OmniDuplicateFileItem,
    OmniDuplicateGroup, OmniExtractionResult,
};
use omni_extract::OmniExtractor;
use serde::Deserialize;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Instant;
use tracing::info;

#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Mutex<OmniConfig>>,
}

#[derive(Deserialize)]
pub struct ExtractRequest {
    pub file_path: String,
}

pub fn create_app_router(state: AppState) -> Router {
    Router::new()
        .route(
            "/health",
            get(|| async {
                axum::Json(serde_json::json!({ "status": "ok", "server": "firefly-omni" }))
            }),
        )
        .route("/api/config", get(get_config).post(update_config))
        .route("/api/extract", post(extract_file_handler))
        .route("/api/extract/upload", post(extract_multipart_handler))
        .route("/api/duplicate/scan", post(duplicate_scan_handler))
        .route("/api/duplicate/scan/stream", post(duplicate_scan_stream_handler))
        .layer(DefaultBodyLimit::max(500 * 1024 * 1024))
        .with_state(state)
}

fn get_config_file_path() -> PathBuf {
    if let Ok(appdata) = std::env::var("APPDATA") {
        let dir = PathBuf::from(appdata).join("firefly-ai-folder");
        let _ = std::fs::create_dir_all(&dir);
        dir.join("omni_config.json")
    } else {
        std::env::temp_dir().join("omni_config.json")
    }
}

fn load_config_from_disk() -> OmniConfig {
    let path = get_config_file_path();
    if path.exists() {
        if let Ok(content) = std::fs::read_to_string(&path) {
            if let Ok(cfg) = serde_json::from_str::<OmniConfig>(&content) {
                return cfg;
            }
        }
    }
    OmniConfig::default()
}

fn save_config_to_disk(cfg: &OmniConfig) {
    let path = get_config_file_path();
    if let Ok(json) = serde_json::to_string_pretty(cfg) {
        let _ = std::fs::write(path, json);
    }
}

pub async fn start_server(addr: SocketAddr) -> anyhow::Result<()> {
    let initial_config = load_config_from_disk();
    let state = AppState {
        config: Arc::new(Mutex::new(initial_config)),
    };
    let app = create_app_router(state);

    info!("firefly-omni Axum HTTP server starting on {}", addr);
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

async fn get_config(
    State(state): State<AppState>,
) -> Json<OmniConfig> {
    let cfg = state.config.lock().unwrap().clone();
    Json(cfg)
}

async fn update_config(
    State(state): State<AppState>,
    Json(new_config): Json<OmniConfig>,
) -> Json<OmniConfig> {
    let mut cfg = state.config.lock().unwrap();
    *cfg = new_config.clone();
    save_config_to_disk(&new_config);
    Json(new_config)
}

/// 处理本地 JSON 文件路径提取请求: POST /api/extract { "file_path": "/path/to/file" }
async fn extract_file_handler(
    State(state): State<AppState>,
    Json(req): Json<ExtractRequest>,
) -> Json<OmniExtractionResult> {
    let cfg = state.config.lock().unwrap().clone();
    match OmniExtractor::extract(&req.file_path, &cfg).await {
        Ok(res) => Json(res),
        Err(err) => Json(OmniExtractionResult {
            file_path: req.file_path,
            mime_type: "application/octet-stream".to_string(),
            file_size: 0,
            markdown_content: format!("Error: Extraction failed - {}", err),
            metadata: serde_json::json!({}),
            phash: None,
            is_corrupted: true,
        }),
    }
}

/// 处理 Web UI 前端拖拽文件二进制流上传请求: POST /api/extract/upload
async fn extract_multipart_handler(
    State(state): State<AppState>,
    mut multipart: Multipart,
) -> Json<OmniExtractionResult> {
    let cfg = state.config.lock().unwrap().clone();

    loop {
        match multipart.next_field().await {
            Ok(Some(field)) => {
                let file_name = field.file_name().unwrap_or("omni_upload.tmp").to_string();
                if let Ok(bytes) = field.bytes().await {
                    let temp_dir = std::env::temp_dir();
                    let temp_path = temp_dir.join(&file_name);
                    if std::fs::write(&temp_path, &bytes).is_ok() {
                        let path_str = temp_path.to_string_lossy().to_string();
                        if let Ok(mut res) = OmniExtractor::extract(&path_str, &cfg).await {
                            res.file_path = file_name;
                            let _ = std::fs::remove_file(&temp_path);
                            return Json(res);
                        }
                        let _ = std::fs::remove_file(&temp_path);
                    }
                }
            }
            Ok(None) => break,
            Err(err) => {
                tracing::error!("Axum Multipart parsing failed: {:?}", err);
                break;
            }
        }
    }

    Json(OmniExtractionResult {
        file_path: "unknown".to_string(),
        mime_type: "application/octet-stream".to_string(),
        file_size: 0,
        markdown_content: "Error: Multipart file upload extraction failed".to_string(),
        metadata: serde_json::json!({}),
        phash: None,
        is_corrupted: true,
    })
}

/// 处理多模态查重扫描请求: POST /api/duplicate/scan
async fn duplicate_scan_handler(
    State(_state): State<AppState>,
    Json(req): Json<DuplicateScanRequest>,
) -> Json<DuplicateScanResponse> {
    let start = Instant::now();
    let mut files_to_scan: Vec<PathBuf> = Vec::new();

    for p in &req.paths {
        let path = Path::new(p);
        if path.is_file() {
            files_to_scan.push(path.to_path_buf());
        } else if path.is_dir() {
            collect_files_recursive(path, &mut files_to_scan);
        }
    }

    let total_scanned = files_to_scan.len();
    let mut duplicate_groups: Vec<OmniDuplicateGroup> = Vec::new();

    let enabled_strategies = req.strategies.clone().unwrap_or_default();
    let run_exact = enabled_strategies.is_empty() || enabled_strategies.iter().any(|s| s == "exact_hash");
    let run_image = enabled_strategies.is_empty() || enabled_strategies.iter().any(|s| s == "image_phash");
    let run_audio = enabled_strategies.is_empty() || enabled_strategies.iter().any(|s| s == "audio_hash");
    let run_video = req.check_video == Some(true) || enabled_strategies.iter().any(|s| s == "video_phash");

    // 1. 100% 精确去重 (按文件大小过滤 -> 采样内容哈希)
    if run_exact {
        let mut size_map: HashMap<u64, Vec<PathBuf>> = HashMap::new();
        for f in &files_to_scan {
            if let Ok(meta) = std::fs::metadata(f) {
                if meta.is_file() && meta.len() > 0 {
                    size_map.entry(meta.len()).or_default().push(f.clone());
                }
            }
        }

        let mut exact_group_idx = 1;
        for (size, paths) in size_map {
            if paths.len() < 2 {
                continue;
            }
            let mut hash_map: HashMap<String, Vec<PathBuf>> = HashMap::new();
            for p in paths {
                if let Ok(bytes) = std::fs::read(&p) {
                    let mut hash: u64 = 0;
                    for b in &bytes {
                        hash = hash.wrapping_mul(31).wrapping_add(*b as u64);
                    }
                    let hash_str = format!("{:016x}", hash);
                    hash_map.entry(hash_str).or_default().push(p);
                }
            }
            for (hash, dup_paths) in hash_map {
                if dup_paths.len() >= 2 {
                    let items: Vec<OmniDuplicateFileItem> = dup_paths
                        .iter()
                        .map(|p| {
                            let name = p.file_name().unwrap_or_default().to_string_lossy().to_string();
                            let meta = std::fs::metadata(p).ok();
                            let modified_at = meta
                                .and_then(|m| m.modified().ok())
                                .map(|t| format!("{:?}", t))
                                .unwrap_or_default();
                            OmniDuplicateFileItem {
                                path: p.to_string_lossy().to_string(),
                                name,
                                size,
                                modified_at,
                                fingerprint: hash.clone(),
                                similarity_score: Some(1.0),
                            }
                        })
                        .collect();
                    let potential_freed = size * (dup_paths.len() as u64 - 1);
                    duplicate_groups.push(OmniDuplicateGroup {
                        group_id: format!("exact_{}", exact_group_idx),
                        strategy: "exact_hash".to_string(),
                        similarity_percentage: 100.0,
                        description: format!("100% 完全精确一致文件 ({}个)", dup_paths.len()),
                        files: items,
                        potential_freed_bytes: potential_freed,
                    });
                    exact_group_idx += 1;
                }
            }
        }
    }

    // 2. 相似图片去重 (通过感知哈希 pHash 聚类)
    if run_image {
        let image_extensions = ["jpg", "jpeg", "png", "webp", "bmp", "avif", "gif"];
        let mut image_files: Vec<(PathBuf, String, u64)> = Vec::new();
        for f in &files_to_scan {
            if let Some(ext) = f.extension().and_then(|e| e.to_str()) {
                if image_extensions.contains(&ext.to_lowercase().as_str()) {
                    if let Some(phash) = OmniExtractionResult::compute_phash(f) {
                        let size = std::fs::metadata(f).map(|m| m.len()).unwrap_or(0);
                        image_files.push((f.clone(), phash, size));
                    }
                }
            }
        }

        let mut img_group_idx = 1;
        let mut visited_images = vec![false; image_files.len()];
        for i in 0..image_files.len() {
            if visited_images[i] {
                continue;
            }
            let mut group: Vec<(PathBuf, String, u64)> = vec![image_files[i].clone()];
            for j in (i + 1)..image_files.len() {
                if visited_images[j] {
                    continue;
                }
                if image_files[i].1 == image_files[j].1 {
                    group.push(image_files[j].clone());
                    visited_images[j] = true;
                }
            }
            if group.len() >= 2 {
                visited_images[i] = true;
                let items: Vec<OmniDuplicateFileItem> = group
                    .iter()
                    .map(|(p, phash, sz)| {
                        let name = p.file_name().unwrap_or_default().to_string_lossy().to_string();
                        OmniDuplicateFileItem {
                            path: p.to_string_lossy().to_string(),
                            name,
                            size: *sz,
                            modified_at: String::new(),
                            fingerprint: phash.clone(),
                            similarity_score: Some(0.95),
                        }
                    })
                    .collect();
                let avg_size = group.iter().map(|(_, _, s)| s).sum::<u64>() / group.len() as u64;
                let potential_freed = avg_size * (group.len() as u64 - 1);
                duplicate_groups.push(OmniDuplicateGroup {
                    group_id: format!("img_{}", img_group_idx),
                    strategy: "image_phash".to_string(),
                    similarity_percentage: 95.0,
                    description: format!("视觉感知高度相似图片 ({}个)", group.len()),
                    files: items,
                    potential_freed_bytes: potential_freed,
                });
                img_group_idx += 1;
            }
        }
    }

    // 3. 音频同源去重 (audio_hash)
    if run_audio {
        let audio_extensions = ["mp3", "wav", "flac", "aac", "m4a", "ogg", "wma"];
        let mut audio_files: Vec<(PathBuf, String, u64)> = Vec::new();
        for f in &files_to_scan {
            if let Some(ext) = f.extension().and_then(|e| e.to_str()) {
                if audio_extensions.contains(&ext.to_lowercase().as_str()) {
                    if let Some(phash) = OmniExtractionResult::compute_phash(f) {
                        let size = std::fs::metadata(f).map(|m| m.len()).unwrap_or(0);
                        audio_files.push((f.clone(), phash, size));
                    }
                }
            }
        }

        let mut audio_group_idx = 1;
        let mut visited_audio = vec![false; audio_files.len()];
        for i in 0..audio_files.len() {
            if visited_audio[i] {
                continue;
            }
            let mut group: Vec<(PathBuf, String, u64)> = vec![audio_files[i].clone()];
            for j in (i + 1)..audio_files.len() {
                if visited_audio[j] {
                    continue;
                }
                if audio_files[i].1 == audio_files[j].1 {
                    group.push(audio_files[j].clone());
                    visited_audio[j] = true;
                }
            }
            if group.len() >= 2 {
                visited_audio[i] = true;
                let items: Vec<OmniDuplicateFileItem> = group
                    .iter()
                    .map(|(p, phash, sz)| {
                        let name = p.file_name().unwrap_or_default().to_string_lossy().to_string();
                        OmniDuplicateFileItem {
                            path: p.to_string_lossy().to_string(),
                            name,
                            size: *sz,
                            modified_at: String::new(),
                            fingerprint: phash.clone(),
                            similarity_score: Some(0.95),
                        }
                    })
                    .collect();
                let avg_size = group.iter().map(|(_, _, s)| s).sum::<u64>() / group.len() as u64;
                let potential_freed = avg_size * (group.len() as u64 - 1);
                duplicate_groups.push(OmniDuplicateGroup {
                    group_id: format!("audio_{}", audio_group_idx),
                    strategy: "audio_hash".to_string(),
                    similarity_percentage: 95.0,
                    description: format!("同源/高度相似音频文件 ({}个)", group.len()),
                    files: items,
                    potential_freed_bytes: potential_freed,
                });
                audio_group_idx += 1;
            }
        }
    }

    // 4. 视频画面去重 (video_phash, 用户需主动勾选触发)
    if run_video {
        let video_extensions = ["mp4", "mkv", "avi", "mov", "wmv", "flv", "webm", "m4v"];
        let mut video_files: Vec<(PathBuf, String, u64)> = Vec::new();
        for f in &files_to_scan {
            if let Some(ext) = f.extension().and_then(|e| e.to_str()) {
                if video_extensions.contains(&ext.to_lowercase().as_str()) {
                    if let Some(phash) = OmniExtractionResult::compute_phash(f) {
                        let size = std::fs::metadata(f).map(|m| m.len()).unwrap_or(0);
                        video_files.push((f.clone(), phash, size));
                    }
                }
            }
        }

        let mut video_group_idx = 1;
        let mut visited_video = vec![false; video_files.len()];
        for i in 0..video_files.len() {
            if visited_video[i] {
                continue;
            }
            let mut group: Vec<(PathBuf, String, u64)> = vec![video_files[i].clone()];
            for j in (i + 1)..video_files.len() {
                if visited_video[j] {
                    continue;
                }
                if video_files[i].1 == video_files[j].1 {
                    group.push(video_files[j].clone());
                    visited_video[j] = true;
                }
            }
            if group.len() >= 2 {
                visited_video[i] = true;
                let items: Vec<OmniDuplicateFileItem> = group
                    .iter()
                    .map(|(p, phash, sz)| {
                        let name = p.file_name().unwrap_or_default().to_string_lossy().to_string();
                        OmniDuplicateFileItem {
                            path: p.to_string_lossy().to_string(),
                            name,
                            size: *sz,
                            modified_at: String::new(),
                            fingerprint: phash.clone(),
                            similarity_score: Some(0.90),
                        }
                    })
                    .collect();
                let avg_size = group.iter().map(|(_, _, s)| s).sum::<u64>() / group.len() as u64;
                let potential_freed = avg_size * (group.len() as u64 - 1);
                duplicate_groups.push(OmniDuplicateGroup {
                    group_id: format!("video_{}", video_group_idx),
                    strategy: "video_phash".to_string(),
                    similarity_percentage: 90.0,
                    description: format!("同源/画面相似视频文件 ({}个)", group.len()),
                    files: items,
                    potential_freed_bytes: potential_freed,
                });
                video_group_idx += 1;
            }
        }
    }

    let total_redundant_files = duplicate_groups
        .iter()
        .map(|g| if g.files.len() > 1 { g.files.len() - 1 } else { 0 })
        .sum();
    let total_freed_bytes = duplicate_groups.iter().map(|g| g.potential_freed_bytes).sum();

    Json(DuplicateScanResponse {
        success: true,
        total_scanned,
        duplicate_groups,
        total_redundant_files,
        total_freed_bytes,
        duration_ms: start.elapsed().as_millis() as u64,
    })
}

fn collect_files_recursive(dir: &Path, files: &mut Vec<PathBuf>) {
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_dir() {
                collect_files_recursive(&p, files);
            } else if p.is_file() {
                files.push(p);
            }
        }
    }
}

/// 实时 SSE 流式查重扫描接口: POST /api/duplicate/scan/stream
pub async fn duplicate_scan_stream_handler(
    State(_state): State<AppState>,
    Json(req): Json<DuplicateScanRequest>,
) -> Sse<impl tokio_stream::Stream<Item = Result<Event, std::convert::Infallible>>> {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<Result<Event, std::convert::Infallible>>();

    tokio::spawn(async move {
        let start = Instant::now();
        let mut files_to_scan: Vec<PathBuf> = Vec::new();

        for p in &req.paths {
            let path = Path::new(p);
            if path.is_file() {
                files_to_scan.push(path.to_path_buf());
            } else if path.is_dir() {
                collect_files_recursive(path, &mut files_to_scan);
            }
        }

        let total_scanned = files_to_scan.len();

        let _ = tx.send(Ok(Event::default()
            .event("start")
            .data(serde_json::json!({ "total_scanned": total_scanned }).to_string())));

        let enabled_strategies = req.strategies.clone().unwrap_or_default();
        let run_exact = enabled_strategies.is_empty() || enabled_strategies.iter().any(|s| s == "exact_hash");
        let run_image = enabled_strategies.is_empty() || enabled_strategies.iter().any(|s| s == "image_phash");
        let run_audio = enabled_strategies.is_empty() || enabled_strategies.iter().any(|s| s == "audio_hash");
        let run_video = req.check_video == Some(true) || enabled_strategies.iter().any(|s| s == "video_phash");

        let mut total_freed_bytes: u64 = 0;
        let mut total_redundant_files: usize = 0;

        // 1. 100% 精确去重 (实时上屏)
        if run_exact {
            let mut size_map: HashMap<u64, Vec<PathBuf>> = HashMap::new();
            for (idx, f) in files_to_scan.iter().enumerate() {
                if idx % 50 == 0 {
                    let _ = tx.send(Ok(Event::default()
                        .event("progress")
                        .data(serde_json::json!({
                            "scanned": idx + 1,
                            "total": total_scanned
                        }).to_string())));
                }
                if let Ok(meta) = std::fs::metadata(f) {
                    if meta.is_file() && meta.len() > 0 {
                        size_map.entry(meta.len()).or_default().push(f.clone());
                    }
                }
            }

            let mut exact_group_idx = 1;
            for (size, paths) in size_map {
                if paths.len() < 2 {
                    continue;
                }
                let mut hash_map: HashMap<String, Vec<PathBuf>> = HashMap::new();
                for p in paths {
                    if let Ok(bytes) = std::fs::read(&p) {
                        let mut hash: u64 = 0;
                        for b in &bytes {
                            hash = hash.wrapping_mul(31).wrapping_add(*b as u64);
                        }
                        let hash_str = format!("{:016x}", hash);
                        hash_map.entry(hash_str).or_default().push(p);
                    }
                }
                for (hash, dup_paths) in hash_map {
                    if dup_paths.len() >= 2 {
                        let items: Vec<OmniDuplicateFileItem> = dup_paths
                            .iter()
                            .map(|p| {
                                let name = p.file_name().unwrap_or_default().to_string_lossy().to_string();
                                let meta = std::fs::metadata(p).ok();
                                let modified_at = meta
                                    .and_then(|m| m.modified().ok())
                                    .map(|t| format!("{:?}", t))
                                    .unwrap_or_default();
                                OmniDuplicateFileItem {
                                    path: p.to_string_lossy().to_string(),
                                    name,
                                    size,
                                    modified_at,
                                    fingerprint: hash.clone(),
                                    similarity_score: Some(1.0),
                                }
                            })
                            .collect();
                        let potential_freed = size * (dup_paths.len() as u64 - 1);
                        total_freed_bytes += potential_freed;
                        total_redundant_files += dup_paths.len() - 1;

                        let group = OmniDuplicateGroup {
                            group_id: format!("exact_{}", exact_group_idx),
                            strategy: "exact_hash".to_string(),
                            similarity_percentage: 100.0,
                            description: format!("100% 完全精确一致文件 ({}个)", dup_paths.len()),
                            files: items,
                            potential_freed_bytes: potential_freed,
                        };

                        let _ = tx.send(Ok(Event::default()
                            .event("group")
                            .data(serde_json::to_string(&group).unwrap())));

                        exact_group_idx += 1;
                    }
                }
            }
        }

        // 2. 相似图片去重 (实时上屏)
        if run_image {
            let image_extensions = ["jpg", "jpeg", "png", "webp", "bmp", "avif", "gif"];
            let mut image_files: Vec<(PathBuf, String, u64)> = Vec::new();
            for f in &files_to_scan {
                if let Some(ext) = f.extension().and_then(|e| e.to_str()) {
                    if image_extensions.contains(&ext.to_lowercase().as_str()) {
                        if let Some(phash) = OmniExtractionResult::compute_phash(f) {
                            let size = std::fs::metadata(f).map(|m| m.len()).unwrap_or(0);
                            image_files.push((f.clone(), phash, size));
                        }
                    }
                }
            }

            let mut img_group_idx = 1;
            let mut visited_images = vec![false; image_files.len()];
            for i in 0..image_files.len() {
                if visited_images[i] {
                    continue;
                }
                let mut group: Vec<(PathBuf, String, u64)> = vec![image_files[i].clone()];
                for j in (i + 1)..image_files.len() {
                    if visited_images[j] {
                        continue;
                    }
                    if image_files[i].1 == image_files[j].1 {
                        group.push(image_files[j].clone());
                        visited_images[j] = true;
                    }
                }
                if group.len() >= 2 {
                    visited_images[i] = true;
                    let items: Vec<OmniDuplicateFileItem> = group
                        .iter()
                        .map(|(p, phash, sz)| {
                            let name = p.file_name().unwrap_or_default().to_string_lossy().to_string();
                            OmniDuplicateFileItem {
                                path: p.to_string_lossy().to_string(),
                                name,
                                size: *sz,
                                modified_at: String::new(),
                                fingerprint: phash.clone(),
                                similarity_score: Some(0.95),
                            }
                        })
                        .collect();
                    let avg_size = group.iter().map(|(_, _, s)| s).sum::<u64>() / group.len() as u64;
                    let potential_freed = avg_size * (group.len() as u64 - 1);
                    total_freed_bytes += potential_freed;
                    total_redundant_files += group.len() - 1;

                    let dup_group = OmniDuplicateGroup {
                        group_id: format!("img_{}", img_group_idx),
                        strategy: "image_phash".to_string(),
                        similarity_percentage: 95.0,
                        description: format!("视觉感知高度相似图片 ({}个)", group.len()),
                        files: items,
                        potential_freed_bytes: potential_freed,
                    };

                    let _ = tx.send(Ok(Event::default()
                        .event("group")
                        .data(serde_json::to_string(&dup_group).unwrap())));

                    img_group_idx += 1;
                }
            }
        }

        // 3. 音频去重 (实时上屏)
        if run_audio {
            let audio_extensions = ["mp3", "wav", "flac", "aac", "m4a", "ogg", "wma"];
            let mut audio_files: Vec<(PathBuf, String, u64)> = Vec::new();
            for f in &files_to_scan {
                if let Some(ext) = f.extension().and_then(|e| e.to_str()) {
                    if audio_extensions.contains(&ext.to_lowercase().as_str()) {
                        if let Some(phash) = OmniExtractionResult::compute_phash(f) {
                            let size = std::fs::metadata(f).map(|m| m.len()).unwrap_or(0);
                            audio_files.push((f.clone(), phash, size));
                        }
                    }
                }
            }

            let mut audio_group_idx = 1;
            let mut visited_audio = vec![false; audio_files.len()];
            for i in 0..audio_files.len() {
                if visited_audio[i] {
                    continue;
                }
                let mut group: Vec<(PathBuf, String, u64)> = vec![audio_files[i].clone()];
                for j in (i + 1)..audio_files.len() {
                    if visited_audio[j] {
                        continue;
                    }
                    if audio_files[i].1 == audio_files[j].1 {
                        group.push(audio_files[j].clone());
                        visited_audio[j] = true;
                    }
                }
                if group.len() >= 2 {
                    visited_audio[i] = true;
                    let items: Vec<OmniDuplicateFileItem> = group
                        .iter()
                        .map(|(p, phash, sz)| {
                            let name = p.file_name().unwrap_or_default().to_string_lossy().to_string();
                            OmniDuplicateFileItem {
                                path: p.to_string_lossy().to_string(),
                                name,
                                size: *sz,
                                modified_at: String::new(),
                                fingerprint: phash.clone(),
                                similarity_score: Some(0.95),
                            }
                        })
                        .collect();
                    let avg_size = group.iter().map(|(_, _, s)| s).sum::<u64>() / group.len() as u64;
                    let potential_freed = avg_size * (group.len() as u64 - 1);
                    total_freed_bytes += potential_freed;
                    total_redundant_files += group.len() - 1;

                    let dup_group = OmniDuplicateGroup {
                        group_id: format!("audio_{}", audio_group_idx),
                        strategy: "audio_hash".to_string(),
                        similarity_percentage: 95.0,
                        description: format!("同源/高度相似音频文件 ({}个)", group.len()),
                        files: items,
                        potential_freed_bytes: potential_freed,
                    };

                    let _ = tx.send(Ok(Event::default()
                        .event("group")
                        .data(serde_json::to_string(&dup_group).unwrap())));

                    audio_group_idx += 1;
                }
            }
        }

        // 4. 视频画面去重 (实时上屏)
        if run_video {
            let video_extensions = ["mp4", "mkv", "avi", "mov", "wmv", "flv", "webm", "m4v"];
            let mut video_files: Vec<(PathBuf, String, u64)> = Vec::new();
            for f in &files_to_scan {
                if let Some(ext) = f.extension().and_then(|e| e.to_str()) {
                    if video_extensions.contains(&ext.to_lowercase().as_str()) {
                        if let Some(phash) = OmniExtractionResult::compute_phash(f) {
                            let size = std::fs::metadata(f).map(|m| m.len()).unwrap_or(0);
                            video_files.push((f.clone(), phash, size));
                        }
                    }
                }
            }

            let mut video_group_idx = 1;
            let mut visited_video = vec![false; video_files.len()];
            for i in 0..video_files.len() {
                if visited_video[i] {
                    continue;
                }
                let mut group: Vec<(PathBuf, String, u64)> = vec![video_files[i].clone()];
                for j in (i + 1)..video_files.len() {
                    if visited_video[j] {
                        continue;
                    }
                    if video_files[i].1 == video_files[j].1 {
                        group.push(video_files[j].clone());
                        visited_video[j] = true;
                    }
                }
                if group.len() >= 2 {
                    visited_video[i] = true;
                    let items: Vec<OmniDuplicateFileItem> = group
                        .iter()
                        .map(|(p, phash, sz)| {
                            let name = p.file_name().unwrap_or_default().to_string_lossy().to_string();
                            OmniDuplicateFileItem {
                                path: p.to_string_lossy().to_string(),
                                name,
                                size: *sz,
                                modified_at: String::new(),
                                fingerprint: phash.clone(),
                                similarity_score: Some(0.90),
                            }
                        })
                        .collect();
                    let avg_size = group.iter().map(|(_, _, s)| s).sum::<u64>() / group.len() as u64;
                    let potential_freed = avg_size * (group.len() as u64 - 1);
                    total_freed_bytes += potential_freed;
                    total_redundant_files += group.len() - 1;

                    let dup_group = OmniDuplicateGroup {
                        group_id: format!("video_{}", video_group_idx),
                        strategy: "video_phash".to_string(),
                        similarity_percentage: 90.0,
                        description: format!("同源/画面相似视频文件 ({}个)", group.len()),
                        files: items,
                        potential_freed_bytes: potential_freed,
                    };

                    let _ = tx.send(Ok(Event::default()
                        .event("group")
                        .data(serde_json::to_string(&dup_group).unwrap())));

                    video_group_idx += 1;
                }
            }
        }

        let _ = tx.send(Ok(Event::default()
            .event("done")
            .data(serde_json::json!({
                "total_scanned": total_scanned,
                "total_redundant_files": total_redundant_files,
                "total_freed_bytes": total_freed_bytes,
                "duration_ms": start.elapsed().as_millis() as u64
            }).to_string())));
    });

    let stream = tokio_stream::wrappers::UnboundedReceiverStream::new(rx);
    Sse::new(stream).keep_alive(KeepAlive::default())
}
