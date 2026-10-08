//! 文件功能：QA Integration API（基线 §49）。
//!
//! - 接口：GET /api/v1/integration/share-url?share_id=&name=&path=
//! - 返回指定共享对象的"动态 HTTP URL"，QA 系统可直接使用该 URL 访问文件；
//! - 重名时返回全部：初版以 share_id 精确指定共享；name 仅用于回显/二次校验；
//! - token 模式的共享，鉴权通过后会把 access_token 拼入 URL，便于 QA 直接下载；
//! - 认证方式（基线 §49 标注"尚未最终确认"）：初版先复用共享本身的访问策略。
use axum::{
    extract::{Host, Query, State},
    http::HeaderMap,
    Json,
};
use serde::{Deserialize, Serialize};

use super::super::HttpState;
use crate::access::{authorize, Caller};
use crate::error::{AppError, AppResult};
use crate::httpapi::security;
use crate::share::repository as share_repo;
use crate::share::AccessMode;

#[derive(Debug, Deserialize)]
pub struct IntegrationQuery {
    pub share_id: Option<String>,
    pub name: Option<String>,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub access_token: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ShareUrlResponse {
    share_id: String,
    name: String,
    path: String,
    url: String,
}

/// GET /api/v1/integration/share-url
pub async fn share_url(
    Host(host): Host,
    State(state): State<HttpState>,
    Query(query): Query<IntegrationQuery>,
    headers: HeaderMap,
) -> AppResult<Json<ShareUrlResponse>> {
    let share_id = query
        .share_id
        .ok_or_else(|| AppError::BadRequest("缺少 share_id 参数".into()))?;

    let share = share_repo::get_by_id(&state.db, &share_id)?;
    if share.protocol != crate::share::ShareProtocol::Nodesend {
        return Err(AppError::NotFound("请使用该共享配置的协议访问".into()));
    }
    if let Some(name) = &query.name {
        if name != &share.name {
            return Err(AppError::BadRequest(format!(
                "name 与共享名称不匹配: 期望 {}，实际 {name}",
                share.name
            )));
        }
    }

    let caller = Caller::new(
        headers
            .get("x-nodesend-node-id")
            .and_then(|v| v.to_str().ok())
            .map(|v| v.to_string()),
        query.access_token.clone(),
    );
    authorize(&share, &caller, &state.db)?;

    let virtual_path = query.path.clone().unwrap_or_default();
    // 确认目标真实存在且未越界（URL 只签发存在的对象）。
    let real = security::resolve_existing(share.path_ref(), &virtual_path)?;
    let target = if real.is_dir() { "list" } else { "download" };

    let mut url = format!(
        "http://{host}/api/v1/shares/{}/{target}?path={}",
        share.id,
        url_encode(&virtual_path)
    );
    if share.access_mode == AccessMode::Token {
        if let Some(token) = &share.access_token {
            url.push_str(&format!("&access_token={token}"));
        }
    }

    Ok(Json(ShareUrlResponse {
        share_id: share.id,
        name: share.name,
        path: virtual_path,
        url,
    }))
}

/// 百分号编码（路径用；保留 / 以维持目录层级）。
///
/// path 位于查询参数中，必须编码 &、=、+ 等字符，避免参数截断或加号被解码为空格。
fn url_encode(value: &str) -> String {
    const KEEP: &[u8] = b"-_.~/";
    let mut out = String::new();
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || KEEP.contains(&byte) {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}
