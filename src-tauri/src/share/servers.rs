use super::{AccessMode, ShareConfig, ShareProtocol};
use crate::{
    error::{AppError, AppResult},
    service::AppService,
};
use serde::Serialize;
use std::{
    collections::{HashMap, HashSet},
    net::{IpAddr, SocketAddr},
    sync::{Arc, RwLock, Weak},
    time::Duration,
};
use tokio::{
    io::AsyncReadExt,
    net::{TcpListener, TcpStream},
    sync::Mutex,
    task::JoinHandle,
};
use tokio_util::sync::CancellationToken;

const FTP_CONTROL_PORT: u16 = 21;
const FTP_COMMON_PORTS: &[u16] = &[20, 21, 22, 23, 25, 53, 67, 68, 69, 80, 88];

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
struct SmbShareStatus {
    protocol_share_name: String,
    username: Option<String>,
    port: u16,
    error: Option<String>,
}
struct SmbUser {
    password: String,
    share_ids: HashSet<String>,
}
struct SmbGroup {
    service: Running,
    share_ids: HashSet<String>,
    configs: Vec<smb_server::ConfigHandle>,
    users: HashMap<String, SmbUser>,
    bind_key: String,
    port_explicit: bool,
}
struct HttpGroup {
    service: Running,
    share_ids: HashSet<String>,
    allowed_share_ids: Arc<RwLock<HashSet<String>>>,
    events: Arc<crate::httpapi::events::ShareEvents>,
    bind_key: String,
    port_explicit: bool,
}
struct FtpGroup {
    service: Running,
    share_ids: HashSet<String>,
    auth_share_ids: Arc<RwLock<HashSet<String>>>,
    bind_key: String,
    port_explicit: bool,
}

async fn add_smb_share_to_groups(
    groups: &mut HashMap<String, SmbGroup>,
    group_keys: &[String],
    share: &ShareConfig,
    username: Option<&str>,
    password: Option<&str>,
) -> AppResult<()> {
    // Configure every listener before recording the share ID. If a listener
    // rejects the change, roll back the earlier listeners so the share cannot
    // become visible on only part of its requested addresses.
    let mut touched: Vec<(String, bool)> = Vec::new();
    for key in group_keys {
        if !groups.contains_key(key) {
            rollback_smb_share(
                groups,
                &touched,
                &share.id,
                &share.protocol_share_name,
                username.unwrap_or(""),
            )
            .await;
            return Err(failure("Listener group disappeared while adding SMB share"));
        }
        let password_conflict = username
            .and_then(|username| groups.get(key)?.users.get(username))
            .zip(password)
            .is_some_and(|(existing, password)| existing.password.as_str() != password);
        if password_conflict {
            rollback_smb_share(
                groups,
                &touched,
                &share.id,
                &share.protocol_share_name,
                username.unwrap_or(""),
            )
            .await;
            return Err(AppError::Conflict("SMB_USERNAME_PASSWORD_CONFLICT".into()));
        }
        let new_user = if let (Some(username), Some(_password)) = (username, password) {
            !groups.get(key).expect("group checked above").users.contains_key(username)
        } else {
            false
        };
        touched.push((key.clone(), new_user));

        let result = {
            let group = groups.get_mut(key).expect("group checked above");
            let mut result = Ok(());
            if let (Some(username), Some(password)) = (username, password) {
                for config in &group.configs {
                    if let Err(error) = config.add_user(username, password).await {
                        result = Err(failure(error));
                        break;
                    }
                }
                if result.is_ok() && new_user {
                    group.users.insert(
                        username.to_owned(),
                        SmbUser {
                            password: password.to_owned(),
                            share_ids: HashSet::new(),
                        },
                    );
                }
            }

            if result.is_ok() {
                for config in &group.configs {
                    let backend = match smb_server::LocalFsBackend::new(&share.path) {
                        Ok(backend) => backend,
                        Err(error) => {
                            result = Err(failure(error));
                            break;
                        }
                    };
                    let mut smb_share = smb_server::Share::new(&share.protocol_share_name, backend);
                    if share.access_mode == AccessMode::Open {
                        smb_share = smb_share.public();
                    } else if let Some(username) = username {
                        smb_share = smb_share.user(username, smb_server::Access::ReadWrite);
                    }
                    if let Err(error) = config.add_share(smb_share).await {
                        result = Err(failure(error));
                        break;
                    }
                }
            }

            if result.is_ok() {
                group.share_ids.insert(share.id.clone());
                if let Some(username) = username {
                    if let Some(user) = group.users.get_mut(username) {
                        user.share_ids.insert(share.id.clone());
                    }
                }
            }
            result
        };
        if let Err(error) = result {
            rollback_smb_share(
                groups,
                &touched,
                &share.id,
                &share.protocol_share_name,
                username.unwrap_or(""),
            )
            .await;
            return Err(error);
        }
    }
    Ok(())
}

async fn rollback_smb_share(
    groups: &mut HashMap<String, SmbGroup>,
    touched: &[(String, bool)],
    share_id: &str,
    share_name: &str,
    username: &str,
) {
    for (key, remove_user) in touched.iter().rev() {
        let Some(group) = groups.get_mut(key) else {
            continue;
        };
        for config in &group.configs {
            if let Err(error) = config.remove_share(share_name).await {
                tracing::error!(share_id = %share_id, share_name = %share_name, %error, "Failed to roll back an SMB share from a listener");
            }
        }
        let remove_unused_user = if username.is_empty() {
            false
        } else if let Some(user) = group.users.get_mut(username) {
            user.share_ids.remove(share_id);
            *remove_user || user.share_ids.is_empty()
        } else {
            false
        };
        if remove_unused_user {
            let mut removed_from_all = true;
            for config in &group.configs {
                if let Err(error) = config.remove_user(username).await {
                    removed_from_all = false;
                    tracing::error!(share_id = %share_id, username = %username, %error, "Failed to roll back an SMB user from a listener");
                }
            }
            if removed_from_all {
                group.users.remove(username);
            }
        }
        group.share_ids.remove(share_id);
    }
}
#[derive(Default)]
pub struct ProtocolManager {
    running: Mutex<HashMap<String, Running>>,
    smb_servers: Mutex<HashMap<String, SmbGroup>>,
    smb_shares: Mutex<HashMap<String, SmbShareStatus>>,
    smb_share_groups: Mutex<HashMap<String, String>>,
    http_lifecycle_locks: Mutex<HashMap<String, Weak<Mutex<()>>>>,
    http_servers: Mutex<HashMap<String, HttpGroup>>,
    http_shares: Mutex<HashMap<String, (u16, Option<String>)>>,
    http_share_groups: Mutex<HashMap<String, String>>,
    http_directory_trees:
        Arc<RwLock<HashMap<String, Arc<RwLock<Vec<crate::httpapi::tree::DirectoryNode>>>>>>,
    http_dav_handlers: Arc<RwLock<HashMap<String, dav_server::DavHandler>>>,
    http_events: Mutex<HashMap<String, Arc<crate::httpapi::events::ShareEvents>>>,
    ftp_servers: Mutex<HashMap<String, FtpGroup>>,
    ftp_shares: super::ftp_fs::FtpShareMap,
    ftp_status: Mutex<HashMap<String, (u16, Option<String>)>>,
    ftp_share_groups: Mutex<HashMap<String, String>>,
}

fn failure(error: impl std::fmt::Display) -> AppError {
    AppError::Other(format!("Failed to start the sharing service: {error}"))
}

fn listener_ips(value: &str) -> AppResult<Vec<IpAddr>> {
    let parsed = value
        .split(',')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .map(|part| {
            part.parse::<IpAddr>()
                .map_err(|_| AppError::BadRequest(format!("Invalid bind IP address: {part}")))
        })
        .collect::<AppResult<Vec<_>>>()?;
    let mut seen = HashSet::new();
    let ips = parsed
        .into_iter()
        .filter(|ip| seen.insert(*ip))
        .collect::<Vec<_>>();
    if ips.is_empty() {
        return Err(AppError::BadRequest("At least one bind IP is required".into()));
    }
    Ok(ips)
}

fn listener_group_key(ips: &[IpAddr], port: u16) -> String {
    let mut addresses = ips.iter().map(ToString::to_string).collect::<Vec<_>>();
    addresses.sort();
    format!("{port}|{}", addresses.join(","))
}

fn listener_group_ips(bind_key: &str) -> Vec<IpAddr> {
    let Some((_, addresses)) = bind_key.split_once('|') else {
        return Vec::new();
    };
    addresses
        .split(',')
        .filter_map(|address| address.parse::<IpAddr>().ok())
        .collect()
}

// A share can join a listener group only when every address served by that
// group is explicitly included in the share's bind list. This prevents a
// narrow share from becoming reachable through extra addresses in a broader
// listener group (for example, a specific IPv4 address joining 0.0.0.0).
fn listener_group_covers(bind_key: &str, requested_ips: &[IpAddr]) -> bool {
    let bound_ips = listener_group_ips(bind_key);
    !bound_ips.is_empty() && bound_ips.iter().all(|bound| requested_ips.contains(bound))
}

#[cfg(target_os = "windows")]
fn tcp_listener_owner(port: u16) -> Option<String> {
    use std::{ffi::OsString, os::windows::ffi::OsStringExt, slice};
    use windows::{
        Win32::{
            Foundation::CloseHandle,
            NetworkManagement::IpHelper::{
                GetExtendedTcpTable, MIB_TCP6TABLE_OWNER_PID, MIB_TCPTABLE_OWNER_PID,
                TCP_TABLE_OWNER_PID_LISTENER,
            },
            Networking::WinSock::{AF_INET, AF_INET6},
            System::Threading::{
                OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32,
                PROCESS_QUERY_LIMITED_INFORMATION,
            },
        },
    };

    unsafe fn table_size(family: u32) -> Option<u32> {
        let mut size = 0u32;
        let status = GetExtendedTcpTable(
            None,
            &mut size,
            false,
            family,
            TCP_TABLE_OWNER_PID_LISTENER,
            0,
        );
        if size == 0 || (status != 0 && status != 122) {
            return None;
        }
        Some(size)
    }

    unsafe fn find_ipv4_pid(port: u16) -> Option<u32> {
        let mut size = table_size(AF_INET.0 as u32)?;
        let mut buffer = vec![0u32; (size as usize + 3) / 4];
        let status = GetExtendedTcpTable(
            Some(buffer.as_mut_ptr().cast()),
            &mut size,
            false,
            AF_INET.0 as u32,
            TCP_TABLE_OWNER_PID_LISTENER,
            0,
        );
        if status != 0 {
            return None;
        }
        let table = buffer.as_ptr().cast::<MIB_TCPTABLE_OWNER_PID>();
        let rows = slice::from_raw_parts((*table).table.as_ptr(), (*table).dwNumEntries as usize);
        rows.iter()
            .find(|row| u16::from_be(row.dwLocalPort as u16) == port)
            .map(|row| row.dwOwningPid)
    }

    unsafe fn find_ipv6_pid(port: u16) -> Option<u32> {
        let mut size = table_size(AF_INET6.0 as u32)?;
        let mut buffer = vec![0u32; (size as usize + 3) / 4];
        let status = GetExtendedTcpTable(
            Some(buffer.as_mut_ptr().cast()),
            &mut size,
            false,
            AF_INET6.0 as u32,
            TCP_TABLE_OWNER_PID_LISTENER,
            0,
        );
        if status != 0 {
            return None;
        }
        let table = buffer.as_ptr().cast::<MIB_TCP6TABLE_OWNER_PID>();
        let rows = slice::from_raw_parts((*table).table.as_ptr(), (*table).dwNumEntries as usize);
        rows.iter()
            .find(|row| u16::from_be(row.dwLocalPort as u16) == port)
            .map(|row| row.dwOwningPid)
    }

    let pid = unsafe { find_ipv4_pid(port) }
        .or_else(|| unsafe { find_ipv6_pid(port) })?;
    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()? };
    let mut path = vec![0u16; 32_768];
    let mut length = path.len() as u32;
    let result = unsafe {
        QueryFullProcessImageNameW(
            process,
            PROCESS_NAME_WIN32,
            windows::core::PWSTR(path.as_mut_ptr()),
            &mut length,
        )
    };
    unsafe { let _ = CloseHandle(process); }
    let executable = result.ok().map(|_| {
        OsString::from_wide(&path[..length as usize])
            .to_string_lossy()
            .into_owned()
    });
    Some(match executable {
        Some(executable) => format!("PID {pid} ({executable})"),
        None => format!("PID {pid}"),
    })
}

#[cfg(not(target_os = "windows"))]
fn tcp_listener_owner(_port: u16) -> Option<String> {
    None
}

fn log_port_conflict(port: u16, error: &std::io::Error) {
    match tcp_listener_owner(port) {
        Some(owner) => tracing::warn!(port, %owner, %error, "Listener port is occupied; trying the next port"),
        None => tracing::warn!(port, %error, "Listener port is occupied; owner could not be identified; trying the next port"),
    }
}

fn bind_socket_listener(ip: IpAddr, port: u16, ipv6_only: bool) -> std::io::Result<TcpListener> {
    let domain = if ip.is_ipv4() {
        socket2::Domain::IPV4
    } else {
        socket2::Domain::IPV6
    };
    let socket = socket2::Socket::new(
        domain,
        socket2::Type::STREAM,
        Some(socket2::Protocol::TCP),
    )?;
    if ip.is_ipv6() {
        socket.set_only_v6(ipv6_only)?;
    }
    socket.set_nonblocking(true)?;
    socket.bind(&SocketAddr::new(ip, port).into())?;
    socket.listen(1024)?;
    let listener: std::net::TcpListener = socket.into();
    TcpListener::from_std(listener)
}

async fn bind_tcp_listener_set(
    ips: &[IpAddr],
    start_port: u16,
    increment_on_conflict: bool,
    skip_ports: &[u16],
) -> AppResult<(Vec<TcpListener>, u16)> {
    if ips.is_empty() {
        return Err(AppError::BadRequest(
            "At least one bind IP is required".into(),
        ));
    }
    let mut port = start_port;
    loop {
        let mut listeners = Vec::with_capacity(ips.len());
        let mut selected_port = port;
        let mut conflict = None;
        for ip in ips {
            // Keep address-family binding explicit. A dual-stack IPv6 wildcard
            // socket did not accept IPv4 connections reliably on Windows; when
            // both wildcards are requested, bind one v4 and one v6-only socket.
            let ipv6_only = ip.is_ipv6();
            match bind_socket_listener(*ip, selected_port, ipv6_only) {
                Ok(listener) => {
                    if selected_port == 0 {
                        selected_port = listener.local_addr()?.port();
                    }
                    listeners.push(listener);
                }
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::AddrInUse | std::io::ErrorKind::PermissionDenied
                    ) =>
                {
                    log_port_conflict(selected_port, &error);
                    conflict = Some(error);
                    break;
                }
                Err(error) => return Err(failure(error)),
            }
        }
        if let Some(error) = conflict {
            drop(listeners);
            if !increment_on_conflict || selected_port == u16::MAX {
                return Err(failure(error));
            }
            port = selected_port
                .checked_add(1)
                .ok_or_else(|| failure("No available listener port"))?;
            while skip_ports.contains(&port) {
                port = port
                    .checked_add(1)
                    .ok_or_else(|| failure("No available listener port"))?;
            }
            continue;
        }
        return Ok((listeners, selected_port));
    }
}

fn spawn_http_listeners(
    listeners: Vec<TcpListener>,
    router: axum::Router,
    cancel: CancellationToken,
    error: Arc<Mutex<Option<String>>>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut tasks = tokio::task::JoinSet::new();
        for listener in listeners {
            let app = router.clone();
            tasks.spawn(async move { axum::serve(listener, app).await });
        }
        tokio::select! {
            _ = cancel.cancelled() => {}
            result = tasks.join_next() => {
                if let Some(Ok(Err(err))) = result {
                    *error.lock().await = Some(err.to_string());
                }
            }
        }
        tasks.abort_all();
        while tasks.join_next().await.is_some() {}
    })
}

impl ProtocolManager {
    async fn http_lifecycle_lock(&self, share_id: &str) -> Arc<Mutex<()>> {
        let mut locks = self.http_lifecycle_locks.lock().await;
        locks.retain(|_, lock| lock.strong_count() > 0);
        if let Some(lock) = locks.get(share_id).and_then(Weak::upgrade) {
            return lock;
        }
        let lock = Arc::new(Mutex::new(()));
        locks.insert(share_id.to_owned(), Arc::downgrade(&lock));
        lock
    }

    pub async fn restore(&self, app: Arc<AppService>) -> AppResult<()> {
        for share in super::repository::list_all(&app.db)? {
            match self.start(app.clone(), &share).await {
                Ok(port) if port != share.port => {
                    if let Err(error) =
                        super::repository::set_protocol(&app.db, &share.id, share.protocol, port)
                    {
                        self.stop(&share.id).await;
                        let disable_error =
                            super::repository::set_enabled(&app.db, &share.id, false).err();
                        tracing::error!(
                            share_id = %share.id,
                            %error,
                            ?disable_error,
                            "Failed to persist the restored share port; listener stopped and share disabled"
                        );
                        self.record_restore_error(&share, error.to_string()).await;
                    }
                }
                Ok(_) => {}
                Err(err) => {
                    self.stop(&share.id).await;
                    let disable_error =
                        super::repository::set_enabled(&app.db, &share.id, false).err();
                    tracing::error!(
                        share_id = %share.id,
                        error = %err,
                        ?disable_error,
                        "Failed to restore share listener; share disabled"
                    );
                    self.record_restore_error(&share, err.to_string()).await;
                }
            }
        }
        Ok(())
    }

    async fn record_restore_error(&self, share: &ShareConfig, error: String) {
        match share.protocol {
            ShareProtocol::Http => {
                self.http_shares
                    .lock()
                    .await
                    .insert(share.id.clone(), (share.port, Some(error)));
            }
            ShareProtocol::Smb => {
                self.smb_shares.lock().await.insert(
                    share.id.clone(),
                    SmbShareStatus {
                        protocol_share_name: share.protocol_share_name.clone(),
                        username: share.access_username.clone(),
                        port: share.port,
                        error: Some(error),
                    },
                );
            }
            ShareProtocol::Ftp => {
                self.ftp_status
                    .lock()
                    .await
                    .insert(share.id.clone(), (share.port, Some(error)));
            }
            ShareProtocol::Nodesend => {
                self.running.lock().await.insert(
                    share.id.clone(),
                    Running {
                        cancel: CancellationToken::new(),
                        task: tokio::spawn(async {}),
                        port: share.port,
                        error: Arc::new(Mutex::new(Some(error))),
                    },
                );
            }
        }
    }
    pub async fn stop(&self, id: &str) {
        let smb_status = self.smb_shares.lock().await.remove(id);
        if let Some(share) = smb_status {
            self.smb_share_groups.lock().await.remove(id);
            let stops = {
                let mut groups = self.smb_servers.lock().await;
                let mut keys = Vec::new();
                for (key, group) in groups.iter_mut() {
                    if !group.share_ids.remove(id) {
                        continue;
                    }
                    for config in &group.configs {
                        if let Err(error) = config.remove_share(&share.protocol_share_name).await {
                            tracing::error!(share_id = %id, share_name = %share.protocol_share_name, %error, "Failed to remove SMB share from a listener");
                        }
                    }
                    if let Some(username) = share.username.as_deref() {
                        let remove_user = group.users.get_mut(username).is_some_and(|user| {
                            user.share_ids.remove(id);
                            user.share_ids.is_empty()
                        });
                        if remove_user {
                            let mut removed_from_all = true;
                            for config in &group.configs {
                                if let Err(error) = config.remove_user(username).await {
                                    removed_from_all = false;
                                    tracing::error!(share_id = %id, username = %username, %error, "Failed to remove SMB user from a listener");
                                }
                            }
                            if removed_from_all {
                                group.users.remove(username);
                            }
                        }
                    }
                    if group.share_ids.is_empty() {
                        keys.push(key.clone());
                    }
                }
                keys.into_iter()
                    .filter_map(|key| groups.remove(&key).map(|group| group.service))
                    .collect::<Vec<_>>()
            };
            for service in stops {
                stop_running(service).await;
            }
            return;
        }
        let http_stopped = {
            let lifecycle_lock = self.http_lifecycle_lock(id).await;
            let _http_lifecycle = lifecycle_lock.lock().await;
            let http_status = self.http_shares.lock().await.remove(id);
            if http_status.is_some() {
                self.http_share_groups.lock().await.remove(id);
                self.http_events.lock().await.remove(id);
                let stops = {
                    let mut groups = self.http_servers.lock().await;
                    for group in groups.values() {
                        group.events.unwatch(id);
                        group
                            .allowed_share_ids
                            .write()
                            .unwrap_or_else(|poisoned| poisoned.into_inner())
                            .remove(id);
                    }
                    let keys = groups
                        .iter_mut()
                        .filter_map(|(key, group)| {
                            (group.share_ids.remove(id) && group.share_ids.is_empty())
                                .then(|| key.clone())
                        })
                        .collect::<Vec<_>>();
                    keys.into_iter()
                        .filter_map(|key| groups.remove(&key).map(|group| group.service))
                        .collect::<Vec<_>>()
                };
                if let Ok(mut trees) = self.http_directory_trees.write() {
                    trees.remove(id);
                }
                if let Ok(mut handlers) = self.http_dav_handlers.write() {
                    handlers.remove(id);
                }
                for service in stops {
                    stop_running(service).await;
                }
                true
            } else {
                false
            }
        };
        if http_stopped {
            return;
        }
        let ftp_status = self.ftp_status.lock().await.remove(id);
        if ftp_status.is_some() {
            self.ftp_share_groups.lock().await.remove(id);
            let stops = {
                let mut groups = self.ftp_servers.lock().await;
                let keys = groups
                    .iter_mut()
                    .filter_map(|(key, group)| {
                        if !group.share_ids.remove(id) {
                            return None;
                        }
                        group
                            .auth_share_ids
                            .write()
                            .unwrap_or_else(|error| error.into_inner())
                            .remove(id);
                        group.share_ids.is_empty().then(|| key.clone())
                    })
                    .collect::<Vec<_>>();
                keys.into_iter()
                    .filter_map(|key| groups.remove(&key).map(|group| group.service))
                    .collect::<Vec<_>>()
            };
            self.ftp_shares
                .write()
                .unwrap_or_else(|error| error.into_inner())
                .remove(id);
            for service in stops {
                stop_running(service).await;
            }
            return;
        }
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
        drop(running);
        let smb_share_groups = self.smb_share_groups.lock().await;
        let smb_servers = self.smb_servers.lock().await;
        for (id, share) in self.smb_shares.lock().await.iter() {
            let server = smb_share_groups.get(id).and_then(|key| smb_servers.get(key));
            let additional_servers = smb_servers
                .values()
                .filter(|group| group.share_ids.contains(id))
                .collect::<Vec<_>>();
            result.push(ProtocolStatus {
                id: id.clone(),
                running: server.is_some_and(|group| !group.service.task.is_finished())
                    && additional_servers
                        .iter()
                        .all(|group| !group.service.task.is_finished())
                    && share.error.is_none(),
                port: share.port,
                error: share.error.clone().or_else(|| {
                    server
                        .and_then(|group| group.service.error.try_lock().ok()?.clone())
                        .or_else(|| {
                            additional_servers.iter().find_map(|group| {
                                group.service.error.try_lock().ok()?.clone()
                            })
                        })
                }),
            });
        }
        drop(smb_servers);
        drop(smb_share_groups);
        let http_share_groups = self.http_share_groups.lock().await;
        let http_servers = self.http_servers.lock().await;
        for (id, (port, error)) in self.http_shares.lock().await.iter() {
            let server = http_share_groups.get(id).and_then(|key| http_servers.get(key));
            let additional_servers = http_servers
                .values()
                .filter(|group| group.share_ids.contains(id))
                .collect::<Vec<_>>();
            result.push(ProtocolStatus {
                id: id.clone(),
                running: server.is_some_and(|group| !group.service.task.is_finished())
                    && additional_servers
                        .iter()
                        .all(|group| !group.service.task.is_finished())
                    && error.is_none(),
                port: *port,
                error: error.clone().or_else(|| {
                    server
                        .and_then(|group| group.service.error.try_lock().ok()?.clone())
                        .or_else(|| {
                            additional_servers.iter().find_map(|group| {
                                group.service.error.try_lock().ok()?.clone()
                            })
                        })
                }),
            });
        }
        drop(http_servers);
        drop(http_share_groups);
        let ftp_share_groups = self.ftp_share_groups.lock().await;
        let ftp_servers = self.ftp_servers.lock().await;
        for (id, (port, error)) in self.ftp_status.lock().await.iter() {
            let server = ftp_share_groups.get(id).and_then(|key| ftp_servers.get(key));
            let additional_servers = ftp_servers
                .values()
                .filter(|group| group.share_ids.contains(id))
                .collect::<Vec<_>>();
            result.push(ProtocolStatus {
                id: id.clone(),
                running: server.is_some_and(|group| !group.service.task.is_finished())
                    && additional_servers
                        .iter()
                        .all(|group| !group.service.task.is_finished())
                    && error.is_none(),
                port: *port,
                error: error.clone().or_else(|| {
                    server
                        .and_then(|group| group.service.error.try_lock().ok()?.clone())
                        .or_else(|| {
                            additional_servers.iter().find_map(|group| {
                                group.service.error.try_lock().ok()?.clone()
                            })
                        })
                }),
            });
        }
        result
    }

    pub async fn start(&self, app: Arc<AppService>, share: &ShareConfig) -> AppResult<u16> {
        if share.protocol == ShareProtocol::Nodesend || !share.enabled {
            return Ok(share.port);
        }
        if share.access_mode == AccessMode::Trusted {
            return Err(AppError::BadRequest(
                "Third-party protocols do not support NodeSend trusted identity; use password access.".into(),
            ));
        }
        if share.access_mode == AccessMode::Token
            && !matches!(share.protocol, ShareProtocol::Http)
        {
            return Err(AppError::BadRequest(
                "Token access is supported only for NodeSend and HTTP shares.".into(),
            ));
        }
        if share.protocol == ShareProtocol::Http {
            return self.start_http(app, share).await;
        }
        if share.protocol == ShareProtocol::Smb {
            return self.start_smb(share).await;
        }
        if share.protocol == ShareProtocol::Ftp {
            return self.start_ftp(app, share).await;
        }
        unreachable!("all share protocols are handled by their protocol managers")
    }

    async fn start_ftp(&self, app: Arc<AppService>, share: &ShareConfig) -> AppResult<u16> {
        let filesystem = unftp_sbe_fs::Filesystem::new(share.path_ref()).map_err(failure)?;
        let ips = listener_ips(&share.bind_ips)?;
        let requested_port = if share.requested_port == 0 {
            FTP_CONTROL_PORT
        } else {
            share.requested_port
        };
        let bind_key = listener_group_key(&ips, 0);
        let joined = {
            let mut groups = self.ftp_servers.lock().await;
            let matching_key = groups
                .iter()
                .find(|(_, group)| {
                    group.bind_key == bind_key
                        && (!share.port_explicit || group.service.port == requested_port)
                })
                .map(|(key, _)| key.clone());
            if let Some(matching_key) = matching_key {
                let group = groups.get_mut(&matching_key).expect("matched FTP group exists");
                self.ftp_shares
                    .write()
                    .unwrap_or_else(|error| error.into_inner())
                    .insert(
                        share.id.clone(),
                        super::ftp_fs::FtpShare {
                            id: share.id.clone(),
                            name: share.protocol_share_name.clone(),
                            filesystem: filesystem.clone(),
                        },
                    );
                group.share_ids.insert(share.id.clone());
                group
                    .auth_share_ids
                    .write()
                    .unwrap_or_else(|error| error.into_inner())
                    .insert(share.id.clone());
                let port = group.service.port;
                Some((matching_key, port))
            } else {
                None
            }
        };
        if let Some((group_key, port)) = joined {
            self.ftp_share_groups.lock().await.insert(share.id.clone(), group_key);
            self.ftp_status.lock().await.insert(share.id.clone(), (port, None));
            return Ok(port);
        }
        // FTP shares on different addresses can listen on the same port. Keep
        // the protocol's effective port consistent instead of independently
        // incrementing from 21 for every new bind-address group.
        let preferred_port = if share.port_explicit {
            requested_port
        } else {
            let groups = self.ftp_servers.lock().await;
            groups
                .values()
                .filter(|group| !group.port_explicit)
                .min_by_key(|group| group.service.port)
                .or_else(|| groups.values().min_by_key(|group| group.service.port))
                .map(|group| group.service.port)
                .unwrap_or(requested_port)
        };
        let (covered_group_keys, covered_ips) = {
            let groups = self.ftp_servers.lock().await;
            let mut keys = Vec::new();
            let mut covered = HashSet::new();
            for (key, group) in groups.iter().filter(|(_, group)| group.service.port == preferred_port) {
                let group_ips = listener_group_ips(&group.bind_key);
                if listener_group_covers(&group.bind_key, &ips) {
                    keys.push(key.clone());
                    covered.extend(group_ips);
                }
            }
            (keys, covered)
        };
        let uncovered_ips = ips
            .iter()
            .copied()
            .filter(|ip| !covered_ips.contains(ip))
            .collect::<Vec<_>>();
        if uncovered_ips.is_empty() && !covered_group_keys.is_empty() {
            self.ftp_shares
                .write()
                .unwrap_or_else(|error| error.into_inner())
                .insert(
                    share.id.clone(),
                    super::ftp_fs::FtpShare {
                        id: share.id.clone(),
                        name: share.protocol_share_name.clone(),
                        filesystem: filesystem.clone(),
                    },
                );
            let mut groups = self.ftp_servers.lock().await;
            for key in &covered_group_keys {
                if let Some(group) = groups.get_mut(key) {
                    group.share_ids.insert(share.id.clone());
                    group
                        .auth_share_ids
                        .write()
                        .unwrap_or_else(|error| error.into_inner())
                        .insert(share.id.clone());
                }
            }
            drop(groups);
            self.ftp_share_groups
                .lock()
                .await
                .insert(share.id.clone(), covered_group_keys[0].clone());
            self.ftp_status.lock().await.insert(share.id.clone(), (preferred_port, None));
            return Ok(preferred_port);
        }
        let (reservation_ips, reservations, port, covered_group_keys) =
            if uncovered_ips.len() < ips.len() {
                match bind_tcp_listener_set(&uncovered_ips, preferred_port, false, FTP_COMMON_PORTS).await {
                    Ok((reservations, port)) => {
                        (uncovered_ips.clone(), reservations, port, covered_group_keys)
                    }
                    Err(_) => {
                        let (reservations, port) =
                            bind_tcp_listener_set(&ips, preferred_port, true, FTP_COMMON_PORTS).await?;
                        (ips.clone(), reservations, port, Vec::new())
                    }
                }
            } else {
                let (reservations, port) =
                    bind_tcp_listener_set(&ips, preferred_port, true, FTP_COMMON_PORTS).await?;
                (ips.clone(), reservations, port, Vec::new())
            };
        drop(reservations);
        let shares = self.ftp_shares.clone();
        let auth_share_ids = Arc::new(RwLock::new(HashSet::from([share.id.clone()])));
        let cancel = CancellationToken::new();
        let error = Arc::new(Mutex::new(None));
        let mut servers = Vec::with_capacity(reservation_ips.len());
        for ip in &reservation_ips {
            let server_shares = shares.clone();
            let signal = cancel.clone();
            let server = libunftp::ServerBuilder::with_user_detail_provider(
                Box::new(move || super::ftp_fs::FtpMultiplexFs::new(server_shares.clone())),
                Arc::new(super::ftp_fs::FtpUserDetailProvider),
            )
            .authenticator(Arc::new(FtpAuth {
                app: app.clone(),
                share_ids: auth_share_ids.clone(),
            }))
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
            servers.push((server, *ip));
        }
        shares
            .write()
            .unwrap_or_else(|error| error.into_inner())
            .insert(
                share.id.clone(),
                super::ftp_fs::FtpShare {
                    id: share.id.clone(),
                    name: share.protocol_share_name.clone(),
                    filesystem,
                },
            );
        let task_cancel = cancel.clone();
        let task_error = error.clone();
        let task = tokio::spawn(async move {
            let mut listeners = tokio::task::JoinSet::new();
            for (server, ip) in servers {
                let server_error = task_error.clone();
                listeners.spawn(async move {
                    if let Err(err) = server.listen(SocketAddr::new(ip, port).to_string()).await {
                        *server_error.lock().await = Some(err.to_string());
                    }
                });
            }
            tokio::select! {
                _ = task_cancel.cancelled() => {}
                result = listeners.join_next() => {
                    if let Some(Err(err)) = result {
                        *task_error.lock().await = Some(err.to_string());
                    }
                }
            }
            listeners.abort_all();
            while listeners.join_next().await.is_some() {}
        });
        let probe_ips = reservation_ips.clone();
        let ready = tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                if task.is_finished() {
                    return false;
                }
                let mut all_ready = true;
                for ip in &probe_ips {
                    let probe_ip = match ip {
                        IpAddr::V4(ip) if ip.is_unspecified() => IpAddr::V4(std::net::Ipv4Addr::LOCALHOST),
                        IpAddr::V6(ip) if ip.is_unspecified() => IpAddr::V6(std::net::Ipv6Addr::LOCALHOST),
                        _ => *ip,
                    };
                    if let Ok(mut stream) = TcpStream::connect(SocketAddr::new(probe_ip, port)).await {
                        let mut greeting = [0; 128];
                        if let Ok(Ok(n)) = tokio::time::timeout(
                            Duration::from_millis(200),
                            stream.read(&mut greeting),
                        )
                        .await
                        {
                            if String::from_utf8_lossy(&greeting[..n]).contains("NodeSend FTP ready") {
                                continue;
                            }
                        }
                    }
                    all_ready = false;
                    break;
                }
                if all_ready {
                    return true;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap_or(false);
        if !ready {
            cancel.cancel();
            let _ = task.await;
            self.ftp_shares
                .write()
                .unwrap_or_else(|error| error.into_inner())
                .remove(&share.id);
            return Err(failure(
                error
                    .lock()
                    .await
                    .clone()
                    .unwrap_or_else(|| "FTP listener is not ready".into()),
            ));
        }
        let group_key = listener_group_key(&reservation_ips, port);
        let listener_bind_key = listener_group_key(&reservation_ips, 0);
        self.ftp_servers.lock().await.insert(group_key.clone(), FtpGroup {
            service: Running { cancel, task, port, error },
            share_ids: HashSet::from([share.id.clone()]),
            auth_share_ids,
            bind_key: listener_bind_key,
            port_explicit: share.port_explicit,
        });
        if !covered_group_keys.is_empty() {
            let mut groups = self.ftp_servers.lock().await;
            for key in &covered_group_keys {
                if let Some(group) = groups.get_mut(key) {
                    group.share_ids.insert(share.id.clone());
                    group
                        .auth_share_ids
                        .write()
                        .unwrap_or_else(|error| error.into_inner())
                        .insert(share.id.clone());
                }
            }
        }
        self.ftp_share_groups
            .lock()
            .await
            .insert(share.id.clone(), covered_group_keys.first().cloned().unwrap_or(group_key));
        self.ftp_status.lock().await.insert(share.id.clone(), (port, None));
        Ok(port)
    }

    async fn start_smb(&self, share: &ShareConfig) -> AppResult<u16> {
        let username = (share.access_mode == AccessMode::Password)
            .then(|| share.access_username.as_deref().unwrap_or_default().to_owned());
        let password = (share.access_mode == AccessMode::Password)
            .then(|| share.access_token.as_deref().unwrap_or_default().to_owned());
        let ips = listener_ips(&share.bind_ips)?;
        let requested_port = if share.requested_port == 0 { 445 } else { share.requested_port };
        let bind_key = listener_group_key(&ips, 0);
        let joined = {
            let mut groups = self.smb_servers.lock().await;
            let matching_key = groups
                .iter()
                .find(|(_, group)| {
                    group.bind_key == bind_key
                        && (!share.port_explicit || group.service.port == requested_port)
                })
                .map(|(key, _)| key.clone());
            if let Some(matching_key) = matching_key {
                let port = groups
                    .get(&matching_key)
                    .expect("matched SMB group exists")
                    .service
                    .port;
                add_smb_share_to_groups(
                    &mut groups,
                    std::slice::from_ref(&matching_key),
                    share,
                    username.as_deref(),
                    password.as_deref(),
                )
                .await?;
                Some((matching_key, port))
            } else {
                None
            }
        };
        if let Some((group_key, port)) = joined {
            self.smb_share_groups.lock().await.insert(share.id.clone(), group_key);
            self.smb_shares.lock().await.insert(
                share.id.clone(),
                SmbShareStatus {
                    protocol_share_name: share.protocol_share_name.clone(),
                    username: username.clone(),
                    port,
                    error: None,
                },
            );
            return Ok(port);
        }
        let preferred_port = if share.port_explicit {
            requested_port
        } else {
            let groups = self.smb_servers.lock().await;
            groups
                .values()
                .filter(|group| !group.port_explicit)
                .min_by_key(|group| group.service.port)
                .or_else(|| groups.values().min_by_key(|group| group.service.port))
                .map(|group| group.service.port)
                .unwrap_or(requested_port)
        };
        let (covered_group_keys, covered_ips) = {
            let groups = self.smb_servers.lock().await;
            let mut keys = Vec::new();
            let mut covered = HashSet::new();
            for (key, group) in groups.iter().filter(|(_, group)| group.service.port == preferred_port) {
                let group_ips = listener_group_ips(&group.bind_key);
                if listener_group_covers(&group.bind_key, &ips) {
                    if let (Some(username), Some(password)) = (username.as_deref(), password.as_deref()) {
                        if group.users.get(username).is_some_and(|existing| existing.password.as_str() != password) {
                            return Err(AppError::Conflict("SMB_USERNAME_PASSWORD_CONFLICT".into()));
                        }
                    }
                    keys.push(key.clone());
                    covered.extend(group_ips);
                }
            }
            (keys, covered)
        };
        let uncovered_ips = ips.iter().copied().filter(|ip| !covered_ips.contains(ip)).collect::<Vec<_>>();
        if uncovered_ips.is_empty() && !covered_group_keys.is_empty() {
            let mut groups = self.smb_servers.lock().await;
            add_smb_share_to_groups(
                &mut groups,
                &covered_group_keys,
                share,
                username.as_deref(),
                password.as_deref(),
            )
            .await?;
            drop(groups);
            self.smb_share_groups.lock().await.insert(share.id.clone(), covered_group_keys[0].clone());
            self.smb_shares.lock().await.insert(
                share.id.clone(),
                SmbShareStatus { protocol_share_name: share.protocol_share_name.clone(), username: username.clone(), port: preferred_port, error: None },
            );
            return Ok(preferred_port);
        }
        let mut attempt_ips = if uncovered_ips.len() < ips.len() { uncovered_ips.clone() } else { ips.clone() };
        let mut use_covered_groups = !covered_group_keys.is_empty();
        let mut attempt_port = preferred_port;
        let (servers, port) = loop {
            let mut servers = Vec::with_capacity(attempt_ips.len());
            let mut selected_port = attempt_port;
            let mut bind_error = None;
            for ip in &attempt_ips {
                let backend = smb_server::LocalFsBackend::new(&share.path).map_err(failure)?;
                let mut builder = smb_server::SmbServer::builder()
                    .listen(SocketAddr::new(*ip, selected_port))
                    .netbios_name("NODESEND");
                if let (Some(username), Some(password)) = (username.as_deref(), password.as_deref()) {
                    builder = builder.user(username, password);
                }
                let mut smb_share = smb_server::Share::new(&share.protocol_share_name, backend);
                if share.access_mode == AccessMode::Open {
                    smb_share = smb_share.public();
                } else if let Some(username) = username.as_deref() {
                    smb_share = smb_share.user(username, smb_server::Access::ReadWrite);
                }
                let server = builder.share(smb_share).build().map_err(failure)?;
                match server.bind().await {
                    Ok(address) => {
                        selected_port = address.port();
                        servers.push(server);
                    }
                    Err(error)
                        if matches!(error.kind(), std::io::ErrorKind::AddrInUse | std::io::ErrorKind::PermissionDenied) =>
                    {
                        log_port_conflict(selected_port, &error);
                        bind_error = Some(error);
                        break;
                    }
                    Err(error) => return Err(failure(error)),
                }
            }
            if let Some(error) = bind_error {
                drop(servers);
                if selected_port == u16::MAX {
                    return Err(failure(error));
                }
                if use_covered_groups {
                    attempt_ips = ips.clone();
                    use_covered_groups = false;
                }
                attempt_port = selected_port.saturating_add(1);
                continue;
            }
            break (servers, selected_port);
        };
        let cancel = CancellationToken::new();
        let error = Arc::new(Mutex::new(None));
        let task_cancel = cancel.clone();
        let task_error = error.clone();
        let configs = servers
            .iter()
            .map(|server| server.config_handle())
            .collect::<Vec<_>>();
        let task = tokio::spawn(async move {
            let mut listeners = tokio::task::JoinSet::new();
            for server in servers {
                let signal = task_cancel.clone();
                let server_error = task_error.clone();
                listeners.spawn(async move {
                    let shutdown = server.shutdown_handle();
                    tokio::select! {
                        result = server.serve() => if let Err(err) = result { *server_error.lock().await = Some(err.to_string()); },
                        _ = signal.cancelled() => shutdown.shutdown(),
                    }
                });
            }
            tokio::select! {
                _ = task_cancel.cancelled() => {}
                result = listeners.join_next() => {
                    if let Some(Err(err)) = result {
                        *task_error.lock().await = Some(err.to_string());
                    }
                }
            }
            listeners.abort_all();
            while listeners.join_next().await.is_some() {}
        });
        self.smb_shares.lock().await.insert(
            share.id.clone(),
            SmbShareStatus {
                protocol_share_name: share.protocol_share_name.clone(),
                username: username.clone(),
                port,
                error: None,
            },
        );
        let mut users = HashMap::new();
        if let (Some(username), Some(password)) = (username.as_ref(), password.as_ref()) {
            users.insert(
                username.clone(),
                SmbUser {
                    password: password.clone(),
                    share_ids: HashSet::from([share.id.clone()]),
                },
            );
        }
        let listener_ips = attempt_ips;
        let listener_bind_key = listener_group_key(&listener_ips, 0);
        let group_key = listener_group_key(&listener_ips, port);
        self.smb_servers.lock().await.insert(group_key.clone(), SmbGroup {
            service: Running { cancel, task, port, error },
            share_ids: HashSet::from([share.id.clone()]),
            configs,
            users,
            bind_key: listener_bind_key,
            port_explicit: share.port_explicit,
        });
        if use_covered_groups {
            let mut groups = self.smb_servers.lock().await;
            let result = add_smb_share_to_groups(
                &mut groups,
                &covered_group_keys,
                share,
                username.as_deref(),
                password.as_deref(),
            )
            .await;
            drop(groups);
            if let Err(error) = result {
                self.stop(&share.id).await;
                return Err(error);
            }
        }
        self.smb_share_groups
            .lock()
            .await
            .insert(share.id.clone(), covered_group_keys.first().cloned().filter(|_| use_covered_groups).unwrap_or(group_key));
        Ok(port)
    }

    async fn start_http(&self, app: Arc<AppService>, share: &ShareConfig) -> AppResult<u16> {
        let lifecycle_lock = self.http_lifecycle_lock(&share.id).await;
        let _http_lifecycle = lifecycle_lock.lock().await;
        let root = share.path_ref().to_path_buf();
        let snapshot =
            tokio::task::spawn_blocking(move || crate::httpapi::tree::build_directory_tree(&root))
                .await
                .map_err(failure)??;
        let tree = Arc::new(RwLock::new(snapshot));
        let dav_handler = dav_server::DavHandler::builder()
            .filesystem(Box::new(super::webdav_fs::GuardedFs::new(share.path_ref())?))
            .locksystem(dav_server::memls::MemLs::new())
            .build_handler();
        let ips = listener_ips(&share.bind_ips)?;
        // Creation chooses 8193 in create_share_service. A zero port means
        // automatic selection from the same protocol's default start port.
        let requested_port = if share.requested_port == 0 { 8193 } else { share.requested_port };
        let bind_key = listener_group_key(&ips, 0);
        let joined = {
            let mut groups = self.http_servers.lock().await;
            let matching_key = groups
                .iter()
                .find(|(_, group)| {
                    group.bind_key == bind_key
                        && (!share.port_explicit || group.service.port == requested_port)
                })
                .map(|(key, _)| key.clone());
            if let Some(matching_key) = matching_key {
                let group = groups.get_mut(&matching_key).expect("matched HTTP group exists");
                self.http_directory_trees
                    .write()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .insert(share.id.clone(), tree.clone());
                self.http_dav_handlers
                    .write()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .insert(share.id.clone(), dav_handler.clone());
                group.events.watch(share, Some(tree.clone()));
                group.share_ids.insert(share.id.clone());
                group
                    .allowed_share_ids
                    .write()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .insert(share.id.clone());
                let port = group.service.port;
                Some((matching_key, port, group.events.clone()))
            } else {
                None
            }
        };
        if let Some((group_key, port, events)) = joined {
            self.http_events.lock().await.insert(share.id.clone(), events);
            self.http_share_groups.lock().await.insert(share.id.clone(), group_key);
            self.http_shares.lock().await.insert(share.id.clone(), (port, None));
            return Ok(port);
        }
        let preferred_port = if share.port_explicit {
            requested_port
        } else {
            let groups = self.http_servers.lock().await;
            groups
                .values()
                .filter(|group| !group.port_explicit)
                .min_by_key(|group| group.service.port)
                .or_else(|| groups.values().min_by_key(|group| group.service.port))
                .map(|group| group.service.port)
                .unwrap_or(requested_port)
        };
        let (covered_group_keys, covered_ips) = {
            let groups = self.http_servers.lock().await;
            let mut keys = Vec::new();
            let mut covered = HashSet::new();
            for (key, group) in groups.iter().filter(|(_, group)| group.service.port == preferred_port) {
                let group_ips = listener_group_ips(&group.bind_key);
                if listener_group_covers(&group.bind_key, &ips) {
                    keys.push(key.clone());
                    covered.extend(group_ips);
                }
            }
            (keys, covered)
        };
        let uncovered_ips = ips
            .iter()
            .copied()
            .filter(|ip| !covered_ips.contains(ip))
            .collect::<Vec<_>>();
        if uncovered_ips.is_empty() && !covered_group_keys.is_empty() {
            self.http_directory_trees
                .write()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .insert(share.id.clone(), tree.clone());
            self.http_dav_handlers
                .write()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .insert(share.id.clone(), dav_handler.clone());
            let mut groups = self.http_servers.lock().await;
            let mut primary_events = None;
            for key in &covered_group_keys {
                if let Some(group) = groups.get_mut(key) {
                    group.events.watch(share, Some(tree.clone()));
                    group.share_ids.insert(share.id.clone());
                    group
                        .allowed_share_ids
                        .write()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .insert(share.id.clone());
                    primary_events.get_or_insert_with(|| group.events.clone());
                }
            }
            drop(groups);
            if let Some(events) = primary_events {
                self.http_events.lock().await.insert(share.id.clone(), events);
                self.http_share_groups
                    .lock()
                    .await
                    .insert(share.id.clone(), covered_group_keys[0].clone());
                self.http_shares.lock().await.insert(share.id.clone(), (preferred_port, None));
                return Ok(preferred_port);
            }
        }
        let (listeners, listener_ips, port, covered_group_keys) = if uncovered_ips.len() < ips.len() {
            match bind_tcp_listener_set(&uncovered_ips, preferred_port, false, &[]).await {
                Ok((listeners, port)) => (listeners, uncovered_ips.clone(), port, covered_group_keys),
                Err(_) => {
                    let (listeners, port) = bind_tcp_listener_set(&ips, preferred_port, true, &[]).await?;
                    (listeners, ips.clone(), port, Vec::new())
                }
            }
        } else {
            let (listeners, port) = bind_tcp_listener_set(&ips, preferred_port, true, &[]).await?;
            (listeners, ips.clone(), port, Vec::new())
        };
        if port != preferred_port {
            tracing::warn!(preferred_port, port, share_id = %share.id, "Could not extend the existing HTTP listener at its current port; started this share on the next available port");
        }
        let group_key = listener_group_key(&listener_ips, port);
        let listener_bind_key = listener_group_key(&listener_ips, 0);
        self.http_directory_trees
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(share.id.clone(), tree.clone());
        self.http_dav_handlers
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(share.id.clone(), dav_handler);
        let allowed_share_ids = Arc::new(RwLock::new(HashSet::from([share.id.clone()])));
        let state = crate::httpapi::HttpState::new(
            Arc::clone(&app.db),
            Arc::clone(&app.identity),
            &app.paths.data_dir,
        )
        .scoped_to_protocol(ShareProtocol::Http)
        .with_directory_trees(self.http_directory_trees.clone())
        .with_dav_handlers(self.http_dav_handlers.clone())
        .with_allowed_share_ids(allowed_share_ids.clone());
        state.events.watch(share, Some(tree.clone()));
        self.http_events
            .lock()
            .await
            .insert(share.id.clone(), state.events.clone());
        let router = crate::httpapi::handlers::router(state);
        let cancel = CancellationToken::new();
        let error = Arc::new(Mutex::new(None));
        let task = spawn_http_listeners(listeners, router, cancel.clone(), error.clone());
        let events = self.http_events.lock().await.get(&share.id).cloned()
            .expect("HTTP event state was inserted");
        self.http_servers.lock().await.insert(group_key.clone(), HttpGroup {
            service: Running { cancel, task, port, error },
            share_ids: HashSet::from([share.id.clone()]),
            allowed_share_ids,
            events,
            bind_key: listener_bind_key,
            port_explicit: share.port_explicit,
        });
        if !covered_group_keys.is_empty() {
            let mut groups = self.http_servers.lock().await;
            for key in &covered_group_keys {
                if let Some(group) = groups.get_mut(key) {
                    group.events.watch(share, Some(tree.clone()));
                    group.share_ids.insert(share.id.clone());
                    group
                        .allowed_share_ids
                        .write()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .insert(share.id.clone());
                }
            }
        }
        self.http_share_groups
            .lock()
            .await
            .insert(share.id.clone(), covered_group_keys.first().cloned().unwrap_or(group_key));
        self.http_shares.lock().await.insert(share.id.clone(), (port, None));
        Ok(port)
    }
}

async fn stop_running(mut service: Running) {
    service.cancel.cancel();
    if tokio::time::timeout(Duration::from_secs(5), &mut service.task)
        .await
        .is_err()
    {
        service.task.abort();
        let _ = service.task.await;
    }
}

struct FtpAuth {
    app: Arc<AppService>,
    share_ids: Arc<RwLock<HashSet<String>>>,
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
        let share_ids = self
            .share_ids
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .clone();
        let shares = super::repository::list_all(&self.app.db)
            .map_err(|_| AuthenticationError::BadPassword)?;
        let mut allowed_share_ids = Vec::new();
        for share in shares
            .into_iter()
            .filter(|share| {
                share_ids.contains(&share.id)
                    && share.enabled
                    && share.protocol == ShareProtocol::Ftp
            })
        {
            // FTP authenticates before it sends CWD/LIST, so the server cannot
            // use the URL path to select a share during USER/PASS. Bind the FTP
            // username to exactly one virtual share instead: password shares
            // use their configured username, while open shares use the share
            // path name as an account selector.
            let username_matches = match share.access_mode {
                AccessMode::Password => {
                    username == share.access_username.as_deref().unwrap_or_default()
                }
                AccessMode::Open => username.eq_ignore_ascii_case(&share.protocol_share_name),
                _ => false,
            };
            if !username_matches {
                continue;
            }
            if crate::access::authorize(
                &share,
                &crate::access::Caller::new(None, credentials.password.clone()),
                &self.app.db,
            )
            .is_ok()
            {
                allowed_share_ids.push(share.id);
            }
        }
        // A USER/PASS pair must never authenticate multiple virtual shares;
        // otherwise the client's later path would not be the authority boundary.
        if allowed_share_ids.len() != 1 {
            return Err(AuthenticationError::BadPassword);
        }
        Ok(Principal {
            username: super::ftp_fs::principal_name(allowed_share_ids),
        })
    }
}
