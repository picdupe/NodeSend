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
        tokio::time::timeout(Duration::from_secs(2), async {
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
    time::Duration,
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
        if matches!(r, Request::Commit { .. }) {
            // SAF providers may copy a large staged file before acknowledging.
            return match protocol::read_frame_with_timeout(&mut *self.reader, Duration::from_secs(1800)).await? {
                Response::Error { message } => Err(AppError::Other(message)),
                response => Ok(response),
            };
        }
        self.response().await
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
    config.keep_alive_interval(Some(Duration::from_secs(5)));
    config.max_idle_timeout(Some(Duration::from_secs(30).try_into().unwrap()));
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
        return Err(AppError::Forbidden(format!("公网路径需要显式允许：{}", endpoint.address)));
    }
    let mut quic_error = None;
    if !prefer_tcp && endpoint.quic_port != 0 {
        match tokio::time::timeout(
            Duration::from_secs(2),
            connect_quic(app, endpoint, expected),
        )
        .await
        {
            Ok(Ok(channel)) => return Ok(channel),
            Ok(Err(err)) => quic_error = Some(err.to_string()),
            Err(_) => quic_error = Some("QUIC 握手超时".into()),
        }
    }
    let stream = tokio::time::timeout(Duration::from_secs(4), TcpStream::connect(address))
        .await
        .map_err(|_| other("连接设备超时，请检查对方是否在线及网络是否可达"))?
        .map_err(|err| match err.kind() {
            std::io::ErrorKind::ConnectionRefused => other(
                "无法连接设备：对方可能离线、未启动 NodeSend，或此端口未开放。请确认对方状态后重试",
            ),
            _ => other(format!("网络连接失败：{err}")),
        })?;
    stream.set_nodelay(true)?;
    let connector =
        tokio_rustls::TlsConnector::from(Arc::new(tls::client(app, expected.map(str::to_owned))?));
    let secured = tokio::time::timeout(
        Duration::from_secs(4),
        connector.connect(
            rustls::pki_types::ServerName::try_from("nodesend.local").unwrap(),
            stream,
        ),
    )
    .await
    .map_err(|_| other("TCP TLS 握手超时"))?
    .map_err(|e| {
        other(format!(
            "TCP TLS: {e}; QUIC: {}",
            quic_error.unwrap_or_default()
        ))
    })?;
    let peer_id = tls::remember(app, secured.get_ref().1.peer_certificates().unwrap_or(&[]))?;
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
pub async fn hello(channel: &mut Channel, app: &AppService) -> AppResult<(String, u16, u16)> {
    match channel
        .request(&Request::Hello {
            version: protocol::VERSION,
            node_id: app.identity.node_id.clone(),
            display_name: display_name(app),
        })
        .await?
    {
        Response::Hello {
            version,
            node_id,
            display_name,
            tcp_port,
            quic_port,
        } if version == protocol::VERSION && node_id == channel.peer_id => {
            Ok((display_name, tcp_port, quic_port))
        }
        _ => Err(AppError::Forbidden("Node 身份或协议版本不匹配".into())),
    }
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
    let slots = Arc::new(Semaphore::new(64));
    let mut children = tokio::task::JoinSet::new();
    let tcp_port = listeners.tcp_port;
    let quic_port = listeners.quic_port;
    loop {
        tokio::select! {
            _=transfers.shutdown.cancelled()=>break,
            Some(_)=children.join_next(),if !children.is_empty()=>{},
            result=listeners.tcp.accept()=>{
                let Ok((stream,remote))=result else{break;};let Ok(permit)=slots.clone().try_acquire_owned() else{continue;};
                let tls=acceptor.clone();let transfers=transfers.clone();let peers=peers.clone();
                children.spawn(async move {let _permit=permit;
                    if let Ok(Ok(stream))=tokio::time::timeout(Duration::from_secs(5),tls.accept(stream)).await {
                        if let Ok(peer_id)=tls::remember(&transfers.app,stream.get_ref().1.peer_certificates().unwrap_or(&[])) {
                            let (reader,writer)=tokio::io::split(stream);let mut channel=Channel{reader:Box::new(reader),writer:Box::new(writer),peer_id,kind:"tcp",_endpoint:None,_connection:None};
                            finish_session(&mut channel,&transfers,&peers,remote,tcp_port,quic_port).await;
                        }
                    }
                });
            },
            Some(incoming)=listeners.quic.accept()=>{
                let Ok(permit)=slots.clone().try_acquire_owned() else{incoming.refuse();continue;};let transfers=transfers.clone();let peers=peers.clone();
                children.spawn(async move {let _permit=permit;
                    if let Ok(Ok(connection))=tokio::time::timeout(Duration::from_secs(5),incoming).await {
                        let identity=connection.peer_identity().and_then(|v|v.downcast::<Vec<rustls::pki_types::CertificateDer<'static>>>().ok());
                        if let Some(identity)=identity {if let Ok(peer_id)=tls::remember(&transfers.app,&identity) {
                            if let Ok(Ok((writer,reader)))=tokio::time::timeout(Duration::from_secs(5),connection.accept_bi()).await {
                                let remote=connection.remote_address();let mut channel=Channel{reader:Box::new(reader),writer:Box::new(writer),peer_id,kind:"quic",_endpoint:None,_connection:Some(connection)};
                                finish_session(&mut channel,&transfers,&peers,remote,tcp_port,quic_port).await;
                            }
                        }}
                    }
                });
            }
        }
    }
    children.abort_all();
    while children.join_next().await.is_some() {}
    listeners.quic.close(0u32.into(), b"shutdown");
}
async fn finish_session(
    channel: &mut Channel,
    transfers: &TransferService,
    peers: &PeerStore,
    remote: SocketAddr,
    tcp_port: u16,
    quic_port: u16,
) {
    if let Err(error) = session(channel, transfers, peers, remote, tcp_port, quic_port).await {
        tracing::debug!(peer_id = %channel.peer_id, transport = channel.kind, %remote, %error, "原生会话结束");
    }
    // Send TLS close_notify (or finish the QUIC stream) on our own exit.
    // A closed stream still does not substitute for a durable commit ACK.
    let _ = tokio::time::timeout(Duration::from_secs(2), channel.writer.shutdown()).await;
}

async fn session(
    channel: &mut Channel,
    transfers: &TransferService,
    _peers: &PeerStore,
    _remote: SocketAddr,
    tcp_port: u16,
    quic_port: u16,
) -> AppResult<()> {
    let request: Request = protocol::read_frame(&mut *channel.reader).await?;
    let name = match request {
        Request::Hello {
            version,
            node_id,
            display_name,
        } if version == protocol::VERSION
            && node_id == channel.peer_id
            && display_name.len() <= 128 =>
        {
            display_name
        }
        _ => return Err(AppError::Forbidden("无效 HELLO".into())),
    };
    protocol::write_frame(
        &mut *channel.writer,
        &Response::Hello {
            version: protocol::VERSION,
            node_id: transfers.app.identity.node_id.clone(),
            display_name: display_name(&transfers.app),
            tcp_port,
            quic_port,
        },
    )
    .await?;
    loop {
        let request: Request = protocol::read_frame(&mut *channel.reader).await?;
        match &request {
            Request::Addresses => {
                let addresses = if_addrs::get_if_addrs()?.into_iter()
                    .filter(|i| !i.is_loopback() && !i.ip().is_unspecified())
                    .map(|i| i.ip().to_string())
                    .collect::<std::collections::BTreeSet<_>>().into_iter().collect();
                protocol::write_frame(&mut *channel.writer, &Response::Addresses {
                    addresses, tcp_port, quic_port,
                }).await?;
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
        let result = match request {
            Request::Chunk {
                id,
                index,
                length,
                sha256,
            } => {
                if length > 4194304 {
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
                        &data,
                        sha256,
                        channel.kind,
                    )
                    .await
            }
            other => {
                transfers
                    .control(&channel.peer_id, &name, other, channel.kind)
                    .await
            }
        };
        let response = match result {
            Ok(r) => r,
            Err(e) => Response::Error {
                message: e.to_string(),
            },
        };
        protocol::write_frame(&mut *channel.writer, &response).await?;
    }
}
