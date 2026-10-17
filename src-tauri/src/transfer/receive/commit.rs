//! Synchronous filesystem commit phase for received files.
use super::super::model::ConflictPolicy;
use super::super::{model::TaskRecord, paths, TransferService};
use crate::error::{AppError, AppResult};
use std::path::Path;

fn remove_published_file(path: &Path) -> AppResult<()> {
    let mut last_error = None;
    for attempt in 0..6 {
        match std::fs::remove_file(path) {
            Ok(()) => return Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => {
                last_error = Some(error);
                if attempt < 5 {
                    std::thread::sleep(std::time::Duration::from_millis(40 * (attempt + 1)));
                }
            }
        }
    }
    Err(last_error.expect("remove_file attempted at least once").into())
}

impl TransferService {
    /// Commit staged files using synchronous filesystem calls.
    /// The caller must run this on Tokio's blocking pool.
    pub(super) fn commit_staged_files(
        task: &TaskRecord,
        root: &Path,
        stage: &Path,
    ) -> AppResult<()> {
        std::fs::create_dir_all(stage)?;
        for (i, entry) in task.manifest.entries.iter().enumerate() {
            Self::commit_staged_file(task, root, stage, i)?;
            // Directories are created by commit_staged_file as they become
            // available in the streaming manifest.
            let _ = entry;
        }
        Ok(())
    }

    /// Commit one file only after the caller has verified that all its bytes
    /// are durable at the destination staging path. A missing/incomplete part
    /// is deliberately not treated as an error by the incremental scheduler;
    /// the final task commit still validates every file.
    pub(super) fn commit_staged_file(
        task: &TaskRecord,
        root: &Path,
        stage: &Path,
        i: usize,
    ) -> AppResult<()> {
        let Some(entry) = task.manifest.entries.get(i) else {
            return Ok(());
        };
        let Some(target) = task.targets.get(i).and_then(Option::as_ref) else {
            return Ok(());
        };
        let target = Path::new(target);
        paths::ensure_under(root, target)?;
        if entry.directory {
            std::fs::create_dir_all(target)?;
            return Ok(());
        }
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }
        // Cache-mode relay publishes directly to the final destination and
        // records progress on the cache side. There is intentionally no
        // destination `.part` file for these entries.
        if task.cache_files.contains(&i) {
            if let Ok(metadata) = std::fs::metadata(target) {
                if metadata.is_file() && metadata.len() == entry.size {
                    return Ok(());
                }
            }
            return Err(AppError::Conflict("缓存文件尚未转移到目标路径".into()));
        }
        let part = stage.join(format!("{i}.part"));
        let done = stage.join(format!("{i}.committed"));
        paths::ensure_under(root, &part)?;
        paths::ensure_under(root, &done)?;
        // Rename stays on the same filesystem and is atomic on the normal path.
        if done.exists() {
            return Ok(());
        }
        if !part.exists() && entry.size == 0 {
            std::fs::File::create(&part)?.sync_all()?;
        }
        if std::fs::metadata(&part)?.len() != entry.size {
            return Err(AppError::Conflict("接收文件大小与清单不匹配".into()));
        }
        if target.exists() {
            if same_file::is_same_file(&part, target).unwrap_or(false) {
                std::fs::write(&done, b"same")?;
                return Ok(());
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
                return Ok(());
            } else {
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
        // Record that this task published the target and capture a small
        // metadata fingerprint. Cancellation only removes it if it still
        // looks like the file we published, preserving later user edits.
        let metadata = std::fs::metadata(target)?;
        let modified = metadata
            .modified()
            .ok()
            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|duration| duration.as_nanos())
            .unwrap_or(0);
        std::fs::write(&done, format!("owned|{}|{modified}", metadata.len()))?;
        Ok(())
    }

    /// Roll back only files that this task published. Existing files displaced
    /// by overwrite are restored from `.previous`; directories are removed only
    /// when empty. This runs before the task staging directory is deleted.
    pub(crate) fn rollback_committed_files(
        task: &TaskRecord,
        root: &Path,
        stage: &Path,
    ) -> AppResult<()> {
        paths::ensure_under(root, stage)?;
        for (i, entry) in task.manifest.entries.iter().enumerate().rev() {
            let Some(raw_target) = task.targets.get(i).and_then(Option::as_ref) else {
                continue;
            };
            let target = Path::new(raw_target);
            paths::ensure_under(root, target)?;
            if entry.directory {
                // Remove only empty directories under the selected destination.
                // Non-empty or user-populated directories are left untouched.
                if target.is_dir() {
                    match std::fs::remove_dir(target) {
                        Ok(()) => {}
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                        Err(error) if error.kind() == std::io::ErrorKind::DirectoryNotEmpty => {}
                        Err(error) => return Err(error.into()),
                    }
                }
                continue;
            }
            if task.cache_files.contains(&i) {
                if task.relayed.get(i).copied().unwrap_or(0) >= entry.size
                    && target.is_file()
                    && std::fs::metadata(target).map(|m| m.len() == entry.size).unwrap_or(false)
                {
                    remove_published_file(target)?;
                }
                continue;
            }
            let marker = stage.join(format!("{i}.committed"));
            let backup = stage.join(format!("{i}.previous"));
            if !marker.is_file() {
                // A crash or marker-write failure may happen after the old
                // target was moved aside. Never let stage cleanup destroy it.
                if backup.exists() {
                    if !target.exists() {
                        std::fs::rename(&backup, target)?;
                    } else {
                        let name = target.file_name().and_then(|v| v.to_str()).unwrap_or("recovered");
                        let mut recovery = target.with_file_name(format!("{name}.nodesend-recovered-{}-{i}", paths::safe_component(&task.id)));
                        let mut suffix = 1usize;
                        while recovery.exists() {
                            recovery = target.with_file_name(format!("{name}.nodesend-recovered-{}-{i}-{suffix}", paths::safe_component(&task.id)));
                            suffix += 1;
                        }
                        std::fs::rename(&backup, &recovery)?;
                        tracing::warn!(path = %recovery.display(), "缺少提交标记，覆盖前的文件已另存为恢复文件");
                    }
                } else if target.exists() && Self::looks_fully_received(task, i, target, entry) {
                    // A cancellation can race the final rename and marker write.
                    // With no backup, only remove a target when every chunk for
                    // this file was durably acknowledged and its size matches;
                    // skipped/pre-existing files do not satisfy this condition.
                    remove_published_file(target)?;
                }
                continue;
            }
            let marker_value = std::fs::read_to_string(&marker).unwrap_or_default();
            if marker_value.trim() == "same" {
                continue;
            }
            let owned = if marker_value.trim().is_empty() {
                // Older builds used empty markers for both newly published
                // files and same-file matches. Ownership is ambiguous, so do
                // not delete the target solely on that evidence.
                false
            } else if let Some(expected) = marker_value.strip_prefix("owned|") {
                let mut fields = expected.split('|');
                let expected_len = fields.next().and_then(|v| v.parse::<u64>().ok());
                // Explorer opening/previewing a file can change timestamp
                // precision without changing its content. The destination
                // path and size are the durable publication identity here;
                // do not treat a mere read/open as a user replacement.
                let _ = fields.next();
                std::fs::metadata(target)
                    .map(|metadata| metadata.is_file() && expected_len == Some(metadata.len()))
                    .unwrap_or(false)
            } else {
                false
            };
            if backup.exists() {
                if target.exists() && owned {
                    remove_published_file(target)?;
                }
                if !target.exists() {
                    std::fs::rename(&backup, target)?;
                } else {
                    // The target changed after publication. Keep both copies:
                    // move the displaced original to a visible recovery file
                    // rather than deleting it with the staging directory.
                    let name = target
                        .file_name()
                        .and_then(|v| v.to_str())
                        .unwrap_or("recovered");
                    let mut recovery = target.with_file_name(format!(
                        "{name}.nodesend-recovered-{}-{i}",
                        paths::safe_component(&task.id)
                    ));
                    let mut suffix = 1usize;
                    while recovery.exists() {
                        recovery = target.with_file_name(format!(
                            "{name}.nodesend-recovered-{}-{i}-{suffix}",
                            paths::safe_component(&task.id)
                        ));
                        suffix += 1;
                    }
                    std::fs::rename(&backup, &recovery)?;
                    tracing::warn!(path = %recovery.display(), "目标文件在取消前发生变化，原文件已另存为恢复文件");
                }
            } else if owned && target.exists() {
                remove_published_file(target)?;
            }
        }
        // A directory can remain when its child file was published before the
        // directory entry was committed. Sweep manifest directories once more
        // in reverse depth order, removing only empty paths under this task's
        // receive root.
        for (i, entry) in task.manifest.entries.iter().enumerate().rev() {
            if !entry.directory {
                continue;
            }
            let Some(raw_target) = task.targets.get(i).and_then(Option::as_ref) else {
                continue;
            };
            let target = Path::new(raw_target);
            paths::ensure_under(root, target)?;
            if target.is_dir() {
                let _ = std::fs::remove_dir(target);
            }
        }
        Ok(())
    }

    fn looks_fully_received(
        task: &TaskRecord,
        file: usize,
        target: &Path,
        entry: &super::super::model::FileEntry,
    ) -> bool {
        let Ok(metadata) = std::fs::metadata(target) else {
            return false;
        };
        if !metadata.is_file() || metadata.len() != entry.size {
            return false;
        }
        let count = entry.size.div_ceil(task.chunk_size) as usize;
        let first = task.manifest.file_base(file);
        (first..first.saturating_add(count)).all(|index| task.completed.contains(&index))
    }
}
