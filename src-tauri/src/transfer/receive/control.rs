//! Incoming transfer protocol control-message dispatch.
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
                if task.use_cache {
                    if let Some(cache_root) = self.cache_root_for_task(&id) {
                        for segment in 0..entry.size.div_ceil(relay::SEG_BYTES) {
                            let _ = tokio::fs::remove_file(paths::cache_segment(
                                &cache_root, &id, file, segment,
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
