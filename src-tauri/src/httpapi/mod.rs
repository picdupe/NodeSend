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
pub mod handlers;
pub mod security;
pub mod tree;

use std::net::SocketAddr;
use std::sync::Arc;

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
}

impl HttpState {
    pub fn new(db: Arc<Db>, identity: Arc<NodeIdentity>) -> Self {
        Self { db, identity }
    }
}

/// 启动 HTTP 共享服务（在 Tauri 异步运行时中运行，随应用生命周期退出）。
pub async fn start(state: HttpState, addr: SocketAddr) {
    let app = handlers::router(state);
    let listener = match tokio::net::TcpListener::bind(addr).await {
        Ok(listener) => listener,
        Err(err) => {
            tracing::error!(%addr, "HTTP 共享服务启动失败: {err}");
            return;
        }
    };
    tracing::info!("HTTP 共享服务已启动: http://{addr}");
    if let Err(err) = axum::serve(listener, app).await {
        tracing::error!("HTTP 共享服务异常退出: {err}");
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
        let state = HttpState::new(Arc::clone(&service.db), Arc::clone(&service.identity));
        let listener = tokio::net::TcpListener::bind(("0.0.0.0", 0)).await?;
        let port = listener.local_addr()?.port();
        let app = handlers::router(state);
        let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
        let task = tauri::async_runtime::spawn(async move {
            let server = axum::serve(listener, app).with_graceful_shutdown(async move {
                let _ = shutdown_rx.await;
            });
            if let Err(err) = server.await {
                tracing::error!("链接分享 HTTP 服务异常退出: {err}");
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

        let state = HttpState::new(Arc::clone(&service.db), Arc::clone(&service.identity));
        let app = handlers::router(state);
        // 在 spawn 前完成端口绑定：失败直接把错误返回给 UI，而不是静默失败。
        let listener = tokio::net::TcpListener::bind(addr).await?;

        let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
        let task = tauri::async_runtime::spawn(async move {
            tracing::info!("HTTP 共享服务已启动: http://{addr}");
            let server = axum::serve(listener, app).with_graceful_shutdown(async move {
                let _ = shutdown_rx.await;
            });
            match server.await {
                Ok(()) => tracing::info!("HTTP 共享服务已停止"),
                Err(err) => tracing::error!("HTTP 共享服务异常退出: {err}"),
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
