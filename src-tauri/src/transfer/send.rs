use super::{model::TaskRecord, source, TransferService};
use crate::{
    error::{AppError, AppResult},
    network::{
        peers::PeerStore,
        protocol::{Request, Response},
        transport::{self, Channel},
    },
};
use sha2::{Digest, Sha256};
use std::{
    collections::VecDeque,
    sync::{
        atomic::{AtomicI64, Ordering},
        Arc,
    },
    time::Duration,
};
use tokio::sync::Mutex;
impl TransferService {
    pub fn start_task(self: &Arc<Self>, peers: Arc<PeerStore>, id: String) {
        if !self.active.lock().unwrap().insert(id.clone()) {
            return;
        }
        let service = self.clone();
        tokio::spawn(async move {
            let run = async {
                let _permit = service
                    .scheduler
                    .clone()
                    .acquire_owned()
                    .await
                    .map_err(|e| AppError::Other(e.to_string()))?;
                let cooldown = Arc::new(AtomicI64::new(0));
                for attempt in 0..3 {
                    if service.stopped(&id)? {
                        return Ok(());
                    }
                    match service
                        .send_once(peers.clone(), &id, cooldown.clone())
                        .await
                    {
                        Ok(()) => return Ok(()),
                        Err(err) => {
                            if service.stopped(&id)? {
                                return Ok(());
                            }
                            let retry = matches!(err, AppError::Io(_) | AppError::Other(_));
                            if !retry || attempt == 2 {
                                return Err(err);
                            }
                            cooldown.store(chrono::Utc::now().timestamp() + 30, Ordering::Relaxed);
                            service.edit(&id, |t| {
                                t.status = "paused".into();
                                t.error = Some(err.to_string());
                                t.event("连接中断，已暂停；点击继续后重新检查所有地址");
                            })?;
                            tokio::time::sleep(Duration::from_secs(2)).await;
                        }
                    }
                }
                Ok(())
            };
            let result = tokio::select! {r=run=>r,_=service.shutdown.cancelled()=>Ok(())};
            if let Err(err) = result {
                let _ = service.edit(&id, |t| {
                    if !["cancelled", "paused"].contains(&t.status.as_str()) {
                        t.status = "paused".into();
                        t.error = Some(err.to_string());
                        t.event("设备暂时离线，已暂停；设备上线后点击继续");
                    }
                });
            }
            service.active.lock().unwrap().remove(&id);
        });
    }
    fn stopped(&self, id: &str) -> AppResult<bool> {
        Ok(["paused", "cancelled", "rejected", "completed"]
            .contains(&self.record(id)?.status.as_str()))
    }
    async fn channel(
        &self,
        peers: &PeerStore,
        task: &TaskRecord,
        cooldown: &AtomicI64,
    ) -> AppResult<Channel> {
        let peer = peers.get(&task.peer_id)?;
        let mut last = "没有可用 Endpoint".to_string();
        for endpoint in &peer.endpoints {
            match transport::connect(
                &self.app,
                endpoint,
                Some(&task.peer_id),
                chrono::Utc::now().timestamp() < cooldown.load(Ordering::Relaxed),
                task.allow_public,
            )
            .await
            {
                Ok(mut channel) => {
                    transport::hello(&mut channel, &self.app).await?;
                    peers.seen(&task.peer_id, endpoint);
                    // TCP is a fallback for this connection only. Do not keep
                    // a cooldown: the next reconnect must probe QUIC again.
                    if channel.kind == "tcp" { cooldown.store(0, Ordering::Relaxed); }
                    self.edit(&task.id, |t| {
                        if t.transport.as_deref() != Some(channel.kind) {
                            t.event(format!(
                                "使用 {} + TLS 1.3，经 {}",
                                channel.kind.to_uppercase(),
                                endpoint.address
                            ));
                        }
                        t.transport = Some(channel.kind.into());
                    })?;
                    return Ok(channel);
                }
                Err(err) => {
                    last = err.to_string();
                    peers.failure(&task.peer_id, endpoint, last.clone());
                }
            }
        }
        Err(AppError::Other(last))
    }
    async fn send_once(
        self: &Arc<Self>,
        peers: Arc<PeerStore>,
        id: &str,
        cooldown: Arc<AtomicI64>,
    ) -> AppResult<()> {
        let task = self.edit(id, |t| {
            t.status = "connecting".into();
            t.error = None;
        })?;
        let mut control = self.channel(&peers, &task, &cooldown).await?;
        let mut state = control
            .request(&Request::Offer {
                id: task.wire_id.clone(),
                manifest: task.manifest.clone(),
            })
            .await?;
        loop {
            if self.stopped(id)? {
                if self.record(id)?.status == "cancelled" {
                    let _ = control
                        .request(&Request::Cancel {
                            id: task.wire_id.clone(),
                        })
                        .await;
                }
                return Ok(());
            }
            match state {
                Response::State { status, completed }
                    if ["accepted", "transferring", "completed"].contains(&status.as_str()) =>
                {
                    let count = task.manifest.chunks().len();
                    if completed.iter().any(|i| *i >= count) {
                        return Err(AppError::BadRequest("对端返回非法分块状态".into()));
                    }
                    self.edit(id, |t| {
                        t.completed = completed.into_iter().collect();
                        t.status = if status == "completed" {
                            "completed"
                        } else {
                            "transferring"
                        }
                        .into();
                    })?;
                    if status == "completed" {
                        return Ok(());
                    }
                    break;
                }
                Response::State { status, .. }
                    if ["rejected", "cancelled"].contains(&status.as_str()) =>
                {
                    self.edit(id, |t| {
                        t.status = status;
                        t.event("接收方拒绝或取消任务");
                    })?;
                    return Ok(());
                }
                Response::State { status, .. }
                    if ["waiting", "paused", "interrupted"].contains(&status.as_str()) =>
                {
                    self.edit(id, |t| t.status = "waiting".into())?;
                    tokio::time::sleep(Duration::from_millis(750)).await;
                    state = control
                        .request(&Request::Poll {
                            id: task.wire_id.clone(),
                        })
                        .await?;
                }
                _ => return Err(AppError::Other("接收方未返回有效接收状态".into())),
            }
        }
        let chunks = task.manifest.chunks();
        let current = self.record(id)?;
        let pending = chunks
            .iter()
            .enumerate()
            .filter(|(i, _)| !current.completed.contains(i))
            .map(|(i, c)| (i, *c))
            .collect::<VecDeque<_>>();
        let concurrency = pending.len().clamp(1, 4);
        let queue = Arc::new(Mutex::new(pending));
        let mut workers = tokio::task::JoinSet::new();
        // Close the idle control connection while file workers own active streams.
        drop(control);
        for _ in 0..concurrency {
            let service = self.clone();
            let peers = peers.clone();
            let task = task.clone();
            let cooldown = cooldown.clone();
            let queue = queue.clone();
            workers.spawn(async move {
                let mut channel = service.channel(&peers, &task, &cooldown).await?;
                loop {
                    if service.stopped(&task.id)? {
                        return Ok::<_, AppError>(());
                    }
                    let Some((index, chunk)) = queue.lock().await.pop_front() else {
                        break;
                    };
                    if service.record(&task.id)?.skipped_files.contains(&chunk.file) { continue; }
                    let bytes = match source::read_chunk(&task, chunk).await {
                        Ok(bytes) => bytes,
                        Err(AppError::Conflict(_)) => {
                            match channel.request(&Request::SkipFile { id: task.wire_id.clone(), file: chunk.file }).await? {
                                Response::Ack => {},
                                _ => return Err(AppError::Other("接收方未确认跳过变化文件，请更新两端软件后重试".into())),
                            }
                            service.edit(&task.id, |t| {
                                if t.skipped_files.insert(chunk.file) { t.event(format!("源文件已变化，跳过：{}", task.manifest.entries[chunk.file].path)); }
                                for (i, c) in t.manifest.chunks().iter().enumerate() { if c.file == chunk.file { t.completed.insert(i); } }
                            })?;
                            continue;
                        },
                        Err(e) => return Err(e),
                    };
                    let sha256 = task
                        .manifest
                        .verify_hash
                        .then(|| format!("{:x}", Sha256::digest(&bytes)));
                    loop {
                        if service.stopped(&task.id)? {
                            return Ok(());
                        }
                        match channel
                            .chunk(
                                &Request::Chunk {
                                    id: task.wire_id.clone(),
                                    index,
                                    length: bytes.len(),
                                    sha256: sha256.clone(),
                                },
                                &bytes,
                            )
                            .await?
                        {
                            Response::Ack => {
                                service.edit(&task.id, |t| {
                                    t.completed.insert(index);
                                })?;
                                break;
                            }
                            Response::State { status, .. } if status == "paused" => {
                                service.edit(&task.id, |t| {
                                    if t.status == "transferring" {
                                        t.status = "waiting".into();
                                        t.event("接收方已暂停，等待对方恢复后自动继续");
                                    }
                                })?;
                                loop {
                                    if service.stopped(&task.id)? {
                                        return Ok(());
                                    }
                                    tokio::time::sleep(Duration::from_millis(750)).await;
                                    match channel
                                        .request(&Request::Poll {
                                            id: task.wire_id.clone(),
                                        })
                                        .await?
                                    {
                                        Response::State { status, .. }
                                            if ["accepted", "transferring"]
                                                .contains(&status.as_str()) =>
                                        {
                                            service.edit(&task.id, |t| {
                                                if t.status == "waiting" {
                                                    t.status = "transferring".into();
                                                }
                                            })?;
                                            break;
                                        }
                                        Response::State { status, .. } if status == "paused" => {}
                                        Response::State { status, .. }
                                            if ["cancelled", "rejected"]
                                                .contains(&status.as_str()) =>
                                        {
                                            service.edit(&task.id, |t| t.status = status)?;
                                            return Ok(());
                                        }
                                        _ => {
                                            return Err(AppError::Other(
                                                "接收方恢复状态非法".into(),
                                            ))
                                        }
                                    }
                                }
                            }
                            Response::State { status, .. } if status == "cancelled" => {
                                service.edit(&task.id, |t| t.status = "cancelled".into())?;
                                return Ok(());
                            }
                            _ => return Err(AppError::Other("接收方未提交 Chunk".into())),
                        }
                    }
                }
                Ok(())
            });
        }
        while let Some(result) = workers.join_next().await {
            if let Err(err) = result.map_err(|e| AppError::Other(e.to_string()))? {
                workers.abort_all();
                while workers.join_next().await.is_some() {}
                return Err(err);
            }
        }
        if self.stopped(id)? {
            return Ok(());
        }
        let mut control = self.channel(&peers, &task, &cooldown).await?;
        loop {
            if self.stopped(id)? {
                return Ok(());
            }
            match control
                .request(&Request::Commit {
                    id: task.wire_id.clone(),
                })
                .await?
            {
                Response::Ack => {
                    self.edit(id, |t| {
                        t.status = "completed".into();
                        t.error = None;
                        t.event("收到接收方的文件落盘回执");
                    })?;
                    return Ok(());
                }
                Response::State { status, .. } if status == "paused" => {
                    self.edit(id, |t| {
                        if t.status == "transferring" {
                            t.status = "waiting".into();
                        }
                    })?;
                    tokio::time::sleep(Duration::from_millis(750)).await;
                }
                Response::State { status, .. }
                    if ["cancelled", "rejected"].contains(&status.as_str()) =>
                {
                    self.edit(id, |t| t.status = status)?;
                    return Ok(());
                }
                _ => return Err(AppError::Other("接收方尚未完成文件提交".into())),
            }
        }
    }
}
