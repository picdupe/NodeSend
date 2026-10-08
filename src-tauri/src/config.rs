//! 文件功能：应用本地路径配置（所有 NodeSend 本地数据的位置约定）。
//!
//! - 基线 §45/§21：数据、身份、数据库等分别落盘，Windows 位于 %APPDATA%/NodeSend，其他平台使用 Tauri app_data_dir；
//! - 身份私钥（identity_key）只保存在本机，不参与导出/同步（基线 §2.1、§35）；
//! - 后续按 §21 支持自定义各类存储位置，初版使用统一数据目录。
use std::path::PathBuf;

/// Windows uses %APPDATA%/NodeSend; other platforms use Tauri's data directory.
/// This only resolves a path and never moves or copies existing profiles.
pub fn app_data_dir(_tauri_dir: PathBuf) -> std::io::Result<PathBuf> {
    #[cfg(target_os = "windows")]
    {
        dirs::data_dir()
            .map(|root| root.join("NodeSend"))
            .ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    "Windows AppData directory unavailable",
                )
            })
    }
    #[cfg(not(target_os = "windows"))]
    Ok(_tauri_dir)
}

#[derive(Debug, Clone)]
pub struct AppPaths {
    /// 数据根目录（Windows 使用软件名，其他平台使用 Tauri app_data_dir）
    pub data_dir: PathBuf,
    /// SQLite 数据库文件
    pub db_file: PathBuf,
    /// ECDSA P-256 身份私钥（PKCS#8 PEM）
    pub identity_key: PathBuf,
    /// 与 Node ID 绑定的自签名证书（PEM）
    pub identity_cert: PathBuf,
}

impl AppPaths {
    /// 以数据根目录构造全部路径（不创建目录，创建由 [`AppPaths::ensure_dirs`] 完成）。
    pub fn new(data_dir: PathBuf) -> Self {
        Self {
            db_file: data_dir.join("nodesend.db"),
            identity_key: data_dir.join("identity").join("node.key.pem"),
            identity_cert: data_dir.join("identity").join("node.cert.pem"),
            data_dir,
        }
    }

    /// 创建数据目录结构（幂等）。
    pub fn ensure_dirs(&self) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.data_dir)?;
        if let Some(parent) = self.identity_key.parent() {
            std::fs::create_dir_all(parent)?;
        }
        Ok(())
    }
}
