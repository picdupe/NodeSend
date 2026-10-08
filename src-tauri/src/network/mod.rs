//! Composition of native discovery, authenticated transports and TransferService.
pub mod browser;
pub mod discovery;
pub mod peers;
pub mod protocol;
pub mod tls;
pub mod transport;
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
            // Dual-stack listeners accept both native IPv6 and IPv4 peers.
            bind: "[::]:58082".parse().unwrap(),
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
    pub receive_policy: String,
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
    pub async fn start(app: Arc<AppService>) -> AppResult<Arc<Self>> {
        Self::start_with(app, NetworkOptions::default()).await
    }
    pub async fn start_with(app: Arc<AppService>, options: NetworkOptions) -> AppResult<Arc<Self>> {
        let listeners = transport::bind(&app, options.bind).or_else(|e| {
            if options.bind.ip().is_unspecified() && options.bind.is_ipv6() {
                tracing::warn!(error = %e, "IPv6 双栈监听失败，回退为仅 IPv4；IPv6 地址将不可连接");
                transport::bind(&app, SocketAddr::from(([0, 0, 0, 0], options.bind.port())))
            } else {
                Err(e)
            }
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
        // Interrupted tasks remain paused until the user explicitly resumes
        // them; startup must never unexpectedly reconnect or send files.
        Ok(manager)
    }
    pub fn status(&self) -> NetworkStatus {
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
            discovery: self
                .discovery
                .as_ref()
                .map(|d| d.status.lock().unwrap().clone())
                .unwrap_or_default(),
            peers: self.peers.list(),
            tasks: self.transfers.snapshots(),
            receive_directory: self.transfers.default_destination(),
            receive_policy: self
                .app
                .get_setting("receive_policy")
                .ok()
                .flatten()
                .unwrap_or_else(|| "ask".into()),
        }
    }
    pub async fn add_endpoint(
        &self,
        address: String,
        expected: Option<String>,
        allow_public: bool,
    ) -> AppResult<Peer> {
        let input = address.trim();
        if input.is_empty() {
            return Err(AppError::BadRequest("请输入对方 IP 或主机名".into()));
        }
        let endpoints = resolve_manual_endpoints(&self.app.identity.node_id, input).await?;
        let mut failure = "Endpoint 无可用地址".to_string();
        for endpoint in endpoints {
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
                    let (name, tcp_port, quic_port) =
                        transport::hello(&mut channel, &self.app).await?;
                    if channel.peer_id == self.app.identity.node_id {
                        return Err(AppError::BadRequest("不能添加本机作为远端设备".into()));
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
                    return self.peers.get(&node_id);
                }
                Err(err) => failure = err.to_string(),
            }
        }
        Err(AppError::Other(failure))
    }
    pub async fn send(
        &self,
        peer_id: String,
        paths: Vec<String>,
        verify_hash: bool,
        allow_public: bool,
    ) -> AppResult<String> {
        let cancellation = CancellationToken::new();
        self.preflights
            .lock()
            .unwrap()
            .insert(peer_id.clone(), cancellation.clone());
        let result = self
            .send_with_preflight(
                peer_id.clone(),
                paths,
                verify_hash,
                allow_public,
                cancellation,
            )
            .await;
        self.preflights.lock().unwrap().remove(&peer_id);
        result
    }

    pub fn cancel_preflight(&self, peer_id: &str) {
        if let Some(cancellation) = self.preflights.lock().unwrap().get(peer_id) {
            cancellation.cancel();
        }
    }

    async fn send_with_preflight(
        &self,
        peer_id: String,
        paths: Vec<String>,
        verify_hash: bool,
        allow_public: bool,
        cancellation: CancellationToken,
    ) -> AppResult<String> {
        let peer = self.peers.get(&peer_id)?;
        let public_endpoint = peer.endpoints.iter().find(|ep| {
            ep.socket(false)
                .is_ok_and(|addr| !transport::is_local(addr.ip()))
        });
        // Discovery freshness is only a UI hint. Verify the stable Node ID on
        // a live connection before creating any task, and refresh metadata.
        let mut reachable = false;
        let mut last_error = String::from("没有可用地址");
        for endpoint in &peer.endpoints {
            if !allow_public
                && !endpoint
                    .socket(false)
                    .is_ok_and(|addr| transport::is_local(addr.ip()))
            {
                continue;
            }
            let result = tokio::select! {
                _ = cancellation.cancelled() => return Err(AppError::Other("已取消发送前连接检测。".into())),
                result = async {
                    let mut channel = transport::connect(&self.app, endpoint, Some(&peer_id), false, allow_public).await?;
                    let hello = transport::hello(&mut channel, &self.app).await?;
                    Ok::<_, AppError>((hello, channel))
                } => result,
            };
            match result {
                Ok(((name, tcp_port, quic_port), channel)) => {
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
                    let connected = Endpoint { tcp_port, quic_port, ..endpoint.clone() };
                    tokio::spawn(async move {
                        if let Err(error) = refresh_send_addresses(app, peers, peer_id, connected, channel, allow_public).await {
                            tracing::debug!(%error, "后台地址更新未完成，保留已有地址");
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
                        "公网路径需要显式允许：{}", endpoint.address
                    )));
                }
            }
            tracing::debug!(%peer_id, %last_error, "发送前连接检测失败");
            return Err(AppError::Other(
                "暂时无法连接这台设备。请确认对方已打开 NodeSend，并且两台设备连接到同一网络。"
                    .into(),
            ));
        }
        let peer = self.peers.get(&peer_id)?;
        let id = self
            .transfers
            .create_outgoing(peer_id, peer.display_name, paths, verify_hash, allow_public)
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
        if accepted
            && destination.as_deref() == Some(self.transfers.default_destination().as_str())
            && !std::path::Path::new(destination.as_ref().unwrap()).exists()
        {
            std::fs::create_dir_all(destination.as_ref().unwrap())?;
        }
        self.transfers
            .decide(&id, accepted, destination, conflict, selected)
            .await
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
            if task.direction != "receive" || task.completed.len() != task.manifest.chunks().len() {
                return Err(AppError::BadRequest("文件尚未接收完整".into()));
            }
            if task.status == "completed" {
                return crate::android_import::export_received(task).await.map_err(AppError::Other);
            }
            self.transfers.commit(&id).await?;
            return Ok(());
        }
        self.transfers.action(&id, &action).await?;
        if ["resume", "retry"].contains(&action.as_str())
            && self.transfers.record(&id)?.direction == "send"
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
        } else if action == "cancel" && self.transfers.record(&id)?.direction == "send" {
            let task = self.transfers.record(&id)?;
            let app = self.app.clone();
            let peer = self.peers.get(&task.peer_id)?;
            tokio::spawn(async move {
                for ep in peer.endpoints {
                    if let Ok(mut channel) =
                        transport::connect(&app, &ep, Some(&task.peer_id), false, task.allow_public)
                            .await
                    {
                        if transport::hello(&mut channel, &app).await.is_ok() {
                            let _ = channel
                                .request(&protocol::Request::Cancel { id: task.wire_id })
                                .await;
                        }
                        break;
                    }
                }
            });
        }
        Ok(())
    }
    pub fn settings(&self, name: String, directory: String, policy: String) -> AppResult<()> {
        if name.trim().is_empty()
            || name.len() > 128
            || !["ask", "trusted", "reject"].contains(&policy.as_str())
        {
            return Err(AppError::BadRequest("本机名称或接收策略非法".into()));
        }
        if directory == self.transfers.default_destination()
            && !std::path::Path::new(&directory).exists()
        {
            std::fs::create_dir_all(&directory)?;
        }
        crate::transfer::paths::destination(&directory)?;
        self.app.set_setting("display_name", name.trim())?;
        self.app.set_setting("receive_directory", &directory)?;
        self.app.set_setting("receive_policy", &policy)?;
        Ok(())
    }
    pub async fn shutdown(&self) {
        if let Some(d) = &self.discovery {
            d.stop();
        }
        self.transfers.shutdown.cancel();
        let handle = self.server.lock().unwrap().take();
        if let Some(h) = handle {
            let _ = h.await;
        }
    }
}

async fn refresh_send_addresses(
    app: Arc<AppService>, peers: Arc<PeerStore>, peer_id: String,
    mut connected: Endpoint, mut channel: transport::Channel, allow_public: bool,
) -> AppResult<()> {
    let protocol::Response::Addresses { addresses, tcp_port, quic_port } = channel.request(&protocol::Request::Addresses).await? else {
        return Ok(());
    };
    if addresses.len() > 128 || tcp_port == 0 { return Err(AppError::BadRequest("地址列表非法".into())); }
    drop(channel);
    connected.last_seen = chrono::Utc::now().timestamp();
    connected.error = None;
    let mut verified = vec![connected.clone()];
    let mut probes = tokio::task::JoinSet::new();
    let slots = Arc::new(tokio::sync::Semaphore::new(4));
    let addresses = addresses.into_iter().collect::<std::collections::BTreeSet<_>>();
    for address in addresses {
        let Ok(ip) = address.parse::<std::net::IpAddr>() else { continue; };
        if ip.is_loopback() || ip.is_unspecified() || ip.is_multicast() { continue; }
        let address = match ip {
            std::net::IpAddr::V6(v) if (v.segments()[0] & 0xffc0) == 0xfe80 => {
                let Some((_, scope)) = connected.address.split_once('%') else { continue; };
                format!("{v}%{scope}")
            }
            _ => peers::address(ip),
        };
        if address == connected.address && tcp_port == connected.tcp_port { continue; }
        let mut endpoint = Endpoint { address, tcp_port, quic_port, source: "send-refresh".into(), last_seen: chrono::Utc::now().timestamp(), error: None };
        if !allow_public && !transport::is_local(ip) {
            // Retain the peer's current IPv6 candidates without silently granting
            // permission to probe public networks during a private transfer.
            endpoint.error = Some("对方公布的地址，尚未验证连通性".into());
            verified.push(endpoint);
            continue;
        }
        let app = app.clone();
        let peer_id = peer_id.clone();
        let slots = slots.clone();
        probes.spawn(async move {
            let _permit = slots.acquire_owned().await.ok()?;
            tokio::time::timeout(std::time::Duration::from_secs(6), async {
                let mut channel = transport::connect(&app, &endpoint, Some(&peer_id), true, allow_public).await.ok()?;
                transport::hello(&mut channel, &app).await.ok()?;
                Some(endpoint)
            }).await.ok().flatten()
        });
    }
    while let Some(result) = probes.join_next().await {
        if let Ok(Some(endpoint)) = result { verified.push(endpoint); }
    }
    peers.refresh_addresses(&peer_id, verified)
}

async fn resolve_manual_endpoints(node_id: &str, input: &str) -> AppResult<Vec<Endpoint>> {
    let addresses = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        tokio::net::lookup_host(input),
    )
    .await
    .map_err(|_| AppError::Other("地址解析超时".into()))?;
    if let Ok(addresses) = addresses {
        return Ok(addresses
            .map(|addr| Endpoint {
                address: endpoint_address(addr),
                tcp_port: addr.port(),
                quic_port: addr.port(),
                source: "manual".into(),
                last_seen: chrono::Utc::now().timestamp(),
                error: None,
            })
            .collect());
    }

    // No explicit port: ask the remote NodeSend discovery listener for its
    // advertised TCP and QUIC ports instead of guessing our own port.
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

    // UDP discovery is intentionally best-effort: firewalls and public
    // networks commonly drop the probe even when the native TCP listener is
    // reachable. The desktop/mobile service uses a stable TCP bind port, so
    // retain the user's host and try that port without making it part of the
    // address they have to type. QUIC remains disabled for this fallback;
    // a successful TCP hello returns the peer's advertised QUIC port below.
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
    .map_err(|_| AppError::Other("地址解析超时".into()))??;
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
        return Err(AppError::Other(
            "未发现 NodeSend 服务。请确认对方已启动并连接同一网络".into(),
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
