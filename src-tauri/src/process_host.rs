use serde_json::{json, Value};
use std::{
    collections::HashMap,
    process::{Command, Stdio},
    sync::{Mutex, OnceLock},
    time::{Duration, Instant},
};

#[derive(Clone, Copy)]
struct Entry {
    pid: u32,
    #[cfg(not(windows))]
    seen: Instant,
    exit: bool,
    ready: bool,
}
#[derive(Default)]
struct Entries {
    entries: HashMap<String, Entry>,
    ever_attached: bool,
    closing: bool,
}
static ENTRIES: OnceLock<Mutex<Entries>> = OnceLock::new();
fn registry() -> std::sync::MutexGuard<'static, Entries> {
    ENTRIES
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}
impl Entries {
    fn reap(&mut self) {
        #[cfg(windows)]
        self.entries
            .retain(|_, entry| crate::cli::process_alive(entry.pid));
        #[cfg(not(windows))]
        self.entries.retain(|_, entry| {
            entry.seen.elapsed() < Duration::from_secs(if entry.ready { 10 } else { 60 })
                && crate::cli::process_alive(entry.pid)
        });
    }
    fn attach(&mut self, kind: &str, pid: u32) -> Result<bool, String> {
        if self.closing {
            return Err("The NodeSend service is shutting down".into());
        }
        if !matches!(kind, "gui" | "cli") && !kind.starts_with("web:") {
            return Err("Invalid process entry kind".into());
        }
        self.reap();
        if self.entries.get(kind).is_some_and(|old| old.pid != pid) {
            return Err(format!("A different {kind} process is already registered"));
        }
        let exit = self.entries.get(kind).is_some_and(|entry| entry.exit);
        self.entries.insert(
            kind.into(),
            Entry {
                pid,
                #[cfg(not(windows))]
                seen: Instant::now(),
                exit,
                ready: true,
            },
        );
        self.ever_attached = true;
        Ok(exit)
    }
}

fn spawn(arguments: &[&str]) -> Result<u32, String> {
    let mut command = Command::new(std::env::current_exe().map_err(|e| e.to_string())?);
    command
        .args(arguments)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let mut child = command.spawn().map_err(|e| e.to_string())?;
    let pid = child.id();
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(pid)
}

pub async fn ensure_service() -> Result<(), String> {
    let mut spawned = false;
    for _ in 0..50 {
        match crate::cli::control("role_probe", vec![]).await {
            Ok(value) if value.get("accepting_entries").and_then(Value::as_bool) != Some(false) => {
                return Ok(())
            }
            Err(_) if !spawned => {
                spawn(&["--service-host"])?;
                spawned = true;
            }
            _ => {}
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    Err("Timed out waiting for the NodeSend service to start".into())
}

pub async fn ensure_cli() -> Result<(), String> {
    ensure_service().await?;
    for _ in 0..50 {
        let value = match crate::cli::control("ensure_cli", vec![]).await {
            Err(error) if error.contains("service is shutting down") => {
                ensure_service().await?;
                continue;
            }
            result => result?,
        };
        if value.get("ready").and_then(Value::as_bool) == Some(true) {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    Err("Timed out waiting for the CLI process to start".into())
}

pub async fn control(command: &str, args: &[String]) -> Option<Result<Value, String>> {
    if matches!(command, "ensure_cli" | "open_gui") && registry().closing {
        return Some(Err("The NodeSend service is shutting down".into()));
    }
    let result = match command {
        "spawn_web_entry" => {
            let parsed = (|| {
                let uri = args.first().ok_or("Missing web download URI")?;
                let parsed = url::Url::parse(uri).map_err(|error| error.to_string())?;
                if parsed.scheme() != "ns" || parsed.host_str() != Some("download") {
                    return Err("Unsupported NodeSend web download URI".into());
                }
                let mut entries = registry();
                entries.reap();
                if entries.closing {
                    return Err("The NodeSend service is shutting down".into());
                }
                let pid = spawn(&["--web-entry", uri])?;
                entries.entries.insert(
                    format!("web:{pid}"),
                    Entry {
                        pid,
                        #[cfg(not(windows))]
                        seen: Instant::now(),
                        exit: false,
                        ready: false,
                    },
                );
                Ok(json!({"pid":pid}))
            })();
            parsed
        }
        "role_probe" => {
            Ok(json!({"pid":std::process::id(),"accepting_entries":!registry().closing}))
        }
        "entry_heartbeat" => {
            let parsed = (|| {
                let kind = args.first().ok_or("Missing process entry kind")?;
                let pid = args
                    .get(1)
                    .ok_or("Missing process ID")?
                    .parse::<u32>()
                    .map_err(|_| "Invalid process ID")?;
                let exit = registry().attach(kind, pid)?;
                Ok(json!({"exit":exit,"host_pid":std::process::id()}))
            })();
            parsed
        }
        "entry_detach" => {
            if let (Some(kind), Some(pid)) = (
                args.first(),
                args.get(1).and_then(|p| p.parse::<u32>().ok()),
            ) {
                let mut entries = registry();
                if entries
                    .entries
                    .get(kind)
                    .is_some_and(|entry| entry.pid == pid)
                {
                    entries.entries.remove(kind);
                }
            }
            Ok(json!({"ok":true}))
        }
        "ensure_cli" => {
            let mut entries = registry();
            entries.reap();
            if let Some(entry) = entries.entries.get("cli") {
                if entry.exit {
                    Ok(json!({"ready":false}))
                } else {
                    Ok(json!({"ready":entry.ready}))
                }
            } else {
                match spawn(&["--cli-entry"]) {
                    Ok(pid) => {
                        entries.entries.insert(
                            "cli".into(),
                            Entry {
                                pid,
                                #[cfg(not(windows))]
                                seen: Instant::now(),
                                exit: false,
                                ready: false,
                            },
                        );
                        Ok(json!({"ready":false}))
                    }
                    Err(error) => Err(error),
                }
            }
        }
        "open_gui" => {
            if !crate::cli::has_gui_environment() {
                Err(crate::cli::cli_translate("noGuiEnvironment"))
            } else {
                let mut entries = registry();
                entries.reap();
                match spawn(&["--gui-client"]) {
                    Ok(pid) => {
                        if !entries.entries.contains_key("gui") {
                            entries.entries.insert(
                                "gui".into(),
                                Entry {
                                    pid,
                                    #[cfg(not(windows))]
                                    seen: Instant::now(),
                                    exit: false,
                                    ready: false,
                                },
                            );
                        }
                        Ok(json!({"ok":true}))
                    }
                    Err(error) => Err(error),
                }
            }
        }
        "exit_cli" | "exit_gui" => {
            let kind = if command == "exit_cli" { "cli" } else { "gui" };
            let mut entries = registry();
            entries.reap();
            let running = if let Some(entry) = entries.entries.get_mut(kind) {
                entry.exit = true;
                true
            } else {
                false
            };
            Ok(json!({"running":running}))
        }
        "entry_status" => {
            let mut entries = registry();
            entries.reap();
            Ok(
                json!({"host_pid":std::process::id(),"gui_pid":entries.entries.get("gui").map(|e|e.pid),"cli_pid":entries.entries.get("cli").map(|e|e.pid)}),
            )
        }
        _ => return None,
    };
    Some(result)
}

fn runtime(work: impl std::future::Future<Output = Result<(), String>>) -> i32 {
    match tokio::runtime::Runtime::new() {
        Ok(runtime) => match runtime.block_on(work) {
            Ok(()) => 0,
            Err(error) => {
                tracing::error!(%error, "Process host request failed");
                eprintln!("{error}");
                1
            }
        },
        Err(error) => {
            eprintln!("{error}");
            1
        }
    }
}

pub fn launch_gui() -> i32 {
    runtime(async {
        ensure_service().await?;
        // Create the WebView process from the interactive launcher so it stays
        // in the user's desktop session even when the service host was started
        // by a CLI caller in another Windows session.
        spawn(&["--gui-client"])?;
        Ok(())
    })
}

pub fn run_web_protocol(uri: String) -> i32 {
    match url::Url::parse(&uri)
        .ok()
        .and_then(|url| url.host_str().map(str::to_owned))
    {
        Some(host) if host.eq_ignore_ascii_case("download") => run_web_download(uri),
        _ => 1,
    }
}

pub fn launch_web_protocol(uri: String) -> i32 {
    runtime(async move {
        ensure_service().await?;
        crate::cli::control("spawn_web_entry", vec![uri]).await?;
        Ok(())
    })
}

pub fn run_web_download(uri: String) -> i32 {
    init_web_download_log();
    tracing::info!("Received a NodeSend web download link");
    runtime(async move {
        let parsed = url::Url::parse(&uri).map_err(|error| error.to_string())?;
        if parsed.scheme() != "ns" || parsed.host_str() != Some("download") {
            return Err("Unsupported NodeSend download link".into());
        }
        let value = |name: &str| {
            parsed
                .query_pairs()
                .find_map(|(key, value)| (key == name).then(|| value.into_owned()))
                .ok_or_else(|| format!("Missing download link parameter: {name}"))
        };
        let node_id = value("node")?;
        let host = value("host")?;
        let tcp_port = value("tcp")?
            .parse::<u16>()
            .map_err(|_| "Invalid TCP port in download link")?;
        let host = host.trim_start_matches('[').trim_end_matches(']');
        let address = match host.parse::<std::net::IpAddr>() {
            Ok(address) => address.to_string(),
            Err(_) => tokio::net::lookup_host((host, tcp_port))
                .await
                .map_err(|error| format!("Could not resolve NodeSend download host: {error}"))?
                .next()
                .map(|address| address.ip().to_string())
                .ok_or_else(|| "NodeSend download host has no IP address".to_string())?,
        };
        let quic_port = value("quic")?
            .parse::<u16>()
            .map_err(|_| "Invalid QUIC port in download link")?;
        let share_id = value("share")?;
        let path = value("path")?;
        let directory = value("directory")? == "1";
        let expected_size = parsed
            .query_pairs()
            .find_map(|(key, value)| (key == "size").then(|| value.into_owned()))
            .map(|value| {
                value
                    .parse::<u64>()
                    .map_err(|_| "Invalid file size in download link")
            })
            .transpose()?;
        let token = parsed
            .query_pairs()
            .find_map(|(key, value)| (key == "token").then(|| value.into_owned()));
        let acknowledgement_token = parsed
            .query_pairs()
            .find_map(|(key, value)| (key == "ack").then(|| value.into_owned()));
        let acknowledgement_url = parsed
            .query_pairs()
            .find_map(|(key, value)| (key == "ackUrl").then(|| value.into_owned()));
        if let (Some(ack_token), Some(ack_url)) = (
            acknowledgement_token.as_deref(),
            acknowledgement_url.as_deref(),
        ) {
            validate_ack_url(host, ack_token, ack_url)?;
        }
        ensure_service().await?;
        let pid = std::process::id().to_string();
        let kind = format!("web:{pid}");
        crate::cli::control("entry_heartbeat", vec![kind.clone(), pid.clone()]).await?;
        tracing::info!(node_id = %node_id, host = %address, tcp_port, quic_port, share_id = %share_id, path = %path, directory, "Starting web share download through NodeSend");
        let args = json!({
            "nodeId": node_id,
            "endpoint": crate::network::peers::Endpoint {
                address,
                tcp_port,
                quic_port,
                source: "web_share".into(),
                last_seen: chrono::Utc::now().timestamp(),
                error: None,
            },
            "shareId": share_id,
            "path": path,
            "token": token,
            "directory": directory,
            "expectedSize": expected_size,
        });
        let result = crate::cli::invoke_service("download_web_share", args).await;
        match result {
            Ok(value) => {
                let task_id: String = match serde_json::from_value(value) {
                    Ok(task_id) => task_id,
                    Err(error) => {
                        let _ = crate::cli::control("entry_detach", vec![kind, pid]).await;
                        return Err(format!("Invalid web download task response: {error}"));
                    }
                };
                tracing::info!(
                    task_id,
                    "Web share download was added to the NodeSend transfer list"
                );
                if let Some(ack_url) = acknowledgement_url {
                    if let Err(error) = send_ack(&ack_url).await {
                        tracing::debug!(%error, "Could not acknowledge the web download launch");
                    }
                }
                let wait_result = wait_for_web_download(&task_id, &kind, &pid).await;
                let _ = crate::cli::control("entry_detach", vec![kind, pid]).await;
                wait_result?;
                Ok(())
            }
            Err(error) => {
                let _ = crate::cli::control("entry_detach", vec![kind, pid]).await;
                tracing::error!(%error, "Web share download could not be started in NodeSend");
                Err(error)
            }
        }
    })
}

async fn wait_for_web_download(task_id: &str, kind: &str, pid: &str) -> Result<(), String> {
    loop {
        crate::cli::control("entry_heartbeat", vec![kind.to_string(), pid.to_string()]).await?;
        let result =
            crate::cli::invoke_service("web_download_task_status", json!({"taskId": task_id}))
                .await?;
        let status = result
            .get("status")
            .and_then(Value::as_str)
            .ok_or("NodeSend returned no web download task status")?;
        if matches!(
            status,
            "completed"
                | "failed"
                | "cancelled"
                | "rejected"
                | "partial_completed"
                | "incompatible"
                | "paused"
                | "offline"
        ) {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

fn validate_ack_url(host: &str, token: &str, ack_url: &str) -> Result<(), String> {
    if token.len() != 32 || !token.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("Invalid download acknowledgement token".into());
    }
    let callback = url::Url::parse(ack_url).map_err(|_| "Invalid download acknowledgement URL")?;
    let callback_host = callback
        .host_str()
        .unwrap_or_default()
        .trim_matches(['[', ']']);
    let expected_host = host.trim_matches(['[', ']']);
    if callback.scheme() != "http"
        || !callback_host.eq_ignore_ascii_case(expected_host)
        || callback.path() != format!("/api/web-downloads/ack/{token}")
        || callback.query().is_some()
    {
        return Err("Invalid download acknowledgement URL".into());
    }
    Ok(())
}

async fn send_ack(ack_url: &str) -> Result<(), String> {
    let client = reqwest::Client::new();
    let mut last_error = String::new();
    for attempt in 0..3 {
        match client.post(ack_url).send().await {
            Ok(response) if response.status().is_success() => return Ok(()),
            Ok(response) => last_error = format!("HTTP {}", response.status()),
            Err(error) => last_error = error.to_string(),
        }
        if attempt < 2 {
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    }
    Err(last_error)
}

pub fn run_cli_entry() -> i32 {
    #[cfg(windows)]
    if !crate::single_instance::acquire_cli() {
        return 0;
    }
    #[cfg(unix)]
    if !crate::cli::local_unix::acquire_cli().unwrap_or(false) {
        return 0;
    }
    runtime(async {
        loop {
            let value = crate::cli::control(
                "entry_heartbeat",
                vec!["cli".into(), std::process::id().to_string()],
            )
            .await?;
            if value.get("exit").and_then(Value::as_bool) == Some(true) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
        crate::cli::control(
            "entry_detach",
            vec!["cli".into(), std::process::id().to_string()],
        )
        .await?;
        Ok(())
    })
}

pub fn start_gui_entry(app: tauri::AppHandle) {
    tauri::async_runtime::spawn(async move {
        let mut service_unavailable = false;
        loop {
            match crate::cli::control(
                "entry_heartbeat",
                vec!["gui".into(), std::process::id().to_string()],
            )
            .await
            {
                Ok(value) if value.get("exit").and_then(Value::as_bool) == Some(true) => {
                    let handle = app.clone();
                    let _ = app.run_on_main_thread(move || crate::tray::quit_gui(&handle));
                    break;
                }
                Ok(_) => service_unavailable = false,
                Err(error) => {
                    if !service_unavailable {
                        tracing::warn!(%error, "GUI is waiting for the NodeSend service connection");
                        service_unavailable = true;
                    }
                    if let Err(start_error) = crate::process_host::ensure_service().await {
                        tracing::debug!(%start_error, "Could not restart the NodeSend service while the GUI is waiting");
                    }
                }
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    });
}

pub fn run_service() -> i32 {
    #[cfg(windows)]
    if !crate::single_instance::acquire_without_activation() {
        return 0;
    }
    #[cfg(unix)]
    if !crate::cli::local_unix::acquire_owner().unwrap_or(false) {
        return 0;
    }
    init_service_log();
    runtime(async {
        let backend = crate::cli::build_backend().await?;
        crate::cli::start_server(backend.network.clone(), backend.clone())
            .map_err(|e| e.to_string())?;
        let started = Instant::now();
        loop {
            tokio::time::sleep(Duration::from_millis(250)).await;
            let (empty, attached) = {
                let mut entries = registry();
                entries.reap();
                (entries.entries.is_empty(), entries.ever_attached)
            };
            if !empty {
                continue;
            }
            if !attached && started.elapsed() < Duration::from_secs(15) {
                continue;
            }
            backend.network.notify_live_tasks_offline().await;
            if let Err(error) = backend.network.transfers.drain_completed_cache().await {
                tracing::error!(%error,"Failed to drain completed transfer cache files");
            }
            let empty = {
                let mut entries = registry();
                entries.reap();
                let empty = entries.entries.is_empty();
                if empty {
                    entries.closing = true;
                }
                empty
            };
            if !empty {
                continue;
            }
            backend.network.mark_live_tasks_offline();
            backend.http.stop().await;
            for status in backend.protocol.statuses().await {
                backend.protocol.stop(&status.id).await;
            }
            backend.network.shutdown().await;
            return Ok(());
        }
    })
}

fn init_service_log() {
    let Some(data) = dirs::data_dir() else {
        return;
    };
    let logs = data.join("NodeSend").join("logs");
    if std::fs::create_dir_all(&logs).is_err() {
        return;
    }
    if let Ok(file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(logs.join("service.log"))
    {
        let _ = tracing_subscriber::fmt()
            .with_ansi(false)
            .with_timer(tracing_subscriber::fmt::time::ChronoLocal::new(
                "%Y-%m-%d %H:%M:%S%.3f".into(),
            ))
            .with_writer(Mutex::new(file))
            .try_init();
    }
}

fn init_process_log(file_name: &str) {
    let Some(data) = dirs::data_dir() else {
        return;
    };
    let logs = data.join("NodeSend").join("logs");
    if std::fs::create_dir_all(&logs).is_err() {
        return;
    }
    if let Ok(file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(logs.join(file_name))
    {
        let _ = tracing_subscriber::fmt()
            .with_ansi(false)
            .with_timer(tracing_subscriber::fmt::time::ChronoLocal::new(
                "%Y-%m-%d %H:%M:%S%.3f".into(),
            ))
            .with_writer(Mutex::new(file))
            .try_init();
    }
}

pub fn init_gui_log() {
    init_process_log("gui.log");
}

fn init_web_download_log() {
    init_process_log("web-download.log");
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn registry_tracks_persistent_entries_and_preserves_exit_requests() {
        let mut entries = Entries::default();
        let pid = std::process::id();
        assert!(!entries.attach("cli", pid).unwrap());
        assert!(!entries.attach("gui", pid).unwrap());
        assert_eq!(entries.entries.len(), 2);
        entries.entries.get_mut("cli").unwrap().exit = true;
        assert!(entries.attach("cli", pid).unwrap());
        assert!(!entries.attach("gui", pid).unwrap());
        assert!(entries.attach("session", pid).is_err());
        assert!(entries.attach("cli", pid + 1).is_err());
    }

    #[test]
    fn live_entries_remain_registered_and_shutdown_rejects_registration() {
        let mut entries = Entries::default();
        let pid = std::process::id();
        entries.attach("cli", pid).unwrap();
        entries.reap();
        assert!(entries.entries.contains_key("cli"));
        entries.closing = true;
        assert!(entries.attach("gui", pid).is_err());
    }
}
