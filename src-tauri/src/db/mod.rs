//! 文件功能：数据库模块入口。
//!
//! - [`Db`] 包装 SQLite 连接（rusqlite），是全应用唯一核心数据存储（基线 §18）；
//! - 连接以 `Arc + Mutex` 在 Service / HTTP / IPC 各模块间共享；
//! - 打开时自动执行 [`migration`]，Schema 只能通过 Migration 修改；
//! - **只通过 [`Db::with_conn`] / [`Db::with_transaction`] 访问数据库**：连接守卫
//!   不对外暴露，因此"持有连接再取一次连接"这种非重入死锁在类型上无法写出来
//!   （历史上 `manifest_chunk` 正是这样死锁的）。
pub mod migration;

use std::sync::{Arc, Mutex};

use rusqlite::{Connection, Transaction};

use crate::config::AppPaths;
use crate::error::AppResult;
use crate::locks;

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

    /// 在独占连接的前提下执行一段 SQL。
    ///
    /// 闭包内不得再调用 `with_conn` / `with_transaction`（编译器也拿不到守卫，
    /// 因此写不出来）。闭包内不要做耗时或阻塞 I/O。
    pub fn with_conn<T>(&self, f: impl FnOnce(&Connection) -> AppResult<T>) -> AppResult<T> {
        let guard = locks::lock(&self.conn);
        f(&guard)
    }

    /// 在单个事务内执行一段 SQL，正常返回后提交；闭包返回 `Err` 时回滚。
    ///
    /// 用 `unchecked_transaction` 是因为连接已由互斥锁独占，且需要允许事务与
    /// 同一连接的普通语句交替使用；`Transaction` 的生命周期被限制在本函数内，
    /// 不会逃逸。
    pub fn with_transaction<T>(
        &self,
        f: impl FnOnce(&Transaction<'_>) -> AppResult<T>,
    ) -> AppResult<T> {
        let guard = locks::lock(&self.conn);
        let tx = guard.unchecked_transaction()?;
        let value = f(&tx)?;
        tx.commit()?;
        Ok(value)
    }
}
