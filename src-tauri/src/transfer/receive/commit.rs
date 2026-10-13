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
            // Rename stays on the same filesystem and is atomic on the normal path.
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
}
