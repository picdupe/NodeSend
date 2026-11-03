use crate::locks::MutexExt;
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
            .map_err(|_| AppError::BadRequest("Endpoint address is invalid".into()))
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
        let values: Vec<String> = app.db.with_conn(|conn| {
            let mut stmt = conn.prepare("SELECT record FROM nodes")?;
            let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
            Ok(rows.collect::<Result<Vec<_>, _>>()?)
        })?;
        let mut peers = HashMap::new();
        for value in values {
            let mut peer: Peer = serde_json::from_str(&value)?;
            // 全新 ID 格式不兼容旧版：无法规整的记录直接跳过，
            // 等对端重新广播后再以规范 ID 入库。
            let Some(node_id) = crate::identity::node_identity::normalize_node_id(&peer.node_id)
            else {
                tracing::warn!(stored = %peer.node_id, "Stored peer has an invalid Node ID");
                continue;
            };
            peer.node_id = node_id;
            peer.online = false;
            for ep in &mut peer.endpoints {
                ep.error = Some("Waiting to reconnect".into());
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
        // 发现报文允许大小写与 I/L/O 等易混字符；统一规整为规范的
        // `XXXX-XXXX-XXXX-XXXX-XX`，保证去重、信任与数据库键值一致。
        let Some(node_id) = crate::identity::node_identity::normalize_node_id(&node_id) else {
            return Err(AppError::BadRequest("Invalid Node ID".into()));
        };
        if node_id == self.app.identity.node_id {
            return Ok(());
        }
        if name.len() > 128 {
            return Err(AppError::BadRequest("Node discovery packet is invalid".into()));
        }
        let mut peers = crate::locks::lock(&self.peers);
        if peers.len() >= 2048 && !peers.contains_key(&node_id) {
            return Err(AppError::Forbidden("Discovered device count exceeded the limit".into()));
        }
        let previous = peers.get(&node_id).cloned();
        let peer = peers.entry(node_id.clone()).or_insert(Peer {
            node_id: node_id.clone(),
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
        peer.endpoints.retain(|e| retain_previous_address(e, now));
        for ep in endpoints {
            if ep.socket(false).is_err() || ep.tcp_port == 0 {
                continue;
            }
            if let Some(existing) = peer.endpoints.iter_mut().find(|v| v.address == ep.address) {
                *existing = ep;
            } else if peer.endpoints.len() < 32 {
                peer.endpoints.push(ep);
            }
        }
        let json = serde_json::to_string(peer)?;
        if let Err(error) = self.app.db.with_conn(|conn| {
            conn.execute("INSERT INTO nodes(node_id,record) VALUES(?1,?2) ON CONFLICT(node_id) DO UPDATE SET record=excluded.record",rusqlite::params![peer.node_id, json])?;
            Ok(())
        }) {
            if let Some(previous) = previous {
                peers.insert(node_id, previous);
            } else {
                peers.remove(&node_id);
            }
            return Err(error);
        }
        Ok(())
    }
    pub fn list(&self) -> Vec<Peer> {
        let trusted = match crate::registry::list(&self.app.db) {
            Ok(trusted) => Some(trusted),
            Err(error) => {
                tracing::error!(%error, "Failed to read the trusted-device list; retaining the known Trust state");
                None
            }
        };
        let mut peers = self
            .peers
            .lock_or_recover()
            .values()
            .cloned()
            .collect::<Vec<_>>();
        let now = chrono::Utc::now().timestamp();
        for p in &mut peers {
            p.endpoints.retain(|e| retain_previous_address(e, now));
            p.online = p.endpoints.iter().any(|e| {
                e.error.is_none() && (e.source == "mdns" || now.saturating_sub(e.last_seen) < 300)
            });
            if let Some(trusted) = &trusted {
                p.trusted = trusted.iter().any(|t| t.node_id == p.node_id);
            }
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
            .ok_or_else(|| {
                AppError::NotFound(
                    "Target Node has not been discovered; add its address manually".into(),
                )
            })
    }
    pub fn refresh_addresses(&self, id: &str, endpoints: Vec<Endpoint>) -> AppResult<()> {
        let mut peers = crate::locks::lock(&self.peers);
        let Some(peer) = peers.get_mut(id) else {
            return Ok(());
        };
        let previous = peer.clone();
        let now = chrono::Utc::now().timestamp();
        peer.endpoints.retain(|e| retain_previous_address(e, now));
        for endpoint in endpoints {
            if let Some(old) = peer
                .endpoints
                .iter_mut()
                .find(|e| e.address == endpoint.address)
            {
                *old = endpoint;
            } else {
                peer.endpoints.push(endpoint);
            }
        }
        let json = serde_json::to_string(peer)?;
        if let Err(error) = self.app.db.with_conn(|conn| {
            conn.execute(
                "UPDATE nodes SET record=?1 WHERE node_id=?2",
                rusqlite::params![json, id],
            )?;
            Ok(())
        }) {
            peers.insert(id.to_string(), previous);
            return Err(error);
        }
        Ok(())
    }
    pub fn failure(&self, id: &str, ep: &Endpoint, error: String) {
        if let Some(p) = crate::locks::lock(&self.peers).get_mut(id) {
            if let Some(e) = p.endpoints.iter_mut().find(|e| e.address == ep.address) {
                e.error = Some(error);
            }
        }
    }
    pub fn seen(&self, id: &str, ep: &Endpoint) {
        if let Some(peer) = crate::locks::lock(&self.peers).get_mut(id) {
            if let Some(current) = peer.endpoints.iter_mut().find(|e| e.address == ep.address) {
                current.last_seen = chrono::Utc::now().timestamp();
                current.error = None;
            }
        }
    }
    pub fn remove(&self, id: &str) -> AppResult<()> {
        self.app.db.with_transaction(|tx| {
            tx.execute("DELETE FROM nodes WHERE node_id=?1", [id])?;
            tx.execute("DELETE FROM trusted_nodes WHERE node_id=?1", [id])?;
            tx.execute("DELETE FROM node_certificates WHERE node_id=?1", [id])?;
            Ok(())
        })?;
        crate::locks::lock(&self.peers).remove(id);
        Ok(())
    }
}
pub fn valid_id(id: &str) -> bool {
    crate::identity::node_identity::is_node_id(id)
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
            ("10.1.1.3", 86399, true),
            ("10.1.1.3", 86400, false),
            ("10.1.1.3", 86401, false),
            ("::ffff:10.1.1.3", 1, true),
            ("2408:846a:7310:3bed::6d", 1, false),
            ("fe80::1%3", 1, false),
        ] {
            let endpoint = Endpoint {
                address: address.into(),
                tcp_port: 58082,
                quic_port: 0,
                source: "manual".into(),
                last_seen: now - age,
                error: None,
            };
            assert_eq!(
                retain_previous_address(&endpoint, now),
                keep,
                "{address}, age {age}"
            );
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
