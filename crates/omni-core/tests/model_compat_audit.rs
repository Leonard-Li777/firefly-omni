//! 工单 06 / T7：全量随包 ONNX 模型在运行时 B 纯 CPU Session 下的兼容性普查。
//!
//! **为什么需要**：运行时 B 是 1.24.4 DirectML 构建，其支持的 ORT opset 上限为 23
//! （SPEC §1 第 4 条）。若后续新增模型带更高 opset，或带 `allowzero=1` 这类 DML
//! 不支持的语义，问题只会在真机首帧推理时才暴露（构建期静默切分回 CPU、运行期
//! `E_INVALIDARG`），代价是整批伪向量入库。本测试把「能不能加载」前移为入库门禁。
//!
//! **执行口径**：遍历 `extraResources/models` 下全部 `.onnx`，在运行时 B 的
//! **纯 CPU Session**（不挂任何 EP）上逐一 `commit_from_file`，并断言默认域
//! opset ≤ 23。刻意不用生产 `build_session`：那会把 DML allowlist 卷进来，
//! 与「模型本身是否可加载」这一被测命题解耦（T1/T9 覆盖 EP 分档）。
//!
//! **运行方式**（默认 `#[ignore]`，需真实模型资产）：
//! ```text
//! cargo test -p omni-core --test model_compat_audit -- --ignored --nocapture
//! ```
//! 环境变量：
//! - `OMNI_MODELS_DIR`：指定模型根目录（缺省从 CWD 向上查找
//!   `apps/desktop/build/extraResources/models`）；**设了却不是目录会直接报错**，不静默回退；
//! - `OMNI_ORT_DLL`：显式指定运行时 B 的 DLL（缺省取模型根目录同级 `bin/omni/onnxruntime_dml.dll`）；
//! - `OMNI_T7_STRICT=1`：**入库门禁必须开启**。任何「命题不成立」（无资产 / DLL 未装配 /
//!   独立模型数不足）都改为 panic。缺省宽松只为不让无资产环境误红，但宽松模式下一次
//!   「跳过」在 CI 报告里与「通过」无法区分。

use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::Instant;

/// 运行时 B（DirectML 1.24.4 构建）支持的最高 ORT opset
const MAX_SUPPORTED_OPSET: i64 = 23;

/// **独立模型**数量下限（按内容采样去重后计数，别名副本不计）。
///
/// 这是本测试自设的资产完整度闸，不来自 SPEC 的任何硬性指标 —— 它的作用是在
/// 「目录指错 / 资产只拷了一半 / 别名副本被删」时立刻红，而不是让一次残缺普查
/// 静默通过并冒充「全量兼容性结论」。当前随包实测：12 个 `.onnx`，去重后 10 个
/// 独立模型（`vision_model.onnx` == `vision_encoder_q4.onnx`、
/// `model.onnx` == `model_q4.onnx` 两组同内容别名）。
const MIN_EXPECTED_MODELS: usize = 10;

/* ------------------------------------------------------------------ *
 * 最小 ONNX protobuf 解码：只取 ModelProto.opset_import（字段 8）
 *
 * 只为读一个 repeated 子消息而引入 `prost` + 生成代码不划算（且会与
 * ort/ort-sys 的依赖图抢版本），故按 protobuf wire format 手写最小解码：
 * 顶层跳过非 8 号字段，8 号内只认 domain(1, string) 与 version(2, varint)。
 * ------------------------------------------------------------------ */

/// 读取 varint（LEB128），返回 `(值, 新偏移)`
fn read_varint(buf: &[u8], pos: &mut usize) -> Option<u64> {
    let mut result: u64 = 0;
    let mut shift = 0u32;
    loop {
        let byte = *buf.get(*pos)?;
        *pos += 1;
        // 64 位最多 10 字节；第 10 字节（shift=63）只允许最低 1 位有效，
        // 超出即为 >64 位数值，必须判错而不是静默截断
        if shift == 63 && (byte & 0x7f) > 1 {
            return None;
        }
        if shift >= 64 {
            return None;
        }
        result |= u64::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return Some(result);
        }
        shift += 7;
    }
}

/// 按 wire type 跳过一个字段（返回新偏移）；未知 wire type 视为解码失败
fn skip_field(buf: &[u8], pos: &mut usize, wire_type: u64) -> Option<usize> {
    match wire_type {
        0 => {
            read_varint(buf, pos)?;
        }
        1 => {
            *pos = pos.checked_add(8)?;
        }
        2 => {
            let len = read_varint(buf, pos)? as usize;
            *pos = pos.checked_add(len)?;
        }
        5 => {
            *pos = pos.checked_add(4)?;
        }
        _ => return None,
    }
    if *pos > buf.len() {
        return None;
    }
    Some(*pos)
}

/// 解析 `ModelProto.opset_import`：返回 `(domain, version)` 列表
///
/// 默认域在 ONNX 里既可写空串也可写 `ai.onnx`，此处原样返回，由调用方归一。
fn parse_opset_imports(model_bytes: &[u8]) -> Option<Vec<(String, i64)>> {
    let mut out: Vec<(String, i64)> = Vec::new();
    let mut pos = 0usize;
    while pos < model_bytes.len() {
        let tag = read_varint(model_bytes, &mut pos)?;
        let field_number = tag >> 3;
        let wire_type = tag & 0x7;
        if field_number == 8 && wire_type == 2 {
            let len = read_varint(model_bytes, &mut pos)? as usize;
            let end = pos.checked_add(len)?;
            if end > model_bytes.len() {
                return None;
            }
            let msg = &model_bytes[pos..end];
            pos = end;

            let mut domain = String::new();
            let mut version: i64 = 0;
            let mut sub = 0usize;
            while sub < msg.len() {
                let sub_tag = read_varint(msg, &mut sub)?;
                let sub_field = sub_tag >> 3;
                let sub_wire = sub_tag & 0x7;
                match (sub_field, sub_wire) {
                    (1, 2) => {
                        let dlen = read_varint(msg, &mut sub)? as usize;
                        let dend = sub.checked_add(dlen)?;
                        if dend > msg.len() {
                            return None;
                        }
                        domain = String::from_utf8_lossy(&msg[sub..dend]).to_string();
                        sub = dend;
                    }
                    (2, 0) => {
                        version = read_varint(msg, &mut sub)? as i64;
                    }
                    _ => {
                        skip_field(msg, &mut sub, sub_wire)?;
                    }
                }
            }
            out.push((domain, version));
        } else {
            skip_field(model_bytes, &mut pos, wire_type)?;
        }
    }
    Some(out)
}

/// 默认域（空串 / `ai.onnx`）的 opset 版本
fn default_domain_opset(opsets: &[(String, i64)]) -> Option<i64> {
    opsets
        .iter()
        .find(|(d, _)| d.is_empty() || d == "ai.onnx")
        .map(|(_, v)| *v)
}

/* ------------------------------------------------------------------ *
 * 模型发现
 * ------------------------------------------------------------------ */

/// 定位随包模型根目录：`OMNI_MODELS_DIR` 优先，其次从 CWD 向上查找
///
/// 显式设置 `OMNI_MODELS_DIR` 却不是目录时返回 `Err`（而不是静默回退）：
/// 显式覆盖写错属于调用方错误，静默回退到自动发现会让人以为「普查跑过了」。
fn models_root() -> Result<PathBuf, String> {
    if let Ok(dir) = std::env::var("OMNI_MODELS_DIR") {
        let p = PathBuf::from(&dir);
        if p.is_dir() {
            return Ok(p);
        }
        return Err(format!("OMNI_MODELS_DIR={dir} 不是目录（显式覆盖写错，拒绝静默回退）"));
    }
    let mut cur = std::env::current_dir().map_err(|e| format!("取当前目录失败: {e}"))?;
    for _ in 0..8 {
        let cand = cur.join("apps/desktop/build/extraResources/models");
        if cand.is_dir() {
            return Ok(cand);
        }
        let cand = cur.join("build/extraResources/models");
        if cand.is_dir() {
            return Ok(cand);
        }
        cur = cur.parent().ok_or("已到根目录仍未找到")?.to_path_buf();
    }
    Err("向上 8 层未找到 apps/desktop/build/extraResources/models".to_string())
}

/// 递归收集 `*.onnx`（排序保证输出稳定，便于人工比对两轮普查结果）
///
/// 读目录失败不再静默忽略：旧实现 `let Ok(entries) = ... else { return }` 会让
/// 「整个子目录不可读」表现为「该目录本来就没有模型」，最终以「数量不足」的形式报错，
/// 归因指向错误的层次。此处把错误原文交给调用方汇总。
fn collect_onnx(dir: &Path, out: &mut Vec<PathBuf>, errors: &mut Vec<String>) {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) => {
            errors.push(format!("读取目录失败 {}: {e}", dir.display()));
            return;
        }
    };
    let mut subdirs: Vec<PathBuf> = Vec::new();
    for entry in entries {
        let entry = match entry {
            Ok(e) => e,
            Err(e) => {
                errors.push(format!("遍历目录项失败 {}: {e}", dir.display()));
                continue;
            }
        };
        let path = entry.path();
        if path.is_dir() {
            subdirs.push(path);
        } else if path
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("onnx"))
        {
            out.push(path);
        }
    }
    subdirs.sort();
    for sub in subdirs {
        collect_onnx(&sub, out, errors);
    }
}

/* ------------------------------------------------------------------ *
 * 独立模型去重
 * ------------------------------------------------------------------ */

/// FNV-1a（64 位），`seed` 允许串接多段采样
fn fnv1a(bytes: &[u8], seed: u64) -> u64 {
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut h = seed;
    for b in bytes {
        h ^= u64::from(*b);
        h = h.wrapping_mul(PRIME);
    }
    h
}

/// 内容采样摘要：`(文件长度, 头 64KB + 尾 64KB 的 FNV-1a)`。
///
/// 为什么不全量哈希：随包模型合计约 260MB，为去重完整读一遍不划算。
/// 为什么不能只按长度：两个内容不同的模型恰好等长时会被误判成别名副本，
/// 独立模型数少算一个 ⇒ 断言误红。头部是 protobuf 元数据（含 opset / 图结构名），
/// 尾部是权重字节，二者组合足以区分同长异构模型。
fn sampled_digest(path: &Path) -> Option<(u64, u64)> {
    const SAMPLE: u64 = 64 * 1024;
    const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;

    let len = std::fs::metadata(path).ok()?.len();
    let mut file = std::fs::File::open(path).ok()?;
    let mut buf = vec![0u8; SAMPLE as usize];

    let head_n = file.read(&mut buf).ok()?;
    let mut h = fnv1a(&buf[..head_n], FNV_OFFSET);

    if len > SAMPLE {
        file.seek(SeekFrom::Start(len - SAMPLE)).ok()?;
        let tail_n = file.read(&mut buf).ok()?;
        h = fnv1a(&buf[..tail_n], h);
    }
    Some((len, h))
}

/* ------------------------------------------------------------------ *
 * T7 主测试
 * ------------------------------------------------------------------ */

#[test]
#[ignore = "需真实随包模型资产（extraResources/models）；无资产环境请显式 --ignored 运行"]
fn test_t7_all_bundled_models_build_on_runtime_b_cpu() {
    // 严格模式（`OMNI_T7_STRICT=1`）：任何「命题不成立」的早退一律改为 panic。
    // 缺省宽松是为了不让无资产环境误红，但那样一次「跳过」在 CI 报告里与「通过」无法区分，
    // 故入库门禁必须显式开启本开关。
    let strict = std::env::var("OMNI_T7_STRICT").is_ok();
    let skip_or_fail = |reason: String| {
        if strict {
            panic!("[T7] strict 模式下命题不成立: {reason}");
        }
        println!("[T7] {reason} ⇒ 跳过（非兼容性结论；设 OMNI_T7_STRICT=1 可改为硬失败）");
    };

    let root = match models_root() {
        Ok(r) => r,
        Err(e) => {
            skip_or_fail(format!("未定位到随包模型目录: {e}"));
            return;
        }
    };
    println!("[T7] 模型根目录: {}", root.display());

    // 运行时 B 的 DLL **由模型根目录的同级 `bin/omni` 直接推导**，不走生产发现链。
    //
    // 原因：发现链会向上/向 exe 同级查找，在 `target/debug` 里常能命中 ort 自己复制出来的
    // `onnxruntime.dll`（运行时 A）。于是「资产其实装了 DML，但发现链先撞上 A」会被误判为
    // 「发现链优先级缺陷」—— 那是测试进程的目录布局问题，与被测命题（模型能否被 1.24.4 加载）
    // 无关，报错归因会指错层。显式取 `extraResources/bin/omni/onnxruntime_dml.dll` 后，
    // T7 的结论只取决于「资产是否装配」。
    //
    // `OMNI_ORT_DLL` 仍可显式覆盖（排障用），但此时若指到运行时 A，属调用方选择 ⇒ 只跳过。
    let explicit_dll = std::env::var("OMNI_ORT_DLL").ok().map(PathBuf::from);
    let dll = match explicit_dll.clone() {
        Some(p) => p,
        None => {
            let cand = root
                .parent()
                .map(|p| p.join("bin/omni/onnxruntime_dml.dll"))
                .unwrap_or_default();
            if !cand.exists() {
                skip_or_fail(format!(
                    "未装配运行时 B（期望路径不存在）: {}",
                    cand.display()
                ));
                return;
            }
            cand
        }
    };
    let is_runtime_b = dll
        .file_name()
        .map(|n| n.to_string_lossy().to_ascii_lowercase().contains("dml"))
        .unwrap_or(false);
    if !is_runtime_b {
        let how = if explicit_dll.is_some() {
            "由 OMNI_ORT_DLL 显式指定（调用方排障选择）"
        } else {
            "非 dml 命名（资产命名不符约定）"
        };
        skip_or_fail(format!(
            "当前 DLL {} {how}，运行时 B 未被选中",
            dll.display()
        ));
        return;
    }
    println!("[T7] 运行时 B: {}", dll.display());
    if let Err(e) = omni_core::ort_runtime::OmniOrtRuntime::init_with_dll(
        dll.to_str().unwrap_or("onnxruntime.dll"),
    ) {
        // 绑定失败是环境问题（不是模型兼容性结论），必须显式报错而不是静默跳过
        panic!("[T7] 运行时 B 绑定失败: {e}");
    }

    let mut models: Vec<PathBuf> = Vec::new();
    let mut scan_errors: Vec<String> = Vec::new();
    collect_onnx(&root, &mut models, &mut scan_errors);
    models.sort();
    let total = models.len();
    println!("[T7] 发现 {total} 个 .onnx 模型");
    // 随包目录存在「同内容别名」（vision_model.onnx == vision_encoder_q4.onnx、
    // model.onnx == model_q4.onnx）：`total` 会把别名副本算成两个模型，
    // 于是「6 个真模型 + 4 个别名」也能凑够 10 ⇒ 断言必须落在**独立模型数**上。
    let mut digests: Vec<(u64, u64)> = Vec::new();
    for p in &models {
        match sampled_digest(p) {
            Some(d) => digests.push(d),
            None => scan_errors.push(format!("无法采样摘要（读取失败）: {}", p.display())),
        }
    }
    digests.sort_unstable();
    digests.dedup();
    let distinct = digests.len();
    println!("[T7] 按内容采样去重后 {distinct} 个独立模型（别名同内容副本不计入）");
    if !scan_errors.is_empty() {
        panic!(
            "[T7] 扫描阶段 {} 个错误（目录不可读 / 文件不可采样），普查不完整:\n  - {}",
            scan_errors.len(),
            scan_errors.join("\n  - ")
        );
    }
    assert!(
        distinct >= MIN_EXPECTED_MODELS,
        "[T7] 独立模型数 {distinct} < {MIN_EXPECTED_MODELS}：属资产不完整 / 目录错误，\
         **不是**兼容性结论；目录: {}（发现 {total} 个 .onnx，其中 {} 个是别名副本）",
        root.display(),
        total.saturating_sub(distinct)
    );

    let intra = omni_core::ort_runtime::OmniOrtRuntime::recommended_intra_threads();
    let mut failures: Vec<String> = Vec::new();

    for path in &models {
        let name = path
            .strip_prefix(&root)
            .unwrap_or(path)
            .display()
            .to_string();
        let bytes = match std::fs::read(path) {
            Ok(b) => b,
            Err(e) => {
                failures.push(format!("{name}: 读取失败 -> {e}"));
                continue;
            }
        };
        let Some(opsets) = parse_opset_imports(&bytes) else {
            failures.push(format!("{name}: opset_import 解析失败（protobuf 结构异常）"));
            continue;
        };
        let Some(opset) = default_domain_opset(&opsets) else {
            failures.push(format!(
                "{name}: 未声明默认域 opset_import（解析结果 {opsets:?}）"
            ));
            continue;
        };
        if opset > MAX_SUPPORTED_OPSET {
            failures.push(format!(
                "{name}: opset {opset} > 运行时 B 上限 {MAX_SUPPORTED_OPSET}"
            ));
            continue;
        }

        // 纯 CPU Session：不挂任何 EP，只验证「模型本身能否被运行时 B 加载」
        let t0 = Instant::now();
        let built = ort::session::Session::builder()
            .and_then(|b| b.with_intra_threads(intra))
            .and_then(|b| b.commit_from_file(path));
        let build_ms = t0.elapsed().as_millis();

        match built {
            Ok(session) => {
                let inputs = session.inputs.len();
                let outputs = session.outputs.len();
                println!(
                    "[T7]   OK  {name}  opset={opset}  {build_ms}ms  inputs={inputs} outputs={outputs}  {:.1}MB",
                    bytes.len() as f64 / 1024.0 / 1024.0
                );
            }
            Err(e) => failures.push(format!("{name}: 纯 CPU Session 构建失败 -> {e}")),
        }
    }

    if !failures.is_empty() {
        panic!(
            "[T7] {} / {total} 个模型未通过兼容普查:\n  - {}",
            failures.len(),
            failures.join("\n  - ")
        );
    }
    println!(
        "[T7] 全量 {total} 个文件（{distinct} 个独立模型）在运行时 B 纯 CPU Session 下 \
         100% 构建成功（opset ≤ {MAX_SUPPORTED_OPSET}）"
    );
}

/* ------------------------------------------------------------------ *
 * 解码器自测（不依赖任何资产，常规 cargo test 即跑）
 * ------------------------------------------------------------------ */

/// 按 protobuf wire format 编码：tag(varint) + length-delimited payload
fn encode_len_delimited(field_number: u64, payload: &[u8]) -> Vec<u8> {
    fn write_varint(mut v: u64, out: &mut Vec<u8>) {
        loop {
            let mut byte = (v & 0x7f) as u8;
            v >>= 7;
            if v != 0 {
                byte |= 0x80;
            }
            out.push(byte);
            if v == 0 {
                break;
            }
        }
    }
    let mut out = Vec::new();
    write_varint((field_number << 3) | 2, &mut out);
    write_varint(payload.len() as u64, &mut out);
    out.extend_from_slice(payload);
    out
}

/// 合成一个仅含 ir_version / producer_name / opset_import×2 的最小 ModelProto
fn synthetic_model(opset_imports: &[(&str, i64)]) -> Vec<u8> {
    fn write_varint(mut v: u64, out: &mut Vec<u8>) {
        loop {
            let mut byte = (v & 0x7f) as u8;
            v >>= 7;
            if v != 0 {
                byte |= 0x80;
            }
            out.push(byte);
            if v == 0 {
                break;
            }
        }
    }
    let mut model = Vec::new();
    // 字段 1 ir_version = 8（varint）
    write_varint(1 << 3, &mut model);
    write_varint(8, &mut model);
    // 字段 2 producer_name = "t7"（length-delimited，用于验证顶层跳过逻辑）
    model.extend_from_slice(&encode_len_delimited(2, b"t7"));
    // 字段 8 opset_import（repeated）
    for (domain, version) in opset_imports {
        let mut sub = Vec::new();
        if !domain.is_empty() {
            sub.extend_from_slice(&encode_len_delimited(1, domain.as_bytes()));
        }
        // 字段 2 version（varint）
        write_varint(2 << 3, &mut sub);
        write_varint(*version as u64, &mut sub);
        model.extend_from_slice(&encode_len_delimited(8, &sub));
    }
    model
}

#[test]
fn test_opset_parser_reads_default_and_custom_domains() {
    let model = synthetic_model(&[("", 21), ("com.microsoft", 1)]);
    let opsets = parse_opset_imports(&model).expect("解析合成 ModelProto 失败");
    assert_eq!(opsets.len(), 2, "应解析出两条 opset_import: {opsets:?}");
    assert_eq!(default_domain_opset(&opsets), Some(21));
    assert_eq!(opsets[1], ("com.microsoft".to_string(), 1));
}

#[test]
fn test_opset_parser_treats_ai_onnx_as_default_domain() {
    let model = synthetic_model(&[("ai.onnx", 23)]);
    let opsets = parse_opset_imports(&model).expect("解析合成 ModelProto 失败");
    assert_eq!(default_domain_opset(&opsets), Some(23));
}

#[test]
fn test_opset_parser_rejects_truncated_payload() {
    let model = synthetic_model(&[("", 21)]);
    // 砍掉尾部 1 字节：长度声明大于剩余数据，必须返回 None 而不是 panic / 静默截断
    assert!(parse_opset_imports(&model[..model.len() - 1]).is_none());
}

#[test]
fn test_opset_parser_returns_empty_when_field_absent() {
    // 只有 ir_version 的模型：解析成功但无 opset_import（调用方据此报「未声明」）
    let mut model = Vec::new();
    model.push(1 << 3);
    model.push(8);
    let opsets = parse_opset_imports(&model).expect("解析失败");
    assert!(opsets.is_empty());
    assert_eq!(default_domain_opset(&opsets), None);
}

#[test]
fn test_opset_parser_survives_field_disorder_and_fixed_width_fields() {
    // 真实导出器的常见形态：graph(7) 体量巨大且内含 0x80 字节，opset_import(8) 在其后；
    // 同时掺入 fixed64(1) / fixed32(5) 两种 wire type，验证跳过逻辑不会越界或错位。
    let mut model = Vec::new();
    // 字段 7 graph：300 字节 payload，内部大量 0x80/0xFF（模拟权重字节）
    let graph_payload: Vec<u8> = (0..300u32).map(|i| (i % 256) as u8).collect();
    model.extend_from_slice(&encode_len_delimited(7, &graph_payload));
    // 字段 10 fixed64（wire type 1）
    model.extend_from_slice(&[((10u64 << 3) | 1) as u8]);
    model.extend_from_slice(&[0xAA; 8]);
    // 字段 11 fixed32（wire type 5）
    model.extend_from_slice(&[((11u64 << 3) | 5) as u8]);
    model.extend_from_slice(&[0xBB; 4]);
    // 字段 8 opset_import 位于末尾
    let mut sub = Vec::new();
    sub.extend_from_slice(&encode_len_delimited(1, b""));
    sub.push(2 << 3);
    sub.push(21);
    model.extend_from_slice(&encode_len_delimited(8, &sub));

    let opsets = parse_opset_imports(&model).expect("字段乱序 + 定宽字段场景解析失败");
    assert_eq!(default_domain_opset(&opsets), Some(21));
}

#[test]
fn test_opset_parser_default_domain_first_wins() {
    // `""` 与 `ai.onnx` 都属默认域：取首个声明（ONNX 规范只允许一个默认域 import，
    // 出现两条时以首条为准，避免后出现的低版本覆盖真实 opset）
    let model = synthetic_model(&[("ai.onnx", 21), ("", 17)]);
    let opsets = parse_opset_imports(&model).expect("解析失败");
    assert_eq!(default_domain_opset(&opsets), Some(21));
}
