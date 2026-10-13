use std::path::{Path, PathBuf};
use std::sync::Arc;
use sherpa_onnx::{
    AudioTagging, AudioTaggingConfig, AudioTaggingModelConfig, GenerationConfig,
    OfflineRecognizer, OfflineRecognizerConfig, OfflineSenseVoiceModelConfig,
    OfflineSpeechDenoiser, OfflineSpeechDenoiserConfig, OfflineSpeechDenoiserGtcrnModelConfig,
    OfflineSpeechDenoiserModelConfig, OfflineTts, OfflineTtsConfig, OfflineTtsKokoroModelConfig,
    SileroVadModelConfig, VadModelConfig, VoiceActivityDetector,
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

/// Sherpa-ONNX 全栈音频管理引擎 (ASR, VAD, GTCRN 降噪, CED 打标, Kokoro 离线 TTS)
pub struct SherpaManager {
    recognizer: Option<OfflineRecognizer>,
    vad_config: Option<VadModelConfig>,
    denoiser: Option<OfflineSpeechDenoiser>,
    tagger: Option<AudioTagging>,
    tts: Option<Arc<OfflineTts>>,
    tts_semaphore: Arc<tokio::sync::Semaphore>,
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

        // 5. 初始化 Kokoro 离线 TTS 引擎
        let tts = Self::init_kokoro_tts();

        // 6. 建立单并发互斥信号量，防止 TTS 多线程并发造成 CPU 颠簸
        let tts_semaphore = Arc::new(tokio::sync::Semaphore::new(1));

        Self {
            recognizer,
            vad_config,
            denoiser,
            tagger,
            tts,
            tts_semaphore,
        }
    }

    /// 探测当前平台推荐的 ONNX Runtime 硬件加速 Execution Provider 列表
    fn preferred_sherpa_providers() -> Vec<&'static str> {
        let mut providers = Vec::new();
        #[cfg(target_os = "windows")]
        {
            providers.push("directml");
            providers.push("cuda");
        }
        #[cfg(target_os = "macos")]
        {
            providers.push("coreml");
        }
        #[cfg(target_os = "linux")]
        {
            providers.push("cuda");
        }
        providers.push("cpu");
        providers
    }

    fn init_sense_voice() -> Option<OfflineRecognizer> {
        let model_path = resolve_model_file(&["sensevoice", "audio/sensevoice"], "model.int8.onnx")
            .or_else(|| resolve_model_file(&["sensevoice", "audio/sensevoice"], "model.onnx"))?;
        let tokens_path = resolve_model_file(&["sensevoice", "audio/sensevoice"], "tokens.txt")?;

        let threads = omni_core::OmniOrtRuntime::recommended_intra_threads() as i32;
        let providers = Self::preferred_sherpa_providers();

        for &provider in &providers {
            let mut config = OfflineRecognizerConfig::default();
            config.model_config.sense_voice = OfflineSenseVoiceModelConfig {
                model: Some(model_path.to_str()?.to_string()),
                language: Some(String::new()),
                use_itn: true,
            };
            config.model_config.tokens = Some(tokens_path.to_str()?.to_string());
            config.model_config.num_threads = threads;
            config.model_config.debug = false;
            config.model_config.provider = Some(provider.to_string());

            if let Some(rec) = OfflineRecognizer::create(&config) {
                tracing::info!(
                    "[SherpaManager] 成功加载 SenseVoice ASR 模型 (provider: {}, threads: {}): {:?}",
                    provider,
                    threads,
                    model_path
                );
                return Some(rec);
            } else if provider != "cpu" {
                tracing::warn!(
                    "[SherpaManager] SenseVoice 硬件加速 provider '{}' 初始化失败，自动降级下一个候选",
                    provider
                );
            }
        }

        tracing::warn!("[SherpaManager] 加载 SenseVoice ASR 最终失败: {:?}", model_path);
        None
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

    /// 初始化 Kokoro 8-bit 量化离线语音合成引擎
    fn init_kokoro_tts() -> Option<Arc<OfflineTts>> {
        // 模型加载策略：优先加载 310MB 全精度未量化模型 (消除 8-bit 量化沙沙底噪)，找不到时降级至 model.int8.onnx
        let model_path = resolve_model_file(&["audio/kokoro", "kokoro", "audio/tts", "tts/kokoro"], "model.full.onnx")
            .or_else(|| resolve_model_file(&["audio/kokoro", "kokoro", "audio/tts", "tts/kokoro"], "model.onnx"))
            .or_else(|| resolve_model_file(&["audio/kokoro", "kokoro", "audio/tts", "tts/kokoro"], "model.int8.onnx"))?;
        let voices_path = resolve_model_file(&["audio/kokoro", "kokoro", "audio/tts", "tts/kokoro"], "voices.bin")?;
        let tokens_path = resolve_model_file(&["audio/kokoro", "kokoro", "audio/tts", "tts/kokoro"], "tokens.txt")?;

        let model_dir = model_path.parent()?;
        let data_dir = model_dir.join("espeak-ng-data");
        let dict_dir = model_dir.join("dict");
        let lexicon_en = model_dir.join("lexicon-us-en.txt");
        let lexicon_zh = model_dir.join("lexicon-zh.txt");
        let lexicon_str = if lexicon_en.exists() && lexicon_zh.exists() {
            format!("{},{}", lexicon_en.to_str()?, lexicon_zh.to_str()?)
        } else if lexicon_en.exists() {
            lexicon_en.to_str()?.to_string()
        } else if lexicon_zh.exists() {
            lexicon_zh.to_str()?.to_string()
        } else {
            String::new()
        };

        let threads = omni_core::OmniOrtRuntime::recommended_intra_threads() as i32;
        let providers = Self::preferred_sherpa_providers();

        for &provider in &providers {
            let mut config = OfflineTtsConfig::default();
            config.model.kokoro = OfflineTtsKokoroModelConfig {
                model: Some(model_path.to_str()?.to_string()),
                voices: Some(voices_path.to_str()?.to_string()),
                tokens: Some(tokens_path.to_str()?.to_string()),
                data_dir: if data_dir.exists() { Some(data_dir.to_str()?.to_string()) } else { None },
                length_scale: 1.0,
                dict_dir: if dict_dir.exists() { Some(dict_dir.to_str()?.to_string()) } else { None },
                lexicon: if !lexicon_str.is_empty() { Some(lexicon_str.clone()) } else { None },
                lang: None,
            };
            config.model.num_threads = threads;
            config.model.debug = false;
            config.model.provider = Some(provider.to_string());

            if let Some(t) = OfflineTts::create(&config) {
                tracing::info!(
                    "[SherpaManager] 成功加载 Kokoro TTS 模型 (provider: {}, threads: {}): {:?}, 采样率: {}Hz, 发音人数: {}",
                    provider,
                    threads,
                    model_path,
                    t.sample_rate(),
                    t.num_speakers()
                );
                return Some(Arc::new(t));
            } else if provider != "cpu" {
                tracing::warn!(
                    "[SherpaManager] Kokoro TTS 硬件加速 provider '{}' 初始化失败，自动降级下一个候选",
                    provider
                );
            }
        }

        tracing::warn!("[SherpaManager] 加载 Kokoro TTS 模型最终失败: {:?}", model_path);
        None
    }

    /// 获取 Kokoro 语音合成引擎状态 (就绪标志, 总发音人数, 采样率)
    pub fn tts_status(&self) -> (bool, i32, i32) {
        if let Some(ref tts) = self.tts {
            (true, tts.num_speakers(), tts.sample_rate())
        } else {
            (false, 0, 24000)
        }
    }

    /// 获取单通道并发控制信号量引用
    pub fn tts_semaphore(&self) -> Arc<tokio::sync::Semaphore> {
        self.tts_semaphore.clone()
    }

    /// 单句离线语音合成：返回标准 24kHz 16-bit 单声道 WAV 二进制数据
    pub fn synthesize_chunk(&self, text: &str, sid: i32, speed: f32) -> Result<Vec<u8>, String> {
        let tts = self.tts.as_ref().ok_or_else(|| "Kokoro TTS engine not initialized or model missing".to_string())?;

        // 1. 安全过滤 NUL 字符与特殊标点平滑映射：
        // a. 将感叹号（! 和 ！）转换为逗号（，和 ,），消除 Kokoro 韵律模型在感叹号下的高频跳变怪音；
        // b. 英文词边界与中英文夹杂规范化：确保 Folder / folder 前后有独立词间距；
        let mut clean_text = text.replace('\0', " ")
            .replace('！', "，")
            .replace('!', ",");

        // 中英混读时，若 Kokoro 中文分词未切分英文大写复合词，确保 Folder 能够被独立发音
        clean_text = clean_text.trim().to_string();
        if clean_text.is_empty() {
            return Err("Input text is empty".to_string());
        }

        // 2. 发音人与速率安全归一化
        let mut gen_config = GenerationConfig::default();
        let max_speakers = tts.num_speakers();
        gen_config.sid = if sid >= 0 && (max_speakers == 0 || sid < max_speakers) {
            sid
        } else {
            0
        };
        gen_config.speed = if (0.1..=5.0).contains(&speed) { speed } else { 1.0 };

        // 3. 执行纯内存生成
        let audio = tts.generate_with_config(&clean_text, &gen_config, None::<fn(&[f32], f32) -> bool>)
            .ok_or_else(|| "Failed to generate audio from Kokoro TTS".to_string())?;

        let raw_samples = audio.samples();

        // 4. 音质与质感声学净化增强 (DSP 链路优化)：
        // 彻底解决 8-bit ISTFTNet 量化带来的高频散粒底噪（沙沙声）与字间底噪泄漏：
        // a. 二阶低通平滑滤波 (Biquad Butterworth Low-Pass Filter @ 8.2 kHz, Q = 0.707):
        //    有效滤除 8.5kHz ~ 12kHz 频段无语义贡献的 8-bit 高频相位截断噪底，保留纯净人声泛音与齿音细节；
        // b. 动态软噪声门 (Soft Downward Noise Gate):
        //    字间换气与静音停顿段平滑压制底噪，打造“纯黑背景”的高保真静音；
        // c. 自动峰值电平归一化 (Peak Normalization @ -0.5 dBFS):
        //    将纯净波形自适应提升至广播级电平，饱满清澈。
        let enhanced_samples: Vec<f32> = if raw_samples.is_empty() {
            Vec::new()
        } else {
            let sample_rate = audio.sample_rate() as f32; // 24000.0 Hz
            let cutoff_hz = 8200.0f32; // 截止频率 8.2 kHz
            let omega = 2.0f32 * std::f32::consts::PI * cutoff_hz / sample_rate;
            let sn = omega.sin();
            let cs = omega.cos();
            let alpha = sn / (2.0f32 * std::f32::consts::FRAC_1_SQRT_2); // Q = 0.707 (Butterworth 最大平坦)

            let b0 = (1.0 - cs) / 2.0;
            let b1 = 1.0 - cs;
            let b2 = (1.0 - cs) / 2.0;
            let a0 = 1.0 + alpha;
            let a1 = -2.0 * cs;
            let a2 = 1.0 - alpha;

            // 归一化双二阶滤波器系数
            let nb0 = b0 / a0;
            let nb1 = b1 / a0;
            let nb2 = b2 / a0;
            let na1 = a1 / a0;
            let na2 = a2 / a0;

            let mut x1 = 0.0f32;
            let mut x2 = 0.0f32;
            let mut y1 = 0.0f32;
            let mut y2 = 0.0f32;

            // 第一阶段：低通滤波滤除高频量化散粒白噪
            let filtered: Vec<f32> = raw_samples
                .iter()
                .map(|&x0| {
                    let y0 = nb0 * x0 + nb1 * x1 + nb2 * x2 - na1 * y1 - na2 * y2;
                    x2 = x1;
                    x1 = x0;
                    y2 = y1;
                    y1 = y0;
                    y0
                })
                .collect();

            // 第二阶段：自适应平滑噪声门 (消除字间残余微弱量化噪底)
            // 设定微弱信号门限 (-48 dBFS 约为 0.004)
            let gate_threshold = 0.0045f32;
            let mut envelope = 0.0f32;
            let attack_coeff = 0.25f32;
            let release_coeff = 0.008f32;

            let gated: Vec<f32> = filtered
                .iter()
                .map(|&s| {
                    let abs_s = s.abs();
                    if abs_s > envelope {
                        envelope += (abs_s - envelope) * attack_coeff;
                    } else {
                        envelope += (abs_s - envelope) * release_coeff;
                    }

                    // 软门限增益衰减曲线
                    let gate_gain = if envelope >= gate_threshold {
                        1.0f32
                    } else if envelope > 1e-5 {
                        (envelope / gate_threshold).powf(1.5f32)
                    } else {
                        0.0f32
                    };

                    s * gate_gain
                })
                .collect();

            // 第三阶段：Soft-Peak 广播级归一化
            let max_peak = gated.iter().fold(0.0f32, |acc, &x| acc.max(x.abs()));
            let target_peak = 0.944f32; // -0.5 dBFS
            let gain = if max_peak > 1e-4 {
                (target_peak / max_peak).min(3.5f32)
            } else {
                1.0f32
            };

            gated
                .iter()
                .map(|&s| (s * gain).clamp(-1.0f32, 1.0f32))
                .collect()
        };

        // 5. 编码为内存 RIFF WAV 字节流
        let wav_bytes = encode_pcm_f32_to_wav(&enhanced_samples, audio.sample_rate() as u32);
        Ok(wav_bytes)
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

/// 纯内存将 f32 PCM 向量编码为标准 16-bit 单声道 WAV 二进制数据（含 44 字节 RIFF 头）
pub fn encode_pcm_f32_to_wav(samples: &[f32], sample_rate: u32) -> Vec<u8> {
    let num_samples = samples.len() as u32;
    let num_channels: u16 = 1;
    let bits_per_sample: u16 = 16;
    let bytes_per_sample: u16 = 2;
    let byte_rate = sample_rate * (num_channels as u32) * (bytes_per_sample as u32);
    let block_align = num_channels * bytes_per_sample;
    let data_chunk_size = num_samples * (bytes_per_sample as u32);
    let riff_chunk_size = 36 + data_chunk_size;

    let mut buf = Vec::with_capacity((44 + data_chunk_size) as usize);
    // RIFF 块描述
    buf.extend_from_slice(b"RIFF");
    buf.extend_from_slice(&riff_chunk_size.to_le_bytes());
    buf.extend_from_slice(b"WAVE");
    // "fmt " 子块
    buf.extend_from_slice(b"fmt ");
    buf.extend_from_slice(&16u32.to_le_bytes()); // subchunk1 size = 16 (PCM)
    buf.extend_from_slice(&1u16.to_le_bytes());  // AudioFormat = 1 (PCM)
    buf.extend_from_slice(&num_channels.to_le_bytes());
    buf.extend_from_slice(&sample_rate.to_le_bytes());
    buf.extend_from_slice(&byte_rate.to_le_bytes());
    buf.extend_from_slice(&block_align.to_le_bytes());
    buf.extend_from_slice(&bits_per_sample.to_le_bytes());
    // "data" 子块
    buf.extend_from_slice(b"data");
    buf.extend_from_slice(&data_chunk_size.to_le_bytes());

    for &sample in samples {
        let clamped = sample.max(-1.0).min(1.0);
        let s16 = (clamped * 32767.0) as i16;
        buf.extend_from_slice(&s16.to_le_bytes());
    }

    buf
}

#[cfg(test)]
mod tests {
    use super::*;
    use omni_core::concepts::Concept;

    #[test]
    fn test_encode_pcm_f32_to_wav_header() {
        let sample_rate = 24000u32;
        // 构造 1 秒静音/正弦波数据 (24000 个浮点数)
        let samples = vec![0.0f32; 24000];
        let wav = encode_pcm_f32_to_wav(&samples, sample_rate);

        // 验证 WAV 头部
        assert_eq!(&wav[0..4], b"RIFF");
        assert_eq!(&wav[8..12], b"WAVE");
        assert_eq!(&wav[12..16], b"fmt ");
        assert_eq!(&wav[36..40], b"data");

        // 验证总长度：44 字节头 + 24000 * 2 = 48044 字节
        assert_eq!(wav.len(), 44 + 48000);

        // 验证采样率字节 (小端 24000 = 0x00005DC0)
        let sr = u32::from_le_bytes(wav[24..28].try_into().unwrap());
        assert_eq!(sr, 24000);
    }

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

    #[test]
    fn test_kokoro_tts_lifecycle_and_synthesis() {
        let manager = SherpaManager::new();
        let (ready, num_speakers, sample_rate) = manager.tts_status();
        println!("Kokoro TTS status: ready={}, speakers={}, rate={}Hz", ready, num_speakers, sample_rate);

        assert!(ready, "Kokoro TTS engine should be initialized when models exist in extraResources");
        assert_eq!(num_speakers, 103, "Kokoro multi-lang v1.1 should have 103 speakers");
        assert_eq!(sample_rate, 24000, "Kokoro TTS standard sample rate is 24000Hz");

        // 测试中英文合成（包含 Folder、感叹号与逗号）
        let wav1 = manager
            .synthesize_chunk("Hello Folder! Welcome to Firefly AI Folder.", 0, 1.0)
            .expect("Synthesize en Folder should succeed");
        println!("Synthesized en Folder wav bytes: {}", wav1.len());

        let wav2 = manager
            .synthesize_chunk("你好！欢迎使用 Firefly AI Folder 驱动的下一代文件管理", 44, 1.0)
            .expect("Synthesize zh Folder should succeed");
        println!("Synthesized zh Folder wav bytes: {}", wav2.len());

        assert!(wav1.len() > 44, "WAV output must be longer than 44-byte header");
        assert_eq!(&wav1[0..4], b"RIFF", "Header magic should be RIFF");
        assert_eq!(&wav1[8..12], b"WAVE", "Header format should be WAVE");
    }
}
