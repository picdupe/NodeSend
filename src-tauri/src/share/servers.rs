//! Embedded protocol listeners. Each share owns its listener and credentials.
use super::{AccessMode, ShareConfig, ShareProtocol};
use crate::{
    error::{AppError, AppResult},
    service::AppService,
};
use axum::{
    body::Body,
    extract::{Request, State},
    http::{header, StatusCode},
    response::{IntoResponse, Response},
    routing::any,
    Router,
};
use base64::Engine;
use serde::Serialize;
use std::{collections::HashMap, sync::Arc, time::Duration};
use tokio::{
    io::AsyncReadExt,
    net::{TcpListener, TcpStream},
    sync::Mutex,
    task::JoinHandle,
};
use tokio_util::sync::CancellationToken;

#[derive(Clone, Serialize)]
pub struct ProtocolStatus {
    pub id: String,
    pub running: bool,
    pub port: u16,
    pub error: Option<String>,
}
struct Running {
    cancel: CancellationToken,
    task: JoinHandle<()>,
    port: u16,
    error: Arc<Mutex<Option<String>>>,
}
#[derive(Default)]
pub struct ProtocolManager {
    running: Mutex<HashMap<String, Running>>,
}

fn failure(error: impl std::fmt::Display) -> AppError {
    AppError::Other(format!("共享服务启动失败：{error}"))
}

impl ProtocolManager {
    pub async fn restore(&self, app: Arc<AppService>) -> AppResult<()> {
        for share in super::repository::list_all(&app.db)? {
            if let Err(err) = self.start(app.clone(), &share).await {
                self.running.lock().await.insert(
                    share.id.clone(),
                    Running {
                        cancel: CancellationToken::new(),
                        task: tokio::spawn(async {}),
                        port: share.port,
                        error: Arc::new(Mutex::new(Some(err.to_string()))),
                    },
                );
            }
        }
        Ok(())
    }
    pub async fn stop(&self, id: &str) {
        let mut running = self.running.lock().await;
        if let Some(mut service) = running.remove(id) {
            service.cancel.cancel();
            if tokio::time::timeout(Duration::from_secs(5), &mut service.task)
                .await
                .is_err()
            {
                service.task.abort();
                let _ = service.task.await;
            }
        }
    }

    pub async fn statuses(&self) -> Vec<ProtocolStatus> {
        let running = self.running.lock().await;
        let mut result = Vec::new();
        for (id, service) in running.iter() {
            result.push(ProtocolStatus {
                id: id.clone(),
                running: !service.task.is_finished(),
                port: service.port,
                error: service.error.lock().await.clone(),
            });
        }
        result
    }

    pub async fn start(&self, app: Arc<AppService>, share: &ShareConfig) -> AppResult<u16> {
        let mut running = self.running.lock().await;
        if let Some(service) = running.get(&share.id) {
            if !service.task.is_finished() {
                return Ok(service.port);
            }
        }
        if share.protocol == ShareProtocol::Nodesend
            || !share.enabled
            || share.access_mode == AccessMode::Deny
        {
            return Ok(share.port);
        }
        if share.access_mode == AccessMode::Trusted {
            return Err(AppError::BadRequest(
                "第三方协议不支持 NodeSend 信任身份，请使用密码访问。".into(),
            ));
        }
        let cancel = CancellationToken::new();
        let signal = cancel.clone();
        let error = Arc::new(Mutex::new(None));
        let task_error = error.clone();
        let (port, task) = match share.protocol {
            ShareProtocol::Webdav => {
                let listener = TcpListener::bind(("0.0.0.0", share.port))
                    .await
                    .map_err(failure)?;
                let port = listener.local_addr()?.port();
                let dav = dav_server::DavHandler::builder()
                    .filesystem(Box::new(super::webdav_fs::GuardedFs::new(
                        share.path_ref(),
                    )?))
                    .locksystem(dav_server::memls::MemLs::new())
                    .build_handler();
                let state = DavState {
                    app,
                    id: share.id.clone(),
                    dav,
                };
                let router = Router::new().fallback(any(webdav)).with_state(state);
                let task = tokio::spawn(async move {
                    if let Err(err) = axum::serve(listener, router)
                        .with_graceful_shutdown(signal.cancelled_owned())
                        .await
                    {
                        *task_error.lock().await = Some(err.to_string());
                    }
                });
                (port, task)
            }
            ShareProtocol::Smb => {
                let backend = smb_server::LocalFsBackend::new(&share.path).map_err(failure)?;
                // Stable wire share name also works when the display name contains punctuation.
                let mut builder = smb_server::SmbServer::builder()
                    .listen(([0, 0, 0, 0], share.port).into())
                    .netbios_name("NODESEND");
                let mut smb_share = smb_server::Share::new("share", backend);
                if share.access_mode == AccessMode::Open {
                    smb_share = smb_share.public();
                } else {
                    builder = builder.user(
                        "nodesend",
                        share.access_token.as_deref().unwrap_or_default(),
                    );
                    smb_share = smb_share.user("nodesend", smb_server::Access::ReadWrite);
                }
                let server = builder.share(smb_share).build().map_err(failure)?;
                let port = server.bind().await.map_err(failure)?.port();
                let config = server.config_handle();
                let shutdown = server.shutdown_handle();
                let task = tokio::spawn(async move {
                    tokio::select! {
                        result = server.serve() => if let Err(err) = result { *task_error.lock().await = Some(err.to_string()); },
                        _ = signal.cancelled() => { let _ = config.remove_share("share").await; shutdown.shutdown(); }
                    }
                });
                (port, task)
            }
            ShareProtocol::Ftp => {
                // libunftp binds internally. Reserve/validate the port, then wait for its
                // own greeting or task failure before reporting a successful start.
                let reservation = TcpListener::bind(("0.0.0.0", share.port))
                    .await
                    .map_err(failure)?;
                let port = reservation.local_addr()?.port();
                let root = std::path::PathBuf::from(&share.path);
                let filesystem = unftp_sbe_fs::Filesystem::new(root).map_err(failure)?;
                let auth = FtpAuth {
                    app,
                    id: share.id.clone(),
                };
                let server = libunftp::ServerBuilder::new(Box::new(move || filesystem.clone()))
                    .authenticator(Arc::new(auth))
                    .greeting("NodeSend FTP ready")
                    .passive_ports(50000..=51000)
                    .idle_session_timeout(300)
                    .active_passive_mode(libunftp::options::ActivePassiveMode::PassiveOnly)
                    .shutdown_indicator(async move {
                        signal.cancelled().await;
                        libunftp::options::Shutdown::new().grace_period(Duration::from_secs(2))
                    })
                    .build()
                    .map_err(failure)?;
                drop(reservation);
                let task = tokio::spawn(async move {
                    if let Err(err) = server.listen(format!("0.0.0.0:{port}")).await {
                        *task_error.lock().await = Some(err.to_string());
                    }
                });
                let ready = tokio::time::timeout(Duration::from_secs(3), async {
                    loop {
                        if task.is_finished() {
                            return false;
                        }
                        if let Ok(mut stream) = TcpStream::connect(("127.0.0.1", port)).await {
                            let mut greeting = [0; 128];
                            if let Ok(Ok(n)) = tokio::time::timeout(
                                Duration::from_millis(200),
                                stream.read(&mut greeting),
                            )
                            .await
                            {
                                if String::from_utf8_lossy(&greeting[..n])
                                    .contains("NodeSend FTP ready")
                                {
                                    return true;
                                }
                            }
                        }
                        tokio::time::sleep(Duration::from_millis(20)).await;
                    }
                })
                .await
                .unwrap_or(false);
                if !ready {
                    cancel.cancel();
                    let _ = task.await;
                    return Err(failure(
                        error
                            .lock()
                            .await
                            .clone()
                            .unwrap_or_else(|| "FTP 监听未就绪".into()),
                    ));
                }
                (port, task)
            }
            ShareProtocol::Nodesend => unreachable!(),
        };
        running.insert(
            share.id.clone(),
            Running {
                cancel,
                task,
                port,
                error,
            },
        );
        Ok(port)
    }
}

#[derive(Clone)]
struct DavState {
    app: Arc<AppService>,
    id: String,
    dav: dav_server::DavHandler,
}
async fn webdav(State(state): State<DavState>, request: Request) -> Response {
    let share = match super::repository::get_by_id(&state.app.db, &state.id) {
        Ok(s) => s,
        Err(e) => return e.into_response(),
    };
    let token = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|h| h.to_str().ok())
        .and_then(|h| h.strip_prefix("Basic "))
        .and_then(|v| base64::engine::general_purpose::STANDARD.decode(v).ok())
        .and_then(|v| String::from_utf8(v).ok())
        .and_then(|v| v.strip_prefix("nodesend:").map(str::to_owned));
    if crate::access::authorize(
        &share,
        &crate::access::Caller::new(None, token),
        &state.app.db,
    )
    .is_err()
    {
        return (
            StatusCode::UNAUTHORIZED,
            [(header::WWW_AUTHENTICATE, "Basic realm=\"NodeSend\"")],
            "共享已关闭或访问密码不正确",
        )
            .into_response();
    }
    state.dav.handle(request).await.map(Body::new)
}

struct FtpAuth {
    app: Arc<AppService>,
    id: String,
}
impl std::fmt::Debug for FtpAuth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("NodeSendFtpAuth")
    }
}
#[async_trait::async_trait]
impl unftp_core::auth::Authenticator for FtpAuth {
    async fn authenticate(
        &self,
        username: &str,
        credentials: &unftp_core::auth::Credentials,
    ) -> Result<unftp_core::auth::Principal, unftp_core::auth::AuthenticationError> {
        use unftp_core::auth::{AuthenticationError, Principal};
        let share = super::repository::get_by_id(&self.app.db, &self.id)
            .map_err(|_| AuthenticationError::BadUser)?;
        if share.access_mode == AccessMode::Token && username != "nodesend" {
            return Err(AuthenticationError::BadUser);
        }
        crate::access::authorize(
            &share,
            &crate::access::Caller::new(None, credentials.password.clone()),
            &self.app.db,
        )
        .map_err(|_| AuthenticationError::BadPassword)?;
        Ok(Principal {
            username: username.into(),
        })
    }
}
