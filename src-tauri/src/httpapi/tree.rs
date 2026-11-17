//! 文件功能：目录树构建器（HTTP 目录树 API 的核心，用户明确要求）。
//!
//! - 输入：已通过安全检查的真实路径、虚拟相对路径、深度 depth；
//! - 输出：统一 [`TreeNode`] 树（名称 / 虚拟路径 / 类型 / 大小 / 修改时间 / 子节点）；
//! - depth=0 仅返回节点本身；depth>0 递归展开子目录；
//! - 排序：目录优先，其次名称（不区分大小写）；
//! - 子项经 canonicalize 后再次做共享根前缀检查，符号链接越界项直接跳过；
//! - 单个目录读取失败不影响其他目录（记录日志后跳过）。
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::Serialize;

use crate::error::{AppError, AppResult};

#[derive(Debug, Clone, Serialize)]
pub struct DirectoryNode {
    pub name: String,
    /// 相对共享根的虚拟路径（始终以 / 风格表示）。
    pub path: String,
}

/// Build a directory-only tree snapshot for a newly created HTTP share.
/// Files are not added to the snapshot or inspected beyond checking entry types.
pub fn build_directory_tree(real_root: &Path) -> AppResult<Vec<DirectoryNode>> {
    let base_real = real_root.canonicalize()?;
    collect_directory_tree(&base_real, &base_real, "")
}

fn collect_directory_tree(
    base_real: &Path,
    start_real: &Path,
    virtual_root: &str,
) -> AppResult<Vec<DirectoryNode>> {
    let mut pending = vec![(start_real.to_path_buf(), virtual_root.to_owned())];
    let mut directories = Vec::new();

    while let Some((real_parent, virtual_parent)) = pending.pop() {
        let entries = match std::fs::read_dir(&real_parent) {
            Ok(entries) => entries,
            Err(error) => {
                tracing::warn!(directory = %real_parent.display(), "Directory tree skipped an unreadable directory: {error}");
                continue;
            }
        };

        for entry in entries.flatten() {
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if !file_type.is_dir() || file_type.is_symlink() {
                continue;
            }
            let Ok(real_path) = entry.path().canonicalize() else {
                continue;
            };
            if !real_path.starts_with(base_real) {
                continue;
            }

            let Ok(name) = entry.file_name().into_string() else {
                continue;
            };
            if crate::httpapi::security::is_upload_staging_name(&name) {
                continue;
            }
            let path = if virtual_parent.is_empty() {
                name.clone()
            } else {
                format!("{virtual_parent}/{name}")
            };
            directories.push(DirectoryNode {
                name,
                path: path.clone(),
            });
            pending.push((real_path, path));
        }
    }

    directories.sort_by(|left, right| left.path.to_lowercase().cmp(&right.path.to_lowercase()));
    Ok(directories)
}

/// Refresh cached directory branches affected by filesystem watcher events.
pub fn refresh_directory_tree_paths(
    real_root: &Path,
    snapshot: &mut Vec<DirectoryNode>,
    paths: &[String],
) {
    let Ok(base_real) = real_root.canonicalize() else {
        return;
    };
    for path in paths {
        let Ok(relative) = crate::httpapi::security::validate_virtual(path) else {
            continue;
        };
        let virtual_path = normalize_virt(&relative.to_string_lossy());
        if virtual_path.is_empty() {
            continue;
        }
        snapshot.retain(|directory| {
            directory.path != virtual_path
                && !directory.path.starts_with(&format!("{virtual_path}/"))
        });

        let candidate = base_real.join(&relative);
        let Ok(real_path) = candidate.canonicalize() else {
            continue;
        };
        if !real_path.starts_with(&base_real) || !real_path.is_dir() {
            continue;
        }
        let Some(name) = relative.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        snapshot.push(DirectoryNode {
            name: name.to_owned(),
            path: virtual_path.clone(),
        });
        if let Ok(mut descendants) = collect_directory_tree(&base_real, &real_path, &virtual_path) {
            snapshot.append(&mut descendants);
        }
    }
    snapshot.sort_by(|left, right| left.path.to_lowercase().cmp(&right.path.to_lowercase()));
}

/// Refresh the immediate directory children of a browsed directory in a cached snapshot.
pub fn merge_directory_listing(
    snapshot: &mut Vec<DirectoryNode>,
    parent: &str,
    entries: &[TreeNode],
) {
    let parent = normalize_virt(parent);
    let child_directories = entries
        .iter()
        .filter(|entry| entry.node_type == NodeType::Dir)
        .map(|entry| DirectoryNode {
            name: entry.name.clone(),
            path: normalize_virt(&entry.path),
        })
        .collect::<Vec<_>>();
    let current_paths = child_directories
        .iter()
        .map(|directory| directory.path.clone())
        .collect::<std::collections::HashSet<_>>();
    let removed_paths = snapshot
        .iter()
        .filter(|directory| parent_path(&directory.path) == parent)
        .filter(|directory| !current_paths.contains(&directory.path))
        .map(|directory| directory.path.clone())
        .collect::<Vec<_>>();

    snapshot.retain(|directory| {
        !removed_paths.iter().any(|removed| {
            directory.path == *removed || directory.path.starts_with(&format!("{removed}/"))
        })
    });
    for directory in child_directories {
        if !snapshot.iter().any(|known| known.path == directory.path) {
            snapshot.push(directory);
        }
    }
}

fn parent_path(path: &str) -> &str {
    path.rsplit_once('/')
        .map(|(parent, _)| parent)
        .unwrap_or("")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum NodeType {
    Dir,
    File,
}

#[derive(Debug, Serialize)]
pub struct TreeNode {
    pub name: String,
    /// 相对共享根的虚拟路径（始终以 / 风格表示，根为 ""）
    pub path: String,
    pub node_type: NodeType,
    pub size: u64,
    pub modified: String,
    /// 文件为 None；目录为 Some（depth 耗尽时为空数组）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub children: Option<Vec<TreeNode>>,
}

/// 构建入口：以真实路径根构建，返回带根节点的树。
pub fn build_tree(real_root: &Path, virtual_root: &str, depth: u32) -> AppResult<TreeNode> {
    let base_real = real_root.canonicalize().map_err(|err| AppError::Io(err))?;
    build(&base_real, &base_real, virtual_root, depth)
}

/// 递归构建单个节点。
fn build(real: &Path, base_real: &Path, virt: &str, depth: u32) -> AppResult<TreeNode> {
    let metadata = std::fs::metadata(real)?;
    let is_dir = metadata.is_dir();
    let name = node_name(real, virt);

    let mut node = TreeNode {
        name,
        path: normalize_virt(virt),
        node_type: if is_dir {
            NodeType::Dir
        } else {
            NodeType::File
        },
        size: metadata.len(),
        modified: format_time(metadata.modified().ok()),
        children: None,
    };

    if is_dir {
        let mut children: Vec<TreeNode> = Vec::new();
        if depth > 0 {
            collect_children(real, base_real, virt, depth, &mut children)?;
        }
        node.children = Some(children);
    }
    Ok(node)
}

fn collect_children(
    real_dir: &Path,
    base_real: &Path,
    virt: &str,
    depth: u32,
    out: &mut Vec<TreeNode>,
) -> AppResult<()> {
    let entries = match std::fs::read_dir(real_dir) {
        Ok(entries) => entries,
        Err(err) => {
            tracing::warn!(dir = %real_dir.display(), "Failed to read directory; skipped: {err}");
            return Ok(());
        }
    };

    // 先收集 (真实路径, 虚拟路径, 是否目录)，再排序，保证顺序稳定。
    let mut items: Vec<(PathBuf, String, bool)> = Vec::new();
    for entry in entries.flatten() {
        let child_name = match entry.file_name().into_string() {
            Ok(name) => name,
            Err(_) => continue,
        };
        if crate::httpapi::security::is_upload_staging_name(&child_name) {
            continue;
        }
        let child_real = match entry.path().canonicalize() {
            Ok(path) => path,
            Err(_) => continue,
        };
        if !child_real.starts_with(base_real) {
            tracing::warn!(path = %child_real.display(), "Detected a path traversal (possibly a symbolic link); skipped");
            continue;
        }
        let child_virt = join_virt(virt, &child_name);
        let is_dir = child_real.is_dir();
        items.push((child_real, child_virt, is_dir));
    }

    items.sort_by(|a, b| {
        b.2.cmp(&a.2) // 目录优先
            .then_with(|| a.1.to_lowercase().cmp(&b.1.to_lowercase()))
    });

    for (child_real, child_virt, _) in items {
        let child = build(&child_real, base_real, &child_virt, depth - 1)?;
        out.push(child);
    }
    Ok(())
}

fn node_name(real: &Path, virt: &str) -> String {
    if let Some(name) = Path::new(virt).file_name().and_then(|n| n.to_str()) {
        if !name.is_empty() {
            return name.to_string();
        }
    }
    real.file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("root")
        .to_string()
}

/// 统一虚拟路径展示：根为空字符串；其余以 / 连接、不以 / 开头。
fn normalize_virt(virt: &str) -> String {
    virt.trim().replace('\\', "/").trim_matches('/').to_string()
}

fn join_virt(parent: &str, name: &str) -> String {
    let parent = normalize_virt(parent);
    if parent.is_empty() {
        name.to_string()
    } else {
        format!("{parent}/{name}")
    }
}

fn format_time(time: Option<std::time::SystemTime>) -> String {
    match time {
        Some(t) => {
            let dt: DateTime<Utc> = t.into();
            dt.to_rfc3339()
        }
        None => String::new(),
    }
}
