use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use tokio::{
    fs::{File, OpenOptions},
    io::{copy, AsyncWriteExt},
    sync::Mutex,
};

use crate::{
    error::{AppError, AppResult},
    httpapi::security,
};

#[derive(Clone, Serialize, Deserialize)]
struct Session {
    share_id: String,
    relative_target: String,
    expected: u64,
    offset: u64,
}

pub struct UploadSessions {
    root: PathBuf,
    sessions: Mutex<HashMap<String, Session>>,
}

#[derive(Deserialize)]
pub struct InitRequest {
    pub directory: Option<String>,
    pub name: String,
    pub size: u64,
    pub upload_id: Option<String>,
}

#[derive(Serialize)]
pub struct SessionStatus {
    pub id: String,
    pub offset: u64,
    pub size: u64,
    pub complete: bool,
}

impl UploadSessions {
    pub fn new(root: PathBuf) -> Self {
        let _ = std::fs::create_dir_all(&root);
        let mut sessions = HashMap::new();
        if let Ok(entries) = std::fs::read_dir(&root) {
            let mut entries = entries.flatten().collect::<Vec<_>>();
            entries
                .sort_by_key(|entry| entry.path().extension().is_some_and(|value| value == "tmp"));
            for entry in entries {
                let path = entry.path();
                if path
                    .extension()
                    .is_some_and(|extension| extension == "json" || extension == "tmp")
                {
                    let Some(filename) = path.file_name().and_then(|value| value.to_str()) else {
                        continue;
                    };
                    let Some(id) = filename
                        .strip_suffix(".json.tmp")
                        .or_else(|| filename.strip_suffix(".json"))
                    else {
                        continue;
                    };
                    if uuid::Uuid::parse_str(id).is_err() {
                        continue;
                    }
                    let Ok(bytes) = std::fs::read(&path) else {
                        continue;
                    };
                    let Ok(mut session) = serde_json::from_slice::<Session>(&bytes) else {
                        continue;
                    };
                    let part = root.join(format!("{id}.part"));
                    let Ok(metadata) = std::fs::metadata(part) else {
                        continue;
                    };
                    if metadata.len() > session.expected {
                        continue;
                    }
                    session.offset = metadata.len();
                    sessions.insert(id.to_owned(), session);
                }
            }
        }
        Self {
            root,
            sessions: Mutex::new(sessions),
        }
    }

    pub async fn init(
        &self,
        share_id: &str,
        share_root: &Path,
        request: InitRequest,
    ) -> AppResult<SessionStatus> {
        let directory = request.directory.as_deref().unwrap_or_default();
        security::create_write_directory(share_root, directory)?;
        let relative = join(directory, &request.name);
        let (target, _) = security::resolve_for_write(share_root, &relative)?;
        if let Some(id) = request.upload_id.as_deref() {
            if uuid::Uuid::parse_str(id).is_err() {
                return Err(AppError::BadRequest("Invalid upload session ID".into()));
            }
            let mut sessions = self.sessions.lock().await;
            if let Some(session) = sessions.get_mut(id) {
                if session.share_id != share_id {
                    return Err(AppError::Forbidden(
                        "Upload session belongs to another share".into(),
                    ));
                }
                if session.expected != request.size || session.relative_target != relative {
                    return Err(AppError::Conflict(
                        "Upload details changed while resuming".into(),
                    ));
                }
                let (current_target, _) =
                    security::resolve_for_write(share_root, &session.relative_target)?;
                if current_target != target {
                    return Err(AppError::Conflict(
                        "Upload destination changed while resuming".into(),
                    ));
                }
                let part = self.part_path(id);
                let actual = tokio::fs::metadata(part).await?.len();
                if actual > session.expected {
                    return Err(AppError::Conflict(
                        "Upload data exceeds the expected file size".into(),
                    ));
                }
                session.offset = actual;
                if actual == session.expected {
                    publish(&self.part_path(id), &target).await?;
                    sessions.remove(id);
                    let _ = tokio::fs::remove_file(self.metadata_path(id)).await;
                    return Ok(SessionStatus {
                        id: id.to_owned(),
                        offset: actual,
                        size: request.size,
                        complete: true,
                    });
                }
                return Ok(status(id, session));
            }
        }
        if tokio::fs::try_exists(&target).await? {
            return Err(AppError::Conflict(
                "Upload destination already exists".into(),
            ));
        }
        let id = uuid::Uuid::new_v4().to_string();
        let part = self.part_path(&id);
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&part)
            .await?;
        if request.size == 0 {
            publish(&part, &target).await?;
            return Ok(SessionStatus {
                id,
                offset: 0,
                size: 0,
                complete: true,
            });
        }
        let session = Session {
            share_id: share_id.to_owned(),
            relative_target: relative,
            expected: request.size,
            offset: 0,
        };
        self.persist(&id, &session).await?;
        self.sessions.lock().await.insert(id.clone(), session);
        Ok(SessionStatus {
            id,
            offset: 0,
            size: request.size,
            complete: false,
        })
    }

    pub async fn status(&self, share_id: &str, id: &str) -> AppResult<SessionStatus> {
        let sessions = self.sessions.lock().await;
        let session = sessions
            .get(id)
            .ok_or_else(|| AppError::NotFound("Upload session not found".into()))?;
        if session.share_id != share_id {
            return Err(AppError::Forbidden(
                "Upload session belongs to another share".into(),
            ));
        }
        Ok(status(id, session))
    }

    pub async fn append(
        &self,
        share_id: &str,
        share_root: &Path,
        id: &str,
        range: &str,
        body: &[u8],
    ) -> AppResult<SessionStatus> {
        if uuid::Uuid::parse_str(id).is_err() {
            return Err(AppError::BadRequest("Invalid upload session ID".into()));
        }
        let mut sessions = self.sessions.lock().await;
        let session = sessions
            .get_mut(id)
            .ok_or_else(|| AppError::NotFound("Upload session not found".into()))?;
        if session.share_id != share_id {
            return Err(AppError::Forbidden(
                "Upload session belongs to another share".into(),
            ));
        }
        let temporary = self.part_path(id);
        let actual = tokio::fs::metadata(&temporary).await?.len();
        if actual > session.expected {
            return Err(AppError::Conflict(
                "Upload data exceeds the expected file size".into(),
            ));
        }
        session.offset = actual;
        if actual == session.expected {
            let size = session.expected;
            let (target, _) = security::resolve_for_write(share_root, &session.relative_target)?;
            publish(&temporary, &target).await?;
            sessions.remove(id);
            let _ = tokio::fs::remove_file(self.metadata_path(id)).await;
            return Ok(SessionStatus {
                id: id.to_owned(),
                offset: actual,
                size,
                complete: true,
            });
        }
        let (start, end, total) = parse_range(range)
            .ok_or_else(|| AppError::BadRequest("Invalid Content-Range".into()))?;
        if start != session.offset
            || total != session.expected
            || end.saturating_sub(start).saturating_add(1) != body.len() as u64
            || end >= total
        {
            return Err(AppError::Conflict(
                "Upload range does not match the current session offset".into(),
            ));
        }
        let mut file = OpenOptions::new().append(true).open(&temporary).await?;
        file.write_all(body).await?;
        file.flush().await?;
        session.offset += body.len() as u64;
        self.persist(id, session).await?;
        let complete = session.offset == session.expected;
        let result = SessionStatus {
            id: id.to_owned(),
            offset: session.offset,
            size: session.expected,
            complete,
        };
        if complete {
            let (target, _) = security::resolve_for_write(share_root, &session.relative_target)?;
            publish(&temporary, &target).await?;
            sessions.remove(id);
            let _ = tokio::fs::remove_file(self.metadata_path(id)).await;
        }
        Ok(result)
    }

    fn part_path(&self, id: &str) -> PathBuf {
        self.root.join(format!("{id}.part"))
    }
    fn metadata_path(&self, id: &str) -> PathBuf {
        self.root.join(format!("{id}.json"))
    }

    async fn persist(&self, id: &str, session: &Session) -> AppResult<()> {
        let path = self.metadata_path(id);
        let temporary = self.root.join(format!("{id}.json.tmp"));
        let bytes =
            serde_json::to_vec(session).map_err(|error| AppError::Other(error.to_string()))?;
        tokio::fs::write(&temporary, bytes).await?;
        if tokio::fs::try_exists(&path).await? {
            tokio::fs::remove_file(&path).await?;
        }
        tokio::fs::rename(temporary, path).await?;
        Ok(())
    }
}

fn status(id: &str, session: &Session) -> SessionStatus {
    SessionStatus {
        id: id.to_owned(),
        offset: session.offset,
        size: session.expected,
        complete: false,
    }
}

async fn publish(temporary: &Path, target: &Path) -> AppResult<()> {
    match tokio::fs::hard_link(temporary, target).await {
        Ok(()) => {
            // The destination now points to the fully written temporary file.
            // Removing the staging name is cleanup only; publication already
            // succeeded and must not be reported as failed if cleanup fails.
            if let Err(error) = tokio::fs::remove_file(temporary).await {
                tracing::warn!(%error, path = %temporary.display(), "Could not remove published upload staging file");
            }
            return Ok(());
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            return Err(AppError::Conflict(
                "Upload destination already exists".into(),
            ));
        }
        // The upload cache may be on another volume. Copy into a temporary
        // sibling of the target, then publish that sibling without replacement.
        Err(_) => {}
    }

    let parent = target
        .parent()
        .ok_or_else(|| AppError::BadRequest("Upload destination has no parent directory".into()))?;
    let staging = parent.join(format!(".nodesend-upload-{}.tmp", uuid::Uuid::new_v4()));
    let mut source = File::open(temporary).await?;
    let mut destination = match OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&staging)
        .await
    {
        Ok(file) => file,
        Err(error) => return Err(error.into()),
    };
    if let Err(error) = copy(&mut source, &mut destination).await {
        drop(destination);
        let _ = tokio::fs::remove_file(&staging).await;
        return Err(error.into());
    }
    if let Err(error) = destination.flush().await {
        drop(destination);
        let _ = tokio::fs::remove_file(&staging).await;
        return Err(error.into());
    }
    if let Err(error) = destination.sync_all().await {
        drop(destination);
        let _ = tokio::fs::remove_file(&staging).await;
        return Err(error.into());
    }
    drop(destination);

    let publish_result = publish_staging(&staging, target).await;
    if let Err(error) = publish_result {
        let _ = tokio::fs::remove_file(&staging).await;
        if error.kind() == std::io::ErrorKind::AlreadyExists {
            return Err(AppError::Conflict(
                "Upload destination already exists".into(),
            ));
        }
        return Err(AppError::Other(format!(
            "Uploaded file could not be published without replacing an existing file: {error}"
        )));
    }
    if tokio::fs::try_exists(&staging).await.unwrap_or(false) {
        if let Err(error) = tokio::fs::remove_file(&staging).await {
            tracing::warn!(%error, path = %staging.display(), "Could not remove upload staging file after publication");
        }
    }
    if let Err(error) = tokio::fs::remove_file(temporary).await {
        tracing::warn!(%error, path = %temporary.display(), "Could not remove copied upload staging file");
    }
    Ok(())
}

async fn publish_staging(staging: &Path, target: &Path) -> std::io::Result<()> {
    #[cfg(windows)]
    {
        // Windows rename fails if the destination already exists.
        tokio::fs::rename(staging, target).await
    }
    #[cfg(not(windows))]
    {
        // A hard link publishes atomically and fails rather than replacing an
        // existing destination. The staging file is in the target directory.
        tokio::fs::hard_link(staging, target).await
    }
}

fn join(directory: &str, name: &str) -> String {
    let directory = directory.trim().trim_matches('/');
    if directory.is_empty() {
        name.to_owned()
    } else {
        format!("{directory}/{name}")
    }
}

fn parse_range(value: &str) -> Option<(u64, u64, u64)> {
    let range = value
        .strip_prefix("bytes=")
        .or_else(|| value.strip_prefix("bytes "))?;
    let (range, total) = range.split_once('/')?;
    let (start, end) = range.split_once('-')?;
    Some((start.parse().ok()?, end.parse().ok()?, total.parse().ok()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_directory() -> PathBuf {
        std::env::temp_dir().join(format!("nodesend-upload-{}", uuid::Uuid::new_v4()))
    }

    #[tokio::test]
    async fn chunked_upload_reports_completion_after_publishing_final_chunk() {
        let root = test_directory();
        let share_root = root.join("share");
        let upload_root = root.join("uploads");
        tokio::fs::create_dir_all(&share_root).await.unwrap();
        let sessions = UploadSessions::new(upload_root);

        let initial = sessions
            .init(
                "share-id",
                &share_root,
                InitRequest {
                    directory: Some("nested".into()),
                    name: "fixture.txt".into(),
                    size: 11,
                    upload_id: None,
                },
            )
            .await
            .unwrap();
        assert!(!initial.complete);

        let first = sessions
            .append(
                "share-id",
                &share_root,
                &initial.id,
                "bytes=0-4/11",
                b"hello",
            )
            .await
            .unwrap();
        assert_eq!(first.offset, 5);
        assert!(!first.complete);

        let final_chunk = sessions
            .append(
                "share-id",
                &share_root,
                &initial.id,
                "bytes=5-10/11",
                b" world",
            )
            .await
            .unwrap();

        assert_eq!(final_chunk.offset, 11);
        assert_eq!(final_chunk.size, 11);
        assert!(final_chunk.complete);
        assert_eq!(
            tokio::fs::read_to_string(share_root.join("nested/fixture.txt"))
                .await
                .unwrap(),
            "hello world"
        );
        assert!(sessions.status("share-id", &initial.id).await.is_err());

        tokio::fs::remove_dir_all(root).await.unwrap();
    }
}
