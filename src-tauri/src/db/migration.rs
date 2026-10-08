//! 文件功能：数据库迁移执行器。
//!
//! - 基线 §18：Schema 只能通过 Migration 修改，迁移必须版本化；
//! - 使用 SQLite `PRAGMA user_version` 记录已应用版本；
//! - 每个迁移在独立事务中执行，成功后提升版本号；失败则整体回滚，不破坏原有数据；
//! - 迁移 SQL 在编译期通过 include_str! 内嵌，避免运行时找不到文件。
use rusqlite::Connection;

use super::Db;
use crate::error::{AppError, AppResult};

/// 有序迁移列表：(版本号, 迁移 SQL)。新增变更时追加新版本文件，禁止修改旧文件。
const MIGRATIONS: &[(u32, &str)] = &[
    (1, include_str!("../../migrations/0001_init.sql")),
    (2, include_str!("../../migrations/0002_p2p.sql")),
    (3, include_str!("../../migrations/0003_browser.sql")),
    (4, include_str!("../../migrations/0004_share_protocols.sql")),
];

pub fn run_migrations(db: &Db) -> AppResult<()> {
    let mut conn = db.conn();
    let current: u32 = conn
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap_or(0);

    for (version, sql) in MIGRATIONS {
        if *version <= current {
            continue;
        }
        apply_one(&mut conn, *version, sql)?;
        tracing::info!(version = version, "数据库迁移已应用");
    }
    Ok(())
}

fn apply_one(conn: &mut Connection, version: u32, sql: &str) -> AppResult<()> {
    let tx = conn.transaction()?;
    tx.execute_batch(sql)
        .map_err(|err| AppError::Other(format!("迁移 {version:04} 执行失败，已回滚: {err}")))?;
    // user_version 无法在事务内用参数绑定，版本号来自代码常量，安全。
    tx.execute_batch(&format!("PRAGMA user_version = {version}"))?;
    tx.commit()?;
    Ok(())
}
