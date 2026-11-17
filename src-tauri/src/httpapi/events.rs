use std::{
    collections::HashMap,
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock, RwLock},
};

use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        ConnectInfo, Extension, Path as AxumPath, Query, State,
    },
    http::HeaderMap,
    response::{IntoResponse, Response},
};
use futures_util::StreamExt;
use notify::{RecursiveMode, Watcher};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::sync::broadcast;

use crate::{
    access::{authorize, Caller},
    error::{AppError, AppResult},
    share::{repository as share_repo, ShareConfig},
};

pub struct ShareEvents {
    watchers: Mutex<HashMap<String, notify::RecommendedWatcher>>,
    sender: broadcast::Sender<Value>,
}

static LANGUAGE_UPDATES: OnceLock<broadcast::Sender<String>> = OnceLock::new();

fn language_update_sender() -> &'static broadcast::Sender<String> {
    LANGUAGE_UPDATES.get_or_init(|| broadcast::channel(32).0)
}

pub fn notify_language_changed(preference: &str) {
    let language = match preference {
        "zh-CN" | "zh-TW" | "en-US" | "ja-JP" => preference.to_owned(),
        "system" => crate::locale::preferred_language(),
        _ => return,
    };
    let _ = language_update_sender().send(language);
}

impl ShareEvents {
    pub fn new() -> Self {
        let (sender, _) = broadcast::channel(1024);
        Self {
            watchers: Mutex::new(HashMap::new()),
            sender,
        }
    }

    pub fn watch(
        &self,
        share: &ShareConfig,
        directory_tree: Option<Arc<RwLock<Vec<super::tree::DirectoryNode>>>>,
    ) {
        let mut watchers = self
            .watchers
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        if watchers.contains_key(&share.id) {
            return;
        }
        let root = match std::fs::canonicalize(share.path_ref()) {
            Ok(root) => root,
            Err(error) => {
                tracing::warn!(share_id = %share.id, %error, "Could not watch shared directory");
                return;
            }
        };
        let share_id = share.id.clone();
        let sender = self.sender.clone();
        let watcher =
            notify::recommended_watcher(move |event: Result<notify::Event, notify::Error>| {
                let Ok(event) = event else {
                    return;
                };
                let paths: Vec<String> = event
                    .paths
                    .iter()
                    .filter_map(|path| relative_path(&root, path))
                    .collect();
                if paths.is_empty() {
                    return;
                }
                if let Some(directory_tree) = &directory_tree {
                    let mut snapshot = directory_tree
                        .write()
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                    super::tree::refresh_directory_tree_paths(&root, &mut snapshot, &paths);
                }
                let _ =
                    sender.send(json!({"type":"tree_changed", "share_id":share_id, "paths":paths}));
            });
        match watcher {
            Ok(mut watcher) => match watcher.watch(share.path_ref(), RecursiveMode::Recursive) {
                Ok(()) => {
                    watchers.insert(share.id.clone(), watcher);
                }
                Err(error) => {
                    tracing::warn!(share_id = %share.id, %error, "Could not watch shared directory")
                }
            },
            Err(error) => {
                tracing::warn!(share_id = %share.id, %error, "Could not create directory watcher")
            }
        }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Value> {
        self.sender.subscribe()
    }

    pub fn unwatch(&self, share_id: &str) {
        self.watchers
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .remove(share_id);
    }
}

fn relative_path(root: &Path, path: &Path) -> Option<String> {
    let path = std::fs::canonicalize(path)
        .ok()
        .or_else(|| lexical_absolute(path).ok())?;
    if !path.starts_with(root) {
        return None;
    }
    Some(
        path.strip_prefix(root)
            .ok()?
            .to_string_lossy()
            .replace('\\', "/"),
    )
}

fn lexical_absolute(path: &Path) -> std::io::Result<PathBuf> {
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        std::env::current_dir().map(|cwd| cwd.join(path))
    }
}

#[derive(Deserialize)]
pub struct EventQuery {
    #[serde(rename = "token")]
    pub access_token: Option<String>,
}

pub async fn events(
    State(state): State<super::HttpState>,
    AxumPath(id): AxumPath<String>,
    Query(query): Query<EventQuery>,
    headers: HeaderMap,
    upgrade: WebSocketUpgrade,
    client: Option<Extension<ConnectInfo<SocketAddr>>>,
) -> AppResult<Response> {
    let share = share_repo::get_by_id(&state.db, &id)?;
    if !state.allows_share(&share) {
        return Err(AppError::NotFound(
            "Use the protocol configured for this share".into(),
        ));
    }
    if let Err(error) = authorize(&share, &caller(&headers, query.access_token), &state.db) {
        if share.access_mode == crate::share::AccessMode::Token
            && matches!(error, AppError::Forbidden(_))
        {
            return Err(AppError::NotFound("Share does not exist".into()));
        }
        return Err(error);
    }
    state.events.watch(&share, state.directory_tree_for(&id));
    let language_receiver = language_update_sender().subscribe();
    let client_ip = client
        .as_ref()
        .map(|Extension(ConnectInfo(address))| address.ip());
    let status = super::handlers::shares::browser_status_payload(
        &state,
        &id,
        &share,
        super::handlers::shares::same_device_client(client_ip),
    )?;
    let receiver = state.events.subscribe();
    Ok(upgrade
        .on_upgrade(move |socket| pump(socket, receiver, language_receiver, id, status))
        .into_response())
}

fn caller(headers: &HeaderMap, query_token: Option<String>) -> Caller {
    let node_id = headers
        .get("x-nodesend-node-id")
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let token = query_token.or_else(|| {
        headers
            .get(axum::http::header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.strip_prefix("Bearer "))
            .map(str::to_owned)
    });
    Caller::new(node_id, token)
}

async fn pump(
    mut socket: WebSocket,
    mut receiver: broadcast::Receiver<Value>,
    mut language_receiver: broadcast::Receiver<String>,
    share_id: String,
    status: Value,
) {
    let mut current_language = status
        .get("language")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let initial_status = json!({"type":"status", "data":status});
    if socket
        .send(Message::Text(initial_status.to_string().into()))
        .await
        .is_err()
    {
        return;
    }
    loop {
        tokio::select! {
            language = language_receiver.recv() => match language {
                Ok(language) if language != current_language => {
                    current_language = language.clone();
                    let update = json!({"type":"language", "data":language});
                    if socket.send(Message::Text(update.to_string().into())).await.is_err() { break; }
                }
                Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => break,
            },
            event = receiver.recv() => match event {
                Ok(value) if value.get("share_id").and_then(Value::as_str) == Some(&share_id) => {
                    if socket.send(Message::Text(value.to_string().into())).await.is_err() { break; }
                }
                Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => break,
            },
            message = socket.next() => match message {
                Some(Ok(Message::Ping(payload))) => if socket.send(Message::Pong(payload)).await.is_err() { break; },
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                _ => {}
            }
        }
    }
}
