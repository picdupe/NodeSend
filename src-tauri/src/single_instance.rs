//! Windows single-instance guard and activation signal.
use std::sync::OnceLock;
use tauri::{AppHandle, Manager, Runtime};
use windows::core::w;
use windows::Win32::Foundation::CloseHandle;
use windows::Win32::System::Threading::{
    CreateEventW, CreateMutexW, OpenEventW, SetEvent, WaitForSingleObject,
    EVENT_MODIFY_STATE, INFINITE,
};

static MUTEX: OnceLock<usize> = OnceLock::new();
const EVENT_NAME: windows::core::PCWSTR = w!("Global\\NodeSend.Activate");

pub fn acquire() -> bool {
    unsafe {
        let Ok(handle) = CreateMutexW(None, false, w!("Global\\NodeSend.SingleInstance")) else {
            return true;
        };
        if windows::Win32::Foundation::GetLastError().0 == 183 {
            let _ = CloseHandle(handle);
            if let Ok(event) = OpenEventW(EVENT_MODIFY_STATE, false, EVENT_NAME) {
                let _ = SetEvent(event);
                let _ = CloseHandle(event);
            }
            return false;
        }
        let _ = MUTEX.set(handle.0 as usize);
        true
    }
}

pub fn start_listener<R: Runtime>(app: AppHandle<R>) {
    std::thread::spawn(move || unsafe {
        let Ok(event) = CreateEventW(None, false, false, EVENT_NAME) else { return };
        loop {
            if WaitForSingleObject(event, INFINITE).0 == 0 {
                let handle = app.clone();
                let app_handle = handle.clone();
                let _ = handle.run_on_main_thread(move || {
                    if let Some(window) = app_handle.get_webview_window("main") {
                        let _ = window.unminimize();
                        let _ = window.show();
                        let _ = window.set_focus();
                    }
                });
            }
        }
    });
}
