use serde_json::{json, Value};
use std::{collections::HashSet, fs, path::PathBuf};
use tauri::{AppHandle, Manager};

const DEFAULT_CONFIG: &str = include_str!("../../update-sources.json");

fn config_path(app: &AppHandle) -> Result<PathBuf, String> {
    let tauri_dir = app
        .path()
        .app_data_dir()
        .map_err(|error| error.to_string())?;
    let data_dir = crate::config::app_data_dir(tauri_dir).map_err(|error| error.to_string())?;
    Ok(data_dir.join("update-sources.json"))
}

fn source_id(source: &Value) -> Option<&str> {
    source.get("id").and_then(Value::as_str)
}

fn merge_defaults(config: &mut Value, defaults: &Value) -> bool {
    let mut changed = false;
    if !config.is_object() {
        changed = true;
    }
    let object = config.as_object_mut().expect("object initialized above");
    if !object.contains_key("schema_version") {
        object.insert("schema_version".into(), json!(1));
        changed = true;
    }
    if !object.contains_key("config_version") {
        object.insert("config_version".into(), json!(1));
        changed = true;
    }
    if !object.get("channels").is_some_and(Value::is_object) {
        object.insert("channels".into(), json!({}));
        changed = true;
    }

    let config_channels = config
        .get_mut("channels")
        .and_then(Value::as_object_mut)
        .unwrap();
    let default_channels = defaults.get("channels").and_then(Value::as_object).unwrap();
    for (channel, default_channel) in default_channels {
        if !config_channels.get(channel).is_some_and(Value::is_object) {
            config_channels.insert(channel.clone(), json!({"sources": []}));
            changed = true;
        }
        let channel_config = config_channels
            .get_mut(channel)
            .and_then(Value::as_object_mut)
            .unwrap();
        if !channel_config.get("sources").is_some_and(Value::is_array) {
            let mut legacy_sources = Vec::new();
            if let Some(intranet) = channel_config
                .get("intranet_sources")
                .and_then(Value::as_array)
            {
                legacy_sources.extend(intranet.iter().cloned());
            }
            if let Some(public) = channel_config
                .get("public_source")
                .filter(|value| value.is_object())
            {
                legacy_sources.push(public.clone());
            }
            channel_config.insert("sources".into(), Value::Array(legacy_sources));
            changed = true;
        }
        let sources = channel_config
            .get_mut("sources")
            .and_then(Value::as_array_mut)
            .unwrap();
        let mut ids: HashSet<String> = sources
            .iter()
            .filter_map(source_id)
            .map(str::to_owned)
            .collect();
        if let Some(default_sources) = default_channel.get("sources").and_then(Value::as_array) {
            for source in default_sources {
                let Some(id) = source_id(source) else {
                    continue;
                };
                if ids.insert(id.to_owned()) {
                    sources.push(source.clone());
                    changed = true;
                }
            }
        }
    }
    changed
}

fn write_atomically(path: &PathBuf, contents: &[u8]) -> Result<(), String> {
    let temporary = path.with_extension("json.tmp");
    let backup = path.with_extension("json.bak");
    fs::write(&temporary, contents).map_err(|error| format!("Failed to write temporary update source configuration: {error}"))?;
    if path.exists() {
        if backup.exists() {
            fs::remove_file(&backup).map_err(|error| format!("Failed to remove update source configuration backup: {error}"))?;
        }
        fs::rename(path, &backup).map_err(|error| format!("Failed to back up the existing update source configuration: {error}"))?;
    }
    if let Err(error) = fs::rename(&temporary, path) {
        if backup.exists() {
            let _ = fs::rename(&backup, path);
        }
        return Err(format!("Failed to save update source configuration: {error}"));
    }
    if backup.exists() {
        let _ = fs::remove_file(backup);
    }
    Ok(())
}

fn load_and_merge(path: &PathBuf) -> Result<Value, String> {
    let defaults: Value =
        serde_json::from_str(DEFAULT_CONFIG).map_err(|error| error.to_string())?;
    if !path.exists() {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|error| format!("Failed to create update configuration directory: {error}"))?;
        }
        fs::write(
            path,
            serde_json::to_vec_pretty(&defaults).map_err(|error| error.to_string())?,
        )
        .map_err(|error| format!("Failed to initialize update source configuration: {error}"))?;
        return Ok(defaults);
    }

    let contents =
        fs::read_to_string(path).map_err(|error| format!("Failed to read update source configuration: {error}"))?;
    let mut config: Value = serde_json::from_str(&contents).map_err(|error| {
        format!(
            "update-sources.json is invalid; the existing file was preserved to protect user settings: {error}"
        )
    })?;
    if merge_defaults(&mut config, &defaults) {
        write_atomically(
            path,
            &serde_json::to_vec_pretty(&config).map_err(|error| error.to_string())?,
        )?;
    }
    Ok(config)
}

pub fn load_sources(app: &AppHandle, channel: &str) -> Result<Vec<String>, String> {
    if !matches!(channel, "stable" | "test") {
        return Err("Invalid update channel.".into());
    }
    let config = load_and_merge(&config_path(app)?)?;
    let sources = config
        .pointer(&format!("/channels/{channel}/sources"))
        .and_then(Value::as_array)
        .ok_or_else(|| format!("Update source configuration is missing the {channel}.sources list."))?;
    let mut endpoints = Vec::new();
    for source in sources {
        let Some(url) = source.get("manifest_url").and_then(Value::as_str) else {
            continue;
        };
        if !url.trim().is_empty() {
            endpoints.push(url.to_owned());
        }
    }
    Ok(endpoints)
}

pub fn initialize(app: &AppHandle) -> Result<(), String> {
    load_and_merge(&config_path(app)?).map(|_| ())
}

#[tauri::command]
pub fn get_update_sources(app: AppHandle, channel: String) -> Result<Vec<String>, String> {
    load_sources(&app, &channel)
}
