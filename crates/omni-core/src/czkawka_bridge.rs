use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Instant;

use czkawka_core::common::model::{CheckingMethod, HashType};
use czkawka_core::common::progress_data::ProgressData;
use czkawka_core::common::tool_data::CommonData;
use czkawka_core::common::traits::Search;
use czkawka_core::re_exported::{FilterType, HashAlg};
use czkawka_core::tools::bad_extensions::{BadExtensions, BadExtensionsParameters};
use czkawka_core::tools::bad_names::{BadNames, BadNamesParameters, NameIssues};
use czkawka_core::tools::big_file::{BigFile, BigFileParameters, SearchMode as BigFileSearchMode};
use czkawka_core::tools::broken_files::{BrokenFiles, BrokenFilesParameters, CheckedTypes};
use czkawka_core::tools::duplicate::{DuplicateFinder, DuplicateFinderParameters};
use czkawka_core::tools::empty_files::{EmptyFiles, EmptyFilesParameters};
use czkawka_core::tools::empty_folder::EmptyFolder;
use czkawka_core::tools::exif_remover::{ExifRemover, ExifRemoverParameters};
use czkawka_core::tools::invalid_symlinks::InvalidSymlinks;
use czkawka_core::tools::same_music::{MusicSimilarity, SameMusic, SameMusicParameters};
use czkawka_core::tools::similar_images::{GeometricInvariance, SimilarImages, SimilarImagesParameters};
use czkawka_core::tools::similar_videos::{SimilarVideos, SimilarVideosParameters};
use czkawka_core::tools::temporary::{Temporary, TemporaryParameters};
use czkawka_core::tools::video_optimizer::{VideoOptimizer, VideoOptimizerParameters};

use crate::{DuplicateScanRequest, DuplicateScanResponse, OmniDuplicateFileItem, OmniDuplicateGroup};

static INIT_CZKAWKA: std::sync::Once = std::sync::Once::new();

pub struct CzkawkaBridge;

fn ensure_media_tools_in_path() {
    let is_win = cfg!(target_os = "windows");
    let ffmpeg_exe = if is_win { "ffmpeg.exe" } else { "ffmpeg" };
    let ffprobe_exe = if is_win { "ffprobe.exe" } else { "ffprobe" };
    let platform_dir = match std::env::consts::OS {
        "windows" => "win32",
        "macos" => "darwin",
        "linux" => "linux",
        other => other,
    };

    let search_roots = [
        std::env::current_dir().unwrap_or_default(),
        std::env::current_exe().map(|p| p.parent().unwrap_or(&p).to_path_buf()).unwrap_or_default(),
        PathBuf::from(r"D:\workspace\firefly-ai-folder"),
    ];

    let mut added_paths = Vec::new();

    for root in &search_roots {
        let mut cur = root.clone();
        for _ in 0..6 {
            // 1. 优先从 extraResources/bin/ffmpeg 探测
            let ffmpeg_candidates = [
                cur.join(format!("apps/omni/build/extraResources/bin/ffmpeg/{}", ffmpeg_exe)),
                cur.join(format!("build/extraResources/bin/ffmpeg/{}", ffmpeg_exe)),
                cur.join(format!("apps/omni/extraResources/bin/ffmpeg/{}", ffmpeg_exe)),
                cur.join(format!("extraResources/bin/ffmpeg/{}", ffmpeg_exe)),
                cur.join(format!("resources/bin/{}/{}", platform_dir, ffmpeg_exe)),
                cur.join(format!("resources/bin/{}", ffmpeg_exe)),
                cur.join(format!("node_modules/@ffmpeg-installer/win32-x64/{}", ffmpeg_exe)),
                cur.join(format!(r"node_modules\.pnpm\@ffmpeg-installer+win32-x64@4.1.0\node_modules\@ffmpeg-installer\win32-x64\{}", ffmpeg_exe)),
            ];
            for c in &ffmpeg_candidates {
                if c.exists() {
                    if let Some(parent) = c.parent() {
                        let parent_buf = parent.to_path_buf();
                        if !added_paths.contains(&parent_buf) {
                            added_paths.push(parent_buf);
                        }
                    }
                    break;
                }
            }

            // 2. 优先从 extraResources/bin/ffprobe 探测
            let ffprobe_candidates = [
                cur.join(format!("apps/omni/build/extraResources/bin/ffprobe/{}", ffprobe_exe)),
                cur.join(format!("build/extraResources/bin/ffprobe/{}", ffprobe_exe)),
                cur.join(format!("apps/omni/extraResources/bin/ffprobe/{}", ffprobe_exe)),
                cur.join(format!("extraResources/bin/ffprobe/{}", ffprobe_exe)),
                cur.join(format!("resources/bin/{}/{}", platform_dir, ffprobe_exe)),
                cur.join(format!("resources/bin/{}", ffprobe_exe)),
                cur.join(format!("node_modules/@ffprobe-installer/win32-x64/{}", ffprobe_exe)),
                cur.join(format!(r"node_modules\.pnpm\@ffprobe-installer+win32-x64@5.1.0\node_modules\@ffprobe-installer\win32-x64\{}", ffprobe_exe)),
            ];
            for c in &ffprobe_candidates {
                if c.exists() {
                    if let Some(parent) = c.parent() {
                        let parent_buf = parent.to_path_buf();
                        if !added_paths.contains(&parent_buf) {
                            added_paths.push(parent_buf);
                        }
                    }
                    break;
                }
            }

            if !cur.pop() {
                break;
            }
        }
    }

    if !added_paths.is_empty() {
        if let Ok(current_path) = std::env::var("PATH") {
            let joined = added_paths
                .iter()
                .map(|p| p.to_string_lossy().to_string())
                .collect::<Vec<_>>()
                .join(";");
            let new_path = format!("{};{}", joined, current_path);
            unsafe {
                std::env::set_var("PATH", new_path);
            }
        }
    }
}

/// 将 Path/PathBuf 统一转换为符合当前操作系统原生标准的路径字符串（Windows 下为 \，Unix/macOS 下为 /）
#[inline]
pub fn to_native_path_str<P: AsRef<std::path::Path>>(path: P) -> String {
    let raw = path.as_ref().to_string_lossy().to_string();
    if cfg!(target_os = "windows") {
        raw.replace('/', "\\")
    } else {
        raw.replace('\\', "/")
    }
}

impl CzkawkaBridge {
    pub fn init() {
        INIT_CZKAWKA.call_once(|| {
            czkawka_core::common::config_cache_path::set_config_cache_path("firefly_omni", "firefly_omni");
            ensure_media_tools_in_path();
        });
    }

    pub fn scan(
        req: &DuplicateScanRequest,
        stop_flag: &Arc<AtomicBool>,
    ) -> DuplicateScanResponse {
        Self::scan_streaming(req, stop_flag, |_| {}, |_, _, _| {})
    }

    pub fn scan_streaming<FG, FP>(
        req: &DuplicateScanRequest,
        stop_flag: &Arc<AtomicBool>,
        mut on_group: FG,
        on_progress: FP,
    ) -> DuplicateScanResponse
    where
        FG: FnMut(&OmniDuplicateGroup),
        FP: Fn(usize, usize, String) + Send + Sync + 'static,
    {
        Self::init();
        let start = Instant::now();
        let target_paths: Vec<PathBuf> = req.paths.iter().map(PathBuf::from).collect();
        let enabled_strategies = req.strategies.clone().unwrap_or_else(|| {
            vec!["exact_hash".to_string(), "image_phash".to_string()]
        });

        // 动态平滑映射 0.0 ~ 10.0 最小相似度阈值到各模态引擎容差参数 (单位为0~10分，支持1位小数)
        let raw_sim = req.min_similarity.unwrap_or(7.5);
        let min_sim = if raw_sim > 10.0 { raw_sim / 10.0 } else { raw_sim }.clamp(0.0, 10.0);
        // 0.0 (10.0 完全精确一致) -> 1.0 (0.0 最大容差/动作差异/同场景)
        let sim_factor = ((10.0 - min_sim) / 10.0).clamp(0.0, 1.0);

        // 1. 图片容差 (256-bit 哈希): 10.0 -> 0, 5.0 -> 35 (连拍微移), 0.0 -> 70 (动作不同/同场景构图)
        let image_max_diff = (sim_factor * 70.0).round() as u32;
        let audio_max_diff = (sim_factor * 9.0 + 1.0) as f64;
        let video_tolerance = (sim_factor * 20.0).round() as i32;
        let video_min_matching = (0.8 - sim_factor * 0.6) as f64;
        let video_subclip_min = (0.7 - sim_factor * 0.5) as f64;
        let video_audio_similarity = (min_sim * 10.0) as f64;

        let total_strategies = enabled_strategies.len();
        let completed_strategies = Arc::new(AtomicUsize::new(0));

        let progress_tx = {
            let on_prog = Arc::new(on_progress);
            let comp = completed_strategies.clone();
            move |data: &ProgressData| {
                let current_step = comp.load(Ordering::Relaxed);
                let current_pct = if data.files_to_check > 0 {
                    (data.current_stage * 100) / data.files_to_check
                } else {
                    0
                };
                let overall_pct = ((current_step * 100) + current_pct) / total_strategies.max(1);
                on_prog(overall_pct.min(100), current_step, format!("正在比对: {}/{}", data.current_stage, data.files_to_check));
            }
        };

        let mut groups: Vec<OmniDuplicateGroup> = Vec::new();

        let run_exact = enabled_strategies.iter().any(|s| s == "exact_hash" || s == "exact" || s == "duplicates");
        let run_image = enabled_strategies.iter().any(|s| s == "image_phash" || s == "image" || s == "similar_images");
        let run_audio = enabled_strategies.iter().any(|s| s == "audio_hash" || s == "audio" || s == "same_music");
        let run_video = req.check_video == Some(true) || enabled_strategies.iter().any(|s| s == "video_phash" || s == "video" || s == "similar_videos");
        let run_bad_ext = enabled_strategies.iter().any(|s| s == "bad_extensions");
        let run_empty_folders = enabled_strategies.iter().any(|s| s == "empty_folders" || s == "empty_folder");
        let run_big_files = enabled_strategies.iter().any(|s| s == "big_files" || s == "big_file");
        let run_empty_files = enabled_strategies.iter().any(|s| s == "empty_files" || s == "empty_file");
        let run_temporary_files = enabled_strategies.iter().any(|s| s == "temporary_files" || s == "temporary");
        let run_invalid_symlinks = enabled_strategies.iter().any(|s| s == "invalid_symlinks" || s == "invalid_symlink");
        let run_broken_files = enabled_strategies.iter().any(|s| s == "broken_files" || s == "broken_file");
        let run_bad_names = enabled_strategies.iter().any(|s| s == "bad_names" || s == "bad_name");
        let run_exif_remover = enabled_strategies.iter().any(|s| s == "exif_remover");
        let run_video_optimizer = enabled_strategies.iter().any(|s| s == "video_optimizer");
        let excluded_items: Vec<String> = req.excluded_items.clone().unwrap_or_default();

        if run_exact {
            let params = DuplicateFinderParameters::new(CheckingMethod::Hash, HashType::Blake3, false, 0, 0, false);
            let mut finder = DuplicateFinder::new(params);
            finder.set_included_paths(target_paths.clone());
            if !excluded_items.is_empty() {
                finder.set_excluded_items(excluded_items.clone());
            }
            finder.search(stop_flag, Some(&progress_tx));
            let mut exact_group_idx = 1;
            for (size, vectors_vector) in finder.get_files_sorted_by_hash().iter().rev() {
                for vector in vectors_vector {
                    if vector.len() >= 2 {
                        let items: Vec<OmniDuplicateFileItem> = vector.iter().map(|entry| {
                            OmniDuplicateFileItem { path: to_native_path_str(&entry.path), name: entry.path.file_name().unwrap_or_default().to_string_lossy().to_string(), size: *size, modified_at: format!("{}", entry.modified_date), fingerprint: entry.hash.clone(), similarity_score: Some(1.0) }
                        }).collect();
                        let group = OmniDuplicateGroup {
                            group_id: format!("exact_{}", exact_group_idx),
                            strategy: "exact_hash".to_string(),
                            similarity_percentage: 100.0,
                            group_threshold: Some(10.0), // 踩线阈值: 10.0 (100% 精确一致)
                            description: format!("100% 完全精确一致文件 ({}个)", vector.len()),
                            files: items,
                            potential_freed_bytes: *size * (vector.len() as u64 - 1)
                        };
                        on_group(&group);
                        groups.push(group);
                        exact_group_idx += 1;
                    }
                }
            }
        }

        if run_image {
            let params = SimilarImagesParameters::new(image_max_diff, 16, HashAlg::Gradient, FilterType::Lanczos3, false, false, GeometricInvariance::Off);
            let mut img_finder = SimilarImages::new(params);
            img_finder.set_included_paths(target_paths.clone());
            img_finder.search(stop_flag, Some(&progress_tx));
            let mut img_group_idx = 1;
            for vector in img_finder.get_similar_images() {
                if vector.len() >= 2 {
                    let items: Vec<OmniDuplicateFileItem> = vector.iter().map(|entry| {
                        OmniDuplicateFileItem { path: to_native_path_str(&entry.path), name: entry.path.file_name().unwrap_or_default().to_string_lossy().to_string(), size: entry.size, modified_at: format!("{}", entry.modified_date), fingerprint: format!("{:?}", entry.hashes), similarity_score: Some(min_sim / 10.0) }
                    }).collect();
                    let avg_size = vector.iter().map(|e| e.size).sum::<u64>() / vector.len() as u64;
                    // 动态计算当前组实际踩线容差阈值 (0.0 ~ 10.0)
                    let current_sim_percent = (1.0 - (image_max_diff as f32 / 70.0)) * 100.0;
                    let group = OmniDuplicateGroup {
                        group_id: format!("image_{}", img_group_idx),
                        strategy: "image_phash".to_string(),
                        similarity_percentage: current_sim_percent,
                        group_threshold: Some(min_sim), // 当前组踩线域值：如果相似度低于此域值就匹配不到
                        description: format!("视觉感知相似图像 ({}个)", vector.len()),
                        files: items,
                        potential_freed_bytes: avg_size * (vector.len() as u64 - 1)
                    };
                    on_group(&group);
                    groups.push(group);
                    img_group_idx += 1;
                }
            }
        }

        if run_audio {
            let params = SameMusicParameters::new(
                MusicSimilarity::TRACK_TITLE,
                false,
                CheckingMethod::AudioContent,
                1.0,
                audio_max_diff,
                false,
            );
            let mut music_tool = SameMusic::new(params);
            music_tool.set_included_paths(target_paths.clone());
            music_tool.search(stop_flag, Some(&progress_tx));
            let mut music_idx = 1;
            for vector in music_tool.get_duplicated_music_entries() {
                if vector.len() >= 2 {
                    let items: Vec<OmniDuplicateFileItem> = vector.iter().map(|entry| {
                        OmniDuplicateFileItem { path: to_native_path_str(&entry.path), name: entry.path.file_name().unwrap_or_default().to_string_lossy().to_string(), size: entry.size, modified_at: format!("{}", entry.modified_date), fingerprint: format!("{:?}", entry.fingerprint), similarity_score: Some(min_sim / 10.0) }
                    }).collect();
                    let avg_size = vector.iter().map(|e| e.size).sum::<u64>() / vector.len() as u64;
                    let group = OmniDuplicateGroup {
                        group_id: format!("audio_{}", music_idx),
                        strategy: "audio_hash".to_string(),
                        similarity_percentage: min_sim * 10.0,
                        group_threshold: Some(min_sim), // 当前组踩线域值
                        description: format!("同源/声学相似音频文件 ({}个)", vector.len()),
                        files: items,
                        potential_freed_bytes: avg_size * (vector.len() as u64 - 1)
                    };
                    on_group(&group);
                    groups.push(group);
                    music_idx += 1;
                }
            }
        }

        if run_video {
            let params = SimilarVideosParameters::new(
                video_tolerance,
                false,
                false,
                0,
                2,
                true,
                3,
                50.0,
                video_min_matching,
                video_subclip_min,
                false,
                10,
                false,
                2,
                false,
                video_audio_similarity,
                audio_max_diff,
                0.1,
                1,
            );
            let mut video_tool = SimilarVideos::new(params);
            video_tool.set_included_paths(target_paths.clone());
            video_tool.search(stop_flag, Some(&progress_tx));
            let mut video_idx = 1;
            for vector in video_tool.get_similar_videos() {
                if vector.len() >= 2 {
                    let items: Vec<OmniDuplicateFileItem> = vector.iter().map(|entry| {
                        OmniDuplicateFileItem { path: to_native_path_str(&entry.path), name: entry.path.file_name().unwrap_or_default().to_string_lossy().to_string(), size: entry.size, modified_at: format!("{}", entry.modified_date), fingerprint: format!("{:?}", entry.signature), similarity_score: Some(min_sim / 10.0) }
                    }).collect();
                    let avg_size = vector.iter().map(|e| e.size).sum::<u64>() / vector.len() as u64;
                    let group = OmniDuplicateGroup {
                        group_id: format!("video_{}", video_idx),
                        strategy: "video_phash".to_string(),
                        similarity_percentage: min_sim * 10.0,
                        group_threshold: Some(min_sim), // 当前组踩线域值
                        description: format!("同源/画面相似视频文件 ({}个)", vector.len()),
                        files: items,
                        potential_freed_bytes: avg_size * (vector.len() as u64 - 1)
                    };
                    on_group(&group);
                    groups.push(group);
                    video_idx += 1;
                }
            }
        }

        let mut detected_bad_ext_paths: std::collections::HashSet<String> = std::collections::HashSet::new();

        if run_bad_ext {
            let mut bad_tool = BadExtensions::new(BadExtensionsParameters::new());
            bad_tool.set_included_paths(target_paths.clone());
            bad_tool.search(stop_flag, Some(&progress_tx));
            let bad_files = bad_tool.get_bad_extensions_files();
            if !bad_files.is_empty() {
                let items: Vec<OmniDuplicateFileItem> = bad_files.iter().map(|entry| {
                    let p = to_native_path_str(&entry.path);
                    detected_bad_ext_paths.insert(p.clone());
                    OmniDuplicateFileItem { path: p, name: format!("{} (真实应为: .{})", entry.path.file_name().unwrap_or_default().to_string_lossy(), entry.proper_extension), size: entry.size, modified_at: format!("{}", entry.modified_date), fingerprint: entry.proper_extensions_group.clone(), similarity_score: Some(0.0) }
                }).collect();
                let group = OmniDuplicateGroup {
                    group_id: "bad_extensions".to_string(),
                    strategy: "bad_extensions".to_string(),
                    similarity_percentage: 0.0,
                    group_threshold: None,
                    description: format!("扩展名不匹配文件 ({}个)", bad_files.len()),
                    files: items,
                    potential_freed_bytes: 0
                };
                on_group(&group);
                groups.push(group);
            }
        }

        if run_empty_folders {
            let mut empty_folder_tool = EmptyFolder::new();
            empty_folder_tool.set_included_paths(target_paths.clone());
            empty_folder_tool.search(stop_flag, Some(&progress_tx));
            let empty_folder_list = empty_folder_tool.get_empty_folder_list();
            if !empty_folder_list.is_empty() {
                let items: Vec<OmniDuplicateFileItem> = empty_folder_list.values().map(|entry| {
                    OmniDuplicateFileItem { path: to_native_path_str(&entry.path), name: entry.path.file_name().unwrap_or_default().to_string_lossy().to_string(), size: 0, modified_at: format!("{}", entry.modified_date), fingerprint: "empty_folder".to_string(), similarity_score: Some(1.0) }
                }).collect();
                let group = OmniDuplicateGroup {
                    group_id: "empty_folders".to_string(),
                    strategy: "empty_folders".to_string(),
                    similarity_percentage: 100.0,
                    group_threshold: None,
                    description: format!("空文件夹 ({}个)", empty_folder_list.len()),
                    files: items,
                    potential_freed_bytes: 0
                };
                on_group(&group);
                groups.push(group);
            }
        }

        if run_big_files {
            let mut big_file_tool = BigFile::new(BigFileParameters::new(50, BigFileSearchMode::BiggestFiles));
            big_file_tool.set_included_paths(target_paths.clone());
            big_file_tool.search(stop_flag, Some(&progress_tx));
            let big_files = big_file_tool.get_big_files();
            const MIN_BIG_FILE_SIZE: u64 = 10 * 1024 * 1024; // 10MB 起始指标
            let filtered_big_files: Vec<_> = big_files.iter().filter(|entry| entry.size >= MIN_BIG_FILE_SIZE).collect();
            if !filtered_big_files.is_empty() {
                let items: Vec<OmniDuplicateFileItem> = filtered_big_files.iter().map(|entry| {
                    OmniDuplicateFileItem { path: to_native_path_str(&entry.path), name: entry.path.file_name().unwrap_or_default().to_string_lossy().to_string(), size: entry.size, modified_at: format!("{}", entry.modified_date), fingerprint: format!("{}_bytes", entry.size), similarity_score: Some(1.0) }
                }).collect();
                let group = OmniDuplicateGroup {
                    group_id: "big_files".to_string(),
                    strategy: "big_files".to_string(),
                    similarity_percentage: 100.0,
                    group_threshold: None,
                    description: format!("占用空间超大文件 >= 10MB Top {} (共{}个)", items.len(), items.len()),
                    files: items,
                    potential_freed_bytes: 0
                };
                on_group(&group);
                groups.push(group);
            }
        }

        if run_empty_files {
            let mut empty_files_tool = EmptyFiles::new(EmptyFilesParameters::default());
            empty_files_tool.set_included_paths(target_paths.clone());
            empty_files_tool.search(stop_flag, Some(&progress_tx));
            let empty_files = empty_files_tool.get_empty_files();
            if !empty_files.is_empty() {
                let items: Vec<OmniDuplicateFileItem> = empty_files.iter().map(|entry| {
                    OmniDuplicateFileItem { path: to_native_path_str(&entry.path), name: entry.path.file_name().unwrap_or_default().to_string_lossy().to_string(), size: entry.size, modified_at: format!("{}", entry.modified_date), fingerprint: "0_bytes".to_string(), similarity_score: Some(1.0) }
                }).collect();
                let group = OmniDuplicateGroup {
                    group_id: "empty_files".to_string(),
                    strategy: "empty_files".to_string(),
                    similarity_percentage: 100.0,
                    group_threshold: None,
                    description: format!("0 字节空文件 ({}个)", empty_files.len()),
                    files: items,
                    potential_freed_bytes: 0
                };
                on_group(&group);
                groups.push(group);
            }
        }

        if run_temporary_files {
            let mut temp_tool = Temporary::new(TemporaryParameters::default());
            temp_tool.set_included_paths(target_paths.clone());
            temp_tool.search(stop_flag, Some(&progress_tx));
            let temp_files = temp_tool.get_temporary_files();
            if !temp_files.is_empty() {
                let total_size: u64 = temp_files.iter().map(|e| e.size).sum();
                let items: Vec<OmniDuplicateFileItem> = temp_files.iter().map(|entry| {
                    OmniDuplicateFileItem { path: to_native_path_str(&entry.path), name: entry.path.file_name().unwrap_or_default().to_string_lossy().to_string(), size: entry.size, modified_at: format!("{}", entry.modified_date), fingerprint: "temp_file".to_string(), similarity_score: Some(1.0) }
                }).collect();
                let group = OmniDuplicateGroup {
                    group_id: "temporary_files".to_string(),
                    strategy: "temporary_files".to_string(),
                    similarity_percentage: 100.0,
                    group_threshold: None,
                    description: format!("临时与残留缓存文件 ({}个)", temp_files.len()),
                    files: items,
                    potential_freed_bytes: total_size
                };
                on_group(&group);
                groups.push(group);
            }
        }

        if run_invalid_symlinks {
            let mut symlink_tool = InvalidSymlinks::new();
            symlink_tool.set_included_paths(target_paths.clone());
            symlink_tool.search(stop_flag, Some(&progress_tx));
            let invalid_symlinks = symlink_tool.get_invalid_symlinks();
            if !invalid_symlinks.is_empty() {
                let items: Vec<OmniDuplicateFileItem> = invalid_symlinks.iter().map(|entry| {
                    OmniDuplicateFileItem { path: to_native_path_str(&entry.path), name: format!("{} (指向不存在: {})", entry.path.file_name().unwrap_or_default().to_string_lossy(), entry.symlink_info.destination_path.to_string_lossy()), size: entry.size, modified_at: format!("{}", entry.modified_date), fingerprint: format!("{:?}", entry.symlink_info.type_of_error), similarity_score: Some(0.0) }
                }).collect();
                let group = OmniDuplicateGroup {
                    group_id: "invalid_symlinks".to_string(),
                    strategy: "invalid_symlinks".to_string(),
                    similarity_percentage: 100.0,
                    group_threshold: None,
                    description: format!("无效或断裂的软链接 ({}个)", invalid_symlinks.len()),
                    files: items,
                    potential_freed_bytes: 0
                };
                on_group(&group);
                groups.push(group);
            }
        }

        if run_broken_files {
            // 若用户未同时勾选 bad_extensions 策略，则在后台动态探测一次 bad_extensions 路径以进行交叉防误判
            if !run_bad_ext {
                let mut temp_bad_tool = BadExtensions::new(BadExtensionsParameters::new());
                temp_bad_tool.set_included_paths(target_paths.clone());
                temp_bad_tool.search(stop_flag, None);
                for entry in temp_bad_tool.get_bad_extensions_files() {
                    detected_bad_ext_paths.insert(to_native_path_str(&entry.path));
                }
            }

            let params = BrokenFilesParameters::new(CheckedTypes::PDF | CheckedTypes::AUDIO | CheckedTypes::IMAGE | CheckedTypes::ARCHIVE | CheckedTypes::MARKUP);
            let mut broken_tool = BrokenFiles::new(params);
            broken_tool.set_included_paths(target_paths.clone());
            broken_tool.search(stop_flag, Some(&progress_tx));
            let broken_files = broken_tool.get_broken_files();
            let valid_broken_files: Vec<_> = broken_files
                .iter()
                .filter(|e| e.size > 0 && !detected_bad_ext_paths.contains(&to_native_path_str(&e.path)))
                .collect();
            if !valid_broken_files.is_empty() {
                let items: Vec<OmniDuplicateFileItem> = valid_broken_files.iter().map(|entry| {
                    let name = entry.path.file_name().unwrap_or_default().to_string_lossy().to_string();
                    let error_msg = entry.get_error_string();
                    OmniDuplicateFileItem { path: to_native_path_str(&entry.path), name: if error_msg.is_empty() { name } else { format!("{} ({})", name, error_msg) }, size: entry.size, modified_at: format!("{}", entry.modified_date), fingerprint: error_msg, similarity_score: Some(0.0) }
                }).collect();
                let group = OmniDuplicateGroup {
                    group_id: "broken_files".to_string(),
                    strategy: "broken_files".to_string(),
                    similarity_percentage: 0.0,
                    group_threshold: None,
                    description: format!("损坏或无法解码的文件 ({}个)", valid_broken_files.len()),
                    files: items,
                    potential_freed_bytes: 0
                };
                on_group(&group);
                groups.push(group);
            }
        }

        if run_bad_names {
            // 方案 2: 根据 name_issues_mode 参数决定是否允许合法多语言字符 (中文/日韩等)
            let is_strict_ascii = req.name_issues_mode.as_deref() == Some("strict_ascii");
            let name_issues = if is_strict_ascii {
                NameIssues::all()
            } else {
                // 默认多语言模式: 关闭 non_ascii_graphical 检查，保留中文/日韩等多语言字符原貌，
                // 同时严密检查首尾空格、emoji、重复特殊符号以及大写扩展名等真正的问题。
                NameIssues {
                    uppercase_extension: true,
                    emoji_used: true,
                    space_at_start_or_end: true,
                    non_ascii_graphical: false,
                    restricted_charset_allowed: None,
                    remove_duplicated_non_alphanumeric: true,
                }
            };

            let params = BadNamesParameters::new(name_issues);
            let mut bad_names_tool = BadNames::new(params);
            bad_names_tool.set_included_paths(target_paths.clone());
            bad_names_tool.search(stop_flag, Some(&progress_tx));
            let bad_names = bad_names_tool.get_bad_names_files();
            if !bad_names.is_empty() {
                let items: Vec<OmniDuplicateFileItem> = bad_names.iter().map(|entry| {
                    OmniDuplicateFileItem { path: to_native_path_str(&entry.path), name: format!("{} (建议更名: {})", entry.path.file_name().unwrap_or_default().to_string_lossy(), entry.new_name), size: entry.size, modified_at: format!("{}", entry.modified_date), fingerprint: entry.new_name.clone(), similarity_score: Some(0.0) }
                }).collect();
                let group = OmniDuplicateGroup {
                    group_id: "bad_names".to_string(),
                    strategy: "bad_names".to_string(),
                    similarity_percentage: 0.0,
                    group_threshold: None,
                    description: format!("包含异常/不合规字符的文件名 ({}个)", bad_names.len()),
                    files: items,
                    potential_freed_bytes: 0
                };
                on_group(&group);
                groups.push(group);
            }
        }

        if run_exif_remover {
            let mut exif_tool = ExifRemover::new(ExifRemoverParameters::default());
            exif_tool.set_included_paths(target_paths.clone());
            exif_tool.search(stop_flag, Some(&progress_tx));
            let exif_files = exif_tool.get_exif_files();
            if !exif_files.is_empty() {
                let items: Vec<OmniDuplicateFileItem> = exif_files.iter().map(|entry| {
                    OmniDuplicateFileItem { path: to_native_path_str(&entry.path), name: format!("{} (含 {} 项Exif标记)", entry.path.file_name().unwrap_or_default().to_string_lossy(), entry.exif_tags.len()), size: entry.size, modified_at: format!("{}", entry.modified_date), fingerprint: format!("{}_exif_tags", entry.exif_tags.len()), similarity_score: Some(1.0) }
                }).collect();
                let group = OmniDuplicateGroup {
                    group_id: "exif_remover".to_string(),
                    strategy: "exif_remover".to_string(),
                    similarity_percentage: 100.0,
                    group_threshold: None,
                    description: format!("可清除 Exif 隐私信息的文件 ({}个)", exif_files.len()),
                    files: items,
                    potential_freed_bytes: 0
                };
                on_group(&group);
                groups.push(group);
            }
        }

        if run_video_optimizer {
            let params = VideoOptimizerParameters::VideoTranscode(czkawka_core::tools::video_optimizer::VideoTranscodeParams::new(vec!["av1".to_string(), "hevc".to_string()], false, 10, false, 3));
            let mut video_opt_tool = VideoOptimizer::new(params);
            video_opt_tool.set_included_paths(target_paths.clone());
            video_opt_tool.search(stop_flag, Some(&progress_tx));
            let transcode_entries = video_opt_tool.get_video_transcode_entries();
            if !transcode_entries.is_empty() {
                let items: Vec<OmniDuplicateFileItem> = transcode_entries.iter().map(|entry| {
                    OmniDuplicateFileItem { path: to_native_path_str(&entry.path), name: format!("{} (当前编码: {}, 分辨率: {}x{})", entry.path.file_name().unwrap_or_default().to_string_lossy(), entry.codec, entry.width, entry.height), size: entry.size, modified_at: format!("{}", entry.modified_date), fingerprint: entry.codec.clone(), similarity_score: Some(1.0) }
                }).collect();
                let group = OmniDuplicateGroup {
                    group_id: "video_optimizer".to_string(),
                    strategy: "video_optimizer".to_string(),
                    similarity_percentage: 100.0,
                    group_threshold: None,
                    description: format!("可转码/优化的高效能视频 ({}个)", transcode_entries.len()),
                    files: items,
                    potential_freed_bytes: 0
                };
                on_group(&group);
                groups.push(group);
            }
        }

        drop(progress_tx);
        let _ = progress_handle.join();

        let total_freed_bytes: u64 = groups.iter().map(|g| g.potential_freed_bytes).sum();
        let total_redundant_files: usize = groups.iter().map(|g| if g.files.len() > 1 { g.files.len() - 1 } else { 0 }).sum();
        let total_scanned = total_files_scanned.load(Ordering::Relaxed).max(groups.iter().map(|g| g.files.len()).sum());

        DuplicateScanResponse {
            success: true,
            total_scanned,
            duplicate_groups: groups,
            total_redundant_files,
            total_freed_bytes,
            duration_ms: start.elapsed().as_millis() as u64,
        }
    }
}
