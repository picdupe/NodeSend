use crate::{
    error::{AppError, AppResult},
    service::AppService,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    net::{IpAddr, SocketAddr},
    sync::{Arc, Mutex},
};
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Endpoint {
    pub address: String,
    pub tcp_port: u16,
    pub quic_port: u16,
    pub source: String,
    pub last_seen: i64,
    pub error: Option<String>,
}
impl Endpoint {
    pub fn socket(&self, quic: bool) -> AppResult<SocketAddr> {
        let port = if quic { self.quic_port } else { self.tcp_port };
        let raw = if self.address.contains(':') {
            format!("[{}]:{port}", self.address)
        } else {
            format!("{}:{port}", self.address)
        };
        raw.parse()
            .map_err(|_| AppError::BadRequest("Endpoint 地址非法".into()))
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Peer {
    pub node_id: String,
    pub display_name: String,
    pub endpoints: Vec<Endpoint>,
    pub online: bool,
    pub trusted: bool,
    pub verified: bool,
}
pub struct PeerStore {
    app: Arc<AppService>,
    peers: Mutex<HashMap<String, Peer>>,
}
impl PeerStore {
    pub fn new(app: Arc<AppService>) -> AppResult<Arc<Self>> {
        let values = {
            let conn = app.db.conn();
            let mut stmt = conn.prepare("SELECT record FROM nodes")?;
            let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
            rows.collect::<Result<Vec<_>, _>>()?
        };
        let mut peers = HashMap::new();
        for value in values {
            let mut peer: Peer = serde_json::from_str(&value)?;
            peer.online = false;
            for ep in &mut peer.endpoints {
                ep.error = Some("等待重新连接".into());
            }
            peers.insert(peer.node_id.clone(), peer);
        }
        Ok(Arc::new(Self {
            app,
            peers: Mutex::new(peers),
        }))
    }
    pub fn upsert(
        &self,
        node_id: String,
        name: String,
        endpoints: Vec<Endpoint>,
        verified: bool,
    ) -> AppResult<()> {
        if node_id == self.app.identity.node_id {
            return Ok(());
        }
        if !valid_id(&node_id) || name.len() > 128 {
            return Err(AppError::BadRequest("Node 发现报文非法".into()));
        }
        let mut peers = self.peers.lock().unwrap();
        if peers.len() >= 2048 && !peers.contains_key(&node_id) {
            return Err(AppError::Forbidden("发现设备数量超出限制".into()));
        }
        let peer = peers.entry(node_id.clone()).or_insert(Peer {
            node_id,
            display_name: name.clone(),
            endpoints: vec![],
            online: true,
            trusted: false,
            verified: false,
        });
        if verified || !peer.verified {
            peer.display_name = name;
        }
        peer.verified |= verified;
        let now = chrono::Utc::now().timestamp();
        peer.endpoints.retain(|e| !e.socket(false).is_ok_and(|a| a.ip().is_ipv4()) || now.saturating_sub(e.last_seen) < 86400);
        for ep in endpoints {
            if ep.socket(false).is_err() || ep.tcp_port == 0 {
                continue;
            }
            if let Some(existing) = peer
                .endpoints
                .iter_mut()
                .find(|v| v.address == ep.address && v.tcp_port == ep.tcp_port)
            {
                *existing = ep;
            } else if peer.endpoints.len() < 32 {
                peer.endpoints.push(ep);
            }
        }
        self.app.db.conn().execute("INSERT INTO nodes(node_id,record) VALUES(?1,?2) ON CONFLICT(node_id) DO UPDATE SET record=excluded.record",rusqlite::params![peer.node_id,serde_json::to_string(peer)?])?;
        Ok(())
    }
    pub fn list(&self) -> Vec<Peer> {
        let trusted = crate::registry::list(&self.app.db).unwrap_or_default();
        let mut peers = self
            .peers
            .lock()
            .unwrap()
            .values()
            .cloned()
            .collect::<Vec<_>>();
        let now = chrono::Utc::now().timestamp();
        for p in &mut peers {
            p.endpoints.retain(|e| !e.socket(false).is_ok_and(|a| a.ip().is_ipv4()) || now.saturating_sub(e.last_seen) < 86400);
            // Discovery announcements are periodic, but mDNS may only emit a
            // resolved event once and mobile multicast delivery can be
            // delayed while the app is resumed. mDNS registration remains a
            // live service signal until a connection failure; other discovery
            // sources use a five-minute freshness window.
            p.online = p.endpoints.iter().any(|e| {
                e.error.is_none() && (e.source == "mdns" || now.saturating_sub(e.last_seen) < 300)
            });
            p.trusted = trusted.iter().any(|t| t.node_id == p.node_id);
            p.endpoints
                .sort_by_key(|e| (e.error.is_some(), std::cmp::Reverse(e.last_seen)));
        }
        peers.sort_by_key(|p| (!p.online, p.display_name.clone()));
        peers
    }
    pub fn get(&self, id: &str) -> AppResult<Peer> {
        self.list()
            .into_iter()
            .find(|p| p.node_id == id)
            .ok_or_else(|| AppError::NotFound("目标 Node 尚未发现，请手动添加地址".into()))
    }
    pub fn refresh_addresses(&self, id: &str, endpoints: Vec<Endpoint>) -> AppResult<()> {
        let mut peers = self.peers.lock().unwrap();
        let Some(peer) = peers.get_mut(id) else { return Ok(()); };
        let now = chrono::Utc::now().timestamp();
        peer.endpoints.retain(|e| retain_previous_address(e, now));
        for endpoint in endpoints {
            if let Some(old) = peer.endpoints.iter_mut().find(|e| e.address == endpoint.address && e.tcp_port == endpoint.tcp_port) {
                *old = endpoint;
            } else {
                peer.endpoints.push(endpoint);
            }
        }
        self.app.db.conn().execute("UPDATE nodes SET record=?1 WHERE node_id=?2", rusqlite::params![serde_json::to_string(peer)?, id])?;
        Ok(())
    }
    pub fn failure(&self, id: &str, ep: &Endpoint, error: String) {
        if let Some(p) = self.peers.lock().unwrap().get_mut(id) {
            if let Some(e) = p
                .endpoints
                .iter_mut()
                .find(|e| e.address == ep.address && e.tcp_port == ep.tcp_port)
            {
                e.error = Some(error);
            }
        }
    }
    pub fn seen(&self, id: &str, ep: &Endpoint) {
        if let Some(peer) = self.peers.lock().unwrap().get_mut(id) {
            if let Some(current) = peer
                .endpoints
                .iter_mut()
                .find(|e| e.address == ep.address && e.tcp_port == ep.tcp_port)
            {
                current.last_seen = chrono::Utc::now().timestamp();
                current.error = None;
            }
        }
    }
    pub fn remove(&self, id: &str) -> AppResult<()> {
        self.peers.lock().unwrap().remove(id);
        let conn = self.app.db.conn();
        conn.execute("DELETE FROM nodes WHERE node_id=?1", [id])?;
        conn.execute("DELETE FROM trusted_nodes WHERE node_id=?1", [id])?;
        conn.execute("DELETE FROM node_certificates WHERE node_id=?1", [id])?;
        Ok(())
    }
}
pub fn valid_id(id: &str) -> bool {
    id.len() == 64 && id.bytes().all(|c| c.is_ascii_hexdigit())
}
fn retain_previous_address(endpoint: &Endpoint, now: i64) -> bool {
    endpoint.socket(false).is_ok_and(|a| match a.ip() {
        IpAddr::V4(_) => true,
        IpAddr::V6(v) => v.to_ipv4_mapped().is_some(),
    }) && now.saturating_sub(endpoint.last_seen) < 86400
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn snapshot_expires_ipv4_after_24_hours_and_replaces_ipv6() {
        let now = 200000;
        for (address, age, keep) in [
            ("10.1.1.3", 86399, true), ("10.1.1.3", 86400, false),
            ("10.1.1.3", 86401, false), ("::ffff:10.1.1.3", 1, true),
            ("2408:846a:7310:3bed::6d", 1, false), ("fe80::1%3", 1, false),
        ] {
            let endpoint = Endpoint { address: address.into(), tcp_port: 58082, quic_port: 0,
                source: "manual".into(), last_seen: now - age, error: None };
            assert_eq!(retain_previous_address(&endpoint, now), keep, "{address}, age {age}");
        }
    }
}
pub fn address(ip: IpAddr) -> String {
    match ip {
        IpAddr::V6(v) => v
            .to_ipv4_mapped()
            .map(|v| v.to_string())
            .unwrap_or_else(|| v.to_string()),
        _ => ip.to_string(),
    }
}
