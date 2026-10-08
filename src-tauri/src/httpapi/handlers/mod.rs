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
    routing::{get, post},
    Router,
};
use tower_http::{cors::CorsLayer, trace::TraceLayer};

use super::HttpState;

pub fn router(state: HttpState) -> Router {
    let v1 = Router::new()
        .route("/hello", get(shares::hello))
        .route("/shares", get(shares::list_shares))
        .route("/shares/:id/tree", get(shares::tree))
        .route("/shares/:id/list", get(shares::list))
        .route("/shares/:id/info", get(shares::info))
        .route("/shares/:id/download", get(shares::download))
        .route("/shares/:id/upload", post(shares::upload))
        .route("/integration/share-url", get(integration::share_url));

    Router::new()
        .nest("/api/v1", v1)
        .layer(TraceLayer::new_for_http())
        .layer(CorsLayer::permissive())
        .with_state(state)
}
