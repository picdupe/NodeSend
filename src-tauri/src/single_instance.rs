use std::sync::OnceLock;
use tauri::{AppHandle, Runtime};
use windows::core::w;
use windows::Win32::Foundation::CloseHandle;
use windows::Win32::System::Threading::{
    CreateEventW, CreateMutexW, OpenEventW, SetEvent, WaitForSingleObject, EVENT_MODIFY_STATE,
    INFINITE,
};

static MUTEX: OnceLock<usize> = OnceLock::new();
static GUI_MUTEX: OnceLock<usize> = OnceLock::new();
static CLI_MUTEX: OnceLock<usize> = OnceLock::new();
const EVENT_NAME: windows::core::PCWSTR = w!("Global\\NodeSend.Activate");

pub fn acquire() -> bool {
    acquire_inner(true)
}

pub fn acquire_without_activation() -> bool {
    acquire_inner(false)
}

pub fn acquire_cli() -> bool {
    unsafe {
        let Ok(handle) = CreateMutexW(None, false, w!("Global\\NodeSend.CliEntry")) else {
            return false;
        };
        if windows::Win32::Foundation::GetLastError().0 == 183 {
            let _ = CloseHandle(handle);
            return false;
        }
        let _ = CLI_MUTEX.set(handle.0 as usize);
        true
    }
}

pub fn acquire_gui() -> bool {
    unsafe {
        let Ok(handle) = CreateMutexW(None, false, w!("Global\\NodeSend.GuiInstance")) else {
            return false;
        };
        if windows::Win32::Foundation::GetLastError().0 == 183 {
            let _ = CloseHandle(handle);
            if let Ok(event) = OpenEventW(EVENT_MODIFY_STATE, false, EVENT_NAME) {
                let _ = SetEvent(event);
                let _ = CloseHandle(event);
            }
            return false;
        }
        let _ = GUI_MUTEX.set(handle.0 as usize);
        true
    }
}

fn acquire_inner(activate: bool) -> bool {
    unsafe {
        let Ok(handle) = CreateMutexW(None, false, w!("Global\\NodeSend.SingleInstance")) else {
            return true;
        };
        if windows::Win32::Foundation::GetLastError().0 == 183 {
            let _ = CloseHandle(handle);
            if activate {
                if let Ok(event) = OpenEventW(EVENT_MODIFY_STATE, false, EVENT_NAME) {
                    let _ = SetEvent(event);
                    let _ = CloseHandle(event);
                }
            }
            return false;
        }
        let _ = MUTEX.set(handle.0 as usize);
        true
    }
}

pub fn start_listener<R: Runtime>(app: AppHandle<R>) {
    std::thread::spawn(move || unsafe {
        let Ok(event) = CreateEventW(None, false, false, EVENT_NAME) else {
            return;
        };
        loop {
            if WaitForSingleObject(event, INFINITE).0 == 0 {
                let handle = app.clone();
                let app_handle = handle.clone();
                let _ = handle.run_on_main_thread(move || {
                    crate::tray::show_main(&app_handle);
                });
            }
        }
    });
}
