pub mod browser;
pub mod discovery;
pub mod peers;
pub mod protocol;
pub(crate) mod state;
pub mod tls;
pub mod transport;
use crate::locks::MutexExt;
use crate::{
    error::{AppError, AppResult},
    service::AppService,
    transfer::{
        model::{ConflictPolicy, TaskSnapshot},
        TransferService,
    },
};
use discovery::{Announcement, Discovery, DiscoveryStatus};
use peers::{Endpoint, Peer, PeerStore};
use serde::Serialize;
use std::{
    collections::HashMap,
    net::SocketAddr,
    sync::{Arc, Mutex},
};
use tokio_util::sync::CancellationToken;
#[derive(Debug, Clone)]
pub struct NetworkOptions {
    pub bind: SocketAddr,
    pub discovery: bool,
}
impl Default for NetworkOptions {
    fn default() -> Self {
        Self {
            bind: "[::]:8192".parse().unwrap(),
            discovery: true,
        }
    }
}
#[derive(Serialize)]
pub struct NetworkStatus {
    pub node_id: String,
    pub display_name: String,
    pub tcp_port: u16,
    pub quic_port: u16,
    pub addresses: Vec<String>,
    pub discovery: DiscoveryStatus,
    pub peers: Vec<Peer>,
    pub tasks: Vec<TaskSnapshot>,
    pub receive_directory: String,
    pub cache_directory: String,
    pub receive_policy: String,
    pub media_library_enabled: bool,
}
pub struct NetworkManager {
    app: Arc<AppService>,
    pub transfers: Arc<TransferService>,
    pub peers: Arc<PeerStore>,
    pub tcp_port: u16,
    pub quic_port: u16,
    discovery: Option<Discovery>,
    server: Mutex<Option<tokio::task::JoinHandle<()>>>,
    preflights: Mutex<HashMap<String, CancellationToken>>,
}
impl NetworkManager {
    pub fn ui_language(&self) -> String {
        self.app
            .get_setting("ui_language")
            .ok()
            .flatten()
            .filter(|value| matches!(value.as_str(), "zh-CN" | "zh-TW" | "en-US" | "ja-JP"))
            .unwrap_or_else(crate::locale::preferred_language)
    }

    pub async fn start(app: Arc<AppService>) -> AppResult<Arc<Self>> {
        Self::start_with(app, NetworkOptions::default()).await
    }
    pub async fn start_with(app: Arc<AppService>, options: NetworkOptions) -> AppResult<Arc<Self>> {
        let mut last_error = None;
        let requested = options.bind.port();
        let base_port = requested.max(8192);
        let ports = if requested == 0 {
            0..=0
        } else {
            base_port..=u16::MAX
        };
        let mut listeners = None;
        for port in ports {
            let ipv6 = SocketAddr::new(options.bind.ip(), port);
            match transport::bind(&app, ipv6) {
                Ok(value) => {
                    listeners = Some(value);
                    break;
                }
                Err(error) if options.bind.is_ipv6() && options.bind.ip().is_unspecified() => {
                    tracing::debug!(port, error = %error, "IPv6 dual-stack listener failed; falling back to IPv4. IPv6 addresses will be unreachable.");
                    match transport::bind(&app, SocketAddr::from(([0, 0, 0, 0], port))) {
                        Ok(value) => {
                            listeners = Some(value);
                            break;
                        }
                        Err(fallback_error) => last_error = Some(fallback_error),
                    }
                }
                Err(error) => last_error = Some(error),
            }
        }
        let listeners = listeners.ok_or_else(|| {
            last_error.unwrap_or_else(|| {
                AppError::Other("Failed to bind a TCP listener".into())
            })
        })?;
        let tcp_port = listeners.tcp_port;
        let quic_port = listeners.quic_port;
        let peers = PeerStore::new(app.clone())?;
        let transfers = TransferService::new(app.clone())?;
        let discovery = options.discovery.then(|| {
            Discovery::start(
                Announcement {
                    app: "nodesend".into(),
                    protocol_version: protocol::VERSION,
                    node_id: app.identity.node_id.clone(),
                    display_name: transport::display_name(&app),
                    port: tcp_port,
                    quic_port,
                    capabilities: vec![
                        "files".into(),
                        "folders".into(),
                        "chunks".into(),
                        "tls13".into(),
                    ],
                },
                peers.clone(),
            )
        });
        let server = tokio::spawn(transport::serve(
            listeners,
            transfers.clone(),
            peers.clone(),
        ));
        let manager = Arc::new(Self {
            app,
            transfers,
            peers,
            tcp_port,
            quic_port,
            discovery,
            server: Mutex::new(Some(server)),
            preflights: Mutex::new(HashMap::new()),
        });
        for task in manager
            .transfers
            .snapshots()
            .into_iter()
            .filter(|task| task.direction == "send" && task.status == "interrupted")
        {
            manager.transfers.start_task(manager.peers.clone(), task.id);
        }
        for task in manager
            .transfers
            .snapshots()
            .into_iter()
            .filter(|task| ["paused", "remote_paused"].contains(&task.status.as_str()))
        {
            let manager = manager.clone();
            tokio::spawn(async move {
                let Ok(task) = manager.transfers.record(&task.id) else {
                    return;
                };
                let Ok(peer) = manager.peers.get(&task.peer_id) else {
                    return;
                };
                for endpoint in peer.endpoints {
                    let Ok(mut channel) = transport::connect(
                        &manager.app,
                        &endpoint,
                        Some(&task.peer_id),
                        false,
                        task.allow_public,
                    )
                    .await
                    else {
                        continue;
                    };
                    if transport::hello(&mut channel, &manager.app).await.is_err() {
                        continue;
                    }
                    if !matches!(
                        channel
                            .request(&protocol::Request::WatchTask {
                                id: task.wire_id.clone(),
                                revision: task.remote_state_revision,
                            })
                            .await,
                        Ok(protocol::Response::Ack)
                    ) {
                        continue;
                    }
                    state::spawn(
                        channel,
                        manager.transfers.clone(),
                        manager.peers.clone(),
                        task.id,
                        0,
                    )
                    .detach();
                    return;
                }
            });
        }
        Ok(manager)
    }
    pub fn status(&self) -> NetworkStatus {
        let mut discovery = self
            .discovery
            .as_ref()
            .map(|d| crate::locks::lock(&d.status).clone())
            .unwrap_or_default();
        if let Some(warning) = crate::transfer::relay::installation_warning() {
            if !discovery.warnings.contains(&warning) {
                discovery.warnings.push(warning);
            }
        }
        NetworkStatus {
            node_id: self.app.identity.node_id.clone(),
            display_name: transport::display_name(&self.app),
            tcp_port: self.tcp_port,
            quic_port: self.quic_port,
            addresses: if_addrs::get_if_addrs()
                .unwrap_or_default()
                .iter()
                .filter(|i| !i.is_loopback())
                .map(|i| i.ip().to_string())
                .collect(),
            discovery,
            peers: self.peers.list(),
            tasks: self.transfers.snapshots(),
            receive_directory: self.transfers.default_destination(),
            cache_directory: self.transfers.cache_directory(),
            receive_policy: self
                .app
                .get_setting("receive_policy")
                .ok()
                .flatten()
                .unwrap_or_else(|| "ask".into()),
            media_library_enabled: cfg!(target_os = "android")
                && self
                    .app
                    .get_setting("android_save_to_gallery")
                    .ok()
                    .flatten()
                    .as_deref()
                    == Some("true"),
        }
    }
    pub fn pending_manual_receive_count(&self) -> usize {
        let policy = self
            .app
            .get_setting("receive_policy")
            .ok()
            .flatten()
            .unwrap_or_else(|| "ask".into());
        let trusted = crate::registry::list(&self.app.db)
            .map(|nodes| {
                nodes
                    .into_iter()
                    .map(|node| node.node_id)
                    .collect::<std::collections::HashSet<_>>()
            })
            .unwrap_or_default();
        self.transfers
            .snapshots()
            .into_iter()
            .filter(|task| task.direction == "receive" && task.status == "waiting")
            .filter(|task| {
                let auto_receive = trusted.contains(&task.peer_id)
                    && (policy == "trusted"
                        || self
                            .app
                            .get_setting(&format!("auto_receive_{}", task.peer_id))
                            .ok()
                            .flatten()
                            .as_deref()
                            == Some("true"));
                !auto_receive
            })
            .count()
    }
    pub async fn add_endpoint(
        &self,
        address: String,
        expected: Option<String>,
        allow_public: bool,
    ) -> AppResult<Peer> {
        let input = address.trim();
        if input.is_empty() {
            return Err(AppError::BadRequest(
                "Enter the peer IP address or host name".into(),
            ));
        }
        let endpoints = resolve_manual_endpoints(&self.app.identity.node_id, input).await?;
        tracing::info!(address = %input, candidates = endpoints.len(), "Manual connection: trying candidate addresses");
        let mut failure = "No usable Endpoint address".to_string();
        for endpoint in endpoints {
            tracing::info!(address = %endpoint.address, tcp = endpoint.tcp_port, quic = endpoint.quic_port, source = %endpoint.source, "Trying candidate address");
            match transport::connect(
                &self.app,
                &endpoint,
                expected.as_deref(),
                false,
                allow_public,
            )
            .await
            {
                Ok(mut channel) => {
                    let handshake = transport::hello(&mut channel, &self.app).await;
                    let (channel, (name, _device_type, tcp_port, quic_port)) = match handshake {
                        Ok(result) => (channel, result),
                        Err(error) if channel.kind == "quic" => {
                            tracing::warn!(
                                address = %endpoint.address,
                                tcp = endpoint.tcp_port,
                                quic = endpoint.quic_port,
                                peer_id = %channel.peer_id,
                                error = %error,
                                "QUIC channel established, but HELLO failed; retrying manual connection over TCP"
                            );
                            let mut tcp_channel = transport::connect(
                                &self.app,
                                &endpoint,
                                expected.as_deref(),
                                true,
                                allow_public,
                            )
                            .await
                            .map_err(|tcp_error| {
                                tracing::warn!(
                                    address = %endpoint.address,
                                    tcp = endpoint.tcp_port,
                                    quic_error = %error,
                                    tcp_error = %tcp_error,
                                    "QUIC HELLO failed and the TCP fallback connection also failed"
                                );
                                AppError::Other(format!(
                                    "QUIC handshake failed: {error}; TCP fallback also failed: {tcp_error}"
                                ))
                            })?;
                            let result = transport::hello(&mut tcp_channel, &self.app)
                                .await
                                .map_err(|tcp_error| {
                                    tracing::warn!(
                                        address = %endpoint.address,
                                        tcp = endpoint.tcp_port,
                                        quic_error = %error,
                                        tcp_error = %tcp_error,
                                        "Both QUIC and TCP NodeSend HELLO handshakes failed"
                                    );
                                    AppError::Other(format!(
                                        "QUIC handshake failed: {error}; TCP handshake also failed: {tcp_error}"
                                    ))
                                })?;
                            (tcp_channel, result)
                        }
                        Err(error) => {
                            tracing::warn!(
                                address = %endpoint.address,
                                tcp = endpoint.tcp_port,
                                transport = channel.kind,
                                peer_id = %channel.peer_id,
                                error = %error,
                                "Manual connection established a transport channel, but the NodeSend HELLO handshake failed"
                            );
                            return Err(AppError::Other(format!(
                                "Connected to the peer's port, but the NodeSend handshake failed: {error}"
                            )));
                        }
                    };
                    if channel.peer_id == self.app.identity.node_id {
                        return Err(AppError::BadRequest("Cannot add this device as a remote device".into()));
                    }
                    let node_id = channel.peer_id.clone();
                    self.peers.upsert(
                        node_id.clone(),
                        name,
                        vec![Endpoint {
                            tcp_port,
                            quic_port,
                            ..endpoint
                        }],
                        true,
                    )?;
                    tracing::info!(peer_id = %node_id, "Device added");
                    return self.peers.get(&node_id);
                }
                Err(err) => {
                    tracing::warn!(address = %endpoint.address, tcp = endpoint.tcp_port, error = %err, "Candidate address connection failed");
                    failure = err.to_string();
                }
            }
        }
        tracing::warn!(address = %input, error = %failure, "Manual connection failed: all candidate addresses are unavailable");
        Err(AppError::Other(failure))
    }
    pub async fn send(
        &self,
        peer_id: String,
        paths: Vec<String>,
        verify_hash: bool,
        allow_public: bool,
        preferred_address: Option<String>,
    ) -> AppResult<String> {
        let cancellation = CancellationToken::new();
        self.preflights
            .lock_or_recover()
            .insert(peer_id.clone(), cancellation.clone());
        let result = self
            .send_with_preflight(
                peer_id.clone(),
                paths,
                verify_hash,
                allow_public,
                preferred_address,
                cancellation,
            )
            .await;
        crate::locks::lock(&self.preflights).remove(&peer_id);
        result
    }

    pub fn cancel_preflight(&self, peer_id: &str) {
        if let Some(cancellation) = crate::locks::lock(&self.preflights).get(peer_id) {
            cancellation.cancel();
        }
    }

    async fn send_with_preflight(
        &self,
        peer_id: String,
        paths: Vec<String>,
        verify_hash: bool,
        allow_public: bool,
        preferred_address: Option<String>,
        cancellation: CancellationToken,
    ) -> AppResult<String> {
        let mut peer = self.peers.get(&peer_id)?;
        if let Some(address) = preferred_address.as_deref() {
            peer.endpoints.sort_by_key(|endpoint| {
                !(endpoint.address == address
                    || address.starts_with(&format!("{}:", endpoint.address))
                    || address.starts_with(&format!("[{}]", endpoint.address)))
            });
        }
        let public_endpoint = peer.endpoints.iter().find(|ep| {
            ep.socket(false)
                .is_ok_and(|addr| !transport::is_local(addr.ip()))
        });
        let mut reachable = false;
        let mut peer_device_type = String::new();
        let mut last_error = String::from("No available address");
        for endpoint in &peer.endpoints {
            if !allow_public
                && !endpoint
                    .socket(false)
                    .is_ok_and(|addr| transport::is_local(addr.ip()))
            {
                continue;
            }
            let result = tokio::select! {
                _ = cancellation.cancelled() => return Err(AppError::Other("Cancelled pre-send connection check.".into())),
                result = async {
                    let mut channel = transport::connect(&self.app, endpoint, Some(&peer_id), false, allow_public).await?;
                    let hello = transport::hello(&mut channel, &self.app).await?;
                    Ok::<_, AppError>((hello, channel))
                } => result,
            };
            match result {
                Ok(((name, device_type, tcp_port, quic_port), channel)) => {
                    peer_device_type = device_type;
                    // 对方证书里的 Node ID 与本机相同：两台设备共用了一份身份
                    // （复制过 %APPDATA%\NodeSend 或指向同一应用数据目录）。
                    // 这种情况必须立刻报错，否则界面会一直停在"连接检测"。
                    if channel.peer_id == self.app.identity.node_id {
                        return Err(AppError::BadRequest(
                            "The peer is using the same Node ID as this device. One identity cannot be used on two devices at once. Delete or recreate the app data directory (%APPDATA%\\NodeSend) on the other device and try again.".into(),
                        ));
                    }
                    self.peers.upsert(
                        peer_id.clone(),
                        name,
                        vec![Endpoint {
                            tcp_port,
                            quic_port,
                            last_seen: chrono::Utc::now().timestamp(),
                            error: None,
                            ..endpoint.clone()
                        }],
                        true,
                    )?;
                    reachable = true;
                    let app = self.app.clone();
                    let peers = self.peers.clone();
                    let peer_id = peer_id.clone();
                    let connected = Endpoint {
                        tcp_port,
                        quic_port,
                        ..endpoint.clone()
                    };
                    tokio::spawn(async move {
                        if let Err(error) = refresh_send_addresses(
                            app,
                            peers,
                            peer_id,
                            connected,
                            channel,
                            allow_public,
                        )
                        .await
                        {
                            tracing::debug!(%error, "Background address update is incomplete; retaining existing addresses");
                        }
                    });
                    break;
                }
                Err(error) => {
                    last_error = error.to_string();
                    self.peers.failure(&peer_id, endpoint, last_error.clone());
                }
            }
        }
        if !reachable {
            if !allow_public {
                if let Some(endpoint) = public_endpoint {
                    return Err(AppError::Forbidden(format!(
                        "Public routes require explicit permission: {}",
                        endpoint.address
                    )));
                }
            }
            tracing::debug!(%peer_id, %last_error, "Pre-send connection check failed");
            return Err(AppError::Other(
                "Could not connect to this device. Make sure NodeSend is open on the other device and both devices are on the same network.".into(),
            ));
        }
        let peer = self.peers.get(&peer_id)?;
        let id = self
            .transfers
            .create_outgoing(
                peer_id,
                peer.display_name,
                peer_device_type,
                paths,
                verify_hash,
                allow_public,
                preferred_address,
            )
            .await?;
        self.transfers.start_task(self.peers.clone(), id.clone());
        Ok(id)
    }
    pub async fn decide(
        &self,
        id: String,
        accepted: bool,
        destination: Option<String>,
        conflict: ConflictPolicy,
        selected: Option<Vec<String>>,
    ) -> AppResult<()> {
        let destination = destination
            .filter(|value| !value.trim().is_empty())
            .or_else(|| Some(self.transfers.default_destination()));
        if accepted
            && destination.as_deref() == Some(self.transfers.default_destination().as_str())
            && !std::path::Path::new(destination.as_ref().unwrap()).exists()
        {
            std::fs::create_dir_all(destination.as_ref().unwrap())?;
        }
        self.transfers
            .decide(&id, accepted, destination, conflict, selected)
            .await?;
        let task = self.transfers.record(&id)?;
        if task.direction == "receive" {
            self.spawn_state_sync(
                id,
                if accepted { "resumed" } else { "rejected" }.into(),
                task.state_revision,
            );
        }
        Ok(())
    }
    pub async fn decide_and_trust(
        &self,
        id: String,
        accepted: bool,
        destination: Option<String>,
        conflict: ConflictPolicy,
        selected: Option<Vec<String>>,
        trust_device: bool,
    ) -> AppResult<()> {
        let task = self.transfers.record(&id)?;
        self.decide(id, accepted, destination, conflict, selected)
            .await?;
        if accepted && trust_device && task.direction == "receive" {
            crate::registry::add(&self.app.db, &task.peer_id, Some(task.peer_name))?;
            self.app
                .set_setting(&format!("auto_receive_{}", task.peer_id), "true")?;
        }
        Ok(())
    }
    pub async fn action(&self, id: String, action: String) -> AppResult<()> {
        #[cfg(target_os = "android")]
        if action == "save" {
            let task = self.transfers.record(&id)?;
            if task.direction != "receive" || task.completed_count != task.chunk_count {
                return Err(AppError::BadRequest("File has not been fully received".into()));
            }
            if task.status == "completed" {
                return crate::android_import::export_received(task)
                    .await
                    .map_err(AppError::Other);
            }
            self.transfers.commit(&id).await?;
            return Ok(());
        }
        let before = self.transfers.record(&id)?;
        if ["resume", "retry"].contains(&action.as_str()) && before.status == "remote_offline" {
            return Err(AppError::Conflict(
                "The peer is offline; reconnect before resuming this task".into(),
            ));
        }
        // 只知道对方的入站连接、没有地址记录的接收任务无法主动回连：
        // 此时本地照常恢复，由发送端重连后继续推送。
        if ["resume", "retry"].contains(&action.as_str())
            && before.direction == "receive"
            && self
                .transfers
                .runtime(&id)
                .state_sessions
                .load(std::sync::atomic::Ordering::SeqCst)
                == 0
            && self.peers.get(&before.peer_id).is_ok()
        {
            self.transfers.edit(&id, |task| {
                task.status = "manual_reconnecting".into();
                task.error = None;
                task.event("Connecting to the sender’s status channel");
            })?;
            let attempt = async {
                let peer = self.peers.get(&before.peer_id)?;
                for endpoint in peer.endpoints {
                    let Ok(mut channel) = transport::connect(
                        &self.app,
                        &endpoint,
                        Some(&before.peer_id),
                        false,
                        before.allow_public,
                    )
                    .await
                    else {
                        continue;
                    };
                    if transport::hello(&mut channel, &self.app).await.is_err() {
                        continue;
                    }
                    if matches!(
                        channel
                            .request(&protocol::Request::ResumeTask {
                                id: before.wire_id.clone(),
                                revision: before.state_revision.wrapping_add(1),
                            })
                            .await,
                        Ok(protocol::Response::Ack)
                    ) {
                        if !matches!(
                            channel
                                .request(&protocol::Request::WatchTask {
                                    id: before.wire_id.clone(),
                                    revision: before.remote_state_revision,
                                })
                                .await,
                            Ok(protocol::Response::Ack)
                        ) {
                            continue;
                        }
                        state::spawn(
                            channel,
                            self.transfers.clone(),
                            self.peers.clone(),
                            id.clone(),
                            0,
                        )
                        .detach();
                        return Ok(());
                    }
                }
                Err(AppError::Other(
                    "The peer is offline; reconnect before resuming this task".into(),
                ))
            }
            .await;
            if let Err(error) = attempt {
                self.transfers.edit(&id, |task| {
                    task.status = "interrupted".into();
                    task.error = Some(error.to_string());
                    task.event(format!("Manual reconnect failed: {error}"));
                })?;
                return Err(error);
            }
        }
        self.transfers.action(&id, &action).await?;
        let task = self.transfers.record(&id)?;
        if ["pause", "resume", "retry", "cancel", "finish"].contains(&action.as_str()) {
            let status = match action.as_str() {
                "pause" => "paused",
                "cancel" => "cancelled",
                "finish" => "finish",
                _ => "resumed",
            };
            self.spawn_state_sync(id.clone(), status.into(), task.state_revision);
        }
        if ["resume", "retry"].contains(&action.as_str())
            && task.direction == "send"
            && !self.transfers.is_active(&id)
        {
            let service = self.transfers.clone();
            let peers = self.peers.clone();
            tokio::spawn(async move {
                while service.is_active(&id) {
                    tokio::select! {_=service.shutdown.cancelled()=>return,_=tokio::time::sleep(std::time::Duration::from_millis(100))=>{}}
                }
                if service.record(&id).is_ok_and(|t| t.status == "queued") {
                    service.start_task(peers, id);
                }
            });
        }
        Ok(())
    }
    fn spawn_state_sync(&self, id: String, status: String, revision: u64) {
        if self
            .transfers
            .runtime(&id)
            .state_sessions
            .load(std::sync::atomic::Ordering::SeqCst)
            > 0
        {
            return;
        }
        if self
            .transfers
            .record(&id)
            .is_ok_and(|task| task.direction == "receive")
        {
            return;
        }
        let service = self.transfers.clone();
        let app = self.app.clone();
        let peers = self.peers.clone();
        tokio::spawn(async move {
            let mut delay = std::time::Duration::from_millis(300);
            loop {
                if service.shutdown.is_cancelled() {
                    return;
                }
                let Ok(current) = service.record(&id) else {
                    return;
                };
                if current.state_revision != revision
                    || current.status == "completed"
                    || (current.status == "rejected" && status != "rejected")
                    || (status != "cancelled" && current.status == "cancelled")
                {
                    return;
                }
                if let Ok(peer) = peers.get(&current.peer_id) {
                    for endpoint in peer.endpoints {
                        let Ok(mut channel) = transport::connect(
                            &app,
                            &endpoint,
                            Some(&current.peer_id),
                            false,
                            current.allow_public,
                        )
                        .await
                        else {
                            continue;
                        };
                        if transport::hello(&mut channel, &app).await.is_err() {
                            continue;
                        }
                        if matches!(
                            channel
                                .request(&protocol::Request::TaskState {
                                    id: current.wire_id.clone(),
                                    revision,
                                    status: status.clone(),
                                })
                                .await,
                            Ok(protocol::Response::Ack)
                        ) {
                            tracing::info!(task = %id, revision, %status, "Peer confirmed task status");
                            return;
                        }
                    }
                }
                tokio::select! {
                    _ = service.shutdown.cancelled() => return,
                    _ = tokio::time::sleep(delay) => {}
                }
                delay = (delay * 2).min(std::time::Duration::from_secs(5));
            }
        });
    }
    pub fn settings(
        &self,
        name: String,
        directory: String,
        cache_directory: String,
        policy: String,
    ) -> AppResult<()> {
        if name.trim().is_empty()
            || name.len() > 128
            || !["ask", "trusted", "reject"].contains(&policy.as_str())
        {
            return Err(AppError::BadRequest("Invalid device name or receive policy".into()));
        }
        // 目录允许尚不存在：保存时只做词法校验，真正接收/缓存时才创建。
        crate::transfer::paths::validate_destination(&directory)?;
        let cache_directory = cache_directory.trim();
        if !cache_directory.is_empty() && !std::path::Path::new(cache_directory).is_absolute() {
            return Err(AppError::BadRequest("Cache directory must be an absolute path".into()));
        }
        self.app.set_setting("display_name", name.trim())?;
        self.app.set_setting("receive_directory", &directory)?;
        self.app.set_setting("cache_directory", cache_directory)?;
        self.app.set_setting("receive_policy", &policy)?;
        Ok(())
    }
    pub fn set_android_media_library_enabled(&self, enabled: bool) -> AppResult<()> {
        self.app.set_setting(
            "android_save_to_gallery",
            if enabled { "true" } else { "false" },
        )
    }
    pub async fn shutdown(&self) {
        if let Some(d) = &self.discovery {
            d.stop();
        }
        self.transfers.shutdown.cancel();
        let handle = crate::locks::lock(&self.server).take();
        if let Some(h) = handle {
            let _ = h.await;
        }
    }
    #[cfg(desktop)]
    pub(crate) fn mark_live_tasks_offline(&self) {
        self.transfers.mark_live_tasks_offline();
    }

    #[cfg(desktop)]
    pub(crate) async fn notify_live_tasks_offline(&self) {
        for task in self.transfers.snapshots() {
            if ![
                "connecting",
                "queued",
                "waiting",
                "accepted",
                "transferring",
                "interrupted",
                "manual_reconnecting",
                "paused",
                "remote_paused",
            ]
            .contains(&task.status.as_str())
            {
                continue;
            }
            let Ok(updated) = self.transfers.edit(&task.id, |record| {
                record.status = "offline".into();
                record.error = None;
                record.state_revision = record.state_revision.wrapping_add(1);
                record.event("The peer was notified to stop automatic reconnection because the app is exiting");
            }) else {
                continue;
            };
            let Ok(peer) = self.peers.get(&updated.peer_id) else {
                continue;
            };
            for endpoint in peer.endpoints {
                let Ok(Ok(mut channel)) = tokio::time::timeout(
                    std::time::Duration::from_secs(2),
                    transport::connect(
                        &self.app,
                        &endpoint,
                        Some(&updated.peer_id),
                        false,
                        updated.allow_public,
                    ),
                )
                .await
                else {
                    continue;
                };
                if transport::hello(&mut channel, &self.app).await.is_err() {
                    continue;
                }
                if matches!(
                    channel
                        .request(&protocol::Request::TaskState {
                            id: updated.wire_id.clone(),
                            revision: updated.state_revision,
                            status: "offline".into(),
                        })
                        .await,
                    Ok(protocol::Response::Ack)
                ) {
                    break;
                }
            }
        }
    }
}

async fn refresh_send_addresses(
    app: Arc<AppService>,
    peers: Arc<PeerStore>,
    peer_id: String,
    mut connected: Endpoint,
    mut channel: transport::Channel,
    allow_public: bool,
) -> AppResult<()> {
    let protocol::Response::Addresses {
        addresses,
        tcp_port,
        quic_port,
    } = channel.request(&protocol::Request::Addresses).await?
    else {
        return Ok(());
    };
    if addresses.len() > 128 || tcp_port == 0 {
        return Err(AppError::BadRequest("Invalid address list".into()));
    }
    drop(channel);
    connected.last_seen = chrono::Utc::now().timestamp();
    connected.error = None;
    let mut verified = vec![connected.clone()];
    let mut probes = tokio::task::JoinSet::new();
    let slots = Arc::new(tokio::sync::Semaphore::new(crate::tuning::MAX_ACTIVE_SENDS));
    let addresses = addresses
        .into_iter()
        .collect::<std::collections::BTreeSet<_>>();
    for address in addresses {
        let Ok(ip) = address.parse::<std::net::IpAddr>() else {
            continue;
        };
        if ip.is_loopback() || ip.is_unspecified() || ip.is_multicast() {
            continue;
        }
        let address = match ip {
            std::net::IpAddr::V6(v) if (v.segments()[0] & 0xffc0) == 0xfe80 => {
                let Some((_, scope)) = connected.address.split_once('%') else {
                    continue;
                };
                format!("{v}%{scope}")
            }
            _ => peers::address(ip),
        };
        if address == connected.address && tcp_port == connected.tcp_port {
            continue;
        }
        let mut endpoint = Endpoint {
            address,
            tcp_port,
            quic_port,
            source: "send-refresh".into(),
            last_seen: chrono::Utc::now().timestamp(),
            error: None,
        };
        if !allow_public && !transport::is_local(ip) {
            endpoint.error = Some("Peer-advertised address; connectivity has not been verified".into());
            verified.push(endpoint);
            continue;
        }
        let app = app.clone();
        let peer_id = peer_id.clone();
        let slots = slots.clone();
        probes.spawn(async move {
            let _permit = slots.acquire_owned().await.ok()?;
            tokio::time::timeout(std::time::Duration::from_secs(6), async {
                let mut channel =
                    transport::connect(&app, &endpoint, Some(&peer_id), true, allow_public)
                        .await
                        .ok()?;
                transport::hello(&mut channel, &app).await.ok()?;
                Some(endpoint)
            })
            .await
            .ok()
            .flatten()
        });
    }
    while let Some(result) = probes.join_next().await {
        if let Ok(Some(endpoint)) = result {
            verified.push(endpoint);
        }
    }
    peers.refresh_addresses(&peer_id, verified)
}

async fn resolve_manual_endpoints(node_id: &str, input: &str) -> AppResult<Vec<Endpoint>> {
    tracing::debug!(input, "Parse manually entered device address");
    let addresses = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        tokio::net::lookup_host(input),
    )
    .await
    .map_err(|_| AppError::Other("Address resolution timed out".into()))?;
    if let Ok(addresses) = addresses {
        let endpoints: Vec<Endpoint> = addresses
            .map(|addr| Endpoint {
                address: endpoint_address(addr),
                tcp_port: addr.port(),
                quic_port: addr.port(),
                source: "manual".into(),
                last_seen: chrono::Utc::now().timestamp(),
                error: None,
            })
            .collect();
        tracing::info!(input, candidates = endpoints.len(), "Using an explicit port");
        return Ok(endpoints);
    }

    let discovery_input = discovery_address(input);
    let targets = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        tokio::net::lookup_host(&discovery_input),
    )
    .await
    .ok()
    .and_then(Result::ok)
    .into_iter()
    .flatten()
    .collect::<Vec<_>>();
    let mut endpoints = Vec::new();
    for target in targets {
        if let Ok(found) = discovery::probe(target, node_id).await {
            tracing::debug!(%target, answered = found.len(), "UDP discovery probe result");
            for (announcement, source) in found {
                endpoints.push(Endpoint {
                    address: endpoint_address(source),
                    tcp_port: announcement.port,
                    quic_port: announcement.quic_port,
                    source: "manual-discovery".into(),
                    last_seen: chrono::Utc::now().timestamp(),
                    error: None,
                });
            }
        }
    }
    if !endpoints.is_empty() {
        return Ok(endpoints);
    }

    let default_tcp = NetworkOptions::default().bind.port();
    let host = input
        .strip_prefix('[')
        .and_then(|value| value.strip_suffix(']'))
        .unwrap_or(input);
    let fallback_input = if host.matches(':').count() > 1 {
        format!("[{host}]:{default_tcp}")
    } else {
        format!("{host}:{default_tcp}")
    };
    let fallback_addresses = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        tokio::net::lookup_host(&fallback_input),
    )
    .await
    .map_err(|_| AppError::Other("Address resolution timed out".into()))??;
    tracing::info!(
        input,
        port = default_tcp,
        "No UDP discovery response; falling back to the default TCP port"
    );
    let fallback = fallback_addresses
        .map(|addr| Endpoint {
            address: endpoint_address(addr),
            tcp_port: default_tcp,
            quic_port: 0,
            source: "manual-fallback".into(),
            last_seen: chrono::Utc::now().timestamp(),
            error: None,
        })
        .collect::<Vec<_>>();
    if fallback.is_empty() {
        tracing::warn!(input, "No NodeSend service was discovered");
        return Err(AppError::Other(
            "No NodeSend service was discovered. Make sure the peer is running and on the same network.".into(),
        ));
    }
    Ok(fallback)
}

fn endpoint_address(addr: SocketAddr) -> String {
    if let SocketAddr::V6(v) = addr {
        if v.scope_id() != 0 {
            return format!("{}%{}", v.ip(), v.scope_id());
        }
    }
    peers::address(addr.ip())
}

fn discovery_address(input: &str) -> String {
    if input.starts_with('[') && input.contains("]:") {
        input.to_string()
    } else if input.starts_with('[') {
        format!("{input}:{}", discovery::UDP_PORT)
    } else if input.matches(':').count() > 1 {
        format!("[{input}]:{}", discovery::UDP_PORT)
    } else {
        format!("{input}:{}", discovery::UDP_PORT)
    }
}
impl Drop for NetworkManager {
    fn drop(&mut self) {
        self.transfers.shutdown.cancel();
        if let Some(d) = &self.discovery {
            d.stop();
        }
    }
}
