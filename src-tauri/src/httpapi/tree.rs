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
            tracing::warn!(dir = %real_dir.display(), "目录读取失败，已跳过: {err}");
            return Ok(());
        }
    };

    // 先收集 (真实路径, 虚拟路径, 是否目录)，再排序，保证顺序稳定。
    let mut items: Vec<(PathBuf, String, bool)> = Vec::new();
    for entry in entries.flatten() {
        let child_real = match entry.path().canonicalize() {
            Ok(path) => path,
            Err(_) => continue,
        };
        if !child_real.starts_with(base_real) {
            tracing::warn!(path = %child_real.display(), "检测到越界路径（可能为符号链接），已跳过");
            continue;
        }
        let child_name = match entry.file_name().into_string() {
            Ok(name) => name,
            Err(_) => continue,
        };
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
