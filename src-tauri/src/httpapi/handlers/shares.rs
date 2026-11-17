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
    body::{Body, Bytes},
    extract::{ConnectInfo, Extension, Multipart, Path, Query, Request, State},
    http::{header, HeaderMap, HeaderValue, Method, StatusCode},
    response::{Html, IntoResponse, Json, Response},
};
use image::{codecs::jpeg::JpegEncoder, DynamicImage};
use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    io::{self, Cursor, Write},
    path::Path as FsPath,
    sync::{Arc, Mutex, OnceLock},
};
use tokio::{
    io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt},
    sync::{mpsc, Semaphore},
};
use tokio_stream::wrappers::ReceiverStream;
use tokio_util::io::ReaderStream;
use zip::{write::SimpleFileOptions, CompressionMethod, ZipWriter};

const RAW_EXTENSIONS: [&str; 19] = [
    ".3fr", ".arw", ".cr2", ".cr3", ".dcr", ".dng", ".erf", ".kdc", ".mrw", ".nef", ".nrw", ".orf",
    ".pef", ".raf", ".raw", ".rw2", ".rwl", ".srw", ".x3f",
];

use super::super::HttpState;
use crate::access::{authorize, Caller};
use crate::error::{AppError, AppResult};
use crate::httpapi::{security, tree};
use crate::share::repository as share_repo;
use crate::share::{ShareConfig, SharePublic};
use serde_json::{json, Value};
use base64::Engine as _;

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
        capabilities: vec![
            "http",
            "share",
            "tree",
            "search",
            "events",
            "preview",
            "range_download",
            "folder_download",
            "upload",
            "resumable_upload",
        ],
    })
}

/// Serve the existing NetShare browser frontend for this share.
pub async fn browser(
    State(state): State<HttpState>,
    Path(id): Path<String>,
    Query(query): Query<TreeQuery>,
    headers: HeaderMap,
) -> AppResult<Response> {
    let token = query.access_token.clone();
    let share = authorize_request(&state, &id, token.clone(), &headers)?;
    if !share.path_ref().is_dir() {
        return Err(AppError::NotFound("Shared directory is unavailable".into()));
    }
    if let Some(path) = query.path.as_deref() {
        let is_directory = security::resolve_existing(share.path_ref(), path)
            .is_ok_and(|resolved| resolved.is_dir());
        if !is_directory {
            return Ok(not_found_response());
        }
    }
    let mut response_headers = HeaderMap::new();
    let id_cookie = HeaderValue::from_str(&format!(
        "nodesend_share_id={id}; Path=/; HttpOnly; SameSite=Strict"
    ))
    .map_err(|error| AppError::BadRequest(error.to_string()))?;
    response_headers.insert(header::SET_COOKIE, id_cookie);
    let token_cookie = match (share.access_mode, token) {
        (crate::share::AccessMode::Password, Some(token)) => format!(
            "nodesend_share_token={}; Path=/; HttpOnly; SameSite=Strict",
            encode_password_session(&token)
        ),
        _ => "nodesend_share_token=; Path=/; Max-Age=0; HttpOnly; SameSite=Strict".into(),
    };
    response_headers.append(
        header::SET_COOKIE,
        HeaderValue::from_str(&token_cookie)
            .map_err(|error| AppError::BadRequest(error.to_string()))?,
    );
    Ok((response_headers, browser_document()).into_response())
}

/// Bare listener root is intentionally not a share URL.
pub async fn browser_webdav_root(
    State(state): State<HttpState>,
    Path(share_name): Path<String>,
    request: Request,
) -> Response {
    serve_http_webdav(state, share_name, request).await
}

pub async fn browser_webdav_path(
    State(state): State<HttpState>,
    Path((share_name, _path)): Path<(String, String)>,
    request: Request,
) -> Response {
    serve_http_webdav(state, share_name, request).await
}

async fn serve_http_webdav(
    state: HttpState,
    share_name: String,
    mut request: Request,
) -> Response {
    let port = listener_port(request.headers()).unwrap_or(8193);
    let share = match share_repo::get_by_protocol_share_name(
        &state.db,
        &share_name,
        crate::share::ShareProtocol::Http,
        port,
    ) {
        Ok(Some(share))
            if share.protocol == crate::share::ShareProtocol::Http
                && state.allows_share(&share)
                && share.access_mode != crate::share::AccessMode::Token =>
        {
            share
        }
        _ => return not_found_response(),
    };

    let credentials = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|header| header.to_str().ok())
        .and_then(|header| header.strip_prefix("Basic "))
        .and_then(|encoded| base64::engine::general_purpose::STANDARD.decode(encoded).ok())
        .and_then(|decoded| String::from_utf8(decoded).ok())
        .and_then(|value| value.split_once(':').map(|(username, password)| (username.to_owned(), password.to_owned())));
    let token = credentials.and_then(|(username, password)| {
        (share.access_mode == crate::share::AccessMode::Password
            && username == share.access_username.as_deref().unwrap_or_default())
        .then_some(password)
    });
    if authorize(&share, &Caller::new(None, token), &state.db).is_err() {
        return (
            StatusCode::UNAUTHORIZED,
            [(header::WWW_AUTHENTICATE, "Basic realm=\"NodeSend WebDAV\"")],
            "WebDAV authentication failed",
        )
            .into_response();
    }

    let dav = state
        .dav_handlers
        .as_ref()
        .and_then(|handlers| handlers.read().ok()?.get(&share.id).cloned());
    let Some(dav) = dav else {
        return not_found_response();
    };

    let uri = request.uri().clone();
    let dav_path = uri
        .path()
        .strip_prefix('/')
        .and_then(|path| path.split_once('/').map(|(_, rest)| rest))
        .and_then(|path| path.strip_prefix("dav"))
        .filter(|path| path.is_empty() || path.starts_with('/'))
        .unwrap_or("/");
    let rewritten_path = if dav_path.is_empty() { "/" } else { dav_path };
    let path_and_query = match uri.query() {
        Some(query) => format!("{rewritten_path}?{query}"),
        None => rewritten_path.to_owned(),
    };
    if let Ok(rewritten_uri) = path_and_query.parse() {
        *request.uri_mut() = rewritten_uri;
    }
    dav.handle(request).await.map(Body::new)
}

pub async fn browser_root() -> Response {
    not_found_response()
}

/// Resolve the named share. Token access remains in the URL and is sent with
/// each browser API request; only the share ID is stored in a routing cookie.
pub async fn browser_named_root(
    State(state): State<HttpState>,
    Path(share_name): Path<String>,
    Query(query): Query<TreeQuery>,
    headers: HeaderMap,
) -> AppResult<Response> {
    let port = listener_port(&headers).unwrap_or(8193);
    let share = match named_http_share(&state, &share_name, port)? {
        Some(share) => share,
        None => return Ok(not_found_response()),
    };
    if !state.allows_share(&share) || !share.path_ref().is_dir() {
        return Ok(not_found_response());
    }
    let session_token = (share.access_mode == crate::share::AccessMode::Password
        && browser_cookie(&headers, "nodesend_share_id").as_deref()
        == Some(share.id.as_str()))
    .then(|| browser_session_token(&headers))
    .flatten();
    let token = match share.access_mode {
        crate::share::AccessMode::Token => query.access_token.clone(),
        crate::share::AccessMode::Password => session_token,
        crate::share::AccessMode::Open => None,
        crate::share::AccessMode::Trusted => return Ok(not_found_response()),
    };
    if share.access_mode == crate::share::AccessMode::Password && token.is_none() {
        return Ok(browser_login_document(
            &share.name,
            &browser_ui_language(&state)?,
        ).into_response());
    }
    if let Err(error) = authorize_request(&state, &share.id, token.clone(), &headers) {
        if share.access_mode == crate::share::AccessMode::Password
            && matches!(error, AppError::Forbidden(_))
        {
            return Ok(browser_login_document(
                &share.name,
                &browser_ui_language(&state)?,
            ).into_response());
        }
        if share.access_mode == crate::share::AccessMode::Token
            && matches!(error, AppError::NotFound(_))
        {
            return Ok(not_found_response());
        }
        return Err(error);
    }
    if let Some(path) = query.path.as_deref() {
        let is_directory = security::resolve_existing(share.path_ref(), path)
            .is_ok_and(|resolved| resolved.is_dir());
        if !is_directory {
            return Ok(not_found_response());
        }
    }
    let mut response_headers = HeaderMap::new();
    let session_cookie = HeaderValue::from_str(&format!(
        "nodesend_share_id={}; Path=/; HttpOnly; SameSite=Strict",
        share.id
    ))
    .map_err(|error| AppError::BadRequest(error.to_string()))?;
    response_headers.insert(header::SET_COOKIE, session_cookie);
    let token_cookie = match (share.access_mode, token) {
        (crate::share::AccessMode::Password, Some(token)) => format!(
            "nodesend_share_token={}; Path=/; HttpOnly; SameSite=Strict",
            encode_password_session(&token)
        ),
        _ => "nodesend_share_token=; Path=/; Max-Age=0; HttpOnly; SameSite=Strict".into(),
    };
    response_headers.append(
        header::SET_COOKIE,
        HeaderValue::from_str(&token_cookie)
            .map_err(|error| AppError::BadRequest(error.to_string()))?,
    );
    Ok((response_headers, browser_document()).into_response())
}

fn named_http_share(
    state: &HttpState,
    folder_name: &str,
    port: u16,
) -> AppResult<Option<ShareConfig>> {
    share_repo::get_by_protocol_share_name(
        &state.db,
        folder_name,
        crate::share::ShareProtocol::Http,
        port,
    )
}

fn listener_port(headers: &HeaderMap) -> Option<u16> {
    headers
        .get(header::HOST)?
        .to_str()
        .ok()?
        .parse::<axum::http::uri::Authority>()
        .ok()?
        .port_u16()
}

#[derive(Debug, Deserialize)]
pub struct BrowserLoginRequest {
    username: String,
    password: String,
}

pub async fn browser_named_login(
    State(state): State<HttpState>,
    Path(share_name): Path<String>,
    headers: HeaderMap,
    Json(credentials): Json<BrowserLoginRequest>,
) -> AppResult<Response> {
    let port = listener_port(&headers).unwrap_or(8193);
    let share = named_http_share(&state, &share_name, port)?
        .ok_or_else(|| AppError::NotFound("HTTP share does not exist".into()))?;
    if share.access_mode != crate::share::AccessMode::Password {
        return Err(AppError::BadRequest(
            "This share does not use password login".into(),
        ));
    }
    if credentials.username != share.access_username.as_deref().unwrap_or("nodesend")
        || authorize(
            &share,
            &Caller::new(None, Some(credentials.password.clone())),
            &state.db,
        )
        .is_err()
    {
        return Err(AppError::Forbidden(
            "The username or password is incorrect".into(),
        ));
    }
    if !state.allows_share(&share) {
        return Err(AppError::NotFound("HTTP share does not exist".into()));
    }
    let mut response_headers = HeaderMap::new();
    response_headers.insert(
        header::SET_COOKIE,
        HeaderValue::from_str(&format!(
            "nodesend_share_id={}; Path=/; HttpOnly; SameSite=Strict",
            share.id
        ))
        .map_err(|error| AppError::BadRequest(error.to_string()))?,
    );
    response_headers.append(
        header::SET_COOKIE,
        HeaderValue::from_str(&format!(
            "nodesend_share_token={}; Path=/; HttpOnly; SameSite=Strict",
            encode_password_session(&credentials.password)
        ))
        .map_err(|error| AppError::BadRequest(error.to_string()))?,
    );
    Ok((response_headers, StatusCode::NO_CONTENT).into_response())
}

fn browser_login_document(share_name: &str, language: &str) -> Html<String> {
    let name = share_name
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;");
    let (title, heading, username, password, login, invalid, failed) = match language {
        "zh-TW" => ("登入共享", "登入共享資料夾", "使用者名稱", "密碼", "登入", "使用者名稱或密碼錯誤", "登入失敗，請重試"),
        "en-US" => ("Sign in to share", "Sign in to shared folder", "Username", "Password", "Sign in", "Incorrect username or password", "Sign-in failed. Please try again."),
        "ja-JP" => ("共有にログイン", "共有フォルダーにログイン", "ユーザー名", "パスワード", "ログイン", "ユーザー名またはパスワードが正しくありません", "ログインに失敗しました。もう一度お試しください。"),
        _ => ("登录共享", "登录共享目录", "用户名", "密码", "登录", "用户名或密码错误", "登录失败，请重试"),
    };
    Html(format!(
        r#"<!doctype html><html lang="{language}"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><meta name="color-scheme" content="light dark"><title>{title} - NodeSend</title><style>
        *{{box-sizing:border-box}}body{{margin:0;min-height:100vh;display:grid;place-items:center;padding:24px;background:#f3f6fa;color:#202938;font:15px system-ui,sans-serif}}.card{{width:min(100%,390px);padding:30px;border:1px solid #dbe2ec;border-radius:18px;background:#fff;box-shadow:0 18px 55px #17243a12}}.brand{{font-weight:750;font-size:18px;margin-bottom:28px}}h1{{font-size:22px;margin:0 0 8px}}.name{{color:#718096;margin:0 0 25px;overflow-wrap:anywhere}}label{{display:block;font-size:13px;font-weight:600;margin:16px 0 7px}}input{{width:100%;height:44px;padding:0 12px;border:1px solid #cbd5e1;border-radius:9px;background:#fff;color:inherit;font:inherit}}button{{width:100%;height:44px;margin-top:22px;border:0;border-radius:9px;background:#087fd1;color:#fff;font:inherit;font-weight:700;cursor:pointer}}.error{{min-height:20px;margin:12px 0 0;color:#d04444;font-size:13px}}@media(prefers-color-scheme:dark){{body{{background:#151c29;color:#e7edf7}}.card{{background:#202939;border-color:#344052;box-shadow:none}}.name{{color:#a7b4c8}}input{{background:#151c29;border-color:#40506a;color:inherit}}}}
        </style></head><body><main class="card"><div class="brand">NodeSend</div><h1>{heading}</h1><p class="name">{name}</p><form id="login"><label for="username">{username}</label><input id="username" name="username" autocomplete="username" required><label for="password">{password}</label><input id="password" name="password" type="password" autocomplete="current-password" required autofocus><button type="submit">{login}</button><p id="error" class="error" role="alert"></p></form></main><script>
        const form=document.getElementById('login'),error=document.getElementById('error');form.addEventListener('submit',async event=>{{event.preventDefault();error.textContent='';const data=new FormData(form);try{{const response=await fetch(location.pathname.replace(/\/+$/, '')+'/login',{{method:'POST',headers:{{'content-type':'application/json'}},body:JSON.stringify({{username:data.get('username'),password:data.get('password')}})}});if(!response.ok)throw new Error('{invalid}');location.replace(location.pathname)}}catch(reason){{error.textContent=reason.message||'{failed}'}}}});
        </script></body></html>"#
    ))
}

pub async fn not_found_page() -> Response {
    not_found_response()
}

fn not_found_response() -> Response {
    let html = r#"<!doctype html><html lang="en"><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><title>404 — NodeSend</title><style>
      *{box-sizing:border-box}html{color-scheme:light dark}body{margin:0;min-height:100vh;display:grid;place-items:center;overflow:hidden;background:#f4f7fb;color:#202938;font:16px system-ui,sans-serif}
      main{text-align:center;position:relative;padding:3rem}.code{font-size:clamp(7rem,25vw,15rem);font-weight:800;line-height:.9;letter-spacing:-.09em;color:#168de0;text-shadow:0 0 35px #168de055;animation:float 3s ease-in-out infinite}
      .dot{display:inline-block;animation:pulse 1.3s ease-in-out infinite}.dot:nth-child(2){animation-delay:.2s}.dot:nth-child(3){animation-delay:.4s}p{color:#a7b4c8;letter-spacing:.08em}
      @media(prefers-color-scheme:dark){body{background:#151c29;color:#e7edf7}}
      @keyframes float{50%{transform:translateY(-12px)}}@keyframes pulse{50%{opacity:.2;transform:translateY(-5px)}}
    </style><main><div class="code">404</div><p>PAGE NOT FOUND <span class="dot">·</span><span class="dot">·</span><span class="dot">·</span></p></main></html>"#;
    (
        StatusCode::NOT_FOUND,
        [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
        html,
    )
        .into_response()
}

pub async fn browser_index() -> Html<String> {
    browser_document()
}

fn browser_document() -> Html<String> {
    Html(
        include_str!("../../../resources/share-browser-src/index.html")
            .replace("src=\"/src/main.js\"", "src=\"/share-ui/webui.js\""),
    )
}

pub async fn browser_favicon() -> Response {
    static_response(
        "image/svg+xml",
        include_bytes!("../../../resources/share-browser/favicon.svg"),
    )
}

pub async fn browser_script() -> Response {
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/javascript; charset=utf-8"),
    );
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    (
        headers,
        Body::from(&include_bytes!("../../../resources/share-browser/webui.js")[..]),
    )
        .into_response()
}

pub async fn browser_styles() -> Response {
    let mut response = static_response(
        "text/css; charset=utf-8",
        include_bytes!("../../../resources/share-browser/webui.css"),
    );
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    response
}

pub async fn browser_runtime() -> Response {
    static_response(
        "text/javascript; charset=utf-8",
        include_bytes!(
            "../../../resources/share-browser/assets/runtime-dom.esm-bundler-DYBPHc56.js"
        ),
    )
}

pub async fn browser_language() -> Response {
    source_script_response(include_bytes!(
        "../../../resources/share-browser-src/src/language.js"
    ))
}

pub async fn browser_i18n() -> Response {
    source_script_response(include_bytes!(
        "../../../resources/share-browser-src/src/i18n.js"
    ))
}

fn source_script_response(bytes: &'static [u8]) -> Response {
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/javascript; charset=utf-8"),
    );
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    (headers, Body::from(bytes)).into_response()
}

fn static_response(content_type: &'static str, bytes: &'static [u8]) -> Response {
    let mut headers = HeaderMap::new();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("public, max-age=31536000, immutable"),
    );
    (headers, Body::from(bytes)).into_response()
}

#[derive(Debug, Deserialize)]
pub struct BrowserPathQuery {
    path: Option<String>,
    thumbnail: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct BrowserSearchQuery {
    q: String,
    path: Option<String>,
}

fn browser_cookie(headers: &HeaderMap, wanted: &str) -> Option<String> {
    headers
        .get(header::COOKIE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| {
            value.split(';').find_map(|cookie| {
                let (name, value) = cookie.trim().split_once('=')?;
                (name == wanted).then(|| value.to_owned())
            })
        })
}

fn encode_password_session(password: &str) -> String {
    use base64::Engine;

    format!(
        "pw1_{}",
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(password.as_bytes())
    )
}

fn browser_session_token(headers: &HeaderMap) -> Option<String> {
    use base64::Engine;

    if let Some(token) = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
    {
        return Some(token.to_owned());
    }
    let value = browser_cookie(headers, "nodesend_share_token")?;
    let Some(encoded) = value.strip_prefix("pw1_") else {
        return Some(value);
    };
    base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(encoded)
        .ok()
        .and_then(|bytes| String::from_utf8(bytes).ok())
}

fn browser_token(headers: &HeaderMap) -> Option<String> {
    browser_session_token(headers)
}

fn browser_share(state: &HttpState, headers: &HeaderMap) -> AppResult<(String, ShareConfig)> {
    let id = state
        .share_scope
        .clone()
        .or_else(|| browser_cookie(headers, "nodesend_share_id"))
        .ok_or_else(|| AppError::NotFound("Open an HTTP share link first".into()))?;
    let share = authorize_request(
        state,
        &id,
        browser_session_token(headers),
        headers,
    )?;
    Ok((id, share))
}

pub async fn browser_status(
    State(state): State<HttpState>,
    headers: HeaderMap,
    client: Option<Extension<ConnectInfo<std::net::SocketAddr>>>,
) -> AppResult<Json<Value>> {
    let (id, share) = browser_share(&state, &headers)?;
    let client_ip = client
        .as_ref()
        .map(|Extension(ConnectInfo(address))| address.ip());
    Ok(Json(browser_status_payload(
        &state,
        &id,
        &share,
        same_device_client(client_ip),
    )?))
}

pub(crate) fn browser_status_payload(
    state: &HttpState,
    id: &str,
    share: &ShareConfig,
    same_device: bool,
) -> AppResult<Value> {
    let language = browser_ui_language(state)?;
    let tcp_port = state
        .db
        .with_conn(|conn| {
            conn.query_row(
                "SELECT value FROM settings WHERE key='network_tcp_port'",
                [],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(Into::into)
        })?
        .and_then(|value| value.parse::<u16>().ok())
        .unwrap_or(0);
    let quic_port = state
        .db
        .with_conn(|conn| {
            conn.query_row(
                "SELECT value FROM settings WHERE key='network_quic_port'",
                [],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(Into::into)
        })?
        .and_then(|value| value.parse::<u16>().ok())
        .unwrap_or(0);
    Ok(json!({
        "shared_folder": share.name,
        "share_id": id,
        "node_id": state.identity.node_id,
        "tcp_port": tcp_port,
        "quic_port": quic_port,
        "same_device": same_device,
        "language": language,
    }))
}

pub(crate) fn browser_ui_language(state: &HttpState) -> AppResult<String> {
    Ok(state
        .db
        .with_conn(|conn| {
            conn.query_row(
                "SELECT value FROM settings WHERE key='ui_language'",
                [],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(Into::into)
        })?
        .filter(|value| matches!(value.as_str(), "zh-CN" | "zh-TW" | "en-US" | "ja-JP"))
        .unwrap_or_else(crate::locale::preferred_language))
}

pub(crate) fn same_device_client(client_ip: Option<std::net::IpAddr>) -> bool {
    client_ip.is_some_and(|client_ip| {
        client_ip.is_loopback()
            || if_addrs::get_if_addrs().is_ok_and(|interfaces| {
                interfaces
                    .into_iter()
                    .any(|interface| interface.ip() == client_ip)
            })
    })
}

pub async fn browser_list(
    State(state): State<HttpState>,
    Query(query): Query<BrowserPathQuery>,
    headers: HeaderMap,
) -> AppResult<Json<Value>> {
    let (id, share) = browser_share(&state, &headers)?;
    state.events.watch(&share, state.directory_tree_for(&id));
    let path = query.path.unwrap_or_default();
    let real = security::resolve_existing(share.path_ref(), &path)?;
    if !real.is_dir() {
        return Err(AppError::NotFound("Path does not exist".into()));
    }
    let directory = tree::build_tree(&real, &path, 1)?;
    if let Some(snapshot) = state.directory_tree_for(&id) {
        let children = directory.children.as_deref().unwrap_or_default();
        let mut snapshot = snapshot
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        tree::merge_directory_listing(&mut snapshot, &path, children);
    }
    let mut root =
        serde_json::to_value(directory).map_err(|error| AppError::Other(error.to_string()))?;
    root["name"] = Value::String(if path.is_empty() {
        share.name
    } else {
        root["name"].as_str().unwrap_or_default().to_owned()
    });
    normalize_browser_nodes(&mut root);
    Ok(Json(root))
}

pub async fn browser_search(
    State(state): State<HttpState>,
    Query(query): Query<BrowserSearchQuery>,
    headers: HeaderMap,
) -> AppResult<Json<Value>> {
    let (id, share) = browser_share(&state, &headers)?;
    let needle = query.q.trim().to_lowercase();
    if needle.is_empty() {
        return Ok(Json(json!([])));
    }
    let root = share.path_ref().canonicalize()?;
    let directory = security::resolve_existing(&root, query.path.as_deref().unwrap_or_default())?;
    if !directory.is_dir() {
        return Err(AppError::BadRequest(
            "Search path is not a directory".into(),
        ));
    }
    let directory_tree = state
        .directory_tree_for(&id)
        .ok_or_else(|| AppError::Other("The shared directory tree is not initialized".into()))?;
    let rows = tokio::task::spawn_blocking(move || {
        let directory_tree = directory_tree
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut results = directory_tree
            .iter()
            .filter(|directory| directory.name.to_lowercase().contains(&needle))
            .take(500)
            .map(|directory| SearchResult {
                name: directory.name.clone(),
                path: directory.path.clone(),
                kind: "dir",
                size: 0,
                modified: String::new(),
            })
            .collect::<Vec<_>>();
        drop(directory_tree);
        let files = search_current_directory_files(&root, &directory, &needle, 200)?;
        results.extend(files);
        Ok::<Vec<SearchResult>, AppError>(results)
    })
    .await
    .map_err(|error| AppError::Other(error.to_string()))??;
    let mut value =
        serde_json::to_value(rows).map_err(|error| AppError::Other(error.to_string()))?;
    normalize_browser_nodes(&mut value);
    Ok(Json(value))
}

fn search_current_directory_files(
    share_root: &FsPath,
    directory: &FsPath,
    needle: &str,
    limit: usize,
) -> AppResult<Vec<SearchResult>> {
    let mut results = Vec::new();
    for entry in std::fs::read_dir(directory)?.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if security::is_upload_staging_name(&name) || !name.to_lowercase().contains(needle) {
            continue;
        }
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if !file_type.is_file() || file_type.is_symlink() {
            continue;
        }
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        let Ok(relative) = entry
            .path()
            .strip_prefix(share_root)
            .map(std::path::Path::to_path_buf)
        else {
            continue;
        };
        results.push(SearchResult {
            name,
            path: relative.to_string_lossy().replace('\\', "/"),
            kind: "file",
            size: metadata.len(),
            modified: metadata
                .modified()
                .ok()
                .map(|time| chrono::DateTime::<chrono::Utc>::from(time).to_rfc3339())
                .unwrap_or_default(),
        });
    }
    results.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    results.truncate(limit);
    Ok(results)
}

fn normalize_browser_nodes(value: &mut Value) {
    match value {
        Value::Object(object) => {
            if let Some(kind) = object.remove("node_type") {
                object.insert("type".into(), kind);
            }
            if let Some(modified) = object.get_mut("modified") {
                if let Some(text) = modified.as_str() {
                    let seconds = chrono::DateTime::parse_from_rfc3339(text)
                        .map(|time| time.timestamp())
                        .unwrap_or_default();
                    *modified = json!(seconds);
                }
            }
            for child in object.values_mut() {
                normalize_browser_nodes(child);
            }
        }
        Value::Array(items) => items.iter_mut().for_each(normalize_browser_nodes),
        _ => {}
    }
}

pub async fn browser_download(
    State(state): State<HttpState>,
    Query(query): Query<BrowserPathQuery>,
    headers: HeaderMap,
    method: Method,
) -> AppResult<Response> {
    let (id, _) = browser_share(&state, &headers)?;
    download(
        State(state),
        Path(id),
        Query(TreeQuery {
            path: query.path,
            depth: None,
            access_token: browser_token(&headers),
            thumbnail: None,
        }),
        headers,
        method,
    )
    .await
}

pub async fn browser_download_folder(
    State(state): State<HttpState>,
    Query(query): Query<BrowserPathQuery>,
    headers: HeaderMap,
) -> AppResult<Response> {
    let (id, _) = browser_share(&state, &headers)?;
    download_folder(
        State(state),
        Path(id),
        Query(TreeQuery {
            path: query.path,
            depth: None,
            access_token: browser_token(&headers),
            thumbnail: None,
        }),
        headers,
    )
    .await
}

pub async fn browser_preview(
    State(state): State<HttpState>,
    Query(query): Query<BrowserPathQuery>,
    headers: HeaderMap,
) -> AppResult<Response> {
    let (id, _) = browser_share(&state, &headers)?;
    preview(
        State(state),
        Path(id),
        Query(TreeQuery {
            path: query.path,
            depth: None,
            access_token: browser_token(&headers),
            thumbnail: query.thumbnail,
        }),
        headers,
    )
    .await
}

pub async fn browser_events(
    State(state): State<HttpState>,
    headers: HeaderMap,
    upgrade: axum::extract::ws::WebSocketUpgrade,
    client: Option<Extension<ConnectInfo<std::net::SocketAddr>>>,
) -> AppResult<Response> {
    let (id, _) = browser_share(&state, &headers)?;
    super::super::events::events(
        State(state),
        Path(id),
        Query(super::super::events::EventQuery {
            access_token: browser_token(&headers),
        }),
        headers,
        upgrade,
        client,
    )
    .await
}

pub async fn browser_upload_init(
    State(state): State<HttpState>,
    headers: HeaderMap,
    Json(request): Json<super::super::uploads::InitRequest>,
) -> AppResult<Json<Value>> {
    let (id, share) = browser_share(&state, &headers)?;
    let status = state.uploads.init(&id, share.path_ref(), request).await?;
    state.events.watch(&share, state.directory_tree_for(&id));
    let mut value =
        serde_json::to_value(status).map_err(|error| AppError::Other(error.to_string()))?;
    if let Some(object) = value.as_object_mut() {
        if let Some(id) = object.remove("id") {
            object.insert("upload_id".into(), id);
        }
    }
    Ok(Json(value))
}

pub async fn browser_upload_chunk(
    State(state): State<HttpState>,
    Path(upload_id): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> AppResult<Json<Value>> {
    let (id, share) = browser_share(&state, &headers)?;
    let range = headers
        .get(header::CONTENT_RANGE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    let status = state
        .uploads
        .append(&id, share.path_ref(), &upload_id, range, &body)
        .await?;
    state.events.watch(&share, state.directory_tree_for(&id));
    let mut value =
        serde_json::to_value(status).map_err(|error| AppError::Other(error.to_string()))?;
    if let Some(object) = value.as_object_mut() {
        if let Some(id) = object.remove("id") {
            object.insert("upload_id".into(), id);
        }
    }
    Ok(Json(value))
}

/// GET /api/v1/shares —— 返回已启用共享的虚拟信息列表。
pub async fn list_shares(State(state): State<HttpState>) -> AppResult<Json<Vec<SharePublic>>> {
    let shares = share_repo::list_all(&state.db)?
        .into_iter()
        .filter(|share| share.is_enabled() && state.allows_share(&share))
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
    #[serde(default, rename = "token")]
    pub access_token: Option<String>,
    #[serde(default)]
    pub thumbnail: Option<bool>,
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
    state.events.watch(&share, state.directory_tree_for(&id));

    let real = security::resolve_existing(share.path_ref(), &virtual_path)?;
    if !real.is_dir() {
        return Err(AppError::NotFound("Path does not exist".into()));
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
    state.events.watch(&share, state.directory_tree_for(&id));

    let real = security::resolve_existing(share.path_ref(), &virtual_path)?;
    if !real.is_dir() {
        return Err(AppError::NotFound("Path does not exist".into()));
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
    state.events.watch(&share, state.directory_tree_for(&id));

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
    method: Method,
) -> AppResult<Response> {
    let share = authorize_request(&state, &id, query.access_token, &headers)?;
    let virtual_path = query.path.unwrap_or_default();

    let real = security::resolve_existing(share.path_ref(), &virtual_path)?;
    if !real.is_file() {
        return Err(AppError::BadRequest("Download target is not a file".into()));
    }
    let file_name = real
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("download")
        .to_string();
    let size = tokio::fs::metadata(&real).await?.len();
    let mut response_headers = HeaderMap::new();
    response_headers.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    response_headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/octet-stream"),
    );
    response_headers.insert(
        header::CONTENT_DISPOSITION,
        HeaderValue::from_str(&format!(
            "attachment; filename*=UTF-8''{}",
            url_encode(&file_name)
        ))
        .map_err(|error| AppError::BadRequest(error.to_string()))?,
    );
    let range = headers
        .get(header::RANGE)
        .and_then(|value| value.to_str().ok());
    let (status, start, end) = if let Some(range) = range {
        let Some((start, end)) = parse_byte_range(range, size) else {
            response_headers.insert(
                header::CONTENT_RANGE,
                HeaderValue::from_str(&format!("bytes */{size}")).unwrap(),
            );
            return Ok((
                StatusCode::RANGE_NOT_SATISFIABLE,
                response_headers,
                Body::empty(),
            )
                .into_response());
        };
        (StatusCode::PARTIAL_CONTENT, start, end)
    } else {
        (StatusCode::OK, 0, size.saturating_sub(1))
    };
    let length = if size == 0 { 0 } else { end - start + 1 };
    response_headers.insert(
        header::CONTENT_LENGTH,
        HeaderValue::from_str(&length.to_string()).unwrap(),
    );
    if status == StatusCode::PARTIAL_CONTENT {
        response_headers.insert(
            header::CONTENT_RANGE,
            HeaderValue::from_str(&format!("bytes {start}-{end}/{size}")).unwrap(),
        );
    }
    if method == Method::HEAD {
        return Ok((status, response_headers, Body::empty()).into_response());
    }
    let mut file = tokio::fs::File::open(&real).await?;
    if start > 0 {
        file.seek(std::io::SeekFrom::Start(start)).await?;
    }
    let body = if length == 0 {
        Body::empty()
    } else {
        Body::from_stream(ReaderStream::new(file.take(length)))
    };
    Ok((status, response_headers, body).into_response())
}

fn parse_byte_range(value: &str, size: u64) -> Option<(u64, u64)> {
    if size == 0 || value.contains(',') {
        return None;
    }
    let (start, end) = value.strip_prefix("bytes=")?.split_once('-')?;
    let (start, end) = if start.is_empty() {
        let suffix = end.parse::<u64>().ok()?;
        if suffix == 0 {
            return None;
        }
        (size.saturating_sub(suffix), size - 1)
    } else {
        let start = start.parse::<u64>().ok()?;
        let end = if end.is_empty() {
            size - 1
        } else {
            end.parse::<u64>().ok()?.min(size - 1)
        };
        (start, end)
    };
    (start < size && start <= end).then_some((start, end))
}

#[derive(Debug, Deserialize)]
pub struct SearchQuery {
    pub q: String,
    /// Virtual path of the directory currently being viewed. File search stays
    /// within this directory; directory names come from the cached tree.
    pub path: Option<String>,
    #[serde(rename = "token")]
    pub access_token: Option<String>,
    pub r#type: Option<String>,
    pub limit: Option<usize>,
}

#[derive(Serialize)]
pub struct SearchResult {
    name: String,
    path: String,
    #[serde(rename = "type")]
    kind: &'static str,
    size: u64,
    modified: String,
}

/// Search directory names from the cached directory tree and file names only
/// in the directory currently being viewed.
pub async fn search(
    State(state): State<HttpState>,
    Path(id): Path<String>,
    Query(query): Query<SearchQuery>,
    headers: HeaderMap,
) -> AppResult<Json<Vec<SearchResult>>> {
    let share = authorize_request(&state, &id, query.access_token, &headers)?;
    let needle = query.q.trim().to_lowercase();
    if needle.is_empty() {
        return Ok(Json(Vec::new()));
    }
    let limit = query.limit.unwrap_or(50).clamp(1, 200);
    if !matches!(query.r#type.as_deref(), None | Some("file") | Some("dir")) {
        return Err(AppError::BadRequest(
            "Search type must be file or dir".into(),
        ));
    }
    let include_directories = query.r#type.as_deref() != Some("file");
    let include_files = query.r#type.as_deref() != Some("dir");
    let root = share.path_ref().canonicalize()?;
    let directory = security::resolve_existing(
        &root,
        query.path.as_deref().unwrap_or_default(),
    )?;
    if !directory.is_dir() {
        return Err(AppError::BadRequest("Search path is not a directory".into()));
    }
    let directory_tree = state
        .directory_tree_for(&share.id)
        .ok_or_else(|| AppError::Other("The shared directory tree is not initialized".into()))?;
    let rows = tokio::task::spawn_blocking(move || {
        let mut results = Vec::new();
        if include_directories {
            let tree = directory_tree
                .read()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            results.extend(
                tree.iter()
                    .filter(|entry| entry.name.to_lowercase().contains(&needle))
                    .map(|entry| SearchResult {
                        name: entry.name.clone(),
                        path: entry.path.clone(),
                        kind: "dir",
                        size: 0,
                        modified: String::new(),
                    }),
            );
        }
        if include_files {
            results.extend(search_current_directory_files(&root, &directory, &needle, limit)?);
        }
        results.sort_by(|a, b| a.path.to_lowercase().cmp(&b.path.to_lowercase()));
        results.truncate(limit);
        Ok::<_, AppError>(results)
    })
    .await
    .map_err(|error| AppError::Other(error.to_string()))??;
    Ok(Json(rows))
}

/// Stream a ZIP archive without staging the whole folder or archive in memory.
pub async fn download_folder(
    State(state): State<HttpState>,
    Path(id): Path<String>,
    Query(query): Query<TreeQuery>,
    headers: HeaderMap,
) -> AppResult<Response> {
    let share = authorize_request(&state, &id, query.access_token, &headers)?;
    let virtual_path = query.path.unwrap_or_default();
    let root = security::resolve_existing(share.path_ref(), &virtual_path)?;
    if !root.is_dir() {
        return Err(AppError::BadRequest(
            "Download target is not a directory".into(),
        ));
    }
    let name = root
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("share")
        .to_string();
    let (sender, receiver) = mpsc::channel::<Result<Bytes, io::Error>>(16);
    tokio::task::spawn_blocking(move || {
        if let Err(error) = write_archive(&root, &sender) {
            let _ = sender.blocking_send(Err(error));
        }
    });
    let encoded = url_encode(&format!("{name}.zip"));
    let mut response_headers = HeaderMap::new();
    response_headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/zip"),
    );
    response_headers.insert(
        header::CONTENT_DISPOSITION,
        HeaderValue::from_str(&format!("attachment; filename*=UTF-8''{encoded}"))
            .map_err(|error| AppError::BadRequest(error.to_string()))?,
    );
    Ok((
        StatusCode::OK,
        response_headers,
        Body::from_stream(ReceiverStream::new(receiver)),
    )
        .into_response())
}

struct ArchiveChannel {
    sender: mpsc::Sender<Result<Bytes, io::Error>>,
}
impl Write for ArchiveChannel {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.is_empty() {
            return Ok(0);
        }
        self.sender
            .blocking_send(Ok(Bytes::copy_from_slice(bytes)))
            .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "client disconnected"))?;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn write_archive(root: &FsPath, sender: &mpsc::Sender<Result<Bytes, io::Error>>) -> io::Result<()> {
    let name = root
        .file_name()
        .map(|value| value.to_string_lossy().into_owned())
        .unwrap_or_else(|| "share".into());
    let mut zip = ZipWriter::new_stream(ArchiveChannel {
        sender: sender.clone(),
    });
    let options = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
    zip.add_directory(format!("{name}/"), options)?;
    archive_directory(&mut zip, root, &name)?;
    zip.finish()?;
    Ok(())
}

fn archive_directory<W: Write + std::io::Seek>(
    zip: &mut ZipWriter<W>,
    directory: &FsPath,
    prefix: &str,
) -> io::Result<()> {
    let mut entries = std::fs::read_dir(directory)?
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            if security::is_upload_staging_name(&name) {
                return None;
            }
            let metadata = std::fs::symlink_metadata(entry.path()).ok()?;
            if metadata.file_type().is_symlink() {
                return None;
            }
            Some((name, entry.path(), metadata))
        })
        .collect::<Vec<_>>();
    entries.sort_by(|a, b| a.0.to_lowercase().cmp(&b.0.to_lowercase()));
    for (name, path, metadata) in &entries {
        let rel = format!("{prefix}/{name}");
        let options = SimpleFileOptions::default()
            .compression_method(CompressionMethod::Stored)
            .large_file(metadata.len() > u32::MAX as u64);
        if metadata.is_dir() {
            zip.add_directory(format!("{rel}/"), options)?;
        } else if metadata.is_file() {
            zip.start_file(rel, options)?;
            let mut file = std::fs::File::open(path)?;
            io::copy(&mut file, zip)?;
        }
    }
    for (name, path, metadata) in entries {
        if metadata.is_dir() {
            archive_directory(zip, &path, &format!("{prefix}/{name}"))?;
        }
    }
    Ok(())
}

pub async fn preview(
    State(state): State<HttpState>,
    Path(id): Path<String>,
    Query(query): Query<TreeQuery>,
    headers: HeaderMap,
) -> AppResult<Response> {
    let share = authorize_request(&state, &id, query.access_token, &headers)?;
    let path =
        security::resolve_existing(share.path_ref(), query.path.as_deref().unwrap_or_default())?;
    if !path.is_file() {
        return Err(AppError::BadRequest("Preview target is not a file".into()));
    }
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .map(|value| format!(".{}", value.to_lowercase()))
        .unwrap_or_default();
    if RAW_EXTENSIONS.contains(&extension.as_str()) {
        let preview_path = path.clone();
        let thumbnail = query.thumbnail.unwrap_or(false);
        let jpeg = if thumbnail {
            render_thumbnail_limited(preview_path, true).await?
        } else {
            tokio::task::spawn_blocking(move || render_raw_preview(&preview_path, None))
                .await
                .map_err(|error| AppError::Other(error.to_string()))??
        };
        let mut response_headers = HeaderMap::new();
        response_headers.insert(header::CONTENT_TYPE, HeaderValue::from_static("image/jpeg"));
        response_headers.insert(
            header::CACHE_CONTROL,
            HeaderValue::from_static("private, max-age=300"),
        );
        return Ok((response_headers, Body::from(jpeg)).into_response());
    }
    if query.thumbnail.unwrap_or(false) {
        let jpeg = render_thumbnail_limited(path.clone(), false).await?;
        let mut response_headers = HeaderMap::new();
        response_headers.insert(header::CONTENT_TYPE, HeaderValue::from_static("image/jpeg"));
        response_headers.insert(
            header::CACHE_CONTROL,
            HeaderValue::from_static("private, max-age=300"),
        );
        return Ok((response_headers, Body::from(jpeg)).into_response());
    }
    let mime = mime_guess::from_path(&path)
        .first()
        .map(|value| value.to_string())
        .unwrap_or_default();
    if !mime.starts_with("image/") {
        return Err(AppError::BadRequest(
            "Only image files can be previewed".into(),
        ));
    }
    let metadata = tokio::fs::metadata(&path).await?;
    let size = metadata.len();
    let modified_ns = metadata
        .modified()
        .ok()
        .and_then(|modified| modified.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    let etag = format!("\"{size:x}-{modified_ns:x}\"");
    let mut response_headers = HeaderMap::new();
    response_headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_str(&mime).map_err(|error| AppError::BadRequest(error.to_string()))?,
    );
    response_headers.insert(
        header::ETAG,
        HeaderValue::from_str(&etag).map_err(|error| AppError::BadRequest(error.to_string()))?,
    );
    response_headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, max-age=300"),
    );
    if headers
        .get(header::IF_NONE_MATCH)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.split(',').any(|candidate| candidate.trim() == etag))
    {
        return Ok((StatusCode::NOT_MODIFIED, response_headers).into_response());
    }
    response_headers.insert(
        header::CONTENT_LENGTH,
        HeaderValue::from_str(&size.to_string())
            .map_err(|error| AppError::BadRequest(error.to_string()))?,
    );
    let file = tokio::fs::File::open(path).await?;
    Ok((
        response_headers,
        Body::from_stream(ReaderStream::with_capacity(file, 128 * 1024)),
    )
        .into_response())
}

#[derive(Clone, Eq, Hash, PartialEq)]
struct ThumbnailKey {
    path: String,
    size: u64,
    modified_ns: u128,
}

#[derive(Default)]
struct ThumbnailCache {
    entries: HashMap<ThumbnailKey, Arc<Vec<u8>>>,
    bytes: usize,
}

static THUMBNAILS: OnceLock<Mutex<ThumbnailCache>> = OnceLock::new();
static THUMBNAIL_GENERATION: OnceLock<Semaphore> = OnceLock::new();
const THUMBNAIL_MAX_BYTES: usize = 32 * 1024 * 1024;
const MAX_CONCURRENT_THUMBNAIL_GENERATIONS: usize = 1;

async fn render_thumbnail_limited(path: std::path::PathBuf, raw: bool) -> AppResult<Vec<u8>> {
    let key = thumbnail_key(&path)?;
    if let Some(jpeg) = cached_thumbnail(&key) {
        return Ok(jpeg);
    }

    let slots =
        THUMBNAIL_GENERATION.get_or_init(|| Semaphore::new(MAX_CONCURRENT_THUMBNAIL_GENERATIONS));
    let _permit = slots
        .acquire()
        .await
        .map_err(|error| AppError::Other(error.to_string()))?;

    let jpeg = tokio::task::spawn_blocking(move || {
        if raw {
            render_cached_raw_thumbnail(&path)
        } else {
            render_cached_image_thumbnail(&path)
        }
    })
    .await
    .map_err(|error| AppError::Other(error.to_string()))??;
    Ok(jpeg)
}

fn thumbnail_key(path: &FsPath) -> AppResult<ThumbnailKey> {
    let metadata = std::fs::metadata(path)?;
    let modified_ns = metadata
        .modified()
        .ok()
        .and_then(|modified| modified.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    Ok(ThumbnailKey {
        path: path.to_string_lossy().into_owned(),
        size: metadata.len(),
        modified_ns,
    })
}

fn cached_thumbnail(key: &ThumbnailKey) -> Option<Vec<u8>> {
    THUMBNAILS
        .get_or_init(|| Mutex::new(ThumbnailCache::default()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .entries
        .get(key)
        .cloned()
        .map(|image| (*image).clone())
}

fn store_thumbnail(key: ThumbnailKey, jpeg: &[u8]) {
    if jpeg.len() > THUMBNAIL_MAX_BYTES {
        return;
    }
    let mut cache = THUMBNAILS
        .get_or_init(|| Mutex::new(ThumbnailCache::default()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    while cache.bytes + jpeg.len() > THUMBNAIL_MAX_BYTES {
        let Some(oldest) = cache.entries.keys().next().cloned() else {
            break;
        };
        if let Some(removed) = cache.entries.remove(&oldest) {
            cache.bytes = cache.bytes.saturating_sub(removed.len());
        }
    }
    cache.bytes += jpeg.len();
    cache.entries.insert(key, Arc::new(jpeg.to_vec()));
}

fn render_cached_raw_thumbnail(path: &FsPath) -> AppResult<Vec<u8>> {
    let key = thumbnail_key(path)?;
    if let Some(jpeg) = cached_thumbnail(&key) {
        return Ok(jpeg);
    }

    let jpeg = render_raw_preview(path, Some(480))?;
    store_thumbnail(key, &jpeg);
    Ok(jpeg)
}

fn render_cached_image_thumbnail(path: &FsPath) -> AppResult<Vec<u8>> {
    let key = thumbnail_key(path)?;
    if let Some(jpeg) = cached_thumbnail(&key) {
        return Ok(jpeg);
    }
    let image = image::open(path)
        .map_err(|error| AppError::BadRequest(format!("Image thumbnail decode failed: {error}")))?;
    let thumbnail = image.thumbnail(480, 480);
    let rgb = DynamicImage::ImageRgb8(thumbnail.to_rgb8());
    let mut output = Cursor::new(Vec::new());
    JpegEncoder::new_with_quality(&mut output, 78)
        .encode_image(&rgb)
        .map_err(|error| AppError::BadRequest(format!("Thumbnail encoding failed: {error}")))?;
    let jpeg = output.into_inner();
    store_thumbnail(key, &jpeg);
    Ok(jpeg)
}

fn render_raw_preview(path: &FsPath, max_dimension: Option<u32>) -> AppResult<Vec<u8>> {
    let image = match rawler::analyze::extract_preview_pixels(
        path,
        &rawler::decoders::RawDecodeParams::default(),
    ) {
        Ok(image) => image,
        Err(preview_error) => {
            let raw = rawler::decode_file(path).map_err(|error| {
                AppError::BadRequest(format!(
                    "RAW image preview and decode failed: {preview_error}; {error}"
                ))
            })?;
            let developed = rawler::imgop::develop::RawDevelop::default()
                .develop_intermediate(&raw)
                .map_err(|error| {
                    AppError::BadRequest(format!("RAW image processing failed: {error}"))
                })?;
            developed
                .to_dynamic_image()
                .ok_or_else(|| AppError::BadRequest("RAW image pixel data is invalid".into()))?
        }
    };
    let image = if let Some(max_dimension) = max_dimension {
        image.thumbnail(max_dimension, max_dimension)
    } else {
        image
    };
    let rgb = DynamicImage::ImageRgb8(image.to_rgb8());
    let mut output = Cursor::new(Vec::new());
    JpegEncoder::new_with_quality(&mut output, if max_dimension.is_some() { 78 } else { 92 })
        .encode_image(&rgb)
        .map_err(|error| AppError::BadRequest(format!("RAW preview encoding failed: {error}")))?;
    Ok(output.into_inner())
}

#[derive(Debug, Deserialize)]
pub struct UploadQuery {
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default, rename = "token")]
    pub access_token: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct UploadResult {
    name: String,
    path: String,
    size: u64,
}

#[derive(Debug, Deserialize)]
pub struct ChunkUploadQuery {
    #[serde(rename = "token")]
    pub access_token: Option<String>,
}

/// Start or resume a resumable chunked upload.
pub async fn upload_init(
    State(state): State<HttpState>,
    Path(id): Path<String>,
    Query(query): Query<ChunkUploadQuery>,
    headers: HeaderMap,
    Json(request): Json<super::super::uploads::InitRequest>,
) -> AppResult<Json<super::super::uploads::SessionStatus>> {
    let share = authorize_request(&state, &id, query.access_token, &headers)?;
    state.events.watch(&share, state.directory_tree_for(&id));
    Ok(Json(
        state.uploads.init(&id, share.path_ref(), request).await?,
    ))
}

pub async fn upload_status(
    State(state): State<HttpState>,
    Path((id, upload_id)): Path<(String, String)>,
    Query(query): Query<ChunkUploadQuery>,
    headers: HeaderMap,
) -> AppResult<Json<super::super::uploads::SessionStatus>> {
    let _share = authorize_request(&state, &id, query.access_token, &headers)?;
    Ok(Json(state.uploads.status(&id, &upload_id).await?))
}

pub async fn upload_chunk(
    State(state): State<HttpState>,
    Path((id, upload_id)): Path<(String, String)>,
    Query(query): Query<ChunkUploadQuery>,
    headers: HeaderMap,
    body: Bytes,
) -> AppResult<Json<super::super::uploads::SessionStatus>> {
    let share = authorize_request(&state, &id, query.access_token, &headers)?;
    let range = headers
        .get(header::CONTENT_RANGE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    let status = state
        .uploads
        .append(&id, share.path_ref(), &upload_id, range, &body)
        .await?;
    state.events.watch(&share, state.directory_tree_for(&id));
    Ok(Json(status))
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
        .map_err(|err| AppError::BadRequest(format!("Multipart parsing failed: {err}")))?
    {
        if field.name() != Some("file") {
            continue;
        }
        let original = field.file_name().unwrap_or("upload.bin").to_string();
        let virtual_target = join_dir(&target_dir, &original);
        let (real_target, file_name) =
            security::resolve_for_write(share.path_ref(), &virtual_target)?;

        let mut file = match tokio::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&real_target)
            .await
        {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                return Err(AppError::Conflict(
                    "Upload destination already exists".into(),
                ));
            }
            Err(error) => return Err(error.into()),
        };
        let upload_result = async {
            let mut size = 0u64;
            while let Some(chunk) = field.chunk().await.map_err(|err| {
                AppError::BadRequest(format!("Failed to read uploaded data: {err}"))
            })? {
                size += chunk.len() as u64;
                file.write_all(&chunk).await?;
            }
            file.flush().await?;
            Ok::<u64, AppError>(size)
        }
        .await;
        let size = match upload_result {
            Ok(size) => size,
            Err(error) => {
                drop(file);
                let _ = tokio::fs::remove_file(&real_target).await;
                return Err(error);
            }
        };
        return Ok(Json(UploadResult {
            name: file_name,
            path: virtual_target,
            size,
        }));
    }
    Err(AppError::BadRequest(
        "No upload field named file was found".into(),
    ))
}

/// 公共鉴权：加载共享、提取调用方、执行访问控制。
fn authorize_request(
    state: &HttpState,
    id: &str,
    query_token: Option<String>,
    headers: &HeaderMap,
) -> AppResult<ShareConfig> {
    let share = share_repo::get_by_id(&state.db, id)?;
    if !state.allows_share(&share) {
        return Err(AppError::NotFound(
            "Use the protocol configured for this share".into(),
        ));
    }
    let caller = extract_caller(headers, query_token);
    if let Err(error) = authorize(&share, &caller, &state.db) {
        if share.access_mode == crate::share::AccessMode::Token
            && matches!(error, AppError::Forbidden(_))
        {
            return Err(AppError::NotFound("HTTP share does not exist".into()));
        }
        return Err(error);
    }
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
