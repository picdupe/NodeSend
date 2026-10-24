//! 接收端「缓存盘中转」转写任务。
//!
//! 目标：让机械盘目标盘只做**连续顺序写**，把随机写、每块 `sync_data`、完成记录
//! 等全部留在缓存盘上，从而消除「接收与写盘交错」造成的顺序写打断。
//!
//! 流程：
//! ```text
//! 接收 → 写缓存分段（write_queue，sync 在缓存盘）
//!      → 缓存中已 sync 字节 ≥ 256 MiB（或全部收齐）→ 启动本模块的转写任务
//!      → 转写任务持续运行：按偏移顺序读缓存分段 → 连续追加目标盘临时文件
//!                          → 记录转写进度（sync 成功后才记录）→ 删除已转写的缓存分段
//!      → 全部转写完成 → 由 `receive::commit` 做 rename
//! ```
//!
//! 关键语义：
//! - 256 MiB 是「启动转写」的触发线，**不是缓存上限**；缓存无逻辑上限。
//! - 缓存盘写满时**阻塞接收**（见 `await_cache_space`），不报错、不丢数据。
//! - 转写启动后**持续运行**，与接收并行；目标盘写入严格按偏移顺序。
//! - 缓存分段文件是必需的：Windows 无法截断文件头部，只有分段才能回收已转写数据。

use super::{paths, write_queue::writable, RelayPlan, RelayState, TransferService};
use crate::error::{AppError, AppResult};
use crate::service::AppService;
use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt, BufReader, BufWriter};

/// 缓存分段大小。见 [`crate::tuning::RELAY_SEGMENT_BYTES`]。
pub(crate) const SEG_BYTES: u64 = crate::tuning::RELAY_SEGMENT_BYTES;

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
            // An explicitly empty setting means the user disabled caching.
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
                tracing::error!(dir = %candidate.display(), "拒绝将磁盘根目录作为 NodeSend 缓存目录");
                continue;
            }
            match std::fs::create_dir_all(&candidate) {
                Ok(()) => return Some(candidate),
                Err(err) => tracing::warn!(dir = %candidate.display(), "缓存目录不可用：{err}"),
            }
        }
        None
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
        return Some("安装目录异常：软件位于磁盘根目录或 Program Files 根目录。为保护其他文件，已禁用缓存清理；请重新安装到 NodeSend 子目录。".into());
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
                                tracing::error!(task = %task_id, "缓冲中转写失败：{err}");
                                fail_relay(&worker, &task_id, &err);
                            }
                            if let Some(runtime) = worker.runtime_if_exists(&task_id) {
                                runtime
                                    .relay_active
                                    .store(false, std::sync::atomic::Ordering::SeqCst);
                            }
                        });
                    }
                    // The sender may have disconnected after every chunk was
                    // acknowledged but before its final Commit arrived. Once
                    // the local relay is complete, finish the filesystem
                    // commit here instead of leaving .part files indefinitely.
                    for id in service.receive_ids() {
                        if service.auto_commit_ready(&id) {
                            if let Err(err) = service.commit(&id).await {
                                tracing::warn!(task = %id, "自动恢复接收任务提交失败：{err}");
                            }
                        }
                    }
                }
            }
        }
    });
}

/// 是否应启动/继续转写该任务（只用标量状态，O(1)）。
fn candidate(state: &RelayState) -> bool {
    state.use_cache
        && state.error.is_none()
        && writable(&state.status)
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
        if !writable(&state.status) || !state.use_cache {
            return Ok(());
        }
        // 接收端可能在传输过程中继续追加清单。不能只使用启动时的快照：
        // 否则转写器会永远看不到后续文件，缓存不断增长，暂停/继续重启转写后才可能推进。
        // 只有条目数量发生变化时才重新加载清单，避免每轮复制几十万条记录。
        if plan.entries.len() < state.file_count {
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
                if !writable(&service.status_of(id)?) {
                    return Ok(());
                }
                if done >= entry.size {
                    break;
                }
                match relay_file_step(service, &plan, &cache_root, id, file, entry.size, done)
                    .await?
                {
                    Some(next) => {
                        done = next;
                        progressed = true;
                    }
                    None => break, // 所需块未到，先处理下一个文件，下一轮再回来
                }
            }
        }
        let state = service.relay_state(id)?;
        if relay_done(&state) || !writable(&state.status) {
            return Ok(());
        }
        if !progressed {
            tokio::time::sleep(TICK).await;
        }
    }
}

/// 转写一个文件的一个分段：读缓存 → 顺序追加目标盘临时文件 → sync → 记录进度 → 删缓存。
/// 返回 `Some(新偏移)` 表示取得进展；`None` 表示所需块尚未在缓存中，等下一轮。
async fn relay_file_step(
    service: &Arc<TransferService>,
    plan: &RelayPlan,
    cache_root: &Path,
    id: &str,
    file: usize,
    size: u64,
    done: u64,
) -> AppResult<Option<u64>> {
    let destination = Path::new(
        plan.targets
            .get(file)
            .and_then(Option::as_deref)
            .ok_or_else(|| AppError::NotFound("缓存文件目标路径不存在".into()))?,
    );
    let root = service.record(id)?.destination.ok_or_else(|| AppError::NotFound("接收目录不存在".into()))?;
    paths::ensure_under(Path::new(&root), destination)?;
    if done >= size {
        let metadata = tokio::fs::metadata(destination).await?;
        if metadata.len() == size {
            service.set_relayed(id, file, size)?;
            let assembled = paths::cache_file(cache_root, id, file);
            let _ = std::fs::remove_file(assembled);
            for segment in 0..size.div_ceil(SEG_BYTES) {
                let _ = std::fs::remove_file(paths::cache_segment(cache_root, id, file, segment));
            }
            return Ok(Some(size));
        }
    }
    let seg_end = ((done / SEG_BYTES + 1) * SEG_BYTES).min(size);
    let chunk_size = plan.chunk_size;
    let base = plan.bases[file];
    // 先确认本段的第一个块已在缓存中，避免无谓地打开/截断目标文件。
    let first = base + (done / chunk_size) as usize;
    if !service.is_completed(id, first) {
        return Ok(None);
    }
    let assembled = paths::cache_file(cache_root, id, file);
    paths::ensure_plain(&assembled)?;
    if let Some(parent) = assembled.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut handle = tokio::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&assembled)
        .await?;
    // A pause/restart can leave the destination staging file from the older
    // relay implementation while the cache-side assembled file is absent.
    // Seed the cache file from that durable prefix so resume does not attempt
    // to read a deleted segment from offset zero.
    if done > 0 && handle.metadata().await?.len() < done {
        if let Ok(source) = tokio::fs::File::open(destination).await {
            let available = source.metadata().await?.len().min(done);
            if available > 0 {
                let mut seeded = tokio::fs::OpenOptions::new()
                    .create(true)
                    .write(true)
                    .open(&assembled)
                    .await?;
                seeded.set_len(0).await?;
                tokio::io::copy(&mut source.take(available), &mut seeded).await?;
                seeded.sync_data().await?;
            }
        }
    }
    // 崩溃后目标文件可能比已记录的进度更长（写入成功但进度未保存）：
    // 回退到已 sync 的进度点，保证「达到初始长度」与「内容正确」一致。
    if handle.metadata().await?.len() > done {
        handle.set_len(done).await?;
    }
    let mut offset = done;
    while offset < seg_end {
        let index = base + (offset / chunk_size) as usize;
        if !service.is_completed(id, index) {
            return Ok(None);
        }
        let len = chunk_size.min(size - offset) as usize;
        let seg_path = paths::cache_segment(cache_root, id, file, offset / SEG_BYTES);
        let data = match read_segment(&seg_path, offset % SEG_BYTES, len).await {
            Ok(data) => data,
            Err(error) if matches!(&error, AppError::Io(io) if io.kind() == std::io::ErrorKind::NotFound) => {
                // The final destination file is a durable fallback for files
                // interrupted during a relay upgrade or pause/resume cycle.
                let Ok(mut source) = tokio::fs::File::open(destination).await else {
                    tracing::debug!(task = %id, file, offset, path = %seg_path.display(), "缓存分段尚未可读，稍后重试");
                    return Ok(None);
                };
                source.seek(std::io::SeekFrom::Start(offset)).await?;
                let mut data = vec![0u8; len];
                tokio::io::AsyncReadExt::read_exact(&mut source, &mut data).await?;
                data
            }
            Err(error) => return Err(error),
        };
        handle.seek(std::io::SeekFrom::Start(offset)).await?;
        handle.write_all(&data).await?;
        offset += len as u64;
    }
    handle.sync_data().await?;
    drop(handle);
    if offset >= size {
        // Cache mode publishes directly to the final target. The cache-side
        // relay cursor is the recovery record; no destination `.part` or
        // per-chunk target marker is involved.
        paths::ensure_plain(destination)?;
        if let Some(parent) = destination.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let output = tokio::fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(destination)
            .await?;
        let input = tokio::fs::File::open(&assembled).await?;
        let mut input = BufReader::with_capacity(1024 * 1024, input);
        let mut output = BufWriter::with_capacity(1024 * 1024, output);
        tokio::io::copy(&mut input, &mut output).await?;
        output.flush().await?;
        drop(output);
        let _ = std::fs::remove_file(&assembled);
        for segment in 0..size.div_ceil(SEG_BYTES) {
            let _ = std::fs::remove_file(paths::cache_segment(cache_root, id, file, segment));
        }
    }
    // The progress marker is written only after the cache file and, for a
    // complete file, the destination staging copy are durable.
    service.set_relayed(id, file, offset)?;
    Ok(Some(offset))
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
        t.error = Some(err.to_string());
        t.event(format!("缓存中转写失败：{err}"));
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
        tracing::error!(dir = %cache_root.display(), "缓存清理已跳过：目录是磁盘根目录或 Program Files 根目录");
        return;
    }
    retry_pending_cleanup(service);
    let protected: HashSet<String> = service
        .receive_ids()
        .into_iter()
        .filter(|id| {
            service
                .status_of(id)
                .map(|status| {
                    ["waiting", "accepted", "transferring", "paused", "interrupted"].contains(&status.as_str())
                })
                .unwrap_or(false)
        })
        .map(|id| paths::safe_component(&id))
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
        if !path.is_dir() || protected.contains(&name) || !name.starts_with("in-") {
            continue;
        }
        if let Err(err) = std::fs::remove_dir_all(&path) {
            tracing::warn!(dir = %path.display(), "清理孤儿缓存失败：{err}");
            record_pending_cleanup(service, &path);
        }
    }
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
            tracing::error!(dir = %path.display(), "跳过危险的磁盘根目录清理记录");
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
