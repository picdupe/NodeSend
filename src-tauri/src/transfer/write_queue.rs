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
//! - 取消 / 拒绝 / 完成：未落盘块被丢弃且不记录完成；
//! - 暂停：`receive_chunk` 立即拒绝新块，但**已 Ack 的块继续写完**——发送端已据 Ack
//!   记为完成、不会重传，若此处丢弃将造成永久缺口（提交时报「仍有未提交 Chunk」）；
//! - 进程退出：未落盘块丢失，续传时由接收端权威进度与发送端状态比对后重传。
//! 因此不会出现「完成状态说完成了，但数据没落盘」。

use super::{paths, relay, TransferService};
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

/// 有界写入队列容量（单位：块）。见 [`crate::tuning::QUEUE_CHUNKS`]。
pub(crate) const QUEUE_CHUNKS: usize = crate::tuning::QUEUE_CHUNKS;

/// 入队等待上限：队列满时最多等这么久（等待期间不持有任务锁）。
/// 超过则返回可诊断错误，避免慢盘把会话无限期挂住、进而耗尽连接许可。
pub(crate) const QUEUE_WAIT_TIMEOUT: Duration = crate::tuning::QUEUE_WAIT_TIMEOUT;

/// 单文件缓冲上限（单位：块）。达到即强制按序写出，避免缺口期间无限占用内存。
const FORCE_DRAIN_CHUNKS: usize = crate::tuning::QUEUE_CHUNKS;

/// 缺口等待上限：超过该时长仍未补齐，则降级为乱序写。
/// 顺序写只是性能优化——每块自带 offset，乱序写依然正确。
const GAP_TIMEOUT: Duration = crate::tuning::WRITE_GAP_TIMEOUT;

/// 后台任务巡检间隔：触发缺口超时降级与失效缓冲清理。
const TICK: Duration = crate::tuning::WRITE_TICK;

/// 缓存盘写满后的重试间隔（不报错、不丢数据，等待转写任务释放空间）。
const CACHE_POLL: Duration = crate::tuning::CACHE_SPACE_POLL;

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

/// 任务是否处于可写入状态（供转写调度等「是否继续推进」判定使用）。
pub(crate) fn writable(status: &str) -> bool {
    ["accepted", "transferring"].contains(&status)
}

/// 已入队（已向发送端 Ack）的块是否应放弃落盘。
///
/// 暂停**不丢弃**已收块：这些块已对发送端确认，发送端据此记为完成；若在此丢弃，
/// 接收端会永久缺失这些块而发送端不会重传（发送端以 Ack 为准），导致提交时报
/// 「仍有未提交 Chunk」。暂停期间 `receive_chunk` 已拒绝新块，因此不会新增。
/// 只有取消 / 拒绝 / 完成等终态才放弃这些块。
fn abandon(status: &str) -> bool {
    ["cancelled", "rejected", "completed"].contains(&status)
}

/// 取已登记的共享文件句柄（复用任务运行期资源里的句柄，避免重复 open）。
fn cached_handle(
    service: &Arc<TransferService>,
    task_id: &str,
    file: usize,
) -> Option<Arc<AsyncMutex<tokio::fs::File>>> {
    let runtime = service.runtime_if_exists(task_id)?;
    let handle = crate::locks::lock(&runtime.receive_files).get(&file).cloned();
    handle
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
        service.clear_all_write_pending();
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

    async fn store(
        &mut self,
        service: &Arc<TransferService>,
        chunk: PendingChunk,
    ) -> AppResult<()> {
        // 热路径：只用 O(1) 访问器判断可写与跳过，不加载/克隆清单明细。
        let status = service.status_of(&chunk.task_id)?;
        // 仅在终态（取消/拒绝/完成）放弃已收块；暂停仍写完已确认的块（见 `abandon`）。
        if abandon(&status) || service.is_skipped(&chunk.task_id, chunk.file) {
            discard_one(service, &chunk.task_id, chunk.index);
            return Ok(());
        }
        let key = (chunk.task_id.clone(), chunk.file);
        if !self.files.contains_key(&key) {
            let buffer = open_buffer(service, &chunk.task_id, chunk.file).await?;
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
            // 增量写入：只推进该块与汇总标量，不重写整份清单/进度。
            service.mark_chunk_done(&chunk.task_id, chunk.index, chunk.data.len() as u64)?;
            discard_one(service, &chunk.task_id, chunk.index);
        }
    }

    /// 巡检：任务进入终态则丢弃其未落盘块；否则重试排空（触发缺口超时降级）。
    /// 暂停不丢块——已确认的块继续写完，避免发送端与接收端进度永久不一致。
    async fn tick(&mut self, service: &Arc<TransferService>) {
        for key in self.files.keys().cloned().collect::<Vec<_>>() {
            let abandon_now = service
                .status_of(&key.0)
                .map(|status| abandon(&status))
                .unwrap_or(true);
            if abandon_now {
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
        if abandon(&service.status_of(&chunk.task_id)?) {
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
    id: &str,
    file: usize,
) -> AppResult<FileBuffer> {
    // 只用 O(1) 访问器取「下一个待写偏移 / 文件大小 / 是否缓存模式」。
    let (next_offset, expected_size, use_cache) = service.file_write_plan(id, file)?;
    if use_cache {
        // 缓存模式：只为缓存盘建目录，绝不触碰目标盘。
        let cache_root = service
            .cache_root
            .as_ref()
            .ok_or_else(|| AppError::Other("缓存目录不可用".into()))?;
        std::fs::create_dir_all(paths::cache_task(cache_root, id))?;
        return Ok(FileBuffer {
            ready: BTreeMap::new(),
            next_offset,
            gap_since: None,
            handle: None,
            expected_size,
            use_cache: true,
        });
    }
    let stage = service.stage_dir(id)?;
    std::fs::create_dir_all(&stage)?;
    paths::ensure_plain(&stage)?;
    let part = stage.join(format!("{file}.part"));
    paths::ensure_plain(&part)?;
    let handle = match cached_handle(service, id, file) {
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
            crate::locks::lock(&service.runtime(id).receive_files).insert(file, handle.clone());
            handle
        }
    };
    Ok(FileBuffer {
        ready: BTreeMap::new(),
        next_offset,
        gap_since: None,
        handle: Some(handle),
        expected_size,
        use_cache: false,
    })
}

/// 从待写登记中移除一个块（丢弃或已落盘后调用）。
fn discard_one(service: &Arc<TransferService>, task_id: &str, index: usize) {
    if let Some(runtime) = service.runtime_if_exists(task_id) {
        crate::locks::lock(&runtime.write_pending).remove(&index);
    }
}

/// 写入失败清理：丢弃该任务全部未落盘登记（不记录完成，续传时重传），
/// 避免 `commit` 永久等待，并把错误上报到任务。
fn fail_task(service: &Arc<TransferService>, task_id: &str, err: &crate::error::AppError) {
    if let Some(runtime) = service.runtime_if_exists(task_id) {
        crate::locks::lock(&runtime.write_pending).clear();
    }
    // 写盘失败后不再继续复用句柄：释放目标盘句柄（调用方已移除文件缓冲）。
    service.release_files(task_id);
    let _ = service.edit(task_id, |t| {
        t.error = Some(err.to_string());
        t.event(format!("写入失败：{err}"));
    });
}
