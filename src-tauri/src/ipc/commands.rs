//! 文件功能：Tauri IPC 命令实现（UI → Service 的全部本机调用入口，基线 §46）。
//!
//! 命令分组：
//! - 身份/服务状态：get_node_info、get_http_status、set_http_config
//! - 共享管理：list_shares、create_share、update_share、delete_share、
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

#[tauri::command]
pub fn get_ui_language(state: State<'_, Arc<AppService>>) -> AppResult<String> {
    Ok(state
        .get_setting("ui_language")?
        .unwrap_or_else(|| "system".to_string()))
}

#[tauri::command]
pub fn set_ui_language(state: State<'_, Arc<AppService>>, language: String) -> AppResult<()> {
    if !["system", "zh-CN", "zh-TW", "en-US", "ja-JP"].contains(&language.as_str()) {
        return Err(crate::error::AppError::BadRequest(
            "Unsupported UI language".into(),
        ));
    }
    state.set_setting("ui_language", &language)
}

/// 查询本机 Node 身份与 HTTP 服务配置。
#[tauri::command]
pub async fn get_node_info(
    state: State<'_, Arc<AppService>>,
    http_manager: State<'_, Arc<crate::httpapi::HttpManager>>,
) -> AppResult<NodeInfo> {
    get_node_info_service(state.inner().clone(), http_manager.inner().clone()).await
}

pub(crate) async fn get_node_info_service(
    state: Arc<AppService>,
    http_manager: Arc<crate::httpapi::HttpManager>,
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
    start_share_link_service(state.inner().clone(), manager.inner().clone()).await
}

pub(crate) async fn start_share_link_service(
    state: Arc<AppService>,
    manager: Arc<crate::httpapi::ShareLinkManager>,
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
    set_http_config_service(state.inner().clone(), http_manager.inner().clone(), config).await
}

pub(crate) async fn set_http_config_service(
    state: Arc<AppService>,
    http_manager: Arc<crate::httpapi::HttpManager>,
    config: HttpConfigInput,
) -> AppResult<()> {
    let previous_enabled = state.http_enabled();
    let previous_listen = state.http_addr()?.to_string();
    let previous_running = http_manager.is_running().await;

    state.set_http_config(config.enabled, config.listen)?;
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
    list_shares_service(state.inner().clone()).await
}

pub(crate) async fn list_shares_service(state: Arc<AppService>) -> AppResult<Vec<ShareConfig>> {
    share_repo::list_all(&state.db)
}

#[tauri::command]
pub async fn share_protocol_status(
    manager: State<'_, Arc<ProtocolManager>>,
) -> AppResult<Vec<ProtocolStatus>> {
    share_protocol_status_service(manager.inner().clone()).await
}

pub(crate) async fn share_protocol_status_service(
    manager: Arc<ProtocolManager>,
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
    create_share_service(state.inner().clone(), manager.inner().clone(), input).await
}

pub(crate) async fn create_share_service(
    state: Arc<AppService>,
    manager: Arc<ProtocolManager>,
    input: CreateShareInput,
) -> AppResult<ShareConfig> {
    let id = generate_id();
    if input.protocol != ShareProtocol::Nodesend && input.access_mode == AccessMode::Trusted {
        return Err(crate::error::AppError::BadRequest(
            "Choose password or open access for third-party protocols.".into(),
        ));
    }
    share_repo::create(&state.db, &id, &input.name, &input.path, input.access_mode)?;
    let share = share_repo::set_protocol(&state.db, &id, input.protocol, input.port)?;
    match manager.start(state.clone(), &share).await {
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
    update_share_service(state.inner().clone(), manager.inner().clone(), id, input).await
}

pub(crate) async fn update_share_service(
    state: Arc<AppService>,
    manager: Arc<ProtocolManager>,
    id: String,
    input: UpdateShareInput,
) -> AppResult<ShareConfig> {
    let previous = share_repo::get_by_id(&state.db, &id)?;
    if previous.protocol != ShareProtocol::Nodesend
        && input.access_mode == Some(AccessMode::Trusted)
    {
        return Err(crate::error::AppError::BadRequest(
            "Third-party protocols do not support trusted-device access.".into(),
        ));
    }
    let share = share_repo::update(&state.db, &id, input.name, input.path, input.access_mode)?;
    restart_share(&state, &manager, share).await
}

#[tauri::command]
pub async fn toggle_share(
    state: State<'_, Arc<AppService>>,
    manager: State<'_, Arc<ProtocolManager>>,
    id: String,
    enabled: bool,
) -> AppResult<ShareConfig> {
    toggle_share_service(state.inner().clone(), manager.inner().clone(), id, enabled).await
}

pub(crate) async fn toggle_share_service(
    state: Arc<AppService>,
    manager: Arc<ProtocolManager>,
    id: String,
    enabled: bool,
) -> AppResult<ShareConfig> {
    let share = share_repo::set_enabled(&state.db, &id, enabled)?;
    restart_share(&state, &manager, share).await
}

#[tauri::command]
pub async fn rotate_share_token(
    state: State<'_, Arc<AppService>>,
    manager: State<'_, Arc<ProtocolManager>>,
    id: String,
) -> AppResult<ShareConfig> {
    rotate_share_token_service(state.inner().clone(), manager.inner().clone(), id).await
}

pub(crate) async fn rotate_share_token_service(
    state: Arc<AppService>,
    manager: Arc<ProtocolManager>,
    id: String,
) -> AppResult<ShareConfig> {
    let share = share_repo::rotate_token(&state.db, &id)?;
    restart_share(&state, &manager, share).await
}

#[tauri::command]
pub async fn delete_share(
    state: State<'_, Arc<AppService>>,
    manager: State<'_, Arc<ProtocolManager>>,
    id: String,
) -> AppResult<()> {
    delete_share_service(state.inner().clone(), manager.inner().clone(), id).await
}

pub(crate) async fn delete_share_service(
    state: Arc<AppService>,
    manager: Arc<ProtocolManager>,
    id: String,
) -> AppResult<()> {
    manager.stop(&id).await;
    share_repo::delete(&state.db, &id)
}

#[tauri::command]
pub async fn list_trusted_nodes(
    state: State<'_, Arc<AppService>>,
) -> AppResult<Vec<registry::TrustedNode>> {
    list_trusted_nodes_service(state.inner().clone()).await
}

pub(crate) async fn list_trusted_nodes_service(
    state: Arc<AppService>,
) -> AppResult<Vec<registry::TrustedNode>> {
    registry::list(&state.db)
}

#[tauri::command]
pub async fn add_trusted_node(
    state: State<'_, Arc<AppService>>,
    node_id: String,
    display_name: Option<String>,
) -> AppResult<()> {
    add_trusted_node_service(state.inner().clone(), node_id, display_name).await
}

pub(crate) async fn add_trusted_node_service(
    state: Arc<AppService>,
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
    remove_trusted_node_service(state.inner().clone(), node_id).await
}

pub(crate) async fn remove_trusted_node_service(
    state: Arc<AppService>,
    node_id: String,
) -> AppResult<()> {
    registry::remove(&state.db, &node_id)
}

async fn restart_share(
    state: &Arc<AppService>,
    manager: &ProtocolManager,
    share: ShareConfig,
) -> AppResult<ShareConfig> {
    manager.stop(&share.id).await;
    match manager.start(Arc::clone(state), &share).await {
        Ok(port) => match share_repo::set_protocol(&state.db, &share.id, share.protocol, port) {
            Ok(updated) => Ok(updated),
            Err(error) => {
                manager.stop(&share.id).await;
                if let Err(disable_error) = share_repo::set_enabled(&state.db, &share.id, false) {
                    tracing::error!(
                        share_id = %share.id,
                        %error,
                        %disable_error,
                        "Sharing listener stopped, but persisting the port and disabled state also failed"
                    );
                    return Err(crate::error::AppError::Other(format!(
                        "Failed to save the sharing port: {error}; disabling sharing also failed: {disable_error}"
                    )));
                }
                tracing::error!(share_id = %share.id, %error, "Failed to save the sharing port; listener stopped and sharing disabled");
                Err(error)
            }
        },
        Err(error) => match share_repo::set_enabled(&state.db, &share.id, false) {
            Ok(_) => {
                tracing::error!(share_id = %share.id, %error, "Share settings saved, but listener restart failed; sharing was disabled");
                Err(error)
            }
            Err(disable_error) => {
                tracing::error!(
                    share_id = %share.id,
                    %error,
                    %disable_error,
                    "Failed to restart the sharing listener and could not persist the disabled state"
                );
                Err(crate::error::AppError::Other(format!(
                    "Failed to restart the sharing listener: {error}; disabling the share also failed: {disable_error}"
                )))
            }
        },
    }
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
