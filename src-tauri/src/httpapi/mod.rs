//! 文件功能：内嵌 HTTP 共享服务模块入口（基线 §12）。
//!
//! - [`HttpState`]：HTTP 层共享状态（数据库 + 本机身份）；
//! - [`start`]：绑定端口启动 axum 服务（供集成测试使用，服务整个生命周期）；
//! - [`HttpManager`]：正式运行时的生命周期管理器——HTTP 共享是附加功能，
//!   软件启动不自动开启；由用户在仪表盘手动开启（即时绑定端口启动）、
//!   手动关闭（优雅关停），同一时刻至多一个实例；
//! - 为 [`AppError`] 实现 axum 的 IntoResponse，统一映射为 HTTP 状态码与错误 JSON；
//! - 对外提供：HELLO、共享列表、目录树、目录列表、文件信息、下载、上传、
//!   QA Integration 动态 URL（§49）。
pub mod events;
pub mod handlers;
pub mod security;
pub mod tree;
pub mod uploads;
pub mod web_download_ack;

use std::net::SocketAddr;
use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, RwLock},
};

use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use tokio::sync::oneshot;

use crate::db::Db;
use crate::error::{AppError, AppResult};
use crate::identity::NodeIdentity;
use crate::service::AppService;

/// HTTP 请求处理共享状态（Clone 成本低，内部均为 Arc）。
#[derive(Clone)]
pub struct HttpState {
    pub db: Arc<Db>,
    pub identity: Arc<NodeIdentity>,
    pub events: Arc<events::ShareEvents>,
    pub uploads: Arc<uploads::UploadSessions>,
    pub directory_tree: Option<Arc<RwLock<Vec<tree::DirectoryNode>>>>,
    pub directory_trees:
        Option<Arc<RwLock<HashMap<String, Arc<RwLock<Vec<tree::DirectoryNode>>>>>>>,
    pub dav_handlers: Option<Arc<RwLock<HashMap<String, dav_server::DavHandler>>>>,
    pub share_scope: Option<String>,
    pub protocol_scope: Option<crate::share::ShareProtocol>,
    pub allowed_share_ids: Option<Arc<RwLock<HashSet<String>>>>,
}

impl HttpState {
    pub fn new(db: Arc<Db>, identity: Arc<NodeIdentity>, data_dir: &std::path::Path) -> Self {
        Self {
            db,
            identity,
            events: Arc::new(events::ShareEvents::new()),
            uploads: Arc::new(uploads::UploadSessions::new(data_dir.join("http-uploads"))),
            directory_tree: None,
            directory_trees: None,
            dav_handlers: None,
            share_scope: None,
            protocol_scope: None,
            allowed_share_ids: None,
        }
    }

    pub fn scoped_to_share(mut self, id: String, protocol: crate::share::ShareProtocol) -> Self {
        self.share_scope = Some(id);
        self.protocol_scope = Some(protocol);
        self
    }

    pub fn scoped_to_protocol(mut self, protocol: crate::share::ShareProtocol) -> Self {
        self.protocol_scope = Some(protocol);
        self
    }

    pub fn with_allowed_share_ids(
        mut self,
        allowed_share_ids: Arc<RwLock<HashSet<String>>>,
    ) -> Self {
        self.allowed_share_ids = Some(allowed_share_ids);
        self
    }

    pub fn with_directory_tree(
        mut self,
        directory_tree: Arc<RwLock<Vec<tree::DirectoryNode>>>,
    ) -> Self {
        self.directory_tree = Some(directory_tree);
        self
    }

    pub fn with_directory_trees(
        mut self,
        directory_trees: Arc<RwLock<HashMap<String, Arc<RwLock<Vec<tree::DirectoryNode>>>>>>,
    ) -> Self {
        self.directory_trees = Some(directory_trees);
        self
    }

    pub fn with_dav_handlers(
        mut self,
        dav_handlers: Arc<RwLock<HashMap<String, dav_server::DavHandler>>>,
    ) -> Self {
        self.dav_handlers = Some(dav_handlers);
        self
    }

    pub fn directory_tree_for(&self, id: &str) -> Option<Arc<RwLock<Vec<tree::DirectoryNode>>>> {
        self.directory_trees
            .as_ref()
            .and_then(|trees| trees.read().ok()?.get(id).cloned())
            .or_else(|| self.directory_tree.clone())
    }

    pub fn allows_share(&self, share: &crate::share::ShareConfig) -> bool {
        if self.allowed_share_ids.as_ref().is_some_and(|ids| {
            !ids.read()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .contains(&share.id)
        }) {
            return false;
        }
        if self
            .share_scope
            .as_deref()
            .is_some_and(|id| id != share.id.as_str())
        {
            return false;
        }
        match (self.share_scope.as_deref(), self.protocol_scope) {
            (None, Some(crate::share::ShareProtocol::Http)) => {
                share.protocol == crate::share::ShareProtocol::Http
            }
            (_, Some(protocol)) => share.protocol == protocol,
            (Some(_), None) => share.protocol == crate::share::ShareProtocol::Nodesend,
            (None, None) => share.protocol == crate::share::ShareProtocol::Nodesend,
        }
    }
}

/// 启动 HTTP 共享服务（在 Tauri 异步运行时中运行，随应用生命周期退出）。
pub async fn start(state: HttpState, addr: SocketAddr) {
    let app = handlers::router(state);
    let listener = match tokio::net::TcpListener::bind(addr).await {
        Ok(listener) => listener,
        Err(err) => {
            tracing::error!(%addr, "Failed to start HTTP share server: {err}");
            return;
        }
    };
    tracing::info!("HTTP share server started: http://{addr}");
    if let Err(err) = axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await
    {
        tracing::error!("HTTP share server exited unexpectedly: {err}");
    }
}

/// 正在运行的 HTTP 服务实例记录：关停信号 + 后台任务句柄。
struct RunningService {
    shutdown: oneshot::Sender<()>,
    task: tauri::async_runtime::JoinHandle<()>,
}

/// HTTP 共享生命周期管理器（附加功能的总开关）。
///
/// - 软件启动时不恢复上次的监听状态；只有用户显式操作才调用 [`apply`](Self::apply)；
/// - 用户在仪表盘开启：apply 绑定端口即时启动，端口绑定失败立即返回错误；
/// - 用户关闭：[`stop`](Self::stop) 发关停信号并等待优雅退出；
/// - tokio 互斥保证同一时刻至多一个实例。
#[derive(Default)]
pub struct HttpManager {
    running: tokio::sync::Mutex<Option<RunningService>>,
}

/// 按需启动的链接分享 HTTP 服务。它与附加 HTTP 共享开关使用不同监听器，
/// 生成链接时才启动，避免关闭 HTTP 共享时仍暴露主服务端口。
#[derive(Default)]
pub struct ShareLinkManager {
    running: tokio::sync::Mutex<
        Option<(
            u16,
            oneshot::Sender<()>,
            tauri::async_runtime::JoinHandle<()>,
        )>,
    >,
}

impl ShareLinkManager {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn ensure(&self, service: &AppService) -> AppResult<u16> {
        let mut guard = self.running.lock().await;
        if let Some((port, _, _)) = guard.as_ref() {
            return Ok(*port);
        }
        let state = HttpState::new(
            Arc::clone(&service.db),
            Arc::clone(&service.identity),
            &service.paths.data_dir,
        );
        let listener = tokio::net::TcpListener::bind(("0.0.0.0", 0)).await?;
        let port = listener.local_addr()?.port();
        let app = handlers::router(state);
        let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
        let task = tauri::async_runtime::spawn(async move {
            let server = axum::serve(
                listener,
                app.into_make_service_with_connect_info::<SocketAddr>(),
            )
            .with_graceful_shutdown(async move {
                let _ = shutdown_rx.await;
            });
            if let Err(err) = server.await {
                tracing::error!("Link sharing HTTP server exited unexpectedly: {err}");
            }
        });
        *guard = Some((port, shutdown_tx, task));
        Ok(port)
    }
}

impl HttpManager {
    pub fn new() -> Self {
        Self::default()
    }

    /// 按 Service 中已持久化的设置同步运行状态：
    /// 设置启用且当前未运行 → 绑定端口并启动；设置关闭或已在运行 → 不做改动。
    pub async fn apply(&self, service: &AppService) -> AppResult<()> {
        if !service.http_enabled() {
            return Ok(());
        }
        let addr = service.http_addr()?;

        let mut guard = self.running.lock().await;
        // 已有实例则不重复启动（关停一律走 stop，stop 会等待任务结束并清理记录）。
        if guard.is_some() {
            return Ok(());
        }

        let state = HttpState::new(
            Arc::clone(&service.db),
            Arc::clone(&service.identity),
            &service.paths.data_dir,
        );
        let app = handlers::router(state);
        // 在 spawn 前完成端口绑定：失败直接把错误返回给 UI，而不是静默失败。
        let listener = tokio::net::TcpListener::bind(addr).await?;

        let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
        let task = tauri::async_runtime::spawn(async move {
            tracing::info!("HTTP share server started: http://{addr}");
            let server = axum::serve(
                listener,
                app.into_make_service_with_connect_info::<SocketAddr>(),
            )
            .with_graceful_shutdown(async move {
                let _ = shutdown_rx.await;
            });
            match server.await {
                Ok(()) => tracing::info!("HTTP share server stopped"),
                Err(err) => tracing::error!("HTTP share server exited unexpectedly: {err}"),
            }
        });

        *guard = Some(RunningService {
            shutdown: shutdown_tx,
            task,
        });
        Ok(())
    }

    /// 当前监听器是否真的在运行。配置项与运行状态刻意分开：
    /// 应用启动不会自动恢复 HTTP 共享。
    pub async fn is_running(&self) -> bool {
        self.running.lock().await.is_some()
    }

    /// 停止正在运行的 HTTP 服务：发关停信号并等待优雅退出（任务已结束则立即返回）。
    pub async fn stop(&self) {
        let mut guard = self.running.lock().await;
        if let Some(running) = guard.take() {
            let _ = running.shutdown.send(());
            let _ = running.task.await;
        }
    }
}

/// 业务错误 → HTTP 响应：状态码 + `{ "error": { "code", "message" } }`。
impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let status = match &self {
            AppError::BadRequest(_) => StatusCode::BAD_REQUEST,
            AppError::NotFound(_) => StatusCode::NOT_FOUND,
            AppError::Forbidden(_) => StatusCode::FORBIDDEN,
            AppError::Conflict(_) => StatusCode::CONFLICT,
            _ => StatusCode::INTERNAL_SERVER_ERROR,
        };
        let body = Json(serde_json::json!({
            "error": { "code": self.code(), "message": self.to_string() }
        }));
        (status, body).into_response()
    }
}
