use super::{
    model::{incoming_id, FileEntry, Manifest},
    paths, relay, TransferService,
};
use crate::{
    error::{AppError, AppResult},
    network::protocol::Response,
};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
pub(super) mod commit;
mod control;
mod manifest;

impl TransferService {
    #[cfg(desktop)]
    pub(crate) async fn drain_completed_cache(&self) -> AppResult<()> {
        for id in self.receive_ids() {
            if !super::recovery::processing(&self.status_of(&id)?) {
                continue;
            }
            self.await_writes(&id).await?;
            self.ensure_bulk(&id)?;
            let mut previous = 0;
            let mut progress_at = tokio::time::Instant::now();
            loop {
                let task = self.record(&id)?;
                if let Some(error) = task.error {
                    return Err(AppError::Other(error));
                }
                let pending = task.cache_files.iter().any(|file| {
                    let Some(entry) = task.manifest.entries.get(*file) else {
                        return false;
                    };
                    if entry.directory
                        || entry.size == 0
                        || task.relayed.get(*file).copied().unwrap_or(0) >= entry.size
                    {
                        return false;
                    }
                    let base = task.manifest.file_base(*file);
                    let count = entry.size.div_ceil(task.chunk_size) as usize;
                    (base..base + count).all(|chunk| task.completed.contains(&chunk))
                });
                if !pending {
                    break;
                }
                let progress = task.relayed_total.saturating_add(
                    self.runtime(&id)
                        .relay_progress
                        .load(std::sync::atomic::Ordering::Relaxed),
                );
                if progress != previous {
                    previous = progress;
                    progress_at = tokio::time::Instant::now();
                }
                if progress_at.elapsed() >= crate::tuning::RELAY_STALL_TIMEOUT {
                    return Err(AppError::Conflict(
                        "Cache relay made no progress before the timeout".into(),
                    ));
                }
                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            }
        }
        Ok(())
    }
    pub(crate) async fn commit_ready_file(&self, id: &str, file: usize) -> AppResult<()> {
        let task = self.record(id)?;
        if task.direction != "receive"
            || task.destination.is_none()
            || !super::recovery::processing(&task.status)
            || task.cache_files.contains(&file)
        {
            return Ok(());
        }
        let Some(entry) = task.manifest.entries.get(file) else {
            return Ok(());
        };
        if entry.directory || task.targets.get(file).and_then(Option::as_ref).is_none() {
            return Ok(());
        }
        let base = task.manifest.file_base(file);
        let count = entry.size.div_ceil(task.chunk_size) as usize;
        if !(base..base + count).all(|index| task.completed.contains(&index)) {
            return Ok(());
        }
        #[cfg(target_os = "android")]
        if self.is_android_direct_receive(&task) {
            let request = self.android_download_request(&task, file)?;
            tokio::task::spawn_blocking(move || {
                crate::android_import::publish_download_file(request)
            })
            .await
            .map_err(|error| AppError::Other(error.to_string()))?
            .map_err(AppError::Other)?;
            return Ok(());
        }
        let root = PathBuf::from(task.destination.as_deref().unwrap());
        let stage = paths::stage(&task)?;
        let size = entry.size;
        let target = task.targets[file].clone();
        tokio::task::spawn_blocking(move || {
            std::fs::create_dir_all(&stage)?;
            TransferService::commit_staged_file(&task, &root, &stage, file)
        })
        .await
        .map_err(|error| AppError::Other(error.to_string()))??;
        self.journal_transition(id, file, size, "published", target.as_deref())?;
        Ok(())
    }

    /// 将已经完整落到目标盘暂存区的文件逐个发布到最终目录；不等待整个文件夹结束。
    /// 缓存模式以 relay 游标为准，直写模式以持久化的 Chunk 完成记录为准。
    #[allow(dead_code)]
    pub(crate) async fn commit_ready_files(&self, id: &str) -> AppResult<()> {
        let task = self.record(id)?;
        if task.direction != "receive"
            || task.destination.is_none()
            || !super::recovery::processing(&task.status)
        {
            return Ok(());
        }
        if !self.begin_commit(id) {
            return Ok(());
        }
        let result = async {
            let task = self.record(id)?;
            if task.direction != "receive"
                || task.destination.is_none()
                || !super::recovery::processing(&task.status)
            {
                return Ok(());
            }
            let root = PathBuf::from(task.destination.as_deref().unwrap());
            let stage = paths::stage(&task)?;
            let mut ready = Vec::new();
            for (file, entry) in task.manifest.entries.iter().enumerate() {
                if entry.directory || task.targets.get(file).and_then(Option::as_ref).is_none() {
                    continue;
                }
                let complete = if task.use_cache {
                    task.relayed.get(file).copied().unwrap_or(0) >= entry.size
                } else {
                    let base = task.manifest.file_base(file);
                    let count = entry.size.div_ceil(task.chunk_size) as usize;
                    (base..base + count).all(|index| task.completed.contains(&index))
                };
                if complete {
                    ready.push(file);
                }
            }
            if ready.is_empty() {
                return Ok(());
            }
            tokio::task::spawn_blocking(move || {
                std::fs::create_dir_all(&stage)?;
                for file in ready {
                    TransferService::commit_staged_file(&task, &root, &stage, file)?;
                }
                Ok::<(), AppError>(())
            })
            .await
            .map_err(|error| AppError::Other(error.to_string()))??;
            Ok::<(), AppError>(())
        }
        .await;
        self.finish_commit(id);
        result
    }

    /// 接收一段清单分片：逐片写入 `task_entries`（边收边落库，不驻留内存），返回确认。
    pub(crate) async fn manifest_chunk(
        &self,
        peer: &str,
        wire: &str,
        first: usize,
        mut entries: Vec<FileEntry>,
    ) -> AppResult<Response> {
        let id = incoming_id(peer, wire);
        let gate = self.gate(&id);
        let guard = gate.lock().await;
        let started = std::time::Instant::now();
        let status = self.status_of(&id)?;
        if ![
            "waiting",
            "accepted",
            "transferring",
            "paused",
            "remote_paused",
        ]
        .contains(&status.as_str())
        {
            tracing::info!(task = %id, first, status = %status, "Manifest chunk arrived after the task ended; acknowledging it");
            return Ok(Response::ManifestAck);
        }
        let (
            current,
            mut total_size,
            mut directory_count,
            mut chunk_count,
            chunk_size,
            top_level_count,
        ) = {
            let tasks = crate::locks::lock(&self.tasks);
            let task = tasks
                .get(&id)
                .ok_or_else(|| AppError::NotFound("Receive task not found".into()))?;
            (
                task.file_count,
                task.total_size,
                task.directory_count,
                task.chunk_count,
                task.chunk_size,
                task.top_level_count,
            )
        };
        if first < current && first.saturating_add(entries.len()) <= current {
            tracing::debug!(task = %id, first, current, entries = entries.len(), "Manifest chunk was already received; acknowledging it");
            return Ok(Response::ManifestAck);
        }
        if first != current {
            return Err(AppError::Conflict("Manifest chunks are out of order".into()));
        }
        if status != "waiting" {
            let destination = {
                let tasks = crate::locks::lock(&self.tasks);
                tasks.get(&id).and_then(|task| task.destination.clone())
            }
            .ok_or_else(|| AppError::Conflict("Receive target has not been selected".into()))?;
            let additional_size = entries.iter().map(|entry| entry.size).sum::<u64>();
            // 可用空间查询是同步调用，且每个清单分片都会走一次：放到阻塞线程池，
            // 否则慢盘/网络盘会把 tokio 工作线程占住。
            let free = tokio::task::spawn_blocking(move || {
                Ok::<_, AppError>(fs2::available_space(Path::new(&destination))?)
            })
            .await
            .map_err(|error| AppError::Other(error.to_string()))??;
            if free < additional_size {
                return Err(AppError::Conflict(
                    "Insufficient space remains in the receive directory; later data was paused".into(),
                ));
            }
        }
        if status != "waiting" {
            let mut map = crate::locks::lock(&self.tasks);
            let task = map
                .get_mut(&id)
                .ok_or_else(|| AppError::NotFound("Receive task not found".into()))?;
            let destination = PathBuf::from(
                task.destination
                    .as_deref()
                    .ok_or_else(|| AppError::Conflict("Receive target has not been selected".into()))?,
            );
            let roots = task
                .manifest
                .entries
                .iter()
                .enumerate()
                .filter(|(_, entry)| !entry.path.contains('/'))
                .map(|(index, entry)| {
                    (
                        entry.path.clone(),
                        task.targets.get(index).cloned().flatten(),
                    )
                })
                .collect::<Vec<_>>();
            // 稳定缓存名称在条目落库前分配并随行持久化：后续任何阶段
            // （写入、转写、恢复、取消）都只按文件索引读取这份映射。
            let node_id = task.peer_id.clone();
            paths::assign_cache_names(&mut entries, &node_id, &mut task.cache_name_seq);
            // 顶层目标数量很少：这里保留完整校验（含符号链接/重解析点），
            // 子条目的逐条校验改用词法检查，避免每帧数千次 canonicalize。
            for (_, root_target) in &roots {
                if let Some(path) = root_target {
                    paths::ensure_under(&destination, Path::new(path))?;
                }
            }
            let mut next_chunk = task.chunk_count;
            let mut completed_to_save = Vec::new();
            let mut target_rows = Vec::new();
            for entry in &entries {
                let (top, tail) = entry.path.split_once('/').ok_or_else(|| {
                    AppError::Conflict("Received a non-root entry while scanning; manifest order is invalid".into())
                })?;
                let root_target = roots
                    .iter()
                    .find(|(name, _)| name == top)
                    .and_then(|(_, target)| target.clone());
                let target = root_target.map(|path| {
                    let mut path = PathBuf::from(path);
                    path.push(tail);
                    path
                });
                if let Some(path) = target.as_ref() {
                    // 一帧最多 5000 条，这里不能做 canonicalize 之类的重活：
                    // 词法校验 + 仅在策略需要时探测文件系统。
                    paths::ensure_relative_safe(&entry.path)?;
                    if path.exists() && entry.directory != path.is_dir() {
                        return Err(AppError::Conflict(format!(
                            "Path type conflicts with {}",
                            entry.path
                        )));
                    }
                }
                let file_index = task.file_count;
                if let Some(path) = target.as_ref() {
                    target_rows.push((file_index, path.to_string_lossy().into_owned()));
                    task.targets.push(Some(path.to_string_lossy().into_owned()));
                    if !entry.directory {
                        #[cfg(target_os = "android")]
                        if task.android_media_library {
                            task.receive_directories
                                .insert(paths::android_media_directory(&entry.path).to_string());
                        }
                        task.relay_target_bytes =
                            task.relay_target_bytes.saturating_add(entry.size);
                    }
                } else {
                    task.targets.push(None);
                }
                task.relayed.push(0);
                let count = if entry.directory {
                    0
                } else {
                    entry.size.div_ceil(task.chunk_size) as usize
                };
                if target.is_none() {
                    for index in next_chunk..next_chunk + count {
                        if task.completed.insert(index) {
                            completed_to_save.push(index);
                            task.completed_bytes =
                                task.completed_bytes.saturating_add(task.chunk_size.min(
                                    entry.size.saturating_sub(
                                        (index - next_chunk) as u64 * task.chunk_size,
                                    ),
                                ));
                        }
                    }
                }
                next_chunk += count;
                task.directory_count += usize::from(entry.directory);
                task.total_size = task.total_size.saturating_add(entry.size);
                task.file_count += 1;
            }
            let mut all_entries = std::mem::take(&mut task.manifest.entries);
            all_entries.extend(entries.iter().cloned());
            task.manifest = Manifest::new(all_entries, task.chunk_size, task.verify_hash);
            task.chunk_count = next_chunk;
            task.completed_count = task.completed.len();
            task.scan_last_seq = task.file_count;
            task.scan_last_path = entries
                .last()
                .map(|entry| entry.path.clone())
                .unwrap_or_default();
            task.updated_at = chrono::Utc::now().to_rfc3339();
            // `with_transaction` 内部持有唯一连接守卫，闭包拿不到连接，
            // 因此"持连接再取连接"这种非重入死锁在这里写不出来。
            self.app.db.with_transaction(|tx| {
                {
                    let mut stmt = tx.prepare("INSERT OR REPLACE INTO task_entries(task_id,index_no,path,size,modified_ms,directory,name,cache_name) VALUES(?1,?2,?3,?4,?5,?6,?7,?8)")?;
                    for (offset, entry) in entries.iter().enumerate() {
                        stmt.execute(rusqlite::params![
                            id,
                            (first + offset) as i64,
                            entry.path,
                            entry.size as i64,
                            entry.modified_ms as i64,
                            entry.directory as i64,
                            entry.name,
                            entry.cache_name
                        ])?;
                    }
                }
                for (index, path) in target_rows {
                    tx.execute(
                        "INSERT OR REPLACE INTO task_targets(task_id,file_index,target) VALUES(?1,?2,?3)",
                        rusqlite::params![id, index as i64, path],
                    )?;
                }
                for index in completed_to_save {
                    tx.execute(
                        "INSERT OR IGNORE INTO task_chunks(task_id,chunk_index) VALUES(?1,?2)",
                        rusqlite::params![id, index as i64],
                    )?;
                }
                Ok(())
            })?;
            self.persist(task)?;
            drop(map);
            self.extend_cache_plan(&id, first)?;
            log_manifest_chunk(&id, first, entries.len(), started.elapsed(), true);
            return Ok(Response::ManifestAck);
        }
        self.assign_entry_cache_names(&id, &mut entries)?;
        self.append_entries(&id, first, &entries)?;
        for entry in &entries {
            total_size = total_size.saturating_add(entry.size);
            directory_count += usize::from(entry.directory);
            if !entry.directory {
                chunk_count += entry.size.div_ceil(chunk_size) as usize;
            }
        }
        self.update_scan_progress(
            &id,
            current + entries.len(),
            directory_count,
            total_size,
            chunk_count,
            entries
                .last()
                .map(|entry| entry.path.as_str())
                .unwrap_or_default(),
        )?;
        // 顶层清单已到齐：信任设备的自动接收在这里就该发生，不能等整份递归清单
        // 扫描结束（几十万文件时那是几十分钟之后）。必须先释放任务锁再 decide。
        let roots_ready = top_level_count > 0 && current + entries.len() >= top_level_count;
        drop(guard);
        log_manifest_chunk(&id, first, entries.len(), started.elapsed(), false);
        if roots_ready {
            let decided = self.auto_accept_if_trusted(peer, &id).await;
            // 自动接收是"清单阶段"里唯一会做较多工作的一步，单独记时：
            // 卡在这一步和卡在分片处理需要区分开。
            tracing::info!(
                task = %id,
                accepted = decided,
                elapsed_ms = started.elapsed().as_millis() as u64,
                "Top-level manifest processing completed and was acknowledged"
            );
        }
        Ok(Response::ManifestAck)
    }

    pub(crate) fn remote_state(&self, id: &str) -> AppResult<Response> {
        let (status, completed, revision) = self.status_and_completed(id)?;
        Ok(Response::State {
            status,
            completed,
            revision,
        })
    }
    pub(crate) async fn receive_chunk(
        &self,
        id: &str,
        index: usize,
        data: Vec<u8>,
        hash: Option<String>,
        file_version: Option<String>,
        transport: &str,
    ) -> AppResult<Response> {
        let chunk = self
            .chunk_at(id, index)?
            .ok_or_else(|| AppError::BadRequest("Chunk index is out of range".into()))?;
        self.admit_cached_file(id, chunk.file).await?;
        let gate = self.gate(id);
        let lock = gate.lock().await;
        let status = self.status_of(id)?;
        if !["accepted", "transferring"].contains(&status.as_str()) {
            return self.remote_state(id);
        }
        {
            let tasks = crate::locks::lock(&self.tasks);
            let task = tasks
                .get(id)
                .ok_or_else(|| AppError::NotFound("Receive task not found".into()))?;
            let expected =
                super::model::file_version(chunk.file, &task.manifest.entries[chunk.file]);
            if file_version.as_ref().is_some_and(|v| v != &expected)
                || file_version.is_none() && !task.repair_files.is_empty()
            {
                return Err(AppError::Conflict(
                    "The received file version does not match the active manifest repair".into(),
                ));
            }
        }
        if self.is_skipped(id, chunk.file) {
            return Ok(Response::Ack);
        }
        if data.len() != chunk.length {
            return Err(AppError::BadRequest("Chunk length mismatch".into()));
        }
        if self.verify_hash_of(id)?
            && hash.as_deref() != Some(format!("{:x}", Sha256::digest(&data)).as_str())
        {
            return Err(AppError::Conflict("Chunk SHA-256 verification failed".into()));
        }
        let runtime = self.runtime(id);
        // 已完成，或已入队但尚未落盘：重复块直接确认，不重复入队。
        if self.is_completed(id, index)
            || crate::locks::lock(&runtime.write_pending).contains(&index)
        {
            return Ok(Response::Ack);
        }
        self.mark_transferring(id, transport)?;
        crate::locks::lock(&runtime.write_pending).insert(index);
        // 入队即返回 Ack：磁盘延迟不再进入 ACK 往返路径。
        // 完成状态由后台写入任务在 sync_data 成功后记录；队列有界，
        // 满时在此背压等待（写入任务持续排空，不会死锁）。
        let pending = super::write_queue::PendingChunk {
            task_id: id.to_string(),
            file: chunk.file,
            index,
            offset: chunk.offset,
            data,
        };
        // 关键：等待队列空间前必须释放任务锁。否则一个慢盘任务会把任务锁长期
        // 占住，同一任务的所有会话一起阻塞，连接许可被逐个耗尽，最终表现为
        // "运行一段时间后掉线、必须重启对端才能连上"。
        drop(lock);
        match tokio::time::timeout(
            super::write_queue::QUEUE_WAIT_TIMEOUT,
            self.write_tx.send(pending),
        )
        .await
        {
            Ok(Ok(())) => Ok(Response::Ack),
            Ok(Err(_)) => {
                crate::locks::lock(&runtime.write_pending).remove(&index);
                Err(AppError::Other("Receive write queue is closed".into()))
            }
            Err(_) => {
                crate::locks::lock(&runtime.write_pending).remove(&index);
                tracing::warn!(task = %id, index, "Write queue had no space for too long; abandoning this enqueue attempt");
                Err(AppError::Conflict(
                    "Receive write queue has no space for too long; check the target disk write speed".into(),
                ))
            }
        }
    }
    /// 等待本任务所有已入队块完成「写入 + sync_data + 记录完成」。
    /// 超时说明写入任务异常或存在长期缺口，改由调用方报错，避免永久阻塞。
    pub(crate) async fn await_writes(&self, id: &str) -> AppResult<()> {
        let deadline = tokio::time::Instant::now() + crate::tuning::AWAIT_WRITES_TIMEOUT;
        let runtime = self.runtime(id);
        loop {
            let busy = !crate::locks::lock(&runtime.write_pending).is_empty();
            if !busy {
                return Ok(());
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(AppError::Conflict(
                    "Some chunks are still being written; retry the commit shortly".into(),
                ));
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    }
    /// 等待「缓存盘 → 目标盘」的转写任务把全部数据连续写完（缓存模式）。
    /// 直写模式或未启用缓存时立即返回。
    ///
    /// 此时发送端已收齐全部回执（`await_writes` 已通过），转写所需数据都在缓存中，
    /// 正常情况下只会慢、不会停，所以用「无进展超时」而非绝对超时：只要转写进度
    /// 仍在推进就一直等（尾部可能很大，取决于缓存盘容量），停滞才报错。
    async fn await_relay(&self, id: &str) -> AppResult<()> {
        let mut last = 0u64;
        let mut stalled_since = tokio::time::Instant::now();
        loop {
            let state = self.relay_state(id)?;
            if relay::relay_done(&state) || state.completed_count < state.chunk_count {
                return Ok(());
            }
            if let Some(err) = state.error {
                return Err(AppError::Other(err));
            }
            if !super::recovery::processing(&state.status) || state.status == "finishing" {
                return Err(AppError::Conflict(
                    "Relay was stopped; retry the commit shortly".into(),
                ));
            }
            let progress = state.relayed_total.saturating_add(
                self.runtime(id)
                    .relay_progress
                    .load(std::sync::atomic::Ordering::Relaxed),
            );
            if progress > last {
                last = progress;
                stalled_since = tokio::time::Instant::now();
            } else if stalled_since.elapsed() >= crate::tuning::RELAY_STALL_TIMEOUT {
                return Err(AppError::Conflict(
                    "Relay made no progress for too long; retry the commit shortly".into(),
                ));
            }
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        }
    }
    pub(crate) async fn commit(&self, id: &str) -> AppResult<Response> {
        {
            let gate = self.gate(id);
            let _lock = gate.lock().await;
            let task = self.record(id)?;
            if task.status == "completed" {
                return Ok(Response::Ack);
            }
            if !super::recovery::processing(&task.status) {
                return self.remote_state(id);
            }
            if !task.scan_complete {
                return Err(AppError::Conflict(
                    "Sender is still scanning the manifest; submission is not available yet".into(),
                ));
            }
            // 提交是本任务最长的收尾步骤（等落盘、等转写、逐文件 rename，
            // 几十万文件时可达数十分钟）。绝不能占着任务锁：否则同一任务的
            // 新会话全部堵死，连接许可被逐个耗尽，表现为"对端必须重启才能连上"。
            if !self.begin_commit(id) {
                // 已有提交在进行：返回当前状态让对端稍后重试。
                return self.remote_state(id);
            }
        }
        let result = self.commit_files(id).await;
        self.finish_commit(id);
        result
    }
    /// 提交的文件落盘部分；不持有任务锁，由 `commit` 用 committing 标记保证唯一性。
    async fn commit_files(&self, id: &str) -> AppResult<Response> {
        // 先等队列排空并记录完成，再做完整性判断与提交。
        self.await_writes(id).await?;
        // 已发布缓存文件若在提交前被删除，必须先让发送端补传；否则转写等待会一直
        // 等一个不会再运行的缓存项，后面的缺失文件检查永远到不了。
        let cached_missing = self.repair_missing_published_cache_files(id).await?;
        if !cached_missing.is_empty() {
            return Ok(Response::Repair {
                files: cached_missing,
            });
        }
        // 缓存中转模式：目标盘临时文件由转写任务连续写，需等转写完成后再 rename。
        self.await_relay(id).await?;
        let task = self.record(id)?;
        if task.completed_count < task.chunk_count {
            let files = task
                .manifest
                .entries
                .iter()
                .enumerate()
                .filter_map(|(file, entry)| {
                    let base = task.manifest.file_base(file);
                    let count = entry.size.div_ceil(task.chunk_size) as usize;
                    (!entry.directory && !(base..base + count).all(|i| task.completed.contains(&i)))
                        .then_some(file)
                })
                .collect();
            return Ok(Response::Repair { files });
        }
        #[cfg(target_os = "android")]
        if self.is_android_direct_receive(&task) {
            let requests = task
                .manifest
                .entries
                .iter()
                .enumerate()
                .filter(|(file, entry)| {
                    !entry.directory && task.targets.get(*file).and_then(Option::as_ref).is_some()
                })
                .map(|(file, _)| self.android_download_request(&task, file))
                .collect::<AppResult<Vec<_>>>()?;
            tokio::task::spawn_blocking(move || {
                for request in requests {
                    crate::android_import::publish_download_file(request)?;
                }
                Ok::<(), String>(())
            })
            .await
            .map_err(|error| AppError::Other(error.to_string()))?
            .map_err(AppError::Other)?;
            self.edit(id, |t| {
                t.status = "completed".into();
                t.error = None;
                t.event("All files were saved and committed. Transfer completion was acknowledged");
            })?;
            self.release_files(id);
            if let Ok(stage) = paths::stage(&task) {
                let _ = std::fs::remove_dir_all(stage);
            }
            self.cleanup_cache_dirs(&task);
            self.release_runtime(id);
            return Ok(Response::Ack);
        }
        let cached_missing: Vec<_> = self
            .missing_published_files(id)
            .await?
            .into_iter()
            .filter(|file| task.cache_files.contains(file) && task.manifest.entries[*file].size > 0)
            .collect();
        if !cached_missing.is_empty() {
            for file in &cached_missing {
                self.invalidate_file(id, *file)?;
            }
            return Ok(Response::Repair {
                files: cached_missing,
            });
        }
        let root = PathBuf::from(task.destination.as_ref().unwrap());
        let stage = paths::stage(&task)?;
        // 逐文件提交全是同步文件系统调用（create_dir_all / rename / exists /
        // sync_all）：几十万个文件会把 tokio 工作线程阻塞很久，因此整体丢到
        // 阻塞线程池执行；这里也统一为"只用同步 API"，不再同步异步混用。
        let broken = {
            let task = task.clone();
            let root = root.clone();
            let stage = stage.clone();
            tokio::task::spawn_blocking(move || {
                let mut broken = Vec::new();
                for file in 0..task.manifest.entries.len() {
                    match Self::commit_staged_file(&task, &root, &stage, file) {
                        Ok(()) => {}
                        Err(AppError::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => {
                            broken.push(file)
                        }
                        Err(AppError::Conflict(_)) => broken.push(file),
                        Err(e) => return Err(e),
                    }
                }
                Ok::<_, AppError>(broken)
            })
            .await
            .map_err(|error| AppError::Other(error.to_string()))??
        };
        if !broken.is_empty() {
            for file in &broken {
                self.invalidate_file(id, *file)?;
            }
            return Ok(Response::Repair { files: broken });
        }
        let missing = self.missing_published_files(id).await?;
        if !missing.is_empty() {
            for file in &missing {
                self.invalidate_file(id, *file)?;
            }
            return Ok(Response::Repair { files: missing });
        }
        #[cfg(target_os = "android")]
        if let Err(error) = crate::android_import::export_received(task.clone()).await {
            self.edit(id, |t| {
                t.error = Some(error.clone());
                t.event("Saving to the selected folder failed. Files remain in the app and can be saved again");
            })?;
            return Err(AppError::Other(error));
        }
        self.edit(id, |t| {
            t.status = "completed".into();
            t.error = None;
            t.event("All files were saved and committed. Transfer completion was acknowledged");
        })?;
        // 传输已结束：释放本任务登记的目标盘句柄，避免最终文件被长时间占用。
        self.release_files(id);
        if paths::ensure_under(&root, &stage).is_ok() {
            let _ = std::fs::remove_dir_all(&stage);
        }
        self.cleanup_cache_dirs(&task);
        // 任务已终态：整体释放运行期资源（门锁、句柄、标志）。
        self.release_runtime(id);
        Ok(Response::Ack)
    }

    fn cleanup_cache_dirs(&self, task: &super::model::TaskRecord) {
        if !task.use_cache {
            return;
        }
        let Some(cache_root) = self.cache_root_for_task(&task.id) else {
            return;
        };
        for dir in paths::cache_task_dirs(&cache_root, task) {
            match std::fs::remove_dir_all(&dir) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    tracing::warn!(dir = %dir.display(), %error, "Failed to clean the receive cache directory");
                    relay::record_pending_cleanup(self, &dir);
                }
            }
        }
    }
}

/// 清单分片处理耗时日志：慢盘上单帧可能要数秒，超过阈值升级为警告，
/// 便于把"扫描慢"与"连接断了"区分开（发送端超时会报"网络读取超时"）。
fn log_manifest_chunk(
    id: &str,
    first: usize,
    entries: usize,
    elapsed: std::time::Duration,
    accepted: bool,
) {
    if elapsed >= crate::tuning::SLOW_REQUEST_WARN {
        tracing::warn!(
            task = %id,
            first,
            entries,
            accepted,
            elapsed_ms = elapsed.as_millis() as u64,
            "Manifest chunk processing is taking a long time (database write and target planning)"
        );
    } else {
        // INFO 级：确认接收后每个分片是否真的在推进，需要能从默认日志看出来。
        tracing::info!(
            task = %id,
            first,
            entries,
            accepted,
            elapsed_ms = elapsed.as_millis() as u64,
            "Manifest chunk processed"
        );
    }
}
