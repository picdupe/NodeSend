use super::{
    model::{Manifest, ManifestDigest, TaskRecord},
    source, TransferService, MANIFEST_BATCH, MANIFEST_PAGE,
};
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
    collections::{BTreeSet, VecDeque},
    sync::{
        atomic::{AtomicI64, Ordering},
        Arc,
    },
};
use tokio::sync::Mutex;
/// 流式扫描的共享状态。
///
/// 两条入口——"全新发送"与"接收端已有该任务时补齐剩余扫描"——此前各写了一份
/// 结构相同、细节略有差异的循环（改一处漏一处是隐患）。现在统一走
/// [`TransferService::stream_scan`]，差异只用下面的开关表达。
struct ScanStream {
    /// 已交付接收端的清单前缀长度；小于它的分片不再重发（续传）。
    resume_manifest_at: usize,
    /// 扫描过程中是否顺带发送已就绪文件的数据（补齐扫描时为 false：数据早前已发）。
    send_data: bool,
    entries: Vec<super::model::FileEntry>,
    pending_files: VecDeque<(usize, usize)>,
    sent_chunks: BTreeSet<usize>,
    chunk_count: usize,
    total_size: u64,
    directories: usize,
    digest: ManifestDigest,
    /// 已扫描但尚未落库的条目：累计到 [`MANIFEST_PAGE`] 再一次性写库，
    /// 这样每帧仍按 [`MANIFEST_BATCH`] 小批量发送（先发先算），
    /// 但数据库事务按 5000 条摊销。
    db_buffer: Vec<super::model::FileEntry>,
    /// `db_buffer` 首条在清单中的下标。
    db_buffer_start: usize,
    cache_restart: Option<Vec<usize>>,
}
impl TransferService {
    async fn prepare_remote_file(
        &self,
        channel: &mut Channel,
        task: &TaskRecord,
        file: usize,
    ) -> AppResult<Option<Vec<usize>>> {
        loop {
            if !self.wait_remote_resume(&task.id).await? {
                return Err(AppError::Conflict(
                    "Remote task ended before the file could be prepared".into(),
                ));
            }
            match channel
                .request(&Request::PrepareFile {
                    id: task.wire_id.clone(),
                    file,
                })
                .await?
            {
                Response::Ack => return Ok(None),
                Response::Repair { files } => return Ok(Some(files)),
                Response::State {
                    status, revision, ..
                } if ["paused", "remote_paused"].contains(&status.as_str()) => {
                    if status == "paused" {
                        self.apply_remote_state(&task.peer_id, &task.wire_id, revision, "paused")
                            .await?;
                    }
                }
                _ => {
                    return Err(AppError::Conflict(
                        "Unexpected response while preparing the remote file".into(),
                    ))
                }
            }
        }
    }

    async fn source_read_failed(
        &self,
        control: &mut Channel,
        task: &TaskRecord,
        file: usize,
        error: AppError,
    ) -> AppError {
        let kind = match &error {
            AppError::Conflict(_) => "changed",
            AppError::Io(e) if e.kind() == std::io::ErrorKind::NotFound => "missing",
            _ => "unavailable",
        };
        if let Err(e) = self.set_source_issue(&task.id, file, kind) {
            return e;
        }
        let _ = control
            .request(&Request::SourceIssue {
                id: task.wire_id.clone(),
                file,
                kind: kind.into(),
            })
            .await;
        let _ = self.wait_remote_resume(&task.id).await;
        if self
            .status_of(&task.id)
            .is_ok_and(|s| s == "partial_completed")
        {
            let runtime = self.runtime(&task.id);
            let mut updates = runtime.state_updates.subscribe();
            let revision = self.record(&task.id).map(|t| t.state_revision).unwrap_or(0);
            let _ = tokio::time::timeout(std::time::Duration::from_secs(5), async {
                while runtime.state_sent_revision.load(Ordering::SeqCst) < revision
                    && runtime.state_sessions.load(Ordering::SeqCst) > 0
                {
                    if updates.changed().await.is_err() {
                        break;
                    }
                }
            })
            .await;
        }
        AppError::Conflict(error.to_string())
    }

    fn flush_scan_buffer(
        &self,
        task: &TaskRecord,
        state: &mut ScanStream,
        next_index: usize,
    ) -> AppResult<()> {
        if state.db_buffer.is_empty() {
            return Ok(());
        }
        let start = state.db_buffer_start;
        let buffered = std::mem::take(&mut state.db_buffer);
        state.db_buffer_start = next_index;
        self.append_source_batch(
            &task.id,
            start,
            &buffered,
            state.directories,
            state.total_size,
            state.chunk_count,
        )
    }

    /// 从 `scanner`（已定位到 `first` 之前）继续逐批扫描并增量交付清单，
    /// 直到扫完；返回扫描结束时的条目游标。
    ///
    /// 每批都：发清单分片（必要时跳过已交付前缀）→ 累加摘要/计数 →
    /// 累计到落库批次后写库并推进游标 →（可选）发送已就绪文件的数据。
    /// 任何一步发现任务被停止都会立即返回。
    async fn stream_scan(
        &self,
        control: &mut Channel,
        task: &TaskRecord,
        state: &mut ScanStream,
        mut scanner: source::SourceScanner,
        mut first: usize,
    ) -> AppResult<usize> {
        loop {
            if ["paused", "remote_paused"].contains(&self.status_of(&task.id)?.as_str()) {
                self.wait_remote_resume(&task.id).await?;
            }
            if self.stopped(&task.id)? {
                self.flush_scan_buffer(task, state, first)?;

                return Ok(first);
            }
            let batch = tokio::task::spawn_blocking(move || {
                let batch = scanner.next_batch(MANIFEST_BATCH)?;
                Ok::<_, AppError>((scanner, batch))
            })
            .await
            .map_err(|e| AppError::Other(e.to_string()))??;
            scanner = batch.0;
            let page = batch.1;
            if page.is_empty() {
                break;
            }
            let send_offset = state
                .resume_manifest_at
                .saturating_sub(first)
                .min(page.len());
            if send_offset < page.len() {
                let requested = std::time::Instant::now();
                let response = control
                    .request(&Request::ManifestChunk {
                        id: task.wire_id.clone(),
                        first: first + send_offset,
                        entries: page[send_offset..].to_vec(),
                    })
                    .await?;
                let elapsed = requested.elapsed();
                if elapsed >= crate::tuning::SLOW_REQUEST_WARN {
                    tracing::warn!(
                        task = %task.wire_id,
                        first = first + send_offset,
                        entries = page.len() - send_offset,
                        elapsed_ms = elapsed.as_millis() as u64,
                        "Receiver is taking a long time to process manifest chunks"
                    );
                } else {
                    tracing::info!(
                        task = %task.wire_id,
                        first = first + send_offset,
                        entries = page.len() - send_offset,
                        elapsed_ms = elapsed.as_millis() as u64,
                        "Manifest chunk confirmed"
                    );
                }
                match response {
                    Response::ManifestAck => {}
                    Response::State { status, .. }
                        if ["cancelled", "rejected"].contains(&status.as_str()) =>
                    {
                        self.edit(&task.id, |t| t.status = status)?;
                        return Ok(first);
                    }
                    _ => {
                        return Err(AppError::Other(
                            "Receiver did not confirm the manifest chunk".into(),
                        ))
                    }
                }
            }
            for (page_offset, entry) in page.iter().enumerate() {
                state.digest.update(entry);
                state.total_size = state.total_size.saturating_add(entry.size);
                state.directories += usize::from(entry.directory);
                if !entry.directory && entry.size > 0 {
                    if state.send_data {
                        state
                            .pending_files
                            .push_back((first + page_offset, state.chunk_count));
                    }
                    state.chunk_count += entry.size.div_ceil(task.chunk_size) as usize;
                }
            }
            state.entries.extend(page.iter().cloned());
            state.db_buffer.extend(page.iter().cloned());
            first += page.len();
            self.update_live_scan_progress(&task.id, first, state.directories)?;
            // 扫满一个落库批次才写数据库：扫描/发送按 256 条小步推进，
            // 数据库按 5000 条摊销事务；两者互不阻塞。
            if state.db_buffer.len() >= MANIFEST_PAGE {
                let start = state.db_buffer_start;
                let buffered = std::mem::take(&mut state.db_buffer);
                state.db_buffer_start = first;
                self.append_source_batch(
                    &task.id,
                    start,
                    &buffered,
                    state.directories,
                    state.total_size,
                    state.chunk_count,
                )?;
                // 每 4 个落库批次（约 2 万条）写一条任务事件：界面能直接看出清单在推进。
                if (first / MANIFEST_PAGE).is_multiple_of(4) || first <= MANIFEST_PAGE {
                    self.edit(&task.id, |t| {
                        t.event(format!(
                            "Sending manifest in parts: {first} entries scanned"
                        ))
                    })?;
                }
            }
            if ["paused", "remote_paused"].contains(&self.status_of(&task.id)?.as_str()) {
                self.wait_remote_resume(&task.id).await?;
            }
            if self.stopped(&task.id)? {
                self.flush_scan_buffer(task, state, first)?;

                return Ok(first);
            }
            if state.send_data {
                self.flush_scan_buffer(task, state, first)?;
                if let Some(files) = self
                    .send_streaming_ready_files(
                        control,
                        task,
                        &state.entries,
                        &mut state.pending_files,
                        &mut state.sent_chunks,
                    )
                    .await?
                {
                    state.cache_restart = Some(files);
                    return Ok(first);
                }
            }
        }
        // 收尾：把不足一个落库批次的剩余条目写库，游标推进到扫描终点。
        if !state.db_buffer.is_empty() {
            let start = state.db_buffer_start;
            let buffered = std::mem::take(&mut state.db_buffer);
            state.db_buffer_start = first;
            self.append_source_batch(
                &task.id,
                start,
                &buffered,
                state.directories,
                state.total_size,
                state.chunk_count,
            )?;
        }
        Ok(first)
    }
    pub fn start_task(self: &Arc<Self>, peers: Arc<PeerStore>, id: String) {
        if self.runtime(&id).active.swap(true, Ordering::SeqCst) {
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
                let mut attempt = 0u32;
                let mut state_session = None;
                loop {
                    if service.stopped(&id)? {
                        if ["paused", "remote_paused"].contains(&service.status_of(&id)?.as_str()) {
                            if !service.wait_remote_resume(&id).await? {
                                return Ok::<(), AppError>(());
                            }
                            continue;
                        }
                        return Ok(());
                    }
                    match service
                        .send_once(peers.clone(), &id, cooldown.clone(), &mut state_session)
                        .await
                    {
                        Ok(()) if service.status_of(&id)? == "queued" => continue,
                        Ok(())
                            if ["paused", "remote_paused"]
                                .contains(&service.status_of(&id)?.as_str()) =>
                        {
                            if !service.wait_remote_resume(&id).await? {
                                return Ok(());
                            }
                        }
                        Ok(()) => return Ok(()),
                        Err(err) => {
                            if matches!(err, AppError::Conflict(_))
                                && service
                                    .record(&id)?
                                    .source_issue
                                    .as_ref()
                                    .is_some_and(|i| i.accepted)
                            {
                                service.edit(&id, |t| t.status = "queued".into())?;
                                continue;
                            }
                            if ["paused", "remote_paused"]
                                .contains(&service.status_of(&id)?.as_str())
                            {
                                if !service.wait_remote_resume(&id).await? {
                                    return Ok(());
                                }
                                continue;
                            }
                            if service.stopped(&id)? {
                                return Ok(());
                            }
                            let retry = matches!(err, AppError::Io(_) | AppError::Other(_));
                            if !retry {
                                service.edit(&id, |task| {
                                    if ![
                                        "cancelled",
                                        "completed",
                                        "partial_completed",
                                        "rejected",
                                        "incompatible",
                                    ]
                                    .contains(&task.status.as_str())
                                    {
                                        task.status = "paused".into();
                                        task.error = Some(err.to_string());
                                    }
                                })?;
                                if !service.wait_remote_resume(&id).await? {
                                    return Ok(());
                                }
                                continue;
                            }
                            cooldown.store(i64::MAX, Ordering::Relaxed);
                            attempt = attempt.saturating_add(1);
                            service.edit(&id, |t| {
                                if t.status != "interrupted" {
                                    t.recovery_epoch = t.recovery_epoch.saturating_add(1);
                                }
                                if t.status != "interrupted" {
                                    t.status = "connecting".into();
                                }
                                t.error = Some(err.to_string());
                                t.event(format!(
                                    "Transfer connection lost. Retrying in {:?} (attempt {attempt})",
                                    crate::tuning::SEND_RETRY_DELAY
                                ));
                            })?;
                            tokio::time::sleep(crate::tuning::SEND_RETRY_DELAY).await;
                        }
                    }
                }
            };
            let result = tokio::select! {r=run=>r,_=service.shutdown.cancelled()=>Ok(())};
            if let Err(err) = result {
                let _ = service.edit(&id, |t| {
                    if ![
                        "cancelled",
                        "paused",
                        "completed",
                        "partial_completed",
                        "rejected",
                        "incompatible",
                    ]
                    .contains(&t.status.as_str())
                    {
                        t.status = "paused".into();
                        t.error = Some(err.to_string());
                        t.event("The device is temporarily offline. The task is paused; resume when it is online");
                    }
                });
            }
            // 任务结束（含暂停/取消/完成）：释放发送端读句柄，避免常驻占用。
            service.release_send_files(&id);
            // 终态（完成/取消/被拒）时整体释放运行期资源；暂停保留，续传时复用。
            let terminal = service
                .status_of(&id)
                .map(|status| {
                    [
                        "completed",
                        "cancelled",
                        "rejected",
                        "partial_completed",
                        "incompatible",
                    ]
                    .contains(&status.as_str())
                })
                .unwrap_or(false);
            if terminal {
                service.release_runtime(&id);
            } else {
                service.runtime(&id).active.store(false, Ordering::SeqCst);
            }
        });
    }
    fn stopped(&self, id: &str) -> AppResult<bool> {
        Ok([
            "paused",
            "remote_paused",
            "remote_offline",
            "offline",
            "cancelling",
            "cancelled",
            "rejected",
            "completed",
            "partial_completed",
            "incompatible",
            "finishing",
        ]
        .contains(&self.status_of(id)?.as_str()))
    }
    fn queue_for_file_repair(&self, id: &str, files: &[usize]) -> AppResult<()> {
        let task = self.record(id)?;
        let mut ranges = Vec::with_capacity(files.len());
        for file in files {
            let entry = task.manifest.entries.get(*file).ok_or_else(|| {
                AppError::BadRequest("Receiver requested an invalid file index".into())
            })?;
            let start = task.manifest.file_base(*file);
            ranges.push((
                start,
                start.saturating_add(entry.size.div_ceil(task.chunk_size) as usize),
            ));
        }
        self.edit(id, |task| {
            task.completed.retain(|index| {
                !ranges
                    .iter()
                    .any(|(start, end)| index >= start && index < end)
            });
            task.completed_count = task.completed.len();
            task.completed_bytes = task
                .completed
                .iter()
                .filter_map(|index| task.manifest.chunk(*index))
                .map(|chunk| chunk.length as u64)
                .sum();
            task.status = "queued".into();
            task.error = None;
            task.event(format!(
                "Receiver requested retransmission of {} files",
                ranges.len()
            ));
        })?;
        self.save_decision(id)?;
        Ok(())
    }
    async fn channel(
        &self,
        peers: &PeerStore,
        task: &TaskRecord,
        cooldown: &AtomicI64,
        prefer_requested_address: bool,
    ) -> AppResult<Channel> {
        let mut peer = peers.get(&task.peer_id)?;
        if prefer_requested_address {
            if let Some(address) = task.preferred_address.as_deref() {
                peer.endpoints.sort_by_key(|endpoint| {
                    !(endpoint.address == address
                        || address.starts_with(&format!("{}:", endpoint.address))
                        || address.starts_with(&format!("[{}]", endpoint.address)))
                });
            }
        }
        let mut last = "No usable Endpoint".to_string();
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
                    if let Err(quic_error) = transport::hello(&mut channel, &self.app).await {
                        if channel.kind != "quic" {
                            last = quic_error.to_string();
                            peers.failure(&task.peer_id, endpoint, last.clone());
                            continue;
                        }
                        tracing::warn!(
                            peer_id = %task.peer_id,
                            address = %endpoint.address,
                            error = %quic_error,
                            "QUIC HELLO/read failed; retry immediately over TCP"
                        );
                        cooldown.store(i64::MAX, Ordering::Relaxed);
                        let mut tcp_channel = match transport::connect(
                            &self.app,
                            endpoint,
                            Some(&task.peer_id),
                            true,
                            task.allow_public,
                        )
                        .await
                        {
                            Ok(channel) => channel,
                            Err(tcp_error) => {
                                last = format!(
                                    "QUIC read failed: {quic_error}; TCP fallback failed: {tcp_error}"
                                );
                                peers.failure(&task.peer_id, endpoint, last.clone());
                                continue;
                            }
                        };
                        if let Err(tcp_error) = transport::hello(&mut tcp_channel, &self.app).await
                        {
                            last = format!(
                                "QUIC read failed: {quic_error}; TCP HELLO failed: {tcp_error}"
                            );
                            peers.failure(&task.peer_id, endpoint, last.clone());
                            continue;
                        }
                        channel = tcp_channel;
                    }
                    peers.seen(&task.peer_id, endpoint);
                    self.edit(&task.id, |t| {
                        if t.transport.as_deref() != Some(channel.kind) {
                            t.event(format!(
                                "Using {} + TLS 1.3 via {}",
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
    /// 补齐未完成的清单扫描（恢复/重试路径）。
    ///
    /// Offer 返回 `Ready` 才会走上面的流式扫描；接收端已经存在同名任务时返回
    /// 的是状态帧，扫描阶段被跳过，`scan_complete` 会一直是 false：小任务表现为
    /// "数据都传完了还显示正在扫描"，大目录则永远发不完剩余清单。这里按持久化
    /// 游标继续扫描、补发分片，最后发 `ManifestDone` 并落 `scan_complete`。
    async fn complete_source_scan(&self, control: &mut Channel, id: &str) -> AppResult<()> {
        let task = self.record(id)?;
        if task.manifest.entries.is_empty() && task.sources.is_empty() {
            return Ok(());
        }
        let wire_id = task.wire_id.clone();
        let selected = task
            .sources
            .iter()
            .map(|root| root.path.clone())
            .collect::<Vec<_>>();
        let cursor = task.file_count;
        let last_path = task.scan_last_path.clone();
        let directories = task.directory_count;
        let total_size = task.total_size;
        let chunk_count = task.chunk_count;
        tracing::info!(task = %id, cursor, "Manifest scan is incomplete; continuing with remaining entries");
        self.edit(id, |t| {
            t.event(format!(
                "The receiver already has this task. Continuing the remaining manifest from entry {cursor}"
            ))
        })?;
        let cutoff = chrono::DateTime::parse_from_rfc3339(&task.created_at)
            .map(|t| t.timestamp_millis() as u64)
            .unwrap_or(u64::MAX);
        let scanner = tokio::task::spawn_blocking(move || {
            let mut scanner = source::SourceScanner::new(selected)?;
            scanner.set_cutoff(cutoff);
            scanner.resume_after(cursor, &last_path)?;
            Ok::<_, AppError>(scanner)
        })
        .await
        .map_err(|e| AppError::Other(e.to_string()))??;
        // 与全新发送共用同一段扫描循环（send_data=false：数据早已发出）。
        let mut stream = ScanStream {
            resume_manifest_at: cursor,
            send_data: false,
            entries: Vec::new(),
            pending_files: VecDeque::new(),
            sent_chunks: BTreeSet::new(),
            chunk_count,
            total_size,
            directories,
            digest: ManifestDigest::new(),
            db_buffer: Vec::new(),
            db_buffer_start: cursor,
            cache_restart: None,
        };
        let _ = self
            .stream_scan(control, &task, &mut stream, scanner, cursor)
            .await?;
        if self.stopped(id)? {
            return Ok(());
        }
        let digest = self.record(id)?.manifest.digest();
        self.finish_source_scan(id, digest)?;
        let complete = self.record(id)?;
        let final_file_count = complete.manifest.entries.len();
        let final_total_size = complete.manifest.total();
        tracing::info!(
            task = %id,
            files = complete.file_count,
            total = complete.total_size,
            "Manifest scan completed with repaired entries"
        );
        self.edit(id, |t| {
            t.event(format!(
                "Manifest scan complete: {} files · {} folders",
                t.file_count, t.directory_count
            ))
        })?;
        // 结束帧的响应由调用方的 Poll 统一处理；这里只把错误向上抛。
        let _ = control
            .request(&Request::ManifestDone {
                id: wire_id,
                file_count: final_file_count,
                total_size: final_total_size,
                digest: complete.digest.clone(),
            })
            .await?;
        Ok(())
    }
    async fn send_streaming_ready_files(
        &self,
        control: &mut Channel,
        task: &TaskRecord,
        entries: &[super::model::FileEntry],
        pending: &mut VecDeque<(usize, usize)>,
        sent: &mut BTreeSet<usize>,
    ) -> AppResult<Option<Vec<usize>>> {
        let state = control
            .request(&Request::Poll {
                id: task.wire_id.clone(),
            })
            .await?;
        let (status, completed) = match state {
            Response::State {
                status, completed, ..
            } => (status, completed),
            _ => {
                return Err(AppError::Other(
                    "Receiver did not return the manifest scan status".into(),
                ))
            }
        };
        if ["waiting", "accepted", "transferring"].contains(&status.as_str()) {
            self.edit(&task.id, |record| {
                if [
                    "queued",
                    "connecting",
                    "waiting",
                    "accepted",
                    "transferring",
                ]
                .contains(&record.status.as_str())
                {
                    record.status = status.clone();
                }
            })?;
        }
        if !["accepted", "transferring"].contains(&status.as_str()) {
            if ["cancelled", "rejected"].contains(&status.as_str()) {
                self.edit(&task.id, |record| record.status = status)?;
            }
            return Ok(None);
        }
        let remote_completed = completed.iter().collect::<BTreeSet<_>>();
        // 每轮只占用有限的几秒发送"已扫描到"的文件数据：清单扫描优先推进，
        // 其余数据交给扫描结束后的工作线程并发发送。否则大目录里扫描会被
        // 数据发送饿死（几万文件时表现为永远停在"正在扫描"）。
        // 边扫边传是刻意设计：数据发送不向扫描让路（真实场景是 1TB/20 万文件，
        // 扫描本来就要十几分钟）。这里只限制"单个文件独占时间片"，避免一个
        // 大文件把清单扫描一直堵到它传完。
        let deadline = std::time::Instant::now() + crate::tuning::SEND_DATA_SLICE;
        let mut handled = 0usize;
        let mut sent_chunks_now = 0usize;
        while let Some((file_index, first_chunk)) = pending.pop_front() {
            if handled > 0 && std::time::Instant::now() >= deadline {
                pending.push_front((file_index, first_chunk));
                break;
            }
            if ["paused", "remote_paused"].contains(&self.status_of(&task.id)?.as_str()) {
                self.wait_remote_resume(&task.id).await?;
            }
            if self.stopped(&task.id)? {
                pending.push_front((file_index, first_chunk));

                return Ok(None);
            }
            let Some(entry) = entries.get(file_index) else {
                return Err(AppError::Conflict(
                    "File indices in the scan are not contiguous".into(),
                ));
            };
            if entry.directory || entry.size == 0 {
                continue;
            }
            if let Some(files) = self.prepare_remote_file(control, task, file_index).await? {
                return Ok(Some(files));
            }
            handled += 1;
            let chunk_count = entry.size.div_ceil(task.chunk_size) as usize;
            let mut truncated = false;
            for local_index in 0..chunk_count {
                // 单个大文件不能在时间片里独占：到点把该文件退回队首，
                // 已发送的块在下一轮会被 sent 去重跳过。否则一个 10 GB 文件
                // 会把清单扫描一直堵到它传完。
                if sent_chunks_now > 0 && std::time::Instant::now() >= deadline {
                    pending.push_front((file_index, first_chunk));
                    truncated = true;
                    break;
                }
                if ["paused", "remote_paused"].contains(&self.status_of(&task.id)?.as_str()) {
                    self.wait_remote_resume(&task.id).await?;
                }
                if self.stopped(&task.id)? {
                    pending.push_front((file_index, first_chunk));

                    return Ok(None);
                }
                let index = first_chunk + local_index;
                if sent.contains(&index) || remote_completed.contains(&index) {
                    continue;
                }
                let offset = local_index as u64 * task.chunk_size;
                let length = task.chunk_size.min(entry.size - offset) as usize;
                let bytes = match self
                    .read_chunk_entry(&task, file_index, entry, offset, length)
                    .await
                {
                    Ok(bytes) => bytes,
                    Err(error) => {
                        return Err(self
                            .source_read_failed(control, task, file_index, error)
                            .await)
                    }
                };
                let sha256 = task
                    .verify_hash
                    .then(|| format!("{:x}", Sha256::digest(&bytes)));
                loop {
                    match control
                        .chunk(
                            &Request::VersionedChunk {
                                id: task.wire_id.clone(),
                                index,
                                file_version: super::model::file_version(file_index, entry),
                                length: bytes.len(),
                                sha256: sha256.clone(),
                            },
                            &bytes,
                        )
                        .await?
                    {
                        Response::Repair { files } => return Ok(Some(files)),
                        Response::Ack => {
                            self.mark_chunk_done(&task.id, index, bytes.len() as u64)?;
                            sent.insert(index);
                            sent_chunks_now += 1;
                            // 数据一开始流动就把状态从"正在连接"切到"传输中"，
                            // 否则界面会在整个清单/数据阶段一直显示正在连接。
                            if sent_chunks_now == 1 {
                                self.edit(&task.id, |record| {
                                    if ["queued", "connecting", "waiting"]
                                        .contains(&record.status.as_str())
                                    {
                                        record.status = "transferring".into();
                                        record.event("File data transfer started");
                                    }
                                })?;
                            }
                        }
                        Response::State {
                            status, revision, ..
                        } if ["paused", "remote_paused"].contains(&status.as_str()) => {
                            if status == "paused" {
                                self.apply_remote_state(
                                    &task.peer_id,
                                    &task.wire_id,
                                    revision,
                                    "paused",
                                )
                                .await?;
                            }
                            if !self.wait_remote_resume(&task.id).await? {
                                return Ok(None);
                            }
                            continue;
                        }
                        Response::State { status, .. }
                            if ["cancelled", "rejected"].contains(&status.as_str()) =>
                        {
                            self.edit(&task.id, |record| record.status = status)?;
                            return Ok(None);
                        }
                        _ => {
                            return Err(AppError::Other(
                                "Unexpected response while waiting for the receiver".into(),
                            ))
                        }
                    }
                    break;
                }
            }
            if truncated {
                break;
            }
        }
        Ok(None)
    }
    async fn wait_remote_resume(&self, id: &str) -> AppResult<bool> {
        let runtime = self.runtime(id);
        let mut updates = runtime.state_updates.subscribe();
        loop {
            match self.status_of(id)?.as_str() {
                "paused" | "remote_paused" | "waiting" | "finishing" => {}
                "interrupted" => {
                    return Err(AppError::Other("Transfer task was interrupted".into()))
                }
                "offline" | "remote_offline" | "cancelled" | "cancelling" | "rejected"
                | "completed" | "partial_completed" | "incompatible" => return Ok(false),
                _ => return Ok(true),
            }
            tokio::select! {
                _ = self.shutdown.cancelled() => return Ok(false),
                changed = updates.changed() => { changed.map_err(|_| AppError::Other("Task state event stream closed".into()))?; }
            }
        }
    }
    async fn send_once(
        self: &Arc<Self>,
        peers: Arc<PeerStore>,
        id: &str,
        cooldown: Arc<AtomicI64>,
        state_session: &mut Option<crate::network::state::StateSession>,
    ) -> AppResult<()> {
        let mut task = self.edit(id, |t| {
            t.status = "connecting".into();
            t.error = None;
        })?;
        let mut control = self.channel(&peers, &task, &cooldown, true).await?;
        if let Some(issue) = task
            .source_issue
            .clone()
            .filter(|issue| issue.accepted && issue.kind == "changed")
        {
            let mut entry = task.manifest.entries[issue.file].clone();
            let path = source::source_path_from_roots(&task.sources, &entry.path)?;
            let meta = std::fs::metadata(path)?;
            entry.size = meta.len();
            entry.modified_ms = source::modified(&meta);
            match control
                .request(&Request::ReplaceFile {
                    id: task.wire_id.clone(),
                    file: issue.file,
                    entry: entry.clone(),
                })
                .await?
            {
                Response::Ack => {}
                _ => {
                    return Err(AppError::Conflict(
                        "Receiver did not acknowledge the replacement file manifest".into(),
                    ))
                }
            }
            self.replace_file_entry(id, issue.file, entry)?;
            task = self.record(id)?;
        }

        // 先用摘要 + 指纹建立接收任务：重复 Offer 返回当前状态，`Ready` 表示需继续发分片清单。
        let mut offer_summary = task.manifest.offer_summary();
        offer_summary.streaming = true;
        offer_summary.file_count = 0;
        offer_summary.directory_count = 0;
        offer_summary.total_size = 0;
        offer_summary.chunk_count = 0;
        offer_summary.digest.clear();
        offer_summary.top_level_fingerprint = task.top_level_fingerprint.clone();
        let mut state = control
            .request(&Request::Offer {
                id: task.wire_id.clone(),
                summary: offer_summary,
            })
            .await?;
        if let Err(err) = control
            .request(&Request::Reconnect {
                id: task.wire_id.clone(),
            })
            .await
        {
            tracing::warn!(task = %task.wire_id, error = %err, "Failed to send automatic recovery signal; will continue recovery through the Offer/resume handshake");
        }
        if self.record(id)?.resume_requested {
            let _ = control
                .request(&Request::Resume {
                    id: task.wire_id.clone(),
                })
                .await;
            state = control
                .request(&Request::Poll {
                    id: task.wire_id.clone(),
                })
                .await?;
            self.edit(id, |record| record.resume_requested = false)?;
        }
        if state_session
            .as_ref()
            .map_or(true, |session| session.is_finished())
        {
            *state_session = None;
            let mut state_channel = self.channel(&peers, &task, &cooldown, true).await?;
            if !matches!(
                state_channel
                    .request(&Request::WatchTask {
                        id: task.wire_id.clone(),
                        revision: task.remote_state_revision,
                    })
                    .await?,
                Response::Ack
            ) {
                return Err(AppError::Other(
                    "Receiver did not acknowledge the task state subscription".into(),
                ));
            }
            *state_session = Some(crate::network::state::spawn(
                state_channel,
                self.clone(),
                peers.clone(),
                id.to_string(),
                0,
            ));
        }
        if let Response::RecoveryFiles { epoch, files } = control
            .request(&Request::RecoveryProbe {
                id: task.wire_id.clone(),
                epoch: task.recovery_epoch,
            })
            .await?
        {
            let mut repair = Vec::new();
            for (file, remote_hash) in files {
                let entry = task.manifest.entries.get(file).ok_or_else(|| {
                    AppError::BadRequest("Invalid file index in recovery response".into())
                })?;
                let path = source::source_path_from_roots(&task.sources, &entry.path)?;
                let meta = match std::fs::metadata(&path) {
                    Ok(meta) => meta,
                    Err(e) => {
                        return Err(self
                            .source_read_failed(&mut control, &task, file, e.into())
                            .await)
                    }
                };
                if meta.len() != entry.size || source::modified(&meta) != entry.modified_ms {
                    return Err(self
                        .source_read_failed(
                            &mut control,
                            &task,
                            file,
                            AppError::Conflict(
                                "Source file changed since the manifest was created".into(),
                            ),
                        )
                        .await);
                }
                let hash = tokio::task::spawn_blocking(move || super::recovery::hash_file(&path))
                    .await
                    .map_err(|e| AppError::Other(e.to_string()))??;
                if remote_hash.as_ref() != Some(&hash) {
                    repair.push(file);
                }
            }
            if !matches!(
                control
                    .request(&Request::RecoveryAck {
                        id: task.wire_id.clone(),
                        epoch,
                        repair
                    })
                    .await?,
                Response::Ack
            ) {
                return Err(AppError::Conflict(
                    "Receiver did not acknowledge the recovery response".into(),
                ));
            }
        }

        let mut resume_manifest_at = 0usize;
        state = match state {
            Response::ManifestResume { next_index } => {
                resume_manifest_at = next_index;
                Response::Ready
            }
            other => other,
        };
        // 任务事件直接显示在任务详情里：不翻日志也能看出停在哪一步。
        self.edit(id, |t| {
            t.event(match &state {
                Response::Ready => "The receiver is ready. Scanning and sending the manifest in parts",
                _ => "The receiver already has this task. Synchronizing status before completing the manifest",
            })
        })?;
        if matches!(state, Response::Ready) {
            let mut entries = task.manifest.entries.clone();
            let mut digest = ManifestDigest::new();
            let mut total_size = 0u64;
            let mut directories = 0usize;
            let mut chunk_count = 0usize;
            let mut pending_files = VecDeque::new();
            let mut sent_chunks = BTreeSet::new();
            for (file_index, entry) in entries.iter().enumerate() {
                digest.update(entry);
                total_size = total_size.saturating_add(entry.size);
                directories += usize::from(entry.directory);
                if !entry.directory && entry.size > 0 {
                    pending_files.push_back((file_index, chunk_count));
                    chunk_count += entry.size.div_ceil(task.chunk_size) as usize;
                }
            }
            for (page_index, page) in entries
                .get(resume_manifest_at.min(entries.len())..)
                .unwrap_or_default()
                .chunks(MANIFEST_PAGE)
                .enumerate()
            {
                let first = resume_manifest_at + page_index * MANIFEST_PAGE;
                let requested = std::time::Instant::now();
                let response = control
                    .request(&Request::ManifestChunk {
                        id: task.wire_id.clone(),
                        first,
                        entries: page.to_vec(),
                    })
                    .await?;
                let elapsed = requested.elapsed();
                if elapsed >= crate::tuning::SLOW_REQUEST_WARN {
                    tracing::warn!(
                        task = %task.wire_id,
                        first,
                        entries = page.len(),
                        elapsed_ms = elapsed.as_millis() as u64,
                        "Receiver is taking a long time to process manifest chunks"
                    );
                } else {
                    tracing::info!(
                        task = %task.wire_id,
                        first,
                        entries = page.len(),
                        elapsed_ms = elapsed.as_millis() as u64,
                        "Manifest chunk confirmed"
                    );
                }
                match response {
                    Response::ManifestAck => {}
                    Response::State { status, .. }
                        if ["cancelled", "rejected"].contains(&status.as_str()) =>
                    {
                        self.edit(id, |t| t.status = status)?;
                        return Ok(());
                    }
                    _ => {
                        return Err(AppError::Other(
                            "Receiver did not confirm the top-level manifest".into(),
                        ))
                    }
                }
            }

            if let Some(files) = self
                .send_streaming_ready_files(
                    &mut control,
                    &task,
                    &entries,
                    &mut pending_files,
                    &mut sent_chunks,
                )
                .await?
            {
                self.queue_for_file_repair(id, &files)?;
                return Ok(());
            }
            if ["paused", "remote_paused"].contains(&self.status_of(id)?.as_str()) {
                self.wait_remote_resume(id).await?;
            }
            if self.stopped(id)? {
                return Ok(());
            }
            let selected = task.sources.iter().map(|root| root.path.clone()).collect();
            let roots_count = entries.len();
            let last_root = entries
                .last()
                .map(|entry| entry.path.clone())
                .unwrap_or_default();
            tracing::info!(task = %task.wire_id, roots = roots_count, "Manifest phase: top-level entries sent; continuing directory scan");
            let cutoff = chrono::DateTime::parse_from_rfc3339(&task.created_at)
                .map(|t| t.timestamp_millis() as u64)
                .unwrap_or(u64::MAX);
            let scanner = tokio::task::spawn_blocking(move || {
                let mut scanner = source::SourceScanner::new(selected)?;
                scanner.set_cutoff(cutoff);
                scanner.resume_after(roots_count, &last_root)?;
                Ok::<_, AppError>(scanner)
            })
            .await
            .map_err(|e| AppError::Other(e.to_string()))??;
            tracing::info!(task = %task.wire_id, "Manifest phase: scanner is ready; starting batched scan");
            // 与"补齐剩余扫描"共用同一段循环：这里 send_data=true（边扫边传）。
            let mut stream = ScanStream {
                resume_manifest_at,
                send_data: true,
                entries: std::mem::take(&mut entries),
                pending_files: std::mem::take(&mut pending_files),
                sent_chunks: std::mem::take(&mut sent_chunks),
                chunk_count,
                total_size,
                directories,
                digest,
                db_buffer: Vec::new(),
                db_buffer_start: roots_count,
                cache_restart: None,
            };
            let first = self
                .stream_scan(&mut control, &task, &mut stream, scanner, roots_count)
                .await?;
            if let Some(files) = stream.cache_restart {
                self.queue_for_file_repair(id, &files)?;
                return Ok(());
            }
            if ["paused", "remote_paused"].contains(&self.status_of(id)?.as_str()) {
                self.wait_remote_resume(id).await?;
            }
            if self.stopped(id)? {
                return Ok(());
            }
            let ScanStream {
                entries, digest, ..
            } = stream;
            let final_digest = digest.finish();
            let manifest = Manifest::new(entries, task.chunk_size, task.verify_hash);
            manifest.validate()?;
            drop(manifest);
            tracing::info!(task = %task.wire_id, entries = first, "Manifest phase: scan ended; sending manifest end frame");
            self.finish_source_scan(id, final_digest)?;
            self.edit(id, |t| {
                t.event(format!(
                    "Manifest scan complete: {} files · {} folders",
                    t.file_count, t.directory_count
                ))
            })?;
            task = self.record(id)?;
            let final_file_count = task.manifest.entries.len();
            let final_total_size = task.manifest.total();
            state = control
                .request(&Request::ManifestDone {
                    id: task.wire_id.clone(),
                    file_count: final_file_count,
                    total_size: final_total_size,
                    digest: task.digest.clone(),
                })
                .await?;
        }
        loop {
            if !task.scan_complete
                && matches!(
                    &state,
                    Response::State { status, .. }
                        if ["accepted", "transferring", "completed"].contains(&status.as_str())
                )
            {
                self.complete_source_scan(&mut control, id).await?;
                task = self.record(id)?;
                state = control
                    .request(&Request::Poll {
                        id: task.wire_id.clone(),
                    })
                    .await?;
                continue;
            }
            if ["paused", "remote_paused"].contains(&self.status_of(id)?.as_str()) {
                if !self.wait_remote_resume(id).await? {
                    return Ok(());
                }
                state = control
                    .request(&Request::Poll {
                        id: task.wire_id.clone(),
                    })
                    .await?;
            }
            if self.stopped(id)? {
                return Ok(());
            }
            match state {
                Response::State {
                    status, completed, ..
                } if ["accepted", "transferring", "completed"].contains(&status.as_str()) => {
                    let count = task.manifest.chunk_count().max(task.chunk_count);
                    if let Some(last) = completed.max().filter(|last| *last >= count) {
                        return Err(AppError::BadRequest(format!(
                    "Peer reported completed chunks beyond the local manifest (maximum chunk {last}, manifest chunks {count})"
                )));
                    }
                    self.edit(id, |t| {
                        // 接收端的 completed 是已经落盘并持久化的权威进度。
                        // 发送端本地记录的是收到 Ack 的块；接收端可能在 Ack 后写盘失败，
                        // 若只做并集，发送端会把该块永久视为完成，进度 100% 后提交就会
                        // 一直报「仍有未提交 Chunk」。因此每次开始发送/续传时必须对齐，
                        // 丢弃接收端尚未确认落盘的本地完成记录，让这些块重新进入发送队列。
                        t.completed = completed.iter().collect();
                        t.completed_count = t.completed.len();
                        t.completed_bytes = t
                            .completed
                            .iter()
                            .filter_map(|index| t.manifest.chunk(*index))
                            .filter(|chunk| !t.skipped_files.contains(&chunk.file))
                            .map(|chunk| chunk.length as u64)
                            .sum();
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
                        if !["cancelled", "rejected"].contains(&t.status.as_str()) {
                            let cancelled = status == "cancelled";
                            t.status = status;
                            t.event(if cancelled {
                                "The receiver cancelled the task"
                            } else {
                                "The receiver declined the task"
                            });
                        }
                    })?;
                    return Ok(());
                }
                Response::State { status, .. }
                    if ["waiting", "paused", "interrupted"].contains(&status.as_str()) =>
                {
                    self.edit(id, |t| {
                        if !t.resume_requested {
                            t.status = match status.as_str() {
                                "paused" => "remote_paused",
                                "interrupted" => "interrupted",
                                _ => "waiting",
                            }
                            .into();
                        }
                        t.event(if status == "paused" {
                            "The receiver paused the task. Waiting for it to resume"
                        } else {
                            "Waiting for the receiver to continue"
                        });
                    })?;
                    if self.wait_remote_resume(id).await? {
                        self.edit(id, |task| task.resume_requested = false)?;
                        state = control
                            .request(&Request::Poll {
                                id: task.wire_id.clone(),
                            })
                            .await?;
                    } else {
                        return Ok(());
                    }
                }
                // 接收端仍停留在"发送方已暂停"的旧认知，而本地已经在传输：
                // 直接重新下发 Resume 让对端回到接收态。这里不能报错，也不能
                // 等对端的状态事件——事件可能已在这次重连中丢失。
                Response::State { status, .. } if status == "remote_paused" => {
                    self.edit(id, |t| {
                        t.event(
                            "The receiver has not received the resume notice. Synchronizing again",
                        )
                    })?;
                    state = control
                        .request(&Request::Resume {
                            id: task.wire_id.clone(),
                        })
                        .await?;
                }
                _ => return Err(AppError::Other("Unexpected receiver state response".into())),
            }
        }
        let chunks = task.manifest.chunks();
        let pending = chunks
            .iter()
            .enumerate()
            .filter(|(i, _)| !self.is_completed(id, *i))
            .map(|(i, c)| (i, *c))
            .collect::<VecDeque<_>>();
        // 根据当前活跃发送任务数动态分配每个任务的 Chunk 工作协程数：
        // 任务少时允许单任务使用多路连接；任务增多时减少单任务连接数，
        // 避免每个任务都固定开 4 路而让总连接数膨胀。实际带宽仍由 QUIC/TCP
        // 拥塞控制和两端 IO 能力共同决定，不设置僵硬的每任务带宽上限。
        let active_sends = crate::tuning::MAX_ACTIVE_SENDS
            .saturating_sub(self.scheduler.available_permits())
            .max(1);
        let per_task_workers = crate::tuning::MAX_ACTIVE_SENDS
            .div_ceil(active_sends)
            .clamp(1, 4);
        let concurrency = pending.len().clamp(1, per_task_workers);
        let queue = Arc::new(Mutex::new(pending));
        let mut workers = tokio::task::JoinSet::new();
        for _ in 0..concurrency {
            let service = self.clone();
            let peers = peers.clone();
            let task = task.clone();
            let cooldown = cooldown.clone();
            let queue = queue.clone();
            workers.spawn(async move {
                let mut channel = service.channel(&peers, &task, &cooldown, false).await?;
                let mut prepared_file = None;
                loop {
                    if !service.wait_remote_resume(&task.id).await? {
                        return Ok::<_, AppError>(None);
                    }
                    let Some((index, chunk)) = queue.lock().await.pop_front() else {
                        break;
                    };
                    if service.is_skipped(&task.id, chunk.file) {
                        continue;
                    }
                    if prepared_file != Some(chunk.file) {
                        if let Some(files) = service
                            .prepare_remote_file(&mut channel, &task, chunk.file)
                            .await?
                        {
                            return Ok(Some(files));
                        }
                        prepared_file = Some(chunk.file);
                    }
                    let bytes = match service.read_chunk_cached(&task, chunk).await {
                        Ok(bytes) => bytes,
                        Err(e) => {
                            return Err(service
                                .source_read_failed(&mut channel, &task, chunk.file, e)
                                .await)
                        }
                    };
                    let sha256 = task
                        .manifest
                        .verify_hash
                        .then(|| format!("{:x}", Sha256::digest(&bytes)));
                    loop {
                        if !service.wait_remote_resume(&task.id).await? {
                            return Ok(None);
                        }
                        match channel
                            .chunk(
                                &Request::VersionedChunk {
                                    id: task.wire_id.clone(),
                                    index,
                                    file_version: super::model::file_version(
                                        chunk.file,
                                        &task.manifest.entries[chunk.file],
                                    ),
                                    length: bytes.len(),
                                    sha256: sha256.clone(),
                                },
                                &bytes,
                            )
                            .await?
                        {
                            Response::Repair { files } => return Ok(Some(files)),
                            Response::Ack => {
                                service.mark_chunk_done(&task.id, index, bytes.len() as u64)?;
                                break;
                            }
                            Response::State {
                                status, revision, ..
                            } if ["paused", "remote_paused"].contains(&status.as_str()) => {
                                if status == "paused" {
                                    service
                                        .apply_remote_state(
                                            &task.peer_id,
                                            &task.wire_id,
                                            revision,
                                            "paused",
                                        )
                                        .await?;
                                }
                                if !service.wait_remote_resume(&task.id).await? {
                                    return Ok(None);
                                }
                            }
                            Response::State { status, .. } if status == "cancelled" => {
                                service.edit(&task.id, |t| {
                                    if t.status != "cancelled" {
                                        t.status = "cancelled".into();
                                        t.error = None;
                                        t.event("The receiver cancelled the task");
                                    }
                                })?;
                                return Ok(None);
                            }
                            _ => {
                                return Err(AppError::Other(
                                    "Receiver did not submit the chunk".into(),
                                ))
                            }
                        }
                    }
                }
                Ok(None)
            });
        }
        let mut repair_requested = None;
        while let Some(result) = workers.join_next().await {
            match result.map_err(|e| AppError::Other(e.to_string()))? {
                Ok(Some(files)) => {
                    repair_requested = Some(files);
                    workers.abort_all();
                    while workers.join_next().await.is_some() {}
                    break;
                }
                Ok(None) => {}
                Err(err) => {
                    workers.abort_all();
                    while workers.join_next().await.is_some() {}
                    return Err(err);
                }
            }
        }
        if let Some(files) = repair_requested {
            self.queue_for_file_repair(id, &files)?;
            return Ok(());
        }
        loop {
            if !self.wait_remote_resume(id).await? {
                return Ok(());
            }
            match control
                .request(&Request::Commit {
                    id: task.wire_id.clone(),
                })
                .await?
            {
                Response::Repair { files } => {
                    if files.is_empty() {
                        return Err(AppError::Conflict(
                            "Receiver requested an empty file repair list".into(),
                        ));
                    }
                    self.queue_for_file_repair(id, &files)?;
                    return Ok(());
                }
                Response::Ack => {
                    self.edit(id, |t| {
                        t.status = "completed".into();
                        t.error = None;
                        t.event("Received the receiver’s file-write receipt");
                    })?;
                    return Ok(());
                }
                Response::State {
                    status, revision, ..
                } if ["paused", "remote_paused", "accepted", "transferring"]
                    .contains(&status.as_str()) =>
                {
                    if status == "paused" {
                        self.apply_remote_state(&task.peer_id, &task.wire_id, revision, "paused")
                            .await?;
                    }
                    if ["paused", "remote_paused"].contains(&status.as_str()) {
                        if !self.wait_remote_resume(id).await? {
                            return Ok(());
                        }
                        continue;
                    }
                    // 提交可能要对几十万文件做 rename，接收端会持续报告 transferring；
                    // 这里必须继续等待回执，不能当成错误结束。
                    self.edit(id, |t| {
                        if t.status == "waiting" {
                            t.status = "transferring".into();
                        }
                    })?;
                    tokio::time::sleep(crate::tuning::SEND_POLL_INTERVAL).await;
                }
                Response::State { status, .. }
                    if ["cancelled", "rejected"].contains(&status.as_str()) =>
                {
                    self.edit(id, |t| t.status = status)?;
                    return Ok(());
                }
                _ => {
                    return Err(AppError::Other(
                        "Receiver has not finished committing the file".into(),
                    ))
                }
            }
        }
    }
}
