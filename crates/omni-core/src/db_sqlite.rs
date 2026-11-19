use rusqlite::Connection;

/// 只读连接统一 PRAGMA 套装（工单09 · 跨 crate 单一收口）。
///
/// 这是连接级 PRAGMA 的**唯一权威实现**，被 omni-server（taxonomy/connection.rs）
/// 与 omni-hownet（db.rs）共用，落实 AGENTS 红线4「PRAGMA 套装禁止在各模块重复实现」。
///
/// 历史：原 omni-server/taxonomy/connection.rs 另有一套 PRAGMA（缺 synchronous/mmap_size、
/// cache_size=-65536），与本处 cache_size 冲突。现统一取值如下，各处一律委托本函数。
pub fn apply_readonly_pragmas(conn: &Connection) -> anyhow::Result<()> {
    // 强制只读，根治误写
    conn.pragma_update(None, "query_only", "ON")?;
    // 只读场景关同步，降低 IO 等待
    conn.pragma_update(None, "synchronous", "OFF")?;
    // mmap 256MB，加速大库只读映射
    conn.pragma_update(None, "mmap_size", 268435456)?;
    // cache_size 负值 = 按 KB 计的软上限；统一 -65536（64MB），
    // 收敛原两套实现 -32000 / -65536 的冲突
    conn.pragma_update(None, "cache_size", -65536)?;
    // 临时表强制存内存，避免只读库落盘临时文件
    conn.pragma_update(None, "temp_store", "MEMORY")?;
    Ok(())
}
