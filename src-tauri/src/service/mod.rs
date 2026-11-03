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
        Self::apply_major_identity_reset(&paths, &db)?;
        let identity = NodeIdentity::load_or_create(&paths)?;
        Ok(Arc::new(Self {
            paths,
            db,
            identity,
        }))
    }

    fn apply_major_identity_reset(paths: &AppPaths, db: &Db) -> AppResult<()> {
        const MARKER: &str = "major_identity_epoch";
        const EPOCH: &str = "crockford-id-v1";
        let current = db.with_conn(|conn| {
            Ok(conn
                .query_row("SELECT value FROM settings WHERE key=?1", [MARKER], |row| {
                    row.get::<_, String>(0)
                })
                .ok())
        })?;
        if current.as_deref() == Some(EPOCH) {
            return Ok(());
        }

        if !NodeIdentity::has_current_identity(paths)? {
            for path in [&paths.identity_key, &paths.identity_cert] {
                match std::fs::remove_file(path) {
                    Ok(()) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => return Err(error.into()),
                }
            }
        }
        db.with_transaction(|tx| {
            tx.execute("DELETE FROM nodes", [])?;
            tx.execute("DELETE FROM node_certificates", [])?;
            tx.execute("DELETE FROM trusted_nodes", [])?;
            tx.execute("DELETE FROM shares", [])?;
            tx.execute("DELETE FROM external_connections", [])?;

            let records: Vec<(String, String)> = {
                let mut stmt = tx.prepare("SELECT id,record FROM transfer_tasks")?;
                let rows = stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
                rows.collect::<Result<_, _>>()?
            };
            for (id, raw) in records {
                let Ok(mut record) = serde_json::from_str::<serde_json::Value>(&raw) else {
                    continue;
                };
                let status = record
                    .get("status")
                    .and_then(|value| value.as_str())
                    .unwrap_or_default();
                if !["completed", "cancelled", "rejected", "partial_completed", "incompatible"].contains(&status) {
                    record["status"] = serde_json::Value::String("incompatible".into());
                    record["error"] = serde_json::Value::String("This transfer task was created by an incompatible app version and cannot be resumed".into());
                    tx.execute(
                        "UPDATE transfer_tasks SET record=?1 WHERE id=?2",
                        rusqlite::params![serde_json::to_string(&record)?, id],
                    )?;
                }
            }

            tx.execute("DELETE FROM settings", [])?;
            tx.execute(
                "INSERT INTO settings(key,value) VALUES('http_enabled','0'),('http_listen','0.0.0.0:58080'),('receive_policy','ask'),(?1,?2)",
                rusqlite::params![MARKER, EPOCH],
            )?;
            Ok(())
        })
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
            .map_err(|err| AppError::BadRequest(format!("Invalid listen address {raw}: {err}")))
    }

    pub fn set_http_config(&self, enabled: bool, listen: Option<String>) -> AppResult<()> {
        let validated_listen = listen
            .as_deref()
            .map(|addr| {
                addr.parse::<SocketAddr>()
                    .map_err(|err| AppError::BadRequest(format!("Invalid listen address: {err}")))
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
        self.db.with_conn(|conn| {
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
        })
    }

    pub fn set_setting(&self, key: &str, value: &str) -> AppResult<()> {
        self.db.with_conn(|conn| {
            conn.execute(
                "INSERT INTO settings (key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value=excluded.value",
                params![key, value],
            )?;
            Ok(())
        })
    }
}

#[cfg(test)]
mod major_identity_reset_tests {
    use super::*;
    use crate::transfer::model::random_id;

    #[test]
    fn major_reset_rotates_identity_once_and_retains_transfer_history() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join(format!("nodesend-major-reset-{}", random_id()));
        let paths = AppPaths::new(root.clone());
        paths.ensure_dirs().unwrap();
        let db = Db::open(&paths).unwrap();
        db.with_conn(|conn| {
            conn.execute("INSERT INTO nodes(node_id,record) VALUES('old','{}')", [])?;
            conn.execute("INSERT INTO trusted_nodes(node_id,created_at) VALUES('old','now')", [])?;
            conn.execute("INSERT INTO transfer_tasks(id,record) VALUES('task','{\"status\":\"transferring\"}')", [])?;
            Ok(())
        })
        .unwrap();
        std::fs::write(&paths.identity_key, b"old-key").unwrap();
        std::fs::write(&paths.identity_cert, b"old-cert").unwrap();

        AppService::apply_major_identity_reset(&paths, &db).unwrap();
        assert!(!paths.identity_key.exists());
        assert!(!paths.identity_cert.exists());
        let task = db
            .with_conn(|conn| {
                Ok(conn.query_row(
                    "SELECT record FROM transfer_tasks WHERE id='task'",
                    [],
                    |row| row.get::<_, String>(0),
                )?)
            })
            .unwrap();
        let task: serde_json::Value = serde_json::from_str(&task).unwrap();
        assert_eq!(task["status"], "incompatible");
        let node_count = db
            .with_conn(|conn| {
                Ok(conn.query_row("SELECT COUNT(*) FROM nodes", [], |row| row.get::<_, i64>(0))?)
            })
            .unwrap();
        assert_eq!(node_count, 0);
        let (trusted_count, task_count) = db
            .with_conn(|conn| {
                Ok((
                    conn.query_row("SELECT COUNT(*) FROM trusted_nodes", [], |row| {
                        row.get::<_, i64>(0)
                    })?,
                    conn.query_row("SELECT COUNT(*) FROM transfer_tasks", [], |row| {
                        row.get::<_, i64>(0)
                    })?,
                ))
            })
            .unwrap();
        assert_eq!(trusted_count, 0);
        assert_eq!(task_count, 1);

        std::fs::write(&paths.identity_key, b"new-key").unwrap();
        AppService::apply_major_identity_reset(&paths, &db).unwrap();
        assert_eq!(std::fs::read(&paths.identity_key).unwrap(), b"new-key");
        drop(db);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn major_reset_preserves_an_existing_current_format_identity() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join(format!("nodesend-current-identity-{}", random_id()));
        let paths = AppPaths::new(root.clone());
        paths.ensure_dirs().unwrap();
        let db = Db::open(&paths).unwrap();
        let identity = NodeIdentity::load_or_create(&paths).unwrap();
        let node_id = identity.node_id.clone();
        let key = std::fs::read(&paths.identity_key).unwrap();
        let cert = std::fs::read(&paths.identity_cert).unwrap();
        db.with_conn(|conn| {
            conn.execute("INSERT INTO nodes(node_id,record) VALUES('old-peer','{}')", [])?;
            conn.execute("INSERT INTO transfer_tasks(id,record) VALUES('task','{\"status\":\"transferring\"}')", [])?;
            Ok(())
        }).unwrap();

        AppService::apply_major_identity_reset(&paths, &db).unwrap();

        assert!(NodeIdentity::has_current_identity(&paths).unwrap());
        assert_eq!(std::fs::read(&paths.identity_key).unwrap(), key);
        assert_eq!(std::fs::read(&paths.identity_cert).unwrap(), cert);
        assert_eq!(
            NodeIdentity::load_or_create(&paths).unwrap().node_id,
            node_id
        );
        let peer_count = db
            .with_conn(|conn| {
                Ok(conn.query_row("SELECT COUNT(*) FROM nodes", [], |row| row.get::<_, i64>(0))?)
            })
            .unwrap();
        let task_count = db
            .with_conn(|conn| {
                Ok(
                    conn.query_row("SELECT COUNT(*) FROM transfer_tasks", [], |row| {
                        row.get::<_, i64>(0)
                    })?,
                )
            })
            .unwrap();
        assert_eq!(peer_count, 0);
        assert_eq!(task_count, 1);

        drop(db);
        std::fs::remove_dir_all(root).unwrap();
    }
}
