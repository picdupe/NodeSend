//! 文件功能：HTTP 共享的路径安全边界（基线 §11"安全边界"、§12）。
//!
//! 规则：
//! - 虚拟路径必须是相对路径，禁止 `..`、绝对路径、盘符、UNC 路径、空字节；
//! - 服务端再次规范化（canonicalize）并检查真实路径是否仍在共享根目录内；
//! - 符号链接解析后若指向共享根外，一律拒绝（防止符号链接越界）；
//! - 上传目标的父目录必须真实存在，不代为创建目录结构。
use std::path::{Component, Path, PathBuf};

use crate::error::{AppError, AppResult};

/// 校验虚拟相对路径，返回规范化的相对 PathBuf。
pub fn validate_virtual(rel: &str) -> AppResult<PathBuf> {
    if rel.contains('\0') {
        return Err(AppError::BadRequest("路径包含非法字符".into()));
    }
    let trimmed = rel.trim();

    // 拒绝绝对路径：/xxx、//server、C:\、\\server、x:/...
    if trimmed.starts_with('/') || trimmed.starts_with('\\') || is_drive_absolute(trimmed) {
        return Err(AppError::BadRequest("必须使用相对路径".into()));
    }

    let path = PathBuf::from(trimmed);
    for component in path.components() {
        match component {
            Component::Normal(_) | Component::CurDir => {}
            Component::ParentDir => return Err(AppError::BadRequest("路径中禁止使用 ..".into())),
            Component::RootDir | Component::Prefix(_) => {
                return Err(AppError::BadRequest("禁止绝对路径".into()))
            }
        }
    }
    Ok(path)
}

/// 解析必须已存在的目标（浏览 / 下载），返回已规范化（越界检查后）的真实路径。
pub fn resolve_existing(base: &Path, rel: &str) -> AppResult<PathBuf> {
    let rel_path = validate_virtual(rel)?;
    let real_base = base.canonicalize()?;
    let joined = real_base.join(&rel_path);
    let real = joined
        .canonicalize()
        .map_err(|_| AppError::NotFound(format!("路径不存在: {rel}")))?;
    ensure_inside(&real_base, &real)?;
    Ok(real)
}

/// 解析写入目标（上传）：父目录必须已存在，目标文件可以不存在。
pub fn resolve_for_write(base: &Path, rel: &str) -> AppResult<(PathBuf, String)> {
    let rel_path = validate_virtual(rel)?;
    let file_name = rel_path
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .ok_or_else(|| AppError::BadRequest("缺少目标文件名".into()))?
        .to_string();

    let parent_rel = rel_path.parent().unwrap_or_else(|| Path::new(""));
    let real_base = base.canonicalize()?;
    let parent_joined = real_base.join(parent_rel);
    let real_parent = parent_joined
        .canonicalize()
        .map_err(|_| AppError::BadRequest("上传目标目录不存在".into()))?;
    ensure_inside(&real_base, &real_parent)?;
    Ok((real_parent.join(&file_name), file_name))
}

fn ensure_inside(real_base: &Path, real_target: &Path) -> AppResult<()> {
    if real_target.starts_with(real_base) {
        Ok(())
    } else {
        Err(AppError::Forbidden("路径越出共享目录边界".into()))
    }
}

/// 识别 `C:` / `C:\` / `C:/` 形式的 Windows 盘符绝对路径。
fn is_drive_absolute(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':'
}
