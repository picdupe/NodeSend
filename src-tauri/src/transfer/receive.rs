use super::{
    model::{incoming_id, ConflictPolicy, FileEntry, Manifest, OfferSummary, TaskRecord},
    paths, relay, TransferService,
};
use crate::{
    error::{AppError, AppResult},
    network::protocol::{Request, Response},
};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};
impl TransferService {
    pub(crate) async fn offer(
        &self,
        peer: &str,
        name: &str,
        wire: &str,
        summary: OfferSummary,
        transport: &str,
    ) -> AppResult<Response> {
        if wire.len() != 32 || !wire.bytes().all(|c| c.is_ascii_hexdigit()) {
            return Err(AppError::BadRequest("任务 ID 非法".into()));
        }
        let id = incoming_id(peer, wire);
        if self.status_of(&id).is_ok() {
            let existing_status = self.status_of(&id)?;
            let (digest, received) = self.manifest_received(&id)?;
            // 摘要（含指纹）必须一致，否则视为内容已变。
            let existing = self.record(&id)?;
            if (!summary.streaming && digest != summary.digest)
                || existing.top_level_fingerprint != summary.top_level_fingerprint
            {
                return Err(AppError::Conflict("已创建的任务内容不能变更".into()));
            }
            if !received {
                if ["accepted", "transferring", "paused"].contains(&existing_status.as_str()) {
                    let recovered_count = existing.manifest.entries.len();
                    if recovered_count != existing.file_count {
                        self.update_scan_progress(
                            &id,
                            recovered_count,
                            existing.manifest.entries.iter().filter(|entry| entry.directory).count(),
                            existing.manifest.total(),
                            existing.manifest.chunk_count(),
                            existing.manifest.entries.last().map(|entry| entry.path.as_str()).unwrap_or_default(),
                        )?;
                    }
                    return Ok(Response::ManifestResume {
                        next_index: recovered_count,
                    });
                }
                // 上次连接在清单传输中途中断：清空已收片段，重新接收完整清单。
                self.clear_entries(&id)?;
                return Ok(Response::Ready);
            }
            return self.remote_state(&id);
        }
        if self
            .snapshots()
            .iter()
            .filter(|t| t.direction == "receive" && t.status == "waiting")
            .count()
            >= crate::tuning::MAX_PENDING_REQUESTS
        {
            return Err(AppError::Forbidden("待处理请求已满".into()));
        }
        let now = chrono::Utc::now().to_rfc3339();
        let task = TaskRecord {
            id: id.clone(),
            wire_id: wire.into(),
            peer_id: peer.into(),
            peer_name: name.into(),
            direction: "receive".into(),
            status: "waiting".into(),
            sources: vec![],
            destination: None,
            conflict: ConflictPolicy::Rename,
            transport: Some(transport.into()),
            error: None,
            events: vec!["已验证发送方身份，正在接收文件清单".into()],
            updated_at: now.clone(),
            created_at: now,
            allow_public: false,
            use_cache: false,
            chunk_size: summary.chunk_size,
            verify_hash: summary.verify_hash,
            file_count: summary.file_count,
            directory_count: summary.directory_count,
            total_size: summary.total_size,
            chunk_count: summary.chunk_count,
            top_level_count: summary.top_level_count,
            completed_count: 0,
            completed_bytes: 0,
            relayed_total: 0,
            relay_target_bytes: 0,
            skipped_bytes: 0,
            skipped_paths: vec![],
            digest: summary.digest,
            top_level_fingerprint: summary.top_level_fingerprint,
            scan_last_seq: 0,
            scan_last_path: String::new(),
            scan_complete: false,
            manifest: Manifest::default(),
            completed: BTreeSet::new(),
            targets: vec![],
            relayed: vec![],
            skipped_files: BTreeSet::new(),
        };
        // 明细不在此驻留：由 `ManifestChunk` 分片送达并逐片落 `task_entries`。
        self.insert(task)?;
        // 告诉发送端可以开始发送分片清单。
        Ok(Response::Ready)
    }
    /// 接收一段清单分片：逐片写入 `task_entries`（边收边落库，不驻留内存），返回确认。
    pub(crate) async fn manifest_chunk(
        &self,
        peer: &str,
        wire: &str,
        first: usize,
        entries: Vec<FileEntry>,
    ) -> AppResult<Response> {
        let id = incoming_id(peer, wire);
        let gate = self.gate(&id);
        let guard = gate.lock().await;
        let started = std::time::Instant::now();
        // Metadata continues to arrive after an early user decision; terminal
        // tasks ignore retries, while accepted tasks extend their target plan.
        let status = self.status_of(&id)?;
        if !["waiting", "accepted", "transferring", "paused"].contains(&status.as_str()) {
            tracing::info!(task = %id, first, status = %status, "清单分片已到，但任务已处于终态，直接确认");
            return Ok(Response::ManifestAck);
        }
        let (current, mut total_size, mut directory_count, mut chunk_count, chunk_size, top_level_count) = {
            let tasks = crate::locks::lock(&self.tasks);
            let task = tasks
                .get(&id)
                .ok_or_else(|| AppError::NotFound("接收任务不存在".into()))?;
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
            tracing::debug!(task = %id, first, current, entries = entries.len(), "清单分片已收到过，直接确认");
            return Ok(Response::ManifestAck);
        }
        if first != current {
            return Err(AppError::Conflict("清单分片顺序不连续".into()));
        }
        if status != "waiting" {
            let destination = {
                let tasks = crate::locks::lock(&self.tasks);
                tasks.get(&id).and_then(|task| task.destination.clone())
            }
            .ok_or_else(|| AppError::Conflict("接收目标尚未确定".into()))?;
            let additional_size = entries.iter().map(|entry| entry.size).sum::<u64>();
            // 可用空间查询是同步调用，且每个清单分片都会走一次：放到阻塞线程池，
            // 否则慢盘/网络盘会把 tokio 工作线程占住。
            let free = tokio::task::spawn_blocking(move || {
                Ok::<_, AppError>(fs2::available_space(Path::new(&destination))?)
            })
            .await
            .map_err(|error| AppError::Other(error.to_string()))??;
            if free < additional_size {
                return Err(AppError::Conflict("接收目录剩余空间不足，已暂停后续数据".into()));
            }
        }
        if status != "waiting" {
            let mut map = crate::locks::lock(&self.tasks);
            let task = map
                .get_mut(&id)
                .ok_or_else(|| AppError::NotFound("接收任务不存在".into()))?;
            let destination = PathBuf::from(
                task.destination
                    .as_deref()
                    .ok_or_else(|| AppError::Conflict("接收目标尚未确定".into()))?,
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
                    AppError::Conflict("扫描期间收到非顶层条目，清单顺序非法".into())
                })?;
                let root_target = roots
                    .iter()
                    .find(|(name, _)| name == top)
                    .and_then(|(_, target)| target.clone());
                let mut target = root_target.map(|path| {
                    let mut path = PathBuf::from(path);
                    path.push(tail);
                    path
                });
                if let Some(path) = target.as_ref() {
                    // 一帧最多 5000 条，这里不能做 canonicalize 之类的重活：
                    // 词法校验 + 仅在策略需要时探测文件系统。
                    paths::ensure_relative_safe(&entry.path)?;
                    let probe = entry.directory
                        || matches!(task.conflict, ConflictPolicy::Skip | ConflictPolicy::Overwrite);
                    if probe {
                        if path.exists() && !entry.directory && matches!(task.conflict, ConflictPolicy::Skip) {
                            target = None;
                        } else if path.exists() && entry.directory != path.is_dir() {
                            return Err(AppError::Conflict(format!("路径类型冲突：{}", entry.path)));
                        }
                    }
                }
                let file_index = task.file_count;
                if let Some(path) = target.as_ref() {
                    target_rows.push((file_index, path.to_string_lossy().into_owned()));
                    task.targets.push(Some(path.to_string_lossy().into_owned()));
                    if !entry.directory {
                        task.relay_target_bytes = task.relay_target_bytes.saturating_add(entry.size);
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
                            task.completed_bytes = task.completed_bytes.saturating_add(
                                task.chunk_size.min(entry.size.saturating_sub(
                                    (index - next_chunk) as u64 * task.chunk_size,
                                )),
                            );
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
            task.scan_last_path = entries.last().map(|entry| entry.path.clone()).unwrap_or_default();
            task.updated_at = chrono::Utc::now().to_rfc3339();
            // `with_transaction` 内部持有唯一连接守卫，闭包拿不到连接，
            // 因此"持连接再取连接"这种非重入死锁在这里写不出来。
            self.app.db.with_transaction(|tx| {
                {
                    let mut stmt = tx.prepare("INSERT OR REPLACE INTO task_entries(task_id,index_no,path,size,modified_ms,directory) VALUES(?1,?2,?3,?4,?5,?6)")?;
                    for (offset, entry) in entries.iter().enumerate() {
                        stmt.execute(rusqlite::params![
                            id,
                            (first + offset) as i64,
                            entry.path,
                            entry.size as i64,
                            entry.modified_ms as i64,
                            entry.directory as i64
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
            log_manifest_chunk(&id, first, entries.len(), started.elapsed(), true);
            return Ok(Response::ManifestAck);
        }
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
            entries.last().map(|entry| entry.path.as_str()).unwrap_or_default(),
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
                "顶层清单处理完毕并已回执"
            );
        }
        Ok(Response::ManifestAck)
    }

    /// 按接收策略自动接收：仅当策略为 trusted（或对该设备单独开启自动接收）
    /// 且对方在信任列表中，并且清单顶层条目已到达时生效。
    pub(crate) async fn auto_accept_if_trusted(&self, peer: &str, id: &str) -> bool {
        if !matches!(self.status_of(id).as_deref(), Ok("waiting")) {
            return false;
        }
        let policy = self
            .app
            .get_setting("receive_policy")
            .ok()
            .flatten()
            .unwrap_or_else(|| "ask".into());
        let per_peer = self
            .app
            .get_setting(&format!("auto_receive_{peer}"))
            .ok()
            .flatten()
            .as_deref()
            == Some("true");
        if policy != "trusted" && !per_peer {
            return false;
        }
        let trusted = crate::registry::list(&self.app.db)
            .map(|nodes| nodes.iter().any(|node| node.node_id == peer))
            .unwrap_or(false);
        if !trusted {
            return false;
        }
        let destination = self.default_destination();
        if let Err(error) = std::fs::create_dir_all(&destination) {
            tracing::warn!(%peer, %error, "信任设备自动接收：创建接收目录失败，保留手动确认");
            return false;
        }
        match self
            .decide(id, true, Some(destination), ConflictPolicy::Rename, None)
            .await
        {
            Ok(()) => {
                let _ = self.edit(id, |task| task.event("已信任的设备：自动接收，无需手动确认"));
                tracing::info!(%peer, task = %id, "信任设备顶层清单已到齐，自动接收");
                true
            }
            Err(error) => {
                tracing::warn!(%peer, task = %id, %error, "信任策略自动接收失败，保留手动确认");
                false
            }
        }
    }
    /// 清单分片收完：载入完整明细、校验指纹与合法性，再按策略处理接收请求。
    pub(crate) async fn manifest_done(
        &self,
        peer: &str,
        wire: &str,
        file_count: usize,
        total_size: u64,
        digest: String,
    ) -> AppResult<Response> {
        let id = incoming_id(peer, wire);
        let initial_status = self.status_of(&id)?;
        if !["waiting", "accepted", "transferring", "paused"].contains(&initial_status.as_str()) {
            return self.remote_state(&id);
        }
        // 一次性载入完整清单（之后运行期一直复用），校验指纹与结构。
        let task = self.record(&id)?;
        let actual_file_count = task.manifest.entries.len();
        let actual_total_size = task.manifest.total();
        let actual_digest = task.manifest.digest();
        if file_count != actual_file_count || total_size != actual_total_size {
            return Err(AppError::Conflict("清单最终计数与实际条目不一致".into()));
        }
        if digest != actual_digest || (!task.digest.is_empty() && task.digest != digest) {
            return Err(AppError::Conflict("文件清单校验失败（指纹不一致）".into()));
        }
        task.manifest.validate()?;
        // ManifestDone is the authoritative end-of-scan summary. Keeping this
        // update here allows a future streaming Offer to omit unknown totals.
        self.edit(&id, |record| {
            record.file_count = file_count;
            record.directory_count = record
                .manifest
                .entries
                .iter()
                .filter(|entry| entry.directory)
                .count();
            record.total_size = total_size;
            record.chunk_count = record.manifest.chunk_count();
            record.digest = digest.clone();
            record.scan_complete = true;
        })?;
        if initial_status == "waiting" {
            let policy = self
                .app
                .get_setting("receive_policy")?
                .unwrap_or_else(|| "ask".into());
            if policy == "reject" {
                self.decide(&id, false, None, ConflictPolicy::Rename, None)
                    .await?;
            } else {
                // 顶层清单通常早就到齐并已自动接收；这里兜底处理"策略后改"的情况。
                self.auto_accept_if_trusted(peer, &id).await;
            }
        }
        self.remote_state(&id)
    }
    pub(crate) fn remote_state(&self, id: &str) -> AppResult<Response> {
        let (status, completed) = self.status_and_completed(id)?;
        Ok(Response::State { status, completed })
    }
    pub(crate) async fn receive_chunk(
        &self,
        id: &str,
        index: usize,
        data: Vec<u8>,
        hash: Option<String>,
        transport: &str,
    ) -> AppResult<Response> {
        // 缓存盘中转的背压等待必须在任务锁之外：缓存盘写满时可能等很久，
        // 占着任务锁会把同一任务的所有会话一起拖住（见下方入队处注释）。
        if self.use_cache_of(id)? {
            relay::await_cache_space(self, id).await?;
            let status = self.status_of(id)?;
            if !["accepted", "transferring"].contains(&status.as_str()) {
                return self.remote_state(id);
            }
        }
        let gate = self.gate(id);
        let lock = gate.lock().await;
        let status = self.status_of(id)?;
        if !["accepted", "transferring"].contains(&status.as_str()) {
            return self.remote_state(id);
        }
        let chunk = self
            .chunk_at(id, index)?
            .ok_or_else(|| AppError::BadRequest("Chunk 编号越界".into()))?;
        if self.is_skipped(id, chunk.file) {
            return Ok(Response::Ack);
        }
        if data.len() != chunk.length {
            return Err(AppError::BadRequest("Chunk 长度不匹配".into()));
        }
        if self.verify_hash_of(id)?
            && hash.as_deref() != Some(format!("{:x}", Sha256::digest(&data)).as_str())
        {
            return Err(AppError::Conflict("Chunk SHA-256 校验失败".into()));
        }
        let runtime = self.runtime(id);
        // 已完成，或已入队但尚未落盘：重复块直接确认，不重复入队。
        if self.is_completed(id, index) || crate::locks::lock(&runtime.write_pending).contains(&index) {
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
        match tokio::time::timeout(super::write_queue::QUEUE_WAIT_TIMEOUT, self.write_tx.send(pending)).await {
            Ok(Ok(())) => Ok(Response::Ack),
            Ok(Err(_)) => {
                crate::locks::lock(&runtime.write_pending).remove(&index);
                Err(AppError::Other("接收写入队列已关闭".into()))
            }
            Err(_) => {
                crate::locks::lock(&runtime.write_pending).remove(&index);
                tracing::warn!(task = %id, index, "写入队列长时间无空间，放弃本次入队");
                Err(AppError::Conflict(
                    "接收写入队列长时间无空间，请检查目标磁盘写入速度".into(),
                ))
            }
        }
    }
    /// 等待本任务所有已入队块完成「写入 + sync_data + 记录完成」。
    /// 超时说明写入任务异常或存在长期缺口，改由调用方报错，避免永久阻塞。
    async fn await_writes(&self, id: &str) -> AppResult<()> {
        let deadline = tokio::time::Instant::now() + crate::tuning::AWAIT_WRITES_TIMEOUT;
        let runtime = self.runtime(id);
        loop {
            let busy = !crate::locks::lock(&runtime.write_pending).is_empty();
            if !busy {
                return Ok(());
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(AppError::Conflict(
                    "仍有未落盘的 Chunk，请稍后重试提交".into(),
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
            if relay::relay_done(&state) {
                return Ok(());
            }
            if let Some(err) = state.error {
                return Err(AppError::Other(err));
            }
            if !["accepted", "transferring"].contains(&state.status.as_str()) {
                return Err(AppError::Conflict("转写已停止，请稍后重试提交".into()));
            }
            let progress = state.relayed_total;
            if progress > last {
                last = progress;
                stalled_since = tokio::time::Instant::now();
            } else if stalled_since.elapsed() >= crate::tuning::RELAY_STALL_TIMEOUT {
                return Err(AppError::Conflict(
                    "转写长时间无进展，请稍后重试提交".into(),
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
            if !["accepted", "transferring"].contains(&task.status.as_str()) {
                return self.remote_state(id);
            }
            if !task.scan_complete {
                return Err(AppError::Conflict("发送方仍在扫描清单，暂不能提交".into()));
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
        // 缓存中转模式：目标盘临时文件由转写任务连续写，需等转写完成后再 rename。
        self.await_relay(id).await?;
        let task = self.record(id)?;
        if task.completed_count != task.chunk_count {
            return Err(AppError::Conflict("仍有未提交 Chunk".into()));
        }
        let root = PathBuf::from(task.destination.as_ref().unwrap());
        let stage = paths::stage(&task)?;
        // 逐文件提交全是同步文件系统调用（create_dir_all / rename / exists /
        // sync_all）：几十万个文件会把 tokio 工作线程阻塞很久，因此整体丢到
        // 阻塞线程池执行；这里也统一为"只用同步 API"，不再同步异步混用。
        {
            let task = task.clone();
            let root = root.clone();
            let stage = stage.clone();
            tokio::task::spawn_blocking(move || Self::commit_staged_files(&task, &root, &stage))
                .await
                .map_err(|error| AppError::Other(error.to_string()))??;
        }
        #[cfg(target_os = "android")]
        if let Err(error) = crate::android_import::export_received(task.clone()).await {
            self.edit(id, |t| {
                t.error = Some(error.clone());
                t.event("保存到所选目录失败，文件仍保留在应用内，可重新保存");
            })?;
            return Err(AppError::Other(error));
        }
        self.edit(id, |t| {
            t.status = "completed".into();
            t.error = None;
            t.event("所有文件已持久化并提交，发送完成回执");
        })?;
        // 传输已结束：释放本任务登记的目标盘句柄，避免最终文件被长时间占用。
        self.release_files(id);
        // Only generated staging files inside this task directory are cleaned.
        if paths::ensure_under(&root, &stage).is_ok() {
            let _ = std::fs::remove_dir_all(&stage);
        }
        // 任务已终态：整体释放运行期资源（门锁、句柄、标志）。
        self.release_runtime(id);
        Ok(Response::Ack)
    }
/// 逐文件把缓存/暂存文件落到最终位置（同步实现，由 `commit_files` 丢进阻塞线程池）。
///
/// 全部使用同步文件系统 API：`commit` 阶段是几十万次 `rename`/`create_dir_all`，
/// 放在 tokio 工作线程上会阻塞同运行时上的其它任务。
    fn commit_staged_files(task: &TaskRecord, root: &Path, stage: &Path) -> AppResult<()> {
    std::fs::create_dir_all(stage)?;
    for (i, entry) in task.manifest.entries.iter().enumerate() {
        let Some(target) = task.targets.get(i).and_then(Option::as_ref) else {
            continue;
        };
        let target = Path::new(target);
        paths::ensure_under(root, target)?;
        if entry.directory {
            std::fs::create_dir_all(target)?;
            continue;
        }
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let part = stage.join(format!("{i}.part"));
        let done = stage.join(format!("{i}.committed"));
        paths::ensure_under(root, &part)?;
        paths::ensure_under(root, &done)?;
        // Rename the staged file into place. Android shared storage and
        // some desktop filesystems forbid hard links; rename stays on the
        // same filesystem and is atomic for the normal receive path.
        if done.exists() {
            continue;
        }
        if !part.exists() && entry.size == 0 {
            std::fs::File::create(&part)?.sync_all()?;
        }
        if std::fs::metadata(&part)?.len() != entry.size {
            return Err(AppError::Conflict("接收文件大小与清单不匹配".into()));
        }
        if target.exists() {
            if same_file::is_same_file(&part, target).unwrap_or(false) {
                std::fs::File::create(&done)?.sync_all()?;
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
        std::fs::File::create(&done)?.sync_all()?;
    }
    Ok(())
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
                if !["accepted", "transferring", "paused"].contains(&task.status.as_str()) {
                    return self.remote_state(&id);
                }
                let entry = task
                    .manifest
                    .entries
                    .get(file)
                    .filter(|e| !e.directory)
                    .ok_or_else(|| AppError::BadRequest("文件编号非法".into()))?;
                let part = paths::stage(&task)?.join(format!("{file}.part"));
                paths::ensure_plain(&part)?;
                match tokio::fs::remove_file(&part).await {
                    Ok(()) => {}
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => return Err(e.into()),
                }
                // 缓存中转模式：一并删除该文件的缓存分段，避免继续占用缓存盘。
                if task.use_cache {
                    if let Some(cache_root) = self.cache_root.as_ref() {
                        for segment in 0..entry.size.div_ceil(relay::SEG_BYTES) {
                            let _ = tokio::fs::remove_file(paths::cache_segment(
                                cache_root, &id, file, segment,
                            ))
                            .await;
                        }
                    }
                }
                self.mark_skipped(&id, file)?;
                // 该文件已跳过：释放可能已打开的目标盘句柄（`.part` 已删除）。
                if let Some(runtime) = self.runtime_if_exists(&id) {
                    crate::locks::lock(&runtime.receive_files).remove(&file);
                }
                Ok(Response::Ack)
            }
            Request::Offer { id, summary } => self.offer(peer, name, &id, summary, transport).await,
            Request::ManifestChunk { id, first, entries } => {
                self.manifest_chunk(peer, &id, first, entries).await
            }
            Request::ManifestDone {
                id,
                file_count,
                total_size,
                digest,
            } => {
                self.manifest_done(peer, &id, file_count, total_size, digest)
                    .await
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
            "清单分片处理耗时较长（落库与目标规划）"
        );
    } else {
        // INFO 级：确认接收后每个分片是否真的在推进，需要能从默认日志看出来。
        tracing::info!(
            task = %id,
            first,
            entries,
            accepted,
            elapsed_ms = elapsed.as_millis() as u64,
            "清单分片已处理"
        );
    }
}
