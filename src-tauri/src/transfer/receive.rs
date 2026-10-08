use super::{
    model::{incoming_id, ConflictPolicy, Manifest, TaskRecord},
    paths, TransferService,
};
use crate::{
    error::{AppError, AppResult},
    network::protocol::{Request, Response},
};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
    sync::Arc,
};
use tokio::io::{AsyncSeekExt, AsyncWriteExt};
impl TransferService {
    pub(crate) async fn offer(
        &self,
        peer: &str,
        name: &str,
        wire: &str,
        manifest: Manifest,
        transport: &str,
    ) -> AppResult<Response> {
        if wire.len() != 32 || !wire.bytes().all(|c| c.is_ascii_hexdigit()) {
            return Err(AppError::BadRequest("任务 ID 非法".into()));
        }
        manifest.validate()?;
        let id = incoming_id(peer, wire);
        if let Ok(task) = self.record(&id) {
            if task.manifest != manifest {
                return Err(AppError::Conflict("已创建的任务内容不能变更".into()));
            }
            return self.remote_state(&id);
        }
        if self
            .snapshots()
            .iter()
            .filter(|t| t.direction == "receive" && t.status == "waiting")
            .count()
            >= 128
        {
            return Err(AppError::Forbidden("待处理请求已满".into()));
        }
        let now = chrono::Utc::now().to_rfc3339();
        self.insert(TaskRecord {
            skipped_files: BTreeSet::new(),
            id: id.clone(),
            wire_id: wire.into(),
            peer_id: peer.into(),
            peer_name: name.into(),
            direction: "receive".into(),
            status: "waiting".into(),
            manifest,
            sources: vec![],
            targets: vec![],
            destination: None,
            conflict: ConflictPolicy::Rename,
            completed: BTreeSet::new(),
            transport: Some(transport.into()),
            error: None,
            events: vec!["已验证发送方身份，等待本机确认".into()],
            updated_at: now.clone(),
            created_at: now,
            allow_public: false,
        })?;
        let policy = self
            .app
            .get_setting("receive_policy")?
            .unwrap_or_else(|| "ask".into());
        if policy == "reject" {
            self.decide(&id, false, None, ConflictPolicy::Rename, None)
                .await?;
        } else if (policy == "trusted"
            || self
                .app
                .get_setting(&format!("auto_receive_{peer}"))?
                .as_deref()
                == Some("true"))
            && crate::registry::list(&self.app.db)?
                .iter()
                .any(|node| node.node_id == peer)
        {
            let destination = self.default_destination();
            std::fs::create_dir_all(&destination)?;
            self.decide(&id, true, Some(destination), ConflictPolicy::Rename, None)
                .await?;
        }
        self.remote_state(&id)
    }
    pub(crate) fn remote_state(&self, id: &str) -> AppResult<Response> {
        let task = self.record(id)?;
        Ok(Response::State {
            status: task.status,
            completed: task.completed.into_iter().collect(),
        })
    }
    pub(crate) async fn receive_chunk(
        &self,
        id: &str,
        index: usize,
        data: &[u8],
        hash: Option<String>,
        transport: &str,
    ) -> AppResult<Response> {
        let gate = self.gate(id);
        let _lock = gate.lock().await;
        let task = self.record(id)?;
        if !["accepted", "transferring"].contains(&task.status.as_str()) {
            return self.remote_state(id);
        }
        let chunk = task.manifest
            .chunk(index)
            .ok_or_else(|| AppError::BadRequest("Chunk 编号越界".into()))?;
        if task.skipped_files.contains(&chunk.file) { return Ok(Response::Ack); }
        if data.len() != chunk.length {
            return Err(AppError::BadRequest("Chunk 长度不匹配".into()));
        }
        if task.manifest.verify_hash
            && hash.as_deref() != Some(format!("{:x}", Sha256::digest(data)).as_str())
        {
            return Err(AppError::Conflict("Chunk SHA-256 校验失败".into()));
        }
        if task.completed.contains(&index) {
            return Ok(Response::Ack);
        }
        let stage = paths::stage(&task)?;
        std::fs::create_dir_all(&stage)?;
        paths::ensure_plain(&stage)?;
        let part = stage.join(format!("{}.part", chunk.file));
        paths::ensure_plain(&part)?;
        let key = (id.to_string(), chunk.file);
        let cached = { self.receive_files.lock().unwrap().get(&key).cloned() };
        let file = if let Some(file) = cached {
            file
        } else {
            let opened = tokio::fs::OpenOptions::new()
                .create(true).truncate(false).read(true).write(true)
                .open(&part).await?;
            let file = Arc::new(tokio::sync::Mutex::new(opened));
            self.receive_files.lock().unwrap().insert(key, file.clone());
            file
        };
        let mut file = file.lock().await;
        let expected_size = task.manifest.entries[chunk.file].size;
        if file.metadata().await?.len() != expected_size { file.set_len(expected_size).await?; }
        file.seek(std::io::SeekFrom::Start(chunk.offset)).await?;
        file.write_all(data).await?;
        file.sync_data().await?;
        drop(file);
        self.edit(id, |t| {
            t.completed.insert(index);
            t.status = "transferring".into();
            t.transport = Some(transport.into());
        })?;
        Ok(Response::Ack)
    }
    pub(crate) async fn commit(&self, id: &str) -> AppResult<Response> {
        let gate = self.gate(id);
        let _lock = gate.lock().await;
        let task = self.record(id)?;
        if task.status == "completed" {
            return Ok(Response::Ack);
        }
        if !["accepted", "transferring"].contains(&task.status.as_str()) {
            return self.remote_state(id);
        }
        if task.completed.len() != task.manifest.chunks().len() {
            return Err(AppError::Conflict("仍有未提交 Chunk".into()));
        }
        let root = PathBuf::from(task.destination.as_ref().unwrap());
        let stage = paths::stage(&task)?;
        std::fs::create_dir_all(&stage)?;
        for (i, entry) in task.manifest.entries.iter().enumerate() {
            let Some(target) = task.targets.get(i).and_then(Option::as_ref) else {
                continue;
            };
            let target = Path::new(target);
            paths::ensure_under(&root, target)?;
            if entry.directory {
                std::fs::create_dir_all(target)?;
                continue;
            }
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let part = stage.join(format!("{i}.part"));
            let done = stage.join(format!("{i}.committed"));
            paths::ensure_under(&root, &part)?;
            paths::ensure_under(&root, &done)?;
            // Rename the staged file into place. Android shared storage and
            // some desktop filesystems forbid hard links; rename stays on the
            // same filesystem and is atomic for the normal receive path.
            if done.exists() {
                continue;
            }
            if !part.exists() && entry.size == 0 {
                tokio::fs::File::create(&part).await?.sync_all().await?;
            }
            if tokio::fs::metadata(&part).await?.len() != entry.size {
                return Err(AppError::Conflict("接收文件大小与清单不匹配".into()));
            }
            if target.exists() {
                if same_file::is_same_file(&part, target).unwrap_or(false) {
                    tokio::fs::File::create(&done).await?.sync_all().await?;
                    continue;
                }
                if matches!(task.conflict, ConflictPolicy::Overwrite) {
                    let backup = stage.join(format!("{i}.previous"));
                    if !backup.exists() {
                        std::fs::rename(target, &backup)?;
                    } else {
                        return Err(AppError::Conflict(
                            "目标在恢复时再次变化，请检查冲突文件".into(),
                        ));
                    }
                } else if matches!(task.conflict, ConflictPolicy::Skip) {
                    continue;
                } else {
                    // An existing final hard link after a crash is only accepted
                    // when the completed marker exists; never overwrite a new file.
                    return Err(AppError::Conflict(format!(
                        "保存期间目标已出现：{}",
                        entry.path
                    )));
                }
            }
            if let Err(err) = std::fs::rename(&part, target) {
                let backup = stage.join(format!("{i}.previous"));
                if backup.exists() && !target.exists() {
                    let _ = std::fs::rename(backup, target);
                }
                return Err(AppError::Io(err));
            }
            tokio::fs::File::create(&done).await?.sync_all().await?;
        }
        #[cfg(target_os = "android")]
        if let Err(error) = crate::android_import::export_received(task.clone()).await {
            self.edit(id, |t| { t.error = Some(error.clone()); t.event("保存到所选目录失败，文件仍保留在应用内，可重新保存"); })?;
            return Err(AppError::Other(error));
        }
        self.edit(id, |t| {
            t.status = "completed".into();
            t.error = None;
            t.event("所有文件已持久化并提交，发送完成回执");
        })?;
        // Only generated staging files inside this task directory are cleaned.
        if paths::ensure_under(&root, &stage).is_ok() {
            let _ = std::fs::remove_dir_all(&stage);
        }
        Ok(Response::Ack)
    }
    pub(crate) async fn control(
        &self,
        peer: &str,
        name: &str,
        request: Request,
        transport: &str,
    ) -> AppResult<Response> {
        match request {
            Request::SkipFile { id, file } => {
                let id = incoming_id(peer, &id);
                let gate = self.gate(&id);
                let _lock = gate.lock().await;
                let task = self.record(&id)?;
                if !["accepted", "transferring", "paused"].contains(&task.status.as_str()) { return self.remote_state(&id); }
                let entry = task.manifest.entries.get(file).filter(|e| !e.directory)
                    .ok_or_else(|| AppError::BadRequest("文件编号非法".into()))?;
                let part = paths::stage(&task)?.join(format!("{file}.part"));
                paths::ensure_plain(&part)?;
                match tokio::fs::remove_file(&part).await {
                    Ok(()) => {},
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {},
                    Err(e) => return Err(e.into()),
                }
                self.edit(&id, |t| {
                    t.targets[file] = None;
                    if t.skipped_files.insert(file) { t.event(format!("源文件已变化，跳过：{}", entry.path)); }
                    for (i, c) in t.manifest.chunks().iter().enumerate() { if c.file == file { t.completed.insert(i); } }
                })?;
                Ok(Response::Ack)
            },
            Request::Offer { id, manifest } => {
                self.offer(peer, name, &id, manifest, transport).await
            }
            Request::Poll { id } => self.remote_state(&incoming_id(peer, &id)),
            Request::Commit { id } => self.commit(&incoming_id(peer, &id)).await,
            Request::Cancel { id } => {
                self.action(&incoming_id(peer, &id), "cancel").await?;
                Ok(Response::Ack)
            }
            _ => Err(AppError::BadRequest("错误的会话消息顺序".into())),
        }
    }
}
