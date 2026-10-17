//! Real QUIC streams and TLS-over-TCP share the same framed protocol.
#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn chunk_drains_buffer_before_waiting_for_ack() {
        let (client, mut server) = tokio::io::duplex(128);
        let (reader, writer) = tokio::io::split(client);
        let mut channel = Channel {
            reader: Box::new(reader),
            writer: Box::new(tokio::io::BufWriter::with_capacity(4096, writer)),
            kind: "tcp",
            peer_id: "test".into(),
            _endpoint: None,
            _connection: None,
        };
        let payload = vec![42; 1000];
        let receive = async {
            let request: Request = protocol::read_frame(&mut server).await.unwrap();
            let Request::Chunk { length, .. } = request else {
                panic!("expected chunk")
            };
            let mut bytes = vec![0; length];
            server.read_exact(&mut bytes).await.unwrap();
            assert_eq!(bytes, payload);
            protocol::write_frame(&mut server, &Response::Ack)
                .await
                .unwrap();
        };
        let send = async {
            let response = channel
                .chunk(
                    &Request::Chunk {
                        id: "test".into(),
                        index: 0,
                        length: payload.len(),
                        sha256: None,
                    },
                    &payload,
                )
                .await
                .unwrap();
            assert!(matches!(response, Response::Ack));
        };
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            tokio::join!(receive, send);
        })
        .await
        .expect("buffered payload must reach receiver before awaiting ACK");
    }
}

use super::{
    browser,
    peers::{Endpoint, PeerStore},
    protocol::{self, Request, Response},
    tls,
};
use crate::{
    error::{AppError, AppResult},
    service::AppService,
    transfer::TransferService,
};
use std::{
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    sync::Arc,
};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::Semaphore,
};

pub struct Channel {
    pub reader: Box<dyn AsyncRead + Unpin + Send>,
    pub writer: Box<dyn AsyncWrite + Unpin + Send>,
    pub kind: &'static str,
    pub peer_id: String,
    _endpoint: Option<quinn::Endpoint>,
    _connection: Option<quinn::Connection>,
}
impl Channel {
    pub async fn request(&mut self, r: &Request) -> AppResult<Response> {
        protocol::write_frame(&mut *self.writer, r).await?;
        let timeout = match r {
            // SAF providers may copy a large staged file before acknowledging.
            Request::Commit { .. } => crate::tuning::COMMIT_ACK_TIMEOUT,
            // 接收端按条处理清单，慢盘上可能远超默认 IO 超时。
            Request::Offer { .. }
            | Request::ManifestChunk { .. }
            | Request::ManifestDone { .. } => protocol::MANIFEST_TIMEOUT,
            _ => protocol::IO_TIMEOUT,
        };
        match protocol::read_frame_with_timeout(&mut *self.reader, timeout).await? {
            Response::Error { message } => Err(AppError::Other(message)),
            response => Ok(response),
        }
    }
    pub async fn response(&mut self) -> AppResult<Response> {
        match protocol::read_frame(&mut *self.reader).await? {
            Response::Error { message } => Err(AppError::Other(message)),
            other => Ok(other),
        }
    }
    pub async fn chunk(&mut self, r: &Request, data: &[u8]) -> AppResult<Response> {
        protocol::write_frame(&mut *self.writer, r).await?;
        tokio::time::timeout(protocol::IO_TIMEOUT, async {
            self.writer.write_all(data).await?;
            // TLS may accept plaintext while encrypted bytes remain buffered
            // under backpressure. Drain them before waiting for the chunk ACK.
            self.writer.flush().await
        })
        .await
        .map_err(|_| AppError::Other("Chunk 写入超时".into()))??;
        self.response().await
    }
}
pub struct Listeners {
    pub tcp: TcpListener,
    pub quic: quinn::Endpoint,
    pub tcp_port: u16,
    pub quic_port: u16,
}
pub fn bind(app: &AppService, addr: SocketAddr) -> AppResult<Listeners> {
    {
        use socket2::{Domain, Protocol, Socket, Type};
        let tcp = Socket::new(
            if addr.is_ipv6() {
                Domain::IPV6
            } else {
                Domain::IPV4
            },
            Type::STREAM,
            Some(Protocol::TCP),
        )?;
        if addr.is_ipv6() {
            tcp.set_only_v6(false)?;
        }
        tcp.set_nonblocking(true)?;
        tcp.bind(&addr.into())?;
        tcp.listen(128)?;
        let std_tcp: std::net::TcpListener = tcp.into();
        let tcp_port = std_tcp.local_addr()?.port();
        let tcp = TcpListener::from_std(std_tcp)?;
        let udp = Socket::new(
            if addr.is_ipv6() {
                Domain::IPV6
            } else {
                Domain::IPV4
            },
            Type::DGRAM,
            Some(Protocol::UDP),
        )?;
        if addr.is_ipv6() {
            udp.set_only_v6(false)?;
        }
        udp.set_nonblocking(true)?;
        udp.bind(&SocketAddr::new(addr.ip(), 0).into())?;
        let (endpoint, quic_port) = make_quic_endpoint(app, udp.into())?;
        Ok(Listeners {
            tcp,
            quic: endpoint,
            tcp_port,
            quic_port,
        })
    }
}
fn make_quic_endpoint(
    app: &AppService,
    udp: std::net::UdpSocket,
) -> AppResult<(quinn::Endpoint, u16)> {
    let crypto =
        quinn::crypto::rustls::QuicServerConfig::try_from(tls::server(app)?).map_err(other)?;
    let mut config = quinn::ServerConfig::with_crypto(Arc::new(crypto));
    config.transport_config(quic_options());
    let endpoint = quinn::Endpoint::new(
        quinn::EndpointConfig::default(),
        Some(config),
        udp,
        Arc::new(quinn::TokioRuntime),
    )?;
    let port = endpoint.local_addr()?.port();
    Ok((endpoint, port))
}
fn quic_options() -> Arc<quinn::TransportConfig> {
    let mut config = quinn::TransportConfig::default();
    config.keep_alive_interval(Some(crate::tuning::QUIC_KEEPALIVE));
    config.max_idle_timeout(Some(crate::tuning::QUIC_IDLE_TIMEOUT.try_into().unwrap()));
    config.max_concurrent_bidi_streams(8u32.into());
    config.max_concurrent_uni_streams(0u32.into());
    Arc::new(config)
}
fn other(e: impl std::fmt::Display) -> AppError {
    AppError::Other(e.to_string())
}
pub async fn connect(
    app: &AppService,
    endpoint: &Endpoint,
    expected: Option<&str>,
    prefer_tcp: bool,
    allow_public: bool,
) -> AppResult<Channel> {
    let address = endpoint.socket(false)?;
    if !allow_public && !is_local(address.ip()) {
        return Err(AppError::Forbidden(format!(
            "公网路径需要显式允许：{}",
            endpoint.address
        )));
    }
    let mut quic_error = None;
    if !prefer_tcp && endpoint.quic_port != 0 {
        match tokio::time::timeout(
            crate::tuning::QUIC_HANDSHAKE_TIMEOUT,
            connect_quic(app, endpoint, expected),
        )
        .await
        {
            Ok(Ok(channel)) => {
                tracing::info!(remote = %endpoint.address, quic = endpoint.quic_port, peer_id = %channel.peer_id, "QUIC 原生连接已建立");
                return Ok(channel);
            }
            Ok(Err(err)) => {
                tracing::debug!(remote = %endpoint.address, quic = endpoint.quic_port, %err, "QUIC 连接失败，回退 TCP");
                quic_error = Some(err.to_string());
            }
            Err(_) => {
                tracing::debug!(remote = %endpoint.address, quic = endpoint.quic_port, "QUIC 握手超时，回退 TCP");
                quic_error = Some("QUIC 握手超时".into());
            }
        }
    }
    let stream = match tokio::time::timeout(
        crate::tuning::TCP_CONNECT_TIMEOUT,
        TcpStream::connect(address),
    )
    .await
    {
        Ok(Ok(stream)) => stream,
        Ok(Err(err)) => {
            let reason = match err.kind() {
                std::io::ErrorKind::ConnectionRefused => {
                    "无法连接设备：对方可能离线、未启动 NodeSend，或此端口未开放。请确认对方状态后重试".to_string()
                }
                _ => format!("网络连接失败：{err}"),
            };
            tracing::warn!(%address, error = %reason, "TCP 连接失败");
            return Err(other(reason));
        }
        Err(_) => {
            tracing::warn!(%address, "TCP 连接超时（对方在线但端口无响应）");
            return Err(other("连接设备超时，请检查对方是否在线及网络是否可达"));
        }
    };
    stream.set_nodelay(true)?;
    tracing::debug!(%address, "TCP 已连接，开始 TLS 握手");
    let connector =
        tokio_rustls::TlsConnector::from(Arc::new(tls::client(app, expected.map(str::to_owned))?));
    let secured = match tokio::time::timeout(
        crate::tuning::TLS_HANDSHAKE_TIMEOUT,
        connector.connect(
            rustls::pki_types::ServerName::try_from("nodesend.local").unwrap(),
            stream,
        ),
    )
    .await
    {
        Ok(Ok(secured)) => secured,
        Ok(Err(error)) => {
            let quic = quic_error.unwrap_or_else(|| "未尝试".into());
            tracing::warn!(%address, %error, quic = %quic, "TLS 握手失败");
            return Err(other(format!("TCP TLS: {error}; QUIC: {quic}")));
        }
        Err(_) => {
            let quic = quic_error.unwrap_or_else(|| "未尝试".into());
            tracing::warn!(%address, quic = %quic, "TLS 握手超时：端口接受了连接但没有完成 NodeSend 握手");
            return Err(other(format!(
                "TCP TLS 握手超时：对方端口接受了连接，但没有完成 NodeSend 握手。常见原因：对方 NodeSend 未正常运行或进程卡住、端口被其他程序占用，或防火墙/安全软件只放行了握手、拦截了数据。QUIC: {quic}"
            )));
        }
    };
    let peer_id = tls::remember(app, secured.get_ref().1.peer_certificates().unwrap_or(&[]))?;
    tracing::info!(%address, peer_id = %peer_id, "TCP 原生连接已建立");
    let (reader, writer) = tokio::io::split(secured);
    Ok(Channel {
        reader: Box::new(reader),
        writer: Box::new(writer),
        kind: "tcp",
        peer_id,
        _endpoint: None,
        _connection: None,
    })
}
async fn connect_quic(
    app: &AppService,
    remote: &Endpoint,
    expected: Option<&str>,
) -> AppResult<Channel> {
    let addr = remote.socket(true)?;
    let mut endpoint = quinn::Endpoint::client(SocketAddr::new(
        if addr.is_ipv6() {
            IpAddr::V6(Ipv6Addr::UNSPECIFIED)
        } else {
            IpAddr::V4(Ipv4Addr::UNSPECIFIED)
        },
        0,
    ))?;
    let crypto = quinn::crypto::rustls::QuicClientConfig::try_from(tls::client(
        app,
        expected.map(str::to_owned),
    )?)
    .map_err(other)?;
    let mut config = quinn::ClientConfig::new(Arc::new(crypto));
    config.transport_config(quic_options());
    endpoint.set_default_client_config(config);
    let connection = endpoint
        .connect(addr, "nodesend.local")
        .map_err(other)?
        .await
        .map_err(other)?;
    let identity = connection
        .peer_identity()
        .ok_or_else(|| other("缺少对端证书"))?
        .downcast::<Vec<rustls::pki_types::CertificateDer<'static>>>()
        .map_err(|_| other("对端证书格式非法"))?;
    let peer_id = tls::remember(app, &identity)?;
    let (writer, reader) = connection.open_bi().await.map_err(other)?;
    Ok(Channel {
        reader: Box::new(reader),
        writer: Box::new(writer),
        kind: "quic",
        peer_id,
        _endpoint: Some(endpoint),
        _connection: Some(connection),
    })
}
pub async fn hello(channel: &mut Channel, app: &AppService) -> AppResult<(String, String, u16, u16)> {
    match channel
        .request(&Request::Hello {
            // Match the protocol version used by the committed GitHub build.
            version: protocol::VERSION,
            node_id: app.identity.node_id.clone(),
            display_name: display_name(app),
            device_type: local_device_type().into(),
        })
        .await?
    {
        Response::Hello {
            version,
            node_id,
            display_name,
            device_type,
            tcp_port,
            quic_port,
        } if version == protocol::VERSION && node_id == channel.peer_id => {
            Ok((display_name, device_type, tcp_port, quic_port))
        }
        _ => Err(AppError::Forbidden("Node 身份或协议版本不匹配".into())),
    }
}

fn local_device_type() -> &'static str {
    #[cfg(target_os = "android")]
    { "android" }
    #[cfg(target_os = "ios")]
    { "ios" }
    #[cfg(target_os = "windows")]
    { "windows" }
    #[cfg(target_os = "macos")]
    { "macos" }
    #[cfg(target_os = "linux")]
    { "linux" }
    #[cfg(not(any(target_os = "android", target_os = "ios", target_os = "windows", target_os = "macos", target_os = "linux")))]
    { "unknown" }
}
#[cfg(target_os = "windows")]
fn windows_device_name() -> Option<String> {
    #[link(name = "kernel32")]
    extern "system" {
        fn GetComputerNameExW(kind: i32, buffer: *mut u16, size: *mut u32) -> i32;
    }
    // ComputerNameDnsHostname preserves case, unlike the NetBIOS name in
    // COMPUTERNAME. Use the active name, not a pending rename from the registry.
    let mut buffer = [0u16; 256];
    let mut size = buffer.len() as u32;
    // SAFETY: buffer is writable for size UTF-16 code units; size is valid.
    let success = unsafe { GetComputerNameExW(1, buffer.as_mut_ptr(), &mut size) };
    if success == 0 || size as usize > buffer.len() {
        return None;
    }
    String::from_utf16(&buffer[..size as usize])
        .ok()
        .filter(|name| !name.trim().is_empty())
}

pub fn display_name(app: &AppService) -> String {
    app.get_setting("display_name")
        .ok()
        .flatten()
        .filter(|name| !name.trim().is_empty())
        .unwrap_or_else(|| {
            #[cfg(target_os = "windows")]
            if let Some(name) = windows_device_name() {
                return name;
            }
            #[cfg(target_os = "android")]
            {
                return std::env::var("NODESEND_ANDROID_DEVICE_NAME")
                    .ok()
                    .filter(|name| !name.trim().is_empty())
                    .unwrap_or_else(|| "Android".into());
            }
            #[cfg(not(target_os = "android"))]
            std::env::var("COMPUTERNAME")
                .or_else(|_| std::env::var("HOSTNAME"))
                .unwrap_or_else(|_| "NodeSend".into())
        })
}
pub fn is_local(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v) => v.is_private() || v.is_loopback() || v.is_link_local(),
        IpAddr::V6(v) => {
            if let Some(v) = v.to_ipv4_mapped() {
                is_local(IpAddr::V4(v))
            } else {
                v.is_loopback()
                    || (v.segments()[0] & 0xfe00) == 0xfc00
                    || (v.segments()[0] & 0xffc0) == 0xfe80
            }
        }
    }
}
pub async fn serve(listeners: Listeners, transfers: Arc<TransferService>, peers: Arc<PeerStore>) {
    let acceptor = match tls::server(&transfers.app) {
        Ok(c) => tokio_rustls::TlsAcceptor::from(Arc::new(c)),
        Err(e) => {
            tracing::error!(%e,"TLS 初始化失败");
            return;
        }
    };
    let slots = Arc::new(Semaphore::new(crate::tuning::MAX_SESSIONS));
    let mut children = tokio::task::JoinSet::new();
    let ports = (listeners.tcp_port, listeners.quic_port);
    loop {
        tokio::select! {
            _ = transfers.shutdown.cancelled() => break,
            Some(_) = children.join_next(), if !children.is_empty() => {}
            result = listeners.tcp.accept() => {
                let Ok((stream, remote)) = result else { break };
                match acquire_slot(&slots, SessionKind::Tcp, remote) {
                    Some(permit) => {
                        let state = SessionState {
                            acceptor: acceptor.clone(),
                            transfers: transfers.clone(),
                            peers: peers.clone(),
                            ports,
                        };
                        children.spawn(async move {
                            let _permit = permit;
                            serve_tcp_session(state, stream, remote).await;
                        });
                    }
                    None => drop(stream),
                }
            }
            Some(incoming) = listeners.quic.accept() => {
                let remote = incoming.remote_address();
                match acquire_slot(&slots, SessionKind::Quic, remote) {
                    Some(permit) => {
                        let state = SessionState {
                            acceptor: acceptor.clone(),
                            transfers: transfers.clone(),
                            peers: peers.clone(),
                            ports,
                        };
                        children.spawn(async move {
                            let _permit = permit;
                            serve_quic_session(state, incoming, remote).await;
                        });
                    }
                    None => incoming.refuse(),
                }
            }
        }
    }
    children.abort_all();
    while children.join_next().await.is_some() {}
    listeners.quic.close(0u32.into(), b"shutdown");
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SessionKind {
    Tcp,
    Quic,
}

impl SessionKind {
    fn label(self) -> &'static str {
        match self {
            SessionKind::Tcp => "TCP",
            SessionKind::Quic => "QUIC",
        }
    }
}

/// 每个新会话所需的共享句柄。避免在 `select!` 分支里逐个 `clone()` 造成噪声。
struct SessionState {
    acceptor: tokio_rustls::TlsAcceptor,
    transfers: Arc<TransferService>,
    peers: Arc<PeerStore>,
    ports: (u16, u16),
}

/// 取一个会话许可。达到上限时记录日志并拒绝（静默丢弃会让对端只看到
/// "连不上、需要重启本机"，必须留下可诊断的日志）。
fn acquire_slot(
    slots: &Arc<Semaphore>,
    kind: SessionKind,
    remote: SocketAddr,
) -> Option<tokio::sync::OwnedSemaphorePermit> {
    match slots.clone().try_acquire_owned() {
        Ok(permit) => Some(permit),
        Err(_) => {
            tracing::warn!(
                %remote,
                permits = slots.available_permits(),
                "连接数已达上限，拒绝新的{}会话",
                kind.label()
            );
            None
        }
    }
}

/// 完成对端证书登记（写库）并给出对端 Node ID。
///
/// 身份登记是同步数据库写入：任何长期占用数据库锁的线程都会让这里无限等待，
/// 进而让对端卡在"连接检测"、并且一直占着一个连接许可。因此放到阻塞线程池
/// 并加超时——宁可放弃这次会话，也不把自身拖死。
async fn register_identity(
    transfers: &Arc<TransferService>,
    certificates: Vec<rustls::pki_types::CertificateDer<'static>>,
    remote: SocketAddr,
    kind: SessionKind,
) -> Option<String> {
    let app = transfers.app.clone();
    let remembered = tokio::time::timeout(
        crate::tuning::IDENTITY_RECORD_TIMEOUT,
        tokio::task::spawn_blocking(move || tls::remember(&app, &certificates)),
    )
    .await;
    match remembered {
        Ok(Ok(Ok(peer_id))) => Some(peer_id),
        Ok(Ok(Err(error))) => {
            tracing::warn!(%remote, %error, "{} 会话身份登记失败", kind.label());
            None
        }
        Ok(Err(error)) => {
            tracing::warn!(%remote, %error, "{} 会话身份登记任务异常", kind.label());
            None
        }
        Err(_) => {
            tracing::warn!(
                %remote,
                "{} 会话身份登记超时（数据库锁可能被长期占用），已放弃本次会话",
                kind.label()
            );
            None
        }
    }
}

async fn serve_tcp_session(state: SessionState, stream: TcpStream, remote: SocketAddr) {
    let Ok(Ok(stream)) =
        tokio::time::timeout(crate::tuning::ACCEPT_TIMEOUT, state.acceptor.accept(stream)).await
    else {
        return;
    };
    let certificates = stream
        .get_ref()
        .1
        .peer_certificates()
        .unwrap_or(&[])
        .to_vec();
    let Some(peer_id) =
        register_identity(&state.transfers, certificates, remote, SessionKind::Tcp).await
    else {
        return;
    };
    let (reader, writer) = tokio::io::split(stream);
    let mut channel = Channel {
        reader: Box::new(reader),
        writer: Box::new(writer),
        peer_id,
        kind: "tcp",
        _endpoint: None,
        _connection: None,
    };
    finish_session(
        &mut channel,
        state.transfers.clone(),
        state.peers.clone(),
        remote,
        state.ports.0,
        state.ports.1,
    )
    .await;
}

async fn serve_quic_session(state: SessionState, incoming: quinn::Incoming, remote: SocketAddr) {
    let Ok(Ok(connection)) = tokio::time::timeout(crate::tuning::ACCEPT_TIMEOUT, incoming).await
    else {
        return;
    };
    let Some(identity) = connection.peer_identity().and_then(|value| {
        value
            .downcast::<Vec<rustls::pki_types::CertificateDer<'static>>>()
            .ok()
    }) else {
        return;
    };
    let Some(peer_id) =
        register_identity(&state.transfers, *identity, remote, SessionKind::Quic).await
    else {
        return;
    };
    let Ok(Ok((writer, reader))) =
        tokio::time::timeout(crate::tuning::ACCEPT_TIMEOUT, connection.accept_bi()).await
    else {
        return;
    };
    let mut channel = Channel {
        reader: Box::new(reader),
        writer: Box::new(writer),
        peer_id,
        kind: "quic",
        _endpoint: None,
        _connection: Some(connection),
    };
    finish_session(
        &mut channel,
        state.transfers.clone(),
        state.peers.clone(),
        remote,
        state.ports.0,
        state.ports.1,
    )
    .await;
}
async fn finish_session(
    channel: &mut Channel,
    transfers: std::sync::Arc<TransferService>,
    peers: std::sync::Arc<PeerStore>,
    remote: SocketAddr,
    tcp_port: u16,
    quic_port: u16,
) {
    if let Err(error) = session(channel, transfers, peers, remote, tcp_port, quic_port).await {
        tracing::debug!(peer_id = %channel.peer_id, transport = channel.kind, %remote, %error, "原生会话结束");
    }
    // Send TLS close_notify (or finish the QUIC stream) on our own exit.
    // A closed stream still does not substitute for a durable commit ACK.
    let _ = tokio::time::timeout(
        crate::tuning::SHUTDOWN_FLUSH_TIMEOUT,
        channel.writer.shutdown(),
    )
    .await;
}

async fn session(
    channel: &mut Channel,
    transfers: std::sync::Arc<TransferService>,
    peers: std::sync::Arc<PeerStore>,
    _remote: SocketAddr,
    tcp_port: u16,
    quic_port: u16,
) -> AppResult<()> {
    let request: Request = protocol::read_frame(&mut *channel.reader).await.map_err(|error| {
        tracing::warn!(
            peer_id = %channel.peer_id,
            transport = channel.kind,
            %error,
            "底层连接已建立，但读取首个 NodeSend HELLO 请求失败"
        );
        error
    })?;
    let (peer_name, peer_device_type) = match request {
        Request::Hello {
            version,
            node_id,
            display_name,
            device_type,
        } if version == protocol::VERSION
            && node_id == channel.peer_id
            && display_name.len() <= 128 =>
        {
            (display_name, device_type)
        }
        _ => return Err(AppError::Forbidden("无效 HELLO".into())),
    };
    protocol::write_frame(
        &mut *channel.writer,
        &Response::Hello {
            version: protocol::VERSION,
            node_id: transfers.app.identity.node_id.clone(),
            display_name: display_name(&transfers.app),
            device_type: local_device_type().into(),
            tcp_port,
            quic_port,
        },
    )
    .await?;
    loop {
        let request: Request = protocol::read_frame(&mut *channel.reader).await?;
        // 清单/提交阶段的帧必须留痕：发送端说"已发出"而接收端毫无记录时，
        // 这是唯一能区分"帧没到"和"处理失败"的证据。
        match &request {
            Request::ManifestChunk { first, entries, .. } => {
                tracing::info!(
                    peer = %channel.peer_id,
                    transport = channel.kind,
                    first,
                    entries = entries.len(),
                    "收到清单分片"
                );
            }
            Request::ManifestDone {
                file_count,
                total_size,
                ..
            } => {
                tracing::info!(
                    peer = %channel.peer_id,
                    transport = channel.kind,
                    file_count,
                    total_size,
                    "收到清单结束帧"
                );
            }
            Request::Offer { .. } => {
                tracing::info!(peer = %channel.peer_id, transport = channel.kind, "收到清单摘要（Offer）");
            }
            Request::Commit { .. } => {
                tracing::info!(peer = %channel.peer_id, transport = channel.kind, "收到提交请求");
            }
            _ => {}
        }
        match &request {
            Request::Addresses => {
                let addresses = if_addrs::get_if_addrs()?
                    .into_iter()
                    .filter(|i| !i.is_loopback() && !i.ip().is_unspecified())
                    .map(|i| i.ip().to_string())
                    .collect::<std::collections::BTreeSet<_>>()
                    .into_iter()
                    .collect();
                protocol::write_frame(
                    &mut *channel.writer,
                    &Response::Addresses {
                        addresses,
                        tcp_port,
                        quic_port,
                    },
                )
                .await?;
                continue;
            }
            Request::Browse {
                share_id,
                path,
                token,
            } => {
                let response = browser::browse(
                    &transfers.app,
                    &channel.peer_id,
                    share_id.clone(),
                    path.clone(),
                    token.clone(),
                )
                .unwrap_or_else(|e| Response::Error {
                    message: e.to_string(),
                });
                protocol::write_frame(&mut *channel.writer, &response).await?;
                continue;
            }
            Request::Fetch {
                share_id,
                path,
                token,
            } => {
                match browser::fetch(
                    &transfers.app,
                    &channel.peer_id,
                    share_id,
                    path,
                    token.clone(),
                )
                .await
                {
                    Ok((mut file, length)) => {
                        protocol::write_frame(&mut *channel.writer, &Response::File { length })
                            .await?;
                        let copied = tokio::io::copy(&mut file, &mut *channel.writer).await?;
                        if copied != length {
                            return Err(AppError::Other("源文件在下载过程中发生变化".into()));
                        }
                        channel.writer.flush().await?;
                    }
                    Err(e) => {
                        protocol::write_frame(
                            &mut *channel.writer,
                            &Response::Error {
                                message: e.to_string(),
                            },
                        )
                        .await?
                    }
                }
                continue;
            }
            Request::Put {
                share_id,
                path,
                length,
                token,
            } => {
                match browser::put_target(
                    &transfers.app,
                    &channel.peer_id,
                    share_id,
                    path,
                    token.clone(),
                )
                .await
                {
                    Ok((mut file, target)) => {
                        protocol::write_frame(&mut *channel.writer, &Response::Ready).await?;
                        let copied =
                            tokio::io::copy(&mut (&mut *channel.reader).take(*length), &mut file)
                                .await;
                        match copied {
                            Ok(n) if n == *length => {
                                file.flush().await?;
                                protocol::write_frame(&mut *channel.writer, &Response::Ack).await?;
                            }
                            _ => {
                                drop(file);
                                let _ = tokio::fs::remove_file(target).await;
                                return Err(AppError::Other("NodeSend 上传中断".into()));
                            }
                        }
                    }
                    Err(e) => {
                        protocol::write_frame(
                            &mut *channel.writer,
                            &Response::Error {
                                message: e.to_string(),
                            },
                        )
                        .await?
                    }
                }
                continue;
            }
            _ => {}
        }
        let request_label = request_label(&request);
        let restart_send = matches!(&request, Request::TaskState { status, .. } if status == "resumed");
        let request_id = match &request {
            Request::TaskState { id, .. } => transfers.task_id_for_peer_wire(&channel.peer_id, id).ok(),
            _ => None,
        };
        let result = match request {
            Request::Chunk {
                id,
                index,
                length,
                sha256,
            } => {
                if length as u64 > crate::transfer::CHUNK_SIZE {
                    return Err(AppError::BadRequest("Chunk 过大".into()));
                }
                let mut data = vec![0; length];
                tokio::time::timeout(protocol::IO_TIMEOUT, channel.reader.read_exact(&mut data))
                    .await
                    .map_err(|_| other("Chunk 读取超时"))??;
                transfers
                    .receive_chunk(
                        &crate::transfer::model::incoming_id(&channel.peer_id, &id),
                        index,
                        data,
                        sha256,
                        channel.kind,
                    )
                    .await
            }
            other => {
                transfers
                    .control(&channel.peer_id, &peer_name, &peer_device_type, other, channel.kind)
                    .await
            }
        };
        let response = match result {
            Ok(r) => r,
            Err(e) => {
                // 接收端必须记录"收到了什么请求、为什么失败"：否则对端只看到
                // 静默等待（清单分片不回执），无法区分"没收到"和"处理失败"。
                tracing::warn!(
                    peer = %channel.peer_id,
                    transport = channel.kind,
                    request = request_label,
                    %e,
                    "会话请求处理失败，已返回错误帧"
                );
                Response::Error {
                    message: e.to_string(),
                }
            }
        };
        protocol::write_frame(&mut *channel.writer, &response).await?;
        if restart_send {
            if let Some(id) = request_id {
                let service = transfers.clone();
                let peer_store = peers.clone();
                tokio::spawn(async move {
                    while service.is_active(&id) {
                        tokio::select! {
                            _ = service.shutdown.cancelled() => return,
                            _ = tokio::time::sleep(std::time::Duration::from_millis(100)) => {}
                        }
                    }
                    if service.record(&id).is_ok_and(|task| task.direction == "send" && task.status == "queued") {
                        service.start_task(peer_store, id);
                    }
                });
            }
        }
    }
}

/// 请求类型简称：用于日志（`Request` 的 Debug 对清单分片会打印上千条条目）。
fn request_label(request: &Request) -> &'static str {
    match request {
        Request::SkipFile { .. } => "SkipFile",
        Request::Addresses => "Addresses",
        Request::Hello { .. } => "Hello",
        Request::Offer { .. } => "Offer",
        Request::Reconnect { .. } => "Reconnect",
        Request::ManifestChunk { .. } => "ManifestChunk",
        Request::ManifestDone { .. } => "ManifestDone",
        Request::Poll { .. } => "Poll",
        Request::Chunk { .. } => "Chunk",
        Request::Commit { .. } => "Commit",
        Request::Cancel { .. } => "Cancel",
        Request::Pause { .. } => "Pause",
        Request::Resume { .. } => "Resume",
        Request::TaskState { .. } => "TaskState",
        Request::Browse { .. } => "Browse",
        Request::Fetch { .. } => "Fetch",
        Request::Put { .. } => "Put",
    }
}
