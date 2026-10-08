use super::{
    model::{FileEntry, Manifest},
    paths,
};
use crate::error::{AppError, AppResult};
use std::{path::Path, time::UNIX_EPOCH};
pub fn modified(meta: &std::fs::Metadata) -> u64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}
pub fn collect(selected: Vec<String>, verify_hash: bool) -> AppResult<(Manifest, Vec<String>)> {
    if selected.is_empty() || selected.len() > 10000 {
        return Err(AppError::BadRequest("请选择文件或目录".into()));
    }
    let mut entries = Vec::new();
    let mut sources = Vec::new();
    for raw in selected {
        let path = Path::new(&raw);
        paths::ensure_plain(path)?;
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| AppError::BadRequest("不能直接发送磁盘根目录".into()))?;
        walk(path, name.to_string(), &mut entries, &mut sources, 0)?;
    }
    let total: u64 = entries.iter().map(|e| e.size).sum();
    let manifest = Manifest {
        entries,
        // Amortize durable receiver writes and ACKs without changing the wire format.
        chunk_size: if total > 16 * 1024 * 1024 {
            4194304
        } else {
            262144
        },
        verify_hash,
    };
    manifest.validate()?;
    Ok((manifest, sources))
}
fn walk(
    path: &Path,
    relative: String,
    entries: &mut Vec<FileEntry>,
    sources: &mut Vec<String>,
    depth: usize,
) -> AppResult<()> {
    if depth > 64 || entries.len() >= 10000 {
        return Err(AppError::BadRequest("目录层级或文件数量超出限制".into()));
    }
    paths::ensure_plain(path)?;
    paths::validate_relative(&relative)?;
    let meta = std::fs::symlink_metadata(path)?;
    if !meta.is_file() && !meta.is_dir() {
        return Err(AppError::BadRequest("只支持普通文件和目录".into()));
    }
    entries.push(FileEntry {
        path: relative.clone(),
        size: if meta.is_file() { meta.len() } else { 0 },
        modified_ms: modified(&meta),
        directory: meta.is_dir(),
    });
    sources.push(path.canonicalize()?.to_string_lossy().into_owned());
    if meta.is_dir() {
        let mut children = std::fs::read_dir(path)?.collect::<Result<Vec<_>, _>>()?;
        children.sort_by_key(|e| e.file_name());
        for child in children {
            let name = child
                .file_name()
                .into_string()
                .map_err(|_| AppError::BadRequest("文件名不是 UTF-8".into()))?;
            walk(
                &child.path(),
                format!("{relative}/{name}"),
                entries,
                sources,
                depth + 1,
            )?;
        }
    }
    Ok(())
}
pub async fn read_chunk(
    task: &super::model::TaskRecord,
    chunk: super::model::Chunk,
) -> AppResult<Vec<u8>> {
    use tokio::io::{AsyncReadExt, AsyncSeekExt};
    let entry = &task.manifest.entries[chunk.file];
    let path = &task.sources[chunk.file];
    paths::ensure_plain(Path::new(path))?;
    let mut file = tokio::fs::File::open(path).await?;
    let meta = file.metadata().await?;
    if meta.len() != entry.size || modified(&meta) != entry.modified_ms {
        return Err(AppError::Conflict(format!("源文件已变化：{}", entry.path)));
    }
    file.seek(std::io::SeekFrom::Start(chunk.offset)).await?;
    let mut data = vec![0; chunk.length];
    if let Err(error) = file.read_exact(&mut data).await {
        if error.kind() == std::io::ErrorKind::UnexpectedEof {
            return Err(AppError::Conflict(format!("读取期间源文件变化：{}", entry.path)));
        }
        return Err(error.into());
    }
    let after = file.metadata().await?;
    if after.len() != entry.size || modified(&after) != entry.modified_ms {
        return Err(AppError::Conflict(format!(
            "读取期间源文件变化：{}",
            entry.path
        )));
    }
    Ok(data)
}
