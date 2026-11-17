use super::{browser, peers::Endpoint};
use crate::{
    error::{AppError, AppResult},
    service::AppService,
    transfer::model::{random_id, TaskSnapshot},
};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
};
use tokio_util::sync::CancellationToken;

const PROGRESS_PERSIST_INTERVAL: u64 = 4 * 1024 * 1024;

#[derive(Clone, Serialize, Deserialize)]
struct WebDownloadTask {
    id: String,
    node_id: String,
    peer_name: String,
    endpoint: Endpoint,
    share_id: String,
    path: String,
    token: Option<String>,
    directory: bool,
    expected_size: Option<u64>,
    file_count: usize,
    directory_count: usize,
    total_bytes: u64,
    completed_bytes: u64,
    status: String,
    error: Option<String>,
    destination: String,
    created_at: String,
    updated_at: String,
}

impl WebDownloadTask {
    fn snapshot(&self) -> TaskSnapshot {
        TaskSnapshot {
            source_issue: None,
            abandoned_count: 0,
            warnings: Vec::new(),
            id: self.id.clone(),
            peer_id: self.node_id.clone(),
            file_name: self
                .path
                .rsplit('/')
                .next()
                .unwrap_or(&self.path)
                .to_string(),
            peer_name: self.peer_name.clone(),
            peer_device_type: "desktop".into(),
            direction: "receive".into(),
            status: self.status.clone(),
            scan_complete: true,
            top_level_count: 1,
            file_count: self.file_count.max(usize::from(!self.directory)),
            directory_count: self.directory_count,
            total_bytes: self.expected_size.unwrap_or(self.total_bytes),
            completed_bytes: self.completed_bytes,
            transport: Some("quic".into()),
            error: self.error.clone(),
            destination: Some(self.destination.clone()),
            events: Vec::new(),
            created_at: self.created_at.clone(),
            verify_hash: false,
            android_media_library: false,
            receive_directories: Vec::new(),
        }
    }
}

pub struct WebDownloadManager {
    app: Arc<AppService>,
    tasks: Mutex<HashMap<String, WebDownloadTask>>,
    active: Mutex<HashMap<String, CancellationToken>>,
}

impl WebDownloadManager {
    pub fn new(app: Arc<AppService>) -> AppResult<Arc<Self>> {
        let tasks = app.db.with_conn(|conn| {
            let mut statement = conn.prepare("SELECT record FROM web_download_tasks")?;
            let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
            let mut tasks = HashMap::new();
            for row in rows {
                if let Ok(mut task) = serde_json::from_str::<WebDownloadTask>(&row?) {
                    if task.status == "transferring" || task.status == "queued" {
                        task.status = "interrupted".into();
                    }
                    tasks.insert(task.id.clone(), task);
                }
            }
            Ok(tasks)
        })?;
        let manager = Arc::new(Self {
            app,
            tasks: Mutex::new(tasks),
            active: Mutex::new(HashMap::new()),
        });
        Ok(manager)
    }

    pub fn snapshots(&self) -> Vec<TaskSnapshot> {
        let mut snapshots: Vec<_> = self
            .tasks
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .values()
            .map(WebDownloadTask::snapshot)
            .collect();
        snapshots.sort_by(|left, right| right.created_at.cmp(&left.created_at));
        snapshots
    }

    pub fn contains(&self, id: &str) -> bool {
        self.tasks
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains_key(id)
    }

    pub fn enqueue(
        self: &Arc<Self>,
        node_id: String,
        endpoint: Endpoint,
        share_id: String,
        path: String,
        token: Option<String>,
        directory: bool,
        expected_size: Option<u64>,
    ) -> AppResult<String> {
        let now = chrono::Utc::now().to_rfc3339();
        let task = WebDownloadTask {
            id: format!("web-{}", random_id()),
            node_id: node_id.clone(),
            peer_name: node_id,
            endpoint,
            share_id,
            path,
            token,
            directory,
            expected_size,
            file_count: usize::from(!directory),
            directory_count: usize::from(directory),
            total_bytes: expected_size.unwrap_or_default(),
            completed_bytes: 0,
            status: "queued".into(),
            error: None,
            destination: dirs::download_dir()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned(),
            created_at: now.clone(),
            updated_at: now,
        };
        self.persist(&task)?;
        let id = task.id.clone();
        self.tasks
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(id.clone(), task);
        self.start(&id);
        Ok(id)
    }

    pub fn action(self: &Arc<Self>, id: &str, action: &str) -> AppResult<()> {
        if !self.contains(id) {
            return Err(AppError::NotFound("Task not found".into()));
        }
        match action {
            "pause" => {
                self.update(id, |task| {
                    task.status = "paused".into();
                    task.error = None;
                })?;
                if let Some(token) = self
                    .active
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .get(id)
                {
                    token.cancel();
                }
                Ok(())
            }
            "cancel" => {
                self.update(id, |task| {
                    task.status = "cancelled".into();
                    task.error = None;
                })?;
                if let Some(token) = self
                    .active
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .get(id)
                {
                    token.cancel();
                }
                Ok(())
            }
            "resume" | "retry"
                if ["paused", "failed", "interrupted"].contains(&self.status(id)?.as_str()) =>
            {
                self.update(id, |task| {
                    task.status = "queued".into();
                    task.error = None;
                })?;
                self.start(id);
                Ok(())
            }
            _ => Err(AppError::BadRequest(
                "Action is not available for this web download".into(),
            )),
        }
    }

    fn status(&self, id: &str) -> AppResult<String> {
        self.tasks
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(id)
            .map(|task| task.status.clone())
            .ok_or_else(|| AppError::NotFound("Task not found".into()))
    }

    fn start(self: &Arc<Self>, id: &str) {
        let task = {
            let mut active = self.active.lock().unwrap_or_else(|e| e.into_inner());
            if active.contains_key(id) {
                return;
            }
            let Some(task) = self
                .tasks
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .get(id)
                .cloned()
            else {
                return;
            };
            let token = CancellationToken::new();
            active.insert(id.to_string(), token.clone());
            (task, token)
        };
        let manager = self.clone();
        let worker = manager.clone();
        let task_id = id.to_string();
        tokio::spawn(async move {
            worker.run(task.0, task.1).await;
            manager
                .active
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&task_id);
            if manager
                .status(&task_id)
                .is_ok_and(|status| status == "queued")
            {
                manager.start(&task_id);
            }
        });
    }

    async fn run(self: Arc<Self>, task: WebDownloadTask, cancel: CancellationToken) {
        let id = task.id.clone();
        if cancel.is_cancelled() {
            return;
        }
        let mut task = {
            let mut tasks = self.tasks.lock().unwrap_or_else(|e| e.into_inner());
            let Some(task) = tasks.get_mut(&id) else {
                return;
            };
            if task.status != "queued" {
                return;
            }
            task.status = "transferring".into();
            task.error = None;
            task.completed_bytes = 0;
            task.file_count = usize::from(!task.directory);
            task.directory_count = usize::from(task.directory);
            task.total_bytes = task.expected_size.unwrap_or_default();
            task.updated_at = chrono::Utc::now().to_rfc3339();
            task.clone()
        };
        let _ = self.persist(&task);
        task = match self
            .tasks
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&id)
            .cloned()
        {
            Some(task) => task,
            None => return,
        };
        let progress_manager = self.clone();
        let progress_id = id.clone();
        let pending_progress = Arc::new(AtomicU64::new(0));
        let progress_pending = pending_progress.clone();
        let progress = Arc::new(move |bytes: u64| {
            let accumulated = progress_pending
                .fetch_add(bytes, Ordering::Relaxed)
                .saturating_add(bytes);
            if accumulated >= PROGRESS_PERSIST_INTERVAL {
                let delta = progress_pending.swap(0, Ordering::AcqRel);
                if delta > 0 {
                    let _ = progress_manager.update(&progress_id, |task| {
                        task.completed_bytes = task.completed_bytes.saturating_add(delta)
                    });
                }
            }
        });
        let entry_manager = self.clone();
        let entry_id = id.clone();
        let on_entry = Arc::new(move |is_dir: bool, size: u64| {
            let _ = entry_manager.update(&entry_id, |task| {
                if is_dir {
                    task.directory_count = task.directory_count.saturating_add(1);
                } else {
                    task.file_count = task.file_count.saturating_add(1);
                    task.total_bytes = task.total_bytes.saturating_add(size);
                }
            });
        });
        let result = browser::download_web_share(
            &self.app,
            &task.node_id,
            task.endpoint.clone(),
            &task.share_id,
            &task.path,
            task.token.as_deref(),
            task.directory,
            task.expected_size,
            cancel.clone(),
            progress,
            on_entry,
        )
        .await;
        let remaining_progress = pending_progress.swap(0, Ordering::AcqRel);
        if remaining_progress > 0 {
            let _ = self.update(&id, |task| {
                task.completed_bytes = task.completed_bytes.saturating_add(remaining_progress)
            });
        }
        match result {
            Ok(_) => {
                let _ = self.update(&id, |task| {
                    task.status = "completed".into();
                    task.error = None;
                    task.completed_bytes =
                        task.total_bytes.max(task.expected_size.unwrap_or_default());
                });
            }
            Err(error) => {
                let current = self.status(&id).unwrap_or_default();
                if !cancel.is_cancelled()
                    && current != "paused"
                    && current != "cancelled"
                    && current != "queued"
                {
                    let _ = self.update(&id, |task| {
                        task.status = "failed".into();
                        task.error = Some(error.to_string());
                    });
                }
            }
        }
    }

    fn update(&self, id: &str, change: impl FnOnce(&mut WebDownloadTask)) -> AppResult<()> {
        let task = {
            let mut tasks = self.tasks.lock().unwrap_or_else(|e| e.into_inner());
            let task = tasks
                .get_mut(id)
                .ok_or_else(|| AppError::NotFound("Task not found".into()))?;
            change(task);
            task.updated_at = chrono::Utc::now().to_rfc3339();
            task.clone()
        };
        self.persist(&task)
    }

    fn persist(&self, task: &WebDownloadTask) -> AppResult<()> {
        let json = serde_json::to_string(task)?;
        self.app.db.with_conn(|conn| {
            conn.execute("INSERT INTO web_download_tasks(id,record,created_at,updated_at) VALUES(?1,?2,?3,?4) ON CONFLICT(id) DO UPDATE SET record=excluded.record,updated_at=excluded.updated_at", rusqlite::params![task.id, json, task.created_at, task.updated_at])?;
            Ok(())
        })
    }
}
