//! 文件功能：NodeSend Rust Service 的组合根（基线 §46/§53）。
//!
//! - 声明全部业务模块，模块之间通过明确的调用关系与共享状态协作；
//! - [`run`]：Tauri 应用入口。setup 阶段完成"目录 → DB/迁移 → 身份"；
//! - HTTP 共享是附加功能，启动时始终关闭；用户必须通过仪表盘显式开启，
//!   运行时即时启停；本地 P2P/Discovery 与 HTTP 共享保持独立；
//! - main 窗口的 UI 通过 invoke_handler 调用 ipc::commands 中的命令；
//! - 桌面端常驻系统托盘：关闭窗口只隐藏界面，退出走托盘菜单（见 [`tray`]）；
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
pub(crate) mod locks;
pub mod network;
pub mod registry;
pub mod service;
pub mod share;
pub mod transfer;
#[cfg(desktop)]
pub mod update_sources;
#[cfg(desktop)]
pub mod tray;
#[cfg(desktop)]
pub mod updater;
pub(crate) mod tuning;

use std::sync::Arc;

use service::AppService;
use tauri::Manager;

/// 构建标记：每个修复批次递增。两端日志都会打印，用来确认"哪台跑的是哪版"
/// —— 排查"发送端已修、接收端还是旧构建"这类问题时必不可少。
pub const BUILD_TAG: &str = "2026-10-11.dl-fix";

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
    #[cfg(desktop)]
    {
        builder = builder.plugin(tauri_plugin_updater::Builder::new().build());
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
            tracing::info!(dir = %data_dir.display(), "应用数据目录");
            let paths = config::AppPaths::new(data_dir);

            #[cfg(desktop)]
            if let Err(error) = update_sources::initialize(app.handle()) {
                tracing::warn!(%error, "初始化更新源配置失败；应用仍将继续启动");
            }

            let service = AppService::init(paths)?;

            let http_manager = Arc::new(httpapi::HttpManager::new());
            let share_link_manager = Arc::new(httpapi::ShareLinkManager::new());
            let protocol_manager = Arc::new(share::servers::ProtocolManager::default());
            tauri::async_runtime::block_on(protocol_manager.restore(service.clone()))?;
            let network_manager =
                tauri::async_runtime::block_on(network::NetworkManager::start(service.clone()))?;
            tracing::info!(
                build = BUILD_TAG,
                node_id = %service.identity.node_id,
                tcp = network_manager.tcp_port,
                quic = network_manager.quic_port,
                discovery = ?network_manager.status().discovery,
                "NodeSend 原生服务已启动"
            );
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
            #[cfg(desktop)]
            app.manage(updater::PendingUpdateState::default());
            // 桌面端常驻系统托盘：关闭窗口后进程继续接收与传输。
            #[cfg(desktop)]
            tray::init(app)?;
            Ok(())
        })
        .on_window_event(|_window, _event| {
            // 桌面端把关闭窗口变成隐藏窗口；退出只能走托盘菜单。
            // 移动端没有托盘，保留系统默认的关闭行为。
            #[cfg(desktop)]
            tray::on_window_event(_window, _event);
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
            ipc::network::preview_status,
            ipc::network::preview_cancel,
            ipc::network::list_task_entries,
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
            #[cfg(desktop)]
            updater::check_update,
            #[cfg(desktop)]
            update_sources::get_update_sources,
            #[cfg(desktop)]
            updater::download_update,
            #[cfg(desktop)]
            updater::install_update,
            #[cfg(desktop)]
            updater::open_update_url,
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
