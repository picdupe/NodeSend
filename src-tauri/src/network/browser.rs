use super::{
    peers::{Endpoint, PeerStore},
    protocol::{NativeEntry, Request, Response},
    transport::{self, Channel},
};
use crate::{
    access::{authorize, Caller},
    error::{AppError, AppResult},
    httpapi::security,
    service::AppService,
    share::{repository, ShareConfig},
    transfer::paths,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    sync::Arc,
};
use tokio::{
    fs,
    io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt},
};
use tokio_util::sync::CancellationToken;

pub async fn connect(app: &AppService, peers: &PeerStore, node_id: &str) -> AppResult<Channel> {
    let peer = peers.get(node_id)?;
    let mut last = String::from("NodeSend device has no available address");
    for endpoint in &peer.endpoints {
        match transport::connect(app, endpoint, Some(node_id), false, false).await {
            Ok(mut channel) => match transport::hello(&mut channel, app).await {
                Ok(_) => return Ok(channel),
                Err(err) => last = err.to_string(),
            },
            Err(err) => last = err.to_string(),
        }
    }
    Err(AppError::Other(last))
}

pub async fn list(
    app: &AppService,
    peers: &PeerStore,
    node_id: &str,
    path: &str,
    token: Option<&str>,
) -> AppResult<Vec<NativeEntry>> {
    let mut channel = connect(app, peers, node_id).await?;
    let (share_id, relative) = split_path(path);
    match channel
        .request(&Request::Browse {
            share_id: share_id.map(str::to_string),
            path: relative.to_string(),
            token: token.map(str::to_string),
        })
        .await?
    {
        Response::Listing { entries } => Ok(entries),
        _ => Err(AppError::Other(
            "NodeSend directory response is invalid".into(),
        )),
    }
}

pub async fn download(
    app: &AppService,
    peers: &PeerStore,
    node_id: &str,
    path: &str,
    token: Option<&str>,
    destination: &str,
) -> AppResult<()> {
    let (share_id, relative) = split_path(path);
    let share_id =
        share_id.ok_or_else(|| AppError::BadRequest("Select a file in the share".into()))?;
    if relative.is_empty() {
        return Err(AppError::BadRequest("Select a file in the share".into()));
    }
    let mut channel = connect(app, peers, node_id).await?;
    let length = match channel
        .request(&Request::Fetch {
            share_id: share_id.to_string(),
            path: relative.to_string(),
            token: token.map(str::to_string),
            offset: 0,
        })
        .await?
    {
        Response::File { length } => length,
        _ => return Err(AppError::Other("NodeSend file response is invalid".into())),
    };
    let mut file = fs::File::create(destination).await?;
    let copied = tokio::io::copy(&mut channel.reader.take(length), &mut file).await?;
    if copied != length {
        return Err(AppError::Other(
            "NodeSend download was interrupted; the file is incomplete".into(),
        ));
    }
    file.flush().await?;
    Ok(())
}

pub async fn upload(
    app: &AppService,
    peers: &PeerStore,
    node_id: &str,
    path: &str,
    token: Option<&str>,
    local: &str,
) -> AppResult<()> {
    let (share_id, relative) = split_path(path);
    let share_id =
        share_id.ok_or_else(|| AppError::BadRequest("Enter the target share first".into()))?;
    if relative.is_empty() {
        return Err(AppError::BadRequest(
            "Target file path cannot be empty".into(),
        ));
    }
    let mut file = fs::File::open(local).await?;
    let length = file.metadata().await?.len();
    let mut channel = connect(app, peers, node_id).await?;
    match channel
        .request(&Request::Put {
            share_id: share_id.to_string(),
            path: relative.to_string(),
            length,
            token: token.map(str::to_string),
        })
        .await?
    {
        Response::Ready => {}
        _ => {
            return Err(AppError::Other(
                "NodeSend upload response is invalid".into(),
            ))
        }
    }
    let copied = tokio::io::copy(&mut file, &mut channel.writer).await?;
    if copied != length {
        return Err(AppError::Other("Local file changed during upload".into()));
    }
    channel.writer.flush().await?;
    match channel.response().await? {
        Response::Ack => Ok(()),
        _ => Err(AppError::Other(
            "NodeSend upload confirmation is invalid".into(),
        )),
    }
}

fn split_path(path: &str) -> (Option<&str>, &str) {
    match path.split_once('/') {
        Some((share, relative)) if !share.is_empty() => (Some(share), relative),
        _ if path.is_empty() => (None, ""),
        _ => (Some(path), ""),
    }
}

fn checked_share(
    app: &AppService,
    peer_id: &str,
    share_id: &str,
    token: Option<String>,
) -> AppResult<ShareConfig> {
    let share = repository::get_by_id(&app.db, share_id)?;
    if !matches!(
        share.protocol,
        crate::share::ShareProtocol::Nodesend | crate::share::ShareProtocol::Http
    ) {
        return Err(AppError::NotFound(
            "This share does not support NodeSend downloads".into(),
        ));
    }
    authorize(
        &share,
        &Caller::new(Some(peer_id.to_string()), token),
        &app.db,
    )?;
    Ok(share)
}

pub fn browse(
    app: &AppService,
    peer_id: &str,
    share_id: Option<String>,
    path: String,
    token: Option<String>,
) -> AppResult<Response> {
    let Some(share_id) = share_id else {
        let entries = repository::list_all(&app.db)?
            .into_iter()
            .filter(|share| {
                share.protocol == crate::share::ShareProtocol::Nodesend && share.enabled
            })
            .map(|share| NativeEntry {
                name: share.name,
                path: share.id,
                is_dir: true,
                size: 0,
                modified: None,
            })
            .collect();
        return Ok(Response::Listing { entries });
    };
    let share = checked_share(app, peer_id, &share_id, token)?;
    if !path.is_empty() {
        paths::validate_relative(&path)?;
    }
    let real = security::resolve_existing(share.path_ref(), &path)?;
    if !real.is_dir() {
        return Err(AppError::BadRequest(
            "Request path is not a directory".into(),
        ));
    }
    let tree = crate::httpapi::tree::build_tree(&real, &path, 1)?;
    let entries = tree
        .children
        .unwrap_or_default()
        .into_iter()
        .map(|node| NativeEntry {
            name: node.name,
            path: format!("{share_id}/{}", node.path),
            is_dir: node.node_type == crate::httpapi::tree::NodeType::Dir,
            size: node.size,
            modified: Some(node.modified),
        })
        .collect();
    Ok(Response::Listing { entries })
}

pub async fn fetch(
    app: &AppService,
    peer_id: &str,
    share_id: &str,
    path: &str,
    token: Option<String>,
    offset: u64,
) -> AppResult<(fs::File, u64)> {
    let share = checked_share(app, peer_id, share_id, token)?;
    paths::validate_relative(path)?;
    let real = security::resolve_existing(share.path_ref(), path)?;
    let mut file = fs::File::open(real).await?;
    let meta = file.metadata().await?;
    if !meta.is_file() {
        return Err(AppError::BadRequest("Download target is not a file".into()));
    }
    if offset > meta.len() {
        return Err(AppError::BadRequest(
            "Resume offset exceeds the source file size".into(),
        ));
    }
    file.seek(std::io::SeekFrom::Start(offset)).await?;
    Ok((file, meta.len() - offset))
}

#[derive(Serialize, Deserialize)]
struct WebDownloadRecord {
    source: String,
    target: String,
    directory: bool,
}

#[derive(Serialize, Deserialize)]
struct CompletedFileRecord {
    size: u64,
    modified: String,
}

pub async fn download_web_share(
    app: &AppService,
    node_id: &str,
    endpoint: Endpoint,
    share_id: &str,
    path: &str,
    token: Option<&str>,
    directory: bool,
    expected_size: Option<u64>,
    cancel: CancellationToken,
    progress: Arc<dyn Fn(u64) + Send + Sync>,
    on_entry: Arc<dyn Fn(bool, u64) + Send + Sync>,
) -> AppResult<PathBuf> {
    paths::validate_relative(path)?;
    let downloads = dirs::download_dir()
        .ok_or_else(|| AppError::Other("The system Downloads directory is unavailable".into()))?;
    fs::create_dir_all(&downloads).await?;
    let source = format!("{node_id}/{share_id}/{path}");
    let key = format!("{:x}", Sha256::digest(source.as_bytes()));
    let state_dir = downloads.join(".nodesend-downloads");
    let task_dir = state_dir.join(&key);
    fs::create_dir_all(&task_dir).await?;
    let record_path = state_dir.join(format!("{key}.json"));
    let record = match fs::read(&record_path).await {
        Ok(bytes) => serde_json::from_slice::<WebDownloadRecord>(&bytes)
            .ok()
            .filter(|record| {
                record.source == source
                    && record.directory == directory
                    && Path::new(&record.target)
                        .file_name()
                        .and_then(|name| name.to_str())
                        == Some(record.target.as_str())
            }),
        Err(_) => None,
    };
    let target_name = record
        .map(|record| record.target)
        .unwrap_or(choose_download_name(&downloads, path, directory));
    let target = downloads.join(&target_name);
    if directory {
        fs::create_dir_all(&target).await?;
    }
    let record = WebDownloadRecord {
        source: source.clone(),
        target: target_name,
        directory,
    };
    let bytes = serde_json::to_vec(&record)?;
    fs::write(&record_path, bytes).await?;

    let mut channel = transport::connect(app, &endpoint, Some(node_id), false, true).await?;
    transport::hello(&mut channel, app).await?;
    if directory {
        let mut pending = vec![(path.to_string(), target.clone())];
        while let Some((remote_dir, local_dir)) = pending.pop() {
            let response = channel
                .request(&Request::Browse {
                    share_id: Some(share_id.to_string()),
                    path: remote_dir.clone(),
                    token: token.map(str::to_string),
                })
                .await?;
            let Response::Listing { entries } = response else {
                return Err(AppError::Other(
                    "NodeSend directory response is invalid".into(),
                ));
            };
            let mut used_names = HashSet::new();
            for entry in entries {
                on_entry(entry.is_dir, entry.size);
                let name = unique_child_name(&entry.name, entry.is_dir, &mut used_names);
                let remote_path = if remote_dir.is_empty() {
                    entry.name
                } else {
                    format!("{remote_dir}/{}", entry.name)
                };
                let local_path = local_dir.join(name);
                if entry.is_dir {
                    fs::create_dir_all(&local_path).await?;
                    pending.push((remote_path, local_path));
                } else {
                    download_file_entry(
                        &mut channel,
                        share_id,
                        &remote_path,
                        token,
                        &local_path,
                        &task_dir,
                        &remote_path,
                        Some(entry.size),
                        entry.modified.as_deref(),
                        cancel.clone(),
                        progress.clone(),
                    )
                    .await?;
                }
            }
        }
    } else {
        download_file_entry(
            &mut channel,
            share_id,
            path,
            token,
            &target,
            &task_dir,
            path,
            expected_size,
            None,
            cancel,
            progress,
        )
        .await?;
    }
    fs::remove_file(record_path).await.ok();
    fs::remove_dir_all(task_dir).await.ok();
    tracing::info!(node_id, share_id, path, target = %target.display(), "Web share download completed through NodeSend");
    Ok(target)
}

fn choose_download_name(root: &Path, source_path: &str, directory: bool) -> String {
    let source_name = source_path.rsplit('/').next().unwrap_or("download");
    let name = paths::sanitize_file_name(source_name);
    if !root.join(&name).exists() {
        return name;
    }
    let (stem, extension) = if directory {
        (name.as_str(), "")
    } else {
        match name.rfind('.') {
            Some(index) if index > 0 => (&name[..index], &name[index..]),
            _ => (name.as_str(), ""),
        }
    };
    for number in 1u64.. {
        let candidate = format!("{stem} ({number}){extension}");
        if !root.join(&candidate).exists() {
            return candidate;
        }
    }
    name
}

fn unique_child_name(source_name: &str, directory: bool, used: &mut HashSet<String>) -> String {
    let base = paths::sanitize_file_name(source_name);
    let (stem, extension) = if directory {
        (base.clone(), String::new())
    } else {
        match base.rfind('.') {
            Some(index) if index > 0 => (base[..index].to_owned(), base[index..].to_owned()),
            _ => (base.clone(), String::new()),
        }
    };
    let mut candidate = base.clone();
    let mut number = 1u64;
    while used.contains(&candidate.to_lowercase()) {
        candidate = format!("{stem} ({number}){extension}");
        number += 1;
    }
    used.insert(candidate.to_lowercase());
    candidate
}

async fn download_file_entry(
    channel: &mut Channel,
    share_id: &str,
    remote_path: &str,
    token: Option<&str>,
    destination: &Path,
    task_dir: &Path,
    part_key: &str,
    expected_size: Option<u64>,
    expected_modified: Option<&str>,
    cancel: CancellationToken,
    progress: Arc<dyn Fn(u64) + Send + Sync>,
) -> AppResult<()> {
    paths::validate_relative(remote_path)?;
    let part_name = format!("{:x}", Sha256::digest(part_key.as_bytes()));
    let marker_path = task_dir.join(format!("{part_name}.done.json"));
    if let (Some(size), Some(modified)) = (expected_size, expected_modified) {
        let completion_matches = fs::read(&marker_path)
            .await
            .ok()
            .and_then(|bytes| serde_json::from_slice::<CompletedFileRecord>(&bytes).ok())
            .is_some_and(|record| record.size == size && record.modified == modified);
        if completion_matches
            && fs::metadata(destination)
                .await
                .is_ok_and(|metadata| metadata.len() == size)
        {
            progress(size);
            return Ok(());
        }
    }
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent).await?;
    }
    let part_path = task_dir.join(format!("{part_name}.part"));
    let mut offset = fs::metadata(&part_path)
        .await
        .map(|meta| meta.len())
        .unwrap_or(0);
    if expected_size.is_some_and(|size| offset > size) {
        fs::remove_file(&part_path).await.ok();
        offset = 0;
    }
    let response = channel
        .request(&Request::Fetch {
            share_id: share_id.to_string(),
            path: remote_path.to_string(),
            token: token.map(str::to_string),
            offset,
        })
        .await?;
    let Response::File { length } = response else {
        return Err(AppError::Other("NodeSend file response is invalid".into()));
    };
    if expected_size.is_some_and(|size| size.saturating_sub(offset) != length) {
        return Err(AppError::Other(
            "The shared file changed while resuming the download".into(),
        ));
    }
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(offset > 0)
        .write(true)
        .truncate(offset == 0)
        .open(&part_path)
        .await?;
    if offset > 0 {
        progress(offset);
    }
    let mut reader = (&mut *channel.reader).take(length);
    let mut copied = 0u64;
    let mut buffer = vec![0u8; 256 * 1024];
    loop {
        let read = tokio::select! {
            _ = cancel.cancelled() => return Err(AppError::Other("Web share download was cancelled".into())),
            result = reader.read(&mut buffer) => result?,
        };
        if read == 0 {
            break;
        }
        file.write_all(&buffer[..read]).await?;
        copied = copied.saturating_add(read as u64);
        progress(read as u64);
    }
    if copied != length {
        return Err(AppError::Other(
            "NodeSend web download was interrupted; retry to resume it".into(),
        ));
    }
    file.flush().await?;
    let actual_size = file.metadata().await?.len();
    if expected_size.is_some_and(|size| actual_size != size) {
        return Err(AppError::Other(
            "The downloaded file size does not match the shared file".into(),
        ));
    }
    if fs::metadata(destination).await.is_ok() {
        fs::remove_file(destination).await?;
    }
    fs::rename(part_path, destination).await?;
    if let (Some(size), Some(modified)) = (expected_size, expected_modified) {
        let record = CompletedFileRecord {
            size,
            modified: modified.to_owned(),
        };
        fs::write(&marker_path, serde_json::to_vec(&record)?).await?;
    }
    Ok(())
}

pub async fn put_target(
    app: &AppService,
    peer_id: &str,
    share_id: &str,
    path: &str,
    token: Option<String>,
) -> AppResult<(fs::File, PathBuf)> {
    let share = checked_share(app, peer_id, share_id, token)?;
    paths::validate_relative(path)?;
    let (target, _) = security::resolve_for_write(Path::new(&share.path), path)?;
    let file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&target)
        .await?;
    Ok((file, target))
}
