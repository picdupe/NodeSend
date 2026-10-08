//! 文件功能：共享目录配置仓储（SQLite shares 表的读写）。
//!
//! - 每个函数对应一类数据操作：列表、按 ID/名称查询、创建、更新、删除、启停；
//! - 不做路径安全检查（由 httpapi/security 负责），不处理 HTTP（职责分离，基线 §53）；
//! - 创建时 token 模式自动生成 256-bit 随机令牌（十六进制）。
use chrono::Utc;
use rand::rngs::OsRng;
use rand::RngCore;
use rusqlite::params;

use super::{AccessMode, ShareConfig};
use crate::db::Db;
use crate::error::{AppError, AppResult};

const COLUMNS: &str =
    "id, name, path, enabled, access_mode, access_token, created_at, updated_at, protocol, port";

pub fn set_protocol(
    db: &Db,
    id: &str,
    protocol: super::ShareProtocol,
    port: u16,
) -> AppResult<ShareConfig> {
    db.conn().execute(
        "UPDATE shares SET protocol=?2, port=?3 WHERE id=?1",
        params![id, protocol.as_str(), port],
    )?;
    get_by_id(db, id)
}

pub fn list_all(db: &Db) -> AppResult<Vec<ShareConfig>> {
    let conn = db.conn();
    let mut stmt = conn.prepare(&format!(
        "SELECT {COLUMNS} FROM shares ORDER BY created_at DESC"
    ))?;
    let rows = stmt.query_map([], row_to_config)?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

pub fn get_by_id(db: &Db, id: &str) -> AppResult<ShareConfig> {
    let conn = db.conn();
    conn.query_row(
        &format!("SELECT {COLUMNS} FROM shares WHERE id = ?1"),
        params![id],
        row_to_config,
    )
    .map_err(|_| AppError::NotFound(format!("共享 {id} 不存在")))
}

pub fn get_by_name(db: &Db, name: &str) -> AppResult<Option<ShareConfig>> {
    let conn = db.conn();
    let mut stmt = conn.prepare(&format!("SELECT {COLUMNS} FROM shares WHERE name = ?1"))?;
    let mut rows = stmt.query_map(params![name], row_to_config)?;
    Ok(rows.next().transpose()?)
}

/// 创建共享。`id` 由调用方（IPC 命令）生成；token 自动生成。
pub fn create(
    db: &Db,
    id: &str,
    name: &str,
    path: &str,
    access_mode: AccessMode,
) -> AppResult<ShareConfig> {
    if name.trim().is_empty() {
        return Err(AppError::BadRequest("共享名称不能为空".into()));
    }
    if get_by_name(db, name)?.is_some() {
        return Err(AppError::Conflict(format!("共享名称 {name} 已存在")));
    }
    if !std::path::Path::is_dir(std::path::Path::new(path)) {
        return Err(AppError::BadRequest(format!(
            "目录不存在或不是目录: {path}"
        )));
    }

    let token = if access_mode == AccessMode::Token {
        Some(generate_token())
    } else {
        None
    };
    let now = Utc::now().to_rfc3339();
    {
        let conn = db.conn();
        conn.execute(
            "INSERT INTO shares
                (id, name, path, enabled, access_mode, access_token, created_at, updated_at)
             VALUES (?1, ?2, ?3, 1, ?4, ?5, ?6, ?6)",
            params![id, name, path, access_mode.as_str(), token, now],
        )?;
    }
    get_by_id(db, id)
}

/// 更新名称 / 路径 / 访问模式；切换到 token 模式且无令牌时自动补发。
pub fn update(
    db: &Db,
    id: &str,
    name: Option<String>,
    path: Option<String>,
    access_mode: Option<AccessMode>,
) -> AppResult<ShareConfig> {
    let mut current = get_by_id(db, id)?;
    if let Some(new_name) = name {
        if new_name.trim().is_empty() {
            return Err(AppError::BadRequest("共享名称不能为空".into()));
        }
        if new_name != current.name {
            if get_by_name(db, &new_name)?.is_some() {
                return Err(AppError::Conflict(format!("共享名称 {new_name} 已存在")));
            }
            current.name = new_name;
        }
    }
    if let Some(new_path) = path {
        if !std::path::Path::is_dir(std::path::Path::new(&new_path)) {
            return Err(AppError::BadRequest(format!("目录不存在: {new_path}")));
        }
        current.path = new_path;
    }
    if let Some(mode) = access_mode {
        current.access_mode = mode;
        if mode == AccessMode::Token && current.access_token.is_none() {
            current.access_token = Some(generate_token());
        }
        if mode != AccessMode::Token {
            current.access_token = None;
        }
    }
    current.updated_at = Utc::now().to_rfc3339();

    let conn = db.conn();
    conn.execute(
        "UPDATE shares SET name=?2, path=?3, access_mode=?4,
             access_token=?5, updated_at=?6 WHERE id=?1",
        params![
            current.id,
            current.name,
            current.path,
            current.access_mode.as_str(),
            current.access_token,
            current.updated_at
        ],
    )?;
    drop(conn);
    get_by_id(db, id)
}

pub fn set_enabled(db: &Db, id: &str, enabled: bool) -> AppResult<ShareConfig> {
    get_by_id(db, id)?;
    let now = Utc::now().to_rfc3339();
    let conn = db.conn();
    conn.execute(
        "UPDATE shares SET enabled=?2, updated_at=?3 WHERE id=?1",
        params![id, enabled as i64, now],
    )?;
    drop(conn);
    get_by_id(db, id)
}

/// 轮换令牌（重新生成），返回新配置。
pub fn rotate_token(db: &Db, id: &str) -> AppResult<ShareConfig> {
    let mut current = get_by_id(db, id)?;
    current.access_token = Some(generate_token());
    current.updated_at = Utc::now().to_rfc3339();
    let conn = db.conn();
    conn.execute(
        "UPDATE shares SET access_token=?2, updated_at=?3 WHERE id=?1",
        params![current.id, current.access_token, current.updated_at],
    )?;
    drop(conn);
    get_by_id(db, id)
}

pub fn delete(db: &Db, id: &str) -> AppResult<()> {
    get_by_id(db, id)?;
    let conn = db.conn();
    conn.execute("DELETE FROM shares WHERE id=?1", params![id])?;
    Ok(())
}

fn row_to_config(row: &rusqlite::Row<'_>) -> rusqlite::Result<ShareConfig> {
    let mode_str: String = row.get(4)?;
    Ok(ShareConfig {
        id: row.get(0)?,
        name: row.get(1)?,
        path: row.get(2)?,
        enabled: row.get::<_, i64>(3)? != 0,
        access_mode: AccessMode::parse(&mode_str),
        access_token: row.get(5)?,
        created_at: row.get(6)?,
        updated_at: row.get(7)?,
        protocol: match row.get::<_, String>(8)?.as_str() {
            "webdav" => super::ShareProtocol::Webdav,
            "smb" => super::ShareProtocol::Smb,
            "ftp" => super::ShareProtocol::Ftp,
            _ => super::ShareProtocol::Nodesend,
        },
        port: row.get(9)?,
    })
}

fn generate_token() -> String {
    let mut bytes = [0u8; 32];
    OsRng.fill_bytes(&mut bytes);
    bytes.iter().map(|b| format!("{b:02x}")).collect::<String>()
}
