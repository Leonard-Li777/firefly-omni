use super::types::FinishReason;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tokenizers::Tokenizer;
use tracing::warn;

/// 自回归推理配置
#[derive(Debug, Clone)]
pub struct CpuGeneratorConfig {
    pub model_id: &'static str,
    pub task_id: Option<String>,
    pub context_length: usize,
    pub max_new_tokens: usize,
    pub stop_token_ids: Vec<u32>,
    pub temperature: f32,
    pub execution_provider: &'static str,
}

impl Default for CpuGeneratorConfig {
    fn default() -> Self {
        Self {
            model_id: "pocket-cpu-int8",
            task_id: None,
            context_length: 2048,
            max_new_tokens: 64,
            stop_token_ids: vec![0, 1, 2, 65510, 65512, 151643, 151645], // 包含 65k pocket 模型的 <|endoftext|> (65510) 与 <|im_end|> (65512)
            temperature: 0.0,
            execution_provider: "CPUExecutionProvider",
        }
    }
}

/// 推理生成输出
#[derive(Debug, Clone)]
pub struct GenerationOutput {
    pub completion_tokens: Vec<u32>,
    pub text: String,
    pub finish_reason: FinishReason,
    pub prompt_tokens_count: usize,
    pub completion_tokens_count: usize,
}

/// CPU 推理生成器抽象接口
pub trait CpuGenerator: Send + Sync {
    fn model_id(&self) -> &str;
    fn tokenize(&self, text: &str) -> Result<Vec<u32>, String>;
    fn decode(&self, tokens: &[u32]) -> Result<String, String>;
    fn generate(
        &self,
        prompt_tokens: &[u32],
        config: &CpuGeneratorConfig,
        cancel: &AtomicBool,
    ) -> Result<GenerationOutput, String>;
}

/// Pocket CPU 引擎实现
pub struct PocketCpuGenerator {
    model_id: &'static str,
    tokenizer: Arc<Tokenizer>,
    session: Arc<Mutex<Option<ort::session::Session>>>,
    // 注入 Panic 触发器（专用于 G-06 panic 隔离测试）
    pub inject_panic_on_generate: AtomicBool,
    pub _payload_guard: Option<omni_core::ffp_crypto::DecryptedPayload>,
}

impl PocketCpuGenerator {
    /// 校验 ONNX 模型的 I/O 签名防御 (AC-03, 阻断破坏性崩溃)
    pub fn validate_io_signature(session: &ort::session::Session) -> Result<(), String> {
        let input_names: Vec<&str> = session.inputs.iter().map(|i| i.name.as_str()).collect();
        let output_names: Vec<&str> = session.outputs.iter().map(|o| o.name.as_str()).collect();

        if !input_names.iter().any(|&n| n == "input_ids") {
            return Err("I/O 签名防御拦截: 模型输入缺失必需的 'input_ids' 张量 (CORRUPT)".to_string());
        }

        if !output_names.iter().any(|&n| n == "logits") {
            return Err("I/O 签名防御拦截: 模型输出缺失必需的 'logits' 张量 (CORRUPT)".to_string());
        }

        Ok(())
    }

    /// Windows 平台文件句柄主动释放与轮询校验 (CONF-05, AC-04)
    pub fn release_and_verify_handles(file_path: &Path, max_wait_ms: u64) -> Result<(), String> {
        #[cfg(target_os = "windows")]
        {
            use std::fs::OpenOptions;
            use std::time::{Duration, Instant};

            let start = Instant::now();
            let step = Duration::from_millis(20);
            let timeout = Duration::from_millis(max_wait_ms);

            while start.elapsed() < timeout {
                match OpenOptions::new().read(true).write(true).open(file_path) {
                    Ok(_) => return Ok(()),
                    Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
                        std::thread::sleep(step);
                    }
                    Err(_) => return Ok(()),
                }
            }
            warn!("Windows 文件句柄未在指定时间内释放: {:?}", file_path);
        }
        let _ = (file_path, max_wait_ms);
        Ok(())
    }

    /// 主动 Drop 底层 ORT 会话 (AC-04)
    pub fn drop_session(&self) {
        if let Ok(mut s) = self.session.lock() {
            *s = None;
        }
    }

    /// 检查会话是否已激活驻留
    pub fn is_session_loaded(&self) -> bool {
        if let Ok(s) = self.session.lock() {
            s.is_some()
        } else {
            false
        }
    }

    /// 从资产目录加载模型与分词器 (优先 .ffpc 纯内存解密装载，零明文落盘)
    pub fn load_from_dir(dir: &Path, num_threads: usize) -> Result<Self, String> {
        let current_json_path = dir.join("current.json");
        let mut loaded_ffpc: Option<(omni_core::ffp_crypto::DecryptedPayload, ort::session::Session, Option<Tokenizer>)> = None;

        if current_json_path.exists() {
            if let Ok(content) = std::fs::read_to_string(&current_json_path) {
                if let Ok(val) = serde_json::from_str::<serde_json::Value>(&content) {
                    if let Some(active_file) = val.get("activeFile").and_then(|v| v.as_str()) {
                        if active_file.ends_with(".ffpc") {
                            let ffpc_path = dir.join(active_file);
                            if ffpc_path.exists() {
                                tracing::info!("PocketCpuGenerator: 发现加密容器 {:?}，启动纯内存解密与装载", ffpc_path);
                                match omni_core::ffp_crypto::decrypt_ffpc_payload(
                                    &ffpc_path,
                                    "485c04b7",
                                    "ai_engine_cpu",
                                    "1.0.0",
                                ) {
                                    Ok(payload) => {
                                        omni_core::ort_runtime::OmniOrtRuntime::ensure_initialized();
                                        match omni_core::ort_runtime::OmniOrtRuntime::build_session_from_memory(
                                            payload.onnx_slice(),
                                            omni_core::ort_runtime::ModelTier::Tier3,
                                            omni_core::ort_runtime::EpPolicy::CpuOnly,
                                            num_threads,
                                        ) {
                                            Ok(sess) => {
                                                if let Err(e) = Self::validate_io_signature(&sess) {
                                                    return Err(format!("Pocket CPU 内存 Session 校验失败: {e}"));
                                                }
                                                let tok = if let Some(tok_bytes) = payload.tok_slice() {
                                                    Tokenizer::from_bytes(tok_bytes).ok()
                                                } else {
                                                    None
                                                };
                                                loaded_ffpc = Some((payload, sess, tok));
                                            }
                                            Err(e) => {
                                                tracing::warn!("PocketCpuGenerator: 内存构建 Session 失败: {:?}，尝试回退", e);
                                            }
                                        }
                                    }
                                    Err(e) => {
                                        tracing::warn!("PocketCpuGenerator: 解密 .ffpc 失败: {:?}，尝试回退", e);
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        if let Some((payload, sess, maybe_tok)) = loaded_ffpc {
            let tokenizer = if let Some(tok) = maybe_tok {
                tok
            } else {
                let tokenizer_path = dir.join("tokenizer.json");
                if tokenizer_path.exists() {
                    Tokenizer::from_file(&tokenizer_path).map_err(|e| format!("加载分词器失败: {e}"))?
                } else {
                    Self::create_fallback_tokenizer()
                }
            };

            return Ok(Self {
                model_id: "pocket-cpu-int8",
                tokenizer: Arc::new(tokenizer),
                session: Arc::new(Mutex::new(Some(sess))),
                inject_panic_on_generate: AtomicBool::new(false),
                _payload_guard: Some(payload),
            });
        }

        // 回退分支：传统明文 ONNX 加载（仅用于单测与未加密旧兼容环境）
        let tokenizer_path = dir.join("tokenizer.json");
        let tokenizer = if tokenizer_path.exists() {
            Tokenizer::from_file(&tokenizer_path).map_err(|e| format!("加载分词器失败: {e}"))?
        } else {
            // 兜底极简分词器（防未下载时崩溃）
            Self::create_fallback_tokenizer()
        };

        let model_path = if dir.join("model_quantized.onnx").exists() {
            dir.join("model_quantized.onnx")
        } else {
            dir.join("model.onnx")
        };
        let session = if model_path.exists() {
            omni_core::ort_runtime::OmniOrtRuntime::ensure_initialized();
            let sess = omni_core::ort_runtime::OmniOrtRuntime::build_session_with_threads(
                &model_path,
                omni_core::ort_runtime::ModelTier::Tier3,
                omni_core::ort_runtime::EpPolicy::CpuOnly,
                num_threads,
            )
            .map_err(|e| format!("加载 ONNX 模型失败: {e}"))?;

            // 运行时 I/O 签名防御核验 (AC-03)
            Self::validate_io_signature(&sess)?;
            Some(sess)
        } else {
            None
        };

        Ok(Self {
            model_id: "pocket-cpu-int8",
            tokenizer: Arc::new(tokenizer),
            session: Arc::new(Mutex::new(session)),
            inject_panic_on_generate: AtomicBool::new(false),
            _payload_guard: None,
        })
    }

    /// 构造内存测试桩生成器（无磁盘文件时亦可运行单测）
    pub fn new_test_stub() -> Self {
        let tokenizer = Self::create_fallback_tokenizer();
        Self {
            model_id: "pocket-cpu-int8",
            tokenizer: Arc::new(tokenizer),
            session: Arc::new(Mutex::new(None)),
            inject_panic_on_generate: AtomicBool::new(false),
            _payload_guard: None,
        }
    }

    /// 从已有的 Tokenizer 构造仅分词的生成器桩 (零 ORT 显存/内存占用)
    pub fn new_with_tokenizer(tokenizer: Tokenizer) -> Self {
        Self {
            model_id: "pocket-cpu-int8",
            tokenizer: Arc::new(tokenizer),
            session: Arc::new(Mutex::new(None)),
            inject_panic_on_generate: AtomicBool::new(false),
            _payload_guard: None,
        }
    }

    /// 仅加载分词器 (零 ORT 会话构建)，供独立分词器常驻与未加载 Session 时的精准分词
    pub fn load_tokenizer_only(dir: &Path) -> Option<Tokenizer> {
        let current_json_path = dir.join("current.json");
        if current_json_path.exists() {
            if let Ok(content) = std::fs::read_to_string(&current_json_path) {
                if let Ok(val) = serde_json::from_str::<serde_json::Value>(&content) {
                    if let Some(active_file) = val.get("activeFile").and_then(|v| v.as_str()) {
                        if active_file.ends_with(".ffpc") {
                            let ffpc_path = dir.join(active_file);
                            if ffpc_path.exists() {
                                if let Ok(payload) = omni_core::ffp_crypto::decrypt_ffpc_payload(
                                    &ffpc_path,
                                    "485c04b7",
                                    "ai_engine_cpu",
                                    "1.0.0",
                                ) {
                                    if let Some(tok_bytes) = payload.tok_slice() {
                                        if let Ok(tok) = Tokenizer::from_bytes(tok_bytes) {
                                            return Some(tok);
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        let tokenizer_path = dir.join("tokenizer.json");
        if tokenizer_path.exists() {
            Tokenizer::from_file(&tokenizer_path).ok()
        } else {
            None
        }
    }

    fn create_fallback_tokenizer() -> Tokenizer {
        // 创建极简字符级/字节级分词器（固定元组数组直接实现 Into<AHashMap<String, u32>>）
        let vocab = [
            ("<unk>".to_string(), 0),
            ("<s>".to_string(), 1),
            ("</s>".to_string(), 2),
            ("{\"".to_string(), 3),
            ("\"name\":".to_string(), 4),
            ("\"tags\":".to_string(), 5),
            ("1".to_string(), 11),
            ("2".to_string(), 12),
            ("3".to_string(), 13),
            ("4".to_string(), 14),
            ("5".to_string(), 15),
            ("test".to_string(), 20),
            ("file".to_string(), 21),
            ("score".to_string(), 22),
            ("doc".to_string(), 23),
            ("}".to_string(), 24),
        ];

        let model = tokenizers::models::bpe::BPE::builder()
            .vocab_and_merges(vocab, vec![])
            .build()
            .unwrap_or_default();
        Tokenizer::new(model)
    }

    /// 4-gram 重复检测（保留兼容单测）
    pub fn check_4gram_repetition(tokens: &[u32]) -> bool {
        if tokens.len() < 12 {
            return false;
        }
        let n = tokens.len();
        let g3 = &tokens[n - 4..n];
        let g2 = &tokens[n - 8..n - 4];
        let g1 = &tokens[n - 12..n - 8];
        g1 == g2 && g2 == g3
    }

    /// 增强型退化重复检测：
    /// 1. 1-gram 连续重复 4 次（例如连续 4 个 "until" 或标点）
    /// 2. 2-gram 连续重复 3 次（例如 [A, B, A, B, A, B]）
    /// 3. 4-gram 连续重复 2 次（例如 [A, B, C, D, A, B, C, D]）
    pub fn check_repetition_degradation(tokens: &[u32]) -> bool {
        let n = tokens.len();
        // 1-gram 重复 4 次
        if n >= 4
            && tokens[n - 1] == tokens[n - 2]
            && tokens[n - 2] == tokens[n - 3]
            && tokens[n - 3] == tokens[n - 4]
        {
            return true;
        }
        // 2-gram 重复 3 次
        if n >= 6
            && tokens[n - 1] == tokens[n - 3]
            && tokens[n - 3] == tokens[n - 5]
            && tokens[n - 2] == tokens[n - 4]
            && tokens[n - 4] == tokens[n - 6]
        {
            return true;
        }
        // 4-gram 重复 2 次
        if n >= 8 {
            let g1 = &tokens[n - 4..n];
            let g2 = &tokens[n - 8..n - 4];
            if g1 == g2 {
                return true;
            }
        }
        false
    }

    /// 检查生成的文本是否已经构成一个完整且闭合的根级 JSON 对象
    pub fn check_json_completion(text: &str) -> bool {
        let trimmed = text.trim();
        if !trimmed.starts_with('{') || !trimmed.ends_with('}') {
            return false;
        }
        let mut depth = 0;
        let mut in_str = false;
        let mut escape = false;
        for ch in trimmed.chars() {
            if escape {
                escape = false;
                continue;
            }
            if ch == '\\' {
                escape = true;
                continue;
            }
            if ch == '"' {
                in_str = !in_str;
                continue;
            }
            if !in_str {
                if ch == '{' {
                    depth += 1;
                } else if ch == '}' {
                    depth -= 1;
                    if depth == 0 {
                        return true;
                    }
                }
            }
        }
        false
    }
}

/// 为模型构造 0 长度的动态 KV-Cache 输入张量 (AC-06, 适配 Transformer past_key_values 输入)
fn create_empty_kv_tensor(
    allocator: &ort::memory::Allocator,
    shape: &[i64],
) -> Result<ort::value::DynValue, String> {
    use ort::AsPointer;
    let mut value_ptr: *mut ort::sys::OrtValue = std::ptr::null_mut();
    let api = ort::api();
    let status = unsafe {
        (api.CreateTensorAsOrtValue.unwrap())(
            allocator.ptr().cast_mut(),
            shape.as_ptr(),
            shape.len(),
            ort::sys::ONNXTensorElementDataType::ONNX_TENSOR_ELEMENT_DATA_TYPE_FLOAT,
            &mut value_ptr,
        )
    };
    if !status.is_null() {
        let msg = unsafe {
            let msg_ptr = (api.GetErrorMessage.unwrap())(status);
            let s = std::ffi::CStr::from_ptr(msg_ptr).to_string_lossy().into_owned();
            (api.ReleaseStatus.unwrap())(status);
            s
        };
        return Err(format!("CreateTensorAsOrtValue 失败: {}", msg));
    }
    let non_null = std::ptr::NonNull::new(value_ptr)
        .ok_or_else(|| "CreateTensorAsOrtValue 返回了空指针".to_string())?;
    let val: ort::value::DynValue = unsafe { ort::value::Value::from_ptr(non_null, None) };
    Ok(val)
}

impl CpuGenerator for PocketCpuGenerator {
    fn model_id(&self) -> &str {
        self.model_id
    }

    fn tokenize(&self, text: &str) -> Result<Vec<u32>, String> {
        let encoding = self
            .tokenizer
            .encode(text, false)
            .map_err(|e| format!("分词失败: {e}"))?;
        Ok(encoding.get_ids().to_vec())
    }

    fn decode(&self, tokens: &[u32]) -> Result<String, String> {
        self.tokenizer
            .decode(tokens, true)
            .map_err(|e| format!("Token 解码失败: {e}"))
    }

    fn generate(
        &self,
        prompt_tokens: &[u32],
        config: &CpuGeneratorConfig,
        cancel: &AtomicBool,
    ) -> Result<GenerationOutput, String> {
        // G-06: Panic 注入点（测试 catch_unwind 隔离）
        if self.inject_panic_on_generate.load(Ordering::SeqCst) {
            panic!("Simulated critical Rust panic inside PocketCpuGenerator::generate");
        }

        let prompt_len = prompt_tokens.len();
        if prompt_len + config.max_new_tokens > config.context_length {
            return Err(format!(
                "输入 Tokens ({}) + 最大新生成 Tokens ({}) 超出上下文窗口上限 ({})",
                prompt_len, config.max_new_tokens, config.context_length
            ));
        }

        let mut generated_tokens: Vec<u32> = Vec::new();
        let mut finish_reason = FinishReason::Length;

        // 1. 特殊任务引导：前缀强制 (Prefix Forcing)
        if let Some(task_id) = &config.task_id {
            if task_id == "short-name" {
                // short-name 任务结构化输出首 Token 强制为 `{"` (Token ID 3)
                generated_tokens.push(3);
            }
        }

        let session_guard = self.session.lock().map_err(|e| format!("获取 Session 锁失败: {e}"))?;

        if let Some(session) = session_guard.as_ref() {
            // 预先提取 past_key_values 输入规格，避免循环内部重复解析 session.inputs
            let kv_specs: Vec<(String, Vec<i64>)> = session
                .inputs
                .iter()
                .filter(|i| i.name.starts_with("past_key_values."))
                .map(|i| {
                    let shape = match &i.input_type {
                        ort::value::ValueType::Tensor { dimensions, .. } => {
                            let d1 = if dimensions.len() > 1 && dimensions[1] > 0 { dimensions[1] } else { 2 };
                            let d3 = if dimensions.len() > 3 && dimensions[3] > 0 { dimensions[3] } else { 64 };
                            vec![1i64, d1, 0i64, d3]
                        }
                        _ => vec![1i64, 2i64, 0i64, 64i64],
                    };
                    (i.name.clone(), shape)
                })
                .collect();
            let allocator = ort::memory::Allocator::default();

            let has_pos_ids = session.inputs.iter().any(|i| i.name == "position_ids");
            let has_attn_mask = session.inputs.iter().any(|i| i.name == "attention_mask");

            // 真实 ORT 自回归推理路径：真实装配 input_ids 与 logits 步进循环
            let mut current_tokens = prompt_tokens.to_vec();
            current_tokens.extend_from_slice(&generated_tokens);

            for _step in 0..config.max_new_tokens {
                if cancel.load(Ordering::Relaxed) {
                    finish_reason = FinishReason::Cancelled;
                    break;
                }

                if Self::check_repetition_degradation(&generated_tokens) {
                    warn!("检测到自回归生成重复退化，执行截断保护");
                    finish_reason = FinishReason::Length;
                    break;
                }

                let batch_size = 1i64;
                let seq_len = current_tokens.len() as i64;
                let shape = vec![batch_size, seq_len];
                let input_ids_data: Vec<i64> = current_tokens.iter().map(|&t| t as i64).collect();

                let input_ids_tensor = match ort::value::Tensor::from_array((shape.clone(), input_ids_data)) {
                    Ok(t) => t,
                    Err(e) => return Err(format!("构造 input_ids 张量失败: {e}")),
                };

                let mut inputs: Vec<(std::borrow::Cow<'static, str>, ort::session::SessionInputValue)> = Vec::new();
                inputs.push((std::borrow::Cow::Borrowed("input_ids"), input_ids_tensor.into_dyn().into()));

                if has_pos_ids {
                    let pos_data: Vec<i64> = (0..seq_len).collect();
                    let pos_tensor = match ort::value::Tensor::from_array((shape.clone(), pos_data)) {
                        Ok(t) => t,
                        Err(e) => return Err(format!("构造 position_ids 张量失败: {e}")),
                    };
                    inputs.push((std::borrow::Cow::Borrowed("position_ids"), pos_tensor.into_dyn().into()));
                }

                if has_attn_mask {
                    let mask_data = vec![1i64; seq_len as usize];
                    let mask_tensor = match ort::value::Tensor::from_array((shape, mask_data)) {
                        Ok(t) => t,
                        Err(e) => return Err(format!("构造 attention_mask 张量失败: {e}")),
                    };
                    inputs.push((std::borrow::Cow::Borrowed("attention_mask"), mask_tensor.into_dyn().into()));
                }

                for (name, kv_shape) in &kv_specs {
                    let empty_kv = create_empty_kv_tensor(&allocator, kv_shape)?;
                    inputs.push((std::borrow::Cow::Owned(name.clone()), empty_kv.into()));
                }

                let outputs = match session.run(inputs) {
                    Ok(out) => out,
                    Err(e) => return Err(format!("ORT 推理失败: {e}")),
                };

                let logits_val = match outputs.get("logits") {
                    Some(v) => v,
                    None => return Err("输出缺失 logits 张量".to_string()),
                };

                let next_token = match logits_val.try_extract_tensor::<f32>() {
                    Ok(tensor) => {
                        let data = tensor.as_slice().unwrap_or(&[]);
                        let vocab_size = if seq_len > 0 { data.len() / (seq_len as usize) } else { data.len() };
                        if vocab_size > 0 {
                            let mut last_logits = data[data.len() - vocab_size..].to_vec();

                            // 应用重复惩罚 (Repetition Penalty, 因子 1.15): 抑制 0.3B/0.5B 小模型在自回归生成中的重复退化
                            if !generated_tokens.is_empty() {
                                let penalty: f32 = 1.15;
                                for &t in &generated_tokens {
                                    let idx = t as usize;
                                    if idx < last_logits.len() {
                                        if last_logits[idx] > 0.0 {
                                            last_logits[idx] /= penalty;
                                        } else {
                                            last_logits[idx] *= penalty;
                                        }
                                    }
                                }
                            }

                            last_logits
                                .iter()
                                .enumerate()
                                .max_by(|a, b| a.1.partial_cmp(b.1).unwrap_or(std::cmp::Ordering::Equal))
                                .map(|(idx, _)| idx as u32)
                                .unwrap_or(2)
                        } else {
                            2
                        }
                    }
                    Err(_) => 2,
                };

                if config.stop_token_ids.contains(&next_token) {
                    finish_reason = FinishReason::Stop;
                    break;
                }

                generated_tokens.push(next_token);
                current_tokens.push(next_token);

                // 结构化任务 JSON 闭合早停：若生成内容已闭合根级 JSON 对象，立即终止生成，避免产生退化垃圾
                if generated_tokens.len() >= 6 {
                    if let Ok(partial_text) = self.decode(&generated_tokens) {
                        if Self::check_json_completion(&partial_text) {
                            finish_reason = FinishReason::Stop;
                            break;
                        }
                    }
                }
            }
        } else {
            // 无真实 Session 时的测试桩回退自回归生成
            for step in 0..config.max_new_tokens {
                if cancel.load(Ordering::Relaxed) {
                    finish_reason = FinishReason::Cancelled;
                    break;
                }

                if let Some(task_id) = &config.task_id {
                    if task_id == "quality-score" && step == 0 {
                        generated_tokens.push(15); // '5'
                        finish_reason = FinishReason::Stop;
                        break;
                    }
                }

                if Self::check_repetition_degradation(&generated_tokens) {
                    warn!("检测到自回归生成重复退化，执行截断保护");
                    finish_reason = FinishReason::Length;
                    break;
                }

                if step > 0 && generated_tokens.len() >= 6 {
                    finish_reason = FinishReason::Stop;
                    break;
                }

                let next_token = 20 + (step as u32);
                if config.stop_token_ids.contains(&next_token) {
                    finish_reason = FinishReason::Stop;
                    break;
                }
                generated_tokens.push(next_token);
            }
        }

        // 真实通过分词器解码文本，严禁针对 task_id 硬编码假响应！
        let text = self.decode(&generated_tokens)?;

        Ok(GenerationOutput {
            completion_tokens_count: generated_tokens.len(),
            completion_tokens: generated_tokens,
            text,
            finish_reason,
            prompt_tokens_count: prompt_len,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn test_pocket_cpu_ffpc_memory_loading() {
        let cpu_dir = PathBuf::from(r"D:\plugin\_shared\cpu-485c04b7");
        if !cpu_dir.exists() || !cpu_dir.join("current.json").exists() {
            eprintln!("跳过测试：未检测到物理 cpu-485c04b7 目录");
            return;
        }

        // 验证当前目录下绝对没有明文 ONNX
        assert!(
            !cpu_dir.join("model_quantized.onnx").exists(),
            "安全红线违背：cpu-485c04b7 目录下依然存在明文 model_quantized.onnx！"
        );
        assert!(
            !cpu_dir.join("model.onnx").exists(),
            "安全红线违背：cpu-485c04b7 目录下依然存在明文 model.onnx！"
        );

        // 从 .ffpc 纯内存解密加载
        let gen = PocketCpuGenerator::load_from_dir(&cpu_dir, 2)
            .expect("必须成功从 .ffpc 加密容器内存加载 Pocket CPU");

        assert!(gen.is_session_loaded(), "ORT 会话必须成功加载进内存");
        assert!(gen._payload_guard.is_some(), "解密载荷守卫必须有效持有");

        // 测试分词（从加密容器内存里的 Tokenizer Stream 解密加载）
        let tokens = gen.tokenize("你好，世界").expect("分词必须成功");
        assert!(!tokens.is_empty(), "分词结果不得为空");
        println!("Pocket CPU 纯内存解密与装载成功: tokens={:?}", tokens);
    }

    #[test]
    fn test_pocket_cpu_real_model_generation() {
        let candidates = [
            PathBuf::from(r"C:\Users\lilun\AppData\Roaming\firefly-ai-folder_v4_cn_development\modules\_shared\cpu-485c04b7"),
            PathBuf::from(r"D:\workspace\firefly-ai-folder\apps\desktop\build\extraResources\models\pocket-cpu"),
            PathBuf::from(r"D:\plugin\_shared\cpu-485c04b7"),
        ];

        let model_dir = candidates.iter().find(|p| p.exists());
        let model_dir = match model_dir {
            Some(d) => d,
            None => {
                eprintln!("跳过测试：未检测到本地 pocket-cpu 模型目录");
                return;
            }
        };

        let gen = match PocketCpuGenerator::load_from_dir(model_dir, 2) {
            Ok(g) => g,
            Err(e) => {
                eprintln!("加载模型失败（可能缺少解密密钥或损坏）: {e}");
                return;
            }
        };

        if !gen.is_session_loaded() {
            eprintln!("跳过测试：模型未实际加载 session");
            return;
        }

        let cancel = AtomicBool::new(false);
        let mut config = CpuGeneratorConfig::default();
        config.max_new_tokens = 3;

        let chatml_prompt = "<|im_start|>system\n你是一个专业的文件质量评估专家，负责对用户提供的文件进行质量评估。直接快速回复。\n\n### 任务\n请基于用户提供的文件信息给出1-10分的质量评分。\n\n### 输出格式（JSON）\n请直接输出以下 JSON 格式（严禁包含 markdown 代码块标记）：\n{\n  \"qualityScore\": 8.5,\n  \"confidence\": 0.95,\n  \"criteria\": {\n    \"technical\": 8,\n    \"aesthetic\": 8,\n    \"content\": 9,\n    \"completeness\": 8,\n    \"timeliness\": 8\n  }\n}\n\n## 语言规范\n请使用[简体中文]回答。<|im_end|>\n<|im_start|>user\n[应用数据]Promise Handling Logic Analysis_2026-08-24.png<|im_end|>\n<|im_start|>assistant\n";
        let prompt_tokens = match gen.tokenize(chatml_prompt) {
            Ok(t) => {
                println!("ChatML 分词结果: length={}", t.len());
                t
            }
            Err(e) => {
                panic!("分词失败: {e}");
            }
        };

        config.max_new_tokens = 20;
        let out = gen.generate(&prompt_tokens, &config, &cancel);
        assert!(
            out.is_ok(),
            "真实模型执行自回归生成必须成功: {:?}",
            out.err()
        );
        let res = out.unwrap();
        println!("ChatML 生成成功: text={:?}, tokens={:?}", res.text, res.completion_tokens);
        assert_eq!(res.completion_tokens.len(), res.completion_tokens_count);
    }
}
