use super::{
    model::{FileEntry, Manifest, SourceRoot, TaskRecord},
    paths, CHUNK_SIZE, MANIFEST_PAGE,
};
use crate::error::{AppError, AppResult};
use sha2::{Digest, Sha256};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::{Instant, UNIX_EPOCH},
};

pub fn modified(meta: &std::fs::Metadata) -> u64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Incremental, deterministic directory scanner for streaming transfers.
///
/// The selected roots are emitted first (so the receiver can present the
/// top-level decision without waiting for recursive traversal). Subsequent
/// calls walk each root in depth-first, filename-sorted order. This iterator
/// deliberately does not build a full `Vec<FileEntry` before yielding data.
pub struct SourceScanner {
    roots: Vec<SourceRoot>,
    next_root: usize,
    stack: Vec<(PathBuf, String, usize, bool)>,
    roots_expanded: bool,
}

impl SourceScanner {
    pub fn new(selected: Vec<String>) -> AppResult<Self> {
        if selected.is_empty() {
            return Err(AppError::BadRequest("请选择文件或目录".into()));
        }
        let mut roots = Vec::with_capacity(selected.len());
        for raw in selected {
            let path = Path::new(&raw);
            paths::ensure_plain(path)?;
            let meta = std::fs::symlink_metadata(path)?;
            paths::ensure_plain_meta(&meta)?;
            if !meta.is_file() && !meta.is_dir() {
                return Err(AppError::BadRequest("只支持普通文件和目录".into()));
            }
            let name = path
                .file_name()
                .and_then(|n| n.to_str())
                .ok_or_else(|| AppError::BadRequest("不能直接发送磁盘根目录".into()))?
                .to_string();
            roots.push(SourceRoot {
                name: name.clone(),
                path: path.canonicalize()?.to_string_lossy().into_owned(),
                directory: meta.is_dir(),
                size: meta.len(),
                modified_ms: modified(&meta),
                legacy_format: false,
            });
        }
        Ok(Self {
            roots,
            next_root: 0,
            stack: Vec::new(),
            roots_expanded: false,
        })
    }

    pub fn roots(&self) -> &[SourceRoot] {
        &self.roots
    }

    /// Rebuild a deterministic scan position after restart without serializing
    /// the OS directory iterator/DFS stack. `last_seq` is the number of entries
    /// already committed to the manifest (1-based for a non-empty cursor), and
    /// `last_path` is the path of that final committed entry. We deliberately
    /// rescan from the beginning and verify the cursor rather than trusting a
    /// stale filesystem iterator position.
    pub fn resume_after(&mut self, last_seq: usize, last_path: &str) -> AppResult<()> {
        if last_seq == 0 {
            if !last_path.is_empty() {
                return Err(AppError::BadRequest("空扫描游标不能包含路径".into()));
            }
            return Ok(());
        }
        if last_path.is_empty() {
            return Err(AppError::BadRequest("扫描游标缺少最后路径".into()));
        }
        for seq in 1..=last_seq {
            let entry = self.next_entry()?.ok_or_else(|| {
                AppError::Conflict("源目录已变化，无法恢复清单扫描位置".into())
            })?;
            if seq == last_seq && entry.path != last_path {
                return Err(AppError::Conflict(
                    "源目录的确定性扫描顺序已变化，无法恢复清单扫描位置".into(),
                ));
            }
        }
        Ok(())
    }

    /// Read at most `limit` entries without materializing the rest of the tree.
    /// The caller can persist/send each batch before asking for the next one.
    pub fn next_batch(&mut self, limit: usize) -> AppResult<Vec<FileEntry>> {
        if limit == 0 || limit > 5000 {
            return Err(AppError::BadRequest(
                "清单批次大小必须在 1 到 5000 之间".into(),
            ));
        }
        let mut batch = Vec::with_capacity(limit);
        while batch.len() < limit {
            match self.next_entry()? {
                Some(entry) => batch.push(entry),
                None => break,
            }
        }
        Ok(batch)
    }

    /// Returns the next entry, or `None` when the traversal is complete.
    pub fn next_entry(&mut self) -> AppResult<Option<FileEntry>> {
        if self.next_root < self.roots.len() {
            let root = &self.roots[self.next_root];
            self.next_root += 1;
            return Ok(Some(FileEntry {
                path: root.name.clone(),
                size: if root.directory { 0 } else { root.size },
                modified_ms: root.modified_ms,
                directory: root.directory,
            }));
        }

        if !self.roots_expanded {
            // Reverse-push so the first selected root is traversed first.
            for root in self.roots.iter().rev() {
                if root.directory {
                    self.stack
                        .push((PathBuf::from(&root.path), root.name.clone(), 0, false));
                }
            }
            self.roots_expanded = true;
        }

        while let Some((path, relative, depth, emit)) = self.stack.pop() {
            if depth > 64 {
                return Err(AppError::BadRequest("目录层级超出限制".into()));
            }
            if !emit {
                let mut children = std::fs::read_dir(&path)?.collect::<Result<Vec<_>, _>>()?;
                children.sort_by_key(|entry| entry.file_name());
                for child in children.into_iter().rev() {
                    let name = child
                        .file_name()
                        .into_string()
                        .map_err(|_| AppError::BadRequest("文件名不是 UTF-8".into()))?;
                    self.stack
                        .push((child.path(), format!("{relative}/{name}"), depth + 1, true));
                }
                continue;
            }

            let name = relative.rsplit('/').next().unwrap_or("");
            paths::validate_component(name)?;
            let meta = std::fs::symlink_metadata(&path)?;
            paths::ensure_plain_meta(&meta)?;
            if !meta.is_file() && !meta.is_dir() {
                return Err(AppError::BadRequest("只支持普通文件和目录".into()));
            }
            if meta.is_dir() {
                self.stack
                    .push((path.clone(), relative.clone(), depth, false));
            }
            return Ok(Some(FileEntry {
                path: relative,
                size: if meta.is_file() { meta.len() } else { 0 },
                modified_ms: modified(&meta),
                directory: meta.is_dir(),
            }));
        }
        Ok(None)
    }
}

/// Fingerprint selected top-level items only. Directory contents are deliberately not traversed.
pub fn top_level_fingerprint(roots: &[SourceRoot]) -> String {
    let mut hasher = Sha256::new();
    for root in roots {
        for value in [root.name.as_bytes(), root.path.as_bytes()] {
            hasher.update((value.len() as u64).to_le_bytes());
            hasher.update(value);
        }
        hasher.update([root.directory as u8]);
        hasher.update(root.size.to_le_bytes());
        hasher.update(root.modified_ms.to_le_bytes());
    }
    format!("{:x}", hasher.finalize())
}

/// Re-read only each selected root's own metadata; never enumerate descendants.
pub fn verify_top_level_fingerprint(task: &TaskRecord) -> AppResult<()> {
    if task.top_level_fingerprint.is_empty() {
        // Compatibility for tasks persisted before the top-level fingerprint existed.
        return Ok(());
    }
    let mut current = Vec::with_capacity(task.sources.len());
    for root in &task.sources {
        let path = Path::new(&root.path);
        let meta = std::fs::symlink_metadata(path)?;
        paths::ensure_plain_meta(&meta)?;
        current.push(SourceRoot {
            name: root.name.clone(),
            path: root.path.clone(),
            directory: meta.is_dir(),
            size: meta.len(),
            modified_ms: modified(&meta),
            legacy_format: false,
        });
    }
    if top_level_fingerprint(&current) != task.top_level_fingerprint {
        return Err(AppError::Conflict(
            "选中的顶层文件或目录已变化，不能从旧清单继续；请重新选择并发送".into(),
        ));
    }
    Ok(())
}
/// 一次性采集整份清单（含递归遍历）。
///
/// 生产发送路径不调用它：发送走 `SourceScanner` 流式扫描（边扫边发）。
/// 这里保留给"需要一份完整清单"的场景（集成测试、小规模校验），
/// 分块大小统一取 [`CHUNK_SIZE`]，不再各自维护一套策略。
pub fn collect(selected: Vec<String>, verify_hash: bool) -> AppResult<(Manifest, Vec<SourceRoot>)> {
    if selected.is_empty() {
        return Err(AppError::BadRequest("请选择文件或目录".into()));
    }
    let started = Instant::now();
    let mut scanner = SourceScanner::new(selected)?;
    let roots = scanner.roots().to_vec();
    let mut entries = Vec::new();
    loop {
        let batch = scanner.next_batch(MANIFEST_PAGE)?;
        if batch.is_empty() {
            break;
        }
        entries.extend(batch);
    }
    let manifest = Manifest::new(entries, CHUNK_SIZE, verify_hash);
    manifest.validate()?;
    tracing::info!(
        entries = manifest.entries.len(),
        elapsed_ms = started.elapsed().as_millis() as u64,
        "已采集完整发送清单"
    );
    Ok((manifest, roots))
}
/// 由「顶层来源映射 + 条目相对路径」拼出磁盘上的真实来源路径。
///
/// 顶层名唯一（重复路径会被 `Manifest::validate` 拒绝），因此按名匹配不会歧义。
pub fn source_path(task: &TaskRecord, relative: &str) -> AppResult<PathBuf> {
    source_path_from_roots(&task.sources, relative)
}
pub fn source_path_from_roots(roots: &[super::model::SourceRoot], relative: &str) -> AppResult<PathBuf> {
    let root = roots
        .iter()
        .find(|r| relative == r.name || relative.starts_with(&format!("{}/", r.name)))
        .ok_or_else(|| AppError::Other(format!("源文件位置缺失：{relative}")))?;
    let mut path = PathBuf::from(&root.path);
    for part in relative[root.name.len()..]
        .split('/')
        .filter(|p| !p.is_empty())
    {
        path.push(part);
    }
    Ok(path)
}
/// 读取一个分块。`cached` 为发送端缓存的读句柄（同一文件的连续分块复用同一
/// 句柄）：机械盘上每次 open/seek 都会打断顺序预读，复用句柄是持续吞吐的前提。
///
/// 无论是否复用句柄，都会用 `metadata(path)` 重新核对"当前磁盘上的文件"是否
/// 仍与清单一致——缓存句柄察觉不到文件被替换（Windows 上旧文件对象仍可读）。
pub async fn read_entry_chunk_with(
    cached: Option<&Arc<tokio::sync::Mutex<tokio::fs::File>>>,
    roots: &[super::model::SourceRoot],
    entry: &FileEntry,
    offset: u64,
    length: usize,
) -> AppResult<Vec<u8>> {
    use tokio::io::{AsyncReadExt, AsyncSeekExt};
    let path = source_path_from_roots(roots, &entry.path)?;
    paths::ensure_plain(&path)?;
    let meta = tokio::fs::metadata(&path).await?;
    if meta.len() != entry.size || modified(&meta) != entry.modified_ms {
        return Err(AppError::Conflict(format!("源文件已变化：{}", entry.path)));
    }
    let mut data = vec![0; length];
    let short_read = |error: std::io::Error| -> AppResult<Vec<u8>> {
        if error.kind() == std::io::ErrorKind::UnexpectedEof {
            Err(AppError::Conflict(format!(
                "读取期间源文件变化：{}",
                entry.path
            )))
        } else {
            Err(error.into())
        }
    };
    match cached {
        Some(handle) => {
            let mut file = handle.lock().await;
            file.seek(std::io::SeekFrom::Start(offset)).await?;
            if let Err(error) = file.read_exact(&mut data).await {
                return short_read(error);
            }
        }
        None => {
            let mut file = tokio::fs::File::open(&path).await?;
            file.seek(std::io::SeekFrom::Start(offset)).await?;
            if let Err(error) = file.read_exact(&mut data).await {
                return short_read(error);
            }
        }
    }
    let after = tokio::fs::metadata(&path).await?;
    if after.len() != entry.size || modified(&after) != entry.modified_ms {
        return Err(AppError::Conflict(format!(
            "读取期间源文件变化：{}",
            entry.path
        )));
    }
    Ok(data)
}

#[cfg(test)]
mod fingerprint_tests {
    use super::{top_level_fingerprint, SourceRoot, SourceScanner};
    use std::{
        fs,
        time::{SystemTime, UNIX_EPOCH},
    };

    fn root(modified_ms: u64) -> SourceRoot {
        SourceRoot {
            name: "folder".into(),
            path: "D:/data/folder".into(),
            directory: true,
            size: 4096,
            modified_ms,
            legacy_format: false,
        }
    }

    #[test]
    fn top_level_fingerprint_uses_only_root_metadata() {
        assert_eq!(
            top_level_fingerprint(&[root(10)]),
            top_level_fingerprint(&[root(10)])
        );
        assert_ne!(
            top_level_fingerprint(&[root(10)]),
            top_level_fingerprint(&[root(11)])
        );
    }

    #[test]
    fn scanner_emits_all_roots_before_sorted_descendants() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let base =
            std::env::temp_dir().join(format!("nodesend-scan-{}-{nonce}", std::process::id()));
        let first = base.join("first");
        let second = base.join("second.txt");
        fs::create_dir_all(first.join("dir")).unwrap();
        fs::write(first.join("b.txt"), b"b").unwrap();
        fs::write(first.join("a.txt"), b"a").unwrap();
        fs::write(first.join("dir").join("c.txt"), b"c").unwrap();
        fs::write(&second, b"second").unwrap();

        let mut scanner = SourceScanner::new(vec![
            first.to_string_lossy().into_owned(),
            second.to_string_lossy().into_owned(),
        ])
        .unwrap();
        let mut paths = Vec::new();
        while let Some(entry) = scanner.next_entry().unwrap() {
            paths.push(entry.path);
        }
        assert_eq!(
            paths,
            [
                "first",
                "second.txt",
                "first/a.txt",
                "first/b.txt",
                "first/dir",
                "first/dir/c.txt",
            ]
        );
        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn scanner_resumes_after_a_verified_deterministic_cursor() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let base =
            std::env::temp_dir().join(format!("nodesend-resume-{}-{nonce}", std::process::id()));
        let first = base.join("first");
        fs::create_dir_all(first.join("dir")).unwrap();
        fs::write(first.join("a.txt"), b"a").unwrap();
        fs::write(first.join("b.txt"), b"b").unwrap();
        fs::write(first.join("dir").join("c.txt"), b"c").unwrap();

        let selected = vec![first.to_string_lossy().into_owned()];
        let mut scanner = SourceScanner::new(selected.clone()).unwrap();
        let committed = scanner.next_batch(2).unwrap();
        assert_eq!(committed.last().unwrap().path, "first/a.txt");

        let mut resumed = SourceScanner::new(selected).unwrap();
        resumed.resume_after(2, "first/a.txt").unwrap();
        let next = resumed.next_entry().unwrap().unwrap();
        assert_eq!(next.path, "first/b.txt");

        let mut invalid = SourceScanner::new(vec![first.to_string_lossy().into_owned()]).unwrap();
        assert!(invalid.resume_after(2, "first/not-a.txt").is_err());
        fs::remove_dir_all(base).unwrap();
    }
}
