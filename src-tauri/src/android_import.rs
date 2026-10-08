//! Android document-picker and share-intent bridge.
//!
//! Android exposes selected and shared files as `content://` URIs. The transfer
//! service works with local paths, so the native bridge copies each URI to the
//! app cache before it reaches the normal send flow.

#[cfg(target_os = "android")]
use serde::{Deserialize, Serialize};
#[cfg(target_os = "android")]
use tauri::{
    ipc::{Channel, InvokeResponseBody},
    plugin::{Builder, PluginApi, PluginHandle, TauriPlugin},
    AppHandle, Emitter, Manager, Runtime,
};

#[cfg(target_os = "android")]
const PLUGIN_IDENTIFIER: &str = "com.picdupe.nodesend";

#[cfg(target_os = "android")]
pub struct AndroidImport<R: Runtime>(PluginHandle<R>);

#[cfg(target_os = "android")]
static RECEIVE_EXPORT: std::sync::OnceLock<Box<dyn Fn(crate::transfer::model::TaskRecord) -> Result<(), String> + Send + Sync>> = std::sync::OnceLock::new();

#[cfg(target_os = "android")]
pub async fn export_received(task: crate::transfer::model::TaskRecord) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        RECEIVE_EXPORT.get().ok_or("Android 目录服务尚未就绪")?(task)
    }).await.map_err(|e| e.to_string())?
}

#[cfg(target_os = "android")]
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PickArgs {
    directory: bool,
    kind: String,
}

#[cfg(target_os = "android")]
#[derive(Deserialize)]
struct SharedFiles {
    paths: Vec<String>,
}

#[cfg(target_os = "android")]
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct EventHandler {
    handler: Channel,
}

#[cfg(target_os = "android")]
pub fn init<R: Runtime>() -> TauriPlugin<R> {
    Builder::new("nodesend-import")
        .setup(|app, api: PluginApi<R, ()>| {
            let handle = api.register_android_plugin(PLUGIN_IDENTIFIER, "NodeSendImportPlugin")?;
            let app_handle = app.clone();
            handle.run_mobile_plugin::<()>(
                "setEventHandler",
                EventHandler {
                    handler: Channel::new(move |event| {
                        let paths = match event {
                            InvokeResponseBody::Json(payload) => {
                                serde_json::from_str::<SharedFiles>(&payload)
                                    .map(|shared| shared.paths)
                                    .unwrap_or_default()
                            }
                            _ => Vec::new(),
                        };
                        if !paths.is_empty() {
                            let _ = app_handle.emit("nodesend://shared-files", paths);
                        }
                        Ok(())
                    }),
                },
            )?;
            let export_handle = handle.clone();
            let _ = RECEIVE_EXPORT.set(Box::new(move |task| {
                export_handle.run_mobile_plugin::<()>("exportReceived", serde_json::json!({
                    "root": task.destination, "taskId": task.id, "targets": task.targets,
                    "conflict": task.conflict
                })).map_err(|e| e.to_string())
            }));
            app.manage(AndroidImport(handle));
            let notification_app = app.clone();
            tauri::async_runtime::spawn(async move {
                loop {
                    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                    let Some(network) = notification_app.try_state::<std::sync::Arc<crate::network::NetworkManager>>() else { continue; };
                    let tasks = network.transfers.snapshots().into_iter().map(|task| serde_json::json!({
                        "id": task.id, "status": task.status, "direction": task.direction,
                        "peer_name": task.peer_name, "completed_bytes": task.completed_bytes, "total_bytes": task.total_bytes
                    })).collect::<Vec<_>>();
                    let _ = notification_app.state::<AndroidImport<R>>().0.run_mobile_plugin_async::<()>(
                        "updateTransferNotifications", serde_json::json!({ "payload": serde_json::to_string(&tasks).unwrap_or_else(|_| "[]".into()) })
                    ).await;
                }
            });
            Ok(())
        })
        .build()
}

#[cfg(target_os = "android")]
pub async fn pick<R: Runtime>(app: AppHandle<R>, directory: bool, kind: String) -> Result<Vec<String>, String> {
    app.state::<AndroidImport<R>>()
        .0
        .run_mobile_plugin_async::<SharedFiles>("pick", PickArgs { directory, kind })
        .await
        .map(|response| response.paths)
        .map_err(|error| error.to_string())
}

#[cfg(target_os = "android")]
pub async fn clipboard<R: Runtime>(app: AppHandle<R>) -> Result<Vec<String>, String> {
    app.state::<AndroidImport<R>>()
        .0
        .run_mobile_plugin_async::<SharedFiles>("readClipboard", ())
        .await
        .map(|response| response.paths)
        .map_err(|error| error.to_string())
}

#[cfg(target_os = "android")]
pub async fn directory_label<R: Runtime>(app: AppHandle<R>, root: String) -> Result<String, String> {
    #[derive(Deserialize)]
    struct Label { label: String }
    app.state::<AndroidImport<R>>().0.run_mobile_plugin_async::<Label>(
        "describeReceiveDirectory", serde_json::json!({"root": root})
    ).await.map(|v| v.label).map_err(|e| e.to_string())
}

#[cfg(target_os = "android")]
pub async fn pick_directory<R: Runtime>(app: AppHandle<R>) -> Result<String, String> {
    let paths = pick(app, true, "receive".into()).await?;
    paths.into_iter().next().ok_or_else(|| "未选择目录".into())
}

#[cfg(target_os = "android")]
pub async fn take_shared_files<R: Runtime>(app: AppHandle<R>) -> Result<Vec<String>, String> {
    app.state::<AndroidImport<R>>()
        .0
        .run_mobile_plugin_async::<SharedFiles>("takePending", ())
        .await
        .map(|response| response.paths)
        .map_err(|error| error.to_string())
}
