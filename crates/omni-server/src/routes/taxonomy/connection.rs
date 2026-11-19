use rusqlite::Connection;
use std::path::Path;

/// 只读 PRAGMA 套装 —— 委托 [`omni_core::apply_readonly_pragmas`] 单点。
///
/// 工单09 · 连接治理收尾：本函数原为与 omni-core/db_sqlite.rs 并存的双套 PRAGMA
/// （cache_size -65536 vs -32000、缺 synchronous/mmap_size）。现统一由 omni-core
/// 权威实现（omni-hownet 亦复用），本处薄委托，落实「禁各模块重复实现」（红线4）。
pub fn apply_readonly_pragmas(conn: &Connection) -> anyhow::Result<()> {
    omni_core::apply_readonly_pragmas(conn)
}

/// 以绝对只读 + busy_timeout 防阻塞 + PRAGMA 收口模式打开桌面端业务主库。
///
/// 工单08 Seam A / 工单09：唯一入口。`busy_timeout(5000)` 根治 Node 主进程写入时
/// 直接抛 SQLITE_BUSY；随后经 [`omni_core::apply_readonly_pragmas`] 统一设只读 PRAGMA。
pub fn open_omni_readonly_connection<P: AsRef<Path>>(db_path: P) -> anyhow::Result<Connection> {
    let conn = Connection::open_with_flags(
        db_path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY
            | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX
            | rusqlite::OpenFlags::SQLITE_OPEN_URI,
    )?;
    // 5 秒 busy 超时，防 Node 主进程写入时直接 SQLITE_BUSY
    conn.busy_timeout(std::time::Duration::from_millis(5000))?;
    // 统一只读 PRAGMA（委托 omni-core 单点）
    apply_readonly_pragmas(&conn)?;
    Ok(conn)
}
