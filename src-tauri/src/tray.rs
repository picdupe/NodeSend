//! 文件功能：系统托盘图标与窗口关闭行为（基线 §46/§53）。
//!
//! - 关闭主窗口只隐藏窗口：进程、托盘图标与原生接收/传输继续运行，
//!   真正退出必须通过托盘菜单"退出 NodeSend"；
//! - 左键单击托盘图标切换主窗口显示，右键（菜单）提供"显示主界面 / 退出"；
//! - 托盘只存在于桌面端：模块由 `#[cfg(desktop)]` 排除，移动端没有托盘，
//!   因此保持系统默认的关闭行为，不会留下无法唤回的后台进程。

use tauri::{
    image::Image,
    menu::{Menu, MenuItem, PredefinedMenuItem},
    tray::{MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent},
    App, AppHandle, Manager, Runtime, Window, WindowEvent,
};
use std::time::Duration;

/// 主窗口标签；tauri.conf.json 只声明一个窗口，未显式命名时为 "main"。
const MAIN_WINDOW: &str = "main";
const MENU_SHOW: &str = "tray-show";
const MENU_QUIT: &str = "tray-quit";
const TRAY_ID: &str = "nodesend-tray";

/// 创建托盘图标与菜单，在 setup 阶段调用一次。
pub fn init(app: &App) -> tauri::Result<()> {
    let show = MenuItem::with_id(app, MENU_SHOW, "显示主界面", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, MENU_QUIT, "退出 NodeSend", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&show, &PredefinedMenuItem::separator(app)?, &quit])?;

    let mut builder = TrayIconBuilder::with_id(TRAY_ID)
        .menu(&menu)
        .tooltip("NodeSend · 近在身边")
        // 左键只负责显示主窗口，窗口关闭按钮负责隐藏到托盘。
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
                show_main(tray.app_handle());
            }
        });
    // 复用应用图标（tauri.conf.json 的 bundle.icon），不额外打包图片资源。
    if let Some(icon) = app.default_window_icon().cloned() {
        builder = builder.icon(icon);
    }
    let tray = builder.build(app)?;
    let app_handle: AppHandle = AppHandle::clone(tray.app_handle());
    let base_icon = app.default_window_icon().map(|icon| {
        Image::new_owned(icon.rgba().to_vec(), icon.width(), icon.height())
    });
    start_pending_monitor(app_handle, tray, base_icon);
    Ok(())
}

fn start_pending_monitor<R: Runtime>(
    app: AppHandle<R>,
    tray: TrayIcon<R>,
    base_icon: Option<Image<'static>>,
) {
    tauri::async_runtime::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_millis(500));
        let mut previous_count = usize::MAX;
        let mut previous_minimized = false;
        let mut previous_flashing = false;
        let mut flash_on = false;

        loop {
            interval.tick().await;
            let Some(network) = app.try_state::<std::sync::Arc<crate::network::NetworkManager>>() else {
                continue;
            };
            let count = network.pending_manual_receive_count();
            let Some(window) = app.get_webview_window(MAIN_WINDOW) else {
                continue;
            };
            let minimized = window.is_minimized().unwrap_or(false);
            // 参考微信：只有窗口不在前台（隐藏到托盘、最小化或切到其它应用）时，
            // 托盘图标才闪烁提醒；用户回到窗口后立即恢复常亮图标。
            let focused = window.is_focused().unwrap_or(true);
            let flashing = count > 0 && !focused;

            if flashing {
                flash_on = !flash_on;
            } else {
                flash_on = false;
            }
            if count != previous_count || flashing || previous_flashing {
                if let Some(icon) = base_icon.as_ref() {
                    let frame = if flashing && !flash_on {
                        blank_icon(icon)
                    } else {
                        icon.clone()
                    };
                    let _ = tray.set_icon(Some(frame));
                }
            }
            previous_flashing = flashing;
            if count != previous_count {
                let _ = tray.set_tooltip(Some(if count == 0 {
                    "NodeSend · 近在身边".into()
                } else {
                    format!("NodeSend · {count} 个待确认接收请求")
                }));
            }

            #[cfg(target_os = "windows")]
            if count != previous_count || minimized != previous_minimized {
                if let Ok(hwnd) = window.hwnd() {
                    let overlay_count = (minimized && count > 0).then_some(count);
                    let hwnd = hwnd.0 as usize;
                    let _ = window.run_on_main_thread(move || {
                        set_taskbar_overlay(hwnd, overlay_count);
                    });
                }
            }

            previous_count = count;
            previous_minimized = minimized;
        }
    });
}

/// 闪烁的“灭”帧：整帧几乎完全透明（alpha = 1/255）。
///
/// 参考微信：图标是“看不见”而不是被移除，托盘按钮位置始终保留，相邻图标
/// 不会回补占位（那是 `set_visible(false)` 的效果，这里刻意不用）。
///
/// 也不能干脆把 alpha 全设成 0：Windows 会把整张 alpha 为 0 的 32bpp 位图
/// 当成没有 alpha 的旧式位图，改用 AND 掩码抠图，于是托盘里出现黑条/黑块。
/// 留 1/255 的 alpha 能让 GDI 继续走 alpha 合成路径，实际肉眼看就是消失。
fn blank_icon(icon: &Image<'_>) -> Image<'static> {
    let mut rgba = icon.rgba().to_vec();
    for pixel in rgba.chunks_exact_mut(4) {
        pixel[3] = 1;
    }
    Image::new_owned(rgba, icon.width(), icon.height())
}

#[cfg(target_os = "windows")]
fn count_overlay_rgba(count: usize) -> Vec<u8> {
    // 参考 Codex 的任务栏角标：深色圆形底、白色数字，压在图标右上角；
    // 圆形和数字按 4 倍超采样后合成，边缘在任务栏缩放下更平滑。
    const SIZE: usize = 16;
    const SS: usize = 4;
    const DIGITS: [[u8; 7]; 10] = [
        [0b01110, 0b10001, 0b10011, 0b10101, 0b11001, 0b10001, 0b01110],
        [0b00100, 0b01100, 0b00100, 0b00100, 0b00100, 0b00100, 0b01110],
        [0b01110, 0b10001, 0b00001, 0b00010, 0b00100, 0b01000, 0b11111],
        [0b11110, 0b00001, 0b00001, 0b01110, 0b00001, 0b00001, 0b11110],
        [0b00010, 0b00110, 0b01010, 0b10010, 0b11111, 0b00010, 0b00010],
        [0b11111, 0b10000, 0b10000, 0b11110, 0b00001, 0b00001, 0b11110],
        [0b01110, 0b10000, 0b10000, 0b11110, 0b10001, 0b10001, 0b01110],
        [0b11111, 0b00001, 0b00010, 0b00100, 0b01000, 0b01000, 0b01000],
        [0b01110, 0b10001, 0b10001, 0b01110, 0b10001, 0b10001, 0b01110],
        [0b01110, 0b10001, 0b10001, 0b01111, 0b00001, 0b00001, 0b01110],
    ];
    const SMALL_DIGITS: [[u8; 5]; 10] = [
        [0b111, 0b101, 0b101, 0b101, 0b111],
        [0b010, 0b110, 0b010, 0b010, 0b111],
        [0b111, 0b001, 0b111, 0b100, 0b111],
        [0b111, 0b001, 0b111, 0b001, 0b111],
        [0b101, 0b101, 0b111, 0b001, 0b001],
        [0b111, 0b100, 0b111, 0b001, 0b111],
        [0b111, 0b100, 0b111, 0b101, 0b111],
        [0b111, 0b001, 0b010, 0b010, 0b010],
        [0b111, 0b101, 0b111, 0b101, 0b111],
        [0b111, 0b101, 0b111, 0b001, 0b111],
    ];
    const BADGE: [u8; 3] = [42, 44, 48];
    const GLYPH: [u8; 3] = [248, 248, 250];
    const CENTER_X: f32 = 10.0;
    const CENTER_Y: f32 = 5.4;
    const HALF_HEIGHT: f32 = 5.0;

    let label = if count > 999 { "99+".to_string() } else { count.to_string() };
    let compact = label.len() > 1;
    let glyph_width = if compact { 3 } else { 5 };
    let glyph_height = if compact { 5 } else { 7 };
    // 三个字符（99+）时不留字距，保证整段文字不超出徽标。
    let spacing = if label.len() >= 3 { 0 } else { 1 };
    let text_width = label.len() * glyph_width + (label.len() - 1) * spacing;
    // 一两位数字用圆形徽标；三位数字横向拉成胶囊，避免数字挤出圆外。
    let half_width = if text_width > 7 { (text_width as f32 + 1.5) / 2.0 } else { HALF_HEIGHT };
    let corner = HALF_HEIGHT.min(half_width);
    let start_x = (CENTER_X - text_width as f32 / 2.0).round();
    let start_y = (CENTER_Y - glyph_height as f32 / 2.0).round();

    let mut coverage = [0f32; SIZE * SIZE];
    let subpixel = (SS * SS) as f32;
    for sy in 0..SIZE * SS {
        for sx in 0..SIZE * SS {
            let px = (sx as f32 + 0.5) / SS as f32;
            let py = (sy as f32 + 0.5) / SS as f32;
            let qx = ((px - CENTER_X).abs() - (half_width - corner)).max(0.0);
            let qy = ((py - CENTER_Y).abs() - (HALF_HEIGHT - corner)).max(0.0);
            if qx * qx + qy * qy <= corner * corner {
                coverage[(sy / SS) * SIZE + sx / SS] += 1.0 / subpixel;
            }
        }
    }

    let mut glyph = [0f32; SIZE * SIZE];
    for (index, ch) in label.chars().enumerate() {
        let pattern = if compact {
            if let Some(digit) = ch.to_digit(10) {
                SMALL_DIGITS[digit as usize].to_vec()
            } else {
                vec![0, 0b010, 0b111, 0b010, 0]
            }
        } else if let Some(digit) = ch.to_digit(10) {
            DIGITS[digit as usize].to_vec()
        } else {
            vec![0b00100, 0b00100, 0b00100, 0b00100, 0b00100, 0b00000, 0b00100]
        };
        for (row, bits) in pattern.into_iter().enumerate() {
            for col in 0..glyph_width {
                if bits & (1 << (glyph_width - 1 - col)) == 0 {
                    continue;
                }
                let x = (start_x + (index * (glyph_width + spacing) + col) as f32).round() as isize;
                let y = (start_y + row as f32).round() as isize;
                if x >= 0 && y >= 0 && x < SIZE as isize && y < SIZE as isize {
                    glyph[y as usize * SIZE + x as usize] = 1.0;
                }
            }
        }
    }

    let mut rgba = vec![0u8; SIZE * SIZE * 4];
    for index in 0..SIZE * SIZE {
        let background = coverage[index];
        let foreground = glyph[index];
        if background <= 0.0 && foreground <= 0.0 {
            continue;
        }
        let color = if foreground > 0.0 { GLYPH } else { BADGE };
        // 数字按徽标的圆形覆盖率裁剪，避免笔画越出徽标边缘。
        let alpha = if foreground > 0.0 { foreground * background } else { background };
        let offset = index * 4;
        rgba[offset..offset + 4].copy_from_slice(&[
            color[0], color[1], color[2], (alpha.min(1.0) * 255.0).round() as u8,
        ]);
    }
    rgba
}

#[cfg(target_os = "windows")]
fn set_taskbar_overlay(hwnd: usize, count: Option<usize>) {
    use std::{ffi::c_void, mem::size_of, ptr, sync::Mutex};
    use windows::{
        core::PCWSTR,
        Win32::{
            Graphics::Gdi::{
                CreateBitmap, CreateDIBSection, DeleteObject, BITMAPINFO, BITMAPINFOHEADER,
                BI_RGB, DIB_RGB_COLORS,
            },
            System::Com::{
                CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_SERVER,
                COINIT_APARTMENTTHREADED,
            },
            UI::{
                Shell::{ITaskbarList3, TaskbarList},
                WindowsAndMessaging::{CreateIconIndirect, DestroyIcon, ICONINFO, HICON},
            },
        },
    };
    use windows::Win32::Foundation::HWND;

    static ACTIVE_ICON: Mutex<isize> = Mutex::new(0);
    let hwnd = HWND(hwnd as *mut c_void);

    let should_uninitialize = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED).is_ok() };
    let Ok(taskbar) = (unsafe { CoCreateInstance::<_, ITaskbarList3>(&TaskbarList, None, CLSCTX_SERVER) }) else {
        if should_uninitialize {
            unsafe { CoUninitialize() };
        }
        return;
    };
    if unsafe { taskbar.HrInit() }.is_err() {
        if should_uninitialize {
            unsafe { CoUninitialize() };
        }
        return;
    }

    let new_icon = count.and_then(|count| {
        const SIZE: usize = 16;
        let rgba = count_overlay_rgba(count);
        let mut bgra = Vec::with_capacity(rgba.len());
        for pixel in rgba.chunks_exact(4) {
            bgra.extend_from_slice(&[pixel[2], pixel[1], pixel[0], pixel[3]]);
        }

        let bitmap_info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: SIZE as i32,
                biHeight: -(SIZE as i32),
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits = ptr::null_mut();
        let color_bitmap = unsafe {
            CreateDIBSection(None, &bitmap_info, DIB_RGB_COLORS, &mut bits, None, 0)
        }
        .ok()?;
        if bits.is_null() {
            let _ = unsafe { DeleteObject(color_bitmap.into()) };
            return None;
        }
        unsafe {
            std::slice::from_raw_parts_mut(bits.cast::<u8>(), bgra.len()).copy_from_slice(&bgra);
        }

        let mut and_mask = vec![0u8; SIZE * 2];
        for y in 0..SIZE {
            for x in 0..SIZE {
                if rgba[(y * SIZE + x) * 4 + 3] == 0 {
                    and_mask[y * 2 + x / 8] |= 0x80 >> (x % 8);
                }
            }
        }
        let mask_bitmap = unsafe {
            CreateBitmap(
                SIZE as i32,
                SIZE as i32,
                1,
                1,
                Some(and_mask.as_ptr().cast::<c_void>()),
            )
        };
        if mask_bitmap.is_invalid() {
            let _ = unsafe { DeleteObject(color_bitmap.into()) };
            return None;
        }

        let icon_info = ICONINFO {
            fIcon: true.into(),
            xHotspot: 0,
            yHotspot: 0,
            hbmMask: mask_bitmap,
            hbmColor: color_bitmap,
        };
        let icon = unsafe { CreateIconIndirect(&icon_info) };
        unsafe {
            let _ = DeleteObject(mask_bitmap.into());
            let _ = DeleteObject(color_bitmap.into());
        }
        icon.ok()
    });

    let icon_handle = new_icon.unwrap_or_default();
    let set_result = unsafe { taskbar.SetOverlayIcon(hwnd, icon_handle, PCWSTR::null()) };
    if set_result.is_ok() {
        if let Ok(mut active) = ACTIVE_ICON.lock() {
            let previous = std::mem::replace(&mut *active, icon_handle.0 as isize);
            if previous != 0 {
                let _ = unsafe { DestroyIcon(HICON(previous as *mut c_void)) };
            }
        }
    } else if !icon_handle.is_invalid() {
        let _ = unsafe { DestroyIcon(icon_handle) };
    }

    if should_uninitialize {
        unsafe { CoUninitialize() };
    }
}

/// Hide the window on close while keeping the native service and tray alive.
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

