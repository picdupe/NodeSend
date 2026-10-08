//! Global native TransferService. Tasks own manifests and durable chunk state.
pub mod model;
pub mod paths;
mod receive;
mod send;
mod source;
use crate::{
    error::{AppError, AppResult},
    service::AppService,
};
use model::{random_id, ConflictPolicy, Manifest, TaskRecord, TaskSnapshot};
use std::{
    collections::{BTreeSet, HashMap, HashSet},
    sync::{Arc, Mutex},
};
use tokio::sync::{Mutex as AsyncMutex, Semaphore};

pub struct TransferService {
    pub(crate) app: Arc<AppService>,
    tasks: Mutex<HashMap<String, TaskRecord>>,
    gates: Mutex<HashMap<String, Arc<AsyncMutex<()>>>>,
    active: Mutex<HashSet<String>>,
    scheduler: Arc<Semaphore>,
    pub(crate) shutdown: tokio_util::sync::CancellationToken,
    pub(crate) receive_files: Mutex<HashMap<(String, usize), Arc<AsyncMutex<tokio::fs::File>>>>,
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
        Ok(Arc::new(Self {
            app,
            tasks: Mutex::new(tasks),
            gates: Mutex::new(HashMap::new()),
            active: Mutex::new(HashSet::new()),
            scheduler: Arc::new(Semaphore::new(4)),
            shutdown: tokio_util::sync::CancellationToken::new(),
            receive_files: Mutex::new(HashMap::new()),
        }))
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
        self.edit(id, |t| {
            t.targets = targets;
            t.destination = Some(root.to_string_lossy().into_owned());
            t.conflict = conflict;
            t.completed = skipped;
            t.status = "accepted".into();
            t.event("接收方已确认保存位置和冲突策略");
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
            let _ = paths::cleanup_stage(&task);
        }
        Ok(())
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
