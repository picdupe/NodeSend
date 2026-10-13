//! Android APK update bridge. APK installation is delegated to Android's
//! package installer; NodeSend never attempts a silent install.
use serde::{Deserialize, Serialize};
use tauri::{plugin::{Builder, PluginApi, PluginHandle, TauriPlugin}, AppHandle, Manager, Runtime};

const PLUGIN_IDENTIFIER: &str = "com.picdupe.nodesend";

pub struct AndroidUpdater<R: Runtime>(PluginHandle<R>);

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CheckArgs { channel: String }

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct DownloadArgs { url: String, sha256: String }

#[derive(Deserialize)]
struct DownloadResult { path: String }

#[derive(Serialize)]
struct InstallArgs { path: String }

pub fn init<R: Runtime>() -> TauriPlugin<R> {
    Builder::new("nodesend-updater")
        .setup(|app, api: PluginApi<R, ()>| {
            let handle = api.register_android_plugin(PLUGIN_IDENTIFIER, "NodeSendUpdaterPlugin")?;
            app.manage(AndroidUpdater(handle));
            Ok(())
        })
        .build()
}

#[tauri::command]
pub async fn check_android_update<R: Runtime>(app: AppHandle<R>, channel: String) -> Result<serde_json::Value, String> {
    if !matches!(channel.as_str(), "stable" | "test") { return Err("更新通道无效。".into()); }
    app.state::<AndroidUpdater<R>>().0.run_mobile_plugin_async::<serde_json::Value>("checkUpdate", CheckArgs { channel })
        .await.map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn download_android_update<R: Runtime>(app: AppHandle<R>, url: String, sha256: String) -> Result<String, String> {
    app.state::<AndroidUpdater<R>>().0.run_mobile_plugin_async::<DownloadResult>("downloadUpdate", DownloadArgs { url, sha256 })
        .await.map(|result| result.path).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn install_android_update<R: Runtime>(app: AppHandle<R>, path: String) -> Result<(), String> {
    app.state::<AndroidUpdater<R>>().0.run_mobile_plugin_async("installUpdate", InstallArgs { path })
        .await.map_err(|e| e.to_string())
}
