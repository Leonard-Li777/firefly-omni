use super::active_guard::ActiveTaskGuard;
use super::circuit_breaker::{get_default_run_dir, get_os_boot_id, CircuitBreakerManager};
use super::generator::{CpuGenerator, CpuGeneratorConfig, GenerationOutput, PocketCpuGenerator};
use super::memory::{preflight_cold_load, preflight_hot_infer};
use super::types::{
    AdmissionState, CpuSessionSnapshot, SessionDrainResponse,
    SessionLifecycle, SessionPreheatResponse, TokenizeResponse,
};
use axum::http::StatusCode;
use std::panic::AssertUnwindSafe;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::sync::{broadcast, Notify};
use tracing::{error, info, warn};

/// 全局生成活跃计数（核心生命线检索侧读取此计数：> 0 时生成侧主动让出 CPU）
pub static GLOBAL_GENERATION_ACTIVE: AtomicUsize = AtomicUsize::new(0);

/// 查询当前是否处于生成式活跃状态
pub fn is_generation_active() -> bool {
    GLOBAL_GENERATION_ACTIVE.load(Ordering::Relaxed) > 0
}

/// 单飞加载等待队列上限 (8 个等待请求)
pub const MAX_WAIT_QUEUE_CAPACITY: usize = 8;

/// 单会话预估内存开销（~380MB 驻留，峰值 ~450MB）
pub const ESTIMATED_RESIDENT_BYTES: u64 = 380 * 1024 * 1024;
pub const ESTIMATED_PEAK_RSS_BYTES: u64 = 450 * 1024 * 1024;

pub struct CpuSlot {
    lifecycle: Arc<RwLock<SessionLifecycle>>,
    admission: Arc<RwLock<AdmissionState>>,
    active_tasks: Arc<AtomicUsize>,
    session_epoch: Arc<AtomicU64>,
    last_active_at: Arc<RwLock<Instant>>,
    last_active_timestamp: Arc<AtomicU64>,
    notify_drain: Arc<Notify>,
    circuit_breaker: Arc<CircuitBreakerManager>,
    generator: Arc<RwLock<Option<Arc<dyn CpuGenerator>>>>,
    // 独立常驻分词器（保证 POST /v1/tokenize 绝不触发模型加载，P99 <= 5ms）
    standalone_tokenizer: Arc<PocketCpuGenerator>,
    // 单飞加载信道与并发排队计数
    load_tx: Arc<Mutex<Option<broadcast::Sender<Result<(), String>>>>>,
    wait_queue_count: Arc<AtomicUsize>,
    model_dir: Option<PathBuf>,
    pub inject_panic: AtomicBool,
}

impl CpuSlot {
    pub fn new(run_dir: Option<PathBuf>, model_dir: Option<PathBuf>) -> Self {
        let run_dir = run_dir.unwrap_or_else(get_default_run_dir);
        let circuit_breaker = Arc::new(CircuitBreakerManager::new(run_dir));
        let boot_id = get_os_boot_id();
        let pid = std::process::id();
        
        // 启动时崩溃标记交叉核验与对账
        let crash_count = circuit_breaker.check_and_recover_on_startup(&boot_id, pid);
        let initial_lifecycle = if crash_count >= 2 {
            warn!("启动检查：检测到底层连续崩溃 2 次，CPU 会话直接锁定为 Failed(CRASH_LOOP)");
            SessionLifecycle::Failed("CRASH_LOOP".to_string())
        } else {
            SessionLifecycle::Unloaded
        };

        let now_sec = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);

        Self {
            lifecycle: Arc::new(RwLock::new(initial_lifecycle)),
            admission: Arc::new(RwLock::new(AdmissionState::Accepting)),
            active_tasks: Arc::new(AtomicUsize::new(0)),
            session_epoch: Arc::new(AtomicU64::new(1)),
            last_active_at: Arc::new(RwLock::new(Instant::now())),
            last_active_timestamp: Arc::new(AtomicU64::new(now_sec)),
            notify_drain: Arc::new(Notify::new()),
            circuit_breaker,
            generator: Arc::new(RwLock::new(None)),
            standalone_tokenizer: Arc::new(PocketCpuGenerator::new_test_stub()),
            load_tx: Arc::new(Mutex::new(None)),
            wait_queue_count: Arc::new(AtomicUsize::new(0)),
            model_dir,
            inject_panic: AtomicBool::new(false),
        }
    }

    /// 获取状态快照（GET /v1/health 消费）
    pub fn get_snapshot(&self) -> CpuSessionSnapshot {
        let lifecycle = self.lifecycle.read().unwrap().clone();
        let admission = *self.admission.read().unwrap();
        let active_tasks = self.active_tasks.load(Ordering::SeqCst);
        let session_epoch = self.session_epoch.load(Ordering::SeqCst);
        let last_active = self.last_active_timestamp.load(Ordering::Relaxed);
        let crash_count = self.circuit_breaker.load_state().crash_count;
        let is_loaded = matches!(lifecycle, SessionLifecycle::Ready);

        CpuSessionSnapshot {
            lifecycle,
            admission,
            model_id: "pocket-cpu-int8".to_string(),
            resident_bytes: if is_loaded { ESTIMATED_RESIDENT_BYTES } else { 0 },
            peak_rss_bytes: if is_loaded { ESTIMATED_PEAK_RSS_BYTES } else { 0 },
            active_tasks,
            last_active_timestamp: last_active,
            crash_count,
            session_epoch,
        }
    }

    /// 尝试准入任务并获取租约守卫
    pub fn try_begin_task(&self) -> Result<ActiveTaskGuard, (StatusCode, &'static str)> {
        let admission = *self.admission.read().unwrap();
        if admission != AdmissionState::Accepting {
            return Err((StatusCode::SERVICE_UNAVAILABLE, "ENGINE_NOT_READY"));
        }

        let lifecycle = self.lifecycle.read().unwrap().clone();
        match lifecycle {
            SessionLifecycle::Ready => {
                // 热态推理内存门限核验 (512MB)
                if let Err(_) = preflight_hot_infer() {
                    return Err((StatusCode::SERVICE_UNAVAILABLE, "LOW_MEMORY"));
                }
                self.touch_active();
                let epoch = self.session_epoch.load(Ordering::SeqCst);
                Ok(ActiveTaskGuard::new(
                    Arc::clone(&self.active_tasks),
                    Arc::clone(&self.notify_drain),
                    epoch,
                ))
            }
            SessionLifecycle::Failed(ref reason) => {
                if reason == "CRASH_LOOP" {
                    Err((StatusCode::INTERNAL_SERVER_ERROR, "ENGINE_CRASHED"))
                } else {
                    Err((StatusCode::INTERNAL_SERVER_ERROR, "ENGINE_FAILED"))
                }
            }
            _ => Err((StatusCode::SERVICE_UNAVAILABLE, "MODEL_UNLOADED")),
        }
    }

    fn touch_active(&self) {
        if let Ok(mut t) = self.last_active_at.write() {
            *t = Instant::now();
        }
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        self.last_active_timestamp.store(now, Ordering::Relaxed);
    }

    /// 显式预热或单飞按需加载 (POST /v1/engines/cpu/session)
    pub async fn preheat(&self, reset_crash: bool) -> Result<SessionPreheatResponse, (StatusCode, String)> {
        // 1. 如果处于 CRASH_LOOP，只有显式重置才允许解除熔断
        if self.circuit_breaker.is_crash_loop() {
            if reset_crash {
                self.circuit_breaker.clear_crash_count_on_preheat();
                *self.lifecycle.write().unwrap() = SessionLifecycle::Unloaded;
            } else {
                return Err((
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "引擎处于 CRASH_LOOP 熔断状态；必须确认重置后方可预热".to_string(),
                ));
            }
        }

        // 2. 检查当前生命周期
        {
            let current = self.lifecycle.read().unwrap().clone();
            if let SessionLifecycle::Ready = current {
                return Ok(SessionPreheatResponse {
                    ok: true,
                    lifecycle: SessionLifecycle::Ready,
                    admission: *self.admission.read().unwrap(),
                    resident_bytes: ESTIMATED_RESIDENT_BYTES,
                    model_id: "pocket-cpu-int8".to_string(),
                });
            }
        }

        // 3. 单飞排队机制 (Single-Flight Load)
        let mut rx = {
            let mut guard = self.load_tx.lock().unwrap();
            if let Some(ref tx) = *guard {
                // 队列容量满载保护 (超过 8 个等待者拒绝并返回 QUEUE_FULL)
                let current_waiters = self.wait_queue_count.fetch_add(1, Ordering::SeqCst);
                if current_waiters >= MAX_WAIT_QUEUE_CAPACITY {
                    self.wait_queue_count.fetch_sub(1, Ordering::SeqCst);
                    return Err((StatusCode::TOO_MANY_REQUESTS, "QUEUE_FULL".to_string()));
                }
                Some(tx.subscribe())
            } else {
                // 当前是首个请求，发起加载
                let (tx, _rx) = broadcast::channel(16);
                *guard = Some(tx);
                None
            }
        };

        if let Some(ref mut receiver) = rx {
            // 排队等待首个加载完成
            let result = receiver.recv().await;
            self.wait_queue_count.fetch_sub(1, Ordering::SeqCst);
            match result {
                Ok(Ok(())) => {
                    return Ok(SessionPreheatResponse {
                        ok: true,
                        lifecycle: SessionLifecycle::Ready,
                        admission: *self.admission.read().unwrap(),
                        resident_bytes: ESTIMATED_RESIDENT_BYTES,
                        model_id: "pocket-cpu-int8".to_string(),
                    });
                }
                Ok(Err(err)) => return Err((StatusCode::SERVICE_UNAVAILABLE, err)),
                Err(_) => {
                    return Err((
                        StatusCode::SERVICE_UNAVAILABLE,
                        "等待模型加载时信道中断".to_string(),
                    ))
                }
            }
        }

        // 4. 执行底层实际加载流程
        let load_res = self.perform_actual_load().await;

        // 广播通知等待队列并清理 sender
        {
            let mut guard = self.load_tx.lock().unwrap();
            if let Some(tx) = guard.take() {
                let _ = tx.send(load_res.clone());
            }
        }

        match load_res {
            Ok(()) => Ok(SessionPreheatResponse {
                ok: true,
                lifecycle: SessionLifecycle::Ready,
                admission: *self.admission.read().unwrap(),
                resident_bytes: ESTIMATED_RESIDENT_BYTES,
                model_id: "pocket-cpu-int8".to_string(),
            }),
            Err(e) => Err((StatusCode::SERVICE_UNAVAILABLE, e)),
        }
    }

    async fn perform_actual_load(&self) -> Result<(), String> {
        // 冷启动内存门限核验
        if let Err((avail, floor)) = preflight_cold_load(ESTIMATED_PEAK_RSS_BYTES, ESTIMATED_RESIDENT_BYTES) {
            let msg = format!("可用物理内存不足 (当前: {}MB, 门限: {}MB)", avail / 1024 / 1024, floor / 1024 / 1024);
            *self.lifecycle.write().unwrap() = SessionLifecycle::Failed("LOW_MEMORY".to_string());
            return Err(msg);
        }

        *self.lifecycle.write().unwrap() = SessionLifecycle::Loading;
        let epoch = self.session_epoch.fetch_add(1, Ordering::SeqCst) + 1;
        let boot_id = get_os_boot_id();
        let pid = std::process::id();

        // 写入 .inflight 崩溃标记
        let _ = self.circuit_breaker.write_inflight(pid, &boot_id, epoch);

        // 加载模型与分词器 (受控 1~4 线程)
        let cpus = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4);
        let num_threads = std::cmp::min(std::cmp::max(cpus.saturating_sub(1), 1), 4);
        let gen: Arc<dyn CpuGenerator> = if let Some(ref dir) = self.model_dir {
            match PocketCpuGenerator::load_from_dir(dir, num_threads) {
                Ok(g) => Arc::new(g),
                Err(e) => {
                    self.circuit_breaker.remove_inflight();
                    *self.lifecycle.write().unwrap() = SessionLifecycle::Failed(e.clone());
                    return Err(e);
                }
            }
        } else {
            // 测试模式桩代码
            Arc::new(PocketCpuGenerator::new_test_stub())
        };

        *self.generator.write().unwrap() = Some(gen);
        *self.lifecycle.write().unwrap() = SessionLifecycle::Ready;
        *self.admission.write().unwrap() = AdmissionState::Accepting;
        self.touch_active();
        info!("Pocket CPU Session 已成功进入 Ready 状态 (epoch: {})", epoch);
        Ok(())
    }

    /// 资源销毁与排空 (DELETE /v1/engines/cpu/session?wait_drain_ms=N)
    pub async fn drain(&self, wait_drain_ms: u64) -> Result<SessionDrainResponse, (StatusCode, String)> {
        // 1. 关闭准入，转入 Draining
        *self.admission.write().unwrap() = AdmissionState::Draining;
        *self.lifecycle.write().unwrap() = SessionLifecycle::Draining;

        // 2. 等待活跃任务清零或超时
        let start = Instant::now();
        let timeout = Duration::from_millis(wait_drain_ms);
        let mut drained = false;

        loop {
            if self.active_tasks.load(Ordering::SeqCst) == 0 {
                drained = true;
                break;
            }
            if start.elapsed() >= timeout {
                warn!("Drain 排空超时 ({}ms)，当前仍有 {} 个任务在途", wait_drain_ms, self.active_tasks.load(Ordering::SeqCst));
                break;
            }
            tokio::select! {
                _ = self.notify_drain.notified() => {},
                _ = tokio::time::sleep(Duration::from_millis(50)) => {}
            }
        }

        // 3. 释放底层资源并删除 inflight
        *self.generator.write().unwrap() = None;
        *self.lifecycle.write().unwrap() = SessionLifecycle::Unloaded;
        *self.admission.write().unwrap() = AdmissionState::Closed;
        self.circuit_breaker.remove_inflight();
        self.session_epoch.fetch_add(1, Ordering::SeqCst);

        Ok(SessionDrainResponse {
            ok: true,
            lifecycle: SessionLifecycle::Unloaded,
            drained,
            released_bytes: ESTIMATED_RESIDENT_BYTES,
        })
    }

    /// TTL 自动回收检查：active_tasks == 0 && Accepting && 超时 -> 安全卸载
    pub fn check_ttl_eviction(&self, timeout: Duration) -> bool {
        if self.active_tasks.load(Ordering::SeqCst) > 0 {
            return false;
        }
        if *self.admission.read().unwrap() != AdmissionState::Accepting {
            return false;
        }
        if !matches!(*self.lifecycle.read().unwrap(), SessionLifecycle::Ready) {
            return false;
        }

        let elapsed = self.last_active_at.read().unwrap().elapsed();
        if elapsed >= timeout {
            info!("CPU Session 空闲超出 TTL ({:?})，执行自动卸载释放内存", timeout);
            *self.generator.write().unwrap() = None;
            *self.lifecycle.write().unwrap() = SessionLifecycle::Unloaded;
            self.circuit_breaker.remove_inflight();
            self.session_epoch.fetch_add(1, Ordering::SeqCst);
            true
        } else {
            false
        }
    }

    /// 独立分词处理 (POST /v1/tokenize)：常驻轻量分词器，绝不隐式加载模型
    pub fn tokenize(&self, texts: &[String]) -> Result<TokenizeResponse, (StatusCode, String)> {
        // 请求体大小硬门限：条目 <= 100，总字符数 <= 65536
        if texts.len() > 100 {
            return Err((StatusCode::PAYLOAD_TOO_LARGE, "分词文本条目数超出限制 (最大 100)".to_string()));
        }
        let total_chars: usize = texts.iter().map(|s| s.len()).sum();
        if total_chars > 64 * 1024 {
            return Err((StatusCode::PAYLOAD_TOO_LARGE, "分词文本总长度超出 64KB 上限".to_string()));
        }

        let mut all_tokens = Vec::with_capacity(texts.len());
        let mut counts = Vec::with_capacity(texts.len());
        let mut total_tokens = 0;

        for text in texts {
            let tokens = self
                .standalone_tokenizer
                .tokenize(text)
                .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;
            total_tokens += tokens.len();
            counts.push(tokens.len());
            all_tokens.push(tokens);
        }

        Ok(TokenizeResponse {
            tokens: all_tokens,
            counts,
            total_tokens,
        })
    }

    /// 对话生成执行 (带 catch_unwind 隔离)
    pub fn execute_generate(
        &self,
        prompt_tokens: &[u32],
        config: &CpuGeneratorConfig,
        cancel: &std::sync::atomic::AtomicBool,
    ) -> Result<GenerationOutput, (StatusCode, String)> {
        let gen = {
            let guard = self.generator.read().unwrap();
            guard.as_ref().cloned()
        };

        let generator = match gen {
            Some(g) => g,
            None => return Err((StatusCode::SERVICE_UNAVAILABLE, "MODEL_UNLOADED".to_string())),
        };

        // G-06: 强制使用 std::panic::catch_unwind 进行 Panic 隔离
        let result = std::panic::catch_unwind(AssertUnwindSafe(|| {
            if self.inject_panic.load(Ordering::SeqCst) {
                panic!("Simulated critical Rust panic inside execute_generate");
            }
            generator.generate(prompt_tokens, config, cancel)
        }));

        match result {
            Ok(Ok(output)) => {
                self.touch_active();
                Ok(output)
            }
            Ok(Err(err)) => Err((StatusCode::BAD_REQUEST, err)),
            Err(panic_err) => {
                error!("捕获到底层算子 Rust Panic，正在隔离释放 Session: {:?}", panic_err);
                // 彻底释放 Slot
                *self.generator.write().unwrap() = None;
                *self.lifecycle.write().unwrap() = SessionLifecycle::Failed("RUST_PANIC".to_string());
                self.circuit_breaker.remove_inflight();
                Err((
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "底层推理算子发生 Panic，已安全隔离".to_string(),
                ))
            }
        }
    }

    /// 注入 Panic 触发（测试专用）
    pub fn set_inject_panic_for_test(&self, inject: bool) {
        self.inject_panic.store(inject, Ordering::SeqCst);
    }
}
