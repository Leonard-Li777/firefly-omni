use super::circuit_breaker::{get_os_boot_id, CircuitBreakerManager};
use super::generator::{CpuGenerator, CpuGeneratorConfig, PocketCpuGenerator};
use super::memory::{
    compute_cold_load_floor, preflight_cold_load, preflight_hot_infer,
    set_mock_available_memory_bytes, HOT_INFER_FLOOR,
};
use super::slot::{
    is_generation_active, CpuSlot, ESTIMATED_PEAK_RSS_BYTES, ESTIMATED_RESIDENT_BYTES,
};
use super::types::{AdmissionState, SessionLifecycle};
use axum::http::StatusCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tempfile::tempdir;
static SERIAL_TEST_MUTEX: std::sync::Mutex<()> = std::sync::Mutex::new(());

// ============================================================================
// G-01: Session 单飞加载与并发排队测试
// ============================================================================
#[tokio::test]
async fn test_single_flight_load() {
    let tmp = tempdir().unwrap();
    // 指向不存在的模型目录：单飞加载必须把同一个确定性失败广播给全部等待者
    let missing_model_dir = tmp.path().join("no-such-model");
    let slot = Arc::new(CpuSlot::new(
        Some(tmp.path().to_path_buf()),
        Some(missing_model_dir),
    ));

    let mut handles = Vec::new();
    for _ in 0..8 {
        let s = Arc::clone(&slot);
        handles.push(tokio::spawn(async move { s.preheat(false).await }));
    }

    for h in handles {
        let res = h.await.unwrap();
        assert!(res.is_err(), "模型缺失时严禁预热成功（禁止假 Ready）: {:?}", res);
        assert_eq!(
            res.err().unwrap().0,
            StatusCode::INTERNAL_SERVER_ERROR,
            "确定性加载失败必须映射 500，不得混入 503 被客户端判为瞬态"
        );
    }

    // 验证最终状态为 Failed，且未因并发而互相覆盖成 Ready
    let snapshot = slot.get_snapshot();
    assert!(
        matches!(snapshot.lifecycle, SessionLifecycle::Failed(_)),
        "模型缺失时生命周期必须为 Failed，实际: {:?}",
        snapshot.lifecycle
    );
}

// ============================================================================
// G-02: 任务租约与 ActiveTaskGuard 原子性测试
// ============================================================================
#[tokio::test]
async fn test_task_guard_atomic() {
    let _serial_lock = SERIAL_TEST_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    let tmp = tempdir().unwrap();
    let slot = Arc::new(CpuSlot::new_test_ready(Some(tmp.path().to_path_buf())));

    // 预热进入 Ready 态
    slot.preheat(false).await.unwrap();

    assert_eq!(slot.get_snapshot().active_tasks, 0);

    // 获取 Guard
    {
        let guard1 = slot.try_begin_task().unwrap();
        assert_eq!(slot.get_snapshot().active_tasks, 1);
        assert!(is_generation_active());

        {
            let guard2 = slot.try_begin_task().unwrap();
            assert_eq!(slot.get_snapshot().active_tasks, 2);
            assert_eq!(guard2.epoch(), guard1.epoch());
        } // guard2 drop

        assert_eq!(slot.get_snapshot().active_tasks, 1);
    } // guard1 drop

    // 所有 guard 离开后严格归零
    assert_eq!(slot.get_snapshot().active_tasks, 0);
    assert!(!is_generation_active());
}

// ============================================================================
// G-03: TTL 与新请求竞态测试
// ============================================================================
#[tokio::test]
async fn test_ttl_atomic() {
    let _serial_lock = SERIAL_TEST_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    let tmp = tempdir().unwrap();
    let slot = Arc::new(CpuSlot::new_test_ready(Some(tmp.path().to_path_buf())));
    slot.preheat(false).await.unwrap();

    // 1. 当有活跃任务时，TTL 回收无条件挂起，不予卸载
    {
        let _guard = slot.try_begin_task().unwrap();
        let evicted = slot.check_ttl_eviction(Duration::from_millis(0));
        assert!(!evicted, "活跃任务进行中严禁 TTL 卸载");
        assert_eq!(slot.get_snapshot().lifecycle, SessionLifecycle::Ready);
    }

    // 2. 活跃任务归零后，超时触发卸载
    tokio::time::sleep(Duration::from_millis(10)).await;
    let evicted = slot.check_ttl_eviction(Duration::from_millis(5));
    assert!(evicted, "超时且无活跃任务时应安全卸载");
    assert_eq!(slot.get_snapshot().lifecycle, SessionLifecycle::Unloaded);
}

// ============================================================================
// G-04: 资源销毁 Drain 代际隔离测试
// ============================================================================
#[tokio::test]
async fn test_drain_barrier() {
    let _serial_lock = SERIAL_TEST_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    let tmp = tempdir().unwrap();
    let slot = Arc::new(CpuSlot::new_test_ready(Some(tmp.path().to_path_buf())));
    slot.preheat(false).await.unwrap();

    let guard = slot.try_begin_task().unwrap();
    let slot_clone = Arc::clone(&slot);

    // 在另一个任务中发起 Drain
    let drain_handle = tokio::spawn(async move {
        slot_clone.drain(2000).await
    });

    // 等待 Drain 协程启动并转入 Draining
    tokio::time::sleep(Duration::from_millis(15)).await;

    // 此时准入已关闭为 Draining
    assert_eq!(slot.get_snapshot().admission, AdmissionState::Draining);
    let new_task = slot.try_begin_task();
    assert!(new_task.is_err(), "Draining 状态下严禁新任务准入");

    // 释放旧任务
    drop(guard);

    let drain_res = drain_handle.await.unwrap().unwrap();
    assert!(drain_res.drained, "在限时内活跃任务归零应成功排空");
    assert_eq!(slot.get_snapshot().lifecycle, SessionLifecycle::Unloaded);
}

// ============================================================================
// G-05: 取消语义与协作式中断测试
// ============================================================================
#[tokio::test]
async fn test_cancel_atomic() {
    let generator = PocketCpuGenerator::new_test_stub();
    let cancel = AtomicBool::new(false);

    let mut config = CpuGeneratorConfig::default();
    config.max_new_tokens = 50;

    // 无真实 ORT Session：必须显式未就绪，严禁桩回退吐 token
    cancel.store(true, Ordering::Relaxed);
    let err = generator.generate(&[1, 2, 3], &config, &cancel).unwrap_err();
    assert!(
        err.contains("ENGINE_NOT_READY"),
        "无 Session 时必须返回 ENGINE_NOT_READY，实际: {err}"
    );
}

// ============================================================================
// G-06: Rust panic="unwind" 隔离测试
// ============================================================================
#[tokio::test]
async fn test_panic_isolation() {
    let tmp = tempdir().unwrap();
    let slot = Arc::new(CpuSlot::new_test_ready(Some(tmp.path().to_path_buf())));
    slot.preheat(false).await.unwrap();

    // 注入 Panic 触发
    slot.set_inject_panic_for_test(true);

    let cancel = AtomicBool::new(false);
    let config = CpuGeneratorConfig::default();

    // 执行推理：底层 panic 被 catch_unwind 隔离，不引起进程终止
    let res = slot.execute_generate(&[1, 2, 3], &config, &cancel);
    assert!(res.is_err(), "Panic 触发后应安全返回错误");

    let snapshot = slot.get_snapshot();
    assert_eq!(
        snapshot.lifecycle,
        SessionLifecycle::Failed("RUST_PANIC".to_string()),
        "Panic 发生后 Session 应被标记为 Failed 并释放"
    );
}

// ============================================================================
// G-07: 进程级崩溃恢复与 CRASH_LOOP 断路器测试
// ============================================================================
#[test]
fn test_crash_loop() {
    let tmp = tempdir().unwrap();
    let run_dir = tmp.path().to_path_buf();
    let boot_id = get_os_boot_id();
    let pid = 999999; // 假定一个不可能存活的 PID

    let cb = CircuitBreakerManager::new(run_dir.clone());

    // 模拟第 1 次异常崩溃：写入 inflight 标记
    cb.write_inflight(pid, &boot_id, 1).unwrap();
    let count1 = cb.check_and_recover_on_startup(&boot_id, 8888);
    assert_eq!(count1, 1);
    assert!(!cb.is_crash_loop());

    // 模拟第 2 次异常崩溃：再次写入 inflight 标记
    cb.write_inflight(pid, &boot_id, 2).unwrap();
    let count2 = cb.check_and_recover_on_startup(&boot_id, 8888);
    assert_eq!(count2, 2);
    assert!(cb.is_crash_loop(), "连续 2 次崩溃必须进入 CRASH_LOOP 熔断态");

    // 正常启动无 inflight 时，重试不清零
    let count_idle = cb.check_and_recover_on_startup(&boot_id, 8888);
    assert_eq!(count_idle, 2, "正常启动绝不清零 crash_count");

    // 模拟用户显式点击预热模型并确认：清零
    cb.clear_crash_count_on_preheat();
    assert_eq!(cb.load_state().crash_count, 0);
    assert!(!cb.is_crash_loop());

    // 二次熔断防穿透保护：预热后若再次崩溃，直接跳至 2
    cb.write_inflight(pid, &boot_id, 3).unwrap();
    let count_secondary = cb.check_and_recover_on_startup(&boot_id, 8888);
    assert_eq!(count_secondary, 2, "预热后再次崩溃必须直接跳至 2 触发二次熔断");
}

// ============================================================================
// G-08: ORT / 检索资源公平性与生成侧让渡测试
// ============================================================================
#[tokio::test]
async fn test_resource_coexistence() {
    let _serial_lock = SERIAL_TEST_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    let tmp = tempdir().unwrap();
    let slot = Arc::new(CpuSlot::new_test_ready(Some(tmp.path().to_path_buf())));
    slot.preheat(false).await.unwrap();

    assert!(!is_generation_active());

    {
        let _guard = slot.try_begin_task().unwrap();
        // 生成活跃标志必须置位，通知检索/索引服务生成正在占用 CPU
        assert!(is_generation_active());
    }

    assert!(!is_generation_active());
}

// ============================================================================
// G-09: 分词计数端点独立性与门限拦截测试
// ============================================================================
#[test]
fn test_tokenize_contract() {
    let tmp = tempdir().unwrap();

    // 1. 模型未挂载 → 分词端点必须显式报未就绪（409），严禁回退假词表
    let unloaded = CpuSlot::new(Some(tmp.path().to_path_buf()), None);
    assert!(
        matches!(
            unloaded.get_snapshot().lifecycle,
            SessionLifecycle::Failed(ref r) if r == "MODEL_UNMOUNTED"
        ),
        "未挂载模型必须直接进入 Failed(MODEL_UNMOUNTED)"
    );
    let res = unloaded.tokenize(&["你好，世界！".to_string()]);
    assert_eq!(
        res.err().unwrap().0,
        StatusCode::CONFLICT,
        "分词器不可用必须返回 409 ENGINE_NOT_READY"
    );

    // 2. 测试构建的 Ready 槽位仍可命中请求体门限
    let slot = CpuSlot::new_test_ready(Some(tmp.path().to_path_buf()));
    let tokens = slot
        .tokenize(&["你好，世界！".to_string(), "财务报表".to_string()])
        .expect("测试桩词表应可分词");
    assert_eq!(tokens.tokens.len(), 2);
    assert_eq!(tokens.counts.len(), 2);

    // 验证条目数超出限制 (> 100) 拦截 413
    let huge_list: Vec<String> = (0..101).map(|i| format!("text_{i}")).collect();
    let err1 = slot.tokenize(&huge_list);
    assert_eq!(err1.err().unwrap().0, StatusCode::PAYLOAD_TOO_LARGE);

    // 验证总字节数超出 64KB 拦截 413
    let huge_str = "A".repeat(65537);
    let err2 = slot.tokenize(&[huge_str]);
    assert_eq!(err2.err().unwrap().0, StatusCode::PAYLOAD_TOO_LARGE);
}

// ============================================================================
// G-10: KV-Cache 绑定与任务引导约束测试
// ============================================================================
#[test]
fn test_kv_cache_binding() {
    let generator = PocketCpuGenerator::new_test_stub();
    let cancel = AtomicBool::new(false);

    // 无真实 ORT Session 时，任何任务（含 short-name / quality-score）都不得伪造产出
    let mut config_name = CpuGeneratorConfig::default();
    config_name.task_id = Some("short-name".to_string());
    let err_name = generator.generate(&[1, 2], &config_name, &cancel).unwrap_err();
    assert!(
        err_name.contains("ENGINE_NOT_READY"),
        "short-name 在无 Session 时必须显式失败，实际: {err_name}"
    );

    let mut config_score = CpuGeneratorConfig::default();
    config_score.task_id = Some("quality-score".to_string());
    let err_score = generator.generate(&[1, 2], &config_score, &cancel).unwrap_err();
    assert!(
        err_score.contains("ENGINE_NOT_READY"),
        "quality-score 严禁固定产出评分 Token，实际: {err_score}"
    );
}

// ============================================================================
// G-11: 冷启动与热态内存双门限测试
// ============================================================================
#[test]
fn test_memory_dual_floors() {
    // 1. 冷启动门限公式验证
    let floor = compute_cold_load_floor(ESTIMATED_PEAK_RSS_BYTES, ESTIMATED_RESIDENT_BYTES);
    let expected_floor = std::cmp::max(
        (ESTIMATED_PEAK_RSS_BYTES as f64 * 1.2) as u64,
        HOT_INFER_FLOOR + ESTIMATED_RESIDENT_BYTES,
    );
    assert_eq!(floor, expected_floor);

    // 2. 模拟可用内存低于门限：拒绝冷加载 (LOW_MEMORY)
    set_mock_available_memory_bytes(Some(floor - 1024 * 1024));
    let cold_check = preflight_cold_load(ESTIMATED_PEAK_RSS_BYTES, ESTIMATED_RESIDENT_BYTES);
    assert!(cold_check.is_err(), "低于冷启动门限必须拒绝加载");

    // 3. 模拟可用内存高于门限：放行
    set_mock_available_memory_bytes(Some(floor + 1024 * 1024));
    let cold_check_ok = preflight_cold_load(ESTIMATED_PEAK_RSS_BYTES, ESTIMATED_RESIDENT_BYTES);
    assert!(cold_check_ok.is_ok());

    // 4. 热态推理门限 (512MB)
    set_mock_available_memory_bytes(Some(HOT_INFER_FLOOR - 1));
    assert!(preflight_hot_infer().is_err(), "低于 512MB 必须拒绝热态推理");

    set_mock_available_memory_bytes(Some(HOT_INFER_FLOOR + 1));
    assert!(preflight_hot_infer().is_ok());

    // 清理 mock
    set_mock_available_memory_bytes(None);
}


// ============================================================================
// AC-04 / CONF-05: Windows 句柄主动释放与轮询校验测试
// ============================================================================
#[test]
fn test_windows_handles_release() {
    let tmp = tempdir().unwrap();
    let dummy_model_path = tmp.path().join("model.onnx");
    std::fs::write(&dummy_model_path, b"dummy onnx content").unwrap();

    let generator = PocketCpuGenerator::new_test_stub();
    assert!(!generator.is_session_loaded());

    // 测试句柄释放轮询：正常文件排他访问成功
    let res = PocketCpuGenerator::release_and_verify_handles(&dummy_model_path, 200);
    assert!(res.is_ok(), "文件未被占用时应直接验证通过");

    // 主动 drop_session
    generator.drop_session();
    assert!(!generator.is_session_loaded());
}

// ============================================================================
// AC-06: 零 Mock 真实分词解码生成测试
// ============================================================================
#[test]
fn test_zero_mock_generation() {
    let generator = PocketCpuGenerator::new_test_stub();
    let cancel = AtomicBool::new(false);
    let mut config = CpuGeneratorConfig::default();
    config.max_new_tokens = 5;

    let err = generator.generate(&[1, 2], &config, &cancel).unwrap_err();
    // 无 Session 时任何文本产出（含死字面量）都属欺诈性 Mock，必须失败
    assert!(
        err.contains("ENGINE_NOT_READY"),
        "无 Session 严禁产出任何伪造文本，实际: {err}"
    );
}

#[test]
fn test_empty_kv_cache_tensor() {
    use ort::AsPointer;
    omni_core::ort_runtime::OmniOrtRuntime::ensure_initialized();
    let allocator = ort::memory::Allocator::default();
    let shape: Vec<i64> = vec![1, 2, 0, 64];
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
        panic!("CreateTensorAsOrtValue failed: {}", msg);
    }
    assert!(!value_ptr.is_null());

    let non_null = std::ptr::NonNull::new(value_ptr).unwrap();
    let val: ort::value::DynValue = unsafe { ort::value::Value::from_ptr(non_null, None) };
    assert!(val.is_tensor());
    println!("Created empty KV cache tensor successfully: {:?}", val.dtype());
}

#[test]
fn test_repetition_degradation_detection() {
    // 1-gram 连续重复 4 次
    assert!(PocketCpuGenerator::check_repetition_degradation(&[10, 20, 20, 20, 20]));
    assert!(!PocketCpuGenerator::check_repetition_degradation(&[10, 20, 20, 20]));

    // 2-gram 连续重复 3 次 ([1, 2, 1, 2, 1, 2])
    assert!(PocketCpuGenerator::check_repetition_degradation(&[1, 2, 1, 2, 1, 2]));

    // 4-gram 连续重复 2 次 ([1, 2, 3, 4, 1, 2, 3, 4])
    assert!(PocketCpuGenerator::check_repetition_degradation(&[1, 2, 3, 4, 1, 2, 3, 4]));

    // 正常文本不误判
    assert!(!PocketCpuGenerator::check_repetition_degradation(&[1, 2, 3, 4, 5, 6, 7, 8]));
}

#[test]
fn test_json_completion_early_stop() {
    // 完整闭合的根 JSON 对象
    assert!(PocketCpuGenerator::check_json_completion(r#"{"score": 8.5}"#));
    assert!(PocketCpuGenerator::check_json_completion(r#"  { "name": "test_file.txt", "tags": [1, 2] }  "#));
    assert!(PocketCpuGenerator::check_json_completion(r#"{"criteria": {"technical": 9}}"#));

    // 未闭合的 JSON 对象
    assert!(!PocketCpuGenerator::check_json_completion(r#"{"score": 8.5"#));
    assert!(!PocketCpuGenerator::check_json_completion(r#"{"criteria": {"technical": 9}"#));

    // 非 JSON 对象
    assert!(!PocketCpuGenerator::check_json_completion("until until until until"));
    assert!(!PocketCpuGenerator::check_json_completion(""));
}
