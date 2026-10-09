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

use super::{model::TaskRecord, paths, write_queue::writable, TransferService};
use crate::error::{AppError, AppResult};
use crate::service::AppService;
use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};

/// 启动转写的触发线：缓存中已 sync 的字节达到该值即启动转写任务。
/// **这是触发线，不是上限**；小文件全部收齐时也立即触发。
pub(crate) const START_RELAY_BYTES: u64 = 256 * 1024 * 1024;

/// 缓存分段大小。必须为块大小的整数倍（256 KiB / 1 MiB / 4 MiB 均整除 64 MiB），
/// 这样每个块都完整落在某一段内，转写按段推进即可严格顺序。
pub(crate) const SEG_BYTES: u64 = 64 * 1024 * 1024;

/// 缓存盘可用空间低于该值时阻塞接收，等待转写释放空间；绝不报错、绝不丢数据。
pub(crate) const CACHE_FREE_FLOOR: u64 = 256 * 1024 * 1024;

/// 巡检间隔（触发转写任务、等待缺口）。
const TICK: Duration = Duration::from_millis(200);

/// 转写进度（已 sync 到目标盘）保存失败时的兜底设置键前缀提示，见 `pending_cleanup`。
const PENDING_CLEANUP_KEY: &str = "pending_cleanup";

/// 解析缓存盘中转目录：
/// 用户设置 `cache_directory` → `<软件安装目录>\Cache`（默认）→ `<数据目录>\Cache`（回退）。
/// 返回第一个「可创建」的目录；全部不可用时返回 `None`（此时所有任务自动直写）。
pub(crate) fn resolve_cache_root(app: &AppService) -> Option<PathBuf> {
    let mut candidates = Vec::new();
    if let Ok(Some(dir)) = app.get_setting("cache_directory") {
        if !dir.trim().is_empty() {
            candidates.push(PathBuf::from(dir));
        }
    }
    if let Some(install) = crate::config::install_dir() {
        candidates.push(install.join("Cache"));
    }
    candidates.push(app.paths.data_dir.join("Cache"));
    for candidate in candidates {
        match std::fs::create_dir_all(&candidate) {
            Ok(()) => return Some(candidate),
            Err(err) => tracing::warn!(dir = %candidate.display(), "缓存目录不可用：{err}"),
        }
    }
    None
}

/// 缓存盘可用空间不足时阻塞，等待转写任务腾出空间。
/// 用于接收路径：把「缓存盘写满」转化为对接收的**背压**，而不是错误。
pub(crate) async fn await_cache_space(
    service: &TransferService,
    task_id: &str,
) -> AppResult<()> {
    let Some(cache_root) = service.cache_root.as_ref() else {
        return Ok(());
    };
    loop {
        if !writable(&service.record(task_id)?.status) {
            return Ok(());
        }
        if fs2::available_space(cache_root).unwrap_or(u64::MAX) >= CACHE_FREE_FLOOR {
            return Ok(());
        }
        tokio::time::sleep(TICK).await;
    }
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
                        let Ok(task) = service.record(&id) else { continue };
                        if !candidate(&task) {
                            continue;
                        }
                        if service.relay_active.lock().unwrap().contains(&id) {
                            continue;
                        }
                        service.relay_active.lock().unwrap().insert(id.clone());
                        let worker = Arc::clone(&service);
                        let task_id = id.clone();
                        tokio::spawn(async move {
                            if let Err(err) = relay_task(&worker, &task_id).await {
                                tracing::error!(task = %task_id, "缓冲中转写失败：{err}");
                                fail_relay(&worker, &task_id, &err);
                            }
                            worker.relay_active.lock().unwrap().remove(&task_id);
                        });
                    }
                }
            }
        }
    });
}

/// 是否应启动/继续转写该任务。
fn candidate(task: &TaskRecord) -> bool {
    task.direction == "receive"
        && task.use_cache
        && task.error.is_none()
        && writable(&task.status)
        && trigger(task)
        && !relay_done(task)
}

/// 转写触发条件：缓存中已 sync 的字节 ≥ [`START_RELAY_BYTES`]，或全部块已收齐/跳过。
fn trigger(task: &TaskRecord) -> bool {
    let chunks = task.manifest.chunks();
    if task.completed.len() >= chunks.len() {
        return true;
    }
    let received: u64 = task
        .completed
        .iter()
        .filter_map(|i| chunks.get(*i))
        .filter(|c| !task.skipped_files.contains(&c.file))
        .map(|c| c.length as u64)
        .sum();
    received >= START_RELAY_BYTES
}

/// 转写是否已全部完成（直写模式或未选中的文件视为已完成）。
/// 完成后目标盘临时文件长度即等于清单大小，`commit` 才能 rename。
pub(crate) fn relay_done(task: &TaskRecord) -> bool {
    if !task.use_cache {
        return true;
    }
    task.manifest.entries.iter().enumerate().all(|(i, entry)| {
        entry.directory
            || task.targets.get(i).and_then(Option::as_ref).is_none()
            || entry.size == 0
            || task.relayed.get(i).copied().unwrap_or(0) >= entry.size
    })
}

/// 单个任务的转写循环：按文件、按偏移顺序把缓存数据搬到目标盘。
async fn relay_task(service: &Arc<TransferService>, id: &str) -> AppResult<()> {
    let Some(cache_root) = service.cache_root.clone() else {
        return Ok(());
    };
    loop {
        let task = service.record(id)?;
        if !writable(&task.status) || !task.use_cache {
            return Ok(());
        }
        if relay_done(&task) {
            return Ok(());
        }
        let bases = file_bases(&task);
        let mut progressed = false;
        // 一个文件尽量连续写完再换下一个，避免在机械盘上多文件交错写入造成寻道。
        // 只有该文件所需块尚未到达缓存（阻塞）时才换到下一个文件。
        for (file, entry) in task.manifest.entries.iter().enumerate() {
            if entry.directory || task.targets.get(file).and_then(Option::as_ref).is_none() {
                continue;
            }
            loop {
                let current = service.record(id)?;
                if !writable(&current.status) {
                    return Ok(());
                }
                let done = current.relayed.get(file).copied().unwrap_or(0);
                if done >= entry.size {
                    break;
                }
                if !relay_file_step(service, &current, &cache_root, &bases, file, entry.size, done)
                    .await?
                {
                    break; // 所需块未到，先处理下一个文件，下一轮再回来
                }
                progressed = true;
            }
        }
        let latest = service.record(id)?;
        if relay_done(&latest) || !writable(&latest.status) {
            return Ok(());
        }
        if !progressed {
            tokio::time::sleep(TICK).await;
        }
    }
}

/// 每个「文件条目」在 `manifest.chunks()` 中的起始全局块号。
fn file_bases(task: &TaskRecord) -> Vec<usize> {
    let mut bases = Vec::with_capacity(task.manifest.entries.len());
    let mut acc = 0usize;
    for entry in &task.manifest.entries {
        bases.push(acc);
        if !entry.directory {
            acc += entry.size.div_ceil(task.manifest.chunk_size) as usize;
        }
    }
    bases
}

/// 转写一个文件的一个分段：读缓存 → 顺序追加目标盘临时文件 → sync → 记录进度 → 删缓存。
/// 返回是否取得进展（`false` 表示所需块尚未在缓存中，等下一轮）。
async fn relay_file_step(
    service: &Arc<TransferService>,
    task: &TaskRecord,
    cache_root: &Path,
    bases: &[usize],
    file: usize,
    size: u64,
    done: u64,
) -> AppResult<bool> {
    let seg_end = ((done / SEG_BYTES + 1) * SEG_BYTES).min(size);
    let chunk_size = task.manifest.chunk_size;
    // 先确认本段的第一个块已在缓存中，避免无谓地打开/截断目标文件。
    let first = bases[file] + (done / chunk_size) as usize;
    if !task.completed.contains(&first) {
        return Ok(false);
    }
    let part = paths::stage(task)?.join(format!("{file}.part"));
    paths::ensure_plain(&part)?;
    if let Some(parent) = part.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut handle = tokio::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&part)
        .await?;
    // 崩溃后目标文件可能比已记录的进度更长（写入成功但进度未保存）：
    // 回退到已 sync 的进度点，保证「达到初始长度」与「内容正确」一致。
    if handle.metadata().await?.len() > done {
        handle.set_len(done).await?;
    }
    let mut offset = done;
    while offset < seg_end {
        let index = bases[file] + (offset / chunk_size) as usize;
        if !task.completed.contains(&index) {
            return Ok(false);
        }
        let len = chunk_size.min(size - offset) as usize;
        let seg_path = paths::cache_segment(cache_root, &task.id, file, offset / SEG_BYTES);
        let data = read_segment(&seg_path, offset % SEG_BYTES, len).await?;
        handle.seek(std::io::SeekFrom::Start(offset)).await?;
        handle.write_all(&data).await?;
        offset += len as u64;
    }
    handle.sync_data().await?;
    drop(handle);
    // sync 成功后记录转写进度：这是崩溃后唯一可信的回退点。
    service.edit(&task.id, |t| {
        if t.relayed.len() < t.manifest.entries.len() {
            t.relayed.resize(t.manifest.entries.len(), 0);
        }
        t.relayed[file] = offset;
    })?;
    // 释放已转写的缓存分段（Windows 上靠分段才能回收已写过的前缀）。
    for segment in fully_consumed(done, size)..fully_consumed(offset, size) {
        let _ = std::fs::remove_file(paths::cache_segment(cache_root, &task.id, file, segment));
    }
    Ok(true)
}

/// 已完全转写（可删除）的缓存分段数量。
fn fully_consumed(offset: u64, size: u64) -> u64 {
    let whole = offset / SEG_BYTES;
    if offset >= size && size % SEG_BYTES != 0 {
        whole + 1
    } else {
        whole
    }
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
    let Some(cache_root) = service.cache_root.clone() else {
        return;
    };
    retry_pending_cleanup(service);
    let protected: HashSet<String> = service
        .receive_ids()
        .into_iter()
        .filter(|id| {
            service
                .record(id)
                .map(|t| ["waiting", "accepted", "transferring", "paused"].contains(&t.status.as_str()))
                .unwrap_or(false)
        })
        .map(|id| paths::safe_component(&id))
        .collect();
    let Ok(entries) = std::fs::read_dir(&cache_root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()).map(str::to_string) else {
            continue;
        };
        if !path.is_dir() || protected.contains(&name) {
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