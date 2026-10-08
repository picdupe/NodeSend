//! 文件功能：共享相关 HTTP 接口实现（基线 §5 HELLO、§11 共享中心、§12 HTTP）。
//!
//! 接口清单：
//! - GET  /api/v1/hello                    本机身份与能力（node_id/protocol_version/capabilities）
//! - GET  /api/v1/shares                   已启用共享列表（仅虚拟信息）
//! - GET  /api/v1/shares/{id}/tree         ★目录树（path, depth；0=仅根，默认 3，上限 20）
//! - GET  /api/v1/shares/{id}/list         单层目录列表（基于 tree depth=1）
//! - GET  /api/v1/shares/{id}/info         单个文件/目录信息（tree depth=0）
//! - GET  /api/v1/shares/{id}/download     文件下载（流式，支持断点 Range 为后续迭代）
//! - POST /api/v1/shares/{id}/upload       multipart 上传（字段名 file）
//!
//! 鉴权：X-NodeSend-Node-ID 头识别 Node；access_token 查询参数或 Bearer 头提供令牌。
use axum::{
    body::Body,
    extract::{Multipart, Path, Query, State},
    http::{header, HeaderMap},
    response::{IntoResponse, Json, Response},
};
use serde::{Deserialize, Serialize};
use tokio::io::AsyncWriteExt;
use tokio_util::io::ReaderStream;

use super::super::HttpState;
use crate::access::{authorize, Caller};
use crate::error::{AppError, AppResult};
use crate::httpapi::{security, tree};
use crate::share::repository as share_repo;
use crate::share::{ShareConfig, SharePublic};

const PROTOCOL_VERSION: &str = "0.1";
const DEFAULT_TREE_DEPTH: u32 = 3;
const MAX_TREE_DEPTH: u32 = 20;

#[derive(Debug, Serialize)]
pub struct HelloResponse {
    node_id: String,
    protocol_version: &'static str,
    capabilities: Vec<&'static str>,
}

/// GET /api/v1/hello —— 对应基线 §5 HELLO 字段。
pub async fn hello(State(state): State<HttpState>) -> Json<HelloResponse> {
    Json(HelloResponse {
        node_id: state.identity.node_id.clone(),
        protocol_version: PROTOCOL_VERSION,
        capabilities: vec!["http", "share", "tree", "download", "upload"],
    })
}

/// GET /api/v1/shares —— 返回已启用共享的虚拟信息列表。
pub async fn list_shares(State(state): State<HttpState>) -> AppResult<Json<Vec<SharePublic>>> {
    let shares = share_repo::list_all(&state.db)?
        .into_iter()
        .filter(|share| share.is_enabled() && share.protocol == crate::share::ShareProtocol::Nodesend)
        .map(|share| share.to_public())
        .collect();
    Ok(Json(shares))
}

#[derive(Debug, Deserialize)]
pub struct TreeQuery {
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub depth: Option<u32>,
    #[serde(default)]
    pub access_token: Option<String>,
}

/// GET /api/v1/shares/{id}/tree —— ★目录树 API。
pub async fn tree(
    State(state): State<HttpState>,
    Path(id): Path<String>,
    Query(query): Query<TreeQuery>,
    headers: HeaderMap,
) -> AppResult<Json<tree::TreeNode>> {
    let share = authorize_request(&state, &id, query.access_token, &headers)?;
    let virtual_path = query.path.unwrap_or_default();
    let depth = query
        .depth
        .unwrap_or(DEFAULT_TREE_DEPTH)
        .min(MAX_TREE_DEPTH);

    let real = security::resolve_existing(share.path_ref(), &virtual_path)?;
    if !real.is_dir() {
        return Err(AppError::BadRequest("请求路径不是目录".into()));
    }
    let mut root = tree::build_tree(&real, &virtual_path, depth)?;
    root.name = share.name.clone(); // 根节点显示共享名称（虚拟根）
    Ok(Json(root))
}

/// GET /api/v1/shares/{id}/list —— 单层目录列表（复用 tree，depth=1）。
pub async fn list(
    State(state): State<HttpState>,
    Path(id): Path<String>,
    Query(query): Query<TreeQuery>,
    headers: HeaderMap,
) -> AppResult<Json<serde_json::Value>> {
    let share = authorize_request(&state, &id, query.access_token, &headers)?;
    let virtual_path = query.path.unwrap_or_default();

    let real = security::resolve_existing(share.path_ref(), &virtual_path)?;
    if !real.is_dir() {
        return Err(AppError::BadRequest("请求路径不是目录".into()));
    }
    let root = tree::build_tree(&real, &virtual_path, 1)?;
    Ok(Json(serde_json::json!({
        "path": root.path,
        "entries": root.children.unwrap_or_default(),
    })))
}

/// GET /api/v1/shares/{id}/info —— 单个文件/目录信息（depth=0）。
pub async fn info(
    State(state): State<HttpState>,
    Path(id): Path<String>,
    Query(query): Query<TreeQuery>,
    headers: HeaderMap,
) -> AppResult<Json<tree::TreeNode>> {
    let share = authorize_request(&state, &id, query.access_token, &headers)?;
    let virtual_path = query.path.unwrap_or_default();

    let real = security::resolve_existing(share.path_ref(), &virtual_path)?;
    let node = tree::build_tree(&real, &virtual_path, 0)?;
    Ok(Json(node))
}

/// GET /api/v1/shares/{id}/download —— 流式文件下载。
pub async fn download(
    State(state): State<HttpState>,
    Path(id): Path<String>,
    Query(query): Query<TreeQuery>,
    headers: HeaderMap,
) -> AppResult<Response> {
    let share = authorize_request(&state, &id, query.access_token, &headers)?;
    let virtual_path = query.path.unwrap_or_default();

    let real = security::resolve_existing(share.path_ref(), &virtual_path)?;
    if !real.is_file() {
        return Err(AppError::BadRequest("下载目标不是文件".into()));
    }
    let file_name = real
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("download")
        .to_string();
    let file = tokio::fs::File::open(&real).await?;
    let reader = ReaderStream::new(file);
    let body = Body::from_stream(reader);

    let disposition = format!("attachment; filename*=UTF-8''{}", url_encode(&file_name));
    Ok((
        [
            (header::CONTENT_TYPE, "application/octet-stream".to_string()),
            (header::CONTENT_DISPOSITION, disposition),
        ],
        body,
    )
        .into_response())
}

#[derive(Debug, Deserialize)]
pub struct UploadQuery {
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub access_token: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct UploadResult {
    name: String,
    path: String,
    size: u64,
}

/// POST /api/v1/shares/{id}/upload —— multipart/form-data 上传（字段名 file）。
pub async fn upload(
    State(state): State<HttpState>,
    Path(id): Path<String>,
    Query(query): Query<UploadQuery>,
    headers: HeaderMap,
    mut multipart: Multipart,
) -> AppResult<Json<UploadResult>> {
    let share = authorize_request(&state, &id, query.access_token, &headers)?;
    let target_dir = query.path.unwrap_or_default();

    while let Some(mut field) = multipart
        .next_field()
        .await
        .map_err(|err| AppError::BadRequest(format!("multipart 解析失败: {err}")))?
    {
        if field.name() != Some("file") {
            continue;
        }
        let original = field.file_name().unwrap_or("upload.bin").to_string();
        let virtual_target = join_dir(&target_dir, &original);
        let (real_target, file_name) =
            security::resolve_for_write(share.path_ref(), &virtual_target)?;

        let mut file = tokio::fs::File::create(&real_target).await?;
        let mut size = 0u64;
        while let Some(chunk) = field
            .chunk()
            .await
            .map_err(|err| AppError::BadRequest(format!("上传数据读取失败: {err}")))?
        {
            size += chunk.len() as u64;
            file.write_all(&chunk).await?;
        }
        file.flush().await?;
        return Ok(Json(UploadResult {
            name: file_name,
            path: virtual_target,
            size,
        }));
    }
    Err(AppError::BadRequest("未找到名为 file 的上传字段".into()))
}

/// 公共鉴权：加载共享、提取调用方、执行访问控制。
fn authorize_request(
    state: &HttpState,
    id: &str,
    query_token: Option<String>,
    headers: &HeaderMap,
) -> AppResult<ShareConfig> {
    let share = share_repo::get_by_id(&state.db, id)?;
    if share.protocol != crate::share::ShareProtocol::Nodesend {
        return Err(AppError::NotFound("请使用该共享配置的协议访问".into()));
    }
    let caller = extract_caller(headers, query_token);
    authorize(&share, &caller, &state.db)?;
    Ok(share)
}

/// 从请求头/查询串构造调用方信息。
fn extract_caller(headers: &HeaderMap, query_token: Option<String>) -> Caller {
    let node_id = headers
        .get("x-nodesend-node-id")
        .and_then(|value| value.to_str().ok())
        .map(|value| value.to_string());
    let token = query_token.or_else(|| {
        headers
            .get(header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.strip_prefix("Bearer "))
            .map(|value| value.to_string())
    });
    Caller::new(node_id, token)
}

fn join_dir(dir: &str, name: &str) -> String {
    let dir = dir.trim().trim_matches('/');
    if dir.is_empty() {
        name.to_string()
    } else {
        format!("{dir}/{name}")
    }
}

/// 极简百分号编码（RFC 5987 filename* 用）：保留未保留字符，其余 %XX。
fn url_encode(value: &str) -> String {
    let mut out = String::new();
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}
