//! 文件功能：应用 Service 装配层（基线 §46/§53）。
//!
//! - [`AppService`] 组合并持有：路径配置、数据库、本机身份、HTTP 监听地址；
//! - 是各模块的组合根：初始化顺序 = 目录 → 数据库迁移 → 身份；
//! - 提供设置读写（settings 表），供 UI/IPC 查询与修改 HTTP 配置；
//! - 不写具体业务规则（业务规则分属 identity/share/access/httpapi 模块）。
use std::net::SocketAddr;
use std::sync::Arc;

use rusqlite::params;

use crate::config::AppPaths;
use crate::db::Db;
use crate::error::{AppError, AppResult};
use crate::identity::NodeIdentity;

pub struct AppService {
    /// 保留路径配置，供后续"存储位置自定义（§21）"使用
    #[allow(dead_code)]
    pub paths: AppPaths,
    pub db: Arc<Db>,
    pub identity: Arc<NodeIdentity>,
}

impl AppService {
    /// 初始化全部本地核心组件（幂等：重复启动只加载已有数据）。
    pub fn init(paths: AppPaths) -> AppResult<Arc<Self>> {
        paths.ensure_dirs()?;
        let db = Db::open(&paths)?;
        let identity = NodeIdentity::load_or_create(&paths)?;
        Ok(Arc::new(Self {
            paths,
            db,
            identity,
        }))
    }

    /// HTTP 服务是否启用（settings.http_enabled，默认关闭——HTTP 共享是附加功能，
    /// 不会随软件启动自动开启，需用户在仪表盘手动开启）。
    pub fn http_enabled(&self) -> bool {
        self.get_setting("http_enabled")
            .ok()
            .flatten()
            .map(|value| value == "1")
            .unwrap_or(false)
    }

    /// HTTP 监听地址（settings.http_listen，默认 0.0.0.0:58080）。
    pub fn http_addr(&self) -> AppResult<SocketAddr> {
        let raw = self
            .get_setting("http_listen")?
            .unwrap_or_else(|| "0.0.0.0:58080".to_string());
        raw.parse::<SocketAddr>()
            .map_err(|err| AppError::BadRequest(format!("监听地址非法 {raw}: {err}")))
    }

    pub fn set_http_config(&self, enabled: bool, listen: Option<String>) -> AppResult<()> {
        // Validate first so a malformed address cannot partially change the
        // enabled flag in the settings table.
        let validated_listen = listen
            .as_deref()
            .map(|addr| {
                addr.parse::<SocketAddr>()
                    .map_err(|err| AppError::BadRequest(format!("监听地址非法: {err}")))
            })
            .transpose()?;

        self.set_setting("http_enabled", if enabled { "1" } else { "0" })?;
        if let Some(addr) = validated_listen {
            let addr = addr.to_string();
            self.set_setting("http_listen", &addr)?;
        }
        Ok(())
    }

    pub fn get_setting(&self, key: &str) -> AppResult<Option<String>> {
        let conn = self.db.conn();
        let value = conn.query_row(
            "SELECT value FROM settings WHERE key=?1",
            params![key],
            |row| row.get(0),
        );
        match value {
            Ok(value) => Ok(Some(value)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(err) => Err(err.into()),
        }
    }

    pub fn set_setting(&self, key: &str, value: &str) -> AppResult<()> {
        let conn = self.db.conn();
        conn.execute(
            "INSERT INTO settings (key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value=excluded.value",
            params![key, value],
        )?;
        Ok(())
    }
}
