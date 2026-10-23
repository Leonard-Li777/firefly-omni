use std::path::{Path, PathBuf};
use sherpa_onnx::{
    AudioTagging, AudioTaggingConfig, AudioTaggingModelConfig,
    OfflineRecognizer, OfflineRecognizerConfig, OfflineSenseVoiceModelConfig,
    OfflineSpeechDenoiser, OfflineSpeechDenoiserConfig, OfflineSpeechDenoiserGtcrnModelConfig,
    OfflineSpeechDenoiserModelConfig,
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

/// 确保 sherpa 音频栈加载安全的 ONNX Runtime 动态库目录（Windows 环境下免疫 System32 的老版本劫持）
///
/// 仅通过 `SetDllDirectoryW` 把 `bin/omni` 加入 DLL 搜索优先级，让 sherpa 自身导入表
/// 自动加载运行时 A (`onnxruntime.dll` 1.28.2 CPU)；**绝不**初始化全局 `ort` crate 单例
/// （拓扑解绑，见函数内注释）。
pub fn ensure_safe_onnx_dll_directory() {
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::ffi::OsStrExt;
        use std::path::PathBuf;

        static ONNX_INIT: std::sync::Once = std::sync::Once::new();
        ONNX_INIT.call_once(|| {
            // 【bin/omni 单点收敛 (工单 03)】全仓动态库部署与发现目录统一为 bin/omni，
            // 旧 bin/onnx 目录已随资产治理 (工单 01) 废弃，严禁再回锚。
            let search_candidates = [
                "apps/desktop/build/extraResources/bin/omni",
                "resources/bin/omni",
                "bin/omni",
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
                // 【拓扑解绑 (工单 03)】此处严禁调用 `OmniOrtRuntime::init_with_dll`：
                // sherpa 音频栈只依赖其自身导入表自动加载 bin/omni/onnxruntime.dll（运行时 A），
                // 若在此抢先执行 `ort::init_from`，会消耗全局 ort crate 的单例句柄，
                // 把 sherpa 的 CPU-only 1.28.2 DLL 绑死到全局 ort 会话上，导致后续
                // DML 运行时 B (onnxruntime_dml.dll) 的加载与 warmup 全部落空。
                //
                // 【跨 crate 时序契约 (盲审 M-3)】`SetDllDirectoryW` 是进程级单槽替换：
                // 本函数必须在 SherpaManager::new() 启动期同步完成运行时 A 的加载，
                // 且先于任何 OmniOrtRuntime 绑定（后者会以运行时 B 父目录覆盖本槽位）。
                // 严禁将 sherpa 初始化改为惰性/延迟——否则运行时 B 目录会先占槽，
                // 排障时自定义 OMNI_ORT_DLL / FIREFLY_ONNX_DIR 指向别处将导致
                // 后续依赖 bin/omni 的延迟加载失配。
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

    /// 解析当前平台推荐的 ONNX Runtime Execution Provider 候选列表（纯函数，不读环境，便于单测）
    ///
    /// 【为何不再按平台盲推硬件加速 EP（2026-10-14 勘误）】
    /// 随包分发的 `sherpa-onnx` 运行时来自上游 **CPU-only** 归档
    /// （`sherpa-onnx-v1.13.8-win-x64-shared-MT-Release-lib`，v1.13.8 **未发布任何 DirectML 变体**），
    /// 其 `Ort::GetAvailableProviders()` 只含 `CPUExecutionProvider`。此时若传入非 cpu 的 provider，
    /// 上游 `csrc/session.cc:355` 会打印
    /// `Please compile with -DSHERPA_ONNX_ENABLE_GPU=ON. Available providers: ... Fallback to cpu!`
    /// 之后**静默回退 CPU 并仍然返回成功**（既不报错也不返回 null），于是：
    /// ① 每次启动都刷出一条 level=1 的伪错误日志；② 上层误报「已用 cuda」的虚假硬件加速归因。
    ///
    /// 【旧启发式的缺陷】曾以 `CUDA_PATH` / `CUDA_HOME` 是否存在推断 Windows 支持 CUDA —— 该变量只说明
    /// 本机装了 CUDA Toolkit，与本项目随包运行时的 EP 能力**完全无关**，属恒假阳性。
    /// 另注：上游 `kDirectML` 分支受**编译期** `SHERPA_ONNX_ENABLE_DIRECTML == 1` 门控，
    /// 故仅替换 `onnxruntime.dll` 也无法为 sherpa 开启 DirectML。
    ///
    /// 【现行策略】
    /// 1. 显式环境变量覆盖：`FIREFLY_SHERPA_PROVIDER` 非空时优先尊重用户指定（如 "cuda", "directml", "coreml"）；
    /// 2. macOS：默认 `coreml` —— 官方 osx 归档的 onnxruntime 默认内置 CoreML EP，属真实可用能力；
    /// 3. Windows / Linux：默认**仅 cpu**。随包归档不含 CUDA/DML EP，盲推只会制造伪告警；
    ///    确需 GPU 时由用户显式设置 `FIREFLY_SHERPA_PROVIDER`（并自备 GPU 版归档）。
    fn resolve_sherpa_providers(custom_override: Option<&str>) -> Vec<String> {
        if let Some(custom) = custom_override {
            let trimmed = custom.trim().to_lowercase();
            if !trimmed.is_empty() {
                return vec![trimmed, "cpu".to_string()];
            }
        }

        let mut providers = Vec::new();
        #[cfg(target_os = "macos")]
        {
            providers.push("coreml".to_string());
        }
        providers.push("cpu".to_string());
        providers
    }

    /// 探测当前平台推荐的 Execution Provider 候选列表（读取 `FIREFLY_SHERPA_PROVIDER` 覆盖）
    fn preferred_sherpa_providers() -> Vec<String> {
        Self::resolve_sherpa_providers(std::env::var("FIREFLY_SHERPA_PROVIDER").ok().as_deref())
    }

    fn init_sense_voice() -> Option<OfflineRecognizer> {
        let model_path = resolve_model_file(&["sensevoice", "audio/sensevoice"], "model.int8.onnx")
            .or_else(|| resolve_model_file(&["sensevoice", "audio/sensevoice"], "model.onnx"))?;
        let tokens_path = resolve_model_file(&["sensevoice", "audio/sensevoice"], "tokens.txt")?;

        let threads = omni_core::OmniOrtRuntime::recommended_asr_threads() as i32;
        let providers = Self::preferred_sherpa_providers();

        for provider in &providers {
            let mut config = OfflineRecognizerConfig::default();
            config.model_config.sense_voice = OfflineSenseVoiceModelConfig {
                model: Some(model_path.to_str()?.to_string()),
                language: Some(String::new()),
                use_itn: true,
            };
            config.model_config.tokens = Some(tokens_path.to_str()?.to_string());
            config.model_config.num_threads = threads;
            config.model_config.debug = false;
            config.model_config.provider = Some(provider.clone());

            if let Some(rec) = OfflineRecognizer::create(&config) {
                if provider == "cpu" {
                    tracing::info!(
                        "[SherpaManager] 成功加载 SenseVoice ASR 模型 (provider: cpu, threads: {}): {:?}",
                        threads,
                        model_path
                    );
                } else {
                    // ★ 严禁把「请求的 provider」当成「生效的 EP」：sherpa-onnx 在 EP 不可用时会静默
                    // 回退 CPU 且仍返回成功（csrc/session.cc:355），故此处只能如实标注为「请求值」。
                    tracing::info!(
                        "[SherpaManager] 成功加载 SenseVoice ASR 模型 (provider 请求值: {}, threads: {}; ★ 真实生效 EP 以 sherpa 上游日志为准，EP 不可用时会静默回退 CPU): {:?}",
                        provider,
                        threads,
                        model_path
                    );
                }
                return Some(rec);
            } else if provider != "cpu" {
                // 注：EP 不可用**不会**走到这里（上游静默回退 CPU 并返回成功），本分支只覆盖
                // 模型文件损坏 / tokens 缺失等真实构建失败，故保留。
                tracing::warn!(
                    "[SherpaManager] SenseVoice provider '{}' 构建失败，自动降级下一个候选",
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
}

/// SenseVoice 端到端转录及声学元数据解析结果
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SenseVoiceResult {
    /// 纯净自然文本（已剥离所有 `<|...|>` 特殊 Token）
    pub text: String,
    /// 识别出的主要语言代码（如 "zh", "en", "ja", "ko", "yue"）
    pub lang: Option<String>,
    /// 识别出的主要情感标记（如 "happy", "sad", "angry", "neutral", "fearful", "disgusted", "surprised"）
    pub emotion: Option<String>,
    /// 识别出的声音事件列表（如 ["laughter", "applause", "sing", "cry", "bgm"]）
    pub audio_events: Vec<String>,
}

/// 解析 SenseVoice 输出的原始带有特殊 Token 的文本并执行「声学标记优先 + 文本字符集兜底」双层语言判定
pub fn parse_sense_voice_tokens(raw_text: &str) -> SenseVoiceResult {
    if raw_text.trim().is_empty() {
        return SenseVoiceResult::default();
    }

    let mut lang: Option<String> = None;
    let mut emotion: Option<String> = None;
    let mut audio_events: Vec<String> = Vec::new();

    let mut clean_text = String::with_capacity(raw_text.len());
    let mut rest = raw_text;

    while let Some(start_idx) = rest.find("<|") {
        clean_text.push_str(&rest[..start_idx]);
        let token_slice = &rest[start_idx + 2..];
        if let Some(end_idx) = token_slice.find("|>") {
            let token_content = &token_slice[..end_idx];
            let token_trim = token_content.trim();

            // 1. 语言判定 (SenseVoice 语言 Token)
            let lower = token_trim.to_ascii_lowercase();
            if lower == "zh" || lower.starts_with("zh/") {
                if lang.is_none() {
                    lang = Some("zh".to_string());
                }
            } else if lower == "en" || lower.starts_with("en/") {
                if lang.is_none() {
                    lang = Some("en".to_string());
                }
            } else if lower == "ja" {
                if lang.is_none() {
                    lang = Some("ja".to_string());
                }
            } else if lower == "ko" {
                if lang.is_none() {
                    lang = Some("ko".to_string());
                }
            } else if lower == "yue" || lower == "dialect" || lower == "minnan" || lower == "wuyu" {
                if lang.is_none() {
                    lang = Some(lower.clone());
                }
            } else if matches!(
                lower.as_str(),
                "de" | "es" | "ru" | "fr" | "pt" | "tr" | "it" | "ar" | "hi" | "vi" | "th"
            ) {
                if lang.is_none() {
                    lang = Some(lower.clone());
                }
            }

            // 2. 情绪判定 (SenseVoice 情感 Token)
            let upper = token_trim.to_ascii_uppercase();
            match upper.as_str() {
                "HAPPY" => emotion = Some("happy".to_string()),
                "SAD" => emotion = Some("sad".to_string()),
                "ANGRY" => emotion = Some("angry".to_string()),
                "NEUTRAL" => emotion = Some("neutral".to_string()),
                "FEARFUL" => emotion = Some("fearful".to_string()),
                "DISGUSTED" => emotion = Some("disgusted".to_string()),
                "SURPRISED" => emotion = Some("surprised".to_string()),
                _ => {}
            }

            // 3. 声音事件判定
            match upper.as_str() {
                "BGM" => {
                    if !audio_events.iter().any(|e| e == "bgm") {
                        audio_events.push("bgm".to_string());
                    }
                }
                "LAUGHTER" => {
                    if !audio_events.iter().any(|e| e == "laughter") {
                        audio_events.push("laughter".to_string());
                    }
                }
                "APPLAUSE" => {
                    if !audio_events.iter().any(|e| e == "applause") {
                        audio_events.push("applause".to_string());
                    }
                }
                "CRY" => {
                    if !audio_events.iter().any(|e| e == "cry") {
                        audio_events.push("cry".to_string());
                    }
                }
                "SNEEZE" => {
                    if !audio_events.iter().any(|e| e == "sneeze") {
                        audio_events.push("sneeze".to_string());
                    }
                }
                "BREATH" => {
                    if !audio_events.iter().any(|e| e == "breath") {
                        audio_events.push("breath".to_string());
                    }
                }
                "COUGH" => {
                    if !audio_events.iter().any(|e| e == "cough") {
                        audio_events.push("cough".to_string());
                    }
                }
                "SING" => {
                    if !audio_events.iter().any(|e| e == "sing") {
                        audio_events.push("sing".to_string());
                    }
                }
                "SPEECH_NOISE" => {
                    if !audio_events.iter().any(|e| e == "speech_noise") {
                        audio_events.push("speech_noise".to_string());
                    }
                }
                _ => {}
            }

            rest = &token_slice[end_idx + 2..];
        } else {
            clean_text.push_str("<|");
            rest = token_slice;
        }
    }
    clean_text.push_str(rest);

    let text = clean_text
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .trim()
        .to_string();

    // 彻底滤除无意义文本（如仅有单个标点 "."、"..."、"。" 或纯符号空白）
    let is_meaningful = omni_core::is_meaningful_asr_text(&text);
    let text = if is_meaningful {
        text
    } else {
        String::new()
    };

    // 4. 双层语言判定：若声学标记缺失或未知，以转录文本 Unicode 字符集比例兜底
    // 注意：若文本无意义或为空，声学语言与情绪标记失去依托（噪音频段随机输出），置为 None
    let final_lang = if text.is_empty() {
        None
    } else if lang.is_some() {
        lang
    } else {
        detect_language_from_text(&text)
    };

    let final_emotion = if text.is_empty() {
        None
    } else {
        emotion
    };

    SenseVoiceResult {
        text,
        lang: final_lang,
        emotion: final_emotion,
        audio_events,
    }
}

/// 基于 Unicode 字符分布的次级兜底语言检测
fn detect_language_from_text(text: &str) -> Option<String> {
    let mut cjk_count = 0usize;
    let mut kana_count = 0usize;
    let mut hangul_count = 0usize;
    let mut latin_count = 0usize;

    for ch in text.chars() {
        match ch {
            '\u{4e00}'..='\u{9fa5}' => cjk_count += 1,
            '\u{3040}'..='\u{30ff}' => kana_count += 1,
            '\u{ac00}'..='\u{d7af}' => hangul_count += 1,
            'a'..='z' | 'A'..='Z' => latin_count += 1,
            _ => {}
        }
    }

    if kana_count > 0 {
        Some("ja".to_string())
    } else if hangul_count > 0 {
        Some("ko".to_string())
    } else if cjk_count > 0 && cjk_count * 2 >= latin_count {
        Some("zh".to_string())
    } else if latin_count > 0 {
        Some("en".to_string())
    } else {
        None
    }
}

impl SherpaManager {
    /// 使用 SenseVoice 进行端到端语音识别与声学元数据解析 (ASR + 语言 + 情感 + 声音事件)
    pub fn transcribe_full(&self, samples: &[f32], sample_rate: i32) -> Option<SenseVoiceResult> {
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
        let raw_text = result.text.trim();
        if raw_text.is_empty() {
            return None;
        }

        let parsed = parse_sense_voice_tokens(raw_text);
        if parsed.text.is_empty() && parsed.audio_events.is_empty() {
            None
        } else {
            Some(parsed)
        }
    }

    /// 使用 SenseVoice 进行端到端语音识别 (ASR，兼容纯文本接口)
    pub fn transcribe(&self, samples: &[f32], sample_rate: i32) -> Option<String> {
        self.transcribe_full(samples, sample_rate)
            .map(|r| r.text)
            .filter(|t| omni_core::is_meaningful_asr_text(t))
    }

    /// 使用 CED-mini 对音频进行声学事件打标，并归一化为受控本体标签 (audio_events)
    pub fn tag_audio(&self, samples: &[f32], sample_rate: i32, top_k: usize) -> Vec<String> {
        let tagger = match &self.tagger {
            Some(t) => t,
            None => return Vec::new(),
        };

        // 防空张量与超短音频守卫（CED-mini patch_embed 卷积要求至少 0.1s 时域宽度，否则报 Invalid input shape）
        let min_samples = (sample_rate as f32 * 0.1) as usize;
        if samples.len() < min_samples {
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

    /// 端到端纯内存音频处理全量流水线：降噪 -> VAD -> ASR + 语言/情绪解析 + 声学打标
    pub fn process_audio_pipeline_full(
        &self,
        samples: &[f32],
        sample_rate: i32,
    ) -> (Option<SenseVoiceResult>, Vec<String>) {
        if samples.is_empty() {
            return (None, Vec::new());
        }

        // 1. 声学打标在原始或微降噪波形上提取事件 (CED-mini)
        let mut audio_events = self.tag_audio(samples, sample_rate, 5);

        // 2. 纯内存 GTCRN 降噪
        let denoised = self.denoise(samples, sample_rate);

        // 3. Silero VAD 防御性语音检测与截断
        let (has_voice, voice_samples) = self.vad_filter(&denoised, sample_rate);

        // 4. 若有有效语音，执行 SenseVoice ASR 完整解析
        let sense_voice_result = if has_voice {
            self.transcribe_full(&voice_samples, sample_rate)
        } else {
            None
        };

        // 5. 合并 SenseVoice 识别出的声音事件到 audio_events 列表
        if let Some(ref sv) = sense_voice_result {
            for ev in &sv.audio_events {
                let norm = normalize_ced_audio_event(ev);
                if !norm.is_empty() && !audio_events.contains(&norm) {
                    audio_events.push(norm);
                }
            }
        }

        (sense_voice_result, audio_events)
    }

    /// 端到端纯内存音频处理流水线：降噪 -> VAD -> ASR + 声学打标 (向后兼容)
    pub fn process_audio_pipeline(
        &self,
        samples: &[f32],
        sample_rate: i32,
    ) -> (Option<String>, Vec<String>) {
        let (res_opt, events) = self.process_audio_pipeline_full(samples, sample_rate);
        (
            res_opt
                .map(|r| r.text)
                .filter(|t| omni_core::is_meaningful_asr_text(t)),
            events,
        )
    }

    /// 检查音频净化所需关键深度学习模型是否就绪（GTCRN 降噪与 Silero VAD）
    pub fn is_purification_ready(&self) -> bool {
        self.denoiser.is_some() && self.vad_config.is_some()
    }

    /// 纯内存多级人声提纯与干声切片抽取 (GTCRN 降噪 + Silero VAD 伴奏截断 + 3.0s 门禁 + 5.0s 黄金区间切片)
    ///
    /// 【契约铁律】:
    /// 1. 显式降级治理 (AGENTS.md §2.9): 若 GTCRN 降噪模型或 Silero VAD 未初始化，严禁 Fail-Open 静默放行未降噪音频，必须返回清晰 Err；
    /// 2. 有效真人发音 3.0s 门禁: 若有效真人发音累计样本点不足 3 秒 (48,000 @ 16kHz)，直接返回明确错误，杜绝 Prompt 过短导致自回归自注意力崩溃；
    /// 3. 时域截断: 仅截取前 3.0s ~ 5.0s (最多 80,000 样本点 @ 16kHz)，确保下游生成的 RVQ-16 帧数严格锁定在 38 ~ 62 帧黄金区间。
    pub fn purify_vocal_and_extract_dry_stem(
        &self,
        samples_16k: &[f32],
        sample_rate: i32,
    ) -> Result<Vec<f32>, String> {
        if samples_16k.is_empty() {
            return Err("输入音频数据为空".to_string());
        }

        // 1. 显式降级治理 (AGENTS.md §2.9): 严禁 Fail-Open 静默放行原音频
        if self.denoiser.is_none() {
            tracing::error!("[SherpaManager] GTCRN 降噪模型未就绪，严禁静默放行未降噪音频");
            return Err("GTCRN 降噪模型未就绪".to_string());
        }
        if self.vad_config.is_none() {
            tracing::error!("[SherpaManager] Silero VAD 模型未就绪，严禁静默放行未过滤伴奏");
            return Err("Silero VAD 模型未就绪".to_string());
        }

        // 2. Stage 2: 纯内存 GTCRN 深度复数残差降噪
        let denoised = self.denoise(samples_16k, sample_rate);

        // 3. Stage 3: Silero VAD 截除伴奏前奏/间奏与静音
        let (has_voice, voice_samples) = self.vad_filter(&denoised, sample_rate);

        // 4. 有效真人发音 3.0s 门禁 (消除自回归注意力数学断裂)
        let min_voice_samples = (sample_rate as usize) * 3;
        if !has_voice || voice_samples.len() < min_voice_samples {
            return Err("真人有效语音时长不足（伴奏过长或有效发音不足 3 秒）".to_string());
        }

        // 5. 黄金发音区间截取 (前 3.0s ~ 5.0s，最多 80,000 样本点 @ 16kHz)
        let max_voice_samples = (sample_rate as usize) * 5;
        let final_stem = if voice_samples.len() > max_voice_samples {
            voice_samples[..max_voice_samples].to_vec()
        } else {
            voice_samples
        };

        Ok(final_stem)
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

/// 纯内存音频解码纯函数：将媒体转码解码为 48kHz 双声道交织 f32 样本 [L0, R0, L1, R1, ...]
pub fn decode_audio_to_48k_stereo(
    file_path: &Path,
    duration_seconds: Option<u32>,
) -> Option<Vec<f32>> {
    if !file_path.exists() {
        return None;
    }

    let ffmpeg = resolve_ffmpeg_binary()?;
    let mut cmd = std::process::Command::new(ffmpeg);
    cmd.arg("-i").arg(file_path);

    if let Some(dur) = duration_seconds {
        cmd.arg("-t").arg(dur.to_string());
    }

    // 强制输出 48000Hz 2 通道，f32le 原生 PCM，输出到标准管道
    cmd.args(["-vn", "-ar", "48000", "-ac", "2", "-f", "f32le", "pipe:1"]);

    let output = match cmd.output() {
        Ok(out) => out,
        Err(e) => {
            tracing::warn!("[decode_audio_to_48k_stereo] FFmpeg 管道执行失败: {:?}", e);
            return None;
        }
    };

    if !output.status.success() || output.stdout.is_empty() {
        tracing::warn!(
            "[decode_audio_to_48k_stereo] FFmpeg 转码非零退出或输出为空: status={:?}",
            output.status
        );
        return None;
    }

    let raw_bytes = output.stdout;
    let samples: Vec<f32> = raw_bytes
        .chunks_exact(4)
        .map(|chunk| f32::from_le_bytes(chunk.try_into().unwrap()))
        .collect();

    Some(samples)
}

/// 纯内存带通音频解码纯函数：专用于音色克隆与人声提纯前置预处理
///
/// 契约要求：
/// 1. 强制走常驻 FFmpeg 管道，注入带通滤波与单声道合规下混：
///    `-vn -af "highpass=f=80,lowpass=f=8000" -ar 16000 -ac 1 -f f32le pipe:1`
/// 2. 截除非人声超低频伴奏（低音鼓、贝斯、环境低频风噪）与超高频乐器泛音（金属镲片、高频杂音）；
/// 3. 原生 `-ac 1` 下混严格遵循 ITU-R BS.775 权重，保护 5.1/7.1 Center 通道人声对话不丢失；
/// 4. 音频处理全程纯内存管道流转，零临时文件落地。
pub fn decode_audio_to_16k_mono_bandpass(
    file_path: &Path,
    duration_seconds: Option<u32>,
) -> Option<Vec<f32>> {
    if !file_path.exists() {
        return None;
    }

    let ffmpeg = resolve_ffmpeg_binary()?;
    let mut cmd = std::process::Command::new(ffmpeg);
    cmd.arg("-i").arg(file_path);

    if let Some(dur) = duration_seconds {
        cmd.arg("-t").arg(dur.to_string());
    }

    cmd.args([
        "-vn",
        "-af",
        "highpass=f=80,lowpass=f=8000",
        "-ar",
        "16000",
        "-ac",
        "1",
        "-f",
        "f32le",
        "pipe:1",
    ]);

    let output = match cmd.output() {
        Ok(out) => out,
        Err(e) => {
            tracing::warn!("[decode_audio_to_16k_mono_bandpass] FFmpeg 管道执行失败: {:?}", e);
            return None;
        }
    };

    if !output.status.success() {
        tracing::warn!(
            "[decode_audio_to_16k_mono_bandpass] FFmpeg 转码退出状态码非 0: status={:?}, stderr={}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        );
        return None;
    }

    let raw_bytes = output.stdout;
    if raw_bytes.is_empty() {
        return Some(Vec::new());
    }

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

    /// ★ 守卫：随包 sherpa / onnxruntime 为 CPU-only 归档，Windows / Linux 上必须默认只给 cpu。
    /// 严禁再依据 `CUDA_PATH` / `CUDA_HOME` 等「本机装了 CUDA Toolkit」的信号盲推 cuda ——
    /// 那会触发上游 `session.cc:355` 的伪告警（`-DSHERPA_ONNX_ENABLE_GPU`）与虚假硬件加速归因。
    #[test]
    fn test_resolve_sherpa_providers_defaults_to_cpu_without_override() {
        let providers = SherpaManager::resolve_sherpa_providers(None);
        #[cfg(target_os = "macos")]
        assert_eq!(providers, vec!["coreml".to_string(), "cpu".to_string()]);
        #[cfg(not(target_os = "macos"))]
        assert_eq!(providers, vec!["cpu".to_string()]);
    }

    /// 显式覆盖（含大小写与首尾空白）必须被规整后置于首位，cpu 兜底恒在末位；
    /// 纯空白覆盖视为未设置，回落平台默认列表。
    #[test]
    fn test_resolve_sherpa_providers_explicit_override_wins() {
        assert_eq!(
            SherpaManager::resolve_sherpa_providers(Some("  CUDA  ")),
            vec!["cuda".to_string(), "cpu".to_string()]
        );
        assert_eq!(
            SherpaManager::resolve_sherpa_providers(Some("directml")),
            vec!["directml".to_string(), "cpu".to_string()]
        );
        assert_eq!(
            SherpaManager::resolve_sherpa_providers(Some("   ")),
            SherpaManager::resolve_sherpa_providers(None)
        );
    }

    /// `preferred_sherpa_providers()` 仅负责把 `FIREFLY_SHERPA_PROVIDER` 透传给纯函数，
    /// 且任何路径下末位都必须有 cpu 兜底，杜绝「无可用 EP 且无兜底」的构造失败。
    #[test]
    fn test_preferred_sherpa_providers_reads_env_override_with_cpu_fallback() {
        let expected = SherpaManager::resolve_sherpa_providers(
            std::env::var("FIREFLY_SHERPA_PROVIDER").ok().as_deref(),
        );
        assert_eq!(SherpaManager::preferred_sherpa_providers(), expected);
        assert_eq!(expected.last().map(String::as_str), Some("cpu"));
    }

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

    #[test]
    fn test_parse_sense_voice_tokens_filters_meaningless_text() {
        // 1. 单个点号 "."
        let res = parse_sense_voice_tokens("<|zh|><|NEUTRAL|><|Speech|>.");
        assert_eq!(res.text, "");
        assert!(res.lang.is_none());
        assert!(res.emotion.is_none());

        // 2. 连续省略号与空白组合 "..."
        let res = parse_sense_voice_tokens("<|en|><|HAPPY|><|Speech|>   ...  ");
        assert_eq!(res.text, "");
        assert!(res.lang.is_none());
        assert!(res.emotion.is_none());

        // 3. 中文全角标点 "。"
        let res = parse_sense_voice_tokens("<|zh|><|Speech|>。");
        assert_eq!(res.text, "");
        assert!(res.lang.is_none());

        // 4. 正常有效语音转录放行
        let res = parse_sense_voice_tokens("<|zh|><|NEUTRAL|><|Speech|>今天开会讨论了三季度预算。");
        assert_eq!(res.text, "今天开会讨论了三季度预算。");
        assert_eq!(res.lang.as_deref(), Some("zh"));
        assert_eq!(res.emotion.as_deref(), Some("neutral"));
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
    fn test_parse_sense_voice_tokens_and_fallbacks() {
        // 1. 中文带情感和事件
        let raw1 = "<|zh|><|HAPPY|><|Speech|><|woitn|><|Laughter|>你好世界，很高兴见到大家！<|/Laughter|>";
        let res1 = parse_sense_voice_tokens(raw1);
        assert_eq!(res1.lang.as_deref(), Some("zh"));
        assert_eq!(res1.emotion.as_deref(), Some("happy"));
        assert!(res1.audio_events.contains(&"laughter".to_string()));
        assert_eq!(res1.text, "你好世界，很高兴见到大家！");

        // 2. 英文带悲伤情绪与 BGM
        let raw2 = "<|en|><|SAD|><|BGM|>I am very sorry for the delay.<|/BGM|>";
        let res2 = parse_sense_voice_tokens(raw2);
        assert_eq!(res2.lang.as_deref(), Some("en"));
        assert_eq!(res2.emotion.as_deref(), Some("sad"));
        assert!(res2.audio_events.contains(&"bgm".to_string()));
        assert_eq!(res2.text, "I am very sorry for the delay.");

        // 3. 次级兜底轨：无声学语言标记，纯文本字符集判定中文
        let raw3 = "<|NEUTRAL|>今天天气真不错";
        let res3 = parse_sense_voice_tokens(raw3);
        assert_eq!(res3.lang.as_deref(), Some("zh"));
        assert_eq!(res3.emotion.as_deref(), Some("neutral"));
        assert_eq!(res3.text, "今天天气真不错");

        // 4. 次级兜底轨：纯英文无标记
        let raw4 = "Hello world from Rust tests";
        let res4 = parse_sense_voice_tokens(raw4);
        assert_eq!(res4.lang.as_deref(), Some("en"));
        assert!(res4.emotion.is_none());
        assert_eq!(res4.text, "Hello world from Rust tests");

        // 5. 日文假名判定
        let raw5 = "こんにちは世界";
        let res5 = parse_sense_voice_tokens(raw5);
        assert_eq!(res5.lang.as_deref(), Some("ja"));

        // 6. 空白输入
        let raw6 = "   <|nospeech|>   ";
        let res6 = parse_sense_voice_tokens(raw6);
        assert!(res6.text.is_empty());
        assert!(res6.lang.is_none());
    }
}
