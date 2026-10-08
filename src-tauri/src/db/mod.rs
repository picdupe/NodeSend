//! 文件功能：数据库模块入口。
//!
//! - [`Db`] 包装 SQLite 连接（rusqlite），是全应用唯一核心数据存储（基线 §18）；
//! - 连接以 `Arc + Mutex` 在 Service / HTTP / IPC 各模块间共享；
//! - 打开时自动执行 [`migration`]，Schema 只能通过 Migration 修改。
pub mod migration;

use std::sync::{Arc, Mutex, MutexGuard};

use rusqlite::Connection;

use crate::config::AppPaths;
use crate::error::AppResult;

pub struct Db {
    conn: Mutex<Connection>,
}

impl Db {
    /// 打开（必要时创建）数据库并执行全部迁移。
    pub fn open(paths: &AppPaths) -> AppResult<Arc<Self>> {
        let conn = Connection::open(&paths.db_file)?;
        // WAL 提高并发读写稳定性；强制外键约束。
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")?;

        let db = Arc::new(Self {
            conn: Mutex::new(conn),
        });
        migration::run_migrations(&db)?;
        Ok(db)
    }

    /// 获取连接锁。各仓储函数通过它执行 SQL。
    pub fn conn(&self) -> MutexGuard<'_, Connection> {
        self.conn.lock().expect("数据库连接锁中毒")
    }
}
