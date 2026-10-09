//! Global native TransferService. Tasks own manifests and durable chunk state.
pub mod model;
pub mod paths;
mod receive;
mod relay;
mod send;
mod source;
mod write_queue;
use crate::{
    error::{AppError, AppResult},
    service::AppService,
};
use model::{random_id, ConflictPolicy, Manifest, TaskRecord, TaskSnapshot};
use std::{
    collections::{BTreeSet, HashMap, HashSet},
    path::PathBuf,
    sync::{Arc, Mutex},
};
use tokio::sync::{mpsc, Mutex as AsyncMutex, Semaphore};

pub struct TransferService {
    pub(crate) app: Arc<AppService>,
    tasks: Mutex<HashMap<String, TaskRecord>>,
    gates: Mutex<HashMap<String, Arc<AsyncMutex<()>>>>,
    active: Mutex<HashSet<String>>,
    scheduler: Arc<Semaphore>,
    pub(crate) shutdown: tokio_util::sync::CancellationToken,
    pub(crate) receive_files: Mutex<HashMap<(String, usize), Arc<AsyncMutex<tokio::fs::File>>>>,
    /// 接收写入队列（有界，满时背压接收路径）。见 `write_queue` 模块。
    pub(crate) write_tx: mpsc::Sender<write_queue::PendingChunk>,
    /// 已入队但尚未落盘并记录的块 `(task_id, chunk_index)`。
    /// 同时用于：接收路径去重、`commit` 等待本任务排空。
    pub(crate) write_pending: Mutex<HashSet<(String, usize)>>,
    /// 缓存盘中转目录（`None` 表示缓存不可用，全部任务自动回退直写）。见 `relay` 模块。
    pub(crate) cache_root: Option<PathBuf>,
    /// 正在运行转写任务的任务 ID 集合（用于取消时等待句柄释放）。
    pub(crate) relay_active: Mutex<HashSet<String>>,
}
impl TransferService {
    pub fn new(app: Arc<AppService>) -> AppResult<Arc<Self>> {
        let records = {
            let conn = app.db.conn();
            let mut stmt = conn.prepare("SELECT record FROM transfer_tasks")?;
            let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
            rows.collect::<Result<Vec<_>, _>>()?
        };
        let mut tasks = HashMap::new();
        for json in records {
            let mut task: TaskRecord = serde_json::from_str(&json)?;
            if ["connecting", "transferring", "interrupted", "accepted", "queued"].contains(&task.status.as_str()) {
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
            gates: Mutex::new(HashMap::new()),
            active: Mutex::new(HashSet::new()),
            scheduler: Arc::new(Semaphore::new(4)),
            shutdown: tokio_util::sync::CancellationToken::new(),
            receive_files: Mutex::new(HashMap::new()),
            write_tx,
            write_pending: Mutex::new(HashSet::new()),
            cache_root,
            relay_active: Mutex::new(HashSet::new()),
        });
        write_queue::spawn(Arc::clone(&service), write_rx);
        relay::spawn(Arc::clone(&service));
        relay::sweep(&service);
        Ok(service)
    }
    /// 全部接收任务的 ID（供转写调度器扫描）。
    pub(crate) fn receive_ids(&self) -> Vec<String> {
        self.tasks
            .lock()
            .unwrap()
            .values()
            .filter(|t| t.direction == "receive")
            .map(|t| t.id.clone())
            .collect()
    }
    pub fn snapshots(&self) -> Vec<TaskSnapshot> {
        let mut tasks = self
            .tasks
            .lock()
            .unwrap()
            .values()
            .map(TaskRecord::snapshot)
            .collect::<Vec<_>>();
        tasks.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        tasks
    }
    pub fn record(&self, id: &str) -> AppResult<TaskRecord> {
        self.tasks
            .lock()
            .unwrap()
            .get(id)
            .cloned()
            .ok_or_else(|| AppError::NotFound("传输任务不存在".into()))
    }
    pub(crate) fn insert(&self, task: TaskRecord) -> AppResult<()> {
        let mut map = self.tasks.lock().unwrap();
        if map.contains_key(&task.id) {
            return Err(AppError::Conflict("任务已经存在".into()));
        }
        self.persist(&task)?;
        map.insert(task.id.clone(), task);
        Ok(())
    }
    fn persist(&self, task: &TaskRecord) -> AppResult<()> {
        self.app.db.conn().execute("INSERT INTO transfer_tasks(id,record) VALUES(?1,?2) ON CONFLICT(id) DO UPDATE SET record=excluded.record",rusqlite::params![task.id,serde_json::to_string(task)?])?;
        Ok(())
    }
    pub(crate) fn edit(
        &self,
        id: &str,
        change: impl FnOnce(&mut TaskRecord),
    ) -> AppResult<TaskRecord> {
        let mut map = self.tasks.lock().unwrap();
        let mut task = map
            .get(id)
            .cloned()
            .ok_or_else(|| AppError::NotFound("任务不存在".into()))?;
        change(&mut task);
        self.persist(&task)?;
        map.insert(id.to_string(), task.clone());
        Ok(task)
    }
    pub(crate) fn gate(&self, id: &str) -> Arc<AsyncMutex<()>> {
        self.gates
            .lock()
            .unwrap()
            .entry(id.into())
            .or_insert_with(|| Arc::new(AsyncMutex::new(())))
            .clone()
    }
    pub async fn create_outgoing(
        &self,
        peer_id: String,
        peer_name: String,
        paths: Vec<String>,
        verify_hash: bool,
        allow_public: bool,
    ) -> AppResult<String> {
        let (manifest, sources) =
            tokio::task::spawn_blocking(move || source::collect(paths, verify_hash))
                .await
                .map_err(|e| AppError::Other(e.to_string()))??;
        let wire_id = random_id();
        let id = format!("out-{wire_id}");
        let now = chrono::Utc::now().to_rfc3339();
        self.insert(TaskRecord {
            skipped_files: BTreeSet::new(),
            id: id.clone(),
            wire_id,
            peer_id,
            peer_name,
            direction: "send".into(),
            status: "queued".into(),
            manifest,
            sources,
            targets: vec![],
            destination: None,
            conflict: ConflictPolicy::Rename,
            completed: BTreeSet::new(),
            transport: None,
            error: None,
            events: vec![],
            updated_at: now.clone(),
            created_at: now,
            allow_public,
            use_cache: false,
            relayed: vec![],
        })?;
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
        let root = paths::destination(&folder)?;
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
            .lock()
            .unwrap()
            .values()
            .filter(|t| {
                t.direction == "receive"
                    && t.destination.as_deref() == Some(root.to_string_lossy().as_ref())
                    && ["accepted", "transferring", "paused"].contains(&t.status.as_str())
            })
            .map(|t| {
                t.manifest
                    .total()
                    .saturating_sub(t.snapshot().completed_bytes)
            })
            .sum();
        if fs2::available_space(&root)? < required.saturating_add(reserved) {
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
        let use_cache = self
            .cache_root
            .as_ref()
            .map(|root_cache| !paths::same_volume(root_cache, &root))
            .unwrap_or(false);
        self.edit(id, |t| {
            t.targets = targets;
            t.destination = Some(root.to_string_lossy().into_owned());
            t.conflict = conflict;
            t.completed = skipped;
            t.status = "accepted".into();
            t.use_cache = use_cache;
            t.relayed = vec![0; t.manifest.entries.len()];
            t.event(if use_cache {
                "接收方已确认保存位置和冲突策略（缓存盘中转已启用）"
            } else {
                "接收方已确认保存位置和冲突策略"
            });
        })?;
        Ok(())
    }
    pub async fn action(&self, id: &str, action: &str) -> AppResult<()> {
        let gate = self.gate(id);
        let _guard = gate.lock().await;
        let task = self.record(id)?;
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
        // 丢弃写入队列中该任务的未落盘登记（不记录完成，不会再写盘）。
        self.write_pending.lock().unwrap().retain(|(t, _)| t != &id);
        // 等转写任务退出：它退出时会关闭目标盘句柄（Windows 上句柄占用会导致删不掉）。
        for _ in 0..50 {
            if !self.relay_active.lock().unwrap().contains(&id) {
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
    }
    /// 释放某任务在 `receive_files` 中登记的目标盘句柄。
    ///
    /// 直写模式复用句柄会一直留在该 map 里，若不显式释放，任务结束后句柄仍指向
    /// 已 rename 到最终位置的接收文件，进程常驻托盘期间一直被占用。
    /// 只在任务进入终态（`completed`/`cancelled`/`rejected`）或写盘失败时调用；
    /// `paused` 保留句柄，续传时复用（见 `write_queue::open_buffer`）。
    pub(crate) fn release_files(&self, id: &str) {
        self.receive_files
            .lock()
            .unwrap()
            .retain(|(task, _), _| task != id);
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
        self.active.lock().unwrap().contains(id)
    }
    pub fn recoverable(&self) -> Vec<String> {
        self.tasks
            .lock()
            .unwrap()
            .values()
            .filter(|t| t.direction == "send" && ["queued"].contains(&t.status.as_str()))
            .map(|t| t.id.clone())
            .collect()
    }
    pub async fn manifest_preview(paths: Vec<String>) -> AppResult<Manifest> {
        tokio::task::spawn_blocking(move || source::collect(paths, false).map(|p| p.0))
            .await
            .map_err(|e| AppError::Other(e.to_string()))?
    }
}
