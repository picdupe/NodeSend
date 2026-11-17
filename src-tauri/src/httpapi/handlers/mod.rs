//! 文件功能：HTTP 处理器模块入口与路由表。
//!
//! - [`router`]：组装全部对外 HTTP 路由，并挂载日志、CORS 中间件；
//! - 路由分组：
//!   - 通用：/api/v1/hello
//!   - 共享：/api/v1/shares、/api/v1/shares/{id}/{tree,list,info,download,upload}
//!   - 集成：/api/v1/integration/share-url（基线 §49）
//! - 具体处理逻辑拆分在 [`shares`] 与 [`integration`] 两个文件。
pub mod integration;
pub mod shares;

use axum::{
    extract::{DefaultBodyLimit, Request},
    http::{header, HeaderValue},
    middleware::{self, Next},
    response::Response,
    routing::{get, post},
    Router,
};
use tower_http::{cors::CorsLayer, trace::TraceLayer};

use super::HttpState;

async fn token_query_header(mut request: Request, next: Next) -> Response {
    let token = request
        .uri()
        .query()
        .into_iter()
        .flat_map(|query| url::form_urlencoded::parse(query.as_bytes()))
        .find_map(|(name, value)| (name == "token").then(|| value.into_owned()));
    if let Some(token) = token {
        if let Ok(value) = HeaderValue::from_str(&format!("Bearer {token}")) {
            request.headers_mut().insert(header::AUTHORIZATION, value);
        }
    }
    next.run(request).await
}

pub fn router(state: HttpState) -> Router {
    let v1 = Router::new()
        .route("/hello", get(shares::hello))
        .route("/shares", get(shares::list_shares))
        .route("/shares/:id/tree", get(shares::tree))
        .route("/shares/:id/browser", get(shares::browser))
        .route("/shares/:id/list", get(shares::list))
        .route("/shares/:id/info", get(shares::info))
        .route(
            "/shares/:id/download",
            get(shares::download).head(shares::download),
        )
        .route("/shares/:id/download-folder", get(shares::download_folder))
        .route("/shares/:id/preview", get(shares::preview))
        .route("/shares/:id/search", get(shares::search))
        .route("/shares/:id/events", get(super::events::events))
        .route("/shares/:id/upload", post(shares::upload))
        .route("/shares/:id/upload/init", post(shares::upload_init))
        .route(
            "/shares/:id/upload/:upload_id",
            get(shares::upload_status).put(shares::upload_chunk),
        )
        .route("/integration/share-url", get(integration::share_url));

    Router::new()
        .route("/:share_name/dav", axum::routing::any(shares::browser_webdav_root))
        .route("/:share_name/dav/", axum::routing::any(shares::browser_webdav_root))
        .route("/:share_name/dav/*path", axum::routing::any(shares::browser_webdav_path))
        .route("/", get(shares::browser_root))
        .route("/:share_name", get(shares::browser_named_root))
        .route("/:share_name/login", post(shares::browser_named_login))
        .route("/share-ui/", get(shares::browser_index))
        .route("/share-ui/favicon.svg", get(shares::browser_favicon))
        .route("/share-ui/webui.js", get(shares::browser_script))
        .route("/share-ui/webui.css", get(shares::browser_styles))
        .route(
            "/share-ui/assets/runtime-dom.esm-bundler-DYBPHc56.js",
            get(shares::browser_runtime),
        )
        .route("/share-ui/language.js", get(shares::browser_language))
        .route("/share-ui/i18n.js", get(shares::browser_i18n))
        .route("/api/status", get(shares::browser_status))
        .route("/api/list", get(shares::browser_list))
        .route("/api/search", get(shares::browser_search))
        .route("/api/events", get(shares::browser_events))
        .route(
            "/api/web-downloads/ack/:token",
            get(super::web_download_ack::poll).post(super::web_download_ack::acknowledge),
        )
        .route(
            "/download",
            get(shares::browser_download).head(shares::browser_download),
        )
        .route("/download-folder", get(shares::browser_download_folder))
        .route("/preview", get(shares::browser_preview))
        .route("/api/upload/init", post(shares::browser_upload_init))
        .route(
            "/api/upload/:upload_id",
            axum::routing::put(shares::browser_upload_chunk),
        )
        .nest("/api/v1", v1)
        .fallback(shares::not_found_page)
        .layer(TraceLayer::new_for_http())
        .layer(middleware::from_fn(token_query_header))
        .layer(DefaultBodyLimit::max(16 * 1024 * 1024))
        .layer(CorsLayer::permissive())
        .with_state(state)
}
