//! Reject traversal, ADS, symlinks/reparse points and Windows device names on all OSes.
use super::model::{ConflictPolicy, TaskRecord};
use crate::error::{AppError, AppResult};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

pub fn validate_relative(raw: &str) -> AppResult<()> {
    if raw.len() > 2048 || raw.is_empty() || raw.starts_with('/') || raw.contains('\\') {
        return Err(invalid());
    }
    for part in raw.split('/') {
        let stem = part.split('.').next().unwrap_or("").to_ascii_uppercase();
        let device = ["CON", "PRN", "AUX", "NUL", "CLOCK$"].contains(&stem.as_str())
            || (stem.len() == 4
                && (stem.starts_with("COM") || stem.starts_with("LPT"))
                && stem.as_bytes()[3].is_ascii_digit());
        if part.is_empty()
            || part == "."
            || part == ".."
            || part.len() > 240
            || part.ends_with(['.', ' '])
            || part
                .chars()
                .any(|c| c.is_control() || r#"<>:"|?*"#.contains(c))
            || device
            || part.to_lowercase().starts_with(".nodesend-")
        {
            return Err(invalid());
        }
    }
    Ok(())
}
fn invalid() -> AppError {
    AppError::BadRequest("文件路径包含越界、保留名称或非法字符".into())
}
pub fn ensure_plain(path: &Path) -> AppResult<()> {
    if let Ok(meta) = std::fs::symlink_metadata(path) {
        if meta.file_type().is_symlink() {
            return Err(AppError::Forbidden("不允许符号链接".into()));
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt;
            if meta.file_attributes() & 0x400 != 0 {
                return Err(AppError::Forbidden("不允许重解析点".into()));
            }
        }
    }
    Ok(())
}
pub fn ensure_under(root: &Path, path: &Path) -> AppResult<()> {
    let relative = path.strip_prefix(root).map_err(|_| invalid())?;
    ensure_plain(root)?;
    let mut cursor = root.to_path_buf();
    for component in relative.components() {
        if !matches!(component, std::path::Component::Normal(_)) {
            return Err(invalid());
        }
        cursor.push(component);
        ensure_plain(&cursor)?;
        if cursor.exists() && !cursor.canonicalize()?.starts_with(root) {
            return Err(invalid());
        }
    }
    Ok(())
}
pub fn destination(raw: &str) -> AppResult<PathBuf> {
    let path = Path::new(raw);
    if !path.is_absolute() || !path.is_dir() {
        return Err(AppError::BadRequest("请选择已存在的接收目录".into()));
    }
    // Android's shared-storage paths (for example /storage/emulated/0) may
    // contain platform-owned aliases. Resolve the user-selected directory
    // first, then apply the symlink/reparse-point checks to the real path.
    // This keeps the transfer boundary enforced while avoiding a false
    // rejection of Android's storage mount aliases.
    let real = path.canonicalize()?;
    for part in real.ancestors() {
        ensure_plain(part)?;
    }
    Ok(real)
}
pub fn plan_targets(
    task: &TaskRecord,
    root: &Path,
    policy: &ConflictPolicy,
    selected: Option<&[String]>,
) -> AppResult<Vec<Option<String>>> {
    let mut roots = HashMap::new();
    let mut used = std::collections::HashSet::new();
    for entry in &task.manifest.entries {
        let top = entry.path.split('/').next().unwrap();
        if roots.contains_key(top) {
            continue;
        }
        let mut candidate = top.to_string();
        let mut n = 1;
        while used.contains(&candidate.to_lowercase())
            || (matches!(policy, ConflictPolicy::Rename) && root.join(&candidate).exists())
        {
            candidate = numbered_name(top, entry.directory || entry.path.contains('/'), n);
            n += 1;
        }
        used.insert(candidate.to_lowercase());
        roots.insert(top.to_string(), candidate);
    }
    let mut result = Vec::new();
    for entry in &task.manifest.entries {
        let included = selected.is_none_or(|s| {
            s.iter().any(|p| {
                entry.path == *p
                    || entry.path.starts_with(&format!("{p}/"))
                    || (entry.directory && p.starts_with(&format!("{}/", entry.path)))
            })
        });
        if !included {
            result.push(None);
            continue;
        }
        let (top, tail) = entry.path.split_once('/').unwrap_or((&entry.path, ""));
        let mut target = root.join(&roots[top]);
        if !tail.is_empty() {
            target.push(tail);
        }
        ensure_under(root, &target)?;
        if target.exists() && !entry.directory && matches!(policy, ConflictPolicy::Skip) {
            result.push(None);
            continue;
        }
        if target.exists() && (entry.directory != target.is_dir()) {
            return Err(AppError::Conflict(format!("路径类型冲突：{}", entry.path)));
        }
        result.push(Some(target.to_string_lossy().into_owned()));
    }
    Ok(result)
}
pub fn stage(task: &TaskRecord) -> AppResult<PathBuf> {
    let root = PathBuf::from(
        task.destination
            .as_deref()
            .ok_or_else(|| AppError::Forbidden("任务尚未接受".into()))?,
    );
    let stage = root.join(".nodesend-partials").join(&task.id);
    ensure_under(&root, &stage)?;
    Ok(stage)
}

/// Remove only the partial directory owned by this task. The task destination
/// and the generated task id are both validated before removal so cancelling a
/// transfer can never broaden cleanup to the receive root.
pub fn cleanup_stage(task: &TaskRecord) -> AppResult<()> {
    let stage = stage(task)?;
    if stage.exists() {
        std::fs::remove_dir_all(stage)?;
    }
    Ok(())
}

fn numbered_name(name: &str, directory: bool, number: usize) -> String {
    if !directory {
        // Treat a tar archive and its compression suffix as one extension.
        // Compare case-insensitively while preserving the original spelling.
        let lower = name.to_ascii_lowercase();
        for suffix in [
            ".tar.gz",
            ".tar.bz2",
            ".tar.xz",
            ".tar.zst",
            ".tar.zstd",
            ".tar.lz",
            ".tar.lzma",
            ".tar.lzo",
            ".tar.lz4",
            ".tar.br",
            ".tar.z",
        ] {
            if lower.ends_with(suffix) && name.len() > suffix.len() {
                let (stem, extension) = name.split_at(name.len() - suffix.len());
                return format!("{stem} ({number}){extension}");
            }
        }
        let path = Path::new(name);
        if let (Some(stem), Some(extension)) = (path.file_stem(), path.extension()) {
            return format!(
                "{} ({number}).{}",
                stem.to_string_lossy(),
                extension.to_string_lossy()
            );
        }
    }
    format!("{name} ({number})")
}

#[cfg(test)]
mod tests {
    use super::numbered_name;

    #[test]
    fn rename_preserves_file_extension_and_directory_names() {
        assert_eq!(numbered_name("报告.pdf", false, 1), "报告 (1).pdf");
        assert_eq!(numbered_name("photo.JPG", false, 2), "photo (2).JPG");
        assert_eq!(
            numbered_name("archive.tar.gz", false, 1),
            "archive (1).tar.gz"
        );
        for suffix in [
            "tar.bz2", "tar.xz", "tar.zst", "tar.zstd", "tar.lz", "tar.lzma", "tar.lzo", "tar.lz4",
            "tar.br", "tar.Z", "TAR.GZ",
        ] {
            assert_eq!(
                numbered_name(&format!("资料.v2.{suffix}"), false, 2),
                format!("资料.v2 (2).{suffix}")
            );
        }
        assert_eq!(
            numbered_name("archive.tar.gz", true, 1),
            "archive.tar.gz (1)"
        );
        assert_eq!(
            numbered_name("report.v2.pdf", false, 1),
            "report.v2 (1).pdf"
        );
        assert_eq!(numbered_name("README", false, 1), "README (1)");
        assert_eq!(numbered_name(".gitignore", false, 1), ".gitignore (1)");
        assert_eq!(numbered_name("资料.v2", true, 1), "资料.v2 (1)");
    }
}
