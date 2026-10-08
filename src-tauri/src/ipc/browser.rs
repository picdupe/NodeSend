use crate::{
    browser::{BrowserConnection, BrowserConnectionInput, BrowserListing, BrowserManager},
    error::AppResult,
};
use std::sync::Arc;
use tauri::State;

#[tauri::command]
pub async fn connect_browser(
    browser: State<'_, Arc<BrowserManager>>,
    input: BrowserConnectionInput,
) -> AppResult<BrowserConnection> {
    browser.connect(input).await
}
#[tauri::command]
pub async fn list_browser_connections(
    browser: State<'_, Arc<BrowserManager>>,
) -> AppResult<Vec<BrowserConnection>> {
    Ok(browser.list())
}
#[tauri::command]
pub async fn close_browser(browser: State<'_, Arc<BrowserManager>>, id: String) -> AppResult<()> {
    browser.close(&id)
}
#[tauri::command]
pub async fn reopen_browser(
    browser: State<'_, Arc<BrowserManager>>,
    id: String,
    password: Option<String>,
) -> AppResult<BrowserConnection> {
    browser.reopen(&id, password).await
}
#[tauri::command]
pub async fn test_browser_connection(
    browser: State<'_, Arc<BrowserManager>>,
    input: BrowserConnectionInput,
) -> AppResult<()> {
    browser.test(input).await
}
#[tauri::command]
pub async fn update_browser_connection(
    browser: State<'_, Arc<BrowserManager>>,
    id: String,
    input: BrowserConnectionInput,
) -> AppResult<BrowserConnection> {
    browser.update(&id, input)
}
#[tauri::command]
pub async fn delete_browser_connection(
    browser: State<'_, Arc<BrowserManager>>,
    id: String,
) -> AppResult<()> {
    browser.remove_connection(&id)
}
#[tauri::command]
pub async fn browse_browser(
    browser: State<'_, Arc<BrowserManager>>,
    id: String,
    path: String,
) -> AppResult<BrowserListing> {
    browser.browse(&id, path).await
}
#[tauri::command]
pub async fn download_browser_file(
    browser: State<'_, Arc<BrowserManager>>,
    id: String,
    path: String,
    destination: String,
) -> AppResult<()> {
    browser.download(&id, path, destination).await
}
#[tauri::command]
pub async fn upload_browser_file(
    browser: State<'_, Arc<BrowserManager>>,
    id: String,
    local: String,
    path: String,
) -> AppResult<()> {
    browser.upload(&id, local, path).await
}
#[tauri::command]
pub async fn create_browser_directory(
    browser: State<'_, Arc<BrowserManager>>,
    id: String,
    path: String,
) -> AppResult<()> {
    browser.mkdir(&id, path).await
}
#[tauri::command]
pub async fn delete_browser_entry(
    browser: State<'_, Arc<BrowserManager>>,
    id: String,
    path: String,
) -> AppResult<()> {
    browser.delete(&id, path).await
}
#[tauri::command]
pub async fn rename_browser_entry(
    browser: State<'_, Arc<BrowserManager>>,
    id: String,
    path: String,
    new_name: String,
) -> AppResult<()> {
    browser.rename(&id, path, new_name).await
}
#[tauri::command]
pub async fn move_browser_entry(
    browser: State<'_, Arc<BrowserManager>>,
    id: String,
    path: String,
    target: String,
) -> AppResult<()> {
    browser.move_entry(&id, path, target).await
}

#[tauri::command]
#[cfg(not(target_os = "android"))]
pub async fn choose_browser_download_path(name: String) -> AppResult<Option<String>> {
    Ok(rfd::AsyncFileDialog::new()
        .set_file_name(&name)
        .save_file()
        .await
        .map(|file| file.path().to_string_lossy().into_owned()))
}

#[tauri::command]
#[cfg(target_os = "android")]
pub async fn choose_browser_download_path(_name: String) -> AppResult<Option<String>> {
    Err(crate::error::AppError::Other(
        "Android 保存位置选择器尚未接入，请使用系统文件管理器保存文件".into(),
    ))
}
