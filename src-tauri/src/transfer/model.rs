use crate::error::{AppError, AppResult};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

#[cfg(test)]
mod chunk_lookup_tests {
    use super::*;

    #[test]
    fn lookup_matches_wire_indices_across_empty_files_and_directories() {
        for chunk_size in [262144, 1048576, 4194304] {
            let manifest = Manifest {
                entries: [(true, 0), (false, 0), (false, chunk_size + 7), (true, 0), (false, chunk_size * 2)]
                    .into_iter().enumerate().map(|(i, (directory, size))| FileEntry {
                        path: i.to_string(), size, directory, modified_ms: 0,
                    }).collect(),
                chunk_size, verify_hash: true,
            };
            let chunks = manifest.chunks();
            for (index, expected) in chunks.iter().enumerate() {
                let actual = manifest.chunk(index).unwrap();
                assert_eq!((actual.file, actual.offset, actual.length), (expected.file, expected.offset, expected.length));
            }
            assert!(manifest.chunk(chunks.len()).is_none());
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FileEntry {
    pub path: String,
    pub size: u64,
    pub modified_ms: u64,
    pub directory: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Manifest {
    pub entries: Vec<FileEntry>,
    pub chunk_size: u64,
    pub verify_hash: bool,
}
#[derive(Debug, Clone, Copy)]
pub struct Chunk {
    pub file: usize,
    pub offset: u64,
    pub length: usize,
}
impl Manifest {
    pub fn validate(&self) -> AppResult<()> {
        if self.entries.is_empty()
            || self.entries.len() > 10000
            || ![262144, 1048576, 4194304].contains(&self.chunk_size)
        {
            return Err(AppError::BadRequest("文件清单或 Chunk 大小非法".into()));
        }
        let mut names = BTreeSet::new();
        let mut total = 0u64;
        let directories = self
            .entries
            .iter()
            .filter(|e| e.directory)
            .map(|e| e.path.to_lowercase())
            .collect::<BTreeSet<_>>();
        for e in &self.entries {
            super::paths::validate_relative(&e.path)?;
            if !names.insert(e.path.to_lowercase()) || (e.directory && e.size != 0) {
                return Err(AppError::BadRequest("重复路径或目录大小非法".into()));
            }
            total = total
                .checked_add(e.size)
                .ok_or_else(|| AppError::BadRequest("文件大小溢出".into()))?;
        }
        if total / self.chunk_size > 1_000_000 {
            return Err(AppError::BadRequest("单任务 Chunk 数量超出限制".into()));
        }
        for e in &self.entries {
            for ancestor in std::path::Path::new(&e.path)
                .ancestors()
                .skip(1)
                .filter(|p| !p.as_os_str().is_empty())
            {
                let parent = ancestor.to_string_lossy().replace('\\', "/");
                if !directories.contains(&parent.to_lowercase()) {
                    return Err(AppError::BadRequest(
                        "文件清单缺少父目录或路径类型冲突".into(),
                    ));
                }
            }
        }
        Ok(())
    }
    pub fn chunk(&self, mut index: usize) -> Option<Chunk> {
        for (file, entry) in self.entries.iter().enumerate() {
            if entry.directory { continue; }
            let count = entry.size.div_ceil(self.chunk_size) as usize;
            if index < count {
                let offset = index as u64 * self.chunk_size;
                return Some(Chunk { file, offset, length: (entry.size - offset).min(self.chunk_size) as usize });
            }
            index -= count;
        }
        None
    }
    pub fn chunks(&self) -> Vec<Chunk> {
        self.entries
            .iter()
            .enumerate()
            .filter(|(_, e)| !e.directory)
            .flat_map(|(file, e)| {
                (0..e.size.div_ceil(self.chunk_size)).map(move |i| Chunk {
                    file,
                    offset: i * self.chunk_size,
                    length: (e.size - i * self.chunk_size).min(self.chunk_size) as usize,
                })
            })
            .collect()
    }
    pub fn total(&self) -> u64 {
        self.entries.iter().map(|e| e.size).sum()
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ConflictPolicy {
    Rename,
    Skip,
    Overwrite,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskRecord {
    #[serde(default)]
    pub skipped_files: BTreeSet<usize>,
    pub id: String,
    pub wire_id: String,
    pub peer_id: String,
    pub peer_name: String,
    pub direction: String,
    pub status: String,
    pub manifest: Manifest,
    pub sources: Vec<String>,
    pub targets: Vec<Option<String>>,
    pub destination: Option<String>,
    pub conflict: ConflictPolicy,
    pub completed: BTreeSet<usize>,
    pub transport: Option<String>,
    pub error: Option<String>,
    pub events: Vec<String>,
    pub updated_at: String,
    pub created_at: String,
    pub allow_public: bool,
}
impl TaskRecord {
    pub fn event(&mut self, text: impl Into<String>) {
        self.updated_at = chrono::Utc::now().to_rfc3339();
        self.events
            .push(format!("{} {}", self.updated_at, text.into()));
        if self.events.len() > 100 {
            self.events.remove(0);
        }
    }
    pub fn snapshot(&self) -> TaskSnapshot {
        let chunks = self.manifest.chunks();
        TaskSnapshot {
            warnings: self.skipped_files.iter().filter_map(|i| self.manifest.entries.get(*i)).map(|e| format!("源文件已变化，未发送：{}", e.path)).collect(),
            id: self.id.clone(),
            peer_id: self.peer_id.clone(),
            peer_name: self.peer_name.clone(),
            direction: self.direction.clone(),
            status: self.status.clone(),
            entries: self.manifest.entries.clone(),
            total_bytes: self.manifest.entries.iter().enumerate().filter(|(i, _)| !self.skipped_files.contains(i)).map(|(_, e)| e.size).sum(),
            completed_bytes: self
                .completed
                .iter()
                .filter_map(|i| chunks.get(*i))
                .filter(|c| !self.skipped_files.contains(&c.file))
                .map(|c| c.length as u64)
                .sum(),
            transport: self.transport.clone(),
            error: self.error.clone(),
            destination: self.destination.clone(),
            events: self.events.clone(),
            created_at: self.created_at.clone(),
            verify_hash: self.manifest.verify_hash,
        }
    }
}
#[derive(Debug, Clone, Serialize)]
pub struct TaskSnapshot {
    pub warnings: Vec<String>,
    pub id: String,
    pub peer_id: String,
    pub peer_name: String,
    pub direction: String,
    pub status: String,
    pub entries: Vec<FileEntry>,
    pub total_bytes: u64,
    pub completed_bytes: u64,
    pub transport: Option<String>,
    pub error: Option<String>,
    pub destination: Option<String>,
    pub events: Vec<String>,
    pub created_at: String,
    pub verify_hash: bool,
}
pub fn random_id() -> String {
    format!("{:032x}", rand::random::<u128>())
}
pub fn incoming_id(peer: &str, wire: &str) -> String {
    format!("in-{peer}-{wire}")
}
