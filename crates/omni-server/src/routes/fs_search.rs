//! fs_search.rs — 未分析文件双轨高速搜索引擎端点 (Everything IPC + Rust Fast-Walk 降轨)
//!
//! 依据 ADR-0039 与 PRD 0039 架构契约：
//! - GET /api/v1/search/fs?dir=...&q=...&limit=100
//! - 快轨 (Everything IPC): Windows 优先探测本地运行中的 Everything，通过 WM_COPYDATA QUERY2 协议直连查询；
//! - 降轨 (Rust Fast-Walk): 未运行 Everything 或非 Windows 平台时，基于 ignore crate 多线程无锁并行遍历；
//! - 严禁要求 Administrator 特权，不弹 UAC 盾牌弹窗，跨平台全盘符自适应。

use axum::{
    extract::Query,
    http::StatusCode,
    Json,
};
use ignore::{DirEntry, WalkState};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

/// 文件搜索查询参数: GET /api/v1/search/fs
#[derive(Debug, Clone, Deserialize)]
pub struct FsSearchQuery {
    /// 目标物理搜索目录绝对路径
    pub dir: String,
    /// 文件名匹配关键词 (支持 alias = "query")
    #[serde(alias = "query")]
    pub q: String,
    /// 返回最大数量约束 (默认 100)
    #[serde(default = "default_fs_limit")]
    pub limit: Option<usize>,
}

fn default_fs_limit() -> Option<usize> {
    Some(100)
}

/// 命中文件条目契约
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct FsSearchItem {
    pub name: String,
    pub path: String,
    pub size: u64,
    pub is_directory: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extension: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mtime: Option<u64>,
}

/// 文件搜索响应体
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FsSearchResponse {
    pub items: Vec<FsSearchItem>,
    /// 已发现的匹配总数；命中 limit 提前终止时为下界（等于 returned，可能仍有更多未扫描匹配）
    pub total: usize,
    /// 本次实际返回条数（恒等于 items.len()）
    pub returned: usize,
    pub duration_ms: u64,
    pub source: String, // "everything" | "fast_walk"
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// 构造 Everything 检索串：path:"dir" keyword
pub fn build_everything_search(dir: &str, keyword: &str) -> String {
    let dir_trim = dir.trim().trim_end_matches(['\\', '/']);
    format!("path:\"{}\" {}", dir_trim, keyword.trim())
}

/// 判断结果路径是否位于目标目录之下（Windows 大小写不敏感）
pub fn path_is_under_dir(result_path: &str, dir: &str) -> bool {
    let dir_trim = dir.trim().trim_end_matches(['\\', '/']);
    if dir_trim.is_empty() {
        return false;
    }
    let rp = result_path.replace('/', "\\");
    let dp = dir_trim.replace('/', "\\");
    rp.to_lowercase()
        .starts_with(&format!("{}\\", dp.to_lowercase()))
}

/// 未分析文件高速搜索处理器: GET /api/v1/search/fs
pub async fn fs_search_handler(
    Query(query): Query<FsSearchQuery>,
) -> Result<Json<FsSearchResponse>, (StatusCode, Json<FsSearchResponse>)> {
    let start = Instant::now();
    let dir = query.dir.trim().to_string();
    let q = query.q.trim().to_string();
    let limit = query.limit.unwrap_or(100).clamp(1, 2000);

    if dir.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(FsSearchResponse {
                items: Vec::new(),
                total: 0,
                returned: 0,
                duration_ms: start.elapsed().as_millis() as u64,
                source: "none".to_string(),
                error: Some("缺少必要参数 'dir' 或路径为空".to_string()),
            }),
        ));
    }

    if !Path::new(&dir).exists() {
        return Err((
            StatusCode::NOT_FOUND,
            Json(FsSearchResponse {
                items: Vec::new(),
                total: 0,
                returned: 0,
                duration_ms: start.elapsed().as_millis() as u64,
                source: "none".to_string(),
                error: Some(format!("搜索目录不存在: {dir}")),
            }),
        ));
    }

    // 关键词为空时直接返回空结果
    if q.is_empty() {
        return Ok(Json(FsSearchResponse {
            items: Vec::new(),
            total: 0,
            returned: 0,
            duration_ms: start.elapsed().as_millis() as u64,
            source: "fast_walk".to_string(),
            error: None,
        }));
    }

    let search_dir = dir.clone();
    let keyword = q.clone();

    // 在 spawn_blocking 中执行双轨检索，避免阻塞异步运行时
    let search_outcome = tokio::task::spawn_blocking(move || {
        // 1. 快轨：探测 Everything 是否运行并尝试 IPC 查询
        // 空结果时仍降轨 fast_walk，避免目录未被 Everything 索引时漏检
        if is_everything_available() {
            if let Some(items) = query_everything_ipc(&search_dir, &keyword, limit) {
                if !items.is_empty() {
                    return (items, "everything".to_string());
                }
            }
        }

        // 2. 降轨：Rust Fast-Walk 多线程无锁文件系统并行遍历
        let items = fast_walk_search(&search_dir, &keyword, limit);
        (items, "fast_walk".to_string())
    })
    .await;

    let duration_ms = start.elapsed().as_millis() as u64;

    match search_outcome {
        Ok((items, source)) => {
            let total = items.len();
            let returned = items.len();
            Ok(Json(FsSearchResponse {
                items,
                total,
                returned,
                duration_ms,
                source,
                error: None,
            }))
        }
        Err(join_err) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(FsSearchResponse {
                items: Vec::new(),
                total: 0,
                returned: 0,
                duration_ms,
                source: "fast_walk".to_string(),
                error: Some(format!("文件系统扫描任务 panic: {join_err}")),
            }),
        )),
    }
}

/// 探测本地 Windows 环境中 Everything 进程是否正处于运行状态
pub fn is_everything_available() -> bool {
    #[cfg(windows)]
    {
        use windows_sys::Win32::UI::WindowsAndMessaging::FindWindowW;
        let class_name: Vec<u16> = "EVERYTHING_TASKBAR_NOTIFICATION\0"
            .encode_utf16()
            .collect();
        unsafe {
            let hwnd = FindWindowW(class_name.as_ptr(), std::ptr::null());
            !hwnd.is_null()
        }
    }
    #[cfg(not(windows))]
    {
        false
    }
}

/// Windows Everything IPC 查询
/// 协议依据 voidtools Everything-SDK `ipc/everything_ipc.h`：
/// - 向 EVERYTHING_TASKBAR_NOTIFICATION 窗口发送 WM_COPYDATA
/// - dwData = EVERYTHING_IPC_COPYDATA_QUERY2W (18)
/// - 载荷为 pack(1) 的 EVERYTHING_IPC_QUERY2 + UTF-16 检索串
/// - 结果以 WM_COPYDATA 回传至 reply_hwnd
/// 失败或超时返回 None，触发 Rust fast_walk 降轨
#[cfg(windows)]
fn query_everything_ipc(search_dir: &str, keyword: &str, limit: usize) -> Option<Vec<FsSearchItem>> {
    everything_ipc::query(search_dir, keyword, limit)
}

#[cfg(not(windows))]
fn query_everything_ipc(_search_dir: &str, _keyword: &str, _limit: usize) -> Option<Vec<FsSearchItem>> {
    None
}

/// Rust Fast-Walk 多线程并行无锁文件树遍历
/// 基于 ignore crate (ripgrep 同源无锁工作窃取)，支持全盘符，无需管理员权限，不弹 UAC 弹窗
pub fn fast_walk_search(search_dir: &str, keyword: &str, limit: usize) -> Vec<FsSearchItem> {
    let target_dir = Path::new(search_dir);
    if !target_dir.is_dir() {
        return Vec::new();
    }

    let kw_lower = keyword.to_lowercase();
    let num_threads = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4);

    let walker = ignore::WalkBuilder::new(target_dir)
        .threads(num_threads)
        .hidden(false)       // 遍历包含隐藏文件
        .ignore(false)       // 忽略 .ignore 规则限制，确保全盘全面搜索
        .git_ignore(false)   // 忽略 .gitignore 规则
        .git_global(false)
        .git_exclude(false)
        .follow_links(false) // 避免符号链接循环
        .same_file_system(false)
        .build_parallel();

    let matched_items = Arc::new(Mutex::new(Vec::with_capacity(limit)));
    let match_count = Arc::new(AtomicUsize::new(0));
    let stop_flag = Arc::new(AtomicBool::new(false));

    let kw_ref = kw_lower;
    let target_dir_path = target_dir.to_path_buf();

    walker.run(|| {
        let items_buf = Arc::clone(&matched_items);
        let count = Arc::clone(&match_count);
        let stop = Arc::clone(&stop_flag);
        let kw = kw_ref.clone();
        let base_dir = target_dir_path.clone();

        Box::new(move |result: Result<DirEntry, ignore::Error>| -> WalkState {
            if stop.load(Ordering::Relaxed) {
                return WalkState::Quit;
            }

            let entry = match result {
                Ok(e) => e,
                Err(_) => {
                    // 权限被拒绝或 I/O 异常时静默忽略，绝不申请特权或弹窗
                    return WalkState::Continue;
                }
            };

            // 跳过根目录本身
            if entry.path() == base_dir {
                return WalkState::Continue;
            }

            let file_name = entry.file_name().to_string_lossy();
            if file_name.to_lowercase().contains(&kw) {
                let path = entry.path();
                let is_directory = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
                let extension = path.extension().map(|e| e.to_string_lossy().to_string());

                let (size, mtime) = if let Ok(meta) = entry.metadata() {
                    let sz = if is_directory { 0 } else { meta.len() };
                    let mt = meta
                        .modified()
                        .ok()
                        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                        .map(|d| d.as_secs());
                    (sz, mt)
                } else {
                    (0, None)
                };

                let item = FsSearchItem {
                    name: file_name.to_string(),
                    path: path.to_string_lossy().to_string(),
                    size,
                    is_directory,
                    extension,
                    mtime,
                };

                let current = count.fetch_add(1, Ordering::Relaxed) + 1;
                {
                    let mut lock = items_buf.lock().unwrap();
                    if lock.len() < limit {
                        lock.push(item);
                    }
                }

                if current >= limit {
                    stop.store(true, Ordering::Relaxed);
                    return WalkState::Quit;
                }
            }

            WalkState::Continue
        })
    });

    let mut final_items = match Arc::try_unwrap(matched_items) {
        Ok(mutex) => mutex.into_inner().unwrap(),
        Err(arc) => arc.lock().unwrap().clone(),
    };

    if final_items.len() > limit {
        final_items.truncate(limit);
    }

    final_items
}

/// Everything IPC 协议实现（仅 Windows）
/// 常量与结构布局严格对齐 voidtools Everything-SDK everything_ipc.h
#[cfg(windows)]
mod everything_ipc {
    use super::{path_is_under_dir, FsSearchItem};
    use std::ffi::c_void;
    use std::sync::mpsc;
    use std::time::Duration;

    use windows_sys::Win32::Foundation::{HWND, LRESULT, WPARAM};
    use windows_sys::Win32::System::DataExchange::COPYDATASTRUCT;
    use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, FindWindowW, GetMessageW,
        PostQuitMessage, RegisterClassExW, SendMessageW, TranslateMessage, CS_HREDRAW, CS_VREDRAW,
        CW_USEDEFAULT, MSG, WM_COPYDATA, WM_DESTROY, WM_USER, WNDCLASSEXW,
    };

    const EVERYTHING_IPC_WNDCLASS: &[u16] = &[
        b'E' as u16, b'V' as u16, b'E' as u16, b'R' as u16, b'Y' as u16, b'T' as u16,
        b'H' as u16, b'I' as u16, b'N' as u16, b'G' as u16, b'_' as u16, b'T' as u16,
        b'A' as u16, b'S' as u16, b'K' as u16, b'B' as u16, b'A' as u16, b'R' as u16,
        b'_' as u16, b'N' as u16, b'O' as u16, b'T' as u16, b'I' as u16, b'F' as u16,
        b'I' as u16, b'C' as u16, b'A' as u16, b'T' as u16, b'I' as u16, b'O' as u16,
        b'N' as u16, 0,
    ];

    /// WM_USER — EVERYTHING_WM_IPC
    const EVERYTHING_WM_IPC: u32 = WM_USER;
    /// EVERYTHING_IPC_COPYDATA_QUERY2W
    const EVERYTHING_IPC_COPYDATA_QUERY2W: usize = 18;
    /// EVERYTHING_IPC_QUERY2_REQUEST_* 请求位
    const REQ_NAME: u32 = 0x0000_0001;
    const REQ_PATH: u32 = 0x0000_0002;
    const REQ_SIZE: u32 = 0x0000_0010;
    const REQ_DATE_MODIFIED: u32 = 0x0000_0040;
    /// EVERYTHING_IPC_SORT_NAME_ASCENDING
    const SORT_NAME_ASC: u32 = 1;
    /// EVERYTHING_IPC_FOLDER
    const ITEM_FOLDER: u32 = 0x0000_0001;
    /// 自定义回传 dwData，避免与其它 IPC 冲突
    const REPLY_COPYDATA_ID: u32 = 0x0FF1_CE01;
    /// IPC 等待超时
    const IPC_TIMEOUT: Duration = Duration::from_millis(1500);

    const REPLY_CLASS_NAME: &[u16] = &[
        b'F' as u16, b'i' as u16, b'r' as u16, b'e' as u16, b'f' as u16, b'l' as u16,
        b'y' as u16, b'O' as u16, b'm' as u16, b'n' as u16, b'i' as u16, b'R' as u16,
        b'e' as u16, b'p' as u16, b'l' as u16, b'y' as u16, 0,
    ];

    struct ReplyState {
        items: Vec<FsSearchItem>,
        dir: String,
        keyword: String,
        limit: usize,
        done: bool,
    }

    /// 对外入口：Everything 可用时发起 IPC 查询
    pub fn query(search_dir: &str, keyword: &str, limit: usize) -> Option<Vec<FsSearchItem>> {
        let (tx, rx) = mpsc::channel::<Option<Vec<FsSearchItem>>>();
        let dir = search_dir.to_string();
        let kw = keyword.to_string();

        // 专用线程承载隐藏窗口与消息泵，避免阻塞 tokio worker 之外的 UI 逻辑
        let handle = std::thread::spawn(move || {
            let result = run_ipc_query(&dir, &kw, limit);
            let _ = tx.send(result);
        });

        match rx.recv_timeout(IPC_TIMEOUT) {
            Ok(items) => {
                let _ = handle.join();
                items
            }
            Err(_) => None,
        }
    }

    unsafe extern "system" fn reply_wnd_proc(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: isize,
    ) -> LRESULT {
        if msg == WM_COPYDATA {
            let cds = lparam as *const COPYDATASTRUCT;
            if !cds.is_null() {
                let cds = &*cds;
                if cds.dwData as u32 == REPLY_COPYDATA_ID && !cds.lpData.is_null() && cds.cbData > 0 {
                    // 从线程局部取回状态指针（创建时写入）
                    let state_ptr = THREAD_STATE.with(|s| s.get());
                    if !state_ptr.is_null() {
                        let state = &mut *state_ptr;
                        parse_everything_list2(
                            cds.lpData as *const u8,
                            cds.cbData as usize,
                            &state.dir,
                            &state.keyword,
                            state.limit,
                            &mut state.items,
                        );
                        state.done = true;
                        PostQuitMessage(0);
                        return 1;
                    }
                }
            }
            return 1;
        }
        if msg == WM_DESTROY {
            PostQuitMessage(0);
            return 0;
        }
        DefWindowProcW(hwnd, msg, wparam, lparam)
    }

    thread_local! {
        static THREAD_STATE: std::cell::Cell<*mut ReplyState> = const { std::cell::Cell::new(std::ptr::null_mut()) };
    }

    fn run_ipc_query(search_dir: &str, keyword: &str, limit: usize) -> Option<Vec<FsSearchItem>> {
        let everything_hwnd = unsafe {
            FindWindowW(EVERYTHING_IPC_WNDCLASS.as_ptr(), std::ptr::null())
        };
        if everything_hwnd.is_null() {
            return None;
        }

        // 探测 DB 是否就绪（EVERYTHING_IPC_IS_DB_LOADED = 401），未就绪则降轨
        let db_loaded = unsafe {
            SendMessageW(everything_hwnd, EVERYTHING_WM_IPC, 401, 0)
        };
        if db_loaded == 0 {
            return None;
        }

        let mut state = Box::new(ReplyState {
            items: Vec::new(),
            dir: search_dir.to_string(),
            keyword: keyword.to_string(),
            limit,
            done: false,
        });
        let state_ptr: *mut ReplyState = &mut *state;

        unsafe {
            THREAD_STATE.with(|s| s.set(state_ptr));

            let hinstance = GetModuleHandleW(std::ptr::null());
            let wnd_class = WNDCLASSEXW {
                cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
                style: CS_HREDRAW | CS_VREDRAW,
                lpfnWndProc: Some(reply_wnd_proc),
                cbClsExtra: 0,
                cbWndExtra: 0,
                hInstance: hinstance,
                hIcon: std::ptr::null_mut(),
                hCursor: std::ptr::null_mut(),
                hbrBackground: std::ptr::null_mut(),
                lpszMenuName: std::ptr::null(),
                lpszClassName: REPLY_CLASS_NAME.as_ptr(),
                hIconSm: std::ptr::null_mut(),
            };
            if RegisterClassExW(&wnd_class) == 0 {
                // 类可能已注册，忽略失败继续尝试创建
            }

            let reply_hwnd = CreateWindowExW(
                0,
                REPLY_CLASS_NAME.as_ptr(),
                REPLY_CLASS_NAME.as_ptr(),
                0,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                std::ptr::null_mut(), // 无父窗口的隐藏接收窗
                std::ptr::null_mut(),
                hinstance,
                std::ptr::null_mut(),
            );
            if reply_hwnd.is_null() {
                THREAD_STATE.with(|s| s.set(std::ptr::null_mut()));
                return None;
            }

            // 构造 pack(1) 的 EVERYTHING_IPC_QUERY2 + UTF-16 检索串
            let search = super::build_everything_search(search_dir, keyword);
            let search_utf16: Vec<u16> = search.encode_utf16().chain(std::iter::once(0)).collect();
            // 7 个 DWORD = 28 字节头，检索串紧随其后
            let header_len = 28usize;
            let total_len = header_len + search_utf16.len() * 2;
            let mut payload: Vec<u8> = vec![0u8; total_len];

            let reply_hwnd_u32 = reply_hwnd as u32;
            write_u32_le(&mut payload[0..4], reply_hwnd_u32);
            write_u32_le(&mut payload[4..8], REPLY_COPYDATA_ID);
            write_u32_le(&mut payload[8..12], 0); // search_flags: 默认大小写不敏感
            write_u32_le(&mut payload[12..16], 0); // offset
            write_u32_le(&mut payload[16..20], limit as u32); // max_results
            write_u32_le(
                &mut payload[20..24],
                REQ_NAME | REQ_PATH | REQ_SIZE | REQ_DATE_MODIFIED,
            );
            write_u32_le(&mut payload[24..28], SORT_NAME_ASC);
            for (i, unit) in search_utf16.iter().enumerate() {
                let off = header_len + i * 2;
                payload[off] = (*unit & 0xFF) as u8;
                payload[off + 1] = (*unit >> 8) as u8;
            }

            let cds = COPYDATASTRUCT {
                dwData: EVERYTHING_IPC_COPYDATA_QUERY2W,
                cbData: payload.len() as u32,
                lpData: payload.as_mut_ptr() as *mut c_void,
            };

            let sent = SendMessageW(
                everything_hwnd,
                WM_COPYDATA,
                reply_hwnd as WPARAM,
                &cds as *const COPYDATASTRUCT as isize,
            );
            if sent == 0 {
                DestroyWindow(reply_hwnd);
                THREAD_STATE.with(|s| s.set(std::ptr::null_mut()));
                return None;
            }

            // 消息泵：等待 WM_COPYDATA 回传
            let mut msg: MSG = std::mem::zeroed();
            loop {
                let ret = GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0);
                if ret == 0 || ret == -1 {
                    break;
                }
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
                if state.done {
                    break;
                }
            }

            DestroyWindow(reply_hwnd);
            THREAD_STATE.with(|s| s.set(std::ptr::null_mut()));
        }

        if state.items.is_empty() {
            // Everything 返回空：可能目录下确实无匹配，视为有效空结果
            return Some(Vec::new());
        }
        Some(state.items)
    }

    fn write_u32_le(buf: &mut [u8], v: u32) {
        buf[0] = (v & 0xFF) as u8;
        buf[1] = ((v >> 8) & 0xFF) as u8;
        buf[2] = ((v >> 16) & 0xFF) as u8;
        buf[3] = ((v >> 24) & 0xFF) as u8;
    }

    fn read_u32_le(buf: &[u8], off: usize) -> Option<u32> {
        if off + 4 > buf.len() {
            return None;
        }
        Some(u32::from_le_bytes([
            buf[off],
            buf[off + 1],
            buf[off + 2],
            buf[off + 3],
        ]))
    }

    fn read_u64_le(buf: &[u8], off: usize) -> Option<u64> {
        if off + 8 > buf.len() {
            return None;
        }
        Some(u64::from_le_bytes([
            buf[off],
            buf[off + 1],
            buf[off + 2],
            buf[off + 3],
            buf[off + 4],
            buf[off + 5],
            buf[off + 6],
            buf[off + 7],
        ]))
    }

    /// 解析 EVERYTHING_IPC_LISTW 回传（Unicode QUERY2W 路径）
    /// LIST2 头: totitems/numitems/offset/request_flags/sort_type 各 DWORD
    /// ITEM2: flags + data_offset
    /// data_offset 处按 request 位序: name(DWORD len + utf16) path(...) size(u64) mtime(FILETIME)
    pub fn parse_everything_list2(
        data: *const u8,
        len: usize,
        dir: &str,
        keyword: &str,
        limit: usize,
        out: &mut Vec<FsSearchItem>,
    ) {
        if data.is_null() || len < 20 {
            return;
        }
        let buf = unsafe { std::slice::from_raw_parts(data, len) };
        let numitems = read_u32_le(buf, 4).unwrap_or(0) as usize;
        let request_flags = read_u32_le(buf, 12).unwrap_or(0);
        if numitems == 0 {
            return;
        }

        let kw_lower = keyword.to_lowercase();
        let mut cursor = 20usize; // LIST2 头 5 * DWORD

        for _ in 0..numitems {
            if cursor + 8 > buf.len() {
                break;
            }
            let item_flags = read_u32_le(buf, cursor).unwrap_or(0);
            let data_offset = read_u32_le(buf, cursor + 4).unwrap_or(0) as usize;
            cursor += 8;

            if data_offset >= buf.len() {
                continue;
            }

            let mut p = data_offset;
            let mut name = String::new();
            let mut path = String::new();
            let mut size = 0u64;
            let mut mtime = None;

            if request_flags & REQ_NAME != 0 {
                if let Some((s, next)) = read_ipc_wstring(buf, p) {
                    name = s;
                    p = next;
                }
            }
            if request_flags & REQ_PATH != 0 {
                if let Some((s, next)) = read_ipc_wstring(buf, p) {
                    path = s;
                    p = next;
                }
            }
            if request_flags & REQ_SIZE != 0 {
                size = read_u64_le(buf, p).unwrap_or(0);
                p += 8;
            }
            if request_flags & REQ_DATE_MODIFIED != 0 {
                if let Some(ft) = read_u64_le(buf, p) {
                    mtime = filetime_to_unix_secs(ft);
                }
            }

            // 组装完整路径
            let full = if !path.is_empty() && !name.is_empty() {
                let sep = if path.ends_with('\\') || path.ends_with('/') {
                    String::new()
                } else {
                    "\\".to_string()
                };
                format!("{path}{sep}{name}")
            } else if !path.is_empty() {
                path.clone()
            } else {
                name.clone()
            };

            if full.is_empty() || name.is_empty() {
                continue;
            }
            if !name.to_lowercase().contains(&kw_lower) {
                continue;
            }
            if !path_is_under_dir(&full, dir) {
                continue;
            }

            let is_directory = item_flags & ITEM_FOLDER != 0;
            let extension = Path::new(&name)
                .extension()
                .map(|e| e.to_string_lossy().to_string());

            out.push(FsSearchItem {
                name,
                path: full,
                size: if is_directory { 0 } else { size },
                is_directory,
                extension,
                mtime,
            });

            if out.len() >= limit {
                break;
            }
        }
    }

    /// 读取 DWORD 长度 + UTF-16 null 终止串
    fn read_ipc_wstring(buf: &[u8], off: usize) -> Option<(String, usize)> {
        let char_len = read_u32_le(buf, off)? as usize;
        let text_start = off + 4;
        // char_len 不含 null；实际存储为 char_len+1 个 u16
        let byte_len = (char_len + 1) * 2;
        if text_start + byte_len > buf.len() {
            return None;
        }
        let units: Vec<u16> = (0..char_len)
            .map(|i| {
                let o = text_start + i * 2;
                u16::from_le_bytes([buf[o], buf[o + 1]])
            })
            .collect();
        let s = String::from_utf16_lossy(&units);
        Some((s, text_start + byte_len))
    }

    /// FILETIME (100ns since 1601) → Unix 秒
    fn filetime_to_unix_secs(ft: u64) -> Option<u64> {
        if ft == 0 {
            return None;
        }
        const EPOCH_DIFF: u64 = 11_644_473_600;
        Some((ft / 10_000_000).saturating_sub(EPOCH_DIFF))
    }

    use std::path::Path;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_everything_search_quotes_dir() {
        let s = build_everything_search("D:\\data\\work\\", "report");
        assert_eq!(s, "path:\"D:\\data\\work\" report");
    }

    #[test]
    fn test_path_is_under_dir_windows_case_insensitive() {
        assert!(path_is_under_dir(
            "D:\\Data\\Work\\a.txt",
            "d:\\data\\work"
        ));
        assert!(!path_is_under_dir(
            "D:\\Data\\Other\\a.txt",
            "d:\\data\\work"
        ));
        assert!(!path_is_under_dir("D:\\data\\work", "d:\\data\\work"));
    }

    #[cfg(windows)]
    #[test]
    fn test_parse_everything_list2_minimal() {
        // 构造最小 LIST2 + 1 条 ITEM2 (name+path+size+mtime)
        // LIST2 header 20 bytes, item 8 bytes, data follows
        let mut buf: Vec<u8> = Vec::new();
        // totitems=1
        buf.extend_from_slice(&1u32.to_le_bytes());
        // numitems=1
        buf.extend_from_slice(&1u32.to_le_bytes());
        // offset=0
        buf.extend_from_slice(&0u32.to_le_bytes());
        // request_flags = NAME|PATH|SIZE|DATE_MODIFIED
        let flags = 0x1u32 | 0x2u32 | 0x10u32 | 0x40u32;
        buf.extend_from_slice(&flags.to_le_bytes());
        // sort
        buf.extend_from_slice(&1u32.to_le_bytes());
        // item header at 20
        let item_data_off = 28u32;
        buf.extend_from_slice(&0u32.to_le_bytes()); // flags = file
        buf.extend_from_slice(&item_data_off.to_le_bytes());
        // data at 28: name "report.txt"
        let name: Vec<u16> = "report.txt".encode_utf16().collect();
        buf.extend_from_slice(&(name.len() as u32).to_le_bytes());
        for u in &name {
            buf.extend_from_slice(&u.to_le_bytes());
        }
        buf.extend_from_slice(&0u16.to_le_bytes()); // null
        // path
        let path: Vec<u16> = "D:\\tmp\\folder".encode_utf16().collect();
        buf.extend_from_slice(&(path.len() as u32).to_le_bytes());
        for u in &path {
            buf.extend_from_slice(&u.to_le_bytes());
        }
        buf.extend_from_slice(&0u16.to_le_bytes());
        // size
        buf.extend_from_slice(&123u64.to_le_bytes());
        // mtime FILETIME = 0
        buf.extend_from_slice(&0u64.to_le_bytes());

        let mut out = Vec::new();
        super::everything_ipc::parse_everything_list2(
            buf.as_ptr(),
            buf.len(),
            "D:\\tmp\\folder",
            "report",
            10,
            &mut out,
        );
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].name, "report.txt");
        assert_eq!(out[0].path, "D:\\tmp\\folder\\report.txt");
        assert_eq!(out[0].size, 123);
        assert!(!out[0].is_directory);
        assert_eq!(out[0].extension.as_deref(), Some("txt"));
    }
}
