//! 接收端写入队列与后台写入任务。
//!
//! 目的：把「接收网络块」与「写盘」解耦，让机械盘获得连续、按偏移顺序的写入流，
//! 且不把磁盘延迟带进 ACK 往返路径。
//!
//! 两种落盘模式（由 `TaskRecord::use_cache` 决定）：
//! - 直写：目标盘与缓存盘同卷时回退，块直接写 `<目标>/.nodesend-partials/<task>/<file>.part`；
//! - 缓存中转：写 `<缓存盘>/<task>/<file>.<seg>.seg`（随机写与 `sync_data` 全在缓存盘），
//!   目标盘只由 [`super::relay`] 做连续顺序写。
//!
//! 结构：
//! - `PendingChunk`：入队元素（任务 id、文件索引、块序号、块偏移、数据）。
//! - `QUEUE_CHUNKS`：有界队列容量；满时接收路径 `send().await` 背压等待。
//! - `Writer`：常驻后台任务，按文件聚合、按偏移排序后严格顺序写盘。
//!
//! 中断恢复安全（核心不变式）：
//! - `task.completed`（完成状态）只在数据 `sync_data()` 成功之后记录；
//! - 队列中尚未写入的块、已写入但未 sync 的块，都不会被记录为完成；
//! - 暂停 / 取消 / 进程退出时，未落盘块被丢弃且不记录完成，续传时自动重传。
//! 因此不会出现「完成状态说完成了，但数据没落盘」。

use super::{model::TaskRecord, paths, relay, TransferService};
use crate::error::{AppError, AppResult};
use std::{
    collections::{BTreeMap, HashMap},
    path::Path,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncSeekExt, AsyncWriteExt},
    sync::{mpsc, Mutex as AsyncMutex},
};

/// 有界写入队列容量（单位：块）。16 × 4 MiB ≈ 64 MiB。
pub(crate) const QUEUE_CHUNKS: usize = 16;

/// 单文件缓冲上限（单位：块）。达到即强制按序写出，避免缺口期间无限占用内存。
const FORCE_DRAIN_CHUNKS: usize = 16;

/// 缺口等待上限：超过该时长仍未补齐，则降级为乱序写。
/// 顺序写只是性能优化——每块自带 offset，乱序写依然正确。
const GAP_TIMEOUT: Duration = Duration::from_secs(2);

/// 后台任务巡检间隔：触发缺口超时降级与失效缓冲清理。
const TICK: Duration = Duration::from_millis(200);

/// 缓存盘写满后的重试间隔（不报错、不丢数据，等待转写任务释放空间）。
const CACHE_POLL: Duration = Duration::from_millis(200);

/// 队列中的单个待写入块。
pub(crate) struct PendingChunk {
    pub task_id: String,
    pub file: usize,
    pub index: usize,
    pub offset: u64,
    pub data: Vec<u8>,
}

/// 单个文件的待写缓冲。
struct FileBuffer {
    /// 已到达但尚未写入的块，按偏移排序（同时天然完成按偏移去重）。
    ready: BTreeMap<u64, PendingChunk>,
    /// 期望的下一个连续写入偏移，用于识别乱序缺口。
    next_offset: u64,
    /// 缺口首次出现的时间；补齐即清空。
    gap_since: Option<Instant>,
    /// 直写模式复用的目标文件句柄（与 `TransferService::receive_files` 共享）；
    /// 缓存模式为 `None`（缓存分段按块开、写、关，不留长期句柄）。
    handle: Option<Arc<AsyncMutex<tokio::fs::File>>>,
    /// 直写模式预分配的完整文件大小。
    expected_size: u64,
    /// 是否缓存中转模式。
    use_cache: bool,
}

/// 任务是否处于可写入状态。
pub(crate) fn writable(status: &str) -> bool {
    ["accepted", "transferring"].contains(&status)
}

/// 取已登记的共享文件句柄（复用 `TransferService::receive_files`，避免重复 open）。
fn cached_handle(
    service: &Arc<TransferService>,
    key: &(String, usize),
) -> Option<Arc<AsyncMutex<tokio::fs::File>>> {
    service.receive_files.lock().unwrap().get(key).cloned()
}

/// 后台写入器：独占持有全部文件的待写缓冲，因此同文件写入天然不会交叉。
#[derive(Default)]
struct Writer {
    files: HashMap<(String, usize), FileBuffer>,
}

/// 启动后台写入任务。随 `TransferService::shutdown` 一起结束。
pub(crate) fn spawn(service: Arc<TransferService>, mut rx: mpsc::Receiver<PendingChunk>) {
    tokio::spawn(async move {
        let mut writer = Writer::default();
        loop {
            tokio::select! {
                _ = service.shutdown.cancelled() => break,
                item = rx.recv() => {
                    let Some(chunk) = item else { break };
                    writer.accept(&service, chunk).await;
                }
                _ = tokio::time::sleep(TICK) => writer.tick(&service).await,
            }
        }
        // 进程级关闭：丢弃全部未落盘块且不记录完成，续传时由发送端重传。
        service.write_pending.lock().unwrap().clear();
    });
}

impl Writer {
    /// 接收一个块：任务不可写时丢弃，否则入缓冲并尽量顺序写出。
    async fn accept(&mut self, service: &Arc<TransferService>, chunk: PendingChunk) {
        let task_id = chunk.task_id.clone();
        if let Err(err) = self.store(service, chunk).await {
            tracing::error!(task = %task_id, "接收写入失败: {err}");
            self.files.retain(|(t, _), _| t != &task_id);
            fail_task(service, &task_id, &err);
        }
    }

    async fn store(&mut self, service: &Arc<TransferService>, chunk: PendingChunk) -> AppResult<()> {
        let task = service.record(&chunk.task_id)?;
        // 暂停 / 取消 / 已跳过该文件：直接丢弃，不记录完成，续传时重传。
        if !writable(&task.status) || task.skipped_files.contains(&chunk.file) {
            discard_one(service, &chunk.task_id, chunk.index);
            return Ok(());
        }
        let key = (chunk.task_id.clone(), chunk.file);
        if !self.files.contains_key(&key) {
            let buffer = open_buffer(service, &task, chunk.file).await?;
            self.files.insert(key.clone(), buffer);
        }
        self.files
            .get_mut(&key)
            .expect("缓冲刚插入")
            .ready
            .insert(chunk.offset, chunk);
        self.drain(service, &key).await
    }

    /// 尽量按偏移顺序写出缓冲中已连续的块；遇到缺口则等待（超时后降级乱序写）。
    async fn drain(
        &mut self,
        service: &Arc<TransferService>,
        key: &(String, usize),
    ) -> AppResult<()> {
        loop {
            let Some(buffer) = self.files.get_mut(key) else {
                return Ok(());
            };
            let Some((&offset, _)) = buffer.ready.iter().next() else {
                buffer.gap_since = None;
                return Ok(());
            };
            let gap_expired = buffer
                .gap_since
                .map(|at| at.elapsed() >= GAP_TIMEOUT)
                .unwrap_or(false);
            // 缺口未补齐时本文件暂停写出，但这些块不会被记录完成；
            // 缓冲达到上限则强制写出，保证内存有界（顺序仍尽量保持）。
            let congested = buffer.ready.len() >= FORCE_DRAIN_CHUNKS;
            if offset > buffer.next_offset && !gap_expired && !congested {
                if buffer.gap_since.is_none() {
                    buffer.gap_since = Some(Instant::now());
                }
                return Ok(());
            }
            let chunk = buffer.ready.remove(&offset).expect("刚取过最小偏移");
            buffer.gap_since = None;
            let end = offset + chunk.data.len() as u64;
            let use_cache = buffer.use_cache;
            let handle = buffer.handle.clone();
            let expected_size = buffer.expected_size;
            if use_cache {
                // 缓存模式：写缓存盘分段并 sync，目标盘不参与传输过程。
                if !store_cached(service, &chunk).await? {
                    continue; // 任务已不可写，块已丢弃
                }
            } else {
                let mut file = handle.as_ref().expect("直写模式必有目标句柄").lock().await;
                if file.metadata().await?.len() != expected_size {
                    file.set_len(expected_size).await?;
                }
                file.seek(std::io::SeekFrom::Start(offset)).await?;
                file.write_all(&chunk.data).await?;
                file.sync_data().await?;
            }
            if buffer.next_offset < end {
                buffer.next_offset = end;
            }
            // 落盘（缓存盘 sync）成功后才记录完成 —— 中断恢复安全的唯一依据。
            service.edit(&chunk.task_id, |t| {
                t.completed.insert(chunk.index);
                if t.status == "accepted" {
                    t.status = "transferring".into();
                }
            })?;
            discard_one(service, &chunk.task_id, chunk.index);
        }
    }

    /// 巡检：任务不可写则丢弃其未落盘块；否则重试排空（触发缺口超时降级）。
    async fn tick(&mut self, service: &Arc<TransferService>) {
        for key in self.files.keys().cloned().collect::<Vec<_>>() {
            let writable_now = service
                .record(&key.0)
                .map(|task| writable(&task.status))
                .unwrap_or(false);
            if !writable_now {
                if let Some(buffer) = self.files.remove(&key) {
                    for chunk in buffer.ready.values() {
                        discard_one(service, &chunk.task_id, chunk.index);
                    }
                }
                continue;
            }
            if let Err(err) = self.drain(service, &key).await {
                tracing::error!(task = %key.0, "接收写入失败: {err}");
                self.files.remove(&key);
                fail_task(service, &key.0, &err);
            }
        }
    }
}

/// 写一个块到缓存盘分段文件，并在成功后 `sync_data`。
/// 缓存盘写满（ENOSPC）时不报错：阻塞等待转写任务释放空间后重试。
async fn store_cached(service: &Arc<TransferService>, chunk: &PendingChunk) -> AppResult<bool> {
    let Some(cache_root) = service.cache_root.as_ref() else {
        return Err(AppError::Other("缓存目录不可用".into()));
    };
    let dir = paths::cache_task(cache_root, &chunk.task_id);
    std::fs::create_dir_all(&dir)?;
    let path = paths::cache_segment(
        cache_root,
        &chunk.task_id,
        chunk.file,
        chunk.offset / relay::SEG_BYTES,
    );
    let within = chunk.offset % relay::SEG_BYTES;
    loop {
        if !writable(&service.record(&chunk.task_id)?.status) {
            discard_one(service, &chunk.task_id, chunk.index);
            return Ok(false);
        }
        match write_segment(&path, within, &chunk.data).await {
            Ok(()) => return Ok(true),
            Err(err) if is_disk_full(&err) => tokio::time::sleep(CACHE_POLL).await,
            Err(err) => return Err(err),
        }
    }
}

/// 在分段文件的指定偏移写入一块数据并持久化。
async fn write_segment(path: &Path, within: u64, data: &[u8]) -> AppResult<()> {
    let mut file = tokio::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path)
        .await?;
    file.seek(std::io::SeekFrom::Start(within)).await?;
    file.write_all(data).await?;
    file.sync_data().await?;
    Ok(())
}

/// 是否为「磁盘写满」类错误（Windows 112 / 类 Unix 28）。
fn is_disk_full(err: &AppError) -> bool {
    match err {
        AppError::Io(io) => matches!(io.raw_os_error(), Some(28) | Some(112)),
        _ => false,
    }
}

/// 建立缓冲：复用（或新建）文件句柄，起点取「该文件第一个未完成块」的偏移。
async fn open_buffer(
    service: &Arc<TransferService>,
    task: &TaskRecord,
    file: usize,
) -> AppResult<FileBuffer> {
    let key = (task.id.clone(), file);
    let mut next_offset = task.manifest.entries[file].size;
    for (i, chunk) in task.manifest.chunks().iter().enumerate() {
        if chunk.file == file && !task.completed.contains(&i) {
            next_offset = chunk.offset;
            break;
        }
    }
    if task.use_cache {
        // 缓存模式：只为缓存盘建目录，绝不触碰目标盘。
        let cache_root = service
            .cache_root
            .as_ref()
            .ok_or_else(|| AppError::Other("缓存目录不可用".into()))?;
        std::fs::create_dir_all(paths::cache_task(cache_root, &task.id))?;
        return Ok(FileBuffer {
            ready: BTreeMap::new(),
            next_offset,
            gap_since: None,
            handle: None,
            expected_size: task.manifest.entries[file].size,
            use_cache: true,
        });
    }
    let stage = paths::stage(task)?;
    std::fs::create_dir_all(&stage)?;
    paths::ensure_plain(&stage)?;
    let part = stage.join(format!("{file}.part"));
    paths::ensure_plain(&part)?;
    let handle = match cached_handle(service, &key) {
        Some(handle) => handle,
        None => {
            let opened = tokio::fs::OpenOptions::new()
                .create(true)
                .truncate(false)
                .read(true)
                .write(true)
                .open(&part)
                .await?;
            let handle = Arc::new(AsyncMutex::new(opened));
            service
                .receive_files
                .lock()
                .unwrap()
                .insert(key, handle.clone());
            handle
        }
    };
    Ok(FileBuffer {
        ready: BTreeMap::new(),
        next_offset,
        gap_since: None,
        handle: Some(handle),
        expected_size: task.manifest.entries[file].size,
        use_cache: false,
    })
}

/// 从待写登记中移除一个块（丢弃或已落盘后调用）。
fn discard_one(service: &Arc<TransferService>, task_id: &str, index: usize) {
    service
        .write_pending
        .lock()
        .unwrap()
        .remove(&(task_id.to_string(), index));
}

/// 写入失败清理：丢弃该任务全部未落盘登记（不记录完成，续传时重传），
/// 避免 `commit` 永久等待，并把错误上报到任务。
fn fail_task(service: &Arc<TransferService>, task_id: &str, err: &crate::error::AppError) {
    service
        .write_pending
        .lock()
        .unwrap()
        .retain(|(task, _)| task != task_id);
    // 写盘失败后不再继续复用句柄：释放目标盘句柄（调用方已移除文件缓冲）。
    service.release_files(task_id);
    let _ = service.edit(task_id, |t| {
        t.error = Some(err.to_string());
        t.event(format!("写入失败：{err}"));
    });
}