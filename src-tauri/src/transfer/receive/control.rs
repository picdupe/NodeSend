use super::super::{model::incoming_id, paths, relay, TransferService};
use crate::{
    error::{AppError, AppResult},
    network::protocol::{Request, Response},
};

impl TransferService {
    pub(crate) async fn control(
        &self,
        peer: &str,
        name: &str,
        device_type: &str,
        request: Request,
        transport: &str,
    ) -> AppResult<Response> {
        match request {
            Request::PrepareFile { id, file } => {
                let id = incoming_id(peer, &id);
                if self.admit_cached_file(&id, file).await? {
                    return Ok(Response::Repair { files: vec![file] });
                }
                if self
                    .runtime_if_exists(&id)
                    .is_some_and(|runtime| crate::locks::lock(&runtime.cache_fallback_pending).contains(&file))
                {
                    return Ok(Response::Repair { files: vec![file] });
                }
                let status = self.status_of(&id)?;
                if !["accepted", "transferring"].contains(&status.as_str()) {
                    return self.remote_state(&id);
                }
                Ok(Response::Ack)
            }
            Request::RecoveryProbe { id, epoch } => {
                let id = incoming_id(peer, &id);
                self.edit(&id, |t| {
                    if epoch > t.remote_recovery_epoch {
                        t.remote_recovery_epoch = epoch;
                        t.recovery_epoch = t.recovery_epoch.saturating_add(1);
                    }
                })?;
                self.recovery_probe(&id).await
            }
            Request::RecoveryAck { id, epoch, repair } => {
                let id = incoming_id(peer, &id);
                let gate = self.gate(&id);
                let _guard = gate.lock().await;
                if self.record(&id)?.recovery_epoch != epoch {
                    return Err(AppError::Conflict(
                        "Recovery acknowledgement uses a stale epoch".into(),
                    ));
                }
                for file in repair {
                    self.invalidate_file(&id, file)?;
                }
                self.edit(&id, |t| {
                    t.checked_epoch = epoch;
                })?;
                Ok(Response::Ack)
            }
            Request::SourceIssue { id, file, kind } => {
                let id = incoming_id(peer, &id);
                self.set_source_issue(&id, file, &kind)?;
                Ok(Response::Ack)
            }
            Request::ReplaceFile { id, file, entry } => {
                let id = incoming_id(peer, &id);
                self.await_writes(&id).await?;
                let _writer = self.destination_writer.lock().await;
                let gate = self.gate(&id);
                let _guard = gate.lock().await;
                self.replace_file_entry(&id, file, entry)?;
                Ok(Response::Ack)
            }
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
                    .ok_or_else(|| AppError::BadRequest("Invalid file index".into()))?;
                let part = paths::stage(&task)?.join(format!("{file}.part"));
                paths::ensure_plain(&part)?;
                match tokio::fs::remove_file(&part).await {
                    Ok(()) => {}
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => return Err(e.into()),
                }
                if task.use_cache {
                    if let Some(cache_root) = self.cache_root_for_task(&id) {
                        for segment in 0..entry.size.div_ceil(relay::SEG_BYTES) {
                            let _ = tokio::fs::remove_file(paths::cache_segment(
                                &cache_root,
                                &task,
                                file,
                                segment,
                            ))
                            .await;
                        }
                    }
                }
                self.mark_skipped(&id, file)?;
                if let Some(runtime) = self.runtime_if_exists(&id) {
                    crate::locks::lock(&runtime.receive_files).remove(&file);
                }
                Ok(Response::Ack)
            }
            Request::Offer { id, summary } => {
                self.offer(peer, name, device_type, &id, summary, transport)
                    .await
            }
            Request::Reconnect { id } => {
                let id = super::super::model::incoming_id(peer, &id);
                let gate = self.gate(&id);
                let _lock = gate.lock().await;
                if self.status_of(&id).is_ok() {
                    let task = self.record(&id)?;
                    if ["interrupted", "manual_reconnecting"].contains(&task.status.as_str())
                        && task.destination.is_some()
                    {
                        self.edit(&id, |task| {
                            task.status = "accepted".into();
                            task.error = None;
                            task.event("The sender reconnected. Receiving resumed automatically");
                        })?;
                    }
                    self.remote_state(&id)
                } else {
                    Ok(Response::Ack)
                }
            }
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
            Request::Pause { id } => {
                self.action(&incoming_id(peer, &id), "remote_pause").await?;
                Ok(Response::Ack)
            }
            Request::Resume { id } => {
                self.action(&incoming_id(peer, &id), "resume").await?;
                self.remote_state(&incoming_id(peer, &id))
            }
            Request::TaskState {
                id,
                revision,
                status,
            } => {
                self.apply_remote_state(peer, &id, revision, &status)
                    .await?;
                Ok(Response::Ack)
            }
            _ => Err(AppError::BadRequest("Invalid session message order".into())),
        }
    }
}
