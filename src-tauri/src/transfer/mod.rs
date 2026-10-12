//! Global native TransferService. Tasks own manifests and durable chunk state.
pub mod model;
pub mod paths;
mod receive;
mod relay;
mod send;
pub mod source;
mod write_queue;
use crate::locks::MutexExt;
use crate::{
    error::{AppError, AppResult},
    locks,
    service::AppService,
};
use model::{
    random_id, Chunk, ConflictPolicy, FileEntry, Manifest, TaskRecord, TaskSnapshot,
};
use std::{
    collections::{BTreeSet, HashMap, HashSet},
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
};
use tokio::sync::{mpsc, Mutex as AsyncMutex, Semaphore};

/// 单帧清单条数（发送起点）。见 [`crate::tuning::MANIFEST_BATCH`]。
pub(crate) const MANIFEST_BATCH: usize = crate::tuning::MANIFEST_BATCH;

/// 清单落库批次（累计多少条写一次数据库）。见 [`crate::tuning::MANIFEST_PAGE`]。
pub(crate) const MANIFEST_PAGE: usize = crate::tuning::MANIFEST_PAGE;

/// 传输分块大小。见 [`crate::tuning::CHUNK_SIZE`]。
pub(crate) const CHUNK_SIZE: u64 = crate::tuning::CHUNK_SIZE;

/// 发送端同时保留的读句柄上限。见 [`crate::tuning::SEND_HANDLES_LIMIT`]。
pub(crate) const SEND_HANDLES_LIMIT: usize = crate::tuning::SEND_HANDLES_LIMIT;

/// 缓存中转的只读标量视图：供转写调度与 `commit` 判定使用，**不加载清单明细**。
/// 全部字段来自 `TaskRecord` 的持久化标量，因此构造与使用都是 O(1)。
pub(crate) struct RelayState {
    pub status: String,
    pub use_cache: bool,
    /// 转写错误（`None` 表示正常）；转写失败后调度器不再重试。
    pub error: Option<String>,
    pub completed_count: usize,
    pub chunk_count: usize,
    pub completed_bytes: u64,
    pub relayed_total: u64,
    pub relay_target_bytes: u64,
}

/// 转写任务所需的只读计划：每个文件条目、块基址、目标路径与已转写进度一次取齐。
/// 仅在转写任务启动时取一次；条目与目标在 `decide` 后不再变化。
pub(crate) struct RelayPlan {
    pub use_cache: bool,
    pub entries: Vec<FileEntry>,
    pub bases: Vec<usize>,
    pub chunk_size: u64,
    pub targets: Vec<Option<String>>,
}

/// 单个任务的运行期状态（持久化记录之外的运行时资源）。
///
/// 以前这些是 7 张按任务 ID 索引的独立集合（gates / active / write_pending /
/// relay_active / committing / send_files / receive_files），各自清理、各自成键，
/// 生命周期与锁顺序只能靠人记。现在收敛成一个结构体：任务结束调用
/// [`TransferService::release_runtime`] 即整体释放。
#[derive(Default)]
pub(crate) struct TaskRuntime {
    /// 串行化该任务的会话、决策与提交操作。
    pub(crate) gate: Arc<AsyncMutex<()>>,
    /// 是否有发送任务正在运行该任务。
    pub(crate) active: AtomicBool,
    /// 已入队但尚未落盘并记录的块下标。
    /// 同时用于：接收路径去重、`commit` 等待本任务排空。
    pub(crate) write_pending: Mutex<HashSet<usize>>,
    /// 缓存→目标盘的转写任务是否正在运行。
    pub(crate) relay_active: AtomicBool,
    /// 是否正在执行提交。提交要对每个文件做 rename，可能持续很久，
    /// 因此提交期间**不持有**任务锁，只用该标志避免并发重复提交。
    pub(crate) committing: AtomicBool,
    /// 发送端读句柄（文件序号 → File）。同一文件的连续分块复用同一句柄，
    /// 机械盘才能保持顺序预读；总量有上限，超出即按任务回收。
    pub(crate) send_files: Mutex<HashMap<usize, Arc<AsyncMutex<tokio::fs::File>>>>,
    /// 接收端目标盘写句柄（文件序号 → File）。
    pub(crate) receive_files: Mutex<HashMap<usize, Arc<AsyncMutex<tokio::fs::File>>>>,
}

pub struct TransferService {
    pub(crate) app: Arc<AppService>,
    tasks: Mutex<HashMap<String, TaskRecord>>,
    /// 按任务 ID 的运行期资源；随任务结束释放。
    runtime: Mutex<HashMap<String, Arc<TaskRuntime>>>,
    scheduler: Arc<Semaphore>,
    pub(crate) shutdown: tokio_util::sync::CancellationToken,
    /// 接收写入队列（有界，满时背压接收路径）。见 `write_queue` 模块。
    pub(crate) write_tx: mpsc::Sender<write_queue::PendingChunk>,
    /// 缓存盘中转目录（`None` 表示缓存不可用，全部任务自动回退直写）。见 `relay` 模块。
    pub(crate) cache_root: Option<PathBuf>,
}
impl TransferService {
    pub fn new(app: Arc<AppService>) -> AppResult<Arc<Self>> {
        let records: Vec<String> = app.db.with_conn(|conn| {
            let mut stmt = conn.prepare("SELECT record FROM transfer_tasks")?;
            let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
            Ok(rows.collect::<Result<Vec<_>, _>>()?)
        })?;
        let mut tasks = HashMap::new();
        for json in records {
            // 单条记录反序列化失败（例如旧版本不兼容的字段）不应中断启动：
            // 跳过该任务，避免整个应用无法启动。
            let mut task: TaskRecord = match serde_json::from_str(&json) {
                Ok(task) => task,
                Err(err) => {
                    tracing::warn!("跳过无法解析的传输任务记录：{err}");
                    continue;
                }
            };
            if [
                "connecting",
                "transferring",
                "interrupted",
                "accepted",
                "queued",
            ]
            .contains(&task.status.as_str())
            {
                task.status = "paused".into();
                task.event("上次意外中断，已暂停；点击继续恢复传输");
            }
            tasks.insert(task.id.clone(), task);
        }
        let (write_tx, write_rx) = mpsc::channel(write_queue::QUEUE_CHUNKS);
        let cache_root = relay::resolve_cache_root(&app);
        let service = Arc::new(Self {
            app,
            tasks: Mutex::new(tasks),
            runtime: Mutex::new(HashMap::new()),
            scheduler: Arc::new(Semaphore::new(crate::tuning::MAX_ACTIVE_SENDS)),
            shutdown: tokio_util::sync::CancellationToken::new(),
            write_tx,
            cache_root,
        });
        write_queue::spawn(Arc::clone(&service), write_rx);
        relay::spawn(Arc::clone(&service));
        relay::sweep(&service);
        Ok(service)
    }
    /// 全部接收任务的 ID（供转写调度器扫描）。
    pub(crate) fn receive_ids(&self) -> Vec<String> {
        self.tasks
            .lock_or_recover()
            .values()
            .filter(|t| t.direction == "receive")
            .map(|t| t.id.clone())
            .collect()
    }
    pub fn snapshots(&self) -> Vec<TaskSnapshot> {
        let mut tasks = self
            .tasks
            .lock_or_recover()
            .values()
            .map(TaskRecord::snapshot)
            .collect::<Vec<_>>();
        tasks.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        tasks
    }
    /// 读取任务（含运行期 bulk）：首次访问时从明细表载入并缓存回内存，
    /// 之后同一任务不再重复读库。
    ///
    /// 注意：`record()` 会克隆整份清单明细，因此**不要**在「每块/每轮巡检」的热路径里调用；
    /// 热路径请改用下面的 O(1) 访问器与增量写入方法。
    pub fn record(&self, id: &str) -> AppResult<TaskRecord> {
        self.ensure_bulk(id)?;
        let map = locks::lock(&self.tasks);
        map.get(id)
            .cloned()
            .ok_or_else(|| AppError::NotFound("传输任务不存在".into()))
    }
    pub(crate) fn insert(&self, task: TaskRecord) -> AppResult<()> {
        {
            let map = locks::lock(&self.tasks);
            if map.contains_key(&task.id) {
                return Err(AppError::Conflict("任务已经存在".into()));
            }
        }
        // 清单明细可能达百万条：落库在锁外进行，避免长时间阻塞状态查询（UI 轮询）。
        self.persist(&task)?;
        self.save_entries(&task)?;
        locks::lock(&self.tasks).insert(task.id.clone(), task);
        Ok(())
    }
    fn persist(&self, task: &TaskRecord) -> AppResult<()> {
        // 只序列化标量字段（bulk 字段标了 #[serde(skip)]），因此开销与文件数无关。
        let json = serde_json::to_string(task)?;
        self.app.db.with_conn(|conn| {
            conn.execute(
                "INSERT INTO transfer_tasks(id,record) VALUES(?1,?2) ON CONFLICT(id) DO UPDATE SET record=excluded.record",
                rusqlite::params![task.id, json],
            )?;
            Ok(())
        })
    }
    /// 写入某任务的清单明细（创建任务时一次）。
    fn save_entries(&self, task: &TaskRecord) -> AppResult<()> {
        if task.manifest.entries.is_empty() {
            return Ok(());
        }
        self.app.db.with_transaction(|tx| {
            let mut stmt = tx.prepare("INSERT OR REPLACE INTO task_entries(task_id,index_no,path,size,modified_ms,directory) VALUES(?1,?2,?3,?4,?5,?6)")?;
            for (i, e) in task.manifest.entries.iter().enumerate() {
                stmt.execute(rusqlite::params![
                    task.id,
                    i as i64,
                    e.path,
                    e.size as i64,
                    e.modified_ms as i64,
                    e.directory as i64
                ])?;
            }
            Ok(())
        })
    }
    /// 增量追加一段清单明细（接收端边收边落库，不驻留内存）。`first` 为该片首条下标。
    pub(crate) fn append_entries(
        &self,
        id: &str,
        first: usize,
        entries: &[FileEntry],
    ) -> AppResult<()> {
        self.app.db.with_transaction(|tx| {
            let mut stmt = tx.prepare("INSERT OR REPLACE INTO task_entries(task_id,index_no,path,size,modified_ms,directory) VALUES(?1,?2,?3,?4,?5,?6)")?;
            for (i, e) in entries.iter().enumerate() {
                stmt.execute(rusqlite::params![
                    id,
                    (first + i) as i64,
                    e.path,
                    e.size as i64,
                    e.modified_ms as i64,
                    e.directory as i64
                ])?;
            }
            Ok(())
        })
    }
    /// Persist only scalar scan progress without cloning the growing manifest.
    /// Entry rows are appended before this cursor advances, so it never points
    /// beyond durable manifest data after a crash.
    pub(crate) fn update_scan_progress(
        &self,
        id: &str,
        count: usize,
        directories: usize,
        total_size: u64,
        chunk_count: usize,
        last_path: &str,
    ) -> AppResult<()> {
        let mut map = locks::lock(&self.tasks);
        let task = map
            .get_mut(id)
            .ok_or_else(|| AppError::NotFound("传输任务不存在".into()))?;
        task.file_count = count;
        task.directory_count = directories;
        task.total_size = total_size;
        task.chunk_count = chunk_count;
        task.scan_last_seq = count;
        task.scan_last_path = last_path.to_string();
        task.updated_at = chrono::Utc::now().to_rfc3339();
        self.persist(task)?;
        Ok(())
    }
    /// Append a sender-side batch to the durable table and live manifest without
    /// cloning all previously scanned entries.
    pub(crate) fn append_source_batch(
        &self,
        id: &str,
        first: usize,
        entries: &[FileEntry],
        directories: usize,
        total_size: u64,
        chunk_count: usize,
    ) -> AppResult<()> {
        // bulk 明细在锁外载入（见 ensure_bulk），避免读几十万行时占住任务锁。
        self.ensure_bulk(id)?;
        let mut map = locks::lock(&self.tasks);
        self.load_bulk_locked(&mut map, id)?;
        let task = map
            .get_mut(id)
            .ok_or_else(|| AppError::NotFound("发送任务不存在".into()))?;
        if task.manifest.entries.len() != first {
            return Err(AppError::Conflict("本地清单游标与持久化条目不一致".into()));
        }
        self.append_entries(id, first, entries)?;
        let mut all = std::mem::take(&mut task.manifest.entries);
        all.extend(entries.iter().cloned());
        task.manifest = Manifest::new(all, task.chunk_size, task.verify_hash);
        task.file_count = first + entries.len();
        task.directory_count = directories;
        task.total_size = total_size;
        task.chunk_count = chunk_count;
        task.scan_last_seq = task.file_count;
        task.scan_last_path = entries.last().map(|entry| entry.path.clone()).unwrap_or_default();
        task.updated_at = chrono::Utc::now().to_rfc3339();
        self.persist(task)?;
        Ok(())
    }
    pub(crate) fn finish_source_scan(&self, id: &str, digest: String) -> AppResult<()> {
        let mut map = locks::lock(&self.tasks);
        let task = map
            .get_mut(id)
            .ok_or_else(|| AppError::NotFound("发送任务不存在".into()))?;
        task.file_count = task.manifest.entries.len();
        task.directory_count = task.manifest.entries.iter().filter(|entry| entry.directory).count();
        task.total_size = task.manifest.total();
        task.chunk_count = task.manifest.chunk_count();
        task.digest = digest;
        task.scan_last_seq = task.file_count;
        task.scan_last_path = task.manifest.entries.last().map(|entry| entry.path.clone()).unwrap_or_default();
        task.scan_complete = true;
        task.updated_at = chrono::Utc::now().to_rfc3339();
        self.persist(task)?;
        Ok(())
    }
    fn load_bulk_locked(&self, map: &mut HashMap<String, TaskRecord>, id: &str) -> AppResult<()> {
        let Some(base) = map.get(id) else {
            return Ok(());
        };
        if base.bulk_loaded() {
            return Ok(());
        }
        let loaded = self.read_bulk(base)?;
        map.insert(id.to_string(), loaded);
        Ok(())
    }
    /// 确保运行期 bulk 已载入，且**读库过程不持有任务锁**。
    ///
    /// `read_bulk` 要读几十万行明细（1 TB/20 万文件场景），若在锁内执行，
    /// UI 每秒一次的状态快照会被整段卡住。这里的顺序是：
    /// 锁内只取标量快照 → 锁外读库装配 → 再锁一次写回；期间若别的线程已经
    /// 装配完成，则保留它的结果（避免覆盖更新的状态字段）。
    ///
    /// 注意：DB 是明细与汇总标量的唯一权威（条目先落库、游标后推进），因此
    /// 由 `read_bulk` 重算的标量可以整体采用。
    fn ensure_bulk(&self, id: &str) -> AppResult<()> {
        let base = {
            let map = locks::lock(&self.tasks);
            let task = map
                .get(id)
                .ok_or_else(|| AppError::NotFound("传输任务不存在".into()))?;
            if task.bulk_loaded() {
                return Ok(());
            }
            task.clone()
        };
        let loaded = self.read_bulk(&base)?;
        let mut map = locks::lock(&self.tasks);
        let Some(current) = map.get_mut(id) else {
            return Ok(());
        };
        if current.bulk_loaded() {
            return Ok(());
        }
        // 只采用明细与由明细重算的标量；其它字段（状态、错误、事件、决策）保持当前值。
        current.manifest = loaded.manifest;
        current.targets = loaded.targets;
        current.completed = loaded.completed;
        current.skipped_files = loaded.skipped_files;
        current.relayed = loaded.relayed;
        current.file_count = loaded.file_count;
        current.directory_count = loaded.directory_count;
        current.total_size = loaded.total_size;
        current.chunk_count = loaded.chunk_count;
        current.completed_count = loaded.completed_count;
        current.completed_bytes = loaded.completed_bytes;
        current.relayed_total = loaded.relayed_total;
        current.skipped_bytes = loaded.skipped_bytes;
        current.relay_target_bytes = loaded.relay_target_bytes;
        Ok(())
    }
    /// 从明细表读取运行期 bulk，并据表内容重算汇总标量（表为唯一权威）。
    fn read_bulk(&self, base: &TaskRecord) -> AppResult<TaskRecord> {
        let mut task = base.clone();
        let entries = self.app.db.with_conn(|conn| {
            let mut stmt = conn.prepare("SELECT path,size,modified_ms,directory FROM task_entries WHERE task_id=?1 ORDER BY index_no")?;
            let rows = stmt.query_map([base.id.as_str()], |r| {
                Ok(FileEntry {
                    path: r.get(0)?,
                    size: r.get::<_, i64>(1)? as u64,
                    modified_ms: r.get::<_, i64>(2)? as u64,
                    directory: r.get::<_, i64>(3)? != 0,
                })
            })?;
            let mut entries = Vec::new();
            for row in rows {
                entries.push(row?);
            }
            Ok(entries)
        })?;
        task.manifest = Manifest::new(entries, base.chunk_size, base.verify_hash);
        // 汇总标量：**仅在清单完整时**按明细重算。残缺清单（清单传输中断）不得覆盖
        // 标量，否则 `file_count` 会被截断成已收片段数，导致「是否收齐」被误判。
        if task.manifest.entries.len() == base.file_count {
            task.file_count = task.manifest.entries.len();
            task.directory_count = task.manifest.entries.iter().filter(|e| e.directory).count();
            task.total_size = task.manifest.total();
            task.chunk_count = task.manifest.chunk_count();
        }
        let (completed, skipped, targets, relayed) = self.app.db.with_conn(|conn| {
            let mut completed = BTreeSet::new();
            {
                let mut stmt = conn.prepare("SELECT chunk_index FROM task_chunks WHERE task_id=?1")?;
                let rows = stmt.query_map([base.id.as_str()], |r| r.get::<_, i64>(0))?;
                for row in rows {
                    completed.insert(row? as usize);
                }
            }
            let mut skipped = BTreeSet::new();
            {
                let mut stmt = conn.prepare("SELECT file_index FROM task_skipped WHERE task_id=?1")?;
                let rows = stmt.query_map([base.id.as_str()], |r| r.get::<_, i64>(0))?;
                for row in rows {
                    skipped.insert(row? as usize);
                }
            }
            let mut targets: Vec<Option<String>> = vec![
                None;
                base.file_count
                    .max(task.manifest.entries.len())
                    .max(task.targets.len())
            ];
            {
                let mut stmt =
                    conn.prepare("SELECT file_index,target FROM task_targets WHERE task_id=?1")?;
                let rows = stmt.query_map([base.id.as_str()], |r| {
                    Ok((r.get::<_, i64>(0)? as usize, r.get::<_, String>(1)?))
                })?;
                for row in rows {
                    let (i, target) = row?;
                    if let Some(slot) = targets.get_mut(i) {
                        *slot = Some(target);
                    }
                }
            }
            let mut relayed = vec![0u64; base.file_count.max(task.relayed.len())];
            {
                let mut stmt =
                    conn.prepare("SELECT file_index,relayed FROM task_relay WHERE task_id=?1")?;
                let rows = stmt.query_map([base.id.as_str()], |r| {
                    Ok((r.get::<_, i64>(0)? as usize, r.get::<_, i64>(1)? as u64))
                })?;
                for row in rows {
                    let (i, value) = row?;
                    if let Some(slot) = relayed.get_mut(i) {
                        *slot = value;
                    }
                }
            }
            Ok((completed, skipped, targets, relayed))
        })?;
        task.completed = completed;
        task.skipped_files = skipped;
        task.targets = targets;
        task.relayed = relayed;
        // 汇总标量一律按表重算，避免与明细不一致。
        task.completed_count = task.completed.len();
        task.completed_bytes = task
            .completed
            .iter()
            .filter_map(|i| task.manifest.chunk(*i))
            .filter(|c| !task.skipped_files.contains(&c.file))
            .map(|c| c.length as u64)
            .sum();
        task.relayed_total = task.relayed.iter().copied().sum();
        task.skipped_bytes = task
            .skipped_files
            .iter()
            .filter_map(|i| task.manifest.entries.get(*i))
            .map(|e| e.size)
            .sum();
        task.relay_target_bytes = task
            .manifest
            .entries
            .iter()
            .enumerate()
            .filter(|(i, e)| {
                !e.directory && e.size > 0 && task.targets.get(*i).is_some_and(Option::is_some)
            })
            .map(|(_, e)| e.size)
            .sum();
        Ok(task)
    }
    /// 记录一个「真正落盘完成」的块：内存去重 + 增量写 `task_chunks` + 推进进度标量。
    /// 不克隆清单明细，也不重写整份进度集合，因此对百万级清单仍是 O(1)。
    pub(crate) fn mark_chunk_done(&self, id: &str, index: usize, bytes: u64) -> AppResult<bool> {
        let json = {
            // bulk 明细在锁外载入（见 ensure_bulk），避免读几十万行时占住任务锁。
            self.ensure_bulk(id)?;
            let mut map = locks::lock(&self.tasks);
            self.load_bulk_locked(&mut map, id)?;
            let task = map
                .get_mut(id)
                .ok_or_else(|| AppError::NotFound("任务不存在".into()))?;
            let mut changed = false;
            if task.completed.insert(index) {
                task.completed_count = task.completed.len();
                task.completed_bytes = task.completed_bytes.saturating_add(bytes);
                changed = true;
            }
            if task.status == "accepted" {
                task.status = "transferring".into();
                changed = true;
            }
            if !changed {
                return Ok(false);
            }
            task.updated_at = chrono::Utc::now().to_rfc3339();
            serde_json::to_string(&*task)?
        };
        self.app.db.with_conn(|conn| {
            conn.execute(
                "INSERT OR IGNORE INTO task_chunks(task_id,chunk_index) VALUES(?1,?2)",
                rusqlite::params![id, index as i64],
            )?;
            conn.execute(
                "UPDATE transfer_tasks SET record=?1 WHERE id=?2",
                rusqlite::params![json, id],
            )?;
            Ok(())
        })?;
        Ok(true)
    }
    /// 记录缓存中转的转写进度（只写非零行；进度以 `task_relay` 表为权威）。
    pub(crate) fn set_relayed(&self, id: &str, file: usize, bytes: u64) -> AppResult<()> {
        {
            // bulk 明细在锁外载入（见 ensure_bulk），避免读几十万行时占住任务锁。
            self.ensure_bulk(id)?;
            let mut map = locks::lock(&self.tasks);
            self.load_bulk_locked(&mut map, id)?;
            let task = map
                .get_mut(id)
                .ok_or_else(|| AppError::NotFound("任务不存在".into()))?;
            if task.relayed.len() < task.file_count {
                task.relayed.resize(task.file_count, 0);
            }
            let old = task.relayed.get(file).copied().unwrap_or(0);
            if bytes <= old {
                return Ok(());
            }
            task.relayed_total = task.relayed_total.saturating_add(bytes - old);
            task.relayed[file] = bytes;
            task.updated_at = chrono::Utc::now().to_rfc3339();
        }
        self.app.db.with_conn(|conn| {
            conn.execute(
                "INSERT INTO task_relay(task_id,file_index,relayed) VALUES(?1,?2,?3) ON CONFLICT(task_id,file_index) DO UPDATE SET relayed=excluded.relayed",
                rusqlite::params![id, file as i64, bytes as i64],
            )?;
            Ok(())
        })
    }
    /// 标记文件被跳过（源文件变化）：内存 + `task_skipped`/`task_chunks`/`task_targets` 增量写入。
    pub(crate) fn mark_skipped(&self, id: &str, file: usize) -> AppResult<()> {
        let (json, chunks, newly) = {
            // bulk 明细在锁外载入（见 ensure_bulk），避免读几十万行时占住任务锁。
            self.ensure_bulk(id)?;
            let mut map = locks::lock(&self.tasks);
            self.load_bulk_locked(&mut map, id)?;
            let task = map
                .get_mut(id)
                .ok_or_else(|| AppError::NotFound("任务不存在".into()))?;
            let entry = task
                .manifest
                .entries
                .get(file)
                .cloned()
                .ok_or_else(|| AppError::BadRequest("文件编号非法".into()))?;
            let base = task.manifest.file_base(file);
            let count = if entry.directory {
                0
            } else {
                entry.size.div_ceil(task.chunk_size) as usize
            };
            let mut added = Vec::new();
            for i in base..base + count {
                if task.completed.insert(i) {
                    added.push(i);
                }
            }
            task.completed_count = task.completed.len();
            if task.targets.len() > file {
                task.targets[file] = None;
            }
            let newly = !task.skipped_files.contains(&file);
            task.note_skipped(file, entry.size, &entry.path);
            if newly {
                task.event(format!("源文件已变化，跳过：{}", entry.path));
            }
            (serde_json::to_string(&*task)?, added, newly)
        };
        self.app.db.with_conn(|conn| {
            {
                let mut stmt = conn
                    .prepare("INSERT OR IGNORE INTO task_chunks(task_id,chunk_index) VALUES(?1,?2)")?;
                for i in &chunks {
                    stmt.execute(rusqlite::params![id, *i as i64])?;
                }
            }
            conn.execute(
                "INSERT OR IGNORE INTO task_skipped(task_id,file_index) VALUES(?1,?2)",
                rusqlite::params![id, file as i64],
            )?;
            if newly {
                conn.execute(
                    "DELETE FROM task_targets WHERE task_id=?1 AND file_index=?2",
                    rusqlite::params![id, file as i64],
                )?;
            }
            conn.execute(
                "UPDATE transfer_tasks SET record=?1 WHERE id=?2",
                rusqlite::params![json, id],
            )?;
            Ok(())
        })
    }
    /// `decide` 后把内存中的「目标路径 + 已完成集合 + 转写目标字节数」整体写回明细表。
    pub(crate) fn save_decision(&self, id: &str) -> AppResult<()> {
        let task = self.record(id)?;
        self.app.db.with_transaction(|tx| {
            tx.execute("DELETE FROM task_targets WHERE task_id=?1", [id])?;
            tx.execute("DELETE FROM task_chunks WHERE task_id=?1", [id])?;
            {
                let mut stmt = tx.prepare(
                    "INSERT OR REPLACE INTO task_targets(task_id,file_index,target) VALUES(?1,?2,?3)",
                )?;
                for (i, target) in task.targets.iter().enumerate() {
                    if let Some(value) = target {
                        stmt.execute(rusqlite::params![id, i as i64, value])?;
                    }
                }
            }
            {
                let mut stmt =
                    tx.prepare("INSERT OR IGNORE INTO task_chunks(task_id,chunk_index) VALUES(?1,?2)")?;
                for index in &task.completed {
                    stmt.execute(rusqlite::params![id, *index as i64])?;
                }
            }
            Ok(())
        })
    }
    /// 只读的状态（热路径用，不加载明细）。
    pub(crate) fn status_of(&self, id: &str) -> AppResult<String> {
        locks::lock(&self.tasks)
            .get(id)
            .map(|t| t.status.clone())
            .ok_or_else(|| AppError::NotFound("任务不存在".into()))
    }
    /// 清单是否已完整收齐（`task_entries` 行数 == 标量 `file_count`）与清单指纹。
    /// 不加载明细，供重复 `Offer` 判定「清单是否需重新传输」。
    pub(crate) fn manifest_received(&self, id: &str) -> AppResult<(String, bool)> {
        let (digest, expected, task_complete) = {
            let map = locks::lock(&self.tasks);
            let task = map
                .get(id)
                .ok_or_else(|| AppError::NotFound("任务不存在".into()))?;
            (task.digest.clone(), task.file_count, task.scan_complete)
        };
        let count: i64 = self.app.db.with_conn(|conn| {
            Ok(conn.query_row(
                "SELECT COUNT(*) FROM task_entries WHERE task_id=?1",
                [id],
                |r| r.get(0),
            )?)
        })?;
        Ok((digest, task_complete && count as usize == expected))
    }
    /// 清空某任务已收的清单片段（清单传输中断后重传前调用），并让内存缓存失效，
    /// 使后续 `record()` 从明细表重新载入完整清单。
    pub(crate) fn clear_entries(&self, id: &str) -> AppResult<()> {
        self.app.db.with_conn(|conn| {
            conn.execute("DELETE FROM task_entries WHERE task_id=?1", [id])?;
            Ok(())
        })?;
        let mut map = locks::lock(&self.tasks);
        if let Some(task) = map.get_mut(id) {
            task.manifest = Manifest::default();
            task.file_count = 0;
            task.directory_count = 0;
            task.total_size = 0;
            task.chunk_count = 0;
            task.scan_last_seq = 0;
            task.scan_last_path.clear();
            task.scan_complete = false;
            self.persist(task)?;
        }
        Ok(())
    }
    /// 按需分页读取清单明细（供 IPC：避免把整份清单一次传到前端）。返回该页明细与总数。
    pub fn entries_page(
        &self,
        id: &str,
        offset: usize,
        limit: usize,
    ) -> AppResult<(Vec<FileEntry>, usize)> {
        let limit = limit.clamp(1, 1000);
        self.app.db.with_conn(|conn| {
            let total: i64 = conn.query_row(
                "SELECT COUNT(*) FROM task_entries WHERE task_id=?1",
                [id],
                |r| r.get(0),
            )?;
            let mut stmt = conn.prepare("SELECT path,size,modified_ms,directory FROM task_entries WHERE task_id=?1 ORDER BY index_no LIMIT ?2 OFFSET ?3")?;
            let rows = stmt.query_map(rusqlite::params![id, limit as i64, offset as i64], |r| {
                Ok(FileEntry {
                    path: r.get(0)?,
                    size: r.get::<_, i64>(1)? as u64,
                    modified_ms: r.get::<_, i64>(2)? as u64,
                    directory: r.get::<_, i64>(3)? != 0,
                })
            })?;
            let mut entries = Vec::new();
            for row in rows {
                entries.push(row?);
            }
            Ok((entries, total as usize))
        })
    }
    /// 缓存中转相关标量：用于触发与完成判定，O(1) 且不加载明细。
    pub(crate) fn relay_state(&self, id: &str) -> AppResult<RelayState> {
        let map = locks::lock(&self.tasks);
        let task = map
            .get(id)
            .ok_or_else(|| AppError::NotFound("任务不存在".into()))?;
        Ok(RelayState {
            status: task.status.clone(),
            use_cache: task.use_cache,
            error: task.error.clone(),
            completed_count: task.completed_count,
            chunk_count: task.chunk_count,
            completed_bytes: task.completed_bytes,
            relayed_total: task.relayed_total,
            relay_target_bytes: task.relay_target_bytes,
        })
    }
    /// 转写任务需要的只读计划（每文件条目、块基址、目标路径），一次取齐。
    /// 已转写进度不在此缓存，转写循环按需用 `relayed_at` 读取，避免每轮克隆大数组。
    pub(crate) fn relay_plan(&self, id: &str) -> AppResult<RelayPlan> {
        // bulk 明细在锁外载入（见 ensure_bulk），避免读几十万行时占住任务锁。
        self.ensure_bulk(id)?;
        let mut map = locks::lock(&self.tasks);
        self.load_bulk_locked(&mut map, id)?;
        let task = map
            .get(id)
            .ok_or_else(|| AppError::NotFound("任务不存在".into()))?;
        let mut bases = Vec::with_capacity(task.manifest.entries.len());
        let mut acc = 0usize;
        for entry in &task.manifest.entries {
            bases.push(acc);
            if !entry.directory {
                acc += entry.size.div_ceil(task.chunk_size) as usize;
            }
        }
        Ok(RelayPlan {
            use_cache: task.use_cache,
            entries: task.manifest.entries.clone(),
            bases,
            chunk_size: task.chunk_size,
            targets: task.targets.clone(),
        })
    }
    pub(crate) fn relayed_at(&self, id: &str, file: usize) -> u64 {
        self.tasks
            .lock_or_recover()
            .get(id)
            .and_then(|t| t.relayed.get(file).copied())
            .unwrap_or(0)
    }
    pub(crate) fn is_completed(&self, id: &str, index: usize) -> bool {
        self.tasks
            .lock_or_recover()
            .get(id)
            .map(|t| t.completed.contains(&index))
            .unwrap_or(false)
    }
    pub(crate) fn is_skipped(&self, id: &str, file: usize) -> bool {
        self.tasks
            .lock_or_recover()
            .get(id)
            .map(|t| t.skipped_files.contains(&file))
            .unwrap_or(false)
    }
    /// 是否启用缓存盘中转（不加载明细）。
    pub(crate) fn use_cache_of(&self, id: &str) -> AppResult<bool> {
        self.tasks
            .lock_or_recover()
            .get(id)
            .map(|t| t.use_cache)
            .ok_or_else(|| AppError::NotFound("任务不存在".into()))
    }
    /// 是否开启 Chunk 校验（不加载明细）。
    pub(crate) fn verify_hash_of(&self, id: &str) -> AppResult<bool> {
        self.tasks
            .lock_or_recover()
            .get(id)
            .map(|t| t.verify_hash)
            .ok_or_else(|| AppError::NotFound("任务不存在".into()))
    }
    /// 该文件的写入计划：`(下一个待写偏移, 文件大小, 是否缓存模式)`。
    /// 取「该文件第一个未完成块」的偏移作为续写起点，供直写/缓存缓冲初始化复用。
    pub(crate) fn file_write_plan(&self, id: &str, file: usize) -> AppResult<(u64, u64, bool)> {
        // bulk 明细在锁外载入（见 ensure_bulk），避免读几十万行时占住任务锁。
        self.ensure_bulk(id)?;
        let mut map = locks::lock(&self.tasks);
        self.load_bulk_locked(&mut map, id)?;
        let task = map
            .get(id)
            .ok_or_else(|| AppError::NotFound("任务不存在".into()))?;
        let entry = task
            .manifest
            .entries
            .get(file)
            .ok_or_else(|| AppError::BadRequest("文件编号非法".into()))?;
        let base = task.manifest.file_base(file);
        let count = if entry.directory {
            0
        } else {
            entry.size.div_ceil(task.chunk_size) as usize
        };
        let mut next_offset = entry.size;
        for i in 0..count {
            if !task.completed.contains(&(base + i)) {
                next_offset = i as u64 * task.chunk_size;
                break;
            }
        }
        Ok((next_offset, entry.size, task.use_cache))
    }
    pub(crate) fn chunk_at(&self, id: &str, index: usize) -> AppResult<Option<Chunk>> {
        // bulk 明细在锁外载入（见 ensure_bulk），避免读几十万行时占住任务锁。
        self.ensure_bulk(id)?;
        let mut map = locks::lock(&self.tasks);
        self.load_bulk_locked(&mut map, id)?;
        let task = map
            .get(id)
            .ok_or_else(|| AppError::NotFound("任务不存在".into()))?;
        Ok(task.manifest.chunk(index))
    }
    /// 任务的目标暂存目录（只用标量字段，O(1)）。
    pub(crate) fn stage_dir(&self, id: &str) -> AppResult<PathBuf> {
        let map = locks::lock(&self.tasks);
        let task = map
            .get(id)
            .ok_or_else(|| AppError::NotFound("任务不存在".into()))?;
        paths::stage(task)
    }
    /// 状态与已完成块集合（供 `Response::State` 使用）：区间编码，避免每次
    /// 轮询回传整份块号列表。
    pub(crate) fn status_and_completed(
        &self,
        id: &str,
    ) -> AppResult<(String, crate::network::protocol::CompletedRanges)> {
        self.ensure_bulk(id)?;
        let map = locks::lock(&self.tasks);
        let task = map
            .get(id)
            .ok_or_else(|| AppError::NotFound("任务不存在".into()))?;
        Ok((
            task.status.clone(),
            crate::network::protocol::CompletedRanges::from_sorted(task.completed.iter().copied()),
        ))
    }
    /// 状态流转为 transferring（幂等：无变化则跳过写库）。
    pub(crate) fn mark_transferring(&self, id: &str, transport: &str) -> AppResult<()> {
        let json = {
            let mut map = locks::lock(&self.tasks);
            let task = map
                .get_mut(id)
                .ok_or_else(|| AppError::NotFound("任务不存在".into()))?;
            if task.status == "transferring" && task.transport.as_deref() == Some(transport) {
                return Ok(());
            }
            task.status = "transferring".into();
            task.transport = Some(transport.into());
            task.updated_at = chrono::Utc::now().to_rfc3339();
            serde_json::to_string(&*task)?
        };
        self.app.db.with_conn(|conn| {
            conn.execute(
                "UPDATE transfer_tasks SET record=?1 WHERE id=?2",
                rusqlite::params![json, id],
            )?;
            Ok(())
        })
    }
    pub(crate) fn edit(
        &self,
        id: &str,
        change: impl FnOnce(&mut TaskRecord),
    ) -> AppResult<TaskRecord> {
        // bulk 明细在锁外载入（见 ensure_bulk），避免读几十万行时占住任务锁。
        self.ensure_bulk(id)?;
        let mut map = locks::lock(&self.tasks);
        // `edit` 会克隆整份任务，因此先确保运行期 bulk 已载入（首次访问读一次库并缓存）。
        // 热路径（每块/每轮巡检）不得调用本方法，请改用下面的增量写入方法。
        self.load_bulk_locked(&mut map, id)?;
        let mut task = map
            .get(id)
            .cloned()
            .ok_or_else(|| AppError::NotFound("任务不存在".into()))?;
        change(&mut task);
        self.persist(&task)?;
        map.insert(id.to_string(), task.clone());
        Ok(task)
    }
    /// 取（必要时新建）某任务的运行期资源。任务结束时用 `release_runtime` 释放。
    pub(crate) fn runtime(&self, id: &str) -> Arc<TaskRuntime> {
        let mut map = locks::lock(&self.runtime);
        if let Some(runtime) = map.get(id) {
            return runtime.clone();
        }
        let runtime = Arc::new(TaskRuntime::default());
        map.insert(id.to_string(), runtime.clone());
        runtime
    }
    /// 已存在则返回；不存在不创建（用于只读查询，避免为已结束任务留下垃圾条目）。
    pub(crate) fn runtime_if_exists(&self, id: &str) -> Option<Arc<TaskRuntime>> {
        locks::lock(&self.runtime).get(id).cloned()
    }
    /// 任务结束：释放该任务的全部运行期资源（门锁、句柄、标志）。
    pub(crate) fn release_runtime(&self, id: &str) {
        locks::lock(&self.runtime).remove(id);
    }
    /// 进程级关闭：丢弃全部任务的未落盘块登记（不记录完成，续传时重传）。
    pub(crate) fn clear_all_write_pending(&self) {
        let runtimes = locks::lock(&self.runtime)
            .values()
            .cloned()
            .collect::<Vec<_>>();
        for runtime in runtimes {
            locks::lock(&runtime.write_pending).clear();
        }
    }
    pub(crate) fn gate(&self, id: &str) -> Arc<AsyncMutex<()>> {
        self.runtime(id).gate.clone()
    }
    /// 标记本任务开始提交；已有提交在进行时返回 false（对端稍后重试提交）。
    pub(crate) fn begin_commit(&self, id: &str) -> bool {
        !self.runtime(id).committing.swap(true, Ordering::SeqCst)
    }
    pub(crate) fn finish_commit(&self, id: &str) {
        if let Some(runtime) = self.runtime_if_exists(id) {
            runtime.committing.store(false, Ordering::SeqCst);
        }
    }
    pub async fn create_outgoing(
        &self,
        peer_id: String,
        peer_name: String,
        paths: Vec<String>,
        verify_hash: bool,
        allow_public: bool,
    ) -> AppResult<String> {
        // 真正的发送马上就会流式扫描同一批目录：结束后台预览扫描，
        // 避免两条扫描在同一块磁盘上互相抢 I/O（发送进度由任务列表展示）。
        TransferService::preview_cancel();
        // Establish the task and connect without recursively scanning the selected
        // directories. Only the selected roots are stat'ed before the first Offer.
        let mut scanner =
            tokio::task::spawn_blocking(move || source::SourceScanner::new(paths))
                .await
                .map_err(|e| AppError::Other(e.to_string()))??;
        let sources = scanner.roots().to_vec();
        let root_entries = scanner.next_batch(sources.len().max(1))?;
        if root_entries.is_empty() {
            return Err(AppError::BadRequest("请选择至少一个普通文件或目录".into()));
        }
        let manifest = Manifest::new(root_entries, CHUNK_SIZE, verify_hash);
        let wire_id = random_id();
        let id = format!("out-{wire_id}");
        let now = chrono::Utc::now().to_rfc3339();
        let top_level_count = sources.len();
        let mut task = TaskRecord {
            id: id.clone(),
            wire_id,
            peer_id,
            peer_name,
            direction: "send".into(),
            status: "queued".into(),
            sources,
            destination: None,
            conflict: ConflictPolicy::Rename,
            transport: None,
            error: None,
            events: vec![],
            updated_at: now.clone(),
            created_at: now,
            allow_public,
            use_cache: false,
            chunk_size: 0,
            verify_hash: false,
            file_count: 0,
            directory_count: 0,
            total_size: 0,
            chunk_count: 0,
            top_level_count,
            completed_count: 0,
            completed_bytes: 0,
            relayed_total: 0,
            relay_target_bytes: 0,
            skipped_bytes: 0,
            skipped_paths: vec![],
            digest: String::new(),
            top_level_fingerprint: String::new(),
            scan_last_seq: 0,
            scan_last_path: String::new(),
            scan_complete: false,
            manifest: Manifest::default(),
            completed: BTreeSet::new(),
            targets: vec![],
            relayed: vec![],
            skipped_files: BTreeSet::new(),
        };
        task.attach_manifest(manifest);
        task.top_level_fingerprint = source::top_level_fingerprint(&task.sources);
        task.scan_last_seq = task.manifest.entries.len();
        task.scan_last_path = task
            .manifest
            .entries
            .last()
            .map(|entry| entry.path.clone())
            .unwrap_or_default();
        // 顶层条目已经确定：先把计数与总量落进任务记录，否则在扫描结束前
        // 界面只能显示"0 个文件"、没有总量、进度条恒为 0。
        task.file_count = task.manifest.entries.len();
        task.directory_count = task
            .manifest
            .entries
            .iter()
            .filter(|entry| entry.directory)
            .count();
        task.total_size = task.manifest.total();
        task.chunk_count = task.manifest.chunk_count();
        self.insert(task)?;
        Ok(id)
    }
    pub async fn decide(
        &self,
        id: &str,
        accepted: bool,
        destination: Option<String>,
        conflict: ConflictPolicy,
        selected: Option<Vec<String>>,
    ) -> AppResult<()> {
        let gate = self.gate(id);
        let _guard = gate.lock().await;
        let task = self.record(id)?;
        if task.direction != "receive" || task.status != "waiting" {
            return Err(AppError::Conflict("接收请求已处理".into()));
        }
        if !accepted {
            self.edit(id, |t| {
                t.status = "rejected".into();
                t.event("接收方拒绝");
            })?;
            return Ok(());
        }
        let folder = destination.unwrap_or_else(|| self.default_destination());
        // 目标路径解析是同步文件系统调用（网络盘上可能明显阻塞），放到阻塞线程池。
        let root = tokio::task::spawn_blocking(move || paths::destination(&folder))
            .await
            .map_err(|error| AppError::Other(error.to_string()))??;
        let targets = paths::plan_targets(&task, &root, &conflict, selected.as_deref())?;
        if targets.iter().all(Option::is_none) {
            return Err(AppError::BadRequest("未选中可接收的文件".into()));
        }
        let required: u64 = task
            .manifest
            .entries
            .iter()
            .zip(&targets)
            .filter(|(_, p)| p.is_some())
            .map(|(e, _)| e.size)
            .sum();
        let reserved: u64 = self
            .tasks
            .lock_or_recover()
            .values()
            .filter(|t| {
                t.direction == "receive"
                    && t.destination.as_deref() == Some(root.to_string_lossy().as_ref())
                    && ["accepted", "transferring", "paused"].contains(&t.status.as_str())
            })
            .map(|t| t.total_size.saturating_sub(t.completed_bytes))
            .sum();
        let free = {
            let root = root.clone();
            tokio::task::spawn_blocking(move || Ok::<_, AppError>(fs2::available_space(&root)?))
                .await
                .map_err(|error| AppError::Other(error.to_string()))??
        };
        if free < required.saturating_add(reserved) {
            return Err(AppError::Conflict(
                "接收目录空间不足（含其他任务预留）".into(),
            ));
        }
        let skipped = task
            .manifest
            .chunks()
            .iter()
            .enumerate()
            .filter(|(_, c)| targets[c.file].is_none())
            .map(|(i, _)| i)
            .collect();
        // 缓存盘中转：缓存可用且与目标盘不同卷时启用（同卷会加倍 I/O，自动回退直写）。
        // 同卷判断要查询卷信息，属于同步文件系统调用，同样放到阻塞线程池。
        let use_cache = match self.cache_root.clone() {
            Some(cache) => {
                let root_probe = root.clone();
                tokio::task::spawn_blocking(move || !paths::same_volume(&cache, &root_probe))
                    .await
                    .unwrap_or(false)
            }
            None => false,
        };
        self.edit(id, |t| {
            t.targets = targets;
            t.destination = Some(root.to_string_lossy().into_owned());
            t.conflict = conflict;
            // 未选中的文件：其全部块标记为已完成（不参与传输），并据此刻度推进汇总标量，
            // 语义与拆分前一致（未选中的字节同时计入 total 与 completed）。
            t.completed = skipped;
            t.completed_count = t.completed.len();
            t.completed_bytes = t
                .completed
                .iter()
                .filter_map(|i| t.manifest.chunk(*i))
                .filter(|c| !t.skipped_files.contains(&c.file))
                .map(|c| c.length as u64)
                .sum();
            t.relay_target_bytes = t
                .manifest
                .entries
                .iter()
                .enumerate()
                .filter(|(i, e)| {
                    !e.directory && e.size > 0 && t.targets.get(*i).is_some_and(Option::is_some)
                })
                .map(|(_, e)| e.size)
                .sum();
            t.status = "accepted".into();
            t.use_cache = use_cache;
            t.relayed = vec![0; t.manifest.entries.len()];
            t.relayed_total = 0;
            t.event(if use_cache {
                "接收方已确认保存位置和冲突策略（缓存盘中转已启用）"
            } else {
                "接收方已确认保存位置和冲突策略"
            });
        })?;
        // 把目标路径与完成集合（含未选中文件的块）整体落明细表（一次性）。
        self.save_decision(id)?;
        Ok(())
    }
    pub async fn action(&self, id: &str, action: &str) -> AppResult<()> {
        let gate = self.gate(id);
        let _guard = gate.lock().await;
        let task = self.record(id)?;
        if ["resume", "retry"].contains(&action)
            && (task.sources.iter().any(|root| root.legacy_format) || task.chunk_size == 0)
        {
            return Err(AppError::Conflict(
                "该任务来自旧版本，当前版本无法继续传输；任务记录及原始路径信息仍予以保留。".into(),
            ));
        }
        if ["completed", "cancelled", "rejected"].contains(&task.status.as_str()) {
            return Err(AppError::Conflict("任务已结束".into()));
        }
        let state = match action {
            "pause" => "paused",
            "cancel" => "cancelled",
            "resume" | "retry" => {
                if task.direction == "send" {
                    "queued"
                } else if task.destination.is_some() {
                    "accepted"
                } else {
                    "waiting"
                }
            }
            _ => return Err(AppError::BadRequest("未知任务操作".into())),
        };
        self.edit(id, |t| {
            t.status = state.into();
            t.error = None;
            t.event(format!("用户操作：{action}"));
        })?;
        if action == "cancel" && task.direction == "receive" {
            // Cancellation is terminal: discard staged chunks immediately.
            // Paused/interrupted tasks keep their partials for resume.
            // Cleanup is best effort: cancellation must still complete when a
            // removable destination has already disappeared or become
            // unavailable. The next startup cleanup pass can retry it.
            self.cleanup_receive(&task).await;
        }
        Ok(())
    }
    /// 取消/删除接收任务的完整清理顺序：
    /// 停接收（状态已置 cancelled）→ 停转写 → 关闭所有句柄 → 删缓存目录与目标暂存目录。
    /// **原文件绝不触碰**；删除失败登记「待清理」，下次启动重试。
    async fn cleanup_receive(&self, task: &TaskRecord) {
        let id = task.id.clone();
        let runtime = self.runtime(&id);
        // 丢弃写入队列中该任务的未落盘登记（不记录完成，不会再写盘）。
        locks::lock(&runtime.write_pending).clear();
        // 等转写任务退出：它退出时会关闭目标盘句柄（Windows 上句柄占用会导致删不掉）。
        for _ in 0..50 {
            if !runtime.relay_active.load(Ordering::SeqCst) {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        // 关闭直写模式复用的目标盘句柄。
        self.release_files(&id);
        // 删缓存目录。
        if let Some(cache_root) = self.cache_root.as_ref() {
            let dir = paths::cache_task(cache_root, &id);
            if dir.exists() {
                if let Err(err) = std::fs::remove_dir_all(&dir) {
                    tracing::warn!(dir = %dir.display(), "取消时清理缓存失败：{err}");
                    relay::record_pending_cleanup(self, &dir);
                }
            }
        }
        // 删目标盘暂存目录（含临时文件）；用户原文件不在其中，绝不触碰。
        if let Ok(stage) = paths::stage(task) {
            if stage.exists() {
                if let Err(err) = std::fs::remove_dir_all(&stage) {
                    tracing::warn!(dir = %stage.display(), "取消时清理暂存目录失败：{err}");
                    relay::record_pending_cleanup(self, &stage);
                }
            }
        }
        // 任务已终态：整体释放运行期资源（门锁、句柄、标志）。
        self.release_runtime(&id);
    }
    /// 释放某任务在 `receive_files` 中登记的目标盘句柄。
    ///
    /// 直写模式复用句柄会一直留在该 map 里，若不显式释放，任务结束后句柄仍指向
    /// 已 rename 到最终位置的接收文件，进程常驻托盘期间一直被占用。
    /// 只在任务进入终态（`completed`/`cancelled`/`rejected`）或写盘失败时调用；
    /// `paused` 保留句柄，续传时复用（见 `write_queue::open_buffer`）。
    pub(crate) fn release_files(&self, id: &str) {
        if let Some(runtime) = self.runtime_if_exists(id) {
            locks::lock(&runtime.receive_files).clear();
        }
    }
    /// 发送端按文件取（或建立）读句柄。上限固定：超出即清掉本任务的旧句柄，
    /// 代价只是重新 open，避免长任务持续累积句柄。
    pub(crate) async fn send_handle(
        &self,
        task_id: &str,
        file: usize,
        roots: &[model::SourceRoot],
        entry: &model::FileEntry,
    ) -> AppResult<Arc<AsyncMutex<tokio::fs::File>>> {
        let runtime = self.runtime(task_id);
        if let Some(handle) = locks::lock(&runtime.send_files).get(&file).cloned() {
            return Ok(handle);
        }
        let path = source::source_path_from_roots(roots, &entry.path)?;
        let opened = Arc::new(AsyncMutex::new(tokio::fs::File::open(&path).await?));
        let mut map = locks::lock(&runtime.send_files);
        if map.len() >= SEND_HANDLES_LIMIT {
            map.clear();
        }
        map.insert(file, opened.clone());
        Ok(opened)
    }
    /// 发送端读取某个文件的某个分块（复用句柄，保持顺序读）。
    pub(crate) async fn read_chunk_entry(
        &self,
        task: &TaskRecord,
        file: usize,
        entry: &model::FileEntry,
        offset: u64,
        length: usize,
    ) -> AppResult<Vec<u8>> {
        let handle = self
            .send_handle(&task.id, file, &task.sources, entry)
            .await
            .ok();
        source::read_entry_chunk_with(handle.as_ref(), &task.sources, entry, offset, length).await
    }
    /// 任务结束时释放发送端读句柄。
    pub(crate) fn release_send_files(&self, id: &str) {
        if let Some(runtime) = self.runtime_if_exists(id) {
            locks::lock(&runtime.send_files).clear();
        }
    }
    /// 发送端读取一个分块：优先复用缓存句柄（顺序读），失败则退回单次 open。
    pub(crate) async fn read_chunk_cached(
        &self,
        task: &TaskRecord,
        chunk: model::Chunk,
    ) -> AppResult<Vec<u8>> {
        let entry = task
            .manifest
            .entries
            .get(chunk.file)
            .ok_or_else(|| AppError::NotFound("分块所属文件不存在".into()))?
            .clone();
        let handle = self
            .send_handle(&task.id, chunk.file, &task.sources, &entry)
            .await
            .ok();
        source::read_entry_chunk_with(
            handle.as_ref(),
            &task.sources,
            &entry,
            chunk.offset,
            chunk.length,
        )
        .await
    }
    pub fn default_destination(&self) -> String {
        self.app
            .get_setting("receive_directory")
            .ok()
            .flatten()
            .unwrap_or_else(|| {
                #[cfg(target_os = "android")]
                let download = self.app.paths.data_dir.clone();
                #[cfg(not(target_os = "android"))]
                let download =
                    dirs::download_dir().unwrap_or_else(|| self.app.paths.data_dir.clone());
                download.join("NodeSend").to_string_lossy().into_owned()
            })
    }
    pub fn is_active(&self, id: &str) -> bool {
        self.runtime_if_exists(id)
            .is_some_and(|runtime| runtime.active.load(Ordering::SeqCst))
    }
    pub fn recoverable(&self) -> Vec<String> {
        self.tasks
            .lock_or_recover()
            .values()
            .filter(|t| t.direction == "send" && ["queued"].contains(&t.status.as_str()))
            .map(|t| t.id.clone())
            .collect()
    }
    /// 发送前预览：立即返回当前快照，并在后台继续扫描（不阻塞发送）。
    ///
    /// 同一批路径重复调用不会重启扫描；路径变化时取消旧扫描、启动新的。
    pub fn preview_start(paths: Vec<String>) -> AppResult<PreviewStatus> {
        let key = paths.join("\n");
        {
            let slot = locks::lock(&PREVIEW_JOB);
            if let Some(job) = slot.as_ref() {
                if job.key == key {
                    return Ok(locks::lock(&job.status).clone());
                }
            }
        }
        TransferService::preview_cancel();
        if paths.is_empty() {
            return Ok(PreviewStatus::default());
        }
        let scanner = source::SourceScanner::new(paths)?;
        let roots = scanner.roots().to_vec();
        let status = Arc::new(Mutex::new(PreviewStatus {
            scanning: true,
            exceeded: false,
            top_level_count: roots.len(),
            file_count: 0,
            directory_count: 0,
            total_size: 0,
            chunk_count: 0,
            chunk_size: CHUNK_SIZE,
        }));
        let cancel = tokio_util::sync::CancellationToken::new();
        let worker_status = status.clone();
        let worker_cancel = cancel.clone();
        // 用独立线程而不是阻塞线程池：预览可能持续几分钟，占用阻塞池会挤占
        // 数据库/提交等真正的阻塞任务；这里也让它不依赖 Tokio 运行时上下文。
        std::thread::Builder::new()
            .name("nodesend-preview-scan".into())
            .spawn(move || preview_scan(scanner, worker_status, worker_cancel, CHUNK_SIZE))
            .map_err(|error| AppError::Other(format!("无法启动预览扫描线程：{error}")))?;
        let snapshot = locks::lock(&status).clone();
        *locks::lock(&PREVIEW_JOB) = Some(PreviewJob {
            key,
            status,
            cancel,
        });
        Ok(snapshot)
    }

    /// 当前预览快照（界面轮询）。没有进行中的预览时返回 `None`。
    pub fn preview_status() -> Option<PreviewStatus> {
        locks::lock(&PREVIEW_JOB)
            .as_ref()
            .map(|job| locks::lock(&job.status).clone())
    }

    /// 结束预览扫描（选择清空或发送已开始：避免与真正的发送扫描抢同一块磁盘）。
    pub fn preview_cancel() {
        if let Some(job) = locks::lock(&PREVIEW_JOB).take() {
            job.cancel.cancel();
        }
    }
}
/// 选择目录后的**后台预览扫描**快照（IPC 用）。
///
/// 预览只服务界面：选择后立即返回"正在扫描"，后台持续把整个选择扫完，
/// 期间不断更新计数。扫描过程中一旦超过 [`crate::tuning::PREVIEW_SCAN_LIMIT`]
/// 就置 `exceeded`，界面据此把提示改成"文件数量过多 · 发送时逐步扫描"——
/// 但**扫描不会停止**；扫完后 `scanning = false`，界面直接显示最终的文件数与总大小。
/// 预览不参与发送：发送端有自己独立的流式扫描（见 `send::stream_scan`）。
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct PreviewStatus {
    /// 是否仍在扫描。
    pub scanning: bool,
    /// 扫描过程中是否已超过"文件数量过多"的提示阈值（不影响继续扫描）。
    pub exceeded: bool,
    pub top_level_count: usize,
    pub file_count: usize,
    pub directory_count: usize,
    pub total_size: u64,
    pub chunk_count: usize,
    pub chunk_size: u64,
}

struct PreviewJob {
    /// 产生该扫描的路径集合（拼接后比较，路径变化即重启扫描）。
    key: String,
    status: Arc<Mutex<PreviewStatus>>,
    cancel: tokio_util::sync::CancellationToken,
}

/// 当前预览扫描任务。预览天然是"当前选择"的单例，因此进程内只保留一个。
static PREVIEW_JOB: std::sync::LazyLock<Mutex<Option<PreviewJob>>> =
    std::sync::LazyLock::new(|| Mutex::new(None));

/// 后台预览扫描：扫完整个选择，边扫边更新快照。
///
/// 超过提示阈值只置 `exceeded`，**继续扫描**（"扫到一万时提示但继续扫描"）。
fn preview_scan(
    mut scanner: source::SourceScanner,
    status: Arc<Mutex<PreviewStatus>>,
    cancel: tokio_util::sync::CancellationToken,
    chunk_size: u64,
) {
    let (mut files, mut directories, mut total, mut chunks) = (0usize, 0usize, 0u64, 0usize);
    loop {
        if cancel.is_cancelled() {
            return;
        }
        let batch = match scanner.next_batch(crate::tuning::PREVIEW_BATCH) {
            Ok(batch) => batch,
            Err(error) => {
                tracing::warn!("预览扫描中断：{error}");
                break;
            }
        };
        if batch.is_empty() {
            break;
        }
        for entry in &batch {
            if entry.directory {
                directories += 1;
            } else {
                files += 1;
                total = total.saturating_add(entry.size);
                chunks += entry.size.div_ceil(chunk_size) as usize;
            }
        }
        let mut snapshot = locks::lock(&status);
        snapshot.file_count = files;
        snapshot.directory_count = directories;
        snapshot.total_size = total;
        snapshot.chunk_count = chunks;
        snapshot.exceeded = files + directories > crate::tuning::PREVIEW_SCAN_LIMIT;
    }
    let mut snapshot = locks::lock(&status);
    snapshot.scanning = false;
    snapshot.file_count = files;
    snapshot.directory_count = directories;
    snapshot.total_size = total;
    snapshot.chunk_count = chunks;
    snapshot.exceeded = files + directories > crate::tuning::PREVIEW_SCAN_LIMIT;
}
#[cfg(test)]
mod preview_tests {
    use super::*;

    /// 预览必须"立即返回正在扫描"，并在后台扫完后给出最终计数；
    /// 超过提示阈值只置 `exceeded`，扫描继续（不提前停止）。
    #[test]
    fn preview_scans_in_background_to_completion() {
        let base = std::env::temp_dir().join(format!(
            "nodesend-preview-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let root = base.join("folder");
        std::fs::create_dir_all(root.join("sub")).unwrap();
        std::fs::write(root.join("a.txt"), b"aaa").unwrap();
        std::fs::write(root.join("b.txt"), b"bb").unwrap();
        std::fs::write(root.join("sub").join("c.txt"), b"c").unwrap();

        let first = TransferService::preview_start(vec![root.to_string_lossy().into_owned()])
            .expect("preview should start");
        assert!(first.scanning, "选择后应立即返回扫描中");
        assert_eq!(first.top_level_count, 1);

        let mut last = first;
        for _ in 0..500 {
            let status = TransferService::preview_status().expect("preview status");
            if !status.scanning {
                last = status;
                break;
            }
            last = status;
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(!last.scanning, "预览应在后台扫完");
        assert_eq!(last.file_count, 3, "三个文件");
        assert_eq!(last.directory_count, 2, "根目录 + 子目录");
        assert_eq!(last.total_size, 6, "aaa + bb + c");
        assert!(!last.exceeded);
        TransferService::preview_cancel();
        let _ = std::fs::remove_dir_all(&base);
    }
}