//! Unified file browser adapters for local shares, native NodeSend, WebDAV, SMB and FTP.
use chrono::Utc;
use aes_gcm::{aead::{Aead, KeyInit}, Aes256Gcm, Nonce};
use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
use futures_util::TryStreamExt;
use quick_xml::{events::Event, Reader};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};
use tokio::{
    fs,
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
};
use tokio_util::io::StreamReader;

use crate::{
    error::{AppError, AppResult},
    network::{browser as native_browser, peers::PeerStore},
    service::AppService,
};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum BrowserProtocol {
    Local,
    Nodesend,
    Webdav,
    Smb,
    Ftp,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BrowserConnection {
    pub id: String,
    pub name: String,
    pub protocol: BrowserProtocol,
    pub address: String,
    pub username: Option<String>,
    pub read_only: bool,
    pub connected: bool,
    /// Device-bound encrypted credential. It is never used as plaintext by the UI.
    #[serde(default)]
    pub saved_credential: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct BrowserConnectionInput {
    pub name: String,
    pub protocol: BrowserProtocol,
    pub address: String,
    pub username: Option<String>,
    pub password: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct BrowserEntry {
    pub name: String,
    pub path: String,
    pub is_dir: bool,
    pub size: u64,
    pub modified: Option<String>,
    pub can_write: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct BrowserListing {
    pub connection_id: String,
    pub path: String,
    pub entries: Vec<BrowserEntry>,
    pub can_write: bool,
}

#[derive(Clone)]
struct Session {
    public: BrowserConnection,
    password: Option<String>,
}

pub struct BrowserManager {
    app: Arc<AppService>,
    sessions: Mutex<HashMap<String, Session>>,
    peers: Arc<PeerStore>,
}

impl BrowserManager {
    pub fn new(app: Arc<AppService>) -> AppResult<Self> {
        let records = {
            let conn = app.db.conn();
            let mut stmt =
                conn.prepare("SELECT record FROM external_connections ORDER BY updated_at DESC")?;
            let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
            rows.collect::<Result<Vec<_>, _>>()?
        };
        let mut sessions = HashMap::new();
        for record in records {
            let value: serde_json::Value = serde_json::from_str(&record)?;
            // HTTP browser connections were removed from the current product surface.
            // Ignore legacy records so an upgrade cannot prevent the app from starting.
            if value.get("protocol").and_then(serde_json::Value::as_str) == Some("http") {
                continue;
            }
            let mut public: BrowserConnection = serde_json::from_value(value)?;
            public.connected = false;
            sessions.insert(
                public.id.clone(),
                Session {
                    public,
                    password: None,
                },
            );
        }
        Ok(Self {
            peers: PeerStore::new(app.clone())?,
            app,
            sessions: Mutex::new(sessions),
        })
    }

    pub fn with_peers(mut self, peers: Arc<PeerStore>) -> Self {
        self.peers = peers;
        self
    }

    pub async fn connect(&self, input: BrowserConnectionInput) -> AppResult<BrowserConnection> {
        let address = input.address.trim().to_string();
        if address.is_empty() || input.name.trim().is_empty() {
            return Err(AppError::BadRequest("连接名称和地址不能为空".into()));
        }
        validate_address(input.protocol, &address)?;
        let id = format!("conn-{:032x}", rand::random::<u128>());
        let mut public = BrowserConnection {
            id: id.clone(),
            name: input.name.trim().to_string(),
            protocol: input.protocol,
            address,
            username: input.username.clone(),
            read_only: false,
            connected: true,
            saved_credential: None,
        };
        let session = Session {
            public: public.clone(),
            password: input.password.clone(),
        };
        self.check_connection(&session).await?;
        public.saved_credential = input.password.as_deref().map(|value| self.encrypt_credential(value)).transpose()?;
        self.persist(&public)?;
        self.sessions.lock().unwrap().insert(id, session);
        Ok(public)
    }

    pub fn list(&self) -> Vec<BrowserConnection> {
        let mut values = self
            .sessions
            .lock()
            .unwrap()
            .values()
            .map(|v| v.public.clone())
            .collect::<Vec<_>>();
        values.sort_by(|a, b| a.name.cmp(&b.name));
        values
    }

    pub fn close(&self, id: &str) -> AppResult<()> {
        let mut sessions = self.sessions.lock().unwrap();
        let session = sessions
            .get_mut(id)
            .ok_or_else(|| AppError::NotFound("连接标签不存在".into()))?;
        session.public.connected = false;
        session.password = None;
        self.persist(&session.public)?;
        Ok(())
    }

    pub async fn reopen(&self, id: &str, password: Option<String>) -> AppResult<BrowserConnection> {
        let mut session = self
            .sessions
            .lock()
            .unwrap()
            .get(id)
            .cloned()
            .ok_or_else(|| AppError::NotFound("连接配置不存在".into()))?;
        let entered_password = password.filter(|value| !value.is_empty());
        session.password = match entered_password.clone() {
            Some(value) => Some(value),
            _ => self.decrypt_credential(session.public.saved_credential.as_deref())?,
        };
        self.check_connection(&session).await?;
        session.public.connected = true;
        if let Some(value) = entered_password.as_deref() {
            session.public.saved_credential = Some(self.encrypt_credential(value)?);
        }
        self.persist(&session.public)?;
        self.sessions
            .lock()
            .unwrap()
            .insert(id.to_string(), session.clone());
        Ok(session.public)
    }

    pub async fn test(&self, input: BrowserConnectionInput) -> AppResult<()> {
        let address = input.address.trim().to_string();
        if address.is_empty() || input.name.trim().is_empty() {
            return Err(AppError::BadRequest("连接名称和地址不能为空".into()));
        }
        validate_address(input.protocol, &address)?;
        let session = Session {
            public: BrowserConnection {
                id: String::new(),
                name: input.name.trim().to_string(),
                protocol: input.protocol,
                address,
                username: input.username,
                read_only: false,
                connected: true,
                saved_credential: None,
            },
            password: input.password,
        };
        self.check_connection(&session).await
    }

    pub fn update(&self, id: &str, input: BrowserConnectionInput) -> AppResult<BrowserConnection> {
        let address = input.address.trim().to_string();
        if address.is_empty() || input.name.trim().is_empty() {
            return Err(AppError::BadRequest("连接名称和地址不能为空".into()));
        }
        validate_address(input.protocol, &address)?;
        let mut sessions = self.sessions.lock().unwrap();
        let session = sessions
            .get_mut(id)
            .ok_or_else(|| AppError::NotFound("连接配置不存在".into()))?;
        session.public.name = input.name.trim().to_string();
        session.public.protocol = input.protocol;
        session.public.address = address;
        session.public.username = input.username;
        if input.password.is_some() {
            session.password = input.password.clone();
        }
        session.public.saved_credential = session.password.as_deref().map(|value| self.encrypt_credential(value)).transpose()?;
        session.public.connected = false;
        self.persist(&session.public)?;
        Ok(session.public.clone())
    }

    pub fn remove_connection(&self, id: &str) -> AppResult<()> {
        let removed = self.sessions.lock().unwrap().remove(id);
        if removed.is_none() {
            return Err(AppError::NotFound("连接配置不存在".into()));
        }
        self.app.db.conn().execute(
            "DELETE FROM external_connections WHERE id=?1",
            rusqlite::params![id],
        )?;
        Ok(())
    }

    pub async fn browse(&self, id: &str, path: String) -> AppResult<BrowserListing> {
        let session = self.session(id)?;
        if !session.public.connected {
            return Err(AppError::Forbidden("连接已关闭".into()));
        }
        let path = normalize_remote_path(&path)?;
        let entries = match session.public.protocol {
            BrowserProtocol::Local | BrowserProtocol::Smb => {
                local_list(&session.public.address, &path).await?
            }
            BrowserProtocol::Nodesend => native_browser::list(
                &self.app,
                &self.peers,
                &session.public.address,
                &path,
                session.password.as_deref(),
            )
            .await?
            .into_iter()
            .map(|entry| BrowserEntry {
                name: entry.name,
                path: entry.path,
                is_dir: entry.is_dir,
                size: entry.size,
                modified: entry.modified,
                can_write: false,
            })
            .collect(),
            BrowserProtocol::Webdav => webdav_list(&session, &path).await?,
            BrowserProtocol::Ftp => ftp_list(&session, &path).await?,
        };
        Ok(BrowserListing {
            connection_id: id.to_string(),
            path: path.clone(),
            entries,
            can_write: !session.public.read_only
                && (session.public.protocol != BrowserProtocol::Nodesend || !path.is_empty()),
        })
    }

    pub async fn download(&self, id: &str, path: String, destination: String) -> AppResult<()> {
        let session = self.session(id)?;
        let path = normalize_remote_path(&path)?;
        validate_local_destination(&destination)?;
        match session.public.protocol {
            BrowserProtocol::Local | BrowserProtocol::Smb => {
                fs::copy(
                    safe_local_existing(&session.public.address, &path)?,
                    &destination,
                )
                .await?;
            }
            BrowserProtocol::Nodesend => {
                native_browser::download(
                    &self.app,
                    &self.peers,
                    &session.public.address,
                    &path,
                    session.password.as_deref(),
                    &destination,
                )
                .await?
            }
            BrowserProtocol::Webdav => {
                webdav_get(&session, &path, &destination).await?;
            }
            BrowserProtocol::Ftp => {
                ftp_retrieve(&session, &path, &destination).await?;
            }
        }
        Ok(())
    }

    pub async fn upload(&self, id: &str, local: String, path: String) -> AppResult<()> {
        let session = self.session(id)?;
        if session.public.read_only {
            return Err(AppError::Forbidden("该连接只读".into()));
        }
        validate_local_destination(&local)?;
        let path = normalize_remote_path(&path)?;
        match session.public.protocol {
            BrowserProtocol::Local | BrowserProtocol::Smb => {
                let target = safe_local_target(&session.public.address, &path)?;
                fs::copy(local, target).await?;
            }
            BrowserProtocol::Webdav => {
                webdav_put(&session, &path, &local).await?;
            }
            BrowserProtocol::Ftp => {
                ftp_store(&session, &path, &local).await?;
            }
            BrowserProtocol::Nodesend => {
                native_browser::upload(
                    &self.app,
                    &self.peers,
                    &session.public.address,
                    &path,
                    session.password.as_deref(),
                    &local,
                )
                .await?
            }
        }
        Ok(())
    }

    pub async fn mkdir(&self, id: &str, path: String) -> AppResult<()> {
        let session = self.session(id)?;
        if session.public.read_only {
            return Err(AppError::Forbidden("该连接只读".into()));
        }
        let path = normalize_remote_path(&path)?;
        match session.public.protocol {
            BrowserProtocol::Local | BrowserProtocol::Smb => {
                fs::create_dir(safe_local_target(&session.public.address, &path)?).await?
            }
            BrowserProtocol::Webdav => webdav_method(&session, &path, "MKCOL", None).await?,
            BrowserProtocol::Ftp => {
                ftp_simple(&session, &format!("MKD {}", ftp_path(&session, &path)?)).await?
            }
            _ => return Err(AppError::Forbidden("该协议不支持创建目录".into())),
        }
        Ok(())
    }

    pub async fn delete(&self, id: &str, path: String) -> AppResult<()> {
        let session = self.session(id)?;
        if session.public.read_only {
            return Err(AppError::Forbidden("该连接只读".into()));
        }
        let path = normalize_remote_path(&path)?;
        match session.public.protocol {
            BrowserProtocol::Local | BrowserProtocol::Smb => {
                let p = safe_local_existing(&session.public.address, &path)?;
                if path.is_empty() {
                    return Err(AppError::Forbidden("不能删除连接根目录".into()));
                }
                if fs::metadata(&p).await?.is_dir() {
                    fs::remove_dir_all(p).await?
                } else {
                    fs::remove_file(p).await?
                }
            }
            BrowserProtocol::Webdav => webdav_method(&session, &path, "DELETE", None).await?,
            BrowserProtocol::Ftp => ftp_delete(&session, &path).await?,
            _ => return Err(AppError::Forbidden("该协议不支持删除".into())),
        }
        Ok(())
    }

    pub async fn rename(&self, id: &str, path: String, new_name: String) -> AppResult<()> {
        let session = self.session(id)?;
        if session.public.read_only {
            return Err(AppError::Forbidden("该连接只读".into()));
        }
        if new_name.is_empty()
            || normalize_remote_path(&new_name)? != new_name
            || new_name.contains('/')
        {
            return Err(AppError::BadRequest("新名称非法".into()));
        }
        let path = normalize_remote_path(&path)?;
        if path.is_empty() {
            return Err(AppError::BadRequest("不能重命名连接根目录".into()));
        }
        let parent = Path::new(&path)
            .parent()
            .map(|p| p.to_string_lossy().replace('\\', "/"))
            .unwrap_or_default();
        let new_path = if parent.is_empty() {
            new_name
        } else {
            format!("{parent}/{new_name}")
        };
        match session.public.protocol {
            BrowserProtocol::Local | BrowserProtocol::Smb => {
                fs::rename(
                    safe_local_existing(&session.public.address, &path)?,
                    safe_local_target(&session.public.address, &new_path)?,
                )
                .await?
            }
            BrowserProtocol::Webdav => {
                webdav_method(&session, &path, "MOVE", Some(&new_path)).await?
            }
            BrowserProtocol::Ftp => ftp_rename(&session, &path, &new_path).await?,
            _ => return Err(AppError::Forbidden("该协议只读".into())),
        }
        Ok(())
    }

    pub async fn move_entry(&self, id: &str, path: String, target: String) -> AppResult<()> {
        let session = self.session(id)?;
        if session.public.read_only {
            return Err(AppError::Forbidden("该连接只读".into()));
        }
        let path = normalize_remote_path(&path)?;
        let target = normalize_remote_path(&target)?;
        match session.public.protocol {
            BrowserProtocol::Local | BrowserProtocol::Smb => {
                fs::rename(
                    safe_local_existing(&session.public.address, &path)?,
                    safe_local_target(&session.public.address, &target)?,
                )
                .await?
            }
            BrowserProtocol::Webdav => {
                webdav_method(&session, &path, "MOVE", Some(&target)).await?
            }
            BrowserProtocol::Ftp => ftp_rename(&session, &path, &target).await?,
            _ => return Err(AppError::Forbidden("该协议不支持移动".into())),
        }
        Ok(())
    }

    fn session(&self, id: &str) -> AppResult<Session> {
        let session = self
            .sessions
            .lock()
            .unwrap()
            .get(id)
            .cloned()
            .ok_or_else(|| AppError::NotFound("连接标签不存在".into()))?;
        if !session.public.connected {
            return Err(AppError::Forbidden("连接已关闭".into()));
        }
        Ok(session)
    }
    fn persist(&self, public: &BrowserConnection) -> AppResult<()> {
        let now = Utc::now().to_rfc3339();
        self.app.db.conn().execute("INSERT INTO external_connections(id,record,created_at,updated_at) VALUES(?1,?2,?3,?3) ON CONFLICT(id) DO UPDATE SET record=excluded.record,updated_at=excluded.updated_at",rusqlite::params![public.id,serde_json::to_string(public)?,now])?;
        Ok(())
    }
    fn credential_key(&self) -> [u8; 32] {
        use sha2::{Digest, Sha256};
        Sha256::digest(format!("NodeSend connection credential:{}", self.app.identity.node_id).as_bytes()).into()
    }
    #[allow(deprecated)]
    fn encrypt_credential(&self, value: &str) -> AppResult<String> {
        let cipher = Aes256Gcm::new_from_slice(&self.credential_key()).map_err(|e| AppError::Crypto(e.to_string()))?;
        let nonce_bytes: [u8; 12] = rand::random();
        let ciphertext = cipher.encrypt(Nonce::from_slice(&nonce_bytes), value.as_bytes()).map_err(|_| AppError::Crypto("连接密码加密失败".into()))?;
        Ok(B64.encode([nonce_bytes.as_slice(), ciphertext.as_slice()].concat()))
    }
    #[allow(deprecated)]
    fn decrypt_credential(&self, value: Option<&str>) -> AppResult<Option<String>> {
        let Some(value) = value else { return Ok(None) };
        let raw = B64.decode(value).map_err(|_| AppError::Crypto("连接密码格式无效".into()))?;
        if raw.len() < 12 { return Err(AppError::Crypto("连接密码格式无效".into())); }
        let cipher = Aes256Gcm::new_from_slice(&self.credential_key()).map_err(|e| AppError::Crypto(e.to_string()))?;
        let plaintext = cipher.decrypt(Nonce::from_slice(&raw[..12]), &raw[12..]).map_err(|_| AppError::Crypto("连接密码无法在此设备解密".into()))?;
        Ok(Some(String::from_utf8(plaintext).map_err(|_| AppError::Crypto("连接密码格式无效".into()))?))
    }
    async fn check_connection(&self, s: &Session) -> AppResult<()> {
        match s.public.protocol {
            BrowserProtocol::Local | BrowserProtocol::Smb => {
                if !Path::new(&s.public.address).is_dir() {
                    return Err(AppError::NotFound("本地/SMB 目录不存在或无法访问".into()));
                }
            }
            BrowserProtocol::Nodesend => {
                native_browser::list(
                    &self.app,
                    &self.peers,
                    &s.public.address,
                    "",
                    s.password.as_deref(),
                )
                .await?;
            }
            BrowserProtocol::Webdav => {
                let client = reqwest::Client::new();
                let req = client
                    .request(
                        reqwest::Method::from_bytes(b"PROPFIND").unwrap(),
                        &s.public.address,
                    )
                    .header("Depth", "0");
                let req = auth(req, s);
                let response = req.send().await.map_err(net_error)?;
                if !response.status().is_success()
                    && response.status() != reqwest::StatusCode::MULTI_STATUS
                {
                    return Err(AppError::Other(format!(
                        "连接返回 HTTP {}",
                        response.status()
                    )));
                }
            }
            BrowserProtocol::Ftp => {
                let mut ftp = ftp_connect(s).await?;
                ftp_simple_on(&mut ftp, "NOOP").await?;
            }
        }
        Ok(())
    }
}

fn validate_address(protocol: BrowserProtocol, address: &str) -> AppResult<()> {
    match protocol {
        BrowserProtocol::Local | BrowserProtocol::Smb => Ok(()),
        BrowserProtocol::Nodesend => {
            if crate::network::peers::valid_id(address) {
                Ok(())
            } else {
                Err(AppError::BadRequest(
                    "NodeSend 地址必须是已发现设备的 Node ID".into(),
                ))
            }
        }
        BrowserProtocol::Webdav => {
            let parsed =
                url::Url::parse(address).map_err(|e| AppError::BadRequest(e.to_string()))?;
            if parsed.scheme() != "http" && parsed.scheme() != "https" {
                Err(AppError::BadRequest(
                    "WebDAV 地址必须以 http:// 或 https:// 开头".into(),
                ))
            } else if !parsed.username().is_empty()
                || parsed.password().is_some()
                || parsed.query().is_some()
            {
                Err(AppError::BadRequest(
                    "地址不能包含凭据或查询参数，请在凭据栏输入".into(),
                ))
            } else {
                Ok(())
            }
        }
        BrowserProtocol::Ftp => {
            let parsed =
                url::Url::parse(address).map_err(|e| AppError::BadRequest(e.to_string()))?;
            if parsed.scheme() == "ftp"
                && parsed.username().is_empty()
                && parsed.password().is_none()
                && parsed.query().is_none()
            {
                Ok(())
            } else {
                Err(AppError::BadRequest(
                    "FTP 地址须以 ftp:// 开头，且不能包含凭据或查询参数".into(),
                ))
            }
        }
    }
}
fn normalize_remote_path(path: &str) -> AppResult<String> {
    let path = path.trim_matches('/').replace('\\', "/");
    if path.split('/').any(|p| {
        p == "."
            || p == ".."
            || (p.is_empty() && !path.is_empty())
            || p.contains(':')
            || p.chars().any(char::is_control)
    }) {
        return Err(AppError::BadRequest("远端路径非法".into()));
    }
    Ok(path)
}
fn local_path(root: &str, path: &str) -> PathBuf {
    let mut p = PathBuf::from(root);
    for part in path.split('/').filter(|p| !p.is_empty()) {
        p.push(part);
    }
    p
}
fn safe_local_existing(root: &str, path: &str) -> AppResult<PathBuf> {
    let base = Path::new(root).canonicalize()?;
    let target = local_path(root, path).canonicalize()?;
    if !target.starts_with(&base) {
        return Err(AppError::Forbidden("路径不在连接目录内".into()));
    }
    Ok(target)
}
fn safe_local_target(root: &str, path: &str) -> AppResult<PathBuf> {
    if path.is_empty() {
        return Err(AppError::BadRequest("目标路径不能为空".into()));
    }
    let target = local_path(root, path);
    let parent = target
        .parent()
        .ok_or_else(|| AppError::BadRequest("目标路径非法".into()))?;
    let base = Path::new(root).canonicalize()?;
    let real_parent = parent.canonicalize()?;
    if !real_parent.starts_with(&base) {
        return Err(AppError::Forbidden("路径不在连接目录内".into()));
    }
    if target.exists() {
        let real_target = target.canonicalize()?;
        if !real_target.starts_with(&base) {
            return Err(AppError::Forbidden("路径不在连接目录内".into()));
        }
    }
    Ok(target)
}
fn validate_local_destination(path: &str) -> AppResult<()> {
    if path.trim().is_empty() {
        Err(AppError::BadRequest("本地文件路径不能为空".into()))
    } else {
        Ok(())
    }
}
fn auth(req: reqwest::RequestBuilder, s: &Session) -> reqwest::RequestBuilder {
    if s.public.protocol == BrowserProtocol::Nodesend {
        if let Some(token) = &s.password {
            return req.bearer_auth(token);
        }
    }
    match &s.public.username {
        Some(user) => req.basic_auth(user, s.password.as_deref()),
        None => {
            if let Some(token) = &s.password {
                req.bearer_auth(token)
            } else {
                req
            }
        }
    }
}
fn net_error(e: impl std::fmt::Display) -> AppError {
    AppError::Other(format!("网络连接失败: {e}"))
}

async fn save_response(response: reqwest::Response, destination: &str) -> AppResult<()> {
    let stream = response.bytes_stream().map_err(std::io::Error::other);
    let mut reader = StreamReader::new(stream);
    let mut file = fs::File::create(destination).await?;
    tokio::io::copy(&mut reader, &mut file).await?;
    file.flush().await?;
    Ok(())
}
fn join_url(root: &str, path: &str) -> String {
    if path.is_empty() {
        root.to_string()
    } else {
        format!(
            "{}/{}",
            root.trim_end_matches('/'),
            path.split('/')
                .map(urlencoding::encode)
                .collect::<Vec<_>>()
                .join("/")
        )
    }
}
fn ftp_path(s: &Session, path: &str) -> AppResult<String> {
    let url =
        url::Url::parse(&s.public.address).map_err(|e| AppError::BadRequest(e.to_string()))?;
    let base = url.path().trim_end_matches('/');
    if path.is_empty() {
        Ok(if base.is_empty() {
            "/".into()
        } else {
            base.into()
        })
    } else {
        Ok(format!("{base}/{path}"))
    }
}

async fn local_list(root: &str, path: &str) -> AppResult<Vec<BrowserEntry>> {
    let dir = safe_local_existing(root, path)?;
    let base = Path::new(root).canonicalize()?;
    let mut entries = Vec::new();
    let mut rd = fs::read_dir(&dir).await?;
    while let Some(item) = rd.next_entry().await? {
        match item.path().canonicalize() {
            Ok(real) if real.starts_with(&base) => {}
            _ => continue,
        }
        let meta = item.metadata().await?;
        let name = item.file_name().to_string_lossy().into_owned();
        let child = if path.is_empty() {
            name.clone()
        } else {
            format!("{path}/{name}")
        };
        entries.push(BrowserEntry {
            name,
            path: child,
            is_dir: meta.is_dir(),
            size: if meta.is_file() { meta.len() } else { 0 },
            modified: meta.modified().ok().map(|v| format!("{v:?}")),
            can_write: true,
        });
    }
    entries.sort_by_key(|e| (!e.is_dir, e.name.to_lowercase()));
    Ok(entries)
}

async fn webdav_list(s: &Session, path: &str) -> AppResult<Vec<BrowserEntry>> {
    let body = r#"<?xml version="1.0"?><d:propfind xmlns:d="DAV:"><d:prop><d:displayname/><d:getcontentlength/><d:resourcetype/><d:getlastmodified/></d:prop></d:propfind>"#;
    let response = auth(
        reqwest::Client::new()
            .request(
                reqwest::Method::from_bytes(b"PROPFIND").unwrap(),
                join_url(&s.public.address, path),
            )
            .header("Depth", "1")
            .body(body),
        s,
    )
    .send()
    .await
    .map_err(net_error)?;
    if !response.status().is_success() && response.status() != reqwest::StatusCode::MULTI_STATUS {
        return Err(AppError::Other(format!(
            "WebDAV 返回 HTTP {}",
            response.status()
        )));
    }
    let text = response.text().await.map_err(net_error)?;
    parse_webdav(&text, &s.public.address, path)
}
fn parse_webdav(text: &str, root: &str, prefix: &str) -> AppResult<Vec<BrowserEntry>> {
    let requested = join_url(root, prefix);
    let requested_url = url::Url::parse(&requested).map_err(|e| AppError::Other(e.to_string()))?;
    let mut reader = Reader::from_str(text);
    let mut entries = Vec::new();
    let mut href = String::new();
    let mut size = 0;
    let mut modified = None;
    let mut is_dir = false;
    let mut in_response = false;
    let mut field = String::new();
    loop {
        let event = reader
            .read_event()
            .map_err(|e| AppError::Other(format!("WebDAV XML 无效: {e}")))?;
        match event {
            Event::Start(e) => {
                let name = e.local_name().as_ref().to_string();
                if name == "response" {
                    in_response = true;
                    href.clear();
                    size = 0;
                    modified = None;
                    is_dir = false;
                } else if in_response && name == "collection" {
                    is_dir = true;
                }
                field = name;
            }
            Event::Empty(e) if in_response && e.local_name().as_ref() == "collection" => {
                is_dir = true
            }
            Event::Text(e) if in_response => {
                let value = quick_xml::escape::unescape(e.as_ref())
                    .map_err(|err| AppError::Other(err.to_string()))?;
                match field.as_str() {
                    "href" => href.push_str(&value),
                    "getcontentlength" => size = value.parse().unwrap_or(0),
                    "getlastmodified" => modified = Some(value.into_owned()),
                    _ => {}
                }
            }
            Event::End(e) if e.local_name().as_ref() == "response" && in_response => {
                in_response = false;
                if !href.is_empty() {
                    let resource = requested_url
                        .join(&href)
                        .map_err(|err| AppError::Other(err.to_string()))?;
                    if resource.path().trim_end_matches('/')
                        != requested_url.path().trim_end_matches('/')
                    {
                        let encoded = resource
                            .path()
                            .trim_end_matches('/')
                            .rsplit('/')
                            .next()
                            .unwrap_or("");
                        let name = urlencoding::decode(encoded)
                            .map_err(|err| AppError::Other(err.to_string()))?
                            .into_owned();
                        if !name.is_empty() && name != "." && name != ".." {
                            let path = if prefix.is_empty() {
                                name.clone()
                            } else {
                                format!("{prefix}/{name}")
                            };
                            entries.push(BrowserEntry {
                                name,
                                path,
                                is_dir,
                                size,
                                modified: modified.clone(),
                                can_write: true,
                            });
                        }
                    }
                }
                field.clear();
            }
            Event::End(_) => field.clear(),
            Event::Eof => break,
            _ => {}
        }
    }
    entries.sort_by_key(|e| (!e.is_dir, e.name.to_lowercase()));
    Ok(entries)
}
async fn webdav_get(s: &Session, path: &str, destination: &str) -> AppResult<()> {
    let r = auth(
        reqwest::Client::new().get(join_url(&s.public.address, path)),
        s,
    )
    .send()
    .await
    .map_err(net_error)?;
    if !r.status().is_success() {
        return Err(AppError::Other(format!(
            "WebDAV 下载返回 HTTP {}",
            r.status()
        )));
    }
    save_response(r, destination).await
}
async fn webdav_put(s: &Session, path: &str, local: &str) -> AppResult<()> {
    let file = fs::File::open(local).await?;
    let size = file.metadata().await?.len();
    let stream = tokio_util::io::ReaderStream::new(file);
    let r = auth(
        reqwest::Client::new()
            .put(join_url(&s.public.address, path))
            .body(reqwest::Body::wrap_stream(stream))
            .header(reqwest::header::CONTENT_LENGTH, size),
        s,
    )
    .send()
    .await
    .map_err(net_error)?;
    if !r.status().is_success() {
        return Err(AppError::Other(format!(
            "WebDAV 上传返回 HTTP {}",
            r.status()
        )));
    }
    Ok(())
}
async fn webdav_method(
    s: &Session,
    path: &str,
    method: &str,
    destination: Option<&str>,
) -> AppResult<()> {
    let m = reqwest::Method::from_bytes(method.as_bytes())
        .map_err(|e| AppError::BadRequest(e.to_string()))?;
    let mut r = reqwest::Client::new().request(m, join_url(&s.public.address, path));
    if let Some(dest) = destination {
        r = r.header("Destination", join_url(&s.public.address, dest));
    }
    let r = auth(r, s).send().await.map_err(net_error)?;
    if !r.status().is_success() {
        return Err(AppError::Other(format!(
            "WebDAV {method} 返回 HTTP {}",
            r.status()
        )));
    }
    Ok(())
}

async fn ftp_connect(s: &Session) -> AppResult<TcpStream> {
    let url =
        url::Url::parse(&s.public.address).map_err(|e| AppError::BadRequest(e.to_string()))?;
    let host = url
        .host_str()
        .ok_or_else(|| AppError::BadRequest("FTP 地址缺少主机".into()))?;
    let port = url.port().unwrap_or(21);
    let mut stream = TcpStream::connect((host, port)).await.map_err(net_error)?;
    ftp_expect(&mut stream, &[220]).await?;
    let user = s
        .public
        .username
        .as_deref()
        .or_else(|| url.username().is_empty().then_some("anonymous"))
        .unwrap_or("anonymous");
    let response = ftp_command(&mut stream, &format!("USER {user}")).await?;
    if response.starts_with("331") {
        let password = s
            .password
            .as_deref()
            .or_else(|| url.password())
            .unwrap_or("anonymous@");
        let response = ftp_command(&mut stream, &format!("PASS {password}")).await?;
        if !response.starts_with("230") {
            return Err(AppError::Other("FTP 登录失败".into()));
        }
    } else if !response.starts_with("230") {
        return Err(AppError::Other("FTP 登录失败".into()));
    }
    ftp_expect_command(&mut stream, "TYPE I", &[200]).await?;
    Ok(stream)
}
async fn ftp_simple(s: &Session, command: &str) -> AppResult<()> {
    let mut ftp = ftp_connect(s).await?;
    ftp_simple_on(&mut ftp, command).await
}
async fn ftp_simple_on(stream: &mut TcpStream, command: &str) -> AppResult<()> {
    ftp_expect_command(stream, command, &[200, 250, 257])
        .await
        .map(|_| ())
}
async fn ftp_expect(stream: &mut TcpStream, codes: &[u16]) -> AppResult<String> {
    let response = ftp_response(stream).await?;
    let code = response.get(..3).and_then(|v| v.parse::<u16>().ok());
    if code.is_some_and(|code| codes.contains(&code)) {
        Ok(response)
    } else {
        Err(AppError::Other(format!("FTP: {}", response.trim())))
    }
}
async fn ftp_expect_command(
    stream: &mut TcpStream,
    command: &str,
    codes: &[u16],
) -> AppResult<String> {
    stream
        .write_all(format!("{command}\r\n").as_bytes())
        .await?;
    ftp_expect(stream, codes).await
}
async fn ftp_response(stream: &mut TcpStream) -> AppResult<String> {
    let mut buf = Vec::new();
    let mut byte = [0];
    loop {
        stream.read_exact(&mut byte).await?;
        buf.push(byte[0]);
        if buf.ends_with(b"\r\n") {
            let text = String::from_utf8_lossy(&buf).into_owned();
            if text.len() >= 4 && text.as_bytes()[3] == b' ' {
                return Ok(text);
            }
        }
        if buf.len() > 64 * 1024 {
            return Err(AppError::Other("FTP 响应过大".into()));
        }
    }
}
async fn ftp_command(stream: &mut TcpStream, command: &str) -> AppResult<String> {
    stream
        .write_all(format!("{command}\r\n").as_bytes())
        .await?;
    let response = ftp_response(stream).await?;
    if response.as_bytes().get(0).is_some_and(|c| *c >= b'4') {
        return Err(AppError::Other(format!("FTP: {}", response.trim())));
    }
    Ok(response)
}
async fn ftp_passive(stream: &mut TcpStream) -> AppResult<TcpStream> {
    let response = ftp_expect_command(stream, "PASV", &[227]).await?;
    let body = response
        .split('(')
        .nth(1)
        .and_then(|v| v.split(')').next())
        .ok_or_else(|| AppError::Other("FTP PASV 响应非法".into()))?;
    let nums = body
        .split(',')
        .map(|v| v.trim().parse::<u16>())
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| AppError::Other("FTP PASV 地址非法".into()))?;
    if nums.len() != 6 || nums.iter().any(|n| *n > 255) || nums[4] == 0 && nums[5] == 0 {
        return Err(AppError::Other("FTP PASV 地址非法".into()));
    }
    TcpStream::connect((
        format!("{}.{}.{}.{}", nums[0], nums[1], nums[2], nums[3]),
        (nums[4] * 256 + nums[5]),
    ))
    .await
    .map_err(net_error)
}
async fn ftp_retrieve(s: &Session, path: &str, destination: &str) -> AppResult<()> {
    let mut control = ftp_connect(s).await?;
    let mut data = ftp_passive(&mut control).await?;
    ftp_expect_command(
        &mut control,
        &format!("RETR {}", ftp_path(s, path)?),
        &[125, 150],
    )
    .await?;
    let mut file = fs::File::create(destination).await?;
    tokio::io::copy(&mut data, &mut file).await?;
    file.flush().await?;
    ftp_expect(&mut control, &[226, 250]).await?;
    Ok(())
}

async fn ftp_list(s: &Session, path: &str) -> AppResult<Vec<BrowserEntry>> {
    let mut control = ftp_connect(s).await?;
    let mut data = ftp_passive(&mut control).await?;
    ftp_expect_command(
        &mut control,
        &format!("LIST {}", ftp_path(s, path)?),
        &[125, 150],
    )
    .await?;
    let mut bytes = Vec::new();
    data.read_to_end(&mut bytes).await?;
    ftp_expect(&mut control, &[226, 250]).await?;
    let text = String::from_utf8_lossy(&bytes);
    Ok(text
        .lines()
        .filter_map(|line| {
            let fields = line.split_whitespace().collect::<Vec<_>>();
            if fields.len() < 9 {
                return None;
            }
            let is_dir = line.starts_with('d');
            let name = fields[8..].join(" ");
            if name == "." || name == ".." {
                return None;
            }
            let child = if path.is_empty() {
                name.clone()
            } else {
                format!("{path}/{name}")
            };
            Some(BrowserEntry {
                name,
                path: child,
                is_dir,
                size: fields[4].parse().unwrap_or(0),
                modified: None,
                can_write: true,
            })
        })
        .collect())
}
async fn ftp_store(s: &Session, path: &str, local: &str) -> AppResult<()> {
    let mut control = ftp_connect(s).await?;
    let mut channel = ftp_passive(&mut control).await?;
    ftp_expect_command(
        &mut control,
        &format!("STOR {}", ftp_path(s, path)?),
        &[125, 150],
    )
    .await?;
    let mut file = fs::File::open(local).await?;
    tokio::io::copy(&mut file, &mut channel).await?;
    channel.shutdown().await?;
    ftp_expect(&mut control, &[226, 250]).await?;
    Ok(())
}
async fn ftp_delete(s: &Session, path: &str) -> AppResult<()> {
    if path.is_empty() {
        return Err(AppError::Forbidden("不能删除连接根目录".into()));
    }
    let full = ftp_path(s, path)?;
    let mut control = ftp_connect(s).await?;
    if ftp_expect_command(&mut control, &format!("DELE {full}"), &[250])
        .await
        .is_err()
    {
        ftp_expect_command(&mut control, &format!("RMD {full}"), &[250]).await?;
    }
    Ok(())
}
async fn ftp_rename(s: &Session, source: &str, target: &str) -> AppResult<()> {
    let mut control = ftp_connect(s).await?;
    ftp_expect_command(
        &mut control,
        &format!("RNFR {}", ftp_path(s, source)?),
        &[350],
    )
    .await?;
    ftp_expect_command(
        &mut control,
        &format!("RNTO {}", ftp_path(s, target)?),
        &[250],
    )
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::parse_webdav;

    #[test]
    fn webdav_accepts_namespace_prefixes_and_skips_current_directory() {
        let xml = r#"<?xml version="1.0"?><x:multistatus xmlns:x="DAV:">
            <x:response><x:href>/dav/folder/</x:href><x:propstat><x:prop><x:resourcetype><x:collection/></x:resourcetype></x:prop></x:propstat></x:response>
            <x:response><x:href>/dav/folder/a%20b.txt</x:href><x:propstat><x:prop><x:getcontentlength>42</x:getcontentlength></x:prop></x:propstat></x:response>
        </x:multistatus>"#;
        let entries = parse_webdav(xml, "https://example.com/dav/", "folder").unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "a b.txt");
        assert_eq!(entries[0].path, "folder/a b.txt");
        assert_eq!(entries[0].size, 42);
    }
}
