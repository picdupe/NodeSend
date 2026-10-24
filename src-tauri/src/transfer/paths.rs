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
        validate_component(part)?;
    }
    Ok(())
}

#[cfg(target_os = "android")]
pub fn android_media_directory(path: &str) -> &'static str {
    let extension = Path::new(path)
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if matches!(
        extension.as_str(),
        "jpg" | "jpeg" | "png" | "gif" | "webp" | "bmp" | "heic" | "heif" | "avif"
            | "dng" | "tif" | "tiff"
    ) {
        "Pictures/NodeSend"
    } else if matches!(
        extension.as_str(),
        "mp4" | "mkv" | "mov" | "avi" | "webm" | "m4v" | "3gp" | "ts" | "m2ts"
    ) {
        "Movies/NodeSend"
    } else if matches!(
        extension.as_str(),
        "mp3" | "flac" | "wav" | "m4a" | "aac" | "ogg" | "opus" | "wma" | "amr"
            | "mid" | "midi"
    ) {
        "Music/NodeSend"
    } else {
        "Download/NodeSend"
    }
}

/// 校验单个文件名组件，避免遍历目录时对每个条目反复扫描整个相对路径。
pub fn validate_component(part: &str) -> AppResult<()> {
    let stem = part.split('.').next().unwrap_or("");
    let reserved = ["CON", "PRN", "AUX", "NUL", "CLOCK$"]
        .iter()
        .any(|name| stem.eq_ignore_ascii_case(name));
    let device_prefix = stem.get(..3);
    let device_number = stem.len() == 4
        && device_prefix.is_some_and(|prefix| {
            prefix.eq_ignore_ascii_case("COM") || prefix.eq_ignore_ascii_case("LPT")
        })
        && stem.as_bytes()[3].is_ascii_digit();
    let nodesend_reserved = part
        .get(..10)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case(".nodesend-"));
    if part.is_empty()
        || part == "."
        || part == ".."
        || part.len() > 240
        || part.ends_with(['.', ' '])
        || part
            .chars()
            .any(|c| c.is_control() || r#"<>:"|?*"#.contains(c))
        || reserved
        || device_number
        || nodesend_reserved
    {
        return Err(invalid());
    }
    Ok(())
}
fn invalid() -> AppError {
    AppError::BadRequest("文件路径包含越界、保留名称或非法字符".into())
}
/// 判定一份**未跟随符号链接**的元数据是否为普通项：拒绝符号链接与 Windows 重解析点。
///
/// 与 [`ensure_plain`] 的检查完全一致，区别是复用调用方已经取得的 `symlink_metadata`，
/// 从而避免对同一路径重复发起一次 stat（遍历路径上每条目省一次文件系统调用）。
pub fn ensure_plain_meta(meta: &std::fs::Metadata) -> AppResult<()> {
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
    Ok(())
}
pub fn ensure_plain(path: &Path) -> AppResult<()> {
    if let Ok(meta) = std::fs::symlink_metadata(path) {
        ensure_plain_meta(&meta)?;
    }
    Ok(())
}
/// 词法路径安全检查：只做字符串判断，不访问文件系统。
///
/// 清单分片一次可达数千条，逐条 `ensure_under` 会在每个路径段上做
/// symlink_metadata/exists/canonicalize（十几条系统调用），在 NAS、HDD 或
/// 网络盘上会把一帧拖到几十秒，进而让发送端误报"网络读取超时"。
/// 因此清单阶段用词法检查；真正的符号链接/重解析点校验仍在
/// [`ensure_under`]，由"确认接收"规划目标路径与"提交落盘"时执行。
pub fn ensure_relative_safe(path: &str) -> AppResult<()> {
    if path.is_empty()
        || path.starts_with('/')
        || path.starts_with('\\')
        || path.contains(':')
        || path.contains('\0')
    {
        return Err(invalid());
    }
    for part in path.split('/') {
        if part.is_empty() || part == "." || part == ".." || part.contains('\\') {
            return Err(invalid());
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

/// 缓存目录/文件名安全化：任务 ID 来自网络（`in-<peer>-<wire>`），
/// 只保留字母数字与 `-`/`_`，杜绝路径分隔符或 `..` 逃逸缓存根目录。
pub fn safe_component(raw: &str) -> String {
    raw.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// 某任务在缓存盘上的根目录（`<cache_root>/<safe task id>`）。
pub fn cache_task(cache_root: &Path, task_id: &str) -> PathBuf {
    cache_root.join(safe_component(task_id))
}

/// 缓存分段文件：`<cache_root>/<task>/<file>.<segment>.seg`。
/// 分段是为了在 Windows 上能「从头部」回收已转写的数据（Windows 无法截断文件头部）。
pub fn cache_segment(cache_root: &Path, task_id: &str, file: usize, segment: u64) -> PathBuf {
    cache_task(cache_root, task_id).join(format!("{file}.{segment}.seg"))
}

/// Completed file assembled on the cache volume before it is copied to the
/// destination volume. Keeping this separate from the destination `.part`
/// prevents the mechanical disk from seeing network-sized writes.
pub fn cache_file(cache_root: &Path, task_id: &str, file: usize) -> PathBuf {
    cache_task(cache_root, task_id).join(format!("{file}.file"))
}

/// 两个路径是否位于同一卷（同卷说明缓存与目标共用一块盘，中转只会加倍 I/O，
/// 调用方据此回退为直写）。
pub fn same_volume(a: &Path, b: &Path) -> bool {
    #[cfg(windows)]
    {
        fn prefix(path: &Path) -> Option<String> {
            let real = path.canonicalize().ok()?;
            match real.components().next()? {
                std::path::Component::Prefix(prefix) => {
                    Some(prefix.as_os_str().to_string_lossy().to_uppercase())
                }
                _ => None,
            }
        }
        matches!((prefix(a), prefix(b)), (Some(x), Some(y)) if x == y)
    }
    #[cfg(not(windows))]
    {
        match (std::fs::metadata(a), std::fs::metadata(b)) {
            (Ok(x), Ok(y)) => {
                use std::os::unix::fs::MetadataExt;
                x.dev() == y.dev()
            }
            _ => false,
        }
    }
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
