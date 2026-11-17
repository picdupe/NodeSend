//! 文件功能：共享目录配置仓储（SQLite shares 表的读写）。
//!
//! - 每个函数对应一类数据操作：列表、按 ID/名称查询、创建、更新、删除、启停；
//! - 不做路径安全检查（由 httpapi/security 负责），不处理 HTTP（职责分离，基线 §53）；
//! - 创建时 token 模式自动生成 256-bit 随机令牌（十六进制）。
use chrono::Utc;
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use rand::rngs::OsRng;
use rand::RngCore;
use rusqlite::{params, Transaction};

use super::{AccessMode, ShareConfig};
use crate::db::Db;
use crate::error::{AppError, AppResult};

const COLUMNS: &str =
    "id, name, path, enabled, access_mode, access_token, created_at, updated_at, protocol, port, protocol_share_name, bind_ips, port_explicit, requested_port";
const PASSWORD_CREDENTIAL_PREFIX: &str = "nodesend-password-v1:";

pub fn set_protocol(
    db: &Db,
    id: &str,
    protocol: super::ShareProtocol,
    port: u16,
) -> AppResult<ShareConfig> {
    db.with_conn(|conn| {
        conn.execute(
            "UPDATE shares SET protocol=?2, port=?3, requested_port=CASE WHEN port_explicit=0 THEN ?3 ELSE requested_port END WHERE id=?1",
            params![id, protocol.as_str(), port],
        )?;
        Ok(())
    })?;
    get_by_id(db, id)
}

pub fn validate_bind_ips(value: &str) -> AppResult<String> {
    let mut addresses = Vec::<std::net::IpAddr>::new();
    for part in value.split(',').map(str::trim).filter(|part| !part.is_empty()) {
        let address = part.parse::<std::net::IpAddr>().map_err(|_| {
            AppError::BadRequest(format!("Invalid bind IP address: {part}"))
        })?;
        if !addresses.contains(&address) {
            addresses.push(address);
        }
    }
    if addresses.is_empty() {
        return Err(AppError::BadRequest("At least one bind IP is required".into()));
    }
    let ipv4_wildcard = std::net::IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED);
    let ipv6_wildcard = std::net::IpAddr::V6(std::net::Ipv6Addr::UNSPECIFIED);
    if addresses.contains(&ipv4_wildcard)
        && addresses
            .iter()
            .any(|address| address.is_ipv4() && *address != ipv4_wildcard)
        || addresses.contains(&ipv6_wildcard)
            && addresses
                .iter()
                .any(|address| address.is_ipv6() && *address != ipv6_wildcard)
    {
        return Err(AppError::BadRequest(
            "A wildcard bind IP cannot be combined with a specific IP of the same family".into(),
        ));
    }
    Ok(addresses
        .into_iter()
        .map(|address| address.to_string())
        .collect::<Vec<_>>()
        .join(","))
}

pub fn list_all(db: &Db) -> AppResult<Vec<ShareConfig>> {
    db.with_conn(|conn| {
        let mut stmt = conn.prepare(&format!(
            "SELECT {COLUMNS} FROM shares ORDER BY created_at DESC"
        ))?;
        let rows = stmt.query_map([], row_to_config)?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    })
}

pub fn get_by_id(db: &Db, id: &str) -> AppResult<ShareConfig> {
    db.with_conn(|conn| {
        conn.query_row(
            &format!("SELECT {COLUMNS} FROM shares WHERE id = ?1"),
            params![id],
            row_to_config,
        )
        .map_err(|error| match error {
            rusqlite::Error::QueryReturnedNoRows => {
                AppError::NotFound(format!("Share {id} does not exist"))
            }
            error => AppError::Db(error),
        })
    })
}

pub fn get_by_protocol_share_name(
    db: &Db,
    share_name: &str,
    protocol: super::ShareProtocol,
    port: u16,
) -> AppResult<Option<ShareConfig>> {
    let matching = list_all(db)?
        .into_iter()
        .filter(|share| {
            share.enabled
                && share.protocol == protocol
                && share.port == port
                && share.protocol_share_name.eq_ignore_ascii_case(share_name)
        })
        .collect::<Vec<_>>();
    Ok((matching.len() == 1).then(|| matching.into_iter().next()).flatten())
}

fn allocate_protocol_share_name(
    tx: &Transaction<'_>,
    path: &str,
    protocol: super::ShareProtocol,
) -> AppResult<String> {
    let folder_name = path
        .trim_end_matches(['\\', '/'])
        .rsplit(['\\', '/'])
        .next()
        .filter(|name| !name.is_empty())
        .unwrap_or("share")
        .to_owned();
    let existing = {
        let mut stmt = tx.prepare(
            "SELECT protocol_share_name FROM shares WHERE protocol=?1",
        )?;
        let rows = stmt.query_map([protocol.as_str()], |row| row.get::<_, String>(0))?;
        rows.collect::<Result<std::collections::HashSet<_>, _>>()?
            .into_iter()
            .map(|name| name.to_lowercase())
            .collect::<std::collections::HashSet<_>>()
    };
    Ok(next_protocol_share_name(&folder_name, &existing))
}

fn next_protocol_share_name(
    folder_name: &str,
    existing: &std::collections::HashSet<String>,
) -> String {
    if !existing.contains(&folder_name.to_lowercase()) {
        return folder_name.to_owned();
    }
    for suffix in 2u64.. {
        let candidate = format!("{folder_name}-{suffix}");
        if !existing.contains(&candidate.to_lowercase()) {
            return candidate;
        }
    }
    unreachable!("share name suffix exhausted")
}

/// Fill missing protocol share names from directory names without changing assigned names.
pub fn initialize_protocol_share_names(db: &Db) -> AppResult<()> {
    db.with_transaction(|tx| {
        let rows = {
            let mut stmt = tx.prepare(
                "SELECT id, protocol, path, protocol_share_name FROM shares ORDER BY created_at, id",
            )?;
            let rows = stmt.query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                ))
            })?;
            rows.collect::<Result<Vec<_>, _>>()?
        };
        let mut used = std::collections::HashSet::new();
        for (id, protocol, path, saved_name) in rows {
            let directory_name = path
                .trim_end_matches(['\\', '/'])
                .rsplit(['\\', '/'])
                .find(|part| !part.is_empty())
                .unwrap_or("share");
            let key = |name: &str| (protocol.clone(), name.to_lowercase());
            let protocol_share_name = if !saved_name.is_empty() && !used.contains(&key(&saved_name))
            {
                saved_name.clone()
            } else {
                let used_names = used
                    .iter()
                    .filter(|(used_protocol, _)| used_protocol == &protocol)
                    .map(|(_, used_name)| used_name.clone())
                    .collect();
                next_protocol_share_name(directory_name, &used_names)
            };
            if protocol_share_name != saved_name {
                tx.execute(
                    "UPDATE shares SET protocol_share_name=?2 WHERE id=?1",
                    params![id, protocol_share_name],
                )?;
            }
            used.insert(key(&protocol_share_name));
        }
        tx.execute_batch(
            "CREATE UNIQUE INDEX IF NOT EXISTS idx_shares_protocol_share_name \
             ON shares(protocol, protocol_share_name COLLATE NOCASE)",
        )?;
        Ok(())
    })
}

fn ensure_path_available_in_transaction(
    tx: &Transaction<'_>,
    canonical_path: &std::path::Path,
    protocol: super::ShareProtocol,
    except_id: Option<&str>,
) -> AppResult<()> {
    fn path_key(path: &std::path::Path) -> String {
        let value = path.to_string_lossy().replace('/', "\\");
        #[cfg(windows)]
        {
            value.to_lowercase()
        }
        #[cfg(not(windows))]
        {
            value
        }
    }

    let key = path_key(canonical_path);
    let mut stmt = tx.prepare("SELECT id, path FROM shares WHERE protocol=?1")?;
    let rows = stmt.query_map([protocol.as_str()], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    for existing_row in rows {
        let (id, existing_path) = existing_row?;
        if Some(id.as_str()) == except_id {
            continue;
        }
        if let Ok(existing) = std::fs::canonicalize(existing_path) {
            if path_key(&existing) == key {
                return Err(AppError::Conflict("SHARE_PATH_PROTOCOL_EXISTS".into()));
            }
        }
    }
    Ok(())
}

/// 创建共享。`id` 由调用方（IPC 命令）生成；token 自动生成。
pub fn create(
    db: &Db,
    id: &str,
    name: &str,
    path: &str,
    protocol: super::ShareProtocol,
    port: u16,
    bind_ips: &str,
    access_mode: AccessMode,
    access_username: Option<&str>,
    access_password: Option<&str>,
) -> AppResult<ShareConfig> {
    if name.trim().is_empty() {
        return Err(AppError::BadRequest("Share name cannot be empty".into()));
    }
    if !std::path::Path::is_dir(std::path::Path::new(path)) {
        return Err(AppError::BadRequest(format!(
            "Directory does not exist or is not a directory: {path}"
        )));
    }
    let canonical_path = std::fs::canonicalize(path)?;
    let bind_ips = validate_bind_ips(bind_ips)?;

    let token = match access_mode {
        AccessMode::Token => Some(generate_token()),
        AccessMode::Password => {
            let username = access_username
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .ok_or_else(|| AppError::BadRequest("Username is required".into()))?;
            let password = access_password
                .filter(|value| !value.is_empty())
                .ok_or_else(|| AppError::BadRequest("Password is required".into()))?;
            Some(encode_password_credential(username, password)?)
        }
        _ => None,
    };
    let now = Utc::now().to_rfc3339();
    db.with_transaction(|tx| {
        ensure_path_available_in_transaction(tx, &canonical_path, protocol, None)?;
        let protocol_share_name = allocate_protocol_share_name(tx, path, protocol)?;
        tx.execute(
            "INSERT INTO shares
                (id, name, path, enabled, access_mode, access_token, created_at, updated_at, protocol, port, protocol_share_name, bind_ips, port_explicit, requested_port)
             VALUES (?1, ?2, ?3, 1, ?4, ?5, ?6, ?6, ?7, ?8, ?9, ?10, 0, ?8)",
            params![id, name, path, access_mode.as_str(), token, now, protocol.as_str(), port, protocol_share_name, bind_ips],
        )?;
        Ok(())
    })?;
    get_by_id(db, id)
}

pub fn update_connection(
    db: &Db,
    id: &str,
    username: Option<&str>,
    password: Option<&str>,
    port: u16,
    bind_ips: &str,
) -> AppResult<ShareConfig> {
    let mut current = get_by_id(db, id)?;
    if current.protocol == super::ShareProtocol::Nodesend {
        return Err(AppError::BadRequest(
            "NodeSend shares do not have a protocol listener to edit".into(),
        ));
    }
    let bind_ips = validate_bind_ips(bind_ips)?;
    let port_explicit = current.port_explicit || port != current.port;
    let requested_port = if port != current.port { port } else { current.requested_port };
    let stored_credential = match current.access_mode {
        AccessMode::Password => {
            let username = username
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_owned)
                .or_else(|| current.access_username.clone())
                .ok_or_else(|| AppError::BadRequest("Username is required".into()))?;
            let password = password
                .filter(|value| !value.is_empty())
                .map(str::to_owned)
                .or_else(|| current.access_token.clone())
                .ok_or_else(|| AppError::BadRequest("Password is required".into()))?;
            let stored = encode_password_credential(&username, &password)?;
            current.access_username = Some(username);
            current.access_token = Some(password);
            Some(stored)
        }
        AccessMode::Token => current.access_token.clone(),
        _ => None,
    };
    current.port = port;
    current.bind_ips = bind_ips.clone();
    current.updated_at = Utc::now().to_rfc3339();
    db.with_conn(|conn| {
        conn.execute(
            "UPDATE shares SET access_token=?2, port=?3, bind_ips=?4, updated_at=?5, port_explicit=?6, requested_port=?7 WHERE id=?1",
            params![id, stored_credential, port, bind_ips, current.updated_at, port_explicit, requested_port],
        )?;
        Ok(())
    })?;
    get_by_id(db, id)
}

pub fn restore_share_config(db: &Db, previous: &ShareConfig) -> AppResult<ShareConfig> {
    let stored_credential = match previous.access_mode {
        AccessMode::Password => match (
            previous.access_username.as_deref(),
            previous.access_token.as_deref(),
        ) {
            (Some(username), Some(password)) => {
                Some(encode_password_credential(username, password)?)
            }
            _ => None,
        },
        AccessMode::Token => previous.access_token.clone(),
        _ => None,
    };
    let updated_at = Utc::now().to_rfc3339();
    db.with_transaction(|tx| {
        tx.execute(
            "UPDATE shares SET name=?2, path=?3, enabled=?4, access_mode=?5, access_token=?6, updated_at=?7, protocol=?8, port=?9, protocol_share_name=?10, bind_ips=?11, port_explicit=?12, requested_port=?13 WHERE id=?1",
            params![
                previous.id,
                previous.name,
                previous.path,
                previous.enabled,
                previous.access_mode.as_str(),
                stored_credential,
                updated_at,
                previous.protocol.as_str(),
                previous.port,
                previous.protocol_share_name,
                previous.bind_ips,
                previous.port_explicit,
                previous.requested_port,
            ],
        )?;
        Ok(())
    })?;
    get_by_id(db, &previous.id)
}

/// 更新名称 / 路径 / 访问模式；切换到受密码保护的模式时自动生成凭据。
pub fn update(
    db: &Db,
    id: &str,
    name: Option<String>,
    path: Option<String>,
    access_mode: Option<AccessMode>,
) -> AppResult<ShareConfig> {
    let mut current = get_by_id(db, id)?;
    let mut canonical_path = None;
    if let Some(new_name) = name {
        if new_name.trim().is_empty() {
            return Err(AppError::BadRequest("Share name cannot be empty".into()));
        }
        if new_name != current.name {
            current.name = new_name;
        }
    }
    if let Some(new_path) = path {
        if !std::path::Path::is_dir(std::path::Path::new(&new_path)) {
            return Err(AppError::BadRequest(format!(
                "Directory does not exist: {new_path}"
            )));
        }
        canonical_path = Some(std::fs::canonicalize(&new_path)?);
        current.path = new_path;
    }
    if let Some(mode) = access_mode {
        let mode_changed = current.access_mode != mode;
        if mode_changed && mode == AccessMode::Password {
            return Err(AppError::BadRequest(
                "Create a password share with a username and password".into(),
            ));
        }
        current.access_mode = mode;
        if mode == AccessMode::Token && (mode_changed || current.access_token.is_none()) {
            current.access_token = Some(generate_token());
        }
        current.access_username = if mode == AccessMode::Password {
            current.access_username
        } else {
            None
        };
        if !matches!(mode, AccessMode::Token | AccessMode::Password) {
            current.access_token = None;
            current.access_username = None;
        }
    }
    current.updated_at = Utc::now().to_rfc3339();

    let stored_credential = if current.access_mode == AccessMode::Password {
        match (current.access_username.as_deref(), current.access_token.as_deref()) {
            (Some(username), Some(password)) => Some(encode_password_credential(username, password)?),
            _ => None,
        }
    } else {
        current.access_token.clone()
    };
    db.with_transaction(|tx| {
        if let Some(canonical_path) = canonical_path.as_deref() {
            ensure_path_available_in_transaction(
                tx,
                canonical_path,
                current.protocol,
                Some(id),
            )?;
        }
        tx.execute(
            "UPDATE shares SET name=?2, path=?3, access_mode=?4,
             access_token=?5, updated_at=?6 WHERE id=?1",
            params![
                current.id,
                current.name,
                current.path,
                current.access_mode.as_str(),
                stored_credential,
                current.updated_at
            ],
        )?;
        Ok(())
    })?;
    get_by_id(db, id)
}

pub fn set_enabled(db: &Db, id: &str, enabled: bool) -> AppResult<ShareConfig> {
    get_by_id(db, id)?;
    let now = Utc::now().to_rfc3339();
    db.with_conn(|conn| {
        conn.execute(
            "UPDATE shares SET enabled=?2, updated_at=?3 WHERE id=?1",
            params![id, enabled as i64, now],
        )?;
        Ok(())
    })?;
    get_by_id(db, id)
}

/// 轮换令牌（重新生成），返回新配置。
pub fn rotate_token(db: &Db, id: &str) -> AppResult<ShareConfig> {
    let mut current = get_by_id(db, id)?;
    if current.access_mode != AccessMode::Token {
        return Err(AppError::BadRequest(
            "Only temporary token shares can rotate a token".into(),
        ));
    }
    current.access_token = Some(generate_token());
    current.updated_at = Utc::now().to_rfc3339();
    db.with_conn(|conn| {
        conn.execute(
            "UPDATE shares SET access_token=?2, updated_at=?3 WHERE id=?1",
            params![current.id, current.access_token, current.updated_at],
        )?;
        Ok(())
    })?;
    get_by_id(db, id)
}

pub fn change_password(db: &Db, id: &str, password: &str) -> AppResult<ShareConfig> {
    if password.is_empty() {
        return Err(AppError::BadRequest("Password is required".into()));
    }
    let mut current = get_by_id(db, id)?;
    if current.access_mode != AccessMode::Password {
        return Err(AppError::BadRequest(
            "Only password shares can change their password".into(),
        ));
    }
    let username = current
        .access_username
        .as_deref()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| AppError::BadRequest("Password share username is missing".into()))?;
    current.access_token = Some(password.to_owned());
    current.updated_at = Utc::now().to_rfc3339();
    let stored = encode_password_credential(username, password)?;
    db.with_conn(|conn| {
        conn.execute(
            "UPDATE shares SET access_token=?2, updated_at=?3 WHERE id=?1",
            params![current.id, stored, current.updated_at],
        )?;
        Ok(())
    })?;
    get_by_id(db, id)
}

pub fn delete(db: &Db, id: &str) -> AppResult<()> {
    get_by_id(db, id)?;
    db.with_conn(|conn| {
        conn.execute("DELETE FROM shares WHERE id=?1", params![id])?;
        Ok(())
    })
}

fn row_to_config(row: &rusqlite::Row<'_>) -> rusqlite::Result<ShareConfig> {
    let mode_str: String = row.get(4)?;
    let protocol = match row.get::<_, String>(8)?.as_str() {
        "http" => super::ShareProtocol::Http,
        "webdav" => super::ShareProtocol::Http,
        "smb" => super::ShareProtocol::Smb,
        "ftp" => super::ShareProtocol::Ftp,
        _ => super::ShareProtocol::Nodesend,
    };
    let access_mode = AccessMode::parse(&mode_str, protocol);
    let stored_credential: Option<String> = row.get(5)?;
    let (access_username, access_token) = decode_access_credential(stored_credential, access_mode);
    Ok(ShareConfig {
        id: row.get(0)?,
        name: row.get(1)?,
        path: row.get(2)?,
        enabled: row.get::<_, i64>(3)? != 0,
        access_mode,
        access_token,
        access_username,
        created_at: row.get(6)?,
        updated_at: row.get(7)?,
        protocol,
        port: row.get(9)?,
        protocol_share_name: row.get(10)?,
        bind_ips: row.get(11)?,
        port_explicit: row.get(12)?,
        requested_port: row.get(13)?,
    })
}

fn encode_password_credential(username: &str, password: &str) -> AppResult<String> {
    let json = serde_json::to_vec(&(username, password))?;
    Ok(format!(
        "{PASSWORD_CREDENTIAL_PREFIX}{}",
        URL_SAFE_NO_PAD.encode(json)
    ))
}

fn decode_access_credential(
    stored: Option<String>,
    access_mode: AccessMode,
) -> (Option<String>, Option<String>) {
    let Some(stored) = stored else {
        return (None, None);
    };
    if access_mode == AccessMode::Password {
        if let Some(encoded) = stored.strip_prefix(PASSWORD_CREDENTIAL_PREFIX) {
            if let Ok(bytes) = URL_SAFE_NO_PAD.decode(encoded) {
                if let Ok((username, password)) =
                    serde_json::from_slice::<(String, String)>(&bytes)
                {
                    return (Some(username), Some(password));
                }
            }
        }
        return (None, None);
    }
    (None, Some(stored))
}

fn generate_token() -> String {
    let mut bytes = [0u8; 32];
    OsRng.fill_bytes(&mut bytes);
    bytes.iter().map(|b| format!("{b:02x}")).collect::<String>()
}
