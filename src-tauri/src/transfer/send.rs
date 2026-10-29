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
}
impl TransferService {
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
            if self.stopped(&task.id)? {
                self.flush_scan_buffer(task, state, first)?;
                self.notify_remote_stop(control, &task.id, &task.wire_id).await;
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
                        "接收端处理清单分片较慢"
                    );
                } else {
                    tracing::info!(
                        task = %task.wire_id,
                        first = first + send_offset,
                        entries = page.len() - send_offset,
                        elapsed_ms = elapsed.as_millis() as u64,
                        "清单分片已确认"
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
                    _ => return Err(AppError::Other("接收方未确认清单分片".into())),
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
            // Keep the sender UI responsive between durable 5000-entry batches.
            // These counters are in-memory only; recovery still uses the durable cursor.
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
                        t.event(format!("正在分段发送清单：已扫描 {first} 条"))
                    })?;
                }
            }
            if self.stopped(&task.id)? {
                self.flush_scan_buffer(task, state, first)?;
                self.notify_remote_stop(control, &task.id, &task.wire_id).await;
                return Ok(first);
            }
            if state.send_data {
                self.send_streaming_ready_files(
                    control,
                    task,
                    &state.entries,
                    &mut state.pending_files,
                    &mut state.sent_chunks,
                )
                .await?;
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
                loop {
                    if service.stopped(&id)? {
                        return Ok(());
                    }
                    match service
                        .send_once(peers.clone(), &id, cooldown.clone())
                        .await
                    {
                        Ok(()) if service.status_of(&id)? == "queued" => continue,
                        Ok(()) => return Ok(()),
                        Err(err) => {
                            if service.stopped(&id)? {
                                return Ok(());
                            }
                            let retry = matches!(err, AppError::Io(_) | AppError::Other(_));
                            if !retry {
                                return Err(err);
                            }
                            // Once this run falls back to TCP, keep using TCP until
                            // the user manually pauses and resumes the task. A new
                            // start_task invocation creates a fresh cooldown (QUIC first).
                            cooldown.store(i64::MAX, Ordering::Relaxed);
                            attempt = attempt.saturating_add(1);
                            service.edit(&id, |t| {
                                // Unexpected network failures are recoverable, not
                                // user pauses. Keep this run alive so it reconnects
                                // automatically; stopped() still exits immediately
                                // when the user explicitly pauses or cancels.
                                t.status = "interrupted".into();
                                t.error = Some(err.to_string());
                                t.event(format!(
                                    "连接中断，{:?} 后自动重试（第 {attempt} 次）",
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
                    if !["cancelled", "paused"].contains(&t.status.as_str()) {
                        t.status = "paused".into();
                        t.error = Some(err.to_string());
                        t.event("设备暂时离线，已暂停；设备上线后点击继续");
                    }
                });
            }
            // 任务结束（含暂停/取消/完成）：释放发送端读句柄，避免常驻占用。
            service.release_send_files(&id);
            // 终态（完成/取消/被拒）时整体释放运行期资源；暂停保留，续传时复用。
            let terminal = service
                .status_of(&id)
                .map(|status| ["completed", "cancelled", "rejected"].contains(&status.as_str()))
                .unwrap_or(false);
            if terminal {
                service.release_runtime(&id);
            } else {
                service.runtime(&id).active.store(false, Ordering::SeqCst);
            }
        });
    }
    fn stopped(&self, id: &str) -> AppResult<bool> {
        Ok(
            ["paused", "remote_paused", "cancelling", "cancelled", "rejected", "completed"]
                .contains(&self.status_of(id)?.as_str()),
        )
    }
    async fn notify_remote_stop(&self, control: &mut Channel, id: &str, wire_id: &str) {
        let request = match self.status_of(id).as_deref() {
            Ok("paused") | Ok("remote_paused") => Some(Request::Pause { id: wire_id.to_string() }),
            _ => None,
        };
        if let Some(request) = request {
            let _ = control.request(&request).await;
        }
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
                            "QUIC HELLO/读取失败，立即回退 TCP 重试"
                        );
                        // Share the fallback decision with every worker and every
                        // reconnect in this transfer run; do not let another worker
                        // silently switch back to QUIC.
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
                                last = format!("QUIC 读取失败：{quic_error}；TCP 回退失败：{tcp_error}");
                                peers.failure(&task.peer_id, endpoint, last.clone());
                                continue;
                            }
                        };
                        if let Err(tcp_error) = transport::hello(&mut tcp_channel, &self.app).await {
                            last = format!("QUIC 读取失败：{quic_error}；TCP HELLO 失败：{tcp_error}");
                            peers.failure(&task.peer_id, endpoint, last.clone());
                            continue;
                        }
                        channel = tcp_channel;
                    }
                    peers.seen(&task.peer_id, endpoint);
                    // Keep the TCP preference for the whole retry window. Clearing
                    // it as soon as one worker connects via TCP lets other workers
                    // (and later reconnects in the same transfer) jump back to QUIC.
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
        tracing::info!(task = %id, cursor, "清单扫描未完成，继续扫描剩余条目");
        self.edit(id, |t| {
            t.event(format!(
                "接收方已有该任务：从第 {cursor} 条继续扫描剩余清单"
            ))
        })?;
        let scanner = tokio::task::spawn_blocking(move || {
            let mut scanner = source::SourceScanner::new(selected)?;
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
            "清单扫描已补齐"
        );
        self.edit(id, |t| {
            t.event(format!(
                "清单扫描已补齐：{} 个文件 · {} 个目录",
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
    ) -> AppResult<()> {
        let state = control
            .request(&Request::Poll {
                id: task.wire_id.clone(),
            })
            .await?;
        let (status, completed) = match state {
            Response::State { status, completed } => (status, completed),
            _ => return Err(AppError::Other("接收方未返回清单扫描状态".into())),
        };
        // Publish the receiver's actual decision during scanning as well as
        // after it. Auto-accepted tasks never enter the sender's waiting UI.
        if ["waiting", "accepted", "transferring"].contains(&status.as_str()) {
            self.edit(&task.id, |record| {
                if ["queued", "connecting", "waiting", "accepted", "transferring"].contains(&record.status.as_str()) {
                    record.status = status.clone();
                }
            })?;
        }
        if !["accepted", "transferring"].contains(&status.as_str()) {
            if ["cancelled", "rejected"].contains(&status.as_str()) {
                self.edit(&task.id, |record| record.status = status)?;
            }
            return Ok(());
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
            if self.stopped(&task.id)? {
                pending.push_front((file_index, first_chunk));
                self.notify_remote_stop(control, &task.id, &task.wire_id).await;
                return Ok(());
            }
            let Some(entry) = entries.get(file_index) else {
                return Err(AppError::Conflict("扫描中的文件索引不连续".into()));
            };
            if entry.directory || entry.size == 0 {
                continue;
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
                if self.stopped(&task.id)? {
                    pending.push_front((file_index, first_chunk));
                    self.notify_remote_stop(control, &task.id, &task.wire_id).await;
                    return Ok(());
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
                    Err(AppError::Conflict(_)) => {
                        match control
                            .request(&Request::SkipFile {
                                id: task.wire_id.clone(),
                                file: file_index,
                            })
                            .await?
                        {
                            Response::Ack => {}
                            _ => return Err(AppError::Other("接收方未确认跳过变化文件".into())),
                        }
                        self.mark_skipped(&task.id, file_index)?;
                        for skipped in first_chunk..first_chunk + chunk_count {
                            sent.insert(skipped);
                        }
                        break;
                    }
                    Err(error) => return Err(error),
                };
                let sha256 = task
                    .verify_hash
                    .then(|| format!("{:x}", Sha256::digest(&bytes)));
                match control
                    .chunk(
                        &Request::Chunk {
                            id: task.wire_id.clone(),
                            index,
                            length: bytes.len(),
                            sha256,
                        },
                        &bytes,
                    )
                    .await?
                {
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
                                    record.event("已开始发送文件数据");
                                }
                            })?;
                        }
                    }
                    Response::State { status, .. } if status == "paused" => {
                        self.edit(&task.id, |record| {
                            record.status = "remote_paused".into();
                            record.event("接收方已暂停，等待恢复");
                        })?;
                        pending.push_front((file_index, first_chunk));
                        return Ok(());
                    }
                    Response::State { status, .. }
                        if ["cancelled", "rejected"].contains(&status.as_str()) =>
                    {
                        self.edit(&task.id, |record| record.status = status)?;
                        return Ok(());
                    }
                    _ => return Err(AppError::Other("接收方未确认流式数据块".into())),
                }
            }
            if truncated {
                break;
            }
        }
        Ok(())
    }
    async fn wait_remote_event(&self, control: &mut Channel, id: &str) -> AppResult<String> {
        let task = self.record(id)?;
        if !matches!(
            control
                .request(&Request::WatchTask {
                    id: task.wire_id.clone(),
                    revision: task.remote_state_revision,
                })
                .await?,
            Response::Ack
        ) {
            return Err(AppError::Other("接收方未确认任务状态等待".into()));
        }
        let Response::TaskEvent { revision, status } =
            crate::network::protocol::read_event(&mut *control.reader).await?
        else {
            return Err(AppError::Other("接收方未返回任务状态事件".into()));
        };
        self.apply_remote_state(&task.peer_id, &task.wire_id, revision, &status)
            .await?;
        self.status_of(id)
    }
    async fn wait_remote_resume(&self, control: &mut Channel, id: &str) -> AppResult<bool> {
        loop {
            tokio::select! {
                _ = self.shutdown.cancelled() => return Ok(false),
                status = self.wait_remote_event(control, id) => match status?.as_str() {
                    "queued" => return Ok(true),
                    "cancelled" | "rejected" | "completed" => return Ok(false),
                    _ => {}
                }
            }
        }
    }
    async fn send_once(
        self: &Arc<Self>,
        peers: Arc<PeerStore>,
        id: &str,
        cooldown: Arc<AtomicI64>,
    ) -> AppResult<()> {
        let mut task = self.edit(id, |t| {
            t.status = "connecting".into();
            t.error = None;
        })?;
        // Resume guard: if a selected top-level item changed, discard this task's
        // old manifest/chunk state and begin a fresh scan under a new wire ID.
        let mut stale_wire = None;
        if let Err(error) = source::verify_top_level_fingerprint(&task) {
            if !matches!(&error, AppError::Conflict(_)) {
                return Err(error);
            }
            let old_wire = task.wire_id.clone();
            let selected = task.sources.iter().map(|root| root.path.clone()).collect();
            let verify_hash = task.verify_hash;
            let (sources, roots) = tokio::task::spawn_blocking(move || {
                let mut scanner = source::SourceScanner::new(selected)?;
                let sources = scanner.roots().to_vec();
                let roots = scanner.next_batch(sources.len().max(1))?;
                Ok::<_, AppError>((sources, roots))
            })
            .await
            .map_err(|e| AppError::Other(e.to_string()))??;
            let top_level_count = sources.len();
            let fingerprint = source::top_level_fingerprint(&sources);
            self.clear_entries(id)?;
            self.app.db.with_conn(|conn| {
                for table in ["task_chunks", "task_targets", "task_skipped", "task_relay"] {
                    conn.execute(&format!("DELETE FROM {table} WHERE task_id=?1"), [id])?;
                }
                Ok(())
            })?;
            task = self.edit(id, |record| {
                record.wire_id = super::model::random_id();
                record.sources = sources;
                record.top_level_count = top_level_count;
                record.top_level_fingerprint = fingerprint;
                record.attach_manifest(Manifest::new(roots, super::CHUNK_SIZE, verify_hash));
                record.digest.clear();
                record.scan_last_seq = record.file_count;
                record.scan_last_path = record
                    .manifest
                    .entries
                    .last()
                    .map(|entry| entry.path.clone())
                    .unwrap_or_default();
                record.scan_complete = false;
                record.completed.clear();
                record.completed_count = 0;
                record.completed_bytes = 0;
                record.targets.clear();
                record.relayed.clear();
                record.skipped_files.clear();
                record.skipped_bytes = 0;
                record.skipped_paths.clear();
                record.relayed_total = 0;
                record.relay_target_bytes = 0;
                record.status = "connecting".into();
                record.error = None;
                record.event("顶层项目元数据变化，已清除旧恢复状态并重新扫描");
            })?;
            // The old manifest was cleared above; persist the replacement
            // roots through the incremental manifest writer.
            self.append_entries(&task.id, 0, &task.manifest.entries)?;
            stale_wire = Some(old_wire);
        }
        let mut control = self.channel(&peers, &task, &cooldown, true).await?;
        if let Some(old_wire) = stale_wire {
            let _ = control
                .request(&Request::TaskState {
                    id: old_wire,
                    revision: task.state_revision,
                    status: "cancelled".into(),
                })
                .await;
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
        // Explicitly notify the receiver that this is an automatic reconnect.
        // The receiver resumes interrupted tasks, but preserves intentional pauses.
        if let Err(err) = control
            .request(&Request::Reconnect {
                id: task.wire_id.clone(),
            })
            .await
        {
            tracing::warn!(task = %task.wire_id, error = %err, "自动恢复信号发送失败，将继续通过 Offer/续传握手恢复");
        }
        if self.record(id)?.resume_requested {
            let _ = control.request(&Request::Resume { id: task.wire_id.clone() }).await;
            state = control.request(&Request::Poll { id: task.wire_id.clone() }).await?;
            self.edit(id, |record| record.resume_requested = false)?;
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
                Response::Ready => "接收方已就绪：开始扫描并把清单分段发给对方",
                _ => "接收方已有该任务：先同步状态，再补齐剩余清单",
            })
        })?;
        if matches!(state, Response::Ready) {
            // The Offer is sent before recursive traversal. Send selected roots
            // first, then walk and acknowledge bounded manifest batches as they
            // are discovered instead of building the full tree up front.
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
                        "接收端处理清单分片较慢"
                    );
                } else {
                    tracing::info!(
                        task = %task.wire_id,
                        first,
                        entries = page.len(),
                        elapsed_ms = elapsed.as_millis() as u64,
                        "清单分片已确认"
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
                    _ => return Err(AppError::Other("接收方未确认顶层清单".into())),
                }
            }

            self.send_streaming_ready_files(
                &mut control,
                &task,
                &entries,
                &mut pending_files,
                &mut sent_chunks,
            )
            .await?;
            if self.stopped(id)? {
                if self.status_of(id)? == "remote_paused" {
                    if self.wait_remote_resume(&mut control, id).await? {
                        return Ok(());
                    }
                }
                self.notify_remote_stop(&mut control, id, &task.wire_id).await;
                return Ok(());
            }
            let selected = task.sources.iter().map(|root| root.path.clone()).collect();
            let roots_count = entries.len();
            let last_root = entries
                .last()
                .map(|entry| entry.path.clone())
                .unwrap_or_default();
            tracing::info!(task = %task.wire_id, roots = roots_count, "清单阶段：顶层已发送，准备继续扫描目录");
            let scanner = tokio::task::spawn_blocking(move || {
                let mut scanner = source::SourceScanner::new(selected)?;
                scanner.resume_after(roots_count, &last_root)?;
                Ok::<_, AppError>(scanner)
            })
            .await
            .map_err(|e| AppError::Other(e.to_string()))??;
            tracing::info!(task = %task.wire_id, "清单阶段：扫描器已就绪，开始逐批扫描");
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
            };
            let first = self
                .stream_scan(&mut control, &task, &mut stream, scanner, roots_count)
                .await?;
            if self.stopped(id)? {
                if self.status_of(id)? == "remote_paused" {
                    if self.wait_remote_resume(&mut control, id).await? {
                        return Ok(());
                    }
                }
                return Ok(());
            }
            let ScanStream {
                entries, digest, ..
            } = stream;
            let final_digest = digest.finish();
            let manifest = Manifest::new(entries, task.chunk_size, task.verify_hash);
            manifest.validate()?;
            drop(manifest);
            tracing::info!(task = %task.wire_id, entries = first, "清单阶段：扫描结束，发送清单结束帧");
            self.finish_source_scan(id, final_digest)?;
            self.edit(id, |t| {
                t.event(format!(
                    "清单扫描完成：{} 个文件 · {} 个目录",
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
            // On Offer reconnect, the peer can already have thousands of
            // committed chunks while this sender still has only the top-level
            // roots in memory. Finish and announce the local manifest before
            // comparing the peer's chunk ranges against it.
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
            if self.stopped(id)? {
                return Ok(());
            }
            match state {
                Response::State { status, completed }
                    if ["accepted", "transferring", "completed"].contains(&status.as_str()) =>
                {
            let count = task.manifest.chunk_count().max(task.chunk_count);
            if let Some(last) = completed.max().filter(|last| *last >= count) {
                return Err(AppError::BadRequest(format!(
                    "对端返回的已完成分块超出本地清单（最大分块 {last}，清单分块数 {count}）"
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
                                "接收方已取消任务"
                            } else {
                                "接收方已拒绝任务"
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
                            "接收方已暂停，等待恢复"
                        } else {
                            "等待接收方继续"
                        });
                    })?;
                    loop {
                        tokio::select! {
                            _ = self.shutdown.cancelled() => return Ok(()),
                            result = self.wait_remote_event(&mut control, id) => {
                                match result? .as_str() {
                                    "queued" => {
                                        self.edit(id, |task| task.resume_requested = false)?;
                                        state = control.request(&Request::Poll { id: task.wire_id.clone() }).await?;
                                        break;
                                    }
                                    "cancelled" | "rejected" | "completed" => return Ok(()),
                                    _ => {}
                                }
                            }
                        }
                    }
                }
                _ => return Err(AppError::Other("接收方未返回有效接收状态".into())),
            }
        }
        let mut creation_control = control;
        let mut control = self.channel(&peers, &task, &cooldown, true).await?;
        let state = control
            .request(&Request::Poll {
                id: task.wire_id.clone(),
            })
            .await?;
        match state {
            Response::State { status, completed }
                if ["accepted", "transferring", "completed"].contains(&status.as_str()) =>
            {
                self.edit(id, |record| {
                    record.completed = completed.iter().collect();
                    record.completed_count = record.completed.len();
                    record.completed_bytes = record.completed.iter()
                        .filter_map(|index| record.manifest.chunk(*index))
                        .filter(|chunk| !record.skipped_files.contains(&chunk.file))
                        .map(|chunk| chunk.length as u64)
                        .sum();
                    record.status = if status == "completed" {
                        "completed"
                    } else {
                        "transferring"
                    }
                    .into();
                })?;
                if status == "completed" {
                    return Ok(());
                }
            }
            Response::State { status, .. } if status == "paused" => {
                self.edit(id, |record| record.status = "remote_paused".into())?;
                let _ = self.wait_remote_resume(&mut creation_control, id).await?;
                return Ok(());
            }
            Response::State { status, .. } if ["cancelled", "rejected"].contains(&status.as_str()) => {
                self.edit(id, |record| record.status = status)?;
                return Ok(());
            }
            _ => return Err(AppError::Other("接收方未就绪，无法建立传输会话".into())),
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
        // Keep the creation connection open for receiver state events while the
        // task connection and chunk streams carry transfer traffic.
        for _ in 0..concurrency {
            let service = self.clone();
            let peers = peers.clone();
            let task = task.clone();
            let cooldown = cooldown.clone();
            let queue = queue.clone();
            workers.spawn(async move {
                let mut channel = service.channel(&peers, &task, &cooldown, false).await?;
                loop {
                    if service.stopped(&task.id)? {
                        return Ok::<_, AppError>(());
                    }
                    let Some((index, chunk)) = queue.lock().await.pop_front() else {
                        break;
                    };
                    if service.is_skipped(&task.id, chunk.file) {
                        continue;
                    }
                    let bytes = match service.read_chunk_cached(&task, chunk).await {
                        Ok(bytes) => bytes,
                        Err(AppError::Conflict(_)) => {
                            match channel
                                .request(&Request::SkipFile {
                                    id: task.wire_id.clone(),
                                    file: chunk.file,
                                })
                                .await?
                            {
                                Response::Ack => {}
                                _ => {
                                    return Err(AppError::Other(
                                        "接收方未确认跳过变化文件，请更新两端软件后重试".into(),
                                    ))
                                }
                            }
                            service.mark_skipped(&task.id, chunk.file)?;
                            continue;
                        }
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
                                service.mark_chunk_done(&task.id, index, bytes.len() as u64)?;
                                break;
                            }
                            Response::State { status, .. } if status == "paused" => {
                                service.edit(&task.id, |t| {
                                    if !["cancelled", "completed"].contains(&t.status.as_str()) {
                                        t.status = "remote_paused".into();
                                        t.event("接收方已暂停，等待状态同步恢复或取消");
                                    }
                                })?;
                                return Ok(());
                            }
                            Response::State { status, .. } if status == "cancelled" => {
                                service.edit(&task.id, |t| {
                                    if t.status != "cancelled" {
                                        t.status = "cancelled".into();
                                        t.error = None;
                                        t.event("接收方已取消任务");
                                    }
                                })?;
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
        if self.status_of(id)? == "remote_paused" {
            let _ = self.wait_remote_resume(&mut creation_control, id).await?;
            return Ok(());
        }
        if self.stopped(id)? {
            if self.status_of(id)?.as_str() == "paused" {
                let _ = control.request(&Request::Pause { id: task.wire_id.clone() }).await;
            }
            return Ok(());
        }
        loop {
            if self.stopped(id)? {
                if self.status_of(id)?.as_str() == "paused" {
                    let _ = control.request(&Request::Pause { id: task.wire_id.clone() }).await;
                }
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
                Response::State { status, .. }
                    if ["paused", "accepted", "transferring"].contains(&status.as_str()) =>
                {
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
                _ => return Err(AppError::Other("接收方尚未完成文件提交".into())),
            }
        }
    }
}
