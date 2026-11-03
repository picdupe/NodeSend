use super::model::{ConflictPolicy, FileEntry, TaskRecord};
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
        "jpg"
            | "jpeg"
            | "png"
            | "gif"
            | "webp"
            | "bmp"
            | "heic"
            | "heif"
            | "avif"
            | "dng"
            | "tif"
            | "tiff"
    ) {
        "Pictures/NodeSend"
    } else if matches!(
        extension.as_str(),
        "mp4" | "mkv" | "mov" | "avi" | "webm" | "m4v" | "3gp" | "ts" | "m2ts"
    ) {
        "Movies/NodeSend"
    } else if matches!(
        extension.as_str(),
        "mp3" | "flac" | "wav" | "m4a" | "aac" | "ogg" | "opus" | "wma" | "amr" | "mid" | "midi"
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
    AppError::BadRequest("File path contains traversal, a reserved name, or invalid characters".into())
}
/// 判定一份**未跟随符号链接**的元数据是否为普通项：拒绝符号链接与 Windows 重解析点。
///
/// 与 [`ensure_plain`] 的检查完全一致，区别是复用调用方已经取得的 `symlink_metadata`，
/// 从而避免对同一路径重复发起一次 stat（遍历路径上每条目省一次文件系统调用）。
pub fn ensure_plain_meta(meta: &std::fs::Metadata) -> AppResult<()> {
    if meta.file_type().is_symlink() {
        return Err(AppError::Forbidden("Symbolic links are not allowed".into()));
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if meta.file_attributes() & 0x400 != 0 {
            return Err(AppError::Forbidden("Reparse points are not allowed".into()));
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
/// 设置页允许填写尚不存在的接收目录：只做词法校验，目录在首次真正接收时创建。
pub fn validate_destination(raw: &str) -> AppResult<()> {
    let path = Path::new(raw);
    if raw.trim().is_empty() || !path.is_absolute() || raw.contains('\0') {
        return Err(AppError::BadRequest("Receive directory must be an absolute path".into()));
    }
    Ok(())
}
/// 首次真正使用接收目录时创建并解析，避免设置后尚未使用就在磁盘上留下空目录。
pub fn destination_for_use(raw: &str) -> AppResult<PathBuf> {
    validate_destination(raw)?;
    let path = Path::new(raw);
    if !path.exists() {
        std::fs::create_dir_all(path)?;
    }
    destination(raw)
}
pub fn destination(raw: &str) -> AppResult<PathBuf> {
    let path = Path::new(raw);
    if !path.is_absolute() || !path.is_dir() {
        return Err(AppError::BadRequest("Select an existing receive directory".into()));
    }
    // 解析规范路径，并检查其祖先路径是否为符号链接或 Windows 重解析点。
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
    let skipped_roots: std::collections::HashSet<_> = roots
        .iter()
        .filter(|(_, name)| matches!(policy, ConflictPolicy::Skip) && root.join(name).exists())
        .map(|(top, _)| top.clone())
        .collect();
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
        if skipped_roots.contains(top) {
            result.push(None);
            continue;
        }
        let mut target = root.join(&roots[top]);
        if !tail.is_empty() {
            target.push(tail);
        }
        ensure_under(root, &target)?;
        if target.exists() && (entry.directory != target.is_dir()) {
            return Err(AppError::Conflict(format!(
                "Path type conflicts with {}",
                entry.path
            )));
        }
        result.push(Some(target.to_string_lossy().into_owned()));
    }
    Ok(result)
}
pub fn stage(task: &TaskRecord) -> AppResult<PathBuf> {
    let root = PathBuf::from(
        task.destination
            .as_deref()
            .ok_or_else(|| AppError::Forbidden("Task has not been accepted".into()))?,
    );
    let stage = root.join(".nodesend-partials").join(&task.id);
    ensure_under(&root, &stage)?;
    Ok(stage)
}

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

/// 缓存名称中的 Node ID 前缀：统一使用带连字符、带校验位的规范 Node ID。
/// 旧记录（64 位十六进制）无法规整时退化为安全化截断，保证仍可构造路径。
fn node_prefix(node_id: &str) -> String {
    if let Some(canonical) = crate::identity::node_identity::normalize_node_id(node_id) {
        return canonical;
    }
    let compact: String = node_id
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .take(24)
        .collect();
    if compact.is_empty() {
        "node".into()
    } else {
        compact
    }
}

/// 拆分文件名与扩展名（不处理多段压缩扩展名：缓存名称只要求稳定与唯一）。
fn split_extension(name: &str) -> (String, String) {
    match name.rfind('.') {
        Some(position) if position > 0 && name.len() - position <= 17 => {
            (name[..position].to_string(), name[position..].to_string())
        }
        _ => (name.to_string(), String::new()),
    }
}

/// 清理单个文件名：控制字符、路径分隔符与 Windows 非法字符替换为 `_`，
/// 去掉结尾的 `.`/空格，规避 Windows 保留名称，并按字符数限长（保留扩展名）。
pub fn sanitize_file_name(raw: &str) -> String {
    let mut cleaned = String::with_capacity(raw.len());
    for ch in raw.chars() {
        if ch.is_control() || matches!(ch, '/' | '\\' | '<' | '>' | ':' | '"' | '|' | '?' | '*') {
            cleaned.push('_');
        } else {
            cleaned.push(ch);
        }
    }
    let cleaned = cleaned
        .trim_end_matches(|c: char| c == '.' || c == ' ')
        .to_string();
    let mut cleaned = if cleaned.is_empty() {
        "file".to_string()
    } else {
        cleaned
    };
    let stem = cleaned.split('.').next().unwrap_or("").to_ascii_uppercase();
    let reserved = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL" | "CLOCK$")
        || (stem.len() == 4
            && (stem.starts_with("COM") || stem.starts_with("LPT"))
            && stem.as_bytes()[3].is_ascii_digit());
    if reserved {
        cleaned.insert(0, '_');
    }
    const MAX_CHARS: usize = 120;
    if cleaned.chars().count() > MAX_CHARS {
        let (name, extension) = split_extension(&cleaned);
        let room = MAX_CHARS.saturating_sub(extension.chars().count());
        let head: String = name.chars().take(room).collect();
        cleaned = format!("{head}{extension}");
    }
    cleaned
}

/// 单个文件在缓存盘上的名称：`<NodeID>-<文件名>`，同名条目追加稳定序号：
/// `duplicate` 是同一基名在本任务清单中已分配的条目数（0 表示首个）。
pub fn cache_name(node_id: &str, display_name: &str, duplicate: usize) -> String {
    let prefix = node_prefix(node_id);
    let part = sanitize_file_name(display_name);
    if duplicate == 0 {
        return format!("{prefix}-{part}");
    }
    let (stem, extension) = split_extension(&part);
    format!("{prefix}-{stem}-{}{extension}", duplicate + 1)
}

/// 按清单顺序为条目分配稳定缓存名称。
///
/// `seq` 为「基名 → 已分配数量」的计数器，调用方跨页复用，
/// 因此同一份清单分多页到达时序号依然连续，且重启后可由清单重建。
pub fn assign_cache_names(
    entries: &mut [FileEntry],
    node_id: &str,
    seq: &mut std::collections::HashMap<String, usize>,
) {
    for entry in entries.iter_mut() {
        if entry.directory {
            entry.cache_name.clear();
            continue;
        }
        if entry.name.is_empty() {
            entry.name = display_name(entry);
        }
        let key = sanitize_file_name(&entry.name).to_ascii_lowercase();
        let duplicate = {
            let counter = seq.entry(key).or_insert(0);
            let assigned = *counter;
            *counter += 1;
            assigned
        };
        let top = entry.path.split('/').next().unwrap_or(&entry.name);
        let expected_prefix = format!("{}-{}--", node_id, sanitize_file_name(top));
        if !entry.cache_name.starts_with(&expected_prefix) {
            let top = sanitize_file_name(top);
            let inner = cache_name(node_id, &entry.name, duplicate);
            entry.cache_name = format!("{node_id}-{top}--{inner}");
        }
    }
}

/// 任务缓存目录：`<cache_root>/<safe task id>`。
pub fn cache_task(cache_root: &Path, task: &TaskRecord) -> PathBuf {
    task.manifest
        .entries
        .iter()
        .find(|entry| !entry.directory && !entry.cache_name.is_empty())
        .map(|entry| cache_dir_named(cache_root, &task.id, Some(&entry.cache_name)))
        .unwrap_or_else(|| cache_task_dir(cache_root, &task.id))
}

pub fn cache_task_dirs(cache_root: &Path, task: &TaskRecord) -> Vec<PathBuf> {
    let mut dirs = std::collections::BTreeSet::new();
    for entry in &task.manifest.entries {
        if entry.directory || entry.cache_name.is_empty() {
            continue;
        }
        dirs.insert(cache_dir_named(
            cache_root,
            &task.id,
            Some(&entry.cache_name),
        ));
    }
    if dirs.is_empty() {
        dirs.insert(cache_task_dir(cache_root, &task.id));
    }
    dirs.into_iter().collect()
}

/// 只依赖任务 ID 的缓存目录路径（热路径与目录清理使用，不触碰清单）。
pub fn cache_task_dir(cache_root: &Path, task_id: &str) -> PathBuf {
    cache_root.join(safe_component(task_id))
}

/// 由已持久化的清单重建「基名 → 已分配数量」计数器。
///
/// 重启后新到达的清单分页要继续分配序号，不能从 0 重来；由于清单本身
/// 已保存每条的缓存名称，按清单顺序重新计数即可得到与首次分配一致的计数。
pub fn rebuild_cache_name_seq(entries: &[FileEntry]) -> HashMap<String, usize> {
    let mut seq = HashMap::new();
    for entry in entries {
        if entry.directory || entry.cache_name.is_empty() {
            continue;
        }
        let key = sanitize_file_name(&display_name(entry)).to_ascii_lowercase();
        *seq.entry(key).or_insert(0) += 1;
    }
    seq
}

/// 条目显示名：优先使用清单中持久化的 `name`，旧清单回退到路径最后一段。
pub fn display_name(entry: &FileEntry) -> String {
    if !entry.name.is_empty() {
        return entry.name.clone();
    }
    entry
        .path
        .rsplit('/')
        .next()
        .unwrap_or(&entry.path)
        .to_string()
}

/// 该文件持久化的稳定缓存名称；`None` 表示任务没有可用的缓存映射。
pub fn cache_name_of(task: &TaskRecord, file: usize) -> Option<&str> {
    task.manifest
        .entries
        .get(file)
        .map(|entry| entry.cache_name.as_str())
        .filter(|name| !name.is_empty())
}

/// 缓存分段文件：`<cache_root>/<task>/<缓存名>.<segment>.seg`。
/// 分段是为了在 Windows 上能「从头部」回收已转写的数据（Windows 无法截断文件头部）。
pub fn cache_segment(cache_root: &Path, task: &TaskRecord, file: usize, segment: u64) -> PathBuf {
    cache_segment_named(
        cache_root,
        &task.id,
        file,
        cache_name_of(task, file),
        segment,
        task.manifest.entries[file].size,
    )
}

pub fn cache_file(cache_root: &Path, task: &TaskRecord, file: usize) -> PathBuf {
    cache_file_named(cache_root, &task.id, file, cache_name_of(task, file))
}

/// 已解析缓存名称的分段路径（热路径用，避免为每个块克隆清单）。
pub fn cache_segment_named(
    cache_root: &Path,
    task_id: &str,
    file: usize,
    cache_name: Option<&str>,
    segment: u64,
    size: u64,
) -> PathBuf {
    let dir = cache_dir_named(cache_root, task_id, cache_name);
    if size <= super::relay::SEG_BYTES {
        return match cache_name {
            Some(name) => dir.join(format!("{}.seg", cache_file_stem(name))),
            None => dir.join(format!("{file}.seg")),
        };
    }
    match cache_name {
        Some(name) => dir.join(format!("{}.{}.seg", cache_file_stem(name), segment)),
        None => dir.join(format!("{file}.{segment}.seg")),
    }
}

/// 已解析缓存名称的组装文件路径（热路径用）。
pub fn cache_file_named(
    cache_root: &Path,
    task_id: &str,
    file: usize,
    cache_name: Option<&str>,
) -> PathBuf {
    let dir = cache_dir_named(cache_root, task_id, cache_name);
    match cache_name {
        Some(name) => dir.join(cache_file_name(name)),
        None => dir.join(format!("{file}.file")),
    }
}

/// 缓存文件的发布日志路径：与缓存文件同名，扩展改为 `.journal`。
pub fn cache_journal_named(
    cache_root: &Path,
    task_id: &str,
    file: usize,
    cache_name: Option<&str>,
) -> PathBuf {
    let dir = cache_dir_named(cache_root, task_id, cache_name);
    match cache_name {
        Some(name) => dir.join(format!("{}.journal", cache_file_name(name))),
        None => dir.join(format!("{file}.journal")),
    }
}

fn cache_dir_named(cache_root: &Path, task_id: &str, cache_name: Option<&str>) -> PathBuf {
    let key = cache_name
        .and_then(|name| name.split_once("--").map(|(dir, _)| dir))
        .or(cache_name)
        .unwrap_or(task_id);
    cache_root.join(safe_component(key))
}

fn cache_file_name(cache_name: &str) -> String {
    if let Some((_, inner)) = cache_name.split_once("--") {
        return cache_file_name(inner);
    }
    cache_name
        .splitn(6, '-')
        .nth(5)
        .map(str::to_string)
        .unwrap_or_else(|| cache_name.to_string())
}

fn cache_file_stem(cache_name: &str) -> String {
    cache_file_name(cache_name)
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

    const NODE: &str = "7K3M-9Q2V-XB5N-4T8H-R6";

    #[test]
    fn cache_names_use_node_prefix_and_stable_duplicates() {
        assert_eq!(
            super::cache_name(NODE, "photo.jpg", 0),
            "7K3M-9Q2V-XB5N-4T8H-R6-photo.jpg"
        );
        assert_eq!(
            super::cache_name(NODE, "photo.jpg", 1),
            "7K3M-9Q2V-XB5N-4T8H-R6-photo-2.jpg"
        );
        assert_eq!(
            super::cache_name(NODE, "CON.txt", 0),
            "7K3M-9Q2V-XB5N-4T8H-R6-_CON.txt"
        );
        assert_eq!(
            super::cache_name(NODE, "bad:name?.txt", 0),
            "7K3M-9Q2V-XB5N-4T8H-R6-bad_name_.txt"
        );
    }

    #[test]
    fn duplicate_numbers_continue_from_the_persisted_manifest() {
        use crate::transfer::model::FileEntry;
        let mut entries = vec![
            FileEntry {
                path: "a/photo.jpg".into(),
                ..Default::default()
            },
            FileEntry {
                path: "b/photo.jpg".into(),
                ..Default::default()
            },
        ];
        let mut seq = std::collections::HashMap::new();
        super::assign_cache_names(&mut entries, NODE, &mut seq);
        assert_eq!(
            entries[0].cache_name,
            "7K3M-9Q2V-XB5N-4T8H-R6-a--7K3M-9Q2V-XB5N-4T8H-R6-photo.jpg"
        );
        assert_eq!(
            entries[1].cache_name,
            "7K3M-9Q2V-XB5N-4T8H-R6-b--7K3M-9Q2V-XB5N-4T8H-R6-photo-2.jpg"
        );
        // 后续清单分页按已持久化清单重建序号，从 3 继续而非从 0 重来。
        let mut page = vec![FileEntry {
            path: "c/photo.jpg".into(),
            ..Default::default()
        }];
        let mut seq = super::rebuild_cache_name_seq(&entries);
        super::assign_cache_names(&mut page, NODE, &mut seq);
        assert_eq!(
            page[0].cache_name,
            "7K3M-9Q2V-XB5N-4T8H-R6-c--7K3M-9Q2V-XB5N-4T8H-R6-photo-3.jpg"
        );
    }

    #[test]
    fn files_under_one_top_level_folder_share_cache_directory() {
        use crate::transfer::model::FileEntry;
        use std::path::Path;
        let mut entries = vec![
            FileEntry {
                path: "CentOS-Stream-10/a.iso".into(),
                name: "a.iso".into(),
                ..Default::default()
            },
            FileEntry {
                path: "CentOS-Stream-10/CHECKSUM".into(),
                name: "CHECKSUM".into(),
                ..Default::default()
            },
            FileEntry {
                path: "other/b.iso".into(),
                name: "b.iso".into(),
                ..Default::default()
            },
        ];
        super::assign_cache_names(&mut entries, NODE, &mut std::collections::HashMap::new());
        let root = Path::new("D:/cache");
        let first = super::cache_segment_named(
            root,
            "task",
            0,
            Some(&entries[0].cache_name),
            0,
            8 * 1024 * 1024,
        );
        let second =
            super::cache_segment_named(root, "task", 1, Some(&entries[1].cache_name), 0, 4096);
        let third =
            super::cache_segment_named(root, "task", 2, Some(&entries[2].cache_name), 0, 4096);
        assert_eq!(first.parent(), second.parent());
        assert_ne!(first.parent(), third.parent());
        assert_eq!(first.file_name().unwrap(), "a.iso.0.seg");
        assert_eq!(second.file_name().unwrap(), "CHECKSUM.seg");
        let boundary = super::cache_segment_named(
            root,
            "task",
            0,
            Some(&entries[0].cache_name),
            0,
            super::super::relay::SEG_BYTES,
        );
        assert_eq!(boundary.file_name().unwrap(), "a.iso.seg");
        let next = super::cache_segment_named(
            root,
            "task",
            0,
            Some(&entries[0].cache_name),
            1,
            super::super::relay::SEG_BYTES + 1,
        );
        assert_eq!(next.file_name().unwrap(), "a.iso.1.seg");
        assert_eq!(
            first.parent().unwrap().file_name().unwrap(),
            "7K3M-9Q2V-XB5N-4T8H-R6-CentOS-Stream-10"
        );
    }

    #[test]
    fn rename_preserves_file_extension_and_directory_names() {
        assert_eq!(
            numbered_name("report.pdf", false, 1),
            "report (1).pdf"
        );
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
                numbered_name(&format!("report.v2.{suffix}"), false, 2),
                format!("report.v2 (2).{suffix}")
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
        assert_eq!(
            numbered_name("report.v2", true, 1),
            "report.v2 (1)"
        );
    }
}
