use super::TransferService;
use crate::error::{AppError, AppResult};
use rusqlite::{params, OptionalExtension};

pub(crate) fn reservation_bytes(size: u64) -> u64 {
    size.div_ceil(4096)
        .saturating_mul(4096)
        .saturating_add(8192)
}

pub(crate) fn processing(status: &str) -> bool {
    ![
        "cancelling",
        "cancelled",
        "rejected",
        "completed",
        "partial_completed",
        "failed",
        "incompatible",
    ]
    .contains(&status)
}

pub(crate) fn hash_file(path: &std::path::Path) -> AppResult<String> {
    use sha2::{Digest, Sha256};
    use std::io::Read;
    let mut source = std::fs::File::open(path)?;
    let mut hash = Sha256::new();
    let mut buffer = vec![0; 1024 * 1024];
    loop {
        let len = source.read(&mut buffer)?;
        if len == 0 {
            break;
        }
        hash.update(&buffer[..len]);
    }
    Ok(format!("{:x}", hash.finalize()))
}

impl TransferService {
    pub(crate) fn init_file_journal(&self) -> AppResult<()> {
        self.app.db.with_conn(|db| {
            db.execute_batch("CREATE TABLE IF NOT EXISTS transfer_file_journal (
                task_id TEXT NOT NULL, file_index INTEGER NOT NULL, size INTEGER NOT NULL,
                state TEXT NOT NULL, receive_reserved INTEGER NOT NULL DEFAULT 0,
                assemble_reserved INTEGER NOT NULL DEFAULT 0, target TEXT,
                publish_order INTEGER NOT NULL DEFAULT 0,
                PRIMARY KEY(task_id,file_index));
                CREATE TABLE IF NOT EXISTS transfer_file_repairs(task_id TEXT NOT NULL,file_index INTEGER NOT NULL,attempts INTEGER NOT NULL,PRIMARY KEY(task_id,file_index));
                CREATE TABLE IF NOT EXISTS transfer_cache_digest(task_id TEXT NOT NULL,file_index INTEGER NOT NULL,digest TEXT NOT NULL,PRIMARY KEY(task_id,file_index));
                CREATE TABLE IF NOT EXISTS transfer_chunk_hash(task_id TEXT NOT NULL,chunk_index INTEGER NOT NULL,digest TEXT NOT NULL,PRIMARY KEY(task_id,chunk_index));
                CREATE INDEX IF NOT EXISTS transfer_active_receive ON transfer_file_journal(receive_reserved) WHERE receive_reserved>0;
                CREATE INDEX IF NOT EXISTS transfer_publication_order ON transfer_file_journal(publish_order);
                CREATE INDEX IF NOT EXISTS transfer_recent_files ON transfer_file_journal(task_id,state,publish_order DESC);")?;
            Ok(())
        })?;
        for id in self.receive_ids() {
            if [
                "completed",
                "cancelled",
                "rejected",
                "partial_completed",
                "incompatible",
            ]
            .contains(&self.status_of(&id)?.as_str())
            {
                self.release_cache_reservations(&id)?;
                continue;
            }
            let task = self.record(&id)?;
            if !task.use_cache {
                continue;
            }
            for file in &task.cache_files {
                let entry = &task.manifest.entries[*file];
                if self.journal_state(&id, *file)?.as_deref() != Some("published") {
                    continue;
                }
                let Some(cache) = self.cache_root_for_task(&id) else {
                    continue;
                };
                let assembled = super::paths::cache_file(&cache, &task, *file);
                let valid_target = task
                    .targets
                    .get(*file)
                    .and_then(Option::as_ref)
                    .is_some_and(|p| {
                        std::fs::metadata(p).is_ok_and(|m| m.is_file() && m.len() == entry.size)
                    });
                if valid_target {
                    let mut files = vec![
                        assembled.clone(),
                        super::paths::cache_journal_named(
                            &cache,
                            &id,
                            *file,
                            super::paths::cache_name_of(&task, *file),
                        ),
                    ];
                    files.extend(
                        (0..entry.size.div_ceil(super::relay::SEG_BYTES))
                            .map(|seg| super::paths::cache_segment(&cache, &task, *file, seg)),
                    );
                    for path in files {
                        match std::fs::remove_file(path) {
                            Ok(()) => {}
                            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                            Err(e) => return Err(e.into()),
                        }
                    }
                } else if assembled.is_file() && std::fs::metadata(&assembled)?.len() == entry.size
                {
                    self.journal_transition(
                        &id,
                        *file,
                        entry.size,
                        "assembled",
                        task.targets[*file].as_deref(),
                    )?;
                    self.edit(&id, |t| {
                        if let Some(value) = t.relayed.get_mut(*file) {
                            t.relayed_total = t.relayed_total.saturating_sub(*value);
                            *value = 0;
                        }
                    })?;
                    self.app.db.with_conn(|db| {
                        db.execute(
                            "DELETE FROM task_relay WHERE task_id=?1 AND file_index=?2",
                            params![id, *file as i64],
                        )?;
                        Ok(())
                    })?;
                } else if let Err(error) = self.invalidate_file(&id, *file) {
                    tracing::warn!(task=%id,file, %error,"Failed to invalidate an incomplete cached file");
                }
            }
            let task = self.record(&id)?;
            for file in &task.cache_files {
                let entry = &task.manifest.entries[*file];
                if task.relayed.get(*file).copied().unwrap_or(0) >= entry.size {
                    continue;
                }
                if self.journal_state(&id, *file)?.is_some() {
                    continue;
                }
                let base = task.manifest.file_base(*file);
                let count = entry.size.div_ceil(task.chunk_size) as usize;
                let exists = self.cache_root_for_task(&id).is_some_and(|cache| {
                    super::paths::cache_file(&cache, &task, *file).exists()
                        || super::paths::cache_segment(&cache, &task, *file, 0).exists()
                });
                if exists || (base..base + count).any(|i| task.completed.contains(&i)) {
                    self.app.db.with_conn(|db| {
                        db.execute("INSERT OR IGNORE INTO transfer_file_journal(task_id,file_index,size,state,receive_reserved,assemble_reserved) VALUES(?1,?2,?3,'receiving',?4,?4)",params![id,*file as i64,entry.size as i64,reservation_bytes(entry.size) as i64])?;
                        Ok(())
                    })?;
                }
            }
        }
        Ok(())
    }

    pub(crate) fn journal_state(&self, id: &str, file: usize) -> AppResult<Option<String>> {
        self.app.db.with_conn(|db| {
            Ok(db
                .query_row(
                    "SELECT state FROM transfer_file_journal WHERE task_id=?1 AND file_index=?2",
                    params![id, file as i64],
                    |r| r.get(0),
                )
                .optional()?)
        })
    }

    pub(crate) fn reserve_cached_file(
        &self,
        id: &str,
        file: usize,
        size: u64,
        limit: u64,
    ) -> AppResult<bool> {
        self.app.db.with_transaction(|db| {
            let old: Option<(i64, i64)> = db.query_row(
                "SELECT receive_reserved,assemble_reserved FROM transfer_file_journal WHERE task_id=?1 AND file_index=?2",
                params![id,file as i64], |r| Ok((r.get(0)?, r.get(1)?))).optional()?;
            let bytes = reservation_bytes(size);
            if old.is_some_and(|(receive, assemble)| receive as u64 >= bytes && assemble as u64 >= bytes) {
                return Ok(true);
            }
            let used_receive: i64 = db.query_row(
                "SELECT COALESCE(SUM(receive_reserved),0) FROM transfer_file_journal WHERE receive_reserved>0 AND NOT(task_id=?1 AND file_index=?2)",
                params![id,file as i64], |r| r.get(0))?;
            let used_assemble: i64 = db.query_row(
                "SELECT COALESCE(SUM(assemble_reserved),0) FROM transfer_file_journal WHERE assemble_reserved>0 AND NOT(task_id=?1 AND file_index=?2)",
                params![id,file as i64], |r| r.get(0))?;
            if bytes > limit
                || (used_receive.max(0) as u64).saturating_add(bytes) > limit
                || (used_assemble.max(0) as u64).saturating_add(bytes) > limit
            {
                return Ok(false);
            }
            db.execute(
                "INSERT INTO transfer_file_journal(task_id,file_index,size,state,receive_reserved,assemble_reserved) \
                 VALUES(?1,?2,?3,'receiving',?4,?4) \
                 ON CONFLICT(task_id,file_index) DO UPDATE SET \
                 state='receiving',receive_reserved=excluded.receive_reserved,assemble_reserved=excluded.assemble_reserved",
                params![id,file as i64,size as i64,bytes as i64],
            )?;
            Ok(true)
        })
    }

    pub(crate) fn cached_file_reserved(&self, id: &str, file: usize, size: u64) -> AppResult<bool> {
        self.app.db.with_conn(|db| {
            let reservation: Option<(i64, i64)> = db
                .query_row(
                    "SELECT receive_reserved,assemble_reserved FROM transfer_file_journal WHERE task_id=?1 AND file_index=?2",
                    params![id, file as i64],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()?;
            let expected = reservation_bytes(size) as i64;
            Ok(reservation
                .is_some_and(|(receive, assemble)| receive >= expected && assemble >= expected))
        })
    }

    pub(crate) fn journal_transition(
        &self,
        id: &str,
        file: usize,
        size: u64,
        state: &str,
        target: Option<&str>,
    ) -> AppResult<()> {
        if let Some(cache) = self.cache_root_for_task(id) {
            let journal = super::paths::cache_journal_named(
                &cache,
                id,
                file,
                self.cache_name_for(id, file).as_deref(),
            );
            if let Some(dir) = journal.parent() {
                std::fs::create_dir_all(dir)?;
            }
            let record = serde_json::to_vec(
                &serde_json::json!({"file":file,"size":size,"state":state,"target":target}),
            )?;
            use std::io::Write;
            let mut output = std::fs::File::create(journal)?;
            output.write_all(&record)?;
            output.sync_all()?;
        }
        self.app.db.with_transaction(|db| {
            let order: i64 = if state == "published" {
                db.query_row("SELECT COALESCE(MAX(publish_order),0)+1 FROM transfer_file_journal", [], |r| r.get(0))?
            } else { 0 };
            let receive = if ["receiving", "assembled"].contains(&state) { reservation_bytes(size) } else { 0 };
            let assemble = if ["receiving", "assembled", "publishing"].contains(&state) { reservation_bytes(size) } else { 0 };
            db.execute("INSERT INTO transfer_file_journal(task_id,file_index,size,state,receive_reserved,assemble_reserved,target,publish_order)
                VALUES(?1,?2,?3,?4,?5,?6,?7,?8) ON CONFLICT(task_id,file_index) DO UPDATE SET
                state=excluded.state,receive_reserved=excluded.receive_reserved,assemble_reserved=excluded.assemble_reserved,
                target=COALESCE(excluded.target,target),publish_order=MAX(publish_order,excluded.publish_order)",
                params![id,file as i64,size as i64,state,receive as i64,assemble as i64,target,order])?;
            Ok(())
        })
    }

    pub(crate) fn release_cache_reservations(&self, id: &str) -> AppResult<()> {
        self.app.db.with_conn(|db| {
            db.execute("UPDATE transfer_file_journal SET receive_reserved=0,assemble_reserved=0 WHERE task_id=?1", [id])?;
            Ok(())
        })
    }

    pub(crate) async fn recovery_probe(
        &self,
        id: &str,
    ) -> AppResult<crate::network::protocol::Response> {
        let _writer = self.destination_writer.lock().await;
        let task = self.record(id)?;
        if task.recovery_epoch <= task.checked_epoch {
            return Ok(crate::network::protocol::Response::RecoveryFiles {
                epoch: task.recovery_epoch,
                files: vec![],
            });
        }
        let recent: Vec<usize> = self.app.db.with_conn(|db| {
            let mut query = db.prepare("SELECT file_index FROM transfer_file_journal WHERE task_id=?1 AND state='published' ORDER BY publish_order DESC LIMIT 3")?;
            let rows = query.query_map([id],|r| Ok(r.get::<_,i64>(0)? as usize))?;
            let mut files = rows.collect::<Result<Vec<_>,_>>()?;
            let mut query = db.prepare("SELECT file_index FROM transfer_file_journal WHERE task_id=?1 AND state='publishing'")?;
            for file in query.query_map([id],|r| Ok(r.get::<_,i64>(0)? as usize))? { let file = file?; if !files.contains(&file) { files.push(file); } }
            Ok(files)
        })?;
        let epoch = task.recovery_epoch;
        let files = tokio::task::spawn_blocking(move || {
            recent
                .into_iter()
                .map(|file| {
                    let hash = task
                        .targets
                        .get(file)
                        .and_then(Option::as_ref)
                        .and_then(|p| hash_file(std::path::Path::new(p)).ok());
                    (file, hash)
                })
                .collect()
        })
        .await
        .map_err(|e| AppError::Other(e.to_string()))?;
        Ok(crate::network::protocol::Response::RecoveryFiles { epoch, files })
    }

    pub(crate) async fn finish_partial(&self, id: &str) -> AppResult<()> {
        let task = self.record(id)?;
        if task.status == "partial_completed" {
            return Ok(());
        }
        if task
            .source_issue
            .as_ref()
            .is_none_or(|i| i.kind != "missing")
        {
            return Err(AppError::Conflict(
                "Partial completion requires a missing source file".into(),
            ));
        }
        self.edit(id, |t| {
            t.status = "finishing".into();
            t.error = None;
        })?;
        let result = async {
            if task.direction == "receive" {
                tracing::info!(task = %id, "Partial completion is waiting for queued writes");
                self.await_writes(id).await?;
                tracing::info!(task = %id, "Partial completion drained queued writes");
                // relay_active 只是调度标记，不代表转写器仍持有目标盘写锁。
                // 转写与本流程的每个文件发布都由 destination_writer 串行化；
                // 直接逐文件补齐即可，避免标记意外残留时永久卡在 finishing。
                if let Some(cache) = self.cache_root_for_task(id) {
                    let plan = self.relay_plan(id)?;
                    tracing::info!(task = %id, cached_files = plan.cache_files.len(), "Partial completion is draining cached files");
                    let published: std::collections::HashSet<usize> = self.app.db.with_conn(|db| {
                        let mut query = db.prepare(
                            "SELECT file_index FROM transfer_file_journal WHERE task_id=?1 AND state='published'",
                        )?;
                        let rows = query.query_map([id], |row| Ok(row.get::<_, i64>(0)? as usize))?;
                        Ok(rows.collect::<Result<_, _>>()?)
                    })?;
                    for (file, entry) in plan.entries.iter().enumerate() {
                        if !plan.cache_files.contains(&file)
                            || entry.directory
                            || entry.size == 0
                            || plan.targets.get(file).and_then(Option::as_ref).is_none()
                        {
                            continue;
                        }
                        if published.contains(&file)
                            && plan
                                .targets
                                .get(file)
                                .and_then(Option::as_deref)
                                .is_some_and(|target| {
                                    std::fs::metadata(target).is_ok_and(|metadata| {
                                        metadata.is_file() && metadata.len() == entry.size
                                    })
                                })
                        {
                            continue;
                        }
                        let base = plan.bases[file];
                        let count = entry.size.div_ceil(plan.chunk_size) as usize;
                        if !(base..base + count).all(|index| task.completed.contains(&index)) {
                            continue;
                        }
                        match super::relay::relay_file_step_shared(
                            self,
                            &plan,
                            &cache,
                            id,
                            file,
                            entry.size,
                            self.relayed_at(id, file),
                        )
                        .await
                        {
                            Ok(_) => {}
                            Err(AppError::Io(e))
                                if [
                                    std::io::ErrorKind::NotFound,
                                    std::io::ErrorKind::UnexpectedEof,
                                ]
                                .contains(&e.kind()) => {}
                            Err(AppError::Conflict(_)) => {}
                            Err(e) => return Err(e),
                        }
                    }
                    tracing::info!(task = %id, "Partial completion finished cached-file drain");
                }
                tracing::info!(task = %id, "Partial completion is committing direct-write files");
                for (file, entry) in task.manifest.entries.iter().enumerate() {
                    if entry.directory
                        || task.cache_files.contains(&file)
                        || task.targets.get(file).and_then(Option::as_ref).is_none()
                    {
                        continue;
                    }
                    let base = task.manifest.file_base(file);
                    let count = entry.size.div_ceil(task.chunk_size) as usize;
                    if !(base..base + count).all(|index| task.completed.contains(&index)) {
                        continue;
                    }
                    self.commit_ready_file(id, file).await?;
                }
                tracing::info!(task = %id, "Partial completion is scanning published files");
                let abandoned = self.missing_published_files(id).await?;
                tracing::info!(task = %id, abandoned = abandoned.len(), "Partial completion finished published-file scan");
                self.edit(id, |t| {
                    t.abandoned_files = abandoned.into_iter().collect();
                })?;
                let task = self.record(id)?;
                tracing::info!(task = %id, abandoned = task.abandoned_files.len(), "Partial completion is writing unreceived-file list");
                write_abandoned_manifest(&task)?;
                tracing::info!(task = %id, "Partial completion wrote unreceived-file list");
                self.release_files(id);
                if let Some(cache) = self.cache_root_for_task(id) {
                    for own in super::paths::cache_task_dirs(&cache, &task) {
                        if own.exists() { std::fs::remove_dir_all(own)?; }
                    }
                }
                tracing::info!(task = %id, "Partial completion removed cache files");
                super::paths::cleanup_stage(&self.record(id)?)?;
                self.release_cache_reservations(id)?;
                tracing::info!(task = %id, "Partial completion cleaned receive staging state");
            } else {
                self.edit(id, |t| {
                    t.abandoned_files = t
                        .manifest
                        .entries
                        .iter()
                        .enumerate()
                        .filter_map(|(file, e)| {
                            let base = t.manifest.file_base(file);
                            let count = e.size.div_ceil(t.chunk_size) as usize;
                            (!e.directory
                                && !(base..base + count).all(|i| t.completed.contains(&i)))
                            .then_some(file)
                        })
                        .collect();
                })?;
            }
            self.edit(id, |t| {
                t.status = "partial_completed".into();
                t.error = None;
                t.state_revision = t.state_revision.wrapping_add(1);
                t.event("Available files were kept. The task is partially complete; unreceived files are listed as abandoned");
            })?;
            Ok(())
        }
        .await;
        if let Err(error) = &result {
            self.edit(id, |t| {
                t.status = "paused".into();
                t.error = Some(format!("Could not finish the received files: {error}"));
            })?;
        }
        result
    }

    pub(crate) fn set_source_issue(&self, id: &str, file: usize, kind: &str) -> AppResult<()> {
        if !["changed", "missing", "unavailable"].contains(&kind) {
            return Err(AppError::BadRequest("Invalid source file status".into()));
        }
        let task = self.record(id)?;
        let entry = task
            .manifest
            .entries
            .get(file)
            .ok_or_else(|| AppError::BadRequest("Invalid file index".into()))?;
        let message = match kind {
            "changed" => "The source file changed. Resuming will retransmit the current version",
            "missing" => {
                "The source file is missing. You can finish with the received files or cancel"
            }
            _ => "The source file is temporarily unavailable. Resume to try again",
        };
        tracing::warn!(task = %id, file_index = file, path = %entry.path, issue = kind, "Source file requires attention");
        self.edit(id, |t| {
            t.source_issue = Some(super::model::SourceIssue {
                file,
                kind: kind.into(),
                accepted: false,
            });
            t.status = "paused".into();
            t.error = Some(format!("{message}: {}", entry.path));
            t.state_revision = t.state_revision.wrapping_add(1);
            t.event(format!("{message}: {}", entry.path));
        })?;
        Ok(())
    }

    pub(crate) fn replace_file_entry(
        &self,
        id: &str,
        file: usize,
        entry: super::model::FileEntry,
    ) -> AppResult<()> {
        let mut task = self.record(id)?;
        let old = task
            .manifest
            .entries
            .get(file)
            .ok_or_else(|| AppError::BadRequest("Invalid file index".into()))?;
        if old.path != entry.path || old.directory || entry.directory {
            return Err(AppError::BadRequest(
                "A replacement must refer to the same regular file path".into(),
            ));
        }
        let base = task.manifest.file_base(file);
        let old_size = old.size;
        // 缓存名称属于本地派生元数据：替换源文件版本时保持不变，
        // 否则同一文件的缓存路径会漂移，旧分段将无法再被识别。
        let preserved_name = old.name.clone();
        let preserved_cache_name = old.cache_name.clone();
        let old_count = old.size.div_ceil(task.chunk_size) as usize;
        let new_count = entry.size.div_ceil(task.chunk_size) as usize;
        let mut completed = std::collections::BTreeSet::new();
        for index in &task.completed {
            if *index < base {
                completed.insert(*index);
            } else if *index >= base + old_count {
                completed.insert(index - old_count + new_count);
            }
        }
        let mut entries = std::mem::take(&mut task.manifest.entries);
        entries[file] = {
            let mut replacement = entry.clone();
            replacement.name = preserved_name;
            replacement.cache_name = preserved_cache_name;
            replacement
        };
        task.manifest = super::model::Manifest::new(entries, task.chunk_size, task.verify_hash);
        task.total_size = task.manifest.total();
        task.chunk_count = task.manifest.chunk_count();
        task.digest = task.manifest.digest();
        task.completed = completed;
        task.completed_count = task.completed.len();
        task.completed_bytes = task
            .completed
            .iter()
            .filter_map(|i| task.manifest.chunk(*i))
            .map(|c| c.length as u64)
            .sum();
        task.source_issue = None;
        task.error = None;
        // 接收端是因为「源文件版本问题」被暂停的：新版本已确认并替换，
        // 必须同时恢复接收状态，否则它会停在 paused，发送方持续等用户操作。
        let unpaused = task.direction == "receive" && task.status == "paused";
        if unpaused {
            task.status = if task.destination.is_some() {
                "accepted"
            } else {
                "waiting"
            }
            .into();
            task.state_revision = task.state_revision.wrapping_add(1);
            task.event("A newer source file replaced the previous version. Receiving continues");
        }
        task.repair_files.insert(file);
        if let Some(value) = task.relayed.get_mut(file) {
            task.relayed_total = task.relayed_total.saturating_sub(*value);
            *value = 0;
        }
        if task.use_cache {
            task.relay_target_bytes = task
                .cache_files
                .iter()
                .filter(|i| task.targets.get(**i).and_then(Option::as_ref).is_some())
                .map(|i| task.manifest.entries[*i].size)
                .sum();
        }
        self.release_send_files(id);
        if let Some(runtime) = self.runtime_if_exists(id) {
            crate::locks::lock(&runtime.receive_files).remove(&file);
        }
        if task.direction == "receive" {
            if let Some(cache) = self.cache_root_for_task(id) {
                let assembled = super::paths::cache_file(&cache, &task, file);
                if assembled.exists() {
                    std::fs::remove_file(assembled)?;
                }
                for segment in 0..old_size.div_ceil(super::relay::SEG_BYTES) {
                    let path = super::paths::cache_segment(&cache, &task, file, segment);
                    match std::fs::remove_file(path) {
                        Ok(()) => {}
                        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                        Err(e) => return Err(e.into()),
                    }
                }
            }
            let stage = super::paths::stage(&task)?;
            for suffix in ["part", "committed"] {
                let path = stage.join(format!("{file}.{suffix}"));
                if path.exists() {
                    std::fs::remove_file(path)?;
                }
            }
        }
        let json = serde_json::to_string(&task)?;
        self.app.db.with_transaction(|db| {
            db.execute("UPDATE task_entries SET size=?1,modified_ms=?2 WHERE task_id=?3 AND index_no=?4",params![entry.size as i64,entry.modified_ms as i64,id,file as i64])?;
            let hashes: Vec<(usize,String)> = {
                let mut stmt = db.prepare("SELECT chunk_index,digest FROM transfer_chunk_hash WHERE task_id=?1")?;
                let rows = stmt.query_map([id],|r|Ok((r.get::<_,i64>(0)? as usize,r.get(1)?)))?;
                rows.collect::<Result<_,_>>()?
            };
            db.execute("DELETE FROM transfer_chunk_hash WHERE task_id=?1",[id])?;
            for (index,digest) in hashes {
                let shifted = if index < base { Some(index) } else if index >= base+old_count { Some(index-old_count+new_count) } else { None };
                if let Some(index) = shifted { db.execute("INSERT INTO transfer_chunk_hash(task_id,chunk_index,digest) VALUES(?1,?2,?3)",params![id,index as i64,digest])?; }
            }
            db.execute("DELETE FROM transfer_cache_digest WHERE task_id=?1 AND file_index=?2",params![id,file as i64])?;
            db.execute("DELETE FROM task_chunks WHERE task_id=?1",[id])?;
            for i in &task.completed { db.execute("INSERT INTO task_chunks(task_id,chunk_index) VALUES(?1,?2)",params![id,*i as i64])?; }
            db.execute("DELETE FROM task_relay WHERE task_id=?1 AND file_index=?2",params![id,file as i64])?;
            db.execute("DELETE FROM transfer_file_journal WHERE task_id=?1 AND file_index=?2",params![id,file as i64])?;
            db.execute("UPDATE transfer_tasks SET record=?1 WHERE id=?2",params![json,id])?;
            Ok(())
        })?;
        crate::locks::lock(&self.tasks).insert(id.to_string(), task.clone());
        // 状态从 paused 恢复必须立即通知控制会话，否则发送方仍停在"对端已暂停"。
        if unpaused {
            self.runtime(id).state_updates.send_replace(());
            if let Some(status) = Self::control_status(&task) {
                self.runtime(id)
                    .control_events
                    .send_replace(Some((task.state_revision, status.into())));
            }
        }
        Ok(())
    }
    pub(crate) fn invalidate_file(&self, id: &str, file: usize) -> AppResult<()> {
        self.reset_file_for_retry(id, file, true)
    }

    pub(crate) async fn fallback_cached_file_to_direct(
        &self,
        id: &str,
        file: usize,
    ) -> AppResult<bool> {
        let _admission = self.cache_admission.lock().await;
        let task = self.record(id)?;
        if !task.cache_files.contains(&file) {
            return Ok(false);
        }
        if self.journal_state(id, file)?.as_deref() == Some("published") {
            return Ok(false);
        }
        let entry = task
            .manifest
            .entries
            .get(file)
            .ok_or_else(|| AppError::BadRequest("Invalid file index".into()))?;
        let size = entry.size;
        self.reset_file_for_retry(id, file, false)?;
        self.edit(id, |current| {
            if current.cache_files.remove(&file) {
                current.use_cache = !current.cache_files.is_empty();
                current.relay_target_bytes = current
                    .cache_files
                    .iter()
                    .filter_map(|index| current.manifest.entries.get(*index))
                    .map(|entry| entry.size)
                    .sum();
                current.cache_plan_revision = current.cache_plan_revision.wrapping_add(1);
            }
        })?;
        self.app.db.with_conn(|db| {
            db.execute(
                "INSERT INTO transfer_file_journal(task_id,file_index,size,state,receive_reserved,assemble_reserved) \
                 VALUES(?1,?2,?3,'direct',0,0) \
                 ON CONFLICT(task_id,file_index) DO UPDATE SET size=excluded.size,state='direct',receive_reserved=0,assemble_reserved=0",
                params![id, file as i64, size as i64],
            )?;
            Ok(())
        })?;
        if let Some(runtime) = self.runtime_if_exists(id) {
            crate::locks::lock(&runtime.cache_fallback_pending).insert(file);
            let base = task.manifest.file_base(file);
            let end = base.saturating_add(size.div_ceil(task.chunk_size) as usize);
            crate::locks::lock(&runtime.write_pending).retain(|index| *index < base || *index >= end);
        }
        tracing::warn!(task = %id, file, size_bytes = size, "Cache space is insufficient; cached data was discarded and the complete file will be retransmitted directly");
        Ok(true)
    }

    fn reset_file_for_retry(&self, id: &str, file: usize, count_repair: bool) -> AppResult<()> {
        let task = self.record(id)?;
        let entry = task
            .manifest
            .entries
            .get(file)
            .ok_or_else(|| AppError::BadRequest("Invalid file index".into()))?;
        if count_repair {
            let attempts: i64 = self.app.db.with_transaction(|db| {
                db.execute("INSERT INTO transfer_file_repairs(task_id,file_index,attempts) VALUES(?1,?2,1) ON CONFLICT(task_id,file_index) DO UPDATE SET attempts=attempts+1",params![id,file as i64])?;
                Ok(db.query_row("SELECT attempts FROM transfer_file_repairs WHERE task_id=?1 AND file_index=?2",params![id,file as i64],|r| r.get(0))?)
            })?;
            if attempts > 3 {
                self.edit(id, |t| {
                    t.status = "failed".into();
                    t.error = Some("The received file could not be restored after three attempts".into());
                })?;
                return Err(AppError::Conflict(
                    "The received file could not be restored after three attempts".into(),
                ));
            }
        }
        let base = task.manifest.file_base(file);
        let end = base + entry.size.div_ceil(task.chunk_size) as usize;
        self.edit(id, |t| {
            t.repair_files.insert(file);
            t.completed.retain(|index| *index < base || *index >= end);
            t.completed_count = t.completed.len();
            t.completed_bytes = t
                .completed
                .iter()
                .filter_map(|index| t.manifest.chunk(*index))
                .map(|c| c.length as u64)
                .sum();
            if let Some(value) = t.relayed.get_mut(file) {
                t.relayed_total = t.relayed_total.saturating_sub(*value);
                *value = 0;
            }
            t.error = None;
            t.event(if count_repair {
                format!("File needs to be retransferred: {}", entry.path)
            } else {
                format!("Cache space is insufficient; the file will be retransmitted directly: {}", entry.path)
            });
        })?;
        self.save_decision(id)?;
        if let Some(cache) = self.cache_root_for_task(id) {
            let assembled = super::paths::cache_file(&cache, &task, file);
            if assembled.exists() {
                std::fs::remove_file(assembled)?;
            }
            for segment in 0..entry.size.div_ceil(super::relay::SEG_BYTES) {
                let path = super::paths::cache_segment(&cache, &task, file, segment);
                match std::fs::remove_file(path) {
                    Ok(()) => {}
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => return Err(e.into()),
                }
            }
        }
        self.app.db.with_conn(|db| {
            db.execute(
                "DELETE FROM task_relay WHERE task_id=?1 AND file_index=?2",
                params![id, file as i64],
            )?;
            db.execute(
                "DELETE FROM transfer_file_journal WHERE task_id=?1 AND file_index=?2",
                params![id, file as i64],
            )?;
            Ok(())
        })?;
        if let Ok(stage) = super::paths::stage(&task) {
            let marker = stage.join(format!("{file}.committed"));
            if marker.exists() {
                std::fs::remove_file(marker)?;
            }
        }
        Ok(())
    }

    pub(crate) async fn missing_published_files(&self, id: &str) -> AppResult<Vec<usize>> {
        let task = self.record(id)?;
        let published: std::collections::HashSet<usize> = self.app.db.with_conn(|db| {
            let mut query = db.prepare("SELECT file_index FROM transfer_file_journal WHERE task_id=?1 AND state='published'")?;
            let rows = query.query_map([id],|r| Ok(r.get::<_,i64>(0)? as usize))?;
            Ok(rows.collect::<Result<_,_>>()?)
        })?;
        tokio::task::spawn_blocking(move || {
            let stage = super::paths::stage(&task)?;
            let mut missing = Vec::new();
            for (file, entry) in task.manifest.entries.iter().enumerate() {
                let Some(target) = task.targets.get(file).and_then(Option::as_ref) else {
                    continue;
                };
                if entry.directory {
                    if !std::fs::metadata(target).is_ok_and(|m| m.is_dir()) {
                        missing.push(file);
                    }
                    continue;
                }
                let recorded = published.contains(&file)
                    || task.relayed.get(file).copied().unwrap_or(0) >= entry.size && entry.size > 0
                    || stage.join(format!("{file}.committed")).exists();
                if !recorded
                    || !std::fs::metadata(target)
                        .is_ok_and(|m| m.is_file() && m.len() == entry.size)
                {
                    missing.push(file);
                }
            }
            Ok(missing)
        })
        .await
        .map_err(|e| AppError::Other(e.to_string()))?
    }

    pub(crate) async fn repair_missing_published_cache_files(
        &self,
        id: &str,
    ) -> AppResult<Vec<usize>> {
        let _writer = self.destination_writer.lock().await;
        let task = self.record(id)?;
        let published: std::collections::HashSet<usize> = self.app.db.with_conn(|db| {
            let mut query = db.prepare(
                "SELECT file_index FROM transfer_file_journal WHERE task_id=?1 AND state IN ('published','publishing')",
            )?;
            let rows = query.query_map([id], |row| Ok(row.get::<_, i64>(0)? as usize))?;
            Ok(rows.collect::<Result<_, _>>()?)
        })?;
        let missing = tokio::task::spawn_blocking(move || {
            let mut missing = Vec::new();
            for file in published {
                if !task.cache_files.contains(&file) {
                    continue;
                }
                let Some(entry) = task.manifest.entries.get(file) else {
                    continue;
                };
                if entry.directory || entry.size == 0 {
                    continue;
                }
                let Some(target) = task.targets.get(file).and_then(Option::as_ref) else {
                    continue;
                };
                if !std::fs::metadata(target)
                    .is_ok_and(|metadata| metadata.is_file() && metadata.len() == entry.size)
                {
                    missing.push(file);
                }
            }
            Ok::<_, AppError>(missing)
        })
        .await
        .map_err(|error| AppError::Other(error.to_string()))??;
        for file in &missing {
            self.invalidate_file(id, *file)?;
        }
        Ok(missing)
    }

    pub(crate) async fn admit_cached_file(&self, id: &str, file: usize) -> AppResult<bool> {
        let task = self.record(id)?;
        let Some(entry) = task.manifest.entries.get(file) else {
            return Err(AppError::BadRequest("Invalid file index".into()));
        };
        if !task.cache_files.contains(&file) {
            return Ok(false);
        }
        let size = entry.size;
        let Some(cache_root) = self.cache_root_for_task(id) else {
            return self.fallback_cached_file_to_direct(id, file).await;
        };
        if !processing(&self.status_of(id)?) {
            return Err(AppError::Conflict("Task is no longer active".into()));
        }
        let mut retry_direct = false;
        let mut switched_without_data = false;
        {
            let _lock = self.cache_admission.lock().await;
            let current = self.record(id)?;
            if !current.cache_files.contains(&file) {
                return Ok(false);
            }
            let base = current.manifest.file_base(file);
            let end = base.saturating_add(size.div_ceil(current.chunk_size) as usize);
            let has_received_chunks = current.completed.range(base..end).next().is_some();
            let journal = self.journal_state(id, file)?;
            let has_cache_state = journal
                .as_deref()
                .is_some_and(|state| matches!(state, "assembled" | "publishing" | "published"));
            let has_cached_data = super::paths::cache_file(&cache_root, &task, file).exists()
                || super::paths::cache_segment(&cache_root, &task, file, 0).exists();
            match self.cache_budget(&cache_root).await {
                Ok(budget) => {
                    let capacity = budget.receive.min(budget.assemble);
                    let fits = super::cache_candidate_fits(size, 0, budget.max_file_size(), capacity);
                    if !fits
                        || !self.cached_file_reserved(id, file, size)?
                            && !self.reserve_cached_file(id, file, size, capacity)?
                    {
                        if has_received_chunks || has_cache_state || has_cached_data {
                            retry_direct = true;
                        } else {
                            self.disable_cached_file(id, file, size)?;
                            switched_without_data = true;
                        }
                    }
                    if switched_without_data {
                        tracing::info!(
                            task = %id,
                            file,
                            size_bytes = size,
                            disk_free_bytes = budget.free,
                            total_budget_bytes = budget.total,
                            "Cache capacity is unavailable; selecting direct destination writes for this file"
                        );
                    }
                }
                Err(_) => {
                    retry_direct = true;
                    tracing::warn!(task = %id, file, "Cache space could not be measured; switching this file to direct writes");
                }
            }
        }
        if retry_direct {
            return self.fallback_cached_file_to_direct(id, file).await;
        }
        Ok(false)
    }

    fn disable_cached_file(&self, id: &str, file: usize, size: u64) -> AppResult<()> {
        let mut changed = false;
        self.edit(id, |task| {
            if task.cache_files.remove(&file) {
                changed = true;
                task.use_cache = !task.cache_files.is_empty();
                task.relay_target_bytes = task.relay_target_bytes.saturating_sub(size);
                task.cache_plan_revision = task.cache_plan_revision.wrapping_add(1);
            }
        })?;
        if changed {
            self.app.db.with_conn(|db| {
                db.execute(
                    "INSERT INTO transfer_file_journal(task_id,file_index,size,state,receive_reserved,assemble_reserved) \
                     VALUES(?1,?2,?3,'direct',0,0) \
                     ON CONFLICT(task_id,file_index) DO UPDATE SET state='direct',receive_reserved=0,assemble_reserved=0",
                    params![id, file as i64, size as i64],
                )?;
                Ok(())
            })?;
        }
        Ok(())
    }
}

fn write_abandoned_manifest(task: &super::model::TaskRecord) -> AppResult<()> {
    if task.abandoned_files.is_empty() {
        return Ok(());
    }
    let destination = task
        .destination
        .as_deref()
        .ok_or_else(|| AppError::Other("Missing receive destination".into()))?;
    let output_dir = if task.top_level_count == 1 {
        task.manifest
            .entries
            .first()
            .filter(|entry| entry.directory)
            .and_then(|_| task.targets.first().and_then(Option::as_deref))
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| std::path::PathBuf::from(destination))
    } else {
        std::path::PathBuf::from(destination)
    };
    std::fs::create_dir_all(&output_dir)?;
    let mut paths = task
        .abandoned_files
        .iter()
        .filter_map(|index| task.manifest.entries.get(*index))
        .filter(|entry| !entry.directory)
        .map(|entry| entry.path.replace('\\', "/"))
        .collect::<Vec<_>>();
    paths.sort();
    let name = format!(
        "NodeSend-unreceived-files-{}.txt",
        super::paths::safe_component(&task.id)
    );
    std::fs::write(output_dir.join(name), format!("{}\n", paths.join("\n")))?;
    Ok(())
}

#[cfg(test)]
mod tests;
