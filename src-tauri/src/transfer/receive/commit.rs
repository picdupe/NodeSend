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
    Err(last_error
        .expect("remove_file attempted at least once")
        .into())
}

impl TransferService {
    pub(in crate::transfer) fn commit_staged_file(
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
        if done.exists() {
            let metadata = std::fs::metadata(target)?;
            if !metadata.is_file() || metadata.len() != entry.size {
                return Err(AppError::Conflict(
                    "Published file is missing or its size does not match the manifest".into(),
                ));
            }
            return Ok(());
        }
        if task.cache_files.contains(&i)
            && !part.exists()
            && task.relayed.get(i).copied().unwrap_or(0) >= entry.size
            && std::fs::metadata(target).is_ok_and(|m| m.is_file() && m.len() == entry.size)
        {
            return Ok(());
        }
        if !part.exists() && entry.size == 0 {
            std::fs::create_dir_all(stage)?;
            std::fs::File::create(&part)?.sync_all()?;
        }
        if std::fs::metadata(&part)?.len() != entry.size {
            return Err(AppError::Conflict(
                "Received file size does not match the manifest".into(),
            ));
        }
        if target.exists() {
            if same_file::is_same_file(&part, target).unwrap_or(false) {
                std::fs::write(&done, b"same")?;
                return Ok(());
            }
            if task.repair_files.contains(&i) {
                remove_published_file(target)?;
            } else if matches!(task.conflict, ConflictPolicy::Overwrite) {
                let backup = stage.join(format!("{i}.previous"));
                if !backup.exists() {
                    std::fs::rename(target, &backup)?;
                } else {
                    return Err(AppError::Conflict(
                        "Target changed again during recovery; check the conflicting file".into(),
                    ));
                }
            } else if matches!(task.conflict, ConflictPolicy::Skip) {
                return Ok(());
            } else {
                return Err(AppError::Conflict(format!(
                    "The target appeared during save: {}",
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
        let metadata = std::fs::metadata(target)?;
        if !metadata.is_file() || metadata.len() != entry.size {
            return Err(AppError::Conflict(
                "Published file is missing or its size does not match the manifest".into(),
            ));
        }
        let modified = metadata
            .modified()
            .ok()
            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|duration| duration.as_nanos())
            .unwrap_or(0);
        std::fs::write(&done, format!("owned|{}|{modified}", metadata.len()))?;
        Ok(())
    }

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
            if task.cache_files.contains(&i) && !stage.join(format!("{i}.committed")).exists() {
                if task.relayed.get(i).copied().unwrap_or(0) >= entry.size
                    && target.is_file()
                    && std::fs::metadata(target)
                        .map(|m| m.len() == entry.size)
                        .unwrap_or(false)
                {
                    remove_published_file(target)?;
                }
                continue;
            }
            let marker = stage.join(format!("{i}.committed"));
            let backup = stage.join(format!("{i}.previous"));
            if !marker.is_file() {
                if backup.exists() {
                    if !target.exists() {
                        std::fs::rename(&backup, target)?;
                    } else {
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
                        tracing::warn!(path = %recovery.display(), "Missing commit marker; the file was saved as a recovery copy before replacement");
                    }
                } else if target.exists() && Self::looks_fully_received(task, i, target, entry) {
                    remove_published_file(target)?;
                }
                continue;
            }
            let marker_value = std::fs::read_to_string(&marker).unwrap_or_default();
            if marker_value.trim() == "same" {
                continue;
            }
            let owned = if marker_value.trim().is_empty() {
                false
            } else if let Some(expected) = marker_value.strip_prefix("owned|") {
                let mut fields = expected.split('|');
                let expected_len = fields.next().and_then(|v| v.parse::<u64>().ok());
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
                    tracing::warn!(path = %recovery.display(), "Target changed before cancellation; original file was saved as a recovery copy");
                }
            } else if owned && target.exists() {
                remove_published_file(target)?;
            }
        }
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
