//! Desktop update mechanism. Update sources are tried in the configured order.
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

/// Resolve a compile-time trusted public key. It is never accepted from the
/// remotely hosted source configuration or from the update manifest.
fn trusted_public_key() -> Result<&'static str, String> {
    match option_env!("NODESEND_UPDATER_PUBLIC_KEY") {
        Some(key) if !key.trim().is_empty() => Ok(key),
        _ => Err("更新签名公钥尚未配置，暂时无法安全检查更新。".into()),
    }
}

fn validate_endpoint(value: &str) -> Result<Url, String> {
    let url = Url::parse(value).map_err(|_| "更新地址格式无效。".to_string())?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err("更新地址必须使用有效的 HTTP 或 HTTPS 地址。".into());
    }
    Ok(url)
}

/// Probe a source before asking Tauri's signed updater to process it. A valid
/// manifest with no newer version is a healthy source and ends the search.
/// Network/HTTP/invalid-manifest failures may fall through to the next source.
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
        .map_err(|error| format!("读取更新清单响应失败：{error}"))?;
    // Some Windows-side JSON writers emit a UTF-8 BOM. JSON text exchanged
    // over HTTP should be BOM-free, but accepting it here avoids treating an
    // otherwise valid manifest as an unavailable source.
    let json_body = body.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(&body);
    let payload: Value = serde_json::from_slice(json_body).map_err(|error| {
        let preview = String::from_utf8_lossy(&body);
        let preview = preview.chars().take(160).collect::<String>();
        let preview = preview.replace('\n', " ").replace('\r', " ");
        format!("更新清单不是有效 JSON：{error}；响应内容：{preview}")
    })?;
    let valid_manifest = payload.get("version").is_some() && payload.get("platforms").is_some()
        || payload.get("version").is_some()
            && payload.get("url").is_some()
            && payload.get("signature").is_some();
    if !valid_manifest {
        return Err("响应内容不符合 Tauri 更新清单格式。".into());
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

/// Checks sources in the exact order configured by the user. An available
/// source with no newer version is authoritative and ends the check.
#[tauri::command]
pub async fn check_update(
    app: AppHandle,
    pending: State<'_, PendingUpdateState>,
    channel: String,
    source_endpoints: Vec<String>,
) -> Result<Option<UpdateInfo>, String> {
    if !matches!(channel.as_str(), "stable" | "test") {
        return Err("更新通道无效。".into());
    }
    let public_key = trusted_public_key()?;
    if public_key.trim().is_empty() {
        return Err("更新签名公钥尚未配置。".into());
    }
    *pending
        .0
        .lock()
        .map_err(|_| "更新状态暂时不可用。".to_string())? = None;

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
                    endpoint.host_str().unwrap_or("更新源")
                ));
                continue;
            }
        };
        if matches!(probe, ProbeResult::HealthyNoUpdate) {
            return Ok(None);
        }
        // Once a valid manifest is reachable, signature/version errors are
        // authoritative failures; do not hide them by silently switching source.
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
                        endpoint.host_str().unwrap_or("更新源")
                    ));
                    continue;
                }
                _ => return Err(format!("更新清单验证失败：{error}")),
            },
        };
        let Some(update) = update else {
            return Ok(None);
        };
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
        };
        *pending
            .0
            .lock()
            .map_err(|_| "更新状态暂时不可用。".to_string())? = Some(PendingUpdate {
            update,
            downloaded: None,
            fallback_endpoints: ordered.iter().skip(index + 1).cloned().collect(),
            fallback_download_urls,
        });
        return Ok(Some(info));
    }

    Err(if failures.is_empty() {
        "尚未配置可用的更新源。".into()
    } else {
        format!("所有更新源均不可用：{}", failures.join("；"))
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
        .map_err(|_| "更新状态暂时不可用。".to_string())?
        .take()
        .ok_or_else(|| "请先检查更新。".to_string())?;
    match download_verified(&current.update, &on_progress).await {
        Ok(bytes) => {
            current.downloaded = Some(bytes);
            *pending
                .0
                .lock()
                .map_err(|_| "更新状态暂时不可用。".to_string())? = Some(current);
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
                    .map_err(|_| "更新状态暂时不可用。".to_string())? = Some(current);
                return Err(format!("更新包签名或版本验证失败：{original_error}"));
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
                        .map_err(|_| "更新状态暂时不可用。".to_string())? = Some(current);
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
                if candidate.version != expected_version {
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
                        .map_err(|_| "更新状态暂时不可用。".to_string())? = Some(current);
                    return Ok(());
                }
            }
            *pending
                .0
                .lock()
                .map_err(|_| "更新状态暂时不可用。".to_string())? = Some(current);
            Err(format!(
                "更新源下载失败，且未能从备用源取得相同版本：{original_error}"
            ))
        }
    }
}

/// The frontend calls this only after an explicit confirmation. Active transfer
/// tasks block installation; paused/failed/terminal history does not.
#[tauri::command]
pub fn install_update(
    pending: State<'_, PendingUpdateState>,
    network: State<'_, std::sync::Arc<crate::network::NetworkManager>>,
    confirmed: bool,
) -> Result<(), String> {
    if !confirmed {
        return Err("需要用户确认后才能安装更新。".into());
    }
    let active = network.status().tasks.iter().any(|task| {
        matches!(
            task.status.as_str(),
            "queued" | "connecting" | "waiting" | "accepted" | "transferring" | "interrupted"
        )
    });
    if active {
        return Err("当前仍有传输任务，请完成或暂停传输后再安装更新。".into());
    }
    let mut current = pending
        .0
        .lock()
        .map_err(|_| "更新状态暂时不可用。".to_string())?
        .take()
        .ok_or_else(|| "请先检查更新并完成下载。".to_string())?;
    let Some(bytes) = current.downloaded.take() else {
        *pending
            .0
            .lock()
            .map_err(|_| "更新状态暂时不可用。".to_string())? = Some(current);
        return Err("更新尚未下载完成。".into());
    };
    match current.update.install(&bytes) {
        Ok(()) => Ok(()),
        Err(error) => {
            current.downloaded = Some(bytes);
            *pending
                .0
                .lock()
                .map_err(|_| "更新状态暂时不可用。".to_string())? = Some(current);
            Err(format!("启动更新安装程序失败：{error}"))
        }
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
        .map_err(|error| format!("无法打开更新链接：{error}"))
}
