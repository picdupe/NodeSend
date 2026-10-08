//! Local-only native transfer commands. Private keys never cross IPC.
use crate::{
    error::AppResult,
    network::{peers::Peer, NetworkManager, NetworkStatus},
    transfer::{
        model::{ConflictPolicy, Manifest},
        TransferService,
    },
};
use std::sync::Arc;
use tauri::State;
#[tauri::command]
pub async fn describe_receive_directory(app: tauri::AppHandle, root: String) -> AppResult<String> {
    #[cfg(target_os = "android")]
    { crate::android_import::directory_label(app, root).await.map_err(crate::error::AppError::Other) }
    #[cfg(not(target_os = "android"))]
    { let _ = app; Ok(root) }
}
#[tauri::command]
pub async fn get_network_status(
    network: State<'_, Arc<NetworkManager>>,
) -> AppResult<NetworkStatus> {
    Ok(network.status())
}
#[tauri::command]
pub async fn add_endpoint(
    network: State<'_, Arc<NetworkManager>>,
    address: String,
    expected_node_id: Option<String>,
    allow_public: bool,
) -> AppResult<Peer> {
    network
        .add_endpoint(address, expected_node_id, allow_public)
        .await
}
#[tauri::command]
pub async fn send_files(
    network: State<'_, Arc<NetworkManager>>,
    peer_id: String,
    paths: Vec<String>,
    verify_hash: bool,
    allow_public: bool,
) -> AppResult<String> {
    network
        .send(peer_id, paths, verify_hash, allow_public)
        .await
}
#[tauri::command]
pub async fn cancel_send_preflight(
    network: State<'_, Arc<NetworkManager>>,
    peer_id: String,
) -> AppResult<()> {
    network.cancel_preflight(&peer_id);
    Ok(())
}
#[tauri::command]
pub async fn decide_incoming_transfer(
    network: State<'_, Arc<NetworkManager>>,
    id: String,
    accepted: bool,
    destination: Option<String>,
    conflict: ConflictPolicy,
    selected: Option<Vec<String>>,
    trust_device: Option<bool>,
) -> AppResult<()> {
    network
        .decide_and_trust(
            id,
            accepted,
            destination,
            conflict,
            selected,
            trust_device.unwrap_or(false),
        )
        .await
}
#[tauri::command]
pub async fn transfer_action(
    network: State<'_, Arc<NetworkManager>>,
    id: String,
    action: String,
) -> AppResult<()> {
    network.action(id, action).await
}
#[tauri::command]
pub async fn preview_transfer(paths: Vec<String>) -> AppResult<Manifest> {
    TransferService::manifest_preview(paths).await
}
#[tauri::command]
pub async fn stage_text_payload(text: String) -> AppResult<String> {
    let dir = std::env::temp_dir().join("nodesend-text");
    std::fs::create_dir_all(&dir).map_err(|e| crate::error::AppError::Other(e.to_string()))?;
    let path = dir.join(format!("text-{}.txt", uuid::Uuid::new_v4()));
    std::fs::write(&path, text).map_err(|e| crate::error::AppError::Other(e.to_string()))?;
    Ok(path.to_string_lossy().into_owned())
}
#[tauri::command]
pub async fn stage_binary_payload(bytes: Vec<u8>, extension: String) -> AppResult<String> {
    let safe_extension = extension
        .trim_start_matches('.')
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect::<String>();
    let extension = if safe_extension.is_empty() {
        "bin"
    } else {
        &safe_extension
    };
    let dir = std::env::temp_dir().join("nodesend-clipboard");
    std::fs::create_dir_all(&dir).map_err(|e| crate::error::AppError::Other(e.to_string()))?;
    let path = dir.join(format!("clipboard-{}.{}", uuid::Uuid::new_v4(), extension));
    std::fs::write(&path, bytes).map_err(|e| crate::error::AppError::Other(e.to_string()))?;
    Ok(path.to_string_lossy().into_owned())
}
#[tauri::command]
#[cfg(target_os = "windows")]
pub async fn read_clipboard_files() -> AppResult<Vec<String>> {
    use windows::Win32::System::DataExchange::{
        CloseClipboard, GetClipboardData, IsClipboardFormatAvailable, OpenClipboard,
    };
    use windows::Win32::UI::Shell::{DragQueryFileW, HDROP};
    unsafe {
        const CF_HDROP: u32 = 15;
        if IsClipboardFormatAvailable(CF_HDROP).is_err() || OpenClipboard(None).is_err() {
            return Ok(Vec::new());
        }
        let handle = match GetClipboardData(CF_HDROP) {
            Ok(handle) if !handle.is_invalid() => handle,
            _ => {
                let _ = CloseClipboard();
                return Ok(Vec::new());
            }
        };
        let drop = HDROP(handle.0);
        let count = DragQueryFileW(drop, 0xFFFFFFFF, None);
        let mut paths = Vec::with_capacity(count as usize);
        for index in 0..count {
            let length = DragQueryFileW(drop, index, None);
            let mut buffer = vec![0u16; length as usize + 1];
            DragQueryFileW(drop, index, Some(&mut buffer));
            if let Ok(path) = String::from_utf16(&buffer[..length as usize]) {
                if std::path::Path::new(&path).exists() {
                    paths.push(path);
                }
            }
        }
        let _ = CloseClipboard();
        Ok(paths)
    }
}
#[tauri::command]
#[cfg(not(any(target_os = "windows", target_os = "android")))]
pub async fn read_clipboard_files() -> AppResult<Vec<String>> {
    Ok(Vec::new())
}
#[tauri::command]
#[cfg(target_os = "android")]
pub async fn read_clipboard_files(app: tauri::AppHandle) -> AppResult<Vec<String>> {
    crate::android_import::clipboard(app)
        .await
        .map_err(crate::error::AppError::Other)
}
#[tauri::command]
#[cfg(not(target_os = "android"))]
pub async fn choose_send_files(directory: bool) -> AppResult<Vec<String>> {
    let picker = rfd::AsyncFileDialog::new().set_title(if directory {
        "选择发送的目录"
    } else {
        "选择发送的文件"
    });
    let files = if directory {
        picker.pick_folders().await
    } else {
        picker.pick_files().await
    };
    Ok(files
        .unwrap_or_default()
        .iter()
        .map(|f| f.path().to_string_lossy().into_owned())
        .collect())
}
#[tauri::command]
#[cfg(target_os = "android")]
pub async fn choose_send_files(app: tauri::AppHandle, directory: bool, kind: Option<String>) -> AppResult<Vec<String>> {
    crate::android_import::pick(app, directory, kind.unwrap_or_else(|| "files".into()))
        .await
        .map_err(crate::error::AppError::Other)
}

#[tauri::command]
#[cfg(target_os = "android")]
pub async fn take_shared_files(app: tauri::AppHandle) -> AppResult<Vec<String>> {
    crate::android_import::take_shared_files(app)
        .await
        .map_err(crate::error::AppError::Other)
}

#[tauri::command]
#[cfg(not(target_os = "android"))]
pub async fn take_shared_files() -> AppResult<Vec<String>> {
    Ok(Vec::new())
}

#[tauri::command]
#[cfg(not(target_os = "android"))]
pub async fn choose_receive_directory() -> AppResult<Option<String>> {
    Ok(rfd::AsyncFileDialog::new()
        .set_title("选择接收目录")
        .pick_folder()
        .await
        .map(|f| f.path().to_string_lossy().into_owned()))
}
#[tauri::command]
#[cfg(target_os = "android")]
pub async fn choose_receive_directory(app: tauri::AppHandle) -> AppResult<Option<String>> {
    crate::android_import::pick_directory(app)
        .await
        .map(Some)
        .map_err(crate::error::AppError::Other)
}
#[tauri::command]
pub async fn set_network_settings(
    network: State<'_, Arc<NetworkManager>>,
    name: String,
    directory: String,
    policy: String,
) -> AppResult<()> {
    network.settings(name, directory, policy)
}
#[tauri::command]
pub async fn remove_peer(
    network: State<'_, Arc<NetworkManager>>,
    node_id: String,
) -> AppResult<()> {
    network.cancel_preflight(&node_id);
    network.peers.remove(&node_id)
}
