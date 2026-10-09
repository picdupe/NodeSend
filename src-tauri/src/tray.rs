//! 文件功能：系统托盘图标与窗口关闭行为（基线 §46/§53）。
//!
//! - 关闭主窗口只隐藏窗口：进程、托盘图标与原生接收/传输继续运行，
//!   真正退出必须通过托盘菜单"退出 NodeSend"；
//! - 左键单击托盘图标切换主窗口显示，右键（菜单）提供"显示主界面 / 退出"；
//! - 托盘只存在于桌面端：模块由 `#[cfg(desktop)]` 排除，移动端没有托盘，
//!   因此保持系统默认的关闭行为，不会留下无法唤回的后台进程。

use tauri::{
    menu::{Menu, MenuItem, PredefinedMenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    App, AppHandle, Manager, Window, WindowEvent,
};

/// 主窗口标签；tauri.conf.json 只声明一个窗口，未显式命名时为 "main"。
const MAIN_WINDOW: &str = "main";
const MENU_SHOW: &str = "tray-show";
const MENU_QUIT: &str = "tray-quit";

/// 创建托盘图标与菜单，在 setup 阶段调用一次。
pub fn init(app: &App) -> tauri::Result<()> {
    let show = MenuItem::with_id(app, MENU_SHOW, "显示主界面", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, MENU_QUIT, "退出 NodeSend", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&show, &PredefinedMenuItem::separator(app)?, &quit])?;

    let mut builder = TrayIconBuilder::with_id("nodesend-tray")
        .menu(&menu)
        .tooltip("NodeSend · 近在身边")
        // 左键用于显示/隐藏主窗口，菜单只在右键弹出。
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id.as_ref() {
            MENU_SHOW => show_main(app),
            MENU_QUIT => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                toggle_main(tray.app_handle());
            }
        });
    // 复用应用图标（tauri.conf.json 的 bundle.icon），不额外打包图片资源。
    if let Some(icon) = app.default_window_icon().cloned() {
        builder = builder.icon(icon);
    }
    builder.build(app)?;
    Ok(())
}

/// 拦截窗口关闭：隐藏窗口但不结束进程，让托盘继续驻留。
pub fn on_window_event(window: &Window, event: &WindowEvent) {
    if let WindowEvent::CloseRequested { api, .. } = event {
        api.prevent_close();
        let _ = window.hide();
    }
}

/// 显示并聚焦主窗口。
pub fn show_main(app: &AppHandle) {
    if let Some(window) = app.get_webview_window(MAIN_WINDOW) {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
    }
}

/// 左键单击托盘：主窗口可见时隐藏，不可见时显示。
fn toggle_main(app: &AppHandle) {
    match app.get_webview_window(MAIN_WINDOW) {
        Some(window) if window.is_visible().unwrap_or(false) => {
            let _ = window.hide();
        }
        Some(_) => show_main(app),
        None => {}
    }
}
