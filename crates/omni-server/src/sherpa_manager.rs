use std::path::{Path, PathBuf};
use sherpa_onnx::{
    AudioTagging, AudioTaggingConfig, AudioTaggingModelConfig,
    OfflineRecognizer, OfflineRecognizerConfig, OfflineSenseVoiceModelConfig,
    OfflineSpeechDenoiser, OfflineSpeechDenoiserConfig, OfflineSpeechDenoiserGtcrnModelConfig,
    OfflineSpeechDenoiserModelConfig, SileroVadModelConfig, VadModelConfig, VoiceActivityDetector,
};

/// 527 类 CED-mini / AudioSet 声学事件映射与受控本体归一化契约 (AGENTS.md / PRD-0058)
///
/// 严格守则：
/// 1. 优先对齐权威知识图谱或应用受控概念（如 `omw.00543233.n` (音乐), `builtin.sound_effects` (音效), `builtin.voice_memo` (语音) 等）；
/// 2. 未收录于受控概念表的声学事件，必须加挂统一命名空间 `custom.audio.<slug>` 前缀；
/// 3. 严禁在业务内裸写未批准的无前缀魔法字符串或裸自然语言。
pub fn normalize_ced_audio_event(raw_event: &str) -> String {
    let clean = raw_event.trim();
    if clean.is_empty() {
        return String::new();
    }

    let lower = clean.to_ascii_lowercase();

    // 1. 音乐范畴收敛 -> Concept::音乐.code()
    if lower.contains("music")
        || lower.contains("singing")
        || lower.contains("song")
        || lower.contains("musical instrument")
        || lower.contains("melody")
        || lower.contains("orchestra")
        || lower.contains("guitar")
        || lower.contains("piano")
        || lower.contains("drum")
        || lower.contains("violin")
    {
        return omni_core::concepts::Concept::音乐.code().to_string();
    }

    // 2. 人声/言语范畴收敛 -> builtin.voice_memo / builtin.voice
    if lower.contains("speech")
        || lower.contains("conversation")
        || lower.contains("narration")
        || lower.contains("whispering")
        || lower.contains("shout")
        || lower.contains("screaming")
        || lower.contains("laughter")
        || lower.contains("giggle")
        || lower.contains("crying")
        || lower.contains("cough")
    {
        return omni_core::concepts::Concept::语音备忘.code().to_string();
    }

    // 3. 音效范畴收敛 -> Concept::音效.code()
    if lower == "sound effect" || lower == "sound effects" || lower == "sfx" {
        return omni_core::concepts::Concept::音效.code().to_string();
    }

    // 4. 其他 CED / AudioSet 标签：收敛为 custom.audio.<slug>
    let slug: String = lower
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    let slug = slug.trim_matches('_');
    format!("custom.audio.{}", slug)
}

/// 辅助：定位模型候选根目录与文件
fn resolve_model_file(sub_dirs: &[&str], file_name: &str) -> Option<PathBuf> {
    let search_roots = [
        std::env::current_dir().unwrap_or_default(),
        std::env::current_exe()
            .map(|p| p.parent().unwrap_or(p.as_path()).to_path_buf())
            .unwrap_or_default(),
    ];

    for root in &search_roots {
        let mut cur = root.clone();
        for _ in 0..8 {
            for sub in sub_dirs {
                let p1 = cur.join(format!("apps/desktop/build/extraResources/models/{}/{}", sub, file_name));
                if p1.exists() {
                    return Some(p1);
                }
                let p2 = cur.join(format!("resources/models/{}/{}", sub, file_name));
                if p2.exists() {
                    return Some(p2);
                }
                let p3 = cur.join(format!("models/{}/{}", sub, file_name));
                if p3.exists() {
                    return Some(p3);
                }
            }
            if let Some(parent) = cur.parent() {
                cur = parent.to_path_buf();
            } else {
                break;
            }
        }
    }
    None
}

/// 确保加载安全的 ONNX Runtime 动态库（Windows 环境下免疫 System32 的老版本劫持）
pub fn ensure_safe_onnx_dll_directory() {
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::ffi::OsStrExt;
        use std::path::PathBuf;

        static ONNX_INIT: std::sync::Once = std::sync::Once::new();
        ONNX_INIT.call_once(|| {
            let search_candidates = [
                "apps/desktop/build/extraResources/bin/onnx",
                "resources/bin/onnx",
                "bin/onnx",
                "target/debug",
                "target/release",
                "apps/omni/target/debug",
            ];

            let search_roots = [
                std::env::var("FIREFLY_ONNX_DIR").ok().map(PathBuf::from),
                std::env::current_dir().ok(),
                std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.to_path_buf())),
                std::env::current_exe().ok().and_then(|p| p.parent().and_then(|d| d.parent().map(|p2| p2.to_path_buf()))),
            ];

            let mut found_dir = None;

            if let Ok(custom_dir) = std::env::var("FIREFLY_ONNX_DIR") {
                let p = PathBuf::from(&custom_dir);
                if p.join("onnxruntime.dll").exists() {
                    found_dir = Some(p);
                }
            }

            if found_dir.is_none() {
                'outer: for root in search_roots.iter().flatten() {
                    let mut cur = root.clone();
                    for _ in 0..8 {
                        for candidate in &search_candidates {
                            let p = cur.join(candidate);
                            if p.join("onnxruntime.dll").exists() {
                                found_dir = Some(p);
                                break 'outer;
                            }
                        }
                        if cur.join("onnxruntime.dll").exists() {
                            found_dir = Some(cur.clone());
                            break 'outer;
                        }
                        if let Some(parent) = cur.parent() {
                            cur = parent.to_path_buf();
                        } else {
                            break;
                        }
                    }
                }
            }

            if let Some(ref dir) = found_dir {
                let dll_path = dir.join("onnxruntime.dll");
                let wide: Vec<u16> = dir
                    .as_os_str()
                    .encode_wide()
                    .chain(std::iter::once(0))
                    .collect();
                unsafe {
                    extern "system" {
                        fn SetDllDirectoryW(lpPathName: *const u16) -> i32;
                    }
                    SetDllDirectoryW(wide.as_ptr());
                }
                if let Some(dll_str) = dll_path.to_str() {
                    let _ = omni_core::OmniOrtRuntime::init_with_dll(dll_str);
                }
            }
        });
    }
}

/// Sherpa-ONNX 全栈音频管理引擎 (ASR, VAD, GTCRN 降噪, CED 打标)
pub struct SherpaManager {
    recognizer: Option<OfflineRecognizer>,
    vad_config: Option<VadModelConfig>,
    denoiser: Option<OfflineSpeechDenoiser>,
    tagger: Option<AudioTagging>,
}

impl SherpaManager {
    /// 尝试从本地资源探测并构建音频全栈引擎
    pub fn new() -> Self {
        ensure_safe_onnx_dll_directory();

        // 1. 初始化 SenseVoice ASR
        let recognizer = Self::init_sense_voice();

        // 2. 初始化 Silero VAD Config
        let vad_config = Self::init_silero_vad_config();

        // 3. 初始化 GTCRN 深度降噪器
        let denoiser = Self::init_gtcrn_denoiser();

        // 4. 初始化 CED-mini 音频打标器
        let tagger = Self::init_ced_tagger();

        Self {
            recognizer,
            vad_config,
            denoiser,
            tagger,
        }
    }

    fn init_sense_voice() -> Option<OfflineRecognizer> {
        let model_path = resolve_model_file(&["sensevoice", "audio/sensevoice"], "model.int8.onnx")
            .or_else(|| resolve_model_file(&["sensevoice", "audio/sensevoice"], "model.onnx"))?;
        let tokens_path = resolve_model_file(&["sensevoice", "audio/sensevoice"], "tokens.txt")?;

        let mut config = OfflineRecognizerConfig::default();
        config.model_config.sense_voice = OfflineSenseVoiceModelConfig {
            model: Some(model_path.to_str()?.to_string()),
            language: Some(String::new()),
            use_itn: true,
        };
        config.model_config.tokens = Some(tokens_path.to_str()?.to_string());
        config.model_config.num_threads = 2;
        config.model_config.debug = false;

        let rec = OfflineRecognizer::create(&config);
        if let Some(ref _r) = rec {
            tracing::info!("[SherpaManager] 成功加载 SenseVoice ASR 模型: {:?}", model_path);
        } else {
            tracing::warn!("[SherpaManager] 加载 SenseVoice ASR 失败: {:?}", model_path);
        }
        rec
    }

    fn init_silero_vad_config() -> Option<VadModelConfig> {
        let model_path = resolve_model_file(&["vad", "audio/vad"], "silero_vad.onnx")?;

        let mut config = VadModelConfig::default();
        config.silero_vad = SileroVadModelConfig {
            model: Some(model_path.to_str()?.to_string()),
            threshold: 0.5,
            min_silence_duration: 0.5,
            min_speech_duration: 0.1,
            window_size: 512,
            max_speech_duration: 30.0,
        };
        config.sample_rate = 16000;
        config.num_threads = 1;
        config.provider = Some("cpu".to_string());
        config.debug = false;

        tracing::info!("[SherpaManager] 成功定位 Silero VAD 模型: {:?}", model_path);
        Some(config)
    }

    fn init_gtcrn_denoiser() -> Option<OfflineSpeechDenoiser> {
        let model_path = resolve_model_file(&["denoise", "audio/denoise"], "gtcrn_simple.onnx")
            .or_else(|| resolve_model_file(&["denoise", "audio/denoise"], "gtcrn.onnx"))?;

        let mut config = OfflineSpeechDenoiserConfig::default();
        config.model = OfflineSpeechDenoiserModelConfig {
            gtcrn: OfflineSpeechDenoiserGtcrnModelConfig {
                model: Some(model_path.to_str()?.to_string()),
            },
            dpdfnet: Default::default(),
            num_threads: 2,
            debug: false,
            provider: Some("cpu".to_string()),
        };

        let denoiser = OfflineSpeechDenoiser::create(&config);
        if let Some(ref _d) = denoiser {
            tracing::info!("[SherpaManager] 成功加载 GTCRN 降噪模型: {:?}", model_path);
        } else {
            tracing::warn!("[SherpaManager] 加载 GTCRN 降噪模型失败: {:?}", model_path);
        }
        denoiser
    }

    fn init_ced_tagger() -> Option<AudioTagging> {
        let model_path = resolve_model_file(&["ced", "audio/ced"], "ced_mini.onnx")
            .or_else(|| resolve_model_file(&["ced", "audio/ced"], "model.onnx"))?;
        let labels_path = resolve_model_file(&["ced", "audio/ced"], "class_labels_indices.csv")?;

        let mut config = AudioTaggingConfig::default();
        config.model = AudioTaggingModelConfig {
            ced: Some(model_path.to_str()?.to_string()),
            zipformer: Default::default(),
            num_threads: 2,
            debug: false,
            provider: Some("cpu".to_string()),
        };
        config.labels = Some(labels_path.to_str()?.to_string());
        config.top_k = 5;

        let tagger = AudioTagging::create(&config);
        if let Some(ref _t) = tagger {
            tracing::info!("[SherpaManager] 成功加载 CED-mini 音频打标模型: {:?}", model_path);
        } else {
            tracing::warn!("[SherpaManager] 加载 CED-mini 音频打标模型失败: {:?}", model_path);
        }
        tagger
    }

    /// 纯内存 GTCRN 深度学习降噪（替代传统 FFmpeg afftdn 临时文件）
    pub fn denoise(&self, samples: &[f32], sample_rate: i32) -> Vec<f32> {
        if samples.is_empty() {
            return Vec::new();
        }
        if let Some(denoiser) = &self.denoiser {
            let denoised = denoiser.run(samples, sample_rate);
            denoised.samples
        } else {
            samples.to_vec()
        }
    }

    /// Silero VAD 语音活性检测与有效语音提取
    ///
    /// 【防 Panic 守卫契约】:
    /// 若输入样本有效语音累计总时长 < 0.1s (1600 个样本 @ 16kHz) 或全为静音，
    /// 直接安全返回 (false, Vec::new())，杜绝空张量传入下游 SenseVoice 引发 Panic。
    ///
    /// 【流式窗口契约】:
    /// sherpa-onnx 的 VoiceActivityDetector::accept_waveform 必须按 window_size (512) 分块喂入，
    /// 并在每次喂入后及时 pop SpeechSegment，严禁一次性塞入整段长音频导致内部缓冲区重置丢帧。
    pub fn vad_filter(&self, samples: &[f32], sample_rate: i32) -> (bool, Vec<f32>) {
        if samples.is_empty() {
            return (false, Vec::new());
        }

        // 基础音量/全静音前置检查：若最大绝对振幅几乎为 0，视为全静音
        let max_amp = samples.iter().fold(0.0f32, |m, &x| m.max(x.abs()));
        if max_amp < 1e-4 {
            return (false, Vec::new());
        }

        let vad_cfg = match &self.vad_config {
            Some(cfg) => cfg,
            None => {
                // 未配置 VAD 时，若总时长 < 0.1s 则返回空，否则返回原波形
                let min_samples = (sample_rate as f32 * 0.1) as usize;
                if samples.len() < min_samples {
                    return (false, Vec::new());
                }
                return (true, samples.to_vec());
            }
        };

        let vad = match VoiceActivityDetector::create(vad_cfg, 60.0) {
            Some(v) => v,
            None => {
                tracing::warn!("[SherpaManager] 创建 VAD 实例失败");
                return (true, samples.to_vec());
            }
        };

        let window_size = vad_cfg.silero_vad.window_size.max(512) as usize;
        // 相邻语音段之间保留 0.2s 自然静音过渡垫片，防止句间停顿被完全剪除后前后字音硬连读失真
        let pause_padding_len = (sample_rate.max(8000) as usize) / 5;
        let mut voice_samples: Vec<f32> = Vec::new();
        let mut speech_only_len: usize = 0;

        let mut drain_segments = |vad_inst: &VoiceActivityDetector| {
            while !vad_inst.is_empty() {
                if let Some(segment) = vad_inst.front() {
                    let seg_samples: &[f32] = segment.samples();
                    if !seg_samples.is_empty() {
                        if !voice_samples.is_empty() {
                            voice_samples.resize(voice_samples.len() + pause_padding_len, 0.0f32);
                        }
                        voice_samples.extend_from_slice(seg_samples);
                        speech_only_len += seg_samples.len();
                    }
                    vad_inst.pop();
                } else {
                    break;
                }
            }
        };

        for chunk in samples.chunks(window_size) {
            vad.accept_waveform(chunk);
            drain_segments(&vad);
        }
        vad.flush();
        drain_segments(&vad);

        let min_samples = (sample_rate as f32 * 0.1) as usize;
        if speech_only_len < min_samples {
            tracing::debug!(
                "[SherpaManager] VAD 过滤后有效语音长度不足 0.1s ({} 样本)，安全返回空",
                speech_only_len
            );
            (false, Vec::new())
        } else {
            (true, voice_samples)
        }
    }

    /// 使用 SenseVoice 进行端到端语音识别 (ASR)
    pub fn transcribe(&self, samples: &[f32], sample_rate: i32) -> Option<String> {
        let recognizer = self.recognizer.as_ref()?;

        // 防空张量守卫
        let min_samples = (sample_rate as f32 * 0.1) as usize;
        if samples.len() < min_samples {
            return None;
        }

        let stream = recognizer.create_stream();
        stream.accept_waveform(sample_rate, samples);
        recognizer.decode(&stream);

        let result = stream.get_result()?;
        let text = result.text.trim().to_string();
        if text.is_empty() {
            None
        } else {
            Some(text)
        }
    }

    /// 使用 CED-mini 对音频进行声学事件打标，并归一化为受控本体标签 (audio_events)
    pub fn tag_audio(&self, samples: &[f32], sample_rate: i32, top_k: usize) -> Vec<String> {
        let tagger = match &self.tagger {
            Some(t) => t,
            None => return Vec::new(),
        };

        if samples.is_empty() {
            return Vec::new();
        }

        // 全静音前置守卫：振幅几乎为 0 时直接返回空打标，避免虚假事件并节省推理开销
        let max_amp = samples.iter().fold(0.0f32, |m, &x| m.max(x.abs()));
        if max_amp < 1e-4 {
            return Vec::new();
        }

        let stream = tagger.create_stream();
        stream.accept_waveform(sample_rate, samples);
        let events = tagger.compute(&stream, top_k as i32);

        let mut res = Vec::new();
        for ev in events {
            if ev.prob >= 0.15 {
                let norm = normalize_ced_audio_event(&ev.name);
                if !norm.is_empty() && !res.contains(&norm) {
                    res.push(norm);
                }
            }
        }
        res
    }

    /// 端到端纯内存音频处理流水线：降噪 -> VAD -> ASR + 声学打标
    pub fn process_audio_pipeline(
        &self,
        samples: &[f32],
        sample_rate: i32,
    ) -> (Option<String>, Vec<String>) {
        if samples.is_empty() {
            return (None, Vec::new());
        }

        // 1. 声学打标在原始或微降噪波形上提取事件 (CED-mini)
        let audio_events = self.tag_audio(samples, sample_rate, 5);

        // 2. 纯内存 GTCRN 降噪
        let denoised = self.denoise(samples, sample_rate);

        // 3. Silero VAD 防御性语音检测与截断
        let (has_voice, voice_samples) = self.vad_filter(&denoised, sample_rate);

        // 4. 若有有效语音，执行 SenseVoice ASR
        let transcript = if has_voice {
            self.transcribe(&voice_samples, sample_rate)
        } else {
            None
        };

        (transcript, audio_events)
    }
}

impl Default for SherpaManager {
    fn default() -> Self {
        Self::new()
    }
}

/// 纯内存音频解码纯函数：将任意音频/视频媒体标准化转码解码为 16kHz 单声道 f32 样本
///
/// 契约要求：
/// 1. WAV 文件优先尝试原生纯内存直接解析；
/// 2. 非 WAV 文件（MP3/M4A/MP4/MKV 等）或非 16kHz WAV 通过常驻 FFmpeg 管道标准化转码：
///    `-ar 16000 -ac 1 -f f32le pipe:1`，自动完成立体声向单声道混音及抗混叠重采样；
/// 3. 音频处理全程纯内存管道流转，无未加密临时文件落地。
pub fn decode_audio_to_16k_mono(
    file_path: &Path,
    duration_seconds: Option<u32>,
) -> Option<Vec<f32>> {
    if !file_path.exists() {
        return None;
    }

    let ext = file_path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();

    // 1. WAV 文件尝试原生纯内存解析
    if ext == "wav" {
        if let Some(path_str) = file_path.to_str() {
            if let Some(wave) = sherpa_onnx::Wave::read(path_str) {
                if wave.sample_rate() == 16000 {
                    let mut samples = wave.samples().to_vec();
                    if let Some(dur) = duration_seconds {
                        let max_samples = (dur as usize) * 16000;
                        if samples.len() > max_samples {
                            samples.truncate(max_samples);
                        }
                    }
                    return Some(samples);
                }
            }
        }
    }

    // 2. 非 WAV 或需要重采样的媒体，通过常驻 FFmpeg 管道标准化转码
    let ffmpeg = resolve_ffmpeg_binary()?;

    let mut cmd = std::process::Command::new(ffmpeg);
    // 基础输入
    cmd.arg("-i").arg(file_path);

    // 截取时长约束 (可选)
    if let Some(dur) = duration_seconds {
        cmd.arg("-t").arg(dur.to_string());
    }

    // 强制禁用视频流，指定 16000Hz 单声道，f32le 原生 PCM，输出到标准管道
    cmd.args(["-vn", "-ar", "16000", "-ac", "1", "-f", "f32le", "pipe:1"]);

    let output = match cmd.output() {
        Ok(out) => out,
        Err(e) => {
            tracing::warn!("[decode_audio_to_16k_mono] FFmpeg 管道执行失败: {:?}", e);
            return None;
        }
    };

    if !output.status.success() {
        tracing::warn!(
            "[decode_audio_to_16k_mono] FFmpeg 转码退出状态码非 0: status={:?}, stderr={}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        );
        return None;
    }

    let raw_bytes = output.stdout;
    if raw_bytes.is_empty() {
        return Some(Vec::new());
    }

    // 将 f32le (每个 float 4 字节) 解析为 Vec<f32>
    let samples: Vec<f32> = raw_bytes
        .chunks_exact(4)
        .map(|chunk| f32::from_le_bytes(chunk.try_into().unwrap()))
        .collect();

    Some(samples)
}

/// 定位 FFmpeg 二进制路径（复用跨平台多级目录搜索）
pub fn resolve_ffmpeg_binary() -> Option<PathBuf> {
    let ffmpeg_exe = if cfg!(target_os = "windows") {
        "ffmpeg.exe"
    } else {
        "ffmpeg"
    };

    let search_roots = [
        std::env::current_dir().unwrap_or_default(),
        std::env::current_exe()
            .map(|p| p.parent().unwrap_or(p.as_path()).to_path_buf())
            .unwrap_or_default(),
    ];

    for root in &search_roots {
        let mut cur = root.clone();
        for _ in 0..8 {
            let candidates = [
                cur.join(format!("apps/desktop/build/extraResources/bin/ffmpeg/{}", ffmpeg_exe)),
                cur.join(format!("resources/bin/ffmpeg/{}", ffmpeg_exe)),
                cur.join(format!("resources/bin/{}", ffmpeg_exe)),
                cur.join(format!("Contents/Resources/bin/ffmpeg/{}", ffmpeg_exe)),
            ];
            for c in &candidates {
                if c.exists() {
                    return Some(c.clone());
                }
            }
            if let Some(parent) = cur.parent() {
                cur = parent.to_path_buf();
            } else {
                break;
            }
        }
    }

    // 兜底：系统 PATH
    let check_cmd = if cfg!(target_os = "windows") { "where" } else { "which" };
    std::process::Command::new(check_cmd)
        .arg(ffmpeg_exe)
        .output()
        .ok()
        .and_then(|out| {
            String::from_utf8(out.stdout)
                .ok()
                .and_then(|s| s.lines().next().map(|l| PathBuf::from(l.trim())))
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use omni_core::concepts::Concept;

    #[test]
    fn test_normalize_ced_audio_event_controlled_ontology() {
        // 1. 音乐范畴收敛 -> Concept::音乐.code()
        assert_eq!(
            normalize_ced_audio_event("Music"),
            Concept::音乐.code()
        );
        assert_eq!(
            normalize_ced_audio_event("Classical music"),
            Concept::音乐.code()
        );
        assert_eq!(
            normalize_ced_audio_event("Acoustic guitar"),
            Concept::音乐.code()
        );
        assert_eq!(
            normalize_ced_audio_event("Singing"),
            Concept::音乐.code()
        );

        // 2. 语音/人声范畴收敛 -> Concept::语音备忘.code()
        assert_eq!(
            normalize_ced_audio_event("Speech"),
            Concept::语音备忘.code()
        );
        assert_eq!(
            normalize_ced_audio_event("Conversation"),
            Concept::语音备忘.code()
        );
        assert_eq!(
            normalize_ced_audio_event("Laughter"),
            Concept::语音备忘.code()
        );
        assert_eq!(
            normalize_ced_audio_event("Whispering"),
            Concept::语音备忘.code()
        );

        // 3. 音效范畴收敛 -> Concept::音效.code()
        assert_eq!(
            normalize_ced_audio_event("Sound effect"),
            Concept::音效.code()
        );
        assert_eq!(
            normalize_ced_audio_event("sfx"),
            Concept::音效.code()
        );

        // 4. 未收录的声学事件 -> custom.audio.<slug>
        assert_eq!(
            normalize_ced_audio_event("Rain"),
            "custom.audio.rain"
        );
        assert_eq!(
            normalize_ced_audio_event("Dog Bark"),
            "custom.audio.dog_bark"
        );
        assert_eq!(
            normalize_ced_audio_event("Car horn / Traffic"),
            "custom.audio.car_horn___traffic"
        );

        // 5. 边缘空输入
        assert_eq!(normalize_ced_audio_event(""), "");
        assert_eq!(normalize_ced_audio_event("   "), "");
    }

    #[test]
    fn test_vad_guard_silence_and_short_audio() {
        let manager = SherpaManager::new();

        // 空切片
        let (has_voice, samples) = manager.vad_filter(&[], 16000);
        assert!(!has_voice);
        assert!(samples.is_empty());

        // 超短音频 (< 0.1s, 例如 0.05s = 800 样本 @ 16kHz)
        let short_audio = vec![0.5f32; 800];
        let (has_voice, samples) = manager.vad_filter(&short_audio, 16000);
        assert!(!has_voice);
        assert!(samples.is_empty());

        // 全静音音频 (1.0s = 16000 样本，振幅全部为 0)
        let silence_audio = vec![0.0f32; 16000];
        let (has_voice, samples) = manager.vad_filter(&silence_audio, 16000);
        assert!(!has_voice);
        assert!(samples.is_empty());

        // 极微弱底噪音频 (振幅 1e-5 < 1e-4)
        let noise_audio = vec![1e-5f32; 16000];
        let (has_voice, samples) = manager.vad_filter(&noise_audio, 16000);
        assert!(!has_voice);
        assert!(samples.is_empty());
    }

    #[test]
    fn test_transcribe_and_pipeline_guards() {
        let manager = SherpaManager::new();

        // 1. transcribe 空切片与超短音频必须安全返回 None，严禁 Panic
        assert!(manager.transcribe(&[], 16000).is_none());
        assert!(manager.transcribe(&vec![0.5f32; 800], 16000).is_none());

        // 2. process_audio_pipeline 全静音安全守卫：无 Panic 且返回 transcript 为 None，声学事件为空
        let (transcript, events) = manager.process_audio_pipeline(&vec![0.0f32; 16000], 16000);
        assert!(transcript.is_none());
        assert!(events.is_empty());

        let (transcript, events) = manager.process_audio_pipeline(&[], 16000);
        assert!(transcript.is_none());
        assert!(events.is_empty());
    }

    /// 辅助：生成标准的 16kHz 单声道 16-bit PCM WAV 字节
    fn create_test_wav_bytes(num_samples: usize, amplitude: i16) -> Vec<u8> {
        let mut bytes = Vec::new();
        let data_len = (num_samples * 2) as u32;
        let file_len = 36 + data_len;

        bytes.extend_from_slice(b"RIFF");
        bytes.extend_from_slice(&file_len.to_le_bytes());
        bytes.extend_from_slice(b"WAVE");
        bytes.extend_from_slice(b"fmt ");
        bytes.extend_from_slice(&16u32.to_le_bytes()); // subchunk size
        bytes.extend_from_slice(&1u16.to_le_bytes());  // PCM
        bytes.extend_from_slice(&1u16.to_le_bytes());  // mono
        bytes.extend_from_slice(&16000u32.to_le_bytes()); // sample rate
        bytes.extend_from_slice(&32000u32.to_le_bytes()); // byte rate
        bytes.extend_from_slice(&2u16.to_le_bytes());  // block align
        bytes.extend_from_slice(&16u16.to_le_bytes()); // bits per sample
        bytes.extend_from_slice(b"data");
        bytes.extend_from_slice(&data_len.to_le_bytes());

        for _ in 0..num_samples {
            bytes.extend_from_slice(&amplitude.to_le_bytes());
        }

        bytes
    }

    #[test]
    fn test_decode_audio_to_16k_mono_wav() {
        let temp_dir = tempfile::tempdir().unwrap();
        let wav_path = temp_dir.path().join("test_1s.wav");

        // 1 秒 16000 样本的 WAV 文件
        let wav_bytes = create_test_wav_bytes(16000, 1000);
        std::fs::write(&wav_path, wav_bytes).unwrap();

        let samples = decode_audio_to_16k_mono(&wav_path, None).expect("应该成功解码 WAV 文件");
        assert_eq!(samples.len(), 16000);

        // 测试 duration 截取
        let truncated = decode_audio_to_16k_mono(&wav_path, Some(1)).expect("应该成功解码并截取");
        assert_eq!(truncated.len(), 16000);

        // 不存在的文件返回 None
        let not_found = decode_audio_to_16k_mono(&temp_dir.path().join("none.wav"), None);
        assert!(not_found.is_none());
    }

    #[test]
    fn test_diagnose_azure_yunxi_wav() {
        let wav_path = Path::new(r"F:\workspace\CosyVoice2-Ex\audios\Azure - 云希.wav");
        if !wav_path.exists() {
            println!("Skip diagnostic: {:?} does not exist", wav_path);
            return;
        }
        let manager = SherpaManager::new();
        println!(
            "Models ready: recognizer={}, vad={}, denoiser={}, tagger={}",
            manager.recognizer.is_some(),
            manager.vad_config.is_some(),
            manager.denoiser.is_some(),
            manager.tagger.is_some()
        );
        assert!(manager.recognizer.is_some(), "SenseVoice recognizer must be loaded");
        assert!(manager.vad_config.is_some(), "Silero VAD config must be loaded");
        assert!(manager.denoiser.is_some(), "GTCRN denoiser must be loaded");
        assert!(manager.tagger.is_some(), "CED-mini tagger must be loaded");

        let samples = decode_audio_to_16k_mono(wav_path, Some(60)).expect("decode_audio_to_16k_mono failed");
        let (transcript, events) = manager.process_audio_pipeline(&samples, 16000);
        println!("Pipeline transcript: {:?}", transcript);
        println!("Pipeline audio_events: {:?}", events);

        let text = transcript.expect("ASR transcript should not be None");
        assert!(
            text.chars().count() > 15,
            "ASR transcript length should be > 15 chars, got: {}",
            text
        );
        assert!(
            text.contains("没有说什么") && text.contains("发动车子"),
            "ASR transcript should contain expected Chinese speech content, got: {}",
            text
        );
        assert!(!events.is_empty(), "CED-mini audio events should not be empty");
    }
}
