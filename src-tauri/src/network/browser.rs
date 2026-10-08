use super::{
    peers::PeerStore,
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
use std::path::{Path, PathBuf};
use tokio::{
    fs,
    io::{AsyncReadExt, AsyncWriteExt},
};

pub async fn connect(app: &AppService, peers: &PeerStore, node_id: &str) -> AppResult<Channel> {
    let peer = peers.get(node_id)?;
    let mut last = String::from("NodeSend 设备没有可用地址");
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
        _ => Err(AppError::Other("NodeSend 目录响应非法".into())),
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
    let share_id = share_id.ok_or_else(|| AppError::BadRequest("请选择共享中的文件".into()))?;
    if relative.is_empty() {
        return Err(AppError::BadRequest("请选择共享中的文件".into()));
    }
    let mut channel = connect(app, peers, node_id).await?;
    let length = match channel
        .request(&Request::Fetch {
            share_id: share_id.to_string(),
            path: relative.to_string(),
            token: token.map(str::to_string),
        })
        .await?
    {
        Response::File { length } => length,
        _ => return Err(AppError::Other("NodeSend 文件响应非法".into())),
    };
    let mut file = fs::File::create(destination).await?;
    let copied = tokio::io::copy(&mut channel.reader.take(length), &mut file).await?;
    if copied != length {
        return Err(AppError::Other("NodeSend 下载中断，文件不完整".into()));
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
    let share_id = share_id.ok_or_else(|| AppError::BadRequest("请先进入目标共享".into()))?;
    if relative.is_empty() {
        return Err(AppError::BadRequest("目标文件路径不能为空".into()));
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
        _ => return Err(AppError::Other("NodeSend 上传响应非法".into())),
    }
    let copied = tokio::io::copy(&mut file, &mut channel.writer).await?;
    if copied != length {
        return Err(AppError::Other("本地文件在上传过程中发生变化".into()));
    }
    channel.writer.flush().await?;
    match channel.response().await? {
        Response::Ack => Ok(()),
        _ => Err(AppError::Other("NodeSend 上传确认非法".into())),
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
    if share.protocol != crate::share::ShareProtocol::Nodesend {
        return Err(AppError::NotFound("请使用该共享配置的协议访问".into()));
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
                share.protocol == crate::share::ShareProtocol::Nodesend
                    && share.enabled
                    && share.access_mode != crate::share::AccessMode::Deny
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
        return Err(AppError::BadRequest("请求路径不是目录".into()));
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
) -> AppResult<(fs::File, u64)> {
    let share = checked_share(app, peer_id, share_id, token)?;
    paths::validate_relative(path)?;
    let real = security::resolve_existing(share.path_ref(), path)?;
    let file = fs::File::open(real).await?;
    let meta = file.metadata().await?;
    if !meta.is_file() {
        return Err(AppError::BadRequest("下载目标不是文件".into()));
    }
    Ok((file, meta.len()))
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
