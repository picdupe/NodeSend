//! 文件功能：NodeSend Rust Service 的组合根（基线 §46/§53）。
//!
//! - 声明全部业务模块，模块之间通过明确的调用关系与共享状态协作；
//! - [`run`]：Tauri 应用入口。setup 阶段完成"目录 → DB/迁移 → 身份"；
//! - HTTP 共享是附加功能，启动时始终关闭；用户必须通过仪表盘显式开启，
//!   运行时即时启停；本地 P2P/Discovery 与 HTTP 共享保持独立；
//! - main 窗口的 UI 通过 invoke_handler 调用 ipc::commands 中的命令；
//! - 本文件只负责组合、注册、启动，不写业务逻辑（基线 §53）。
pub mod access;
#[cfg(target_os = "android")]
pub mod android_import;
pub mod browser;
pub mod config;
pub mod db;
pub mod error;
pub mod httpapi;
pub mod identity;
pub mod ipc;
pub mod network;
pub mod registry;
pub mod service;
pub mod share;
pub mod transfer;

use std::sync::Arc;

use service::AppService;
use tauri::Manager;

/// Tauri 桌面应用启动入口（main.rs 调用）。
#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    init_tracing();

    #[allow(unused_mut)]
    let mut builder = tauri::Builder::default();
    // The deep-link plugin currently generates a desktop-only Android helper
    // project. Keep the native plugin on desktop.
    #[cfg(not(target_os = "android"))]
    {
        builder = builder.plugin(tauri_plugin_deep_link::init());
    }
    #[cfg(not(target_os = "android"))]
    {
        builder = builder.plugin(tauri_plugin_clipboard_manager::init());
    }
    #[cfg(target_os = "android")]
    {
        builder = builder.plugin(android_import::init());
    }
    builder
        .setup(|app| {
            // 数据根目录：Tauri 管理的 app_data_dir（基线 §45/§21）。
            let data_dir = app.path().app_data_dir()?;
            let data_dir = config::app_data_dir(data_dir)?;
            let paths = config::AppPaths::new(data_dir);

            let service = AppService::init(paths)?;

            let http_manager = Arc::new(httpapi::HttpManager::new());
            let share_link_manager = Arc::new(httpapi::ShareLinkManager::new());
            let protocol_manager = Arc::new(share::servers::ProtocolManager::default());
            tauri::async_runtime::block_on(protocol_manager.restore(service.clone()))?;
            let network_manager =
                tauri::async_runtime::block_on(network::NetworkManager::start(service.clone()))?;
            let browser_manager = Arc::new(
                browser::BrowserManager::new(service.clone())?
                    .with_peers(network_manager.peers.clone()),
            );

            app.manage(service);
            app.manage(http_manager);
            app.manage(share_link_manager);
            app.manage(protocol_manager);
            app.manage(browser_manager);
            app.manage(network_manager);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            ipc::commands::get_node_info,
            ipc::commands::start_share_link,
            ipc::commands::set_http_config,
            ipc::commands::list_shares,
            ipc::commands::share_protocol_status,
            ipc::commands::create_share,
            ipc::commands::update_share,
            ipc::commands::toggle_share,
            ipc::commands::rotate_share_token,
            ipc::commands::delete_share,
            ipc::commands::list_trusted_nodes,
            ipc::commands::add_trusted_node,
            ipc::commands::remove_trusted_node,
            ipc::network::get_network_status,
            ipc::network::add_endpoint,
            ipc::network::send_files,
            ipc::network::cancel_send_preflight,
            ipc::network::decide_incoming_transfer,
            ipc::network::transfer_action,
            ipc::network::preview_transfer,
            ipc::network::stage_text_payload,
            ipc::network::stage_binary_payload,
            ipc::network::read_clipboard_files,
            ipc::network::choose_send_files,
            ipc::network::take_shared_files,
            ipc::network::choose_receive_directory,
            ipc::network::describe_receive_directory,
            ipc::network::set_network_settings,
            ipc::network::remove_peer,
            ipc::browser::connect_browser,
            ipc::browser::list_browser_connections,
            ipc::browser::close_browser,
            ipc::browser::reopen_browser,
            ipc::browser::test_browser_connection,
            ipc::browser::update_browser_connection,
            ipc::browser::delete_browser_connection,
            ipc::browser::browse_browser,
            ipc::browser::download_browser_file,
            ipc::browser::upload_browser_file,
            ipc::browser::create_browser_directory,
            ipc::browser::delete_browser_entry,
            ipc::browser::rename_browser_entry,
            ipc::browser::move_browser_entry,
            ipc::browser::choose_browser_download_path,
        ])
        .run(tauri::generate_context!())
        .expect("NodeSend 启动失败");
}

/// 初始化日志（tracing）。RUST_LOG 环境变量可覆盖级别，默认 info。
fn init_tracing() {
    use tracing_subscriber::{fmt, EnvFilter};
    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info,sqlx=warn"));
    let _ = fmt().with_env_filter(filter).try_init();
}
