//! Receive-side offer and manifest lifecycle.
use super::super::{
    model::{incoming_id, ConflictPolicy, Manifest, OfferSummary, TaskRecord},
    TransferService,
};
use crate::{
    error::{AppError, AppResult},
    network::protocol::Response,
};
use std::collections::BTreeSet;

impl TransferService {
    pub(crate) async fn offer(
        &self,
        peer: &str,
        name: &str,
        device_type: &str,
        wire: &str,
        summary: OfferSummary,
        transport: &str,
    ) -> AppResult<Response> {
        if wire.len() != 32 || !wire.bytes().all(|c| c.is_ascii_hexdigit()) {
            return Err(AppError::BadRequest("任务 ID 非法".into()));
        }
        let id = incoming_id(peer, wire);
        if self.status_of(&id).is_ok() {
            let mut existing_status = self.status_of(&id)?;
            let (digest, received) = self.manifest_received(&id)?;
            let existing = self.record(&id)?;
            if (!summary.streaming && digest != summary.digest)
                || existing.top_level_fingerprint != summary.top_level_fingerprint
            {
                return Err(AppError::Conflict("已创建的任务内容不能变更".into()));
            }
            // A previously accepted receive task may have been interrupted by
            // an application update/restart. Resume it automatically only if
            // its destination was already chosen; an intentional `paused`
            // state is never overridden here.
            if existing_status == "interrupted" && existing.destination.is_some() {
                self.edit(&id, |task| {
                    task.status = "accepted".into();
                    task.error = None;
                    task.event("重新连接后自动恢复已确认的接收任务");
                })?;
                existing_status = "accepted".into();
            }
            if !received {
                if ["accepted", "transferring", "paused", "remote_paused", "interrupted"].contains(&existing_status.as_str()) {
                    let recovered_count = existing.manifest.entries.len();
                    if recovered_count != existing.file_count {
                        self.update_scan_progress(
                            &id,
                            recovered_count,
                            existing
                                .manifest
                                .entries
                                .iter()
                                .filter(|entry| entry.directory)
                                .count(),
                            existing.manifest.total(),
                            existing.manifest.chunk_count(),
                            existing
                                .manifest
                                .entries
                                .last()
                                .map(|entry| entry.path.as_str())
                                .unwrap_or_default(),
                        )?;
                    }
                    return Ok(Response::ManifestResume {
                        next_index: recovered_count,
                    });
                }
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
            peer_device_type: device_type.into(),
            preferred_address: None,
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
            cache_files: std::collections::BTreeSet::new(),
            resume_requested: false,
            state_revision: 0,
            remote_state_revision: 0,
            digest: summary.digest,
            top_level_fingerprint: summary.top_level_fingerprint,
            scan_last_seq: 0,
            scan_last_path: String::new(),
            scan_complete: false,
            scan_progress_count: 0,
            scan_progress_directories: 0,
            manifest: Manifest::default(),
            completed: BTreeSet::new(),
            targets: vec![],
            relayed: vec![],
            skipped_files: BTreeSet::new(),
        };
        self.insert(task)?;
        Ok(Response::Ready)
    }

    /// Apply trusted-device auto-accept policy once the top-level manifest arrives.
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
        let trusted = match crate::registry::list(&self.app.db) {
            Ok(nodes) => nodes.iter().any(|node| node.node_id == peer),
            Err(error) => {
                tracing::warn!(%peer, %error, "读取信任设备列表失败，自动接收已安全降级为手动确认");
                return false;
            }
        };
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
                let _ = self.edit(id, |task| {
                    task.event("已信任的设备：自动接收，无需手动确认")
                });
                tracing::info!(%peer, task = %id, "信任设备顶层清单已到齐，自动接收");
                true
            }
            Err(error) => {
                tracing::warn!(%peer, task = %id, %error, "信任策略自动接收失败，保留手动确认");
                false
            }
        }
    }

    /// Validate the completed manifest and apply the final receive policy.
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
        if !["waiting", "accepted", "transferring", "paused", "remote_paused"].contains(&initial_status.as_str()) {
            return self.remote_state(&id);
        }
        let task = self.record(&id)?;
        let actual_file_count = task.manifest.entries.len();
        let actual_total_size = task.manifest.total();
        let actual_digest = task.manifest.digest();
        if file_count != actual_file_count || total_size != actual_total_size {
            // Streaming senders may finish the scan while the durable sender
            // snapshot still contains the top-level summary. The received
            // entries are already persisted and their digest is checked below,
            // so use those authoritative values instead of aborting cache
            // planning on a stale count/size frame.
            tracing::warn!(
                task = %id,
                announced_files = file_count,
                actual_files = actual_file_count,
                announced_bytes = total_size,
                actual_bytes = actual_total_size,
                "清单结束帧摘要落后于已接收清单，使用接收端实际值"
            );
        }
        if digest != actual_digest || (!task.digest.is_empty() && task.digest != digest) {
            return Err(AppError::Conflict("文件清单校验失败（指纹不一致）".into()));
        }
        task.manifest.validate()?;
        // Build the final per-file cache route before publishing scan_complete.
        // The writer uses scan_complete to stop its temporary cache fallback;
        // flipping it first creates a race where new files open destination
        // .part files while refresh_cache_plan is still pending.
        self.refresh_cache_plan(&id)?;
        self.edit(&id, |record| {
            record.file_count = actual_file_count;
            record.directory_count = record
                .manifest
                .entries
                .iter()
                .filter(|entry| entry.directory)
                .count();
            record.total_size = actual_total_size;
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
                self.auto_accept_if_trusted(peer, &id).await;
            }
        }
        self.remote_state(&id)
    }
}
