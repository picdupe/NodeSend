//! 文件功能：系统托盘图标与窗口关闭行为（基线 §46/§53）。
//!
//! - 关闭主窗口只隐藏窗口：进程、托盘图标与原生接收/传输继续运行，
//!   真正退出必须通过托盘菜单"退出 NodeSend"；
//! - 左键单击托盘图标切换主窗口显示，右键（菜单）提供"显示主界面 / 退出"；
//! - 托盘只存在于桌面端：模块由 `#[cfg(desktop)]` 排除，移动端没有托盘，
//!   因此保持系统默认的关闭行为，不会留下无法唤回的后台进程。

use std::time::Duration;
use tauri::{
    image::Image,
    menu::{Menu, MenuItem, PredefinedMenuItem},
    tray::{MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent},
    App, AppHandle, Listener, Manager, Runtime, Window, WindowEvent,
};

/// 主窗口标签；tauri.conf.json 只声明一个窗口，未显式命名时为 "main"。
const MAIN_WINDOW: &str = "main";
const MENU_SHOW: &str = "tray-show";
const MENU_QUIT: &str = "tray-quit";
const TRAY_ID: &str = "nodesend-tray";

fn tray_translation(locale: &str) -> serde_json::Value {
    let source = match locale {
        "zh-TW" => include_str!("../../src/i18n/locales/zh-TW/common.json"),
        "en-US" => include_str!("../../src/i18n/locales/en-US/common.json"),
        "ja-JP" => include_str!("../../src/i18n/locales/ja-JP/common.json"),
        _ => include_str!("../../src/i18n/locales/zh-CN/common.json"),
    };
    serde_json::from_str(source).unwrap_or_default()
}

fn native_menu_text(locale: &str) -> (String, String) {
    let bundle = tray_translation(locale);
    let show = bundle["tray"]["show"].as_str().unwrap_or("Show NodeSend");
    let quit = bundle["tray"]["quit"].as_str().unwrap_or("Exit NodeSend");
    (show.to_string(), quit.to_string())
}

fn tray_tooltip(locale: &str, count: usize) -> String {
    if count == 0 {
        return "NodeSend".to_string();
    }
    let label = tray_translation(locale)["tray"]["pendingReceive"]
        .as_str()
        .unwrap_or("{{count}} pending receive requests")
        .replace("{{count}}", &count.to_string());
    format!("NodeSend · {label}")
}

pub fn quit_gui(app: &AppHandle) {
    let _ = tauri::async_runtime::block_on(crate::cli::control(
        "entry_detach",
        vec!["gui".into(), std::process::id().to_string()],
    ));
    app.exit(0);
}

/// 创建托盘图标与菜单，在 setup 阶段调用一次。
pub fn init(app: &App) -> tauri::Result<()> {
    let initial_language = crate::locale::preferred_language();
    let (show_text, quit_text) = native_menu_text(&initial_language);
    let show = MenuItem::with_id(app, MENU_SHOW, show_text, true, None::<&str>)?;
    let quit = MenuItem::with_id(app, MENU_QUIT, quit_text, true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&show, &PredefinedMenuItem::separator(app)?, &quit])?;

    let mut builder = TrayIconBuilder::with_id(TRAY_ID)
        .menu(&menu)
        .tooltip("NodeSend")
        // 左键只负责显示主窗口，窗口关闭按钮负责隐藏到托盘。
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id.as_ref() {
            MENU_SHOW => show_main(app),
            MENU_QUIT => {
                quit_gui(app);
            }
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
    // 运行时从 PNG 读取图标，保留重新着色所需的透明度。
    let png_icon = Image::from_bytes(include_bytes!("../icons/icon.png")).ok();
    let base_icon = png_icon.or_else(|| {
        app.default_window_icon()
            .map(|icon| Image::new_owned(icon.rgba().to_vec(), icon.width(), icon.height()))
    });
    if let Some(icon) = base_icon.clone() {
        builder = builder.icon(icon);
    }
    let tray = builder.build(app)?;
    let language = std::sync::Arc::new(std::sync::Mutex::new(initial_language));
    let language_for_event = language.clone();
    let show_for_event = show.clone();
    let quit_for_event = quit.clone();
    app.listen("nodesend-language-changed", move |event| {
        let locale = event.payload().trim_matches('"').to_string();
        if let Ok(mut current) = language_for_event.lock() {
            *current = locale.clone();
        }
        let (show_text, quit_text) = native_menu_text(&locale);
        let _ = show_for_event.set_text(show_text);
        let _ = quit_for_event.set_text(quit_text);
    });
    let app_handle: AppHandle = AppHandle::clone(tray.app_handle());
    let active_icon = std::sync::Arc::new(std::sync::Mutex::new(base_icon.clone()));
    let icon_for_event = active_icon.clone();
    let tray_for_event = tray.clone();
    let app_for_event = app_handle.clone();
    let source_icon = base_icon.clone();
    app.listen("nodesend-accent-changed", move |event| {
        let color = event.payload().trim_matches('"').to_string();
        let Some(source) = source_icon.as_ref() else {
            return;
        };
        let icon = tint_icon(source, &color);
        if let Ok(mut current) = icon_for_event.lock() {
            *current = Some(icon.clone());
        }
        let _ = tray_for_event.set_icon(Some(icon.clone()));
        if let Some(window) = app_for_event.get_webview_window(MAIN_WINDOW) {
            #[cfg(target_os = "windows")]
            if let Err(error) = set_native_window_icon(&window, &icon) {
                tracing::warn!(%error, "Failed to update the native window icon");
            }
            #[cfg(not(target_os = "windows"))]
            let _ = window.set_icon(icon);
        }
    });
    start_pending_monitor(app_handle, tray, active_icon, language);
    Ok(())
}

fn start_pending_monitor<R: Runtime>(
    app: AppHandle<R>,
    tray: TrayIcon<R>,
    active_icon: std::sync::Arc<std::sync::Mutex<Option<Image<'static>>>>,
    language: std::sync::Arc<std::sync::Mutex<String>>,
) {
    tauri::async_runtime::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_millis(500));
        let mut previous_count = usize::MAX;
        let mut previous_minimized = false;
        let mut previous_flashing = false;
        let mut flash_on = false;
        let mut previous_locale = String::new();

        loop {
            interval.tick().await;
            let count = if let Some(network) =
                app.try_state::<std::sync::Arc<crate::network::NetworkManager>>()
            {
                network.pending_manual_receive_count()
            } else if crate::cli::is_gui_client() {
                crate::cli::pending_count().await
            } else {
                continue;
            };
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
                let icon = active_icon.lock().ok().and_then(|current| current.clone());
                if let Some(icon) = icon.as_ref() {
                    let frame = if flashing && !flash_on {
                        blank_icon(icon)
                    } else {
                        icon.clone()
                    };
                    let _ = tray.set_icon(Some(frame));
                }
            }
            previous_flashing = flashing;
            let locale = language
                .lock()
                .map(|value| value.clone())
                .unwrap_or_else(|_| "zh-CN".to_string());
            if count != previous_count || locale != previous_locale {
                let _ = tray.set_tooltip(Some(tray_tooltip(&locale, count)));
                previous_locale = locale;
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

fn tint_icon(icon: &Image<'_>, color: &str) -> Image<'static> {
    let Some([red, green, blue, alpha]) = parse_accent_color(color) else {
        return Image::new_owned(icon.rgba().to_vec(), icon.width(), icon.height());
    };
    let mut rgba = icon.rgba().to_vec();
    for pixel in rgba.chunks_exact_mut(4) {
        if pixel[3] == 0 {
            continue;
        }
        let luminance =
            (u16::from(pixel[0]) * 54 + u16::from(pixel[1]) * 183 + u16::from(pixel[2]) * 19) >> 8;
        let highlight = ((f32::from(luminance) - 90.0) / 165.0).clamp(0.0, 1.0) * 0.42;
        for (channel, target) in pixel[..3].iter_mut().zip([red, green, blue]) {
            *channel = (f32::from(target) * (1.0 - highlight) + 255.0 * highlight).round() as u8;
        }
        pixel[3] = (u16::from(pixel[3]) * u16::from(alpha) / 255) as u8;
    }
    Image::new_owned(rgba, icon.width(), icon.height())
}

fn parse_accent_color(value: &str) -> Option<[u8; 4]> {
    let hex = value.strip_prefix('#')?;
    if hex.len() != 6 && hex.len() != 8 {
        return None;
    }
    let mut channels = [255; 4];
    for (index, channel) in channels.iter_mut().enumerate().take(hex.len() / 2) {
        *channel = u8::from_str_radix(&hex[index * 2..index * 2 + 2], 16).ok()?;
    }
    Some(channels)
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
fn set_native_window_icon(window: &tauri::WebviewWindow, image: &Image<'_>) -> Result<(), String> {
    use std::{ffi::c_void, sync::Mutex};
    use windows::Win32::{
        Foundation::{HWND, LPARAM, WPARAM},
        UI::WindowsAndMessaging::{DestroyIcon, HICON, ICON_BIG, ICON_SMALL, WM_SETICON},
    };

    fn create_icon(image: &Image<'_>, size: u32) -> Result<HICON, String> {
        use std::{ffi::c_void, mem::size_of, ptr};
        use windows::Win32::Graphics::Gdi::{
            CreateBitmap, CreateDIBSection, DeleteObject, BITMAPINFO, BITMAPINFOHEADER, BI_RGB,
            DIB_RGB_COLORS,
        };
        use windows::Win32::UI::WindowsAndMessaging::{CreateIconIndirect, ICONINFO};

        let width = image.width() as usize;
        let height = image.height() as usize;
        let pixels = image.rgba();
        if width == 0 || height == 0 || pixels.len() != width * height * 4 {
            return Err("Invalid icon image dimensions".into());
        }

        let mut bgra = vec![0u8; size as usize * size as usize * 4];
        let mask_stride = ((size as usize + 15) / 16) * 2;
        let mut and_mask = vec![0u8; mask_stride * size as usize];
        for y in 0..size as usize {
            let source_y = y * height / size as usize;
            for x in 0..size as usize {
                let source_x = x * width / size as usize;
                let source = (source_y * width + source_x) * 4;
                let destination = (y * size as usize + x) * 4;
                let alpha = u16::from(pixels[source + 3]);
                bgra[destination..destination + 4].copy_from_slice(&[
                    (u16::from(pixels[source + 2]) * alpha / 255) as u8,
                    (u16::from(pixels[source + 1]) * alpha / 255) as u8,
                    (u16::from(pixels[source]) * alpha / 255) as u8,
                    alpha as u8,
                ]);
                if pixels[source + 3] == 0 {
                    and_mask[y * mask_stride + x / 8] |= 0x80 >> (x % 8);
                }
            }
        }

        let bitmap_info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: size as i32,
                biHeight: -(size as i32),
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits = ptr::null_mut();
        let color_bitmap =
            unsafe { CreateDIBSection(None, &bitmap_info, DIB_RGB_COLORS, &mut bits, None, 0) }
                .map_err(|error| error.to_string())?;
        if bits.is_null() {
            let _ = unsafe { DeleteObject(color_bitmap.into()) };
            return Err("Windows did not allocate the icon bitmap".into());
        }
        unsafe {
            std::slice::from_raw_parts_mut(bits.cast::<u8>(), bgra.len()).copy_from_slice(&bgra);
        }

        let mask_bitmap = unsafe {
            CreateBitmap(
                size as i32,
                size as i32,
                1,
                1,
                Some(and_mask.as_ptr().cast::<c_void>()),
            )
        };
        if mask_bitmap.is_invalid() {
            let _ = unsafe { DeleteObject(color_bitmap.into()) };
            return Err("Windows could not create the icon transparency mask".into());
        }

        let info = ICONINFO {
            fIcon: true.into(),
            xHotspot: 0,
            yHotspot: 0,
            hbmMask: mask_bitmap,
            hbmColor: color_bitmap,
        };
        let result = unsafe { CreateIconIndirect(&info) }.map_err(|error| error.to_string());
        unsafe {
            let _ = DeleteObject(mask_bitmap.into());
            let _ = DeleteObject(color_bitmap.into());
        }
        result
    }

    static ACTIVE_WINDOW_ICONS: Mutex<[isize; 2]> = Mutex::new([0; 2]);
    let hwnd = HWND(window.hwnd().map_err(|error| error.to_string())?.0);
    let mut active = ACTIVE_WINDOW_ICONS
        .lock()
        .map_err(|_| "Window icon state is unavailable".to_string())?;
    let small = create_icon(image, 16)?;
    let big = match create_icon(image, 32) {
        Ok(icon) => icon,
        Err(error) => {
            let _ = unsafe { DestroyIcon(small) };
            return Err(error);
        }
    };
    unsafe {
        let _ = windows::Win32::UI::WindowsAndMessaging::SendMessageW(
            hwnd,
            WM_SETICON,
            Some(WPARAM(ICON_SMALL as usize)),
            Some(LPARAM(small.0 as isize)),
        );
        let _ = windows::Win32::UI::WindowsAndMessaging::SendMessageW(
            hwnd,
            WM_SETICON,
            Some(WPARAM(ICON_BIG as usize)),
            Some(LPARAM(big.0 as isize)),
        );
    }

    let previous = std::mem::replace(&mut *active, [small.0 as isize, big.0 as isize]);
    for handle in previous {
        if handle != 0 {
            let _ = unsafe { DestroyIcon(HICON(handle as *mut c_void)) };
        }
    }
    Ok(())
}

#[cfg(target_os = "windows")]
fn count_overlay_rgba(count: usize) -> Vec<u8> {
    // 参考 Codex 的任务栏角标：深色圆形底、白色数字，压在图标右上角；
    // 圆形和数字按 4 倍超采样后合成，边缘在任务栏缩放下更平滑。
    const SIZE: usize = 16;
    const SS: usize = 4;
    const DIGITS: [[u8; 7]; 10] = [
        [
            0b01110, 0b10001, 0b10011, 0b10101, 0b11001, 0b10001, 0b01110,
        ],
        [
            0b00100, 0b01100, 0b00100, 0b00100, 0b00100, 0b00100, 0b01110,
        ],
        [
            0b01110, 0b10001, 0b00001, 0b00010, 0b00100, 0b01000, 0b11111,
        ],
        [
            0b11110, 0b00001, 0b00001, 0b01110, 0b00001, 0b00001, 0b11110,
        ],
        [
            0b00010, 0b00110, 0b01010, 0b10010, 0b11111, 0b00010, 0b00010,
        ],
        [
            0b11111, 0b10000, 0b10000, 0b11110, 0b00001, 0b00001, 0b11110,
        ],
        [
            0b01110, 0b10000, 0b10000, 0b11110, 0b10001, 0b10001, 0b01110,
        ],
        [
            0b11111, 0b00001, 0b00010, 0b00100, 0b01000, 0b01000, 0b01000,
        ],
        [
            0b01110, 0b10001, 0b10001, 0b01110, 0b10001, 0b10001, 0b01110,
        ],
        [
            0b01110, 0b10001, 0b10001, 0b01111, 0b00001, 0b00001, 0b01110,
        ],
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

    let label = if count > 999 {
        "99+".to_string()
    } else {
        count.to_string()
    };
    let compact = label.len() > 1;
    let glyph_width = if compact { 3 } else { 5 };
    let glyph_height = if compact { 5 } else { 7 };
    // 三个字符（99+）时不留字距，保证整段文字不超出徽标。
    let spacing = if label.len() >= 3 { 0 } else { 1 };
    let text_width = label.len() * glyph_width + (label.len() - 1) * spacing;
    // 一两位数字用圆形徽标；三位数字横向拉成胶囊，避免数字挤出圆外。
    let half_width = if text_width > 7 {
        (text_width as f32 + 1.5) / 2.0
    } else {
        HALF_HEIGHT
    };
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
            vec![
                0b00100, 0b00100, 0b00100, 0b00100, 0b00100, 0b00000, 0b00100,
            ]
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
        let alpha = if foreground > 0.0 {
            foreground * background
        } else {
            background
        };
        let offset = index * 4;
        rgba[offset..offset + 4].copy_from_slice(&[
            color[0],
            color[1],
            color[2],
            (alpha.min(1.0) * 255.0).round() as u8,
        ]);
    }
    rgba
}

#[cfg(target_os = "windows")]
fn set_taskbar_overlay(hwnd: usize, count: Option<usize>) {
    use std::{ffi::c_void, mem::size_of, ptr, sync::Mutex};
    use windows::Win32::Foundation::HWND;
    use windows::{
        core::PCWSTR,
        Win32::{
            Graphics::Gdi::{
                CreateBitmap, CreateDIBSection, DeleteObject, BITMAPINFO, BITMAPINFOHEADER, BI_RGB,
                DIB_RGB_COLORS,
            },
            System::Com::{
                CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_SERVER,
                COINIT_APARTMENTTHREADED,
            },
            UI::{
                Shell::{ITaskbarList3, TaskbarList},
                WindowsAndMessaging::{CreateIconIndirect, DestroyIcon, HICON, ICONINFO},
            },
        },
    };

    static ACTIVE_ICON: Mutex<isize> = Mutex::new(0);
    let hwnd = HWND(hwnd as *mut c_void);

    let should_uninitialize = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED).is_ok() };
    let Ok(taskbar) =
        (unsafe { CoCreateInstance::<_, ITaskbarList3>(&TaskbarList, None, CLSCTX_SERVER) })
    else {
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
        let color_bitmap =
            unsafe { CreateDIBSection(None, &bitmap_info, DIB_RGB_COLORS, &mut bits, None, 0) }
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

pub fn on_window_event(window: &Window, event: &WindowEvent) {
    if let WindowEvent::CloseRequested { api, .. } = event {
        api.prevent_close();
        let _ = window.hide();
    }
}

/// 显示并聚焦主窗口。
pub fn show_main<R: Runtime>(app: &AppHandle<R>) {
    if let Some(tray) = app.tray_by_id(TRAY_ID) {
        let _ = tray.set_visible(true);
    }
    if let Some(window) = app.get_webview_window(MAIN_WINDOW) {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
    }
}
