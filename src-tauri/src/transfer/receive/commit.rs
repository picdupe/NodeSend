//! Synchronous filesystem commit phase for received files.
use super::super::model::ConflictPolicy;
use super::super::{model::TaskRecord, paths, TransferService};
use crate::error::{AppError, AppResult};
use std::path::Path;

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
                // Keep directories: some may have existed before the transfer,
                // and an empty directory is safer than deleting user data.
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
                let expected_modified = fields.next().and_then(|v| v.parse::<u128>().ok());
                match (std::fs::metadata(target), expected_len, expected_modified) {
                    (Ok(metadata), Some(len), Some(modified)) => {
                        let actual_modified = metadata
                            .modified()
                            .ok()
                            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                            .map(|duration| duration.as_nanos())
                            .unwrap_or(0);
                        metadata.is_file() && metadata.len() == len && actual_modified == modified
                    }
                    _ => false,
                }
            } else {
                false
            };
            if backup.exists() {
                if target.exists() && owned {
                    std::fs::remove_file(target)?;
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
                std::fs::remove_file(target)?;
            }
        }
        Ok(())
    }
}
