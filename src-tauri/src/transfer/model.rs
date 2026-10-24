use crate::error::{AppError, AppResult};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::sync::OnceLock;

#[cfg(test)]
mod chunk_lookup_tests {
    use super::*;

    #[test]
    fn incremental_manifest_digest_matches_full_manifest_digest() {
        let manifest = Manifest::new(
            vec![
                FileEntry {
                    path: "root".into(),
                    size: 0,
                    modified_ms: 1,
                    directory: true,
                },
                FileEntry {
                    path: "root/a.bin".into(),
                    size: 123,
                    modified_ms: 2,
                    directory: false,
                },
            ],
            262144,
            true,
        );
        let mut digest = ManifestDigest::new();
        digest.update(&manifest.entries[0]);
        digest.update(&manifest.entries[1]);
        assert_eq!(digest.finish(), manifest.digest());
    }

    #[test]
    fn lookup_matches_wire_indices_across_empty_files_and_directories() {
        for chunk_size in [262144, 1048576, 4194304] {
            let manifest = Manifest::new(
                [
                    (true, 0),
                    (false, 0),
                    (false, chunk_size + 7),
                    (true, 0),
                    (false, chunk_size * 2),
                ]
                .into_iter()
                .enumerate()
                .map(|(i, (directory, size))| FileEntry {
                    path: i.to_string(),
                    size,
                    directory,
                    modified_ms: 0,
                })
                .collect(),
                chunk_size,
                true,
            );
            let chunks = manifest.chunks();
            assert_eq!(manifest.chunk_count(), chunks.len());
            for (index, expected) in chunks.iter().enumerate() {
                let actual = manifest.chunk(index).unwrap();
                assert_eq!(
                    (actual.file, actual.offset, actual.length),
                    (expected.file, expected.offset, expected.length)
                );
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
impl FileEntry {
    /// 清单指纹的逐条编码：长度前缀 + 字段，跨端字节一致。
    /// 用于「分片传输后校验完整性」——两端各自按同一顺序增量哈希同一编码。
    pub fn hash_into(&self, hasher: &mut Sha256) {
        hasher.update((self.path.len() as u32).to_le_bytes());
        hasher.update(self.path.as_bytes());
        hasher.update(self.size.to_le_bytes());
        hasher.update(self.modified_ms.to_le_bytes());
        hasher.update([self.directory as u8]);
    }
}

/// Incremental digest builder for a manifest that is scanned and sent in
/// batches. The byte encoding is exactly the same as `Manifest::digest()` so
/// a receiver can verify the completed stream without retaining a second copy.
pub struct ManifestDigest(Sha256);

impl ManifestDigest {
    pub fn new() -> Self {
        Self(Sha256::new())
    }

    pub fn update(&mut self, entry: &FileEntry) {
        entry.hash_into(&mut self.0);
    }

    pub fn finish(self) -> String {
        format!("{:x}", self.0.finalize())
    }
}

impl Default for ManifestDigest {
    fn default() -> Self {
        Self::new()
    }
}

/// `Offer` 携带的清单摘要（不含条目明细）：让接收端在收到分片清单前就能建立任务、判断重复请求。
/// 条目明细随后由 `ManifestChunk` 分片流式送达，`digest` 覆盖全部条目用于最终校验。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OfferSummary {
    /// True when the sender is still scanning and totals are not known yet.
    #[serde(default)]
    pub streaming: bool,
    /// Number of selected top-level items; these are the first manifest entries.
    #[serde(default)]
    pub top_level_count: usize,
    pub file_count: usize,
    pub directory_count: usize,
    pub total_size: u64,
    pub chunk_count: usize,
    pub chunk_size: u64,
    pub verify_hash: bool,
    pub digest: String,
    /// Non-recursive fingerprint of selected top-level items; checked before resume.
    #[serde(default)]
    pub top_level_fingerprint: String,
}
/// 顶层发送项的来源位置：`name` 为清单中的顶层名，`path` 为其规范化绝对路径。
/// 只保存顶层项（数量等于用户选中的条目数），其余条目路径在读取时按需拼接，
/// 避免为每个条目保存绝对路径并逐条 `canonicalize`（大目录下这是主要卡顿来源）。
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct SourceRoot {
    pub name: String,
    pub path: String,
    /// Metadata of the selected item itself only; never derived from descendants.
    #[serde(default)]
    pub directory: bool,
    #[serde(default)]
    pub size: u64,
    #[serde(default)]
    pub modified_ms: u64,
    /// Old releases persisted source roots as plain path strings. They can be
    /// displayed for history, but lack the metadata required by the current
    /// streaming/resume protocol.
    #[serde(skip)]
    pub legacy_format: bool,
}

impl<'de> Deserialize<'de> for SourceRoot {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Repr {
            Legacy(String),
            Current {
                name: String,
                path: String,
                #[serde(default)]
                directory: bool,
                #[serde(default)]
                size: u64,
                #[serde(default)]
                modified_ms: u64,
            },
        }

        match Repr::deserialize(deserializer)? {
            Repr::Legacy(path) => {
                let name = std::path::Path::new(&path)
                    .file_name()
                    .and_then(|value| value.to_str())
                    .unwrap_or(&path)
                    .to_string();
                Ok(Self {
                    name,
                    path,
                    directory: false,
                    size: 0,
                    modified_ms: 0,
                    legacy_format: true,
                })
            }
            Repr::Current {
                name,
                path,
                directory,
                size,
                modified_ms,
            } => Ok(Self {
                name,
                path,
                directory,
                size,
                modified_ms,
                legacy_format: false,
            }),
        }
    }
}
/// 清单的内存明细 + 线协议/持久化元数据。
///
/// `entries` 为完整条目明细；`prefix` 是「全局块号 → 文件」的前缀和缓存
/// （懒建、单飞），仅用于把 `chunk(index)` 的线性扫描降为二分查找，不参与序列化与相等性比较。
#[derive(Debug, Serialize, Deserialize)]
pub struct Manifest {
    pub entries: Vec<FileEntry>,
    pub chunk_size: u64,
    pub verify_hash: bool,
    #[serde(skip)]
    prefix: OnceLock<Vec<u64>>,
}
impl Clone for Manifest {
    fn clone(&self) -> Self {
        // 缓存是派生数据，克隆时不复制，交由下次访问重建。
        Self::new(self.entries.clone(), self.chunk_size, self.verify_hash)
    }
}
impl PartialEq for Manifest {
    fn eq(&self, other: &Self) -> bool {
        self.entries == other.entries
            && self.chunk_size == other.chunk_size
            && self.verify_hash == other.verify_hash
    }
}
impl Eq for Manifest {}
impl Default for Manifest {
    /// 供 `#[serde(skip)]` 反序列化使用：条目明细在运行期由 `task_entries` 表按需载入。
    fn default() -> Self {
        Self::new(Vec::new(), 262144, false)
    }
}
#[derive(Debug, Clone, Copy)]
pub struct Chunk {
    pub file: usize,
    pub offset: u64,
    pub length: usize,
}
/// `preview_transfer` 返回给前端的清单摘要：只含计数与总量，不含条目明细，
/// 避免把整份清单（数十万条）经 IPC 传输并在前端解析。
#[derive(Debug, Clone, Serialize)]
pub struct ManifestSummary {
    /// False when this is only a top-level preview; recursive scanning happens
    /// in the actual send task and must never block the send button.
    pub scan_complete: bool,
    pub top_level_count: usize,
    pub file_count: usize,
    pub directory_count: usize,
    pub total_size: u64,
    pub chunk_count: usize,
    pub chunk_size: u64,
    pub verify_hash: bool,
}
impl Manifest {
    pub fn new(entries: Vec<FileEntry>, chunk_size: u64, verify_hash: bool) -> Self {
        Self {
            entries,
            chunk_size,
            verify_hash,
            prefix: OnceLock::new(),
        }
    }
    /// 前缀和：`sums[i]` = 文件 `0..i` 的块数之和，长度恒为 `entries.len()+1`。
    fn prefix(&self) -> &[u64] {
        self.prefix.get_or_init(|| {
            let mut sums = Vec::with_capacity(self.entries.len() + 1);
            let mut acc = 0u64;
            for entry in &self.entries {
                sums.push(acc);
                if !entry.directory {
                    acc += entry.size.div_ceil(self.chunk_size);
                }
            }
            sums.push(acc);
            sums
        })
    }
    /// 清单摘要（供预览使用）。
    pub fn summary(&self) -> ManifestSummary {
        ManifestSummary {
            scan_complete: true,
            top_level_count: self
                .entries
                .iter()
                .filter(|entry| !entry.path.contains('/'))
                .count(),
            file_count: self.entries.iter().filter(|e| !e.directory).count(),
            directory_count: self.entries.iter().filter(|e| e.directory).count(),
            total_size: self.total(),
            chunk_count: self.chunk_count(),
            chunk_size: self.chunk_size,
            verify_hash: self.verify_hash,
        }
    }
    /// `Offer` 摘要：计数 + 总量 + 指纹，不含条目明细（明细走 `ManifestChunk` 分片）。
    pub fn offer_summary(&self) -> OfferSummary {
        OfferSummary {
            streaming: false,
            top_level_count: self
                .entries
                .iter()
                .filter(|entry| !entry.path.contains('/'))
                .count(),
            file_count: self.entries.len(),
            directory_count: self.entries.iter().filter(|e| e.directory).count(),
            total_size: self.total(),
            chunk_count: self.chunk_count(),
            chunk_size: self.chunk_size,
            verify_hash: self.verify_hash,
            digest: self.digest(),
            top_level_fingerprint: String::new(),
        }
    }
    /// 清单条目（只读）。
    pub fn entries(&self) -> &[FileEntry] {
        &self.entries
    }
    /// 清单内容指纹：按顺序对全部条目做增量 SHA-256。
    pub fn digest(&self) -> String {
        let mut hasher = ManifestDigest::new();
        for entry in &self.entries {
            hasher.update(entry);
        }
        hasher.finish()
    }
    pub fn validate(&self) -> AppResult<()> {
        if self.entries.is_empty() || ![262144, 1048576, 4194304].contains(&self.chunk_size) {
            return Err(AppError::BadRequest("文件清单或 Chunk 大小非法".into()));
        }
        // Manifest paths are case-insensitive on supported Windows receivers.
        // Hash sets avoid O(n log n) tree operations for very large directories.
        let mut names = std::collections::HashSet::with_capacity(self.entries.len());
        let mut directories = std::collections::HashSet::new();
        let mut total = 0u64;
        for e in &self.entries {
            super::paths::validate_relative(&e.path)?;
            let normalized = e.path.to_lowercase();
            if !names.insert(normalized.clone()) || (e.directory && e.size != 0) {
                return Err(AppError::BadRequest("重复路径或目录大小非法".into()));
            }
            if e.directory {
                directories.insert(normalized);
            }
            total = total
                .checked_add(e.size)
                .ok_or_else(|| AppError::BadRequest("文件大小溢出".into()))?;
        }
        // 条目数不设上限；只保留总量溢出检查（防止清单被构造成不可能的巨量总量）。
        debug_assert_eq!(total, self.total(), "清单总量与条目汇总不一致");
        for e in &self.entries {
            // Checking the immediate parent is sufficient: every directory
            // entry is checked by this same loop, so its own parent is checked
            // recursively through the manifest. This avoids walking every
            // ancestor for every file (which is expensive in deep trees).
            if let Some((parent, _)) = e.path.rsplit_once('/') {
                if !directories.contains(&parent.to_lowercase()) {
                    return Err(AppError::BadRequest(
                        "文件清单缺少父目录或路径类型冲突".into(),
                    ));
                }
            }
        }
        Ok(())
    }
    /// 全局块号总数。
    pub fn chunk_count(&self) -> usize {
        self.prefix().last().copied().unwrap_or(0) as usize
    }
    /// 该文件第一块的全局块号（前缀和），用于按文件批量定位其块区间。
    pub fn file_base(&self, file: usize) -> usize {
        self.prefix().get(file).copied().unwrap_or(0) as usize
    }
    /// 按全局块号定位块：前缀和 + 二分，O(log n)，不再线性扫描全部条目。
    pub fn chunk(&self, index: usize) -> Option<Chunk> {
        let sums = self.prefix();
        if sums.len() != self.entries.len() + 1 {
            return self.chunk_scan(index);
        }
        // 找到最后一个 sums[i] <= index 的 i，则 file = i - 1。
        let file = sums
            .partition_point(|&end| end <= index as u64)
            .checked_sub(1)?;
        let entry = self.entries.get(file)?;
        let offset = (index as u64)
            .checked_sub(sums[file])?
            .checked_mul(self.chunk_size)?;
        if offset >= entry.size {
            return None;
        }
        Some(Chunk {
            file,
            offset,
            length: (entry.size - offset).min(self.chunk_size) as usize,
        })
    }
    /// `chunk` 的安全回退实现（缓存长度异常时使用，正常不会触发）。
    fn chunk_scan(&self, mut index: usize) -> Option<Chunk> {
        for (file, entry) in self.entries.iter().enumerate() {
            if entry.directory {
                continue;
            }
            let count = entry.size.div_ceil(self.chunk_size) as usize;
            if index < count {
                let offset = index as u64 * self.chunk_size;
                return Some(Chunk {
                    file,
                    offset,
                    length: (entry.size - offset).min(self.chunk_size) as usize,
                });
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
/// 传输任务。
///
/// 持久化拆分（Stage 2）：`transfer_tasks.record` 只保存「小标量」字段（本结构中标了
/// `#[serde(skip)]` 的字段全部不进 JSON）；清单明细、完成块、目标路径、转写进度、
/// 跳过文件分别落在 `task_entries` / `task_chunks` / `task_targets` / `task_relay` /
/// `task_skipped` 表，按行增量读写。这样 `persist()`（每次 `edit()` 都会调用）的开销
/// 与文件数无关，百万级清单也不会「每收一块就重写上百 MB JSON」。
///
/// 标量中 `file_count` / `directory_count` / `total_size` / `completed_bytes` 是清单与
/// 进度的**汇总**，让 `snapshot()` 不必加载 bulk 即可 O(1) 生成。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskRecord {
    pub id: String,
    pub wire_id: String,
    pub peer_id: String,
    pub peer_name: String,
    #[serde(default)]
    pub peer_device_type: String,
    #[serde(default)]
    pub preferred_address: Option<String>,
    pub direction: String,
    pub status: String,
    /// 顶层来源映射（名称 → 规范化绝对路径）。条目路径在读取时按其顶层名拼接，
    /// 不再为每个条目保存绝对路径。
    pub sources: Vec<SourceRoot>,
    pub destination: Option<String>,
    pub conflict: ConflictPolicy,
    pub transport: Option<String>,
    pub error: Option<String>,
    pub events: Vec<String>,
    pub updated_at: String,
    pub created_at: String,
    pub allow_public: bool,
    /// 是否启用「缓存盘中转」：接收只写缓存盘，目标盘仅由转写任务连续顺序写。
    /// 缓存盘与目标盘同卷时在 `decide` 阶段置为 false（自动回退直写）。
    #[serde(default)]
    pub use_cache: bool,
    /// 清单块大小与校验开关（原 `manifest` 内字段，移到标量以便不加载明细即可判块）。
    /// Older records may predate this scalar; zero marks them as incompatible history.
    #[serde(default)]
    pub chunk_size: u64,
    /// Missing in older persisted task formats; legacy records are display-only.
    #[serde(default)]
    pub verify_hash: bool,
    /// Android task-level snapshot of the media-library routing choice.
    #[serde(default)]
    pub android_media_library: bool,
    /// Actual public media directories used by this task, populated while the
    /// manifest is received. Empty when media-library routing is disabled.
    #[serde(default)]
    pub receive_directories: BTreeSet<String>,
    /// Summary counters were introduced after the original persisted task format.
    /// Missing counters default to zero; chunk_size=0 still marks old tasks incompatible.
    #[serde(default)]
    pub file_count: usize,
    #[serde(default)]
    pub directory_count: usize,
    #[serde(default)]
    pub total_size: u64,
    /// 全局块总数（清单汇总），用于不加载明细即可判定「是否全部收齐」。
    #[serde(default)]
    pub chunk_count: usize,
    /// Number of selected top-level items, known before recursive scanning.
    #[serde(default)]
    pub top_level_count: usize,
    /// 已完成块数量（含被跳过文件的块），对应 `completed.len()`。
    #[serde(default)]
    pub completed_count: usize,
    /// 已落盘完成字节数（不含被跳过文件），随完成块增量推进。
    #[serde(default)]
    pub completed_bytes: u64,
    /// 已转写字节数汇总（仅缓存中转模式），O(1) 判定「转写是否已完成」。
    #[serde(default)]
    pub relayed_total: u64,
    /// 转写目标字节数汇总（`decide` 时按目标集合算出）。
    #[serde(default)]
    pub relay_target_bytes: u64,
    /// 因源文件变化被跳过的字节数，用于快照中扣除未发送部分。
    #[serde(default)]
    pub skipped_bytes: u64,
    /// 因源文件变化被跳过的条目路径（有上限，仅用于界面提示，避免标量无界增长）。
    #[serde(default)]
    pub skipped_paths: Vec<String>,
    /// 接受任务时选择使用 SSD 缓存的文件索引；未包含的文件直接写目标盘。
    #[serde(default)]
    pub cache_files: BTreeSet<usize>,
    /// Sender explicitly requested resume; consumed by the next connected send run.
    #[serde(default)]
    pub resume_requested: bool,
    /// Monotonic local/remote control-plane revisions prevent delayed retries
    /// from rolling a task back to an older state.
    #[serde(default)]
    pub state_revision: u64,
    #[serde(default)]
    pub remote_state_revision: u64,
    /// 清单内容指纹（`Offer` 摘要携带）：用于重复请求的内容一致性与收完后校验。
    #[serde(default)]
    pub digest: String,
    /// Non-recursive fingerprint of selected top-level source items.
    #[serde(default)]
    pub top_level_fingerprint: String,
    /// Durable deterministic scanner cursor for pause/restart recovery.
    #[serde(default)]
    pub scan_last_seq: usize,
    #[serde(default)]
    pub scan_last_path: String,
    #[serde(default)]
    pub scan_complete: bool,
    /// Sender scan counters for UI display. Persist these independently from
    /// `file_count`: the latter is the durable manifest cursor and must never
    /// advance beyond entries that have actually been saved to the database.
    #[serde(default)]
    pub scan_progress_count: usize,
    #[serde(default)]
    pub scan_progress_directories: usize,
    /// 运行期清单明细：启动时不加载，按需从 `task_entries` 载入（见 `TransferService`）。
    #[serde(skip)]
    pub manifest: Manifest,
    /// 运行期完成块集合：来自 `task_chunks`。
    #[serde(skip)]
    pub completed: BTreeSet<usize>,
    /// 运行期目标路径：来自 `task_targets`（未选中的条目为 `None`）。
    #[serde(skip)]
    pub targets: Vec<Option<String>>,
    /// 运行期每文件转写字节数：来自 `task_relay`。
    #[serde(skip)]
    pub relayed: Vec<u64>,
    /// 运行期跳过文件集合：来自 `task_skipped`。
    #[serde(skip)]
    pub skipped_files: BTreeSet<usize>,
}

/// 因源文件变化被跳过时保留的路径条数上限（界面提示用）。
pub(crate) const SKIPPED_PATHS_LIMIT: usize = 100;

impl TaskRecord {
    /// 运行期 bulk（清单明细等）是否已按 `file_count` 全部载入。
    pub fn bulk_loaded(&self) -> bool {
        self.manifest.entries.len() == self.file_count
            && (self.targets.len() == self.file_count || self.targets.is_empty())
            && (self.relayed.len() == self.file_count || self.relayed.is_empty())
    }
    /// 用清单初始化记录：写入汇总标量并挂载运行期明细（创建任务时调用）。
    pub fn attach_manifest(&mut self, manifest: Manifest) {
        self.chunk_size = manifest.chunk_size;
        self.verify_hash = manifest.verify_hash;
        self.file_count = manifest.entries.len();
        self.directory_count = manifest.entries.iter().filter(|e| e.directory).count();
        self.scan_progress_count = self.file_count;
        self.scan_progress_directories = self.directory_count;
        self.total_size = manifest.total();
        self.chunk_count = manifest.chunk_count();
        self.manifest = manifest;
    }
    pub fn event(&mut self, text: impl Into<String>) {
        self.updated_at = chrono::Utc::now().to_rfc3339();
        self.events
            .push(format!("{} {}", chrono::Local::now().to_rfc3339(), text.into()));
        if self.events.len() > 100 {
            self.events.remove(0);
        }
    }
    /// 记录一个被跳过的文件（发送端源文件变化 / 接收端收到跳过通知）。
    /// 只更新标量汇总，不加载清单明细。
    pub fn note_skipped(&mut self, file: usize, size: u64, path: &str) {
        if self.skipped_files.insert(file) {
            self.skipped_bytes = self.skipped_bytes.saturating_add(size);
        }
        if self.skipped_paths.len() < SKIPPED_PATHS_LIMIT
            && !self.skipped_paths.iter().any(|p| p == path)
        {
            self.skipped_paths.push(path.to_string());
        }
    }
    /// 任务快照（Stage 2 拆分）：只含计数、总量与进度，**不含清单明细**。
    /// 因此无需加载 bulk，也不会再把整份清单经 IPC 传到前端（明细走分页接口）。
    pub fn snapshot(&self) -> TaskSnapshot {
        // Send-side live counters may be ahead of the last durable manifest
        // cursor. Never replace a known count with zero just because a pause,
        // resume, or snapshot happens between scan batches. `scan_last_seq`
        // is also a durable lower bound for the number of scanned entries.
        let (file_count, directory_count) = if self.direction == "send" {
            (
                self.file_count
                    .max(self.scan_progress_count)
                    .max(self.scan_last_seq),
                self.directory_count.max(self.scan_progress_directories),
            )
        } else {
            (self.file_count, self.directory_count)
        };
        let file_name = self
            .manifest
            .entries
            .iter()
            .filter_map(|entry| entry.path.split('/').next())
            .fold(Vec::<String>::new(), |mut names, name| {
                if !names.iter().any(|value| value == name) && names.len() < 32 {
                    names.push(name.to_string());
                }
                names
            })
            .join("、");
        TaskSnapshot {
            warnings: self
                .skipped_paths
                .iter()
                .map(|p| format!("源文件已变化，未发送：{p}"))
                .collect(),
            id: self.id.clone(),
            peer_id: self.peer_id.clone(),
            file_name,
            peer_name: self.peer_name.clone(),
            peer_device_type: self.peer_device_type.clone(),
            direction: self.direction.clone(),
            status: self.status.clone(),
            scan_complete: self.scan_complete,
            top_level_count: self.top_level_count,
            file_count,
            directory_count,
            total_bytes: self.total_size.saturating_sub(self.skipped_bytes),
            completed_bytes: self.completed_bytes,
            transport: self.transport.clone(),
            error: self.error.clone(),
            destination: self.destination.clone(),
            events: self.events.clone(),
            created_at: self.created_at.clone(),
            verify_hash: self.verify_hash,
            android_media_library: self.android_media_library,
            receive_directories: if self.android_media_library {
                ["Pictures/NodeSend", "Movies/NodeSend", "Music/NodeSend", "Download/NodeSend"]
                    .into_iter()
                    .filter(|directory| self.receive_directories.contains(*directory))
                    .map(str::to_string)
                    .collect()
            } else {
                Vec::new()
            },
        }
    }
}
#[derive(Debug, Clone, Serialize)]
pub struct TaskSnapshot {
    pub warnings: Vec<String>,
    pub id: String,
    pub peer_id: String,
    pub file_name: String,
    pub peer_name: String,
    pub peer_device_type: String,
    pub direction: String,
    pub status: String,
    pub scan_complete: bool,
    pub top_level_count: usize,
    pub file_count: usize,
    pub directory_count: usize,
    pub total_bytes: u64,
    pub completed_bytes: u64,
    pub transport: Option<String>,
    pub error: Option<String>,
    pub destination: Option<String>,
    pub events: Vec<String>,
    pub created_at: String,
    pub verify_hash: bool,
    pub android_media_library: bool,
    pub receive_directories: Vec<String>,
}
pub fn random_id() -> String {
    format!("{:032x}", rand::random::<u128>())
}
pub fn incoming_id(peer: &str, wire: &str) -> String {
    format!("in-{peer}-{wire}")
}
