use serde::Serialize;
use serde_json::Value;
use std::{
    sync::{
        atomic::{AtomicU64, Ordering},
        Mutex,
    },
    time::Duration,
};
use tauri::{ipc::Channel, AppHandle, State};
use tauri_plugin_updater::{Update, UpdaterExt};
use url::Url;

const REQUEST_TIMEOUT: Duration = Duration::from_secs(12);

#[derive(Default)]
pub struct PendingUpdateState(Mutex<Option<PendingUpdate>>);

struct PendingUpdate {
    update: Update,
    downloaded: Option<Vec<u8>>,
    fallback_endpoints: Vec<String>,
    fallback_download_urls: Vec<String>,
    protocol_version: Option<u16>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateInfo {
    pub version: String,
    pub current_version: String,
    pub notes: Option<String>,
    pub published_at: Option<String>,
    pub download_url: String,
    pub release_page_url: String,
    pub channel: String,
    pub protocol_version: Option<u16>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DownloadProgress {
    pub downloaded_bytes: u64,
    pub total_bytes: Option<u64>,
    pub finished: bool,
}

enum ProbeResult {
    HealthyNoUpdate,
    HealthyManifest,
}

fn trusted_public_key() -> Result<&'static str, String> {
    match option_env!("NODESEND_UPDATER_PUBLIC_KEY") {
        Some(key) if !key.trim().is_empty() => Ok(key),
        _ => Err("The update signing public key is not configured; update checks cannot be performed securely.".into()),
    }
}

fn validate_endpoint(value: &str) -> Result<Url, String> {
    let url = Url::parse(value).map_err(|_| "Invalid update address format.".to_string())?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err("Update address must be a valid HTTP or HTTPS URL.".into());
    }
    Ok(url)
}

async fn probe_endpoint(endpoint: &Url) -> Result<ProbeResult, String> {
    let client = reqwest::Client::builder()
        .timeout(REQUEST_TIMEOUT)
        .redirect(reqwest::redirect::Policy::limited(3))
        .build()
        .map_err(|error| error.to_string())?;
    let response = client
        .get(endpoint.clone())
        .send()
        .await
        .map_err(|error| error.to_string())?;
    if response.status() == reqwest::StatusCode::NO_CONTENT {
        return Ok(ProbeResult::HealthyNoUpdate);
    }
    if !response.status().is_success() {
        return Err(format!("HTTP {}", response.status()));
    }
    let body = response
        .bytes()
        .await
        .map_err(|error| format!("Failed to read update manifest response: {error}"))?;
    let json_body = body.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(&body);
    let payload: Value = serde_json::from_slice(json_body).map_err(|error| {
        let preview = String::from_utf8_lossy(&body);
        let preview = preview.chars().take(160).collect::<String>();
        let preview = preview.replace('\n', " ").replace('\r', " ");
        format!("Update manifest is not valid JSON: {error}; response: {preview}")
    })?;
    let valid_manifest = payload.get("version").is_some() && payload.get("platforms").is_some()
        || payload.get("version").is_some()
            && payload.get("url").is_some()
            && payload.get("signature").is_some();
    if !valid_manifest {
        return Err("Response does not match the Tauri update manifest format.".into());
    }
    Ok(ProbeResult::HealthyManifest)
}

async fn check_source(
    app: &AppHandle,
    endpoint: &Url,
    public_key: &str,
) -> Result<Option<Update>, tauri_plugin_updater::Error> {
    let update = app
        .updater_builder()
        .endpoints(vec![endpoint.clone()])?
        .pubkey(public_key)
        .timeout(REQUEST_TIMEOUT)
        .build()?
        .check()
        .await?;
    Ok(update)
}

async fn download_verified(
    update: &Update,
    on_progress: &Channel<DownloadProgress>,
) -> Result<Vec<u8>, tauri_plugin_updater::Error> {
    let downloaded = AtomicU64::new(0);
    update
        .download(
            |chunk_length, content_length| {
                downloaded.fetch_add(chunk_length as u64, Ordering::Relaxed);
                let _ = on_progress.send(DownloadProgress {
                    downloaded_bytes: downloaded.load(Ordering::Relaxed),
                    total_bytes: content_length,
                    finished: false,
                });
            },
            || {
                let _ = on_progress.send(DownloadProgress {
                    downloaded_bytes: downloaded.load(Ordering::Relaxed),
                    total_bytes: Some(downloaded.load(Ordering::Relaxed)),
                    finished: true,
                });
            },
        )
        .await
}

#[tauri::command]
pub async fn check_update(
    app: AppHandle,
    pending: State<'_, PendingUpdateState>,
    channel: String,
    source_endpoints: Vec<String>,
) -> Result<Option<UpdateInfo>, String> {
    if !matches!(channel.as_str(), "stable" | "test") {
        return Err("Invalid update channel.".into());
    }
    let public_key = trusted_public_key()?;
    if public_key.trim().is_empty() {
        return Err("Update signing public key has not been configured.".into());
    }
    *pending
        .0
        .lock()
        .map_err(|_| "Update status is temporarily unavailable.".to_string())? = None;

    let mut failures = Vec::new();
    let ordered = source_endpoints.clone();

    for (index, raw_endpoint) in ordered.iter().enumerate() {
        let endpoint = match validate_endpoint(raw_endpoint) {
            Ok(endpoint) => endpoint,
            Err(error) => {
                failures.push(error);
                continue;
            }
        };
        let probe = match probe_endpoint(&endpoint).await {
            Ok(result) => result,
            Err(error) => {
                failures.push(format!(
                    "{}：{error}",
                    endpoint.host_str().unwrap_or("Update source")
                ));
                continue;
            }
        };
        if matches!(probe, ProbeResult::HealthyNoUpdate) {
            return Ok(None);
        }
        let update = match check_source(&app, &endpoint, public_key).await {
            Ok(update) => update,
            Err(error) => match &error {
                tauri_plugin_updater::Error::ReleaseNotFound
                | tauri_plugin_updater::Error::TargetNotFound(_)
                | tauri_plugin_updater::Error::TargetsNotFound(_)
                | tauri_plugin_updater::Error::UnsupportedArch
                | tauri_plugin_updater::Error::UnsupportedOs
                | tauri_plugin_updater::Error::Network(_)
                | tauri_plugin_updater::Error::Reqwest(_)
                | tauri_plugin_updater::Error::Http(_)
                | tauri_plugin_updater::Error::InvalidUpdaterFormat
                | tauri_plugin_updater::Error::Serialization(_) => {
                    failures.push(format!(
                        "{}：{error}",
                        endpoint.host_str().unwrap_or("Update source")
                    ));
                    continue;
                }
                _ => return Err(format!("Update manifest validation failed: {error}")),
            },
        };
        let Some(update) = update else {
            return Ok(None);
        };
        let protocol_version = manifest_protocol_version(&update.raw_json);
        let fallback_download_urls = update
            .raw_json
            .get("platforms")
            .and_then(Value::as_object)
            .and_then(|platforms| {
                platforms.values().find(|platform| {
                    platform.get("url").and_then(Value::as_str)
                        == Some(update.download_url.as_str())
                })
            })
            .and_then(|platform| platform.get("fallback_urls"))
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .filter(|url| validate_endpoint(url).is_ok())
            .map(str::to_owned)
            .collect::<Vec<_>>();
        let release_page_url = update
            .raw_json
            .get("release_page_url")
            .or_else(|| update.raw_json.get("releasePageUrl"))
            .and_then(Value::as_str)
            .filter(|url| validate_endpoint(url).is_ok())
            .map(str::to_owned)
            .unwrap_or_else(|| update.download_url.to_string());
        let info = UpdateInfo {
            version: update.version.clone(),
            current_version: update.current_version.clone(),
            notes: update.body.clone(),
            published_at: update.date.as_ref().map(|date| date.to_string()),
            download_url: update.download_url.to_string(),
            release_page_url,
            channel,
            protocol_version,
        };
        *pending
            .0
            .lock()
            .map_err(|_| "Update status is temporarily unavailable.".to_string())? =
            Some(PendingUpdate {
                update,
                downloaded: None,
                fallback_endpoints: ordered.iter().skip(index + 1).cloned().collect(),
                fallback_download_urls,
                protocol_version,
            });
        return Ok(Some(info));
    }

    Err(if failures.is_empty() {
        "No update sources are configured.".into()
    } else {
        format!(
            "All update sources are unavailable: {}",
            failures.join("；")
        )
    })
}

#[tauri::command]
pub async fn download_update(
    app: AppHandle,
    pending: State<'_, PendingUpdateState>,
    on_progress: Channel<DownloadProgress>,
) -> Result<(), String> {
    let public_key = trusted_public_key()?;
    let mut current = pending
        .0
        .lock()
        .map_err(|_| "Update status is temporarily unavailable.".to_string())?
        .take()
        .ok_or_else(|| "Check for updates first.".to_string())?;
    match download_verified(&current.update, &on_progress).await {
        Ok(bytes) => {
            current.downloaded = Some(bytes);
            *pending
                .0
                .lock()
                .map_err(|_| "Update status is temporarily unavailable.".to_string())? =
                Some(current);
            Ok(())
        }
        Err(original_error) => {
            let can_fallback = matches!(
                &original_error,
                tauri_plugin_updater::Error::Io(_)
                    | tauri_plugin_updater::Error::Network(_)
                    | tauri_plugin_updater::Error::Reqwest(_)
                    | tauri_plugin_updater::Error::Http(_)
            );
            if !can_fallback {
                *pending
                    .0
                    .lock()
                    .map_err(|_| "Update status is temporarily unavailable.".to_string())? =
                    Some(current);
                return Err(format!(
                    "Update package signature or version validation failed: {original_error}"
                ));
            }
            let original_url = current.update.download_url.clone();
            for fallback_url in current.fallback_download_urls.clone() {
                let Ok(fallback_url) = validate_endpoint(&fallback_url) else {
                    continue;
                };
                current.update.download_url = fallback_url;
                if let Ok(bytes) = download_verified(&current.update, &on_progress).await {
                    current.downloaded = Some(bytes);
                    *pending
                        .0
                        .lock()
                        .map_err(|_| "Update status is temporarily unavailable.".to_string())? =
                        Some(current);
                    return Ok(());
                }
            }
            current.update.download_url = original_url;
            let expected_version = current.update.version.clone();
            for raw_endpoint in current.fallback_endpoints.clone() {
                let Ok(endpoint) = validate_endpoint(&raw_endpoint) else {
                    continue;
                };
                let Ok(ProbeResult::HealthyManifest) = probe_endpoint(&endpoint).await else {
                    continue;
                };
                let Ok(Some(candidate)) = check_source(&app, &endpoint, public_key).await else {
                    continue;
                };
                if candidate.version != expected_version
                    || manifest_protocol_version(&candidate.raw_json) != current.protocol_version
                {
                    continue;
                }
                let _ = on_progress.send(DownloadProgress {
                    downloaded_bytes: 0,
                    total_bytes: None,
                    finished: false,
                });
                if let Ok(bytes) = download_verified(&candidate, &on_progress).await {
                    current.update = candidate;
                    current.downloaded = Some(bytes);
                    *pending
                        .0
                        .lock()
                        .map_err(|_| "Update status is temporarily unavailable.".to_string())? =
                        Some(current);
                    return Ok(());
                }
            }
            *pending
                .0
                .lock()
                .map_err(|_| "Update status is temporarily unavailable.".to_string())? =
                Some(current);
            Err(format!(
                "Update source download failed, and no backup source provided the same version: {original_error}"
            ))
        }
    }
}

#[tauri::command]
pub async fn install_update(
    app: AppHandle,
    pending: State<'_, PendingUpdateState>,
    confirmed: bool,
) -> Result<(), String> {
    tracing::info!(confirmed, "Received desktop update installation request");
    if !confirmed {
        return Err("User confirmation is required before installing the update.".into());
    }
    let requires_idle = {
        let guard = pending
            .0
            .lock()
            .map_err(|_| "Update status is temporarily unavailable.".to_string())?;
        let current = guard
            .as_ref()
            .ok_or_else(|| "Check for updates and finish downloading first.".to_string())?;
        crate::update_policy::requires_idle_transfers(current.protocol_version)
    };
    let status = crate::cli::service_invoke(app, "get_network_status".into(), Value::Null).await?;
    let blocked = has_blocking_transfers(&status, requires_idle)?;
    if blocked {
        let code = if requires_idle {
            crate::update_policy::BLOCKED_UNFINISHED
        } else {
            crate::update_policy::BLOCKED_ACTIVE
        };
        tracing::warn!(
            requires_idle,
            "Update installation blocked by transfer activity"
        );
        return Err(code.into());
    }
    let mut current = pending
        .0
        .lock()
        .map_err(|_| "Update status is temporarily unavailable.".to_string())?
        .take()
        .ok_or_else(|| "Check for updates and finish downloading first.".to_string())?;
    let Some(bytes) = current.downloaded.take() else {
        *pending
            .0
            .lock()
            .map_err(|_| "Update status is temporarily unavailable.".to_string())? = Some(current);
        return Err("Update download is incomplete.".into());
    };
    tracing::info!(
        bytes = bytes.len(),
        "Downloaded update package; launching installer"
    );
    match current.update.install(&bytes) {
        Ok(()) => {
            tracing::info!("Update installer started");
            Ok(())
        }
        Err(error) => {
            tracing::error!(%error, "Failed to launch the update installer");
            current.downloaded = Some(bytes);
            *pending
                .0
                .lock()
                .map_err(|_| "Update status is temporarily unavailable.".to_string())? =
                Some(current);
            Err(format!("Failed to launch the update installer: {error}"))
        }
    }
}

fn has_blocking_transfers(response: &Value, requires_idle: bool) -> Result<bool, String> {
    if response.get("handled").and_then(Value::as_bool) != Some(true) {
        return Err("The background network service is unavailable.".into());
    }
    let tasks = response
        .pointer("/value/tasks")
        .and_then(Value::as_array)
        .ok_or_else(|| "The background network service returned an invalid status.".to_string())?;
    let statuses = tasks
        .iter()
        .map(|task| task.get("status").and_then(Value::as_str).unwrap_or(""));
    Ok(crate::update_policy::has_blocking_transfers(
        statuses,
        requires_idle,
    ))
}

fn manifest_protocol_version(manifest: &Value) -> Option<u16> {
    manifest
        .get("protocol_version")
        .or_else(|| manifest.get("protocolVersion"))
        .and_then(Value::as_u64)
        .and_then(|version| u16::try_from(version).ok())
}

#[cfg(test)]
mod install_update_tests {
    use super::*;

    #[test]
    fn active_transfers_block_updates_when_protocol_is_unchanged_or_changed() {
        let response = serde_json::json!({
            "handled": true,
            "value": {"tasks": [{"status": "transferring"}]}
        });
        assert!(has_blocking_transfers(&response, false).unwrap());
        assert!(has_blocking_transfers(&response, true).unwrap());
    }

    #[test]
    fn paused_tasks_are_allowed_only_when_protocol_is_unchanged() {
        let response = serde_json::json!({
            "handled": true,
            "value": {"tasks": [{"status": "paused"}, {"status": "completed"}]}
        });
        assert!(!has_blocking_transfers(&response, false).unwrap());
        assert!(has_blocking_transfers(&response, true).unwrap());
    }

    #[test]
    fn missing_service_status_is_reported_instead_of_panicking() {
        assert!(has_blocking_transfers(&serde_json::json!({"handled": false}), false).is_err());
        assert!(
            has_blocking_transfers(&serde_json::json!({"handled": true, "value": {}}), false)
                .is_err()
        );
    }
}

#[tauri::command]
pub fn open_update_url(url: String) -> Result<(), String> {
    let url = validate_endpoint(&url)?;
    #[cfg(target_os = "windows")]
    let result = std::process::Command::new("rundll32.exe")
        .args(["url.dll,FileProtocolHandler", url.as_str()])
        .spawn();
    #[cfg(target_os = "macos")]
    let result = std::process::Command::new("open").arg(url.as_str()).spawn();
    #[cfg(target_os = "linux")]
    let result = std::process::Command::new("xdg-open")
        .arg(url.as_str())
        .spawn();
    result
        .map(|_| ())
        .map_err(|error| format!("Failed to open update link: {error}"))
}
