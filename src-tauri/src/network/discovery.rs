//! Independent mDNS and UDP discovery tasks; one failure never disables the other.
use super::{
    peers::{address, valid_id, Endpoint, PeerStore},
    protocol::VERSION,
};
use crate::error::{AppError, AppResult};
use mdns_sd::{ServiceDaemon, ServiceEvent, ServiceInfo};
use serde::{Deserialize, Serialize};
use std::{
    net::{IpAddr, Ipv4Addr, SocketAddr},
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::net::UdpSocket;
pub const SERVICE_TYPE: &str = "_nodesend._tcp.local.";
pub const UDP_PORT: u16 = 58081;
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Announcement {
    pub app: String,
    pub protocol_version: u16,
    pub node_id: String,
    pub display_name: String,
    pub port: u16,
    pub quic_port: u16,
    pub capabilities: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct DiscoveryProbe {
    app: String,
    kind: String,
    node_id: String,
}
impl DiscoveryProbe {
    fn valid(&self) -> bool {
        self.app == "nodesend" && self.kind == "probe" && valid_id(&self.node_id)
    }
}
impl Announcement {
    pub fn valid(&self) -> bool {
        self.app == "nodesend"
            && self.protocol_version == VERSION
            && valid_id(&self.node_id)
            && self.port != 0
            && self.quic_port != 0
            && self.display_name.len() <= 128
            && self.capabilities.len() <= 8
    }
}

/// Ask a NodeSend peer for its current TCP/QUIC ports. This is used when a
/// user enters only an IP/hostname; the service port is discovered over the
/// fixed UDP discovery channel and is never assumed to match our own port.
pub async fn probe(
    target: SocketAddr,
    node_id: &str,
) -> AppResult<Vec<(Announcement, SocketAddr)>> {
    let bind = if target.is_ipv6() {
        SocketAddr::from(([0u16; 8], 0))
    } else {
        SocketAddr::from(([0, 0, 0, 0], 0))
    };
    let socket = UdpSocket::bind(bind).await?;
    let packet = serde_json::to_vec(&DiscoveryProbe {
        app: "nodesend".into(),
        kind: "probe".into(),
        node_id: node_id.into(),
    })?;
    socket.send_to(&packet, target).await?;
    let mut buf = [0; 4096];
    let mut found = Vec::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        let Ok(Ok((n, source))) = tokio::time::timeout(remaining, socket.recv_from(&mut buf)).await
        else {
            break;
        };
        let Ok(announcement) = serde_json::from_slice::<Announcement>(&buf[..n]) else {
            continue;
        };
        if announcement.valid() && announcement.node_id != node_id {
            found.push((announcement, source));
            break;
        }
    }
    Ok(found)
}
#[derive(Debug, Default, Clone, Serialize)]
pub struct DiscoveryStatus {
    pub mdns: bool,
    pub udp: bool,
    pub warnings: Vec<String>,
}
pub struct Discovery {
    pub status: Arc<Mutex<DiscoveryStatus>>,
    stop: tokio_util::sync::CancellationToken,
}
impl Discovery {
    pub fn start(hello: Announcement, peers: Arc<PeerStore>) -> Self {
        let status = Arc::new(Mutex::new(DiscoveryStatus::default()));
        let stop = tokio_util::sync::CancellationToken::new();
        let (h, p, s, c) = (hello.clone(), peers.clone(), status.clone(), stop.clone());
        tokio::spawn(async move {
            if let Err(e) = mdns(h, p, s.clone(), c).await {
                let mut status = s.lock().unwrap();
                status.mdns = false;
                status.warnings.push(format!("mDNS: {e}"));
            }
        });
        let (s, c) = (status.clone(), stop.clone());
        tokio::spawn(async move {
            if let Err(e) = udp(hello, peers, s.clone(), c).await {
                let mut status = s.lock().unwrap();
                status.udp = false;
                status.warnings.push(format!("UDP: {e}"));
            }
        });
        Self { status, stop }
    }
    pub fn stop(&self) {
        self.stop.cancel();
    }
}
impl Drop for Discovery {
    fn drop(&mut self) {
        self.stop();
    }
}
async fn mdns(
    hello: Announcement,
    peers: Arc<PeerStore>,
    status: Arc<Mutex<DiscoveryStatus>>,
    stop: tokio_util::sync::CancellationToken,
) -> AppResult<()> {
    let daemon = ServiceDaemon::new().map_err(other)?;
    let props = std::collections::HashMap::from([
        ("node_id".into(), hello.node_id.clone()),
        ("name".into(), hello.display_name.clone()),
        ("version".into(), VERSION.to_string()),
        ("quic_port".into(), hello.quic_port.to_string()),
        ("capabilities".into(), "files,folders,chunks,tls13".into()),
    ]);
    let info = ServiceInfo::new(
        SERVICE_TYPE,
        &format!("ns-{}", &hello.node_id[..24]),
        &format!("ns-{}.local.", &hello.node_id[..32]),
        "",
        hello.port,
        props,
    )
    .map_err(other)?
    .enable_addr_auto();
    let fullname = info.get_fullname().to_string();
    daemon.register(info).map_err(other)?;
    let receiver = daemon.browse(SERVICE_TYPE).map_err(other)?;
    status.lock().unwrap().mdns = true;
    loop {
        tokio::select! {
            _=stop.cancelled()=>break,
            event=receiver.recv_async()=>match event {
                Ok(ServiceEvent::ServiceResolved(info))=>{
                    let props=info.get_properties();
                    if props.get("version").map(|v|v.val_str())!=Some("1"){continue;}
                    let Some(id)=props.get("node_id").map(|v|v.val_str()) else{continue;};if !valid_id(id)||id==hello.node_id{continue;}
                    let name=props.get("name").map(|v|v.val_str()).unwrap_or("NodeSend");
                    let quic=props.get("quic_port").and_then(|v|v.val_str().parse().ok()).unwrap_or(info.get_port());
                    let endpoints=info.get_addresses().iter().flat_map(|ip|scoped_addresses(*ip)).map(|address|Endpoint{address,tcp_port:info.get_port(),quic_port:quic,source:"mdns".into(),last_seen:chrono::Utc::now().timestamp(),error:None}).collect();
                    let _=peers.upsert(id.into(),name.into(),endpoints,false);
                },
                Err(_)=>break,_=>{}
            }
        }
    }
    let _ = daemon.unregister(&fullname);
    let _ = daemon.shutdown();
    status.lock().unwrap().mdns = false;
    Ok(())
}
fn scoped_addresses(ip: IpAddr) -> Vec<String> {
    if let IpAddr::V6(v) = ip {
        if (v.segments()[0] & 0xffc0) == 0xfe80 {
            return if_addrs::get_if_addrs()
                .unwrap_or_default()
                .iter()
                .filter_map(|i| i.index)
                .map(|index| format!("{v}%{index}"))
                .collect();
        }
    }
    vec![address(ip)]
}
async fn udp(
    hello: Announcement,
    peers: Arc<PeerStore>,
    status: Arc<Mutex<DiscoveryStatus>>,
    stop: tokio_util::sync::CancellationToken,
) -> AppResult<()> {
    #[cfg(target_os = "android")]
    let socket = {
        let raw = std::net::UdpSocket::bind(SocketAddr::from((Ipv4Addr::UNSPECIFIED, UDP_PORT)))?;
        raw.set_broadcast(true)?;
        raw.set_nonblocking(true)?;
        UdpSocket::from_std(raw)?
    };

    #[cfg(not(target_os = "android"))]
    let socket = {
        use socket2::{Domain, Protocol, Socket, Type};
        let raw = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
        raw.set_reuse_address(true)?;
        raw.set_broadcast(true)?;
        raw.set_nonblocking(true)?;
        raw.bind(&SocketAddr::from((Ipv4Addr::UNSPECIFIED, UDP_PORT)).into())?;
        UdpSocket::from_std(raw.into())?
    };
    let group = Ipv4Addr::new(239, 255, 58, 81);
    let _ = socket.join_multicast_v4(group, Ipv4Addr::UNSPECIFIED);
    socket.set_multicast_ttl_v4(1)?;
    status.lock().unwrap().udp = true;
    let data = serde_json::to_vec(&hello)?;
    let mut buf = [0; 4096];
    let mut tick = tokio::time::interval(Duration::from_secs(3));
    loop {
        tokio::select! {
            _=stop.cancelled()=>break,
            _=tick.tick()=>{
                let _=socket.send_to(&data,(Ipv4Addr::BROADCAST,UDP_PORT)).await;
                let _=socket.send_to(&data,(group,UDP_PORT)).await;

            },
            packet=socket.recv_from(&mut buf)=>{
                let (n, source) = packet?;
                if let Ok(probe) = serde_json::from_slice::<DiscoveryProbe>(&buf[..n]) {
                    if probe.valid() && probe.node_id != hello.node_id {
                        let _ = socket.send_to(&data, source).await;
                    }
                    continue;
                }
                if let Ok(item) = serde_json::from_slice::<Announcement>(&buf[..n]) {
                    if item.valid() && item.node_id != hello.node_id && super::transport::is_local(source.ip()) {
                        let _ = peers.upsert(
                            item.node_id,
                            item.display_name,
                            vec![Endpoint {
                                address: address(source.ip()),
                                tcp_port: item.port,
                                quic_port: item.quic_port,
                                source: "udp".into(),
                                last_seen: chrono::Utc::now().timestamp(),
                                error: None,
                            }],
                            false,
                        );
                    }
                }
            }
        }
    }
    status.lock().unwrap().udp = false;
    Ok(())
}
fn other(e: impl std::fmt::Display) -> AppError {
    AppError::Other(e.to_string())
}
