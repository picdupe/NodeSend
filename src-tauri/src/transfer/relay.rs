use super::{paths, recovery::processing, RelayPlan, RelayState, TransferService};
use crate::error::{AppError, AppResult};
use crate::service::AppService;
use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};

/// 缓存分段大小。见 [`crate::tuning::RELAY_SEGMENT_BYTES`]。
pub(crate) const SEG_BYTES: u64 = crate::tuning::RELAY_SEGMENT_BYTES;

#[cfg(test)]
pub(crate) fn cache_usage(root: &Path) -> AppResult<u64> {
    let mut pending = vec![root.to_path_buf()];
    let mut bytes = 0u64;
    while let Some(dir) = pending.pop() {
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let metadata = match std::fs::symlink_metadata(entry.path()) {
                Ok(metadata) => metadata,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error.into()),
            };
            paths::ensure_plain_meta(&metadata)?;
            if metadata.is_dir() {
                pending.push(entry.path());
            } else if metadata.is_file() {
                bytes = bytes.saturating_add(metadata.len());
            }
        }
    }
    Ok(bytes)
}

#[cfg(test)]
pub(crate) fn cache_write_fits(
    root: &Path,
    segment: &Path,
    end: u64,
    limit: u64,
) -> AppResult<bool> {
    let used = cache_usage(root)?;
    let existing = match std::fs::metadata(segment) {
        Ok(meta) => meta.len(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => 0,
        Err(error) => return Err(error.into()),
    };
    Ok(used.saturating_add(end.saturating_sub(existing)) <= limit)
}

/// 巡检间隔（触发转写任务、等待缺口）。
const TICK: Duration = crate::tuning::WRITE_TICK;

/// 转写进度（已 sync 到目标盘）保存失败时的兜底设置键前缀提示，见 `pending_cleanup`。
const PENDING_CLEANUP_KEY: &str = "pending_cleanup";

/// 解析缓存盘中转目录：
/// 用户设置 `cache_directory` → `D:\ProgramData\NodeSend\Cache` → `C:\ProgramData\NodeSend\Cache`。
/// 返回第一个「可创建」的目录；全部不可用时返回 `None`（此时所有任务自动直写）。
pub(crate) fn resolve_cache_root(app: &AppService) -> Option<PathBuf> {
    #[cfg(target_os = "android")]
    {
        let _ = app;
        return None;
    }
    #[cfg(not(target_os = "android"))]
    {
        let mut candidates = Vec::new();
        if let Ok(Some(dir)) = app.get_setting("cache_directory") {
            if dir.trim().is_empty() {
                return None;
            }
            candidates.push(PathBuf::from(dir));
        }
        #[cfg(windows)]
        {
            candidates.push(PathBuf::from(r"D:\ProgramData\NodeSend\Cache"));
            candidates.push(PathBuf::from(r"C:\ProgramData\NodeSend\Cache"));
        }
        #[cfg(not(windows))]
        {
            // 非 Windows 平台没有 D:/C: ProgramData 约定，保留平台数据目录。
            candidates.push(app.paths.data_dir.join("Cache"));
        }
        for candidate in candidates {
            if is_filesystem_root(&candidate) {
                tracing::error!(dir = %candidate.display(), "Refusing to use a drive root as the NodeSend cache directory");
                continue;
            }
            match std::fs::create_dir_all(&candidate) {
                Ok(()) if cache_writable(&candidate) => return Some(candidate),
                Ok(()) => {
                    tracing::warn!(dir = %candidate.display(), "Cache directory is not writable")
                }
                Err(err) => tracing::warn!(dir = %candidate.display(), "Cache directory is unavailable: {err}"),
            }
        }
        None
    }
}

/// 缓存目录存在不等于可写（ProgramData 常见为管理员独占）。真正建一个探针文件，
/// 否则接收任务会先选缓存中转、再在写分段时因「拒绝访问」把任务暂停。
#[cfg(not(target_os = "android"))]
fn cache_writable(dir: &std::path::Path) -> bool {
    let probe = dir.join(".nodesend-write-probe");
    match std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&probe)
    {
        Ok(_) => {
            let _ = std::fs::remove_file(&probe);
            true
        }
        Err(_) => false,
    }
}

/// 仅解析设置页要展示的缓存目录，不创建任何目录。
/// 用户未指定时给出平台默认候选（Windows 优先 D 盘，不存在则 C 盘）。
pub(crate) fn configured_cache_root(app: &AppService) -> Option<PathBuf> {
    #[cfg(target_os = "android")]
    {
        let _ = app;
        return None;
    }
    #[cfg(not(target_os = "android"))]
    {
        if let Ok(Some(dir)) = app.get_setting("cache_directory") {
            let dir = dir.trim();
            if dir.is_empty() {
                return None;
            }
            return Some(PathBuf::from(dir));
        }
        #[cfg(windows)]
        {
            let d = PathBuf::from(r"D:\ProgramData\NodeSend\Cache");
            if Path::new("D:\\").exists() {
                return Some(d);
            }
            return Some(PathBuf::from(r"C:\ProgramData\NodeSend\Cache"));
        }
        #[cfg(not(windows))]
        {
            Some(app.paths.data_dir.join("Cache"))
        }
    }
}

fn is_filesystem_root(path: &Path) -> bool {
    let normalized = path.to_string_lossy().replace('/', "\\");
    normalized.len() == 3
        && normalized.as_bytes().get(1) == Some(&b':')
        && normalized.ends_with('\\')
}

pub(crate) fn is_protected_cleanup_root(path: &Path) -> bool {
    if is_filesystem_root(path) {
        return true;
    }
    #[cfg(windows)]
    if let Ok(program_files) = std::env::var("ProgramFiles") {
        let normalize = |value: &Path| {
            value
                .to_string_lossy()
                .trim_end_matches(['\\', '/'])
                .to_ascii_lowercase()
        };
        return normalize(path) == normalize(Path::new(&program_files));
    }
    false
}

pub(crate) fn installation_warning() -> Option<String> {
    let executable = std::env::current_exe().ok()?;
    let install_dir = executable.parent()?;
    if is_protected_cleanup_root(install_dir) {
        return Some("Invalid installation directory: the app is installed at a drive root or the Program Files root. Cache cleanup was disabled to protect other files. Reinstall NodeSend in its own subdirectory.".into());
    }
    None
}

/// 启动转写调度器。随 `TransferService::shutdown` 一起结束。
/// 调度器只做「发现需要转写的任务并拉起转写任务」，转写本身在独立任务中并行进行。
pub(crate) fn spawn(service: Arc<TransferService>) {
    tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = service.shutdown.cancelled() => break,
                _ = tokio::time::sleep(TICK) => {
                    for id in service.receive_ids() {
                        let Ok(state) = service.relay_state(&id) else { continue };
                        if !candidate(&state) {
                            continue;
                        }
                        let runtime = service.runtime(&id);
                        if runtime.relay_active.load(std::sync::atomic::Ordering::SeqCst) {
                            continue;
                        }
                        runtime
                            .relay_active
                            .store(true, std::sync::atomic::Ordering::SeqCst);
                        let worker = Arc::clone(&service);
                        let task_id = id.clone();
                        tokio::spawn(async move {
                            if let Err(err) = relay_task(&worker, &task_id).await {
                                tracing::error!(task = %task_id, "Buffered relay write failed: {err}");
                                fail_relay(&worker, &task_id, &err);
                            }
                            if let Some(runtime) = worker.runtime_if_exists(&task_id) {
                                runtime
                                    .relay_active
                                    .store(false, std::sync::atomic::Ordering::SeqCst);
                            }
                        });
                    }
                }
            }
        }
    });
}

/// 是否应启动/继续转写该任务（只用标量状态，O(1)）。
fn candidate(state: &RelayState) -> bool {
    state.use_cache
        && state.status != "finishing"
        && processing(&state.status)
        && trigger(state)
        && !relay_done(state)
}

/// 转写触发条件：缓存中已 sync 的字节 ≥ [`START_RELAY_BYTES`]，或全部块已收齐/跳过。
fn trigger(state: &RelayState) -> bool {
    state.completed_count >= state.chunk_count || state.completed_bytes > 0
}

/// 转写是否已全部完成（直写模式或未选中的文件视为已完成）。
/// 完成后目标盘临时文件长度即等于清单大小，`commit` 才能 rename。
pub(crate) fn relay_done(state: &RelayState) -> bool {
    !state.use_cache || state.relayed_total >= state.relay_target_bytes
}

/// 单个任务的转写循环：按文件、按偏移顺序把缓存数据搬到目标盘。
///
/// 明细只在启动时取一次（`RelayPlan`）；进度用 `relayed_at` 按文件读取，
/// 状态用 `status_of` 读取，因此每轮巡检都不再克隆整份清单（百万级清单仍 O(1)）。
async fn relay_task(service: &Arc<TransferService>, id: &str) -> AppResult<()> {
    let Some(cache_root) = service.cache_root_for_task(id) else {
        return Ok(());
    };
    let mut plan = service.relay_plan(id)?;
    if !plan.use_cache {
        return Ok(());
    }
    loop {
        let state = service.relay_state(id)?;
        if state.status == "finishing" || !processing(&state.status) || !state.use_cache {
            return Ok(());
        }
        // 接收端可能在传输过程中继续追加清单。不能只使用启动时的快照：
        // 否则转写器会永远看不到后续文件，缓存不断增长，暂停/继续重启转写后才可能推进。
        // 只有条目数量发生变化时才重新加载清单，避免每轮复制几十万条记录。
        if plan.entries.len() < state.file_count || plan.digest != state.digest {
            plan = service.relay_plan(id)?;
        }
        if relay_done(&state) {
            return Ok(());
        }
        let mut progressed = false;
        // 一个文件尽量连续写完再换下一个，避免在机械盘上多文件交错写入造成寻道。
        // 只有该文件所需块尚未到达缓存（阻塞）时才换到下一个文件。
        for (file, entry) in plan.entries.iter().enumerate() {
            if !plan.cache_files.contains(&file)
                || entry.directory
                || plan.targets.get(file).and_then(Option::as_ref).is_none()
            {
                continue;
            }
            let mut done = service.relayed_at(id, file);
            loop {
                if !processing(&service.status_of(id)?) {
                    return Ok(());
                }
                if done >= entry.size {
                    break;
                }
                let result =
                    relay_file_step(service, &plan, &cache_root, id, file, entry.size, done).await;
                let step = match result {
                    Ok(step) => step,
                    Err(error)
                        if matches!(&error, AppError::Io(e) if [std::io::ErrorKind::NotFound,std::io::ErrorKind::UnexpectedEof].contains(&e.kind()))
                            || matches!(&error, AppError::Conflict(_)) =>
                    {
                        let gate = service.gate(id);
                        let _guard = gate.lock().await;
                        if service.record(id)?.manifest.entries.get(file) == plan.entries.get(file)
                        {
                            service.invalidate_file(id, file)?;
                        }
                        break;
                    }
                    Err(error) => return Err(error),
                };
                match step {
                    Some(next) => {
                        done = next;
                        progressed = true;
                    }
                    None => break,
                }
            }
        }
        let state = service.relay_state(id)?;
        if relay_done(&state) || !processing(&state.status) {
            return Ok(());
        }
        if !progressed {
            tokio::time::sleep(TICK).await;
        }
    }
}

async fn relay_file_step(
    service: &TransferService,
    plan: &RelayPlan,
    cache_root: &Path,
    id: &str,
    file: usize,
    size: u64,
    done: u64,
) -> AppResult<Option<u64>> {
    if done >= size {
        return Ok(None);
    }
    if size > crate::tuning::CACHE_ASSEMBLE_BYTES {
        return Err(AppError::Conflict(
            "File exceeds the cache assembly size limit".into(),
        ));
    }
    let base = plan.bases[file];
    let count = size.div_ceil(plan.chunk_size) as usize;
    if !(base..base + count).all(|index| service.is_completed(id, index)) {
        return Ok(None);
    }
    let _writer = service.destination_writer.lock().await;
    let task = service.record(id)?;
    if task.manifest.entries.get(file) != plan.entries.get(file) {
        return Ok(None);
    }
    let root = Path::new(
        task.destination
            .as_deref()
            .ok_or_else(|| AppError::NotFound("Receive destination is not set".into()))?,
    );
    let target = Path::new(
        plan.targets[file]
            .as_ref()
            .ok_or_else(|| AppError::NotFound("Receive target is not set".into()))?,
    );
    paths::ensure_under(root, target)?;
    let assembled = paths::cache_file(cache_root, &task, file);
    std::fs::create_dir_all(assembled.parent().unwrap())?;
    let state = service.journal_state(id, file)?;
    if state.as_deref() == Some("published") {
        if !std::fs::metadata(target).is_ok_and(|m| m.is_file() && m.len() == size) {
            return Err(AppError::Conflict(
                "Published file is missing or its size does not match the manifest".into(),
            ));
        }
        service.set_relayed(id, file, size)?;
        if assembled.exists() {
            std::fs::remove_file(assembled)?;
        }
        return Ok(Some(size));
    }
    if !matches!(state.as_deref(), Some("assembled" | "publishing")) {
        if !service.reserve_cached_file(id, file, size, crate::tuning::CACHE_RECEIVE_BYTES)? {
            return Ok(None);
        }
        let mut output = tokio::fs::File::create(&assembled).await?;
        use sha2::{Digest, Sha256};
        let mut assembled_hash = task.verify_hash.then(Sha256::new);
        let mut offset = 0;
        while offset < size {
            if !processing(&service.status_of(id)?) {
                return Ok(None);
            }
            let len = plan
                .chunk_size
                .min(size - offset)
                .min(SEG_BYTES - offset % SEG_BYTES) as usize;
            let segment = paths::cache_segment(cache_root, &task, file, offset / SEG_BYTES);
            let data = read_segment(&segment, offset % SEG_BYTES, len).await?;
            if task.verify_hash {
                use rusqlite::OptionalExtension;
                use sha2::{Digest, Sha256};
                let index = base + (offset / plan.chunk_size) as usize;
                let expected: Option<String> = service.app.db.with_conn(|db| Ok(db.query_row("SELECT digest FROM transfer_chunk_hash WHERE task_id=?1 AND chunk_index=?2",rusqlite::params![id,index as i64],|r| r.get(0)).optional()?))?;
                if expected.as_deref() != Some(format!("{:x}", Sha256::digest(&data)).as_str()) {
                    return Err(AppError::Conflict(
                        "Cached chunk SHA-256 verification failed".into(),
                    ));
                }
            }
            if let Some(hash) = &mut assembled_hash {
                hash.update(&data);
            }
            output.write_all(&data).await?;
            service
                .runtime(id)
                .relay_progress
                .fetch_add(len as u64, std::sync::atomic::Ordering::Relaxed);
            offset += len as u64;
        }
        output.sync_all().await?;
        if output.metadata().await?.len() != size {
            return Err(AppError::Conflict("Assembled cache file size does not match the manifest".into()));
        }
        drop(output);
        if let Some(hash) = assembled_hash {
            let digest = format!("{:x}", hash.finalize());
            service.app.db.with_conn(|db| { db.execute("INSERT OR REPLACE INTO transfer_cache_digest(task_id,file_index,digest) VALUES(?1,?2,?3)",rusqlite::params![id,file as i64,digest])?; Ok(()) })?;
        }
        service.journal_transition(id, file, size, "assembled", Some(&target.to_string_lossy()))?;
    }
    if !tokio::fs::metadata(&assembled)
        .await
        .is_ok_and(|m| m.is_file() && m.len() == size)
    {
        return Err(AppError::Conflict(
            "Assembled cache file is missing or its size does not match the manifest".into(),
        ));
    }
    if task.verify_hash {
        use rusqlite::OptionalExtension;
        let expected: Option<String> = service.app.db.with_conn(|db| {
            Ok(db
                .query_row(
                    "SELECT digest FROM transfer_cache_digest WHERE task_id=?1 AND file_index=?2",
                    rusqlite::params![id, file as i64],
                    |r| r.get(0),
                )
                .optional()?)
        })?;
        let path = assembled.clone();
        let actual = tokio::task::spawn_blocking(move || super::recovery::hash_file(&path))
            .await
            .map_err(|e| AppError::Other(e.to_string()))??;
        if expected.as_ref() != Some(&actual) {
            return Err(AppError::Conflict(
                "Assembled cache file SHA-256 verification failed".into(),
            ));
        }
    }
    for segment in 0..size.div_ceil(SEG_BYTES) {
        let path = paths::cache_segment(cache_root, &task, file, segment);
        match tokio::fs::remove_file(path).await {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
    }
    service.journal_transition(
        id,
        file,
        size,
        "publishing",
        Some(&target.to_string_lossy()),
    )?;
    if let Some(parent) = target.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    paths::ensure_plain(target)?;
    let mut input = tokio::fs::File::open(&assembled).await?;
    let mut output = tokio::fs::File::create(target).await?;
    let mut copied = 0u64;
    let mut buffer = vec![0u8; 1024 * 1024];
    loop {
        if !processing(&service.status_of(id)?) {
            return Ok(None);
        }
        let len = input.read(&mut buffer).await?;
        if len == 0 {
            break;
        }
        output.write_all(&buffer[..len]).await?;
        copied += len as u64;
        service
            .runtime(id)
            .relay_progress
            .fetch_add(len as u64, std::sync::atomic::Ordering::Relaxed);
    }
    output.sync_all().await?;
    drop(output);
    if copied != size
        || !tokio::fs::metadata(target)
            .await
            .is_ok_and(|m| m.is_file() && m.len() == size)
    {
        return Err(AppError::Conflict(
            "Published file size does not match the manifest".into(),
        ));
    }
    service.journal_transition(id, file, size, "published", Some(&target.to_string_lossy()))?;
    service.set_relayed(id, file, size)?;
    drop(input);
    tokio::fs::remove_file(&assembled).await?;
    let journal =
        paths::cache_journal_named(cache_root, id, file, paths::cache_name_of(&task, file));
    if journal.exists() {
        tokio::fs::remove_file(journal).await?;
    }
    Ok(Some(size))
}

pub(crate) async fn relay_file_step_shared(
    service: &TransferService,
    plan: &RelayPlan,
    cache: &Path,
    id: &str,
    file: usize,
    size: u64,
    done: u64,
) -> AppResult<Option<u64>> {
    relay_file_step(service, plan, cache, id, file, size, done).await
}

/// 从缓存分段文件的指定偏移读取一块数据。
async fn read_segment(path: &Path, offset: u64, len: usize) -> AppResult<Vec<u8>> {
    let mut file = tokio::fs::File::open(path).await?;
    file.seek(std::io::SeekFrom::Start(offset)).await?;
    let mut buf = vec![0u8; len];
    file.read_exact(&mut buf).await?;
    Ok(buf)
}

/// 转写失败：记录错误（阻止调度器反复重试），`commit` 会据此尽早失败而非永久等待。
fn fail_relay(service: &Arc<TransferService>, task_id: &str, err: &AppError) {
    let _ = service.edit(task_id, |t| {
        t.status = "failed".into();
        t.error = Some(err.to_string());
        t.event(format!("Cache relay write failed: {err}"));
    });
}

/// 启动清理：
/// 1. 重试上次删除失败的「待清理」项；
/// 2. 扫描缓存根目录，删除没有对应活动任务的孤儿缓存目录（进程崩溃遗留）。
pub(crate) fn sweep(service: &Arc<TransferService>) {
    let Some(cache_root) = service.current_cache_root() else {
        return;
    };
    if is_protected_cleanup_root(&cache_root) {
        tracing::error!(dir = %cache_root.display(), "Skipped cache cleanup because the directory is a drive root or Program Files root");
        return;
    }
    retry_pending_cleanup(service);
    let protected: HashSet<String> = service
        .receive_ids()
        .into_iter()
        .filter_map(|id| {
            let status = service.status_of(&id).ok()?;
            if [
                "completed",
                "cancelled",
                "rejected",
                "partial_completed",
                "incompatible",
            ]
            .contains(&status.as_str())
            {
                return None;
            }
            let task = service.record(&id).ok()?;
            Some(
                paths::cache_task_dirs(&cache_root, &task)
                    .into_iter()
                    .filter_map(|path| path.file_name()?.to_str().map(str::to_string))
                    .collect::<Vec<_>>(),
            )
        })
        .flatten()
        .collect();
    let Ok(entries) = std::fs::read_dir(&cache_root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path
            .file_name()
            .and_then(|n| n.to_str())
            .map(str::to_string)
        else {
            continue;
        };
        if !path.is_dir()
            || protected.contains(&name)
            || !(name.starts_with("in-") || is_node_cache_dir(&name))
        {
            continue;
        }
        if let Err(err) = std::fs::remove_dir_all(&path) {
            tracing::warn!(dir = %path.display(), "Failed to clean orphaned cache: {err}");
            record_pending_cleanup(service, &path);
        }
    }
}

fn is_node_cache_dir(name: &str) -> bool {
    let parts = name.splitn(6, '-').collect::<Vec<_>>();
    if parts.len() != 6 || parts[5].is_empty() {
        return false;
    }
    let id = parts[..5].join("-");
    crate::identity::node_identity::normalize_node_id(&id).is_some()
}

/// 删除失败时登记「待清理」，下次启动重试。
pub(crate) fn record_pending_cleanup(service: &TransferService, path: &Path) {
    let mut list = pending_cleanup(service);
    let value = path.to_string_lossy().into_owned();
    if !list.contains(&value) {
        list.push(value);
    }
    if let Ok(json) = serde_json::to_string(&list) {
        let _ = service.app.set_setting(PENDING_CLEANUP_KEY, &json);
    }
}

fn pending_cleanup(service: &TransferService) -> Vec<String> {
    service
        .app
        .get_setting(PENDING_CLEANUP_KEY)
        .ok()
        .flatten()
        .and_then(|raw| serde_json::from_str::<Vec<String>>(&raw).ok())
        .unwrap_or_default()
}

fn retry_pending_cleanup(service: &TransferService) {
    let list = pending_cleanup(service);
    if list.is_empty() {
        return;
    }
    let mut remaining = Vec::new();
    for value in list {
        let path = PathBuf::from(&value);
        if is_protected_cleanup_root(&path) {
            tracing::error!(dir = %path.display(), "Skipped unsafe drive-root cleanup record");
            continue;
        }
        let active = service.receive_ids().into_iter().any(|id| {
            path.file_name().and_then(|n| n.to_str()) == Some(paths::safe_component(&id).as_str())
                && service.status_of(&id).is_ok_and(|s| {
                    ![
                        "completed",
                        "cancelled",
                        "rejected",
                        "partial_completed",
                        "incompatible",
                    ]
                    .contains(&s.as_str())
                })
        });
        if active {
            remaining.push(value);
            continue;
        }
        if !path.exists() {
            continue;
        }
        if std::fs::remove_dir_all(&path).is_err() {
            remaining.push(value);
        }
    }
    if let Ok(json) = serde_json::to_string(&remaining) {
        let _ = service.app.set_setting(PENDING_CLEANUP_KEY, &json);
    }
}

#[cfg(test)]
mod safety_tests {
    use super::*;
    use crate::{config::AppPaths, transfer::model::random_id};

    #[tokio::test]
    async fn relay_publishes_durable_bytes_and_rejects_missing_cache() {
        let root = std::env::temp_dir().join(format!("relay-{}", random_id()));
        std::fs::create_dir_all(&root).unwrap();
        let app = AppService::init(AppPaths::new(root.join("data"))).unwrap();
        app.set_setting("cache_directory", "").unwrap();
        let service = TransferService::new(app).unwrap();
        let source = root.join("payload.bin");
        let bytes = vec![73u8; 4 * 1024 * 1024 + 3];
        std::fs::write(&source, &bytes).unwrap();
        let id = service
            .create_outgoing(
                "peer".into(),
                "peer".into(),
                "test".into(),
                vec![source.to_string_lossy().into()],
                false,
                false,
                None,
            )
            .await
            .unwrap();
        let inbox = root.join("inbox");
        std::fs::create_dir_all(&inbox).unwrap();
        let inbox = inbox.canonicalize().unwrap();
        let target = inbox.join("payload.bin");
        service
            .edit(&id, |t| {
                t.direction = "receive".into();
                t.status = "paused".into();
                t.destination = Some(inbox.to_string_lossy().into());
                t.targets = vec![Some(target.to_string_lossy().into())];
                t.use_cache = true;
                t.cache_files.insert(0);
                t.relay_target_bytes = bytes.len() as u64;
            })
            .unwrap();
        service.save_decision(&id).unwrap();
        let task = service.record(&id).unwrap();
        for (index, chunk) in task.manifest.chunks().iter().enumerate() {
            service
                .mark_chunk_done(&id, index, chunk.length as u64)
                .unwrap();
        }
        let cache = root.join("cache");
        let segment = paths::cache_segment(&cache, &task, 0, 0);
        std::fs::create_dir_all(segment.parent().unwrap()).unwrap();
        let plan = service.relay_plan(&id).unwrap();
        assert!(
            relay_file_step(&service, &plan, &cache, &id, 0, bytes.len() as u64, 0)
                .await
                .is_err()
        );
        assert!(!target.exists());
        assert_eq!(service.relayed_at(&id, 0), 0);
        std::fs::write(&segment, &bytes).unwrap();
        assert_eq!(
            relay_file_step(&service, &plan, &cache, &id, 0, bytes.len() as u64, 0)
                .await
                .unwrap(),
            Some(bytes.len() as u64)
        );
        assert_eq!(std::fs::read(&target).unwrap(), bytes);
        assert!(!segment.exists());
        assert!(!paths::cache_file(&cache, &task, 0).exists());
        std::fs::remove_file(&target).unwrap();
        let current = service.record(&id).unwrap();
        assert!(TransferService::commit_staged_file(
            &current,
            &inbox,
            &paths::stage(&current).unwrap(),
            0
        )
        .is_err());
        service.shutdown.cancel();
        drop(service);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn usage_counts_all_tasks_and_legacy_assembled_files() {
        let root = std::env::temp_dir().join(format!("relay-budget-{}", random_id()));
        std::fs::create_dir_all(root.join("in-a")).unwrap();
        std::fs::create_dir_all(root.join("in-b")).unwrap();
        std::fs::write(root.join("in-a/0.0.seg"), [1u8; 17]).unwrap();
        std::fs::write(root.join("in-b/0.file"), [2u8; 31]).unwrap();
        assert_eq!(cache_usage(&root).unwrap(), 48);
        let segment = root.join("in-a/0.0.seg");
        assert!(cache_write_fits(&root, &segment, 17, 48).unwrap());
        assert!(!cache_write_fits(&root, &segment, 18, 48).unwrap());
        assert!(!cache_write_fits(&root, &root.join("in-b/1.0.seg"), 1, 48).unwrap());
        assert_eq!(crate::tuning::CACHE_MAX_BYTES, 32 * 1024 * 1024 * 1024);
        std::fs::remove_dir_all(root).unwrap();
    }
}
