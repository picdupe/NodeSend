//! 文件功能：共享中心模块入口（基线 §11）。
//!
//! - 定义共享目录配置 [`ShareConfig`] 与访问模式 [`AccessMode`]；
//! - 仓储函数（list/get/create/update/delete）操作 SQLite shares 表；
//! - 共享的是"虚拟根"，对外只暴露共享名称与虚拟相对路径，不暴露绝对路径；
//! - 父子目录可同时共享、watcher 增量索引等能力为后续迭代（见 §11、§50）。
pub mod repository;
pub mod servers;
mod webdav_fs;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShareConfig {
    pub id: String,
    pub name: String,
    /// 本机绝对路径（仅服务端使用，HTTP 响应中不返回）
    pub path: String,
    pub enabled: bool,
    pub access_mode: AccessMode,
    pub access_token: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    #[serde(default)]
    pub protocol: ShareProtocol,
    #[serde(default)]
    pub port: u16,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ShareProtocol {
    #[default]
    Nodesend,
    Webdav,
    Smb,
    Ftp,
}

impl ShareProtocol {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Nodesend => "nodesend",
            Self::Webdav => "webdav",
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
    /// 需携带访问令牌（第三方外部调用 HTTP 的主要方式）
    Token,
    /// 仅已信任 Node（trusted_nodes）可访问
    Trusted,
    /// 禁止所有访问
    Deny,
}

impl AccessMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            AccessMode::Open => "open",
            AccessMode::Token => "token",
            AccessMode::Trusted => "trusted",
            AccessMode::Deny => "deny",
        }
    }

    pub fn parse(value: &str) -> Self {
        match value {
            "open" => AccessMode::Open,
            "trusted" => AccessMode::Trusted,
            "deny" => AccessMode::Deny,
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
