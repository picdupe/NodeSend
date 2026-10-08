//! 文件功能：Node Trust 关系管理（基线 §3 Trust、§16、§4 Node Registry 的初版子集）。
//!
//! - trusted_nodes 表的增删查；
//! - Trust 以 Node ID 为单位：同 Node ID 换网络不重新 Trust；
//! - Node 删除（remove）后再次发现相同 ID 需要重新 Trust（基线 §3）；
//! - 完整 Node Registry（endpoint、别名、标签分组等）在后续迭代实现（§4）。
use chrono::Utc;
use rusqlite::params;
use serde::Serialize;

use crate::db::Db;
use crate::error::{AppError, AppResult};

#[derive(Debug, Serialize)]
pub struct TrustedNode {
    pub node_id: String,
    pub display_name: Option<String>,
    pub created_at: String,
}

pub fn list(db: &Db) -> AppResult<Vec<TrustedNode>> {
    let conn = db.conn();
    let mut stmt = conn.prepare(
        "SELECT node_id, display_name, created_at
         FROM trusted_nodes ORDER BY created_at DESC",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok(TrustedNode {
            node_id: row.get(0)?,
            display_name: row.get(1)?,
            created_at: row.get(2)?,
        })
    })?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

/// 添加 Trust（已存在则更新显示名称）。
pub fn add(db: &Db, node_id: &str, display_name: Option<String>) -> AppResult<()> {
    if node_id.trim().is_empty() {
        return Err(AppError::BadRequest("node_id 不能为空".into()));
    }
    let now = Utc::now().to_rfc3339();
    let conn = db.conn();
    conn.execute(
        "INSERT INTO trusted_nodes (node_id, display_name, created_at)
         VALUES (?1, ?2, ?3)
         ON CONFLICT(node_id) DO UPDATE SET display_name=excluded.display_name",
        params![node_id, display_name, now],
    )?;
    Ok(())
}

/// 移除 Trust。
pub fn remove(db: &Db, node_id: &str) -> AppResult<()> {
    let conn = db.conn();
    let affected = conn.execute(
        "DELETE FROM trusted_nodes WHERE node_id=?1",
        params![node_id],
    )?;
    if affected == 0 {
        return Err(AppError::NotFound(format!("未信任的 Node: {node_id}")));
    }
    conn.execute(
        "DELETE FROM settings WHERE key=?1",
        params![format!("auto_receive_{node_id}")],
    )?;
    Ok(())
}
