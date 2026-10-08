//! 文件功能：应用统一错误类型。
//!
//! - 所有 Service 模块（db / identity / share / httpapi / ipc）统一返回 [`AppError`]；
//! - 同时实现 `serde::Serialize`，使错误可以作为 Tauri IPC 命令的 Err 返回给前端；
//! - HTTP 层（httpapi）会把本错误映射为对应的 HTTP 状态码。
use serde::{ser::SerializeStruct, Serialize, Serializer};

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("数据库错误: {0}")]
    Db(#[from] rusqlite::Error),

    #[error("文件 IO 错误: {0}")]
    Io(#[from] std::io::Error),

    #[error("身份/密钥错误: {0}")]
    Crypto(String),

    #[error("证书错误: {0}")]
    Certificate(String),

    #[error("参数错误: {0}")]
    BadRequest(String),

    #[error("未找到: {0}")]
    NotFound(String),

    #[error("禁止访问: {0}")]
    Forbidden(String),

    #[error("冲突: {0}")]
    Conflict(String),

    #[error("{0}")]
    Other(String),
}

impl AppError {
    /// 机器可读的错误码（前端可据此分支处理；后续细化见基线 §17 错误码模型）。
    pub fn code(&self) -> &'static str {
        match self {
            AppError::Db(_) => "DB_ERROR",
            AppError::Io(_) => "IO_ERROR",
            AppError::Crypto(_) => "CRYPTO_ERROR",
            AppError::Certificate(_) => "CERT_ERROR",
            AppError::BadRequest(_) => "BAD_REQUEST",
            AppError::NotFound(_) => "NOT_FOUND",
            AppError::Forbidden(_) => "FORBIDDEN",
            AppError::Conflict(_) => "CONFLICT",
            AppError::Other(_) => "INTERNAL",
        }
    }
}

/// 跨 IPC 传递时序列化为 `{ "code": ..., "message": ... }`。
impl Serialize for AppError {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut state = serializer.serialize_struct("AppError", 2)?;
        state.serialize_field("code", &self.code())?;
        state.serialize_field("message", &self.to_string())?;
        state.end()
    }
}

impl From<serde_json::Error> for AppError {
    fn from(value: serde_json::Error) -> Self {
        AppError::Other(format!("JSON 错误: {value}"))
    }
}

pub type AppResult<T> = Result<T, AppError>;
