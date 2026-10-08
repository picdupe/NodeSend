//! 文件功能：Tauri IPC 命令实现（UI → Service 的全部本机调用入口，基线 §46）。
//!
//! 命令分组：
//! - 身份/服务状态：get_node_info、get_http_status、set_http_config
//! - 共享管理：list_shares、create_share、update_share、delete_share、
//!   toggle_share、rotate_share_token
//! - Trust 管理：list_trusted_nodes、add_trusted_node、remove_trusted_node
//!
//! 约定：命令只做参数转发与 DTO 组装，不写业务规则。
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tauri::State;

use crate::error::AppResult;
use crate::registry;
use crate::service::AppService;
use crate::share::repository as share_repo;
use crate::share::{
    servers::{ProtocolManager, ProtocolStatus},
    AccessMode, ShareConfig, ShareProtocol,
};

#[derive(Debug, Serialize)]
pub struct NodeInfo {
    pub node_id: String,
    pub http_enabled: bool,
    pub http_listen: String,
}

/// 查询本机 Node 身份与 HTTP 服务配置。
#[tauri::command]
pub async fn get_node_info(
    state: State<'_, Arc<AppService>>,
    http_manager: State<'_, Arc<crate::httpapi::HttpManager>>,
) -> AppResult<NodeInfo> {
    Ok(NodeInfo {
        node_id: state.identity.node_id.clone(),
        http_enabled: http_manager.is_running().await,
        http_listen: state.http_addr()?.to_string(),
    })
}

/// 生成分享链接所需的独立 HTTP 监听端口；主 HTTP 共享仍保持关闭。
#[tauri::command]
pub async fn start_share_link(
    state: State<'_, Arc<AppService>>,
    manager: State<'_, Arc<crate::httpapi::ShareLinkManager>>,
) -> AppResult<u16> {
    manager.ensure(&state).await
}

#[derive(Debug, Deserialize)]
pub struct HttpConfigInput {
    pub enabled: bool,
    #[serde(default)]
    pub listen: Option<String>,
}

/// 修改 HTTP 服务配置并即时生效（HTTP 共享为附加功能）：
/// 开启 → 持久化设置并绑定端口启动；关闭 → 持久化设置并优雅停止。
#[tauri::command]
pub async fn set_http_config(
    state: State<'_, Arc<AppService>>,
    http_manager: State<'_, Arc<crate::httpapi::HttpManager>>,
    config: HttpConfigInput,
) -> AppResult<()> {
    let previous_enabled = state.http_enabled();
    let previous_listen = state.http_addr()?.to_string();
    let previous_running = http_manager.is_running().await;

    state.set_http_config(config.enabled, config.listen)?;
    // Rebind on every explicit apply so changing the listen address takes
    // effect immediately and never leaves a stale listener running.
    http_manager.stop().await;
    let result = if config.enabled {
        http_manager.apply(&state).await
    } else {
        Ok(())
    };

    if let Err(err) = result {
        state.set_http_config(previous_enabled, Some(previous_listen))?;
        if previous_running && previous_enabled {
            let _ = http_manager.apply(&state).await;
        }
        return Err(err);
    }
    Ok(())
}

#[tauri::command]
pub async fn list_shares(state: State<'_, Arc<AppService>>) -> AppResult<Vec<ShareConfig>> {
    share_repo::list_all(&state.db)
}

#[tauri::command]
pub async fn share_protocol_status(
    manager: State<'_, Arc<ProtocolManager>>,
) -> AppResult<Vec<ProtocolStatus>> {
    Ok(manager.statuses().await)
}

#[derive(Debug, Deserialize)]
pub struct CreateShareInput {
    pub name: String,
    pub path: String,
    #[serde(default = "default_mode")]
    pub access_mode: AccessMode,
    #[serde(default)]
    pub protocol: ShareProtocol,
    #[serde(default)]
    pub port: u16,
}

fn default_mode() -> AccessMode {
    AccessMode::Token
}

#[tauri::command]
pub async fn create_share(
    state: State<'_, Arc<AppService>>,
    manager: State<'_, Arc<ProtocolManager>>,
    input: CreateShareInput,
) -> AppResult<ShareConfig> {
    let id = generate_id();
    if input.protocol != ShareProtocol::Nodesend && input.access_mode == AccessMode::Trusted {
        return Err(crate::error::AppError::BadRequest(
            "第三方协议请选择密码或开放访问。".into(),
        ));
    }
    share_repo::create(&state.db, &id, &input.name, &input.path, input.access_mode)?;
    let share = share_repo::set_protocol(&state.db, &id, input.protocol, input.port)?;
    match manager.start(state.inner().clone(), &share).await {
        Ok(port) => share_repo::set_protocol(&state.db, &id, input.protocol, port),
        Err(err) => {
            let _ = share_repo::delete(&state.db, &id);
            Err(err)
        }
    }
}

#[derive(Debug, Deserialize, Default)]
pub struct UpdateShareInput {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub access_mode: Option<AccessMode>,
}

#[tauri::command]
pub async fn update_share(
    state: State<'_, Arc<AppService>>,
    manager: State<'_, Arc<ProtocolManager>>,
    id: String,
    input: UpdateShareInput,
) -> AppResult<ShareConfig> {
    let previous = share_repo::get_by_id(&state.db, &id)?;
    if previous.protocol != ShareProtocol::Nodesend
        && input.access_mode == Some(AccessMode::Trusted)
    {
        return Err(crate::error::AppError::BadRequest(
            "第三方协议不支持信任设备访问。".into(),
        ));
    }
    let share = share_repo::update(&state.db, &id, input.name, input.path, input.access_mode)?;
    manager.stop(&id).await;
    match manager.start(state.inner().clone(), &share).await {
        Ok(_) => Ok(share),
        Err(err) => {
            share_repo::set_enabled(&state.db, &id, false)?;
            Err(err)
        }
    }
}

#[tauri::command]
pub async fn toggle_share(
    state: State<'_, Arc<AppService>>,
    manager: State<'_, Arc<ProtocolManager>>,
    id: String,
    enabled: bool,
) -> AppResult<ShareConfig> {
    let share = share_repo::set_enabled(&state.db, &id, enabled)?;
    manager.stop(&id).await;
    match manager.start(state.inner().clone(), &share).await {
        Ok(port) => share_repo::set_protocol(&state.db, &id, share.protocol, port),
        Err(err) => {
            share_repo::set_enabled(&state.db, &id, false)?;
            Err(err)
        }
    }
}

#[tauri::command]
pub async fn rotate_share_token(
    state: State<'_, Arc<AppService>>,
    manager: State<'_, Arc<ProtocolManager>>,
    id: String,
) -> AppResult<ShareConfig> {
    let share = share_repo::rotate_token(&state.db, &id)?;
    manager.stop(&id).await;
    match manager.start(state.inner().clone(), &share).await {
        Ok(_) => Ok(share),
        Err(err) => {
            share_repo::set_enabled(&state.db, &id, false)?;
            Err(err)
        }
    }
}

#[tauri::command]
pub async fn delete_share(
    state: State<'_, Arc<AppService>>,
    manager: State<'_, Arc<ProtocolManager>>,
    id: String,
) -> AppResult<()> {
    manager.stop(&id).await;
    share_repo::delete(&state.db, &id)
}

#[tauri::command]
pub async fn list_trusted_nodes(
    state: State<'_, Arc<AppService>>,
) -> AppResult<Vec<registry::TrustedNode>> {
    registry::list(&state.db)
}

#[tauri::command]
pub async fn add_trusted_node(
    state: State<'_, Arc<AppService>>,
    node_id: String,
    display_name: Option<String>,
) -> AppResult<()> {
    registry::add(&state.db, &node_id, display_name)
}

#[tauri::command]
pub async fn remove_trusted_node(
    state: State<'_, Arc<AppService>>,
    node_id: String,
) -> AppResult<()> {
    registry::remove(&state.db, &node_id)
}

/// 生成共享 ID（uuid v4 风格，不引入 uuid 依赖）。
fn generate_id() -> String {
    use rand::RngCore;
    let mut bytes = [0u8; 16];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    bytes[6] = (bytes[6] & 0x0f) | 0x40; // version 4
    bytes[8] = (bytes[8] & 0x3f) | 0x80; // variant 10
    format!(
        "{}-{}-{}-{}-{}",
        hex4(&bytes[0..4]),
        hex2(&bytes[4..6]),
        hex2(&bytes[6..8]),
        hex2(&bytes[8..10]),
        hex4(&bytes[10..16])
    )
}

fn hex4(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
fn hex2(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
