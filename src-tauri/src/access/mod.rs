//! 文件功能：共享访问控制（基线 §16）。
//!
//! - 访问者两类：识别出的 NodeSend Node（携带 X-NodeSend-Node-ID）与匿名第三方；
//! - 根据共享的 [`AccessMode`] 判定：open / token / trusted / deny；
//! - trusted 模式查 trusted_nodes 表；token 模式比对共享令牌；
//! - 只影响新访问/新请求，已接受任务不受影响（基线 §16，任务体系为后续迭代）。
use crate::db::Db;
use crate::error::{AppError, AppResult};
use crate::share::{AccessMode, ShareConfig};

/// 一次外部访问的调用方信息（由 HTTP 层从请求头/查询串提取）。
#[derive(Debug, Default)]
pub struct Caller {
    /// X-NodeSend-Node-ID 头：对端是 NodeSend Node 时存在
    pub node_id: Option<String>,
    /// access_token 查询参数或 Authorization: Bearer 头
    pub token: Option<String>,
}

impl Caller {
    pub fn new(node_id: Option<String>, token: Option<String>) -> Self {
        Self { node_id, token }
    }
}

/// 按共享访问模式鉴权，失败返回 [`AppError::Forbidden`]。
pub fn authorize(share: &ShareConfig, caller: &Caller, db: &Db) -> AppResult<()> {
    if !share.enabled {
        return Err(AppError::NotFound(format!("共享 {} 已禁用", share.name)));
    }

    match share.access_mode {
        AccessMode::Deny => Err(AppError::Forbidden("该共享禁止所有访问".into())),
        AccessMode::Open => Ok(()),
        AccessMode::Token => {
            let expected = share.access_token.as_deref().unwrap_or_default();
            if expected.is_empty() {
                return Err(AppError::Forbidden("共享未配置访问令牌".into()));
            }
            match &caller.token {
                Some(token) if constant_time_eq(token.as_bytes(), expected.as_bytes()) => Ok(()),
                _ => Err(AppError::Forbidden("访问令牌缺失或不正确".into())),
            }
        }
        AccessMode::Trusted => match &caller.node_id {
            Some(node_id) if is_trusted(db, node_id) => Ok(()),
            Some(_) => Err(AppError::Forbidden("该 Node 不在信任列表中".into())),
            None => Err(AppError::Forbidden("无法识别 Node 身份，拒绝访问".into())),
        },
    }
}

fn is_trusted(db: &Db, node_id: &str) -> bool {
    let conn = db.conn();
    conn.query_row(
        "SELECT 1 FROM trusted_nodes WHERE node_id=?1",
        rusqlite::params![node_id],
        |_| Ok(()),
    )
    .is_ok()
}

/// 定长比较，避免令牌比较的计时侧信道（不等长时仍很快返回，仅初版防护）。
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter()
        .zip(b.iter())
        .fold(0u8, |acc, (x, y)| acc | (x ^ y))
        == 0
}
