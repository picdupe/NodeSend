//! 文件功能：共享中心模块入口（基线 §11）。
//!
//! - 定义共享目录配置 [`ShareConfig`] 与访问模式 [`AccessMode`]；
//! - 仓储函数（list/get/create/update/delete）操作 SQLite shares 表；
//! - 共享的是"虚拟根"，对外只暴露共享名称与虚拟相对路径，不暴露绝对路径；
//! - 父子目录可同时共享、watcher 增量索引等能力为后续迭代（见 §11、§50）。
pub mod repository;
pub mod servers;
mod ftp_fs;
mod webdav_fs;

use serde::{Deserialize, Serialize};

pub const SHARE_CONFIG_FILE: &str = "desktop.ini";

pub fn resolve_share_name(
    path: &str,
    requested_name: Option<&str>,
) -> crate::error::AppResult<String> {
    if let Some(name) = requested_name
        .map(str::trim)
        .filter(|name| !name.is_empty())
    {
        return Ok(name.to_owned());
    }

    let directory = std::path::Path::new(path);
    if !directory.is_dir() {
        return Err(crate::error::AppError::BadRequest(
            "Shared path does not point to a directory".into(),
        ));
    }

    if let Some(config_path) = std::fs::read_dir(directory)?.flatten().find_map(|entry| {
        entry
            .file_name()
            .to_str()
            .filter(|name| name.eq_ignore_ascii_case(SHARE_CONFIG_FILE))
            .map(|_| entry.path())
    }) {
        let bytes = std::fs::read(config_path)?;
        let contents = decode_desktop_ini(&bytes)?;
        if let Some(name) = desktop_ini_share_name(&contents) {
            return Ok(name);
        }
    }

    directory
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.trim().is_empty())
        .map(str::to_owned)
        .or_else(|| {
            directory
                .to_string_lossy()
                .trim_end_matches(|character| character == '\\' || character == '/')
                .rsplit(|character| character == '\\' || character == '/')
                .next()
                .filter(|name| !name.trim().is_empty())
                .map(str::to_owned)
        })
        .ok_or_else(|| {
            crate::error::AppError::BadRequest("Unable to determine the share name".into())
        })
}

fn decode_desktop_ini(bytes: &[u8]) -> crate::error::AppResult<String> {
    if let Some(bytes) = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
        return String::from_utf8(bytes.to_vec()).map_err(|error| {
            crate::error::AppError::BadRequest(format!("Invalid {SHARE_CONFIG_FILE}: {error}"))
        });
    }
    if let Some(bytes) = bytes.strip_prefix(&[0xFF, 0xFE]) {
        let words = bytes
            .chunks_exact(2)
            .map(|chunk| u16::from_le_bytes([chunk[0], chunk[1]]))
            .collect::<Vec<_>>();
        return String::from_utf16(&words).map_err(|error| {
            crate::error::AppError::BadRequest(format!("Invalid {SHARE_CONFIG_FILE}: {error}"))
        });
    }
    if let Some(bytes) = bytes.strip_prefix(&[0xFE, 0xFF]) {
        let words = bytes
            .chunks_exact(2)
            .map(|chunk| u16::from_be_bytes([chunk[0], chunk[1]]))
            .collect::<Vec<_>>();
        return String::from_utf16(&words).map_err(|error| {
            crate::error::AppError::BadRequest(format!("Invalid {SHARE_CONFIG_FILE}: {error}"))
        });
    }
    String::from_utf8(bytes.to_vec()).map_err(|error| {
        crate::error::AppError::BadRequest(format!("Invalid {SHARE_CONFIG_FILE}: {error}"))
    })
}

fn desktop_ini_share_name(contents: &str) -> Option<String> {
    let mut in_nodesend_section = false;
    for line in contents.lines().map(str::trim) {
        if line.is_empty() || line.starts_with(';') || line.starts_with('#') {
            continue;
        }
        if line.starts_with('[') && line.ends_with(']') {
            in_nodesend_section = line[1..line.len() - 1]
                .trim()
                .eq_ignore_ascii_case("NodeSend");
            continue;
        }
        if !in_nodesend_section {
            continue;
        }
        if let Some((key, value)) = line.split_once('=') {
            if key.trim().eq_ignore_ascii_case("ShareName") {
                let name = value.trim().trim_matches('"').trim();
                if !name.is_empty() {
                    return Some(name.to_owned());
                }
            }
        }
    }
    None
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShareConfig {
    pub id: String,
    pub name: String,
    /// 本机绝对路径（仅服务端使用，HTTP 响应中不返回）
    pub path: String,
    pub enabled: bool,
    pub access_mode: AccessMode,
    pub access_token: Option<String>,
    #[serde(default)]
    pub access_username: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    #[serde(default)]
    pub protocol: ShareProtocol,
    #[serde(default)]
    pub port: u16,
    #[serde(default)]
    pub port_explicit: bool,
    #[serde(default)]
    pub requested_port: u16,
    #[serde(default)]
    pub protocol_share_name: String,
    #[serde(default = "default_bind_ips")]
    pub bind_ips: String,
}

fn default_bind_ips() -> String {
    String::new()
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ShareProtocol {
    #[default]
    Nodesend,
    Http,
    Smb,
    Ftp,
}

impl ShareProtocol {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Nodesend => "nodesend",
            Self::Http => "http",
            Self::Smb => "smb",
            Self::Ftp => "ftp",
        }
    }
}

/// Node 级 / 第三方访问模式（基线 §16）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AccessMode {
    /// 允许任何访问者（仅建议在受信内网使用）
    Open,
    /// URL 中携带访问令牌，适合临时分享
    Token,
    /// 通过网页用户名和密码登录，适合长期共享
    Password,
    /// 仅已信任 Node（trusted_nodes）可访问
    Trusted,
}

impl AccessMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            AccessMode::Open => "open",
            // Distinguish the new temporary-link mode from legacy third-party
            // rows that stored password access as "token".
            AccessMode::Token => "temporary_token",
            AccessMode::Password => "password",
            AccessMode::Trusted => "trusted",
        }
    }

    pub fn parse(value: &str, protocol: ShareProtocol) -> Self {
        match value {
            "open" => AccessMode::Open,
            "password" => AccessMode::Password,
            "temporary_token" => AccessMode::Token,
            "token" if protocol != ShareProtocol::Nodesend => AccessMode::Password,
            "trusted" => AccessMode::Trusted,
            _ => AccessMode::Token,
        }
    }
}

/// 对外 DTO：不包含本机绝对路径与令牌等敏感字段（基线 §11：共享虚拟根）。
#[derive(Debug, Clone, Serialize)]
pub struct SharePublic {
    pub id: String,
    pub name: String,
    pub enabled: bool,
    pub access_mode: AccessMode,
}

impl ShareConfig {
    pub fn to_public(&self) -> SharePublic {
        SharePublic {
            id: self.id.clone(),
            name: self.name.clone(),
            enabled: self.enabled,
            access_mode: self.access_mode,
        }
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    pub fn path_ref(&self) -> &std::path::Path {
        std::path::Path::new(&self.path)
    }
}
