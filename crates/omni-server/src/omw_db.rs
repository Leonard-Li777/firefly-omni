//! OMW 多语言标签词库只读连接池与热重连组件
//!
//! 实现 ADR-0035「双消费者直连」架构中的 Omni 侧只读消费者：
//! - 以 `SQLITE_OPEN_READ_ONLY` 打开桌面端 SQLite 数据库，并叠加 `PRAGMA query_only = ON`
//!   双保险，使 INSERT/UPDATE/DELETE/CREATE 等写操作被 SQLite 原生拒绝；
//! - 连接池模式避免查询时反复开关文件句柄，查询期间连接独占借出（池锁保证互斥，
//!   故采用 NO_MUTEX 只读连接，与 omni-hownet 惯例一致）；
//! - 池容量有上限、归还即回池，句柄随 `Connection` drop 自动释放，防句柄泄漏；
//! - `connect` 支持多语言切换时热替换整个连接池与数据库路径，无需重启服务进程。

use rusqlite::{Connection, OpenFlags};
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use anyhow::{anyhow, Result};

/// 单只读连接池的默认容量上限（并发查询饱和阈值，超出即随查询自然回收）
const DEFAULT_POOL_CAPACITY: usize = 4;

/// OMW 词库只读连接池（Clone 共享同一池，开销极低）
#[derive(Clone)]
pub struct OmwDb {
    inner: Arc<Mutex<OmwDbPool>>,
}

/// 池内部状态：当前数据库路径 + 空闲只读连接句柄
struct OmwDbPool {
    path: Option<PathBuf>,
    avail: VecDeque<Connection>,
    capacity: usize,
}

/// 以严格只读 + 内存映射加速模式打开数据库（与 omni-hownet 保持同一套 PRAGMA 调优）
fn open_readonly_connection(path: &Path) -> Result<Connection> {
    let flags = OpenFlags::SQLITE_OPEN_READ_ONLY
        | OpenFlags::SQLITE_OPEN_URI
        | OpenFlags::SQLITE_OPEN_NO_MUTEX;
    let conn = Connection::open_with_flags(path, flags)?;

    // 只读性能优化 + 二级写保护（query_only 为连接级只读护栏）
    conn.execute_batch(
        "PRAGMA query_only = ON;
         PRAGMA synchronous = OFF;
         PRAGMA mmap_size = 268435456;
         PRAGMA cache_size = -32000;
         PRAGMA temp_store = MEMORY;",
    )?;

    Ok(conn)
}

impl OmwDb {
    /// 创建一个初始不可用的连接池（未配置数据库路径）
    pub fn unavailable() -> Self {
        Self::with_capacity(DEFAULT_POOL_CAPACITY)
    }

    /// 以指定容量创建初始不可用的连接池
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            inner: Arc::new(Mutex::new(OmwDbPool {
                path: None,
                avail: VecDeque::new(),
                capacity: capacity.max(1),
            })),
        }
    }

    /// 当前配置的数据库路径（未配置时返回 None）
    pub fn db_path(&self) -> Option<PathBuf> {
        self.inner.lock().ok().and_then(|g| g.path.clone())
    }

    /// 是否处于可用状态（路径已配置）
    pub fn is_available(&self) -> bool {
        self.inner.lock().map(|g| g.path.is_some()).unwrap_or(false)
    }

    /// 连接（或热切换至）指定数据库：
    /// 先以新路径探测打开一次，成功后原子替换池内路径并清空旧句柄，
    /// 失败时保持原状态不变，避免把一个可用的池切到打不开的库。
    pub fn connect<P: AsRef<Path>>(&self, path: P) -> Result<()> {
        let path = path.as_ref();
        let probe = open_readonly_connection(path)?;
        let mut guard = self
            .inner
            .lock()
            .map_err(|_| anyhow!("OMW 连接池锁中毒"))?;
        guard.path = Some(path.to_path_buf());
        guard.avail.clear();
        guard.avail.push_back(probe);
        Ok(())
    }

    /// 断开直连：清空路径与全部只读句柄，恢复初始不可用态
    pub fn disconnect(&self) {
        if let Ok(mut guard) = self.inner.lock() {
            guard.path = None;
            guard.avail.clear();
        }
    }

    /// 借出只读连接执行查询后自动归还
    ///
    /// 查询过程不持有池锁（连接为独占借出），因此并发查询不会被互相阻塞；
    /// 借出期间若发生重连，归还时因路径不一致而直接关闭旧句柄，避免混池。
    pub fn with_conn<T>(&self, f: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
        let conn = self.checkout()?;
        let result = f(&conn);
        self.checkin(conn);
        result
    }

    /// 校验可用性：实际执行一次 `SELECT 1`，反映连接真实可读性（供健康检查使用）
    pub fn validate(&self) -> bool {
        self.with_conn(|conn| {
            conn.query_row("SELECT 1", [], |row| row.get::<_, i64>(0))?;
            Ok(())
        })
        .is_ok()
    }

    /// 从池中借出一个连接；池空且路径仍有效时即时新建一个只读连接
    fn checkout(&self) -> Result<Connection> {
        let mut guard = self
            .inner
            .lock()
            .map_err(|_| anyhow!("OMW 连接池锁中毒"))?;
        let Some(path) = guard.path.clone() else {
            return Err(anyhow!("OMW 词库未配置数据库路径，处于不可用状态"));
        };
        if let Some(conn) = guard.avail.pop_front() {
            return Ok(conn);
        }
        let conn = open_readonly_connection(&path)?;
        Ok(conn)
    }

    /// 归还连接：路径仍有效且未超容量时回池复用，否则直接关闭释放句柄
    fn checkin(&self, conn: Connection) {
        if let Ok(mut guard) = self.inner.lock() {
            if guard.path.is_some() && guard.avail.len() < guard.capacity {
                guard.avail.push_back(conn);
                return;
            }
        }
        // 已断开或无多余容量 → 连接 drop 自动关闭，无句柄泄漏
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 构造一个带标记值的临时 SQLite 数据库（默认日志模式，返回仍持有的写连接，
    /// 模拟桌面端在 WAL 数据库保持打开的场景时由调用方自行决定是否 drop）
    fn create_fixture_db(dir: &Path, marker: i64) -> (PathBuf, Connection) {
        let path = dir.join(format!("omw_fixture_{marker}.db"));
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE probe (id INTEGER PRIMARY KEY, marker INTEGER NOT NULL);",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO probe (marker) VALUES (?1)",
            rusqlite::params![marker],
        )
        .unwrap();
        (path, conn)
    }

    fn read_marker(db: &OmwDb) -> i64 {
        db.with_conn(|conn| {
            Ok(conn.query_row(
                "SELECT marker FROM probe WHERE id = 1",
                [],
                |row| row.get(0),
            )?)
        })
        .unwrap()
    }

    #[test]
    fn unavailable_state_refuses_queries() {
        let db = OmwDb::unavailable();
        assert!(!db.is_available());
        assert!(db.db_path().is_none());
        assert!(!db.validate());
        // 未配置路径时查询直接报错，而非尝试打开数据库
        assert!(db.with_conn(|_| Ok(())).is_err());
    }

    #[test]
    fn connect_makes_db_available_and_queryable() {
        let dir = tempfile::tempdir().unwrap();
        let (path, _writable) = create_fixture_db(dir.path(), 7);
        let db = OmwDb::unavailable();
        db.connect(&path).unwrap();
        assert!(db.is_available());
        assert!(db.validate());
        assert_eq!(db.db_path().unwrap(), path);
        assert_eq!(read_marker(&db), 7);
    }

    #[test]
    fn read_only_connection_natively_rejects_writes() {
        let dir = tempfile::tempdir().unwrap();
        let (path, _writable) = create_fixture_db(dir.path(), 1);
        let db = OmwDb::unavailable();
        db.connect(&path).unwrap();

        // INSERT
        let err = db
            .with_conn(|conn| Ok(conn.execute("INSERT INTO probe (marker) VALUES (99)", [])?))
            .unwrap_err();
        assert!(err.to_string().to_lowercase().contains("readonly"), "INSERT 应被只读拦截: {err}");

        // UPDATE
        let err = db
            .with_conn(|conn| Ok(conn.execute("UPDATE probe SET marker = 99 WHERE id = 1", [])?))
            .unwrap_err();
        assert!(err.to_string().to_lowercase().contains("readonly"), "UPDATE 应被只读拦截: {err}");

        // DELETE
        let err = db
            .with_conn(|conn| Ok(conn.execute("DELETE FROM probe WHERE id = 1", [])?))
            .unwrap_err();
        assert!(err.to_string().to_lowercase().contains("readonly"), "DELETE 应被只读拦截: {err}");

        // CREATE（DDL 同样被原生拒绝）
        let err = db
            .with_conn(|conn| Ok(conn.execute_batch("CREATE TABLE hacker (id INTEGER);")?))
            .unwrap_err();
        assert!(err.to_string().to_lowercase().contains("readonly"), "CREATE TABLE 应被只读拦截: {err}");

        // 数据未被篡改
        assert_eq!(read_marker(&db), 1);
    }

    #[test]
    fn reconnect_hot_swaps_database_without_restart() {
        let dir = tempfile::tempdir().unwrap();
        let (path_a, _wa) = create_fixture_db(dir.path(), 111);
        let (path_b, _wb) = create_fixture_db(dir.path(), 222);
        let db = OmwDb::unavailable();
        db.connect(&path_a).unwrap();
        assert_eq!(read_marker(&db), 111);

        // 热切换至 B：同一实例、无需重启
        db.connect(&path_b).unwrap();
        assert_eq!(read_marker(&db), 222);
        assert_eq!(db.db_path().unwrap(), path_b);
        assert!(db.validate());
    }

    #[test]
    fn reconnect_to_invalid_path_keeps_old_state() {
        let dir = tempfile::tempdir().unwrap();
        let (path_a, _wa) = create_fixture_db(dir.path(), 5);
        let db = OmwDb::unavailable();
        db.connect(&path_a).unwrap();
        let bad_path = dir.path().join("no_such_database.db");
        assert!(db.connect(&bad_path).is_err());
        // 失败不破坏既有可用状态
        assert!(db.is_available());
        assert_eq!(db.db_path().unwrap(), path_a);
        assert_eq!(read_marker(&db), 5);
    }

    #[test]
    fn disconnect_restores_unavailable_state() {
        let dir = tempfile::tempdir().unwrap();
        let (path_a, _wa) = create_fixture_db(dir.path(), 3);
        let db = OmwDb::unavailable();
        db.connect(&path_a).unwrap();
        assert!(db.is_available());
        db.disconnect();
        assert!(!db.is_available());
        assert!(db.db_path().is_none());
        assert!(!db.validate());
    }

    #[test]
    fn wal_mode_database_readable_read_only() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("omw_wal_fixture.db");
        // 模拟桌面端：以 WAL 模式打开并保持写连接存活（WAL/-shm 文件存在）
        let writable = Connection::open(&path).unwrap();
        writable
            .pragma_update(None, "journal_mode", "WAL")
            .unwrap();
        writable
            .execute_batch("CREATE TABLE probe (id INTEGER PRIMARY KEY, marker INTEGER NOT NULL);")
            .unwrap();
        writable
            .execute("INSERT INTO probe (marker) VALUES (42)", [])
            .unwrap();

        let db = OmwDb::unavailable();
        db.connect(&path).unwrap();
        assert!(db.validate());
        let marker: i64 = db
            .with_conn(|conn| {
                Ok(conn.query_row(
                    "SELECT marker FROM probe WHERE id = 1",
                    [],
                    |row| row.get(0),
                )?)
            })
            .unwrap();
        assert_eq!(marker, 42);
        drop(writable);
    }
}