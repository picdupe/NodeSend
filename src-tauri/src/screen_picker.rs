#[tauri::command]
pub async fn pick_screen_color() -> Result<Option<String>, String> {
    #[cfg(target_os = "windows")]
    {
        return tauri::async_runtime::spawn_blocking(|| {
            std::thread::spawn(pick_windows)
                .join()
                .map_err(|_| "Screen color picker thread failed".to_owned())?
        })
        .await
        .map_err(|error| error.to_string())?;
    }
    #[cfg(not(target_os = "windows"))]
    Err("Native screen color picking is unavailable on this platform".into())
}

#[cfg(target_os = "windows")]
fn pick_windows() -> Result<Option<String>, String> {
    use windows::{
        core::w,
        Wdk::System::SystemServices::RtlGetVersion,
        Win32::{
            Foundation::{COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM},
            Graphics::Dwm::DwmFlush,
            Graphics::Gdi::{
                BeginPaint, BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, CreateSolidBrush,
                DeleteDC, DeleteObject, EndPaint, FillRect, FrameRect, GetDC, GetMonitorInfoW,
                GetPixel, GetStockObject, InvalidateRect, MonitorFromPoint, ReleaseDC,
                SelectObject, SetBkMode, SetStretchBltMode, SetTextColor, StretchBlt, TextOutW,
                UpdateWindow, BLACK_BRUSH, CAPTUREBLT, COLORONCOLOR, HBITMAP, HBRUSH, HDC, HGDIOBJ,
                MONITORINFO, MONITOR_DEFAULTTONEAREST, PAINTSTRUCT, SRCCOPY, TRANSPARENT,
            },
            System::LibraryLoader::GetModuleHandleW,
            System::SystemInformation::OSVERSIONINFOW,
            UI::WindowsAndMessaging::*,
        },
    };
    static ACTIVE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if ACTIVE.swap(true, std::sync::atomic::Ordering::AcqRel) {
        return Err("A screen color picker is already open".into());
    }
    struct ActiveGuard;
    impl Drop for ActiveGuard {
        fn drop(&mut self) {
            ACTIVE.store(false, std::sync::atomic::Ordering::Release);
        }
    }
    let _active = ActiveGuard;
    const COLS: usize = 13;
    const ROWS: usize = 9;
    const PREVIEW_WIDTH: i32 = 150;
    const PREVIEW_HEIGHT: i32 = 140;
    unsafe fn supports_capture_exclusion() -> bool {
        let mut version = OSVERSIONINFOW {
            dwOSVersionInfoSize: std::mem::size_of::<OSVERSIONINFOW>() as u32,
            ..Default::default()
        };
        (unsafe { RtlGetVersion(&mut version).0 >= 0 })
            && (version.dwMajorVersion > 10
                || (version.dwMajorVersion == 10 && version.dwBuildNumber >= 19041))
    }
    struct Capture {
        screen: HDC,
        memory: HDC,
        bitmap: HBITMAP,
        previous: HGDIOBJ,
    }
    impl Capture {
        unsafe fn new() -> windows::core::Result<Self> {
            unsafe {
                let screen = GetDC(None);
                if screen.0.is_null() {
                    return Err(windows::core::Error::from_win32());
                }
                let memory = CreateCompatibleDC(Some(screen));
                if memory.0.is_null() {
                    let error = windows::core::Error::from_win32();
                    ReleaseDC(None, screen);
                    return Err(error);
                }
                let bitmap = CreateCompatibleBitmap(screen, COLS as i32, ROWS as i32);
                if bitmap.0.is_null() {
                    let error = windows::core::Error::from_win32();
                    let _ = DeleteDC(memory);
                    ReleaseDC(None, screen);
                    return Err(error);
                }
                let previous = SelectObject(memory, HGDIOBJ(bitmap.0));
                if previous.0.is_null() || previous.0 as isize == -1 {
                    let error = windows::core::Error::from_win32();
                    let _ = DeleteObject(HGDIOBJ(bitmap.0));
                    let _ = DeleteDC(memory);
                    ReleaseDC(None, screen);
                    return Err(error);
                }
                Ok(Self {
                    screen,
                    memory,
                    bitmap,
                    previous,
                })
            }
        }
    }
    impl Drop for Capture {
        fn drop(&mut self) {
            unsafe {
                SelectObject(self.memory, self.previous);
                let _ = DeleteObject(HGDIOBJ(self.bitmap.0));
                let _ = DeleteDC(self.memory);
                ReleaseDC(None, self.screen);
            }
        }
    }
    struct Selection {
        result: Option<POINT>,
        preview: HWND,
        capture: Capture,
        color: COLORREF,
        include_layered: bool,
    }
    unsafe fn update_preview(state: &mut Selection) {
        unsafe {
            let mut point = POINT::default();
            if GetCursorPos(&mut point).is_err() {
                return;
            }
            let left = GetSystemMetrics(SM_XVIRTUALSCREEN);
            let top = GetSystemMetrics(SM_YVIRTUALSCREEN);
            let right = left + GetSystemMetrics(SM_CXVIRTUALSCREEN);
            let bottom = top + GetSystemMetrics(SM_CYVIRTUALSCREEN);
            let mut monitor = MONITORINFO {
                cbSize: std::mem::size_of::<MONITORINFO>() as u32,
                ..Default::default()
            };
            let area = if GetMonitorInfoW(
                MonitorFromPoint(point, MONITOR_DEFAULTTONEAREST),
                &mut monitor,
            )
            .as_bool()
            {
                monitor.rcMonitor
            } else {
                RECT {
                    left,
                    top,
                    right,
                    bottom,
                }
            };
            let x = if point.x + 24 + PREVIEW_WIDTH <= area.right {
                point.x + 24
            } else {
                point.x - PREVIEW_WIDTH - 24
            };
            let y = if point.y + 24 + PREVIEW_HEIGHT <= area.bottom {
                point.y + 24
            } else {
                point.y - PREVIEW_HEIGHT - 24
            };
            let mut previous = RECT::default();
            let intersects_previous = IsWindowVisible(state.preview).as_bool()
                && GetWindowRect(state.preview, &mut previous).is_ok()
                && point.x - 6 < previous.right
                && point.x + 7 > previous.left
                && point.y - 4 < previous.bottom
                && point.y + 5 > previous.top;
            let _ = SetWindowPos(
                state.preview,
                Some(HWND_TOPMOST),
                x.clamp(area.left, (area.right - PREVIEW_WIDTH).max(area.left)),
                y.clamp(area.top, (area.bottom - PREVIEW_HEIGHT).max(area.top)),
                PREVIEW_WIDTH,
                PREVIEW_HEIGHT,
                SWP_NOACTIVATE | SWP_SHOWWINDOW,
            );
            if intersects_previous {
                // 当指针进入预览原位置时，等窗口移走后再采样，避免取到预览自身。
                let _ = DwmFlush();
            }
            // 一次复制鼠标周围区域到内存，避免逐像素读取桌面造成卡顿。
            let source_left = (point.x - 6).max(left);
            let source_top = (point.y - 4).max(top);
            let source_right = (point.x - 6 + COLS as i32).min(right);
            let source_bottom = (point.y - 4 + ROWS as i32).min(bottom);
            FillRect(
                state.capture.memory,
                &RECT {
                    left: 0,
                    top: 0,
                    right: COLS as i32,
                    bottom: ROWS as i32,
                },
                HBRUSH(GetStockObject(BLACK_BRUSH).0),
            );
            if source_right <= source_left || source_bottom <= source_top {
                return;
            }
            if BitBlt(
                state.capture.memory,
                source_left - (point.x - 6),
                source_top - (point.y - 4),
                source_right - source_left,
                source_bottom - source_top,
                Some(state.capture.screen),
                source_left,
                source_top,
                if state.include_layered {
                    SRCCOPY | CAPTUREBLT
                } else {
                    SRCCOPY
                },
            )
            .is_err()
            {
                return;
            }
            state.color = GetPixel(state.capture.memory, 6, 4);
            let _ = InvalidateRect(Some(state.preview), None, false);
        }
    }
    unsafe extern "system" fn preview_procedure(
        hwnd: HWND,
        msg: u32,
        wp: WPARAM,
        lp: LPARAM,
    ) -> LRESULT {
        unsafe {
            match msg {
                WM_NCHITTEST => LRESULT(HTTRANSPARENT as isize),
                WM_PAINT => {
                    let state = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const Selection;
                    let mut paint = PAINTSTRUCT::default();
                    let dc = BeginPaint(hwnd, &mut paint);
                    let background = CreateSolidBrush(COLORREF(0x00202020));
                    FillRect(
                        dc,
                        &RECT {
                            left: 0,
                            top: 0,
                            right: PREVIEW_WIDTH,
                            bottom: PREVIEW_HEIGHT,
                        },
                        background,
                    );
                    let _ = DeleteObject(HGDIOBJ(background.0));
                    if let Some(state) = state.as_ref() {
                        SetStretchBltMode(dc, COLORONCOLOR);
                        let _ = StretchBlt(
                            dc,
                            10,
                            10,
                            130,
                            90,
                            Some(state.capture.memory),
                            0,
                            0,
                            COLS as i32,
                            ROWS as i32,
                            SRCCOPY,
                        );
                        let marker = RECT {
                            left: 70,
                            top: 50,
                            right: 80,
                            bottom: 60,
                        };
                        let white =
                            HBRUSH(GetStockObject(windows::Win32::Graphics::Gdi::WHITE_BRUSH).0);
                        FrameRect(dc, &marker, white);
                        let color = state.color;
                        let brush = CreateSolidBrush(color);
                        FillRect(
                            dc,
                            &RECT {
                                left: 10,
                                top: 110,
                                right: 34,
                                bottom: 134,
                            },
                            brush,
                        );
                        let _ = DeleteObject(HGDIOBJ(brush.0));
                        SetBkMode(dc, TRANSPARENT);
                        SetTextColor(dc, COLORREF(0x00ffffff));
                        let text: Vec<u16> = format!(
                            "#{:02X}{:02X}{:02X}",
                            color.0 & 255,
                            (color.0 >> 8) & 255,
                            (color.0 >> 16) & 255
                        )
                        .encode_utf16()
                        .collect();
                        let _ = TextOutW(dc, 44, 114, &text);
                    }
                    let _ = EndPaint(hwnd, &paint);
                    LRESULT(0)
                }
                _ => DefWindowProcW(hwnd, msg, wp, lp),
            }
        }
    }
    unsafe extern "system" fn procedure(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
        unsafe {
            match msg {
                WM_MOUSEMOVE => {
                    let state = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut Selection;
                    let preview = if let Some(state) = state.as_mut() {
                        update_preview(state);
                        Some(state.preview)
                    } else {
                        None
                    };
                    // 绘制回调可能重入，先结束 selection 的可变借用。
                    if let Some(preview) = preview {
                        let _ = UpdateWindow(preview);
                    }
                    LRESULT(0)
                }
                WM_LBUTTONDOWN => {
                    let state = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut Selection;
                    if let Some(state) = state.as_mut() {
                        let mut point = POINT::default();
                        if GetCursorPos(&mut point).is_ok() {
                            state.result = Some(point);
                        }
                    }
                    let _ = DestroyWindow(hwnd);
                    LRESULT(0)
                }
                WM_RBUTTONDOWN | WM_CLOSE => {
                    let _ = DestroyWindow(hwnd);
                    LRESULT(0)
                }
                WM_KEYDOWN if wp.0 == 27 => {
                    let _ = DestroyWindow(hwnd);
                    LRESULT(0)
                }
                WM_DESTROY => {
                    let state = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut Selection;
                    if let Some(state) = state.as_mut() {
                        let _ = DestroyWindow(state.preview);
                    }
                    PostQuitMessage(0);
                    LRESULT(0)
                }
                _ => DefWindowProcW(hwnd, msg, wp, lp),
            }
        }
    }
    let result = (|| -> windows::core::Result<Option<String>> {
        unsafe {
            let instance = HINSTANCE(GetModuleHandleW(None)?.0);
            let capture = Capture::new()?;
            let class_name = w!("NodeSendScreenColorPicker");
            let class = WNDCLASSW {
                hInstance: instance,
                lpszClassName: class_name,
                lpfnWndProc: Some(procedure),
                hCursor: LoadCursorW(None, IDC_CROSS)?,
                hbrBackground: HBRUSH(GetStockObject(BLACK_BRUSH).0),
                ..Default::default()
            };
            RegisterClassW(&class);
            let preview_class_name = w!("NodeSendScreenColorPreview");
            let preview_class = WNDCLASSW {
                hInstance: instance,
                lpszClassName: preview_class_name,
                lpfnWndProc: Some(preview_procedure),
                hbrBackground: HBRUSH(GetStockObject(BLACK_BRUSH).0),
                ..Default::default()
            };
            RegisterClassW(&preview_class);
            let preview = CreateWindowExW(
                WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
                preview_class_name,
                w!(""),
                WS_POPUP,
                0,
                0,
                PREVIEW_WIDTH,
                PREVIEW_HEIGHT,
                None,
                None,
                Some(instance),
                None,
            )?;
            if let Err(error) = SetLayeredWindowAttributes(preview, COLORREF(0), 255, LWA_ALPHA) {
                let _ = DestroyWindow(preview);
                return Err(error);
            }
            // 透明覆盖层接收选色点击，避免误触屏幕上的其他程序；覆盖所有显示器。
            let hwnd = CreateWindowExW(
                WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
                class_name,
                w!("NodeSend"),
                WS_POPUP,
                GetSystemMetrics(SM_XVIRTUALSCREEN),
                GetSystemMetrics(SM_YVIRTUALSCREEN),
                GetSystemMetrics(SM_CXVIRTUALSCREEN),
                GetSystemMetrics(SM_CYVIRTUALSCREEN),
                None,
                None,
                Some(instance),
                None,
            )
            .map_err(|error| {
                let _ = DestroyWindow(preview);
                error
            })?;
            let mut selection = Selection {
                result: None,
                preview,
                capture,
                color: COLORREF(0),
                include_layered: false,
            };
            SetWindowLongPtrW(
                hwnd,
                GWLP_USERDATA,
                &mut selection as *mut Selection as isize,
            );
            SetWindowLongPtrW(
                preview,
                GWLP_USERDATA,
                &mut selection as *mut Selection as isize,
            );
            if let Err(error) = SetLayeredWindowAttributes(hwnd, COLORREF(0), 1, LWA_ALPHA) {
                let _ = DestroyWindow(hwnd);
                return Err(error);
            }
            // 仅排除透明覆盖层，使预览窗口可以出现在截图和远程画面中。
            if supports_capture_exclusion() {
                let exclude_overlay =
                    SetWindowDisplayAffinity(hwnd, WDA_EXCLUDEFROMCAPTURE).is_ok();
                selection.include_layered = exclude_overlay;
            }
            let _ = ShowWindow(hwnd, SW_SHOW);
            let _ = SetForegroundWindow(hwnd);
            update_preview(&mut selection);
            let _ = UpdateWindow(preview);
            let mut message = MSG::default();
            loop {
                let status = GetMessageW(&mut message, None, 0, 0).0;
                if status < 0 {
                    let error = windows::core::Error::from_win32();
                    let _ = DestroyWindow(hwnd);
                    return Err(error);
                }
                if status == 0 {
                    break;
                }
                let _ = TranslateMessage(&message);
                DispatchMessageW(&message);
            }
            if IsWindow(Some(hwnd)).as_bool() {
                let _ = DestroyWindow(hwnd);
            }
            let Some(point) = selection.result else {
                return Ok(None);
            };
            // 移除取色窗口后读取选中像素，避免预览窗口影响最终色值。
            std::thread::sleep(std::time::Duration::from_millis(50));
            let dc = GetDC(None);
            let color = GetPixel(dc, point.x, point.y).0;
            ReleaseDC(None, dc);
            if color == u32::MAX {
                return Err(windows::core::Error::from_win32());
            }
            Ok(Some(format!(
                "#{:02X}{:02X}{:02X}",
                color & 255,
                (color >> 8) & 255,
                (color >> 16) & 255
            )))
        }
    })();
    result.map_err(|error| error.to_string())
}
