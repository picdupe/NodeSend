use crate::{
    config::AppPaths, network::NetworkManager, service::AppService, transfer::model::ConflictPolicy,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    env,
    io::{self, IsTerminal, Write},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
#[cfg(windows)]
use tokio::net::windows::named_pipe::{ClientOptions, ServerOptions};

const MAX_FRAME: usize = 16 * 1024 * 1024;

fn cli_language() -> &'static str {
    static LANGUAGE: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    LANGUAGE
        .get_or_init(crate::locale::preferred_language)
        .as_str()
}

fn cli_bundle(language: &str) -> &'static Value {
    static ZH_CN: std::sync::OnceLock<Value> = std::sync::OnceLock::new();
    static ZH_TW: std::sync::OnceLock<Value> = std::sync::OnceLock::new();
    static EN_US: std::sync::OnceLock<Value> = std::sync::OnceLock::new();
    static JA_JP: std::sync::OnceLock<Value> = std::sync::OnceLock::new();
    let (cell, source) = match language {
        "zh-TW" => (
            &ZH_TW,
            include_str!("../../src/i18n/locales/zh-TW/common.json"),
        ),
        "en-US" => (
            &EN_US,
            include_str!("../../src/i18n/locales/en-US/common.json"),
        ),
        "ja-JP" => (
            &JA_JP,
            include_str!("../../src/i18n/locales/ja-JP/common.json"),
        ),
        _ => (
            &ZH_CN,
            include_str!("../../src/i18n/locales/zh-CN/common.json"),
        ),
    };
    cell.get_or_init(|| serde_json::from_str(source).unwrap_or(Value::Null))
}

fn cli_text(key: &str) -> String {
    cli_bundle(cli_language())
        .get("cli")
        .and_then(|bundle| bundle.get(key))
        .and_then(Value::as_str)
        .or_else(|| {
            cli_bundle("zh-CN")
                .get("cli")
                .and_then(|bundle| bundle.get(key))
                .and_then(Value::as_str)
        })
        .unwrap_or(key)
        .to_owned()
}

pub fn cli_translate(key: &str) -> String {
    cli_text(key)
}

fn cli_text_args(key: &str, args: &[(&str, &str)]) -> String {
    let mut value = cli_text(key);
    for (name, argument) in args {
        value = value.replace(&format!("{{{{{name}}}}}"), argument);
    }
    value
}
pub(crate) async fn build_backend() -> Result<Arc<crate::service_rpc::Backend>, String> {
    let data = dirs::data_dir()
        .ok_or_else(|| cli_text("dataDirectoryUnavailable"))?
        .join("NodeSend");
    let app = AppService::init(AppPaths::new(data)).map_err(|e| e.to_string())?;
    let network = NetworkManager::start(app.clone())
        .await
        .map_err(|e| e.to_string())?;
    let protocol = Arc::new(crate::share::servers::ProtocolManager::default());
    protocol
        .restore(app.clone())
        .await
        .map_err(|e| e.to_string())?;
    let browser = Arc::new(
        crate::browser::BrowserManager::new(app.clone())
            .map_err(|e| e.to_string())?
            .with_peers(network.peers.clone()),
    );
    Ok(Arc::new(crate::service_rpc::Backend {
        service: app,
        network,
        protocol,
        browser,
        http: Arc::new(crate::httpapi::HttpManager::new()),
        links: Arc::new(crate::httpapi::ShareLinkManager::new()),
    }))
}

pub(crate) fn process_alive(pid: u32) -> bool {
    #[cfg(windows)]
    unsafe {
        use windows::Win32::System::Threading::{
            GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
        };
        let Ok(handle) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else {
            return false;
        };
        let mut code = 0;
        let alive = GetExitCodeProcess(handle, &mut code).is_ok() && code == 259;
        let _ = windows::Win32::Foundation::CloseHandle(handle);
        alive
    }
    #[cfg(not(windows))]
    {
        let _ = pid;
        true
    }
}

pub fn is_gui_client() -> bool {
    env::args().nth(1).as_deref() == Some("--gui-client")
}

pub fn open_gui() -> Result<(), String> {
    tokio::runtime::Runtime::new()
        .map_err(|e| e.to_string())?
        .block_on(async {
            remote(Request {
                command: "open_gui".into(),
                args: vec![],
            })
            .await
            .map(|_| ())
        })
}

pub async fn pending_count() -> usize {
    remote(Request {
        command: "pending_count".into(),
        args: vec![],
    })
    .await
    .ok()
    .and_then(|v| v.as_u64())
    .unwrap_or(0) as usize
}

pub async fn service_invoke(
    _app: tauri::AppHandle,
    command: String,
    args: Value,
) -> Result<Value, String> {
    if !crate::service_rpc::is_service_command(&command) {
        return Ok(json!({"handled":false}));
    }
    let value = remote(Request {
        command: "invoke".into(),
        args: vec![command, args.to_string()],
    })
    .await?;
    Ok(json!({"handled":true,"value":value}))
}
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Request {
    command: String,
    args: Vec<String>,
}
#[derive(Serialize, Deserialize)]
struct Reply {
    result: Value,
    error: Option<String>,
}

struct Commands {
    network: Arc<NetworkManager>,
    backend: Arc<crate::service_rpc::Backend>,
    receive: Mutex<Option<String>>,
}
#[cfg(windows)]
fn pipe_name() -> io::Result<String> {
    use sha2::{Digest, Sha256};
    let path = dirs::data_dir()
        .ok_or_else(|| io::Error::other("Application data directory is unavailable"))?
        .join("NodeSend");
    let digest = Sha256::digest(path.to_string_lossy().to_lowercase().as_bytes());
    Ok(format!(r"\\.\pipe\NodeSend.Control.{:x}", digest))
}
pub fn is_cli_invocation() -> bool {
    env::args()
        .nth(1)
        .is_some_and(|arg| !arg.starts_with("ns://"))
}

pub fn has_gui_environment() -> bool {
    #[cfg(target_os = "linux")]
    {
        return ["DISPLAY", "WAYLAND_DISPLAY"]
            .iter()
            .any(|key| env::var_os(key).is_some_and(|value| !value.is_empty()));
    }
    #[cfg(not(target_os = "linux"))]
    {
        true
    }
}

pub fn start_server(
    network: Arc<NetworkManager>,
    backend: Arc<crate::service_rpc::Backend>,
) -> io::Result<()> {
    #[cfg(windows)]
    let name = pipe_name()?;
    #[cfg(windows)]
    let first = ServerOptions::new()
        .first_pipe_instance(true)
        .reject_remote_clients(true)
        .create(&name)?;
    #[cfg(unix)]
    let listener = local_unix::listen()?;
    let commands = Arc::new(Commands {
        network,
        backend,
        receive: Mutex::new(None),
    });
    let worker = commands.clone();
    tauri::async_runtime::spawn(async move {
        loop {
            let target = worker.receive.lock().ok().and_then(|value| value.clone());
            if let Some(target) = target {
                for task in worker
                    .network
                    .transfers
                    .snapshots()
                    .into_iter()
                    .filter(|t| t.direction == "receive" && t.status == "waiting")
                {
                    if let Err(error) = worker
                        .network
                        .decide_and_trust(
                            task.id,
                            true,
                            Some(target.clone()),
                            ConflictPolicy::Rename,
                            None,
                            false,
                        )
                        .await
                    {
                        tracing::warn!(%error, "CLI auto-receive action failed");
                    }
                }
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    });
    #[cfg(unix)]
    {
        let commands = commands.clone();
        tauri::async_runtime::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                let commands = commands.clone();
                tauri::async_runtime::spawn(async move {
                    let work = async {
                        let request: Request = read_frame(&mut stream).await?;
                        let reply = match commands.execute(request).await {
                            Ok(result) => Reply {
                                result,
                                error: None,
                            },
                            Err(error) => Reply {
                                result: Value::Null,
                                error: Some(error),
                            },
                        };
                        write_frame(&mut stream, &reply).await
                    };
                    let _ = tokio::time::timeout(Duration::from_secs(3600), work).await;
                });
            }
        });
    }
    #[cfg(windows)]
    tauri::async_runtime::spawn(async move {
        let mut server = first;
        loop {
            if let Err(error) = server.connect().await {
                tracing::error!(%error, "CLI IPC server connection failed");
                break;
            }
            let next = match ServerOptions::new()
                .reject_remote_clients(true)
                .create(&name)
            {
                Ok(next) => next,
                Err(error) => {
                    tracing::error!(%error, "Failed to create the next CLI IPC pipe instance");
                    break;
                }
            };
            let commands = commands.clone();
            tauri::async_runtime::spawn(async move {
                let result = async {
                    let request: Request = read_frame(&mut server).await?;
                    let reply = match commands.execute(request).await {
                        Ok(value) => Reply {
                            result: value,
                            error: None,
                        },
                        Err(error) => Reply {
                            result: Value::Null,
                            error: Some(error),
                        },
                    };
                    write_frame(&mut server, &reply).await
                };
                if let Err(error) = tokio::time::timeout(Duration::from_secs(3600), result)
                    .await
                    .unwrap_or_else(|_| {
                        Err(io::Error::new(
                            io::ErrorKind::TimedOut,
                            "CLI IPC request timed out",
                        ))
                    })
                {
                    tracing::debug!(%error, "CLI IPC request failed");
                }
            });
            server = next;
        }
    });
    Ok(())
}
impl Commands {
    async fn execute(&self, request: Request) -> Result<Value, String> {
        let arg = |index: usize| {
            request
                .args
                .get(index)
                .cloned()
                .ok_or_else(|| "A required CLI argument is missing".to_string())
        };
        if let Some(result) = crate::process_host::control(&request.command, &request.args).await {
            return result;
        }
        match request.command.as_str() {
            "role_probe" => Ok(json!({"pid":std::process::id()})),
            "invoke" => {
                let name = arg(0)?;
                let args = serde_json::from_str(&arg(1)?).map_err(|e| e.to_string())?;
                self.backend.invoke(&name, args).await
            }
            "pending_count" => Ok(json!(self.network.pending_manual_receive_count())),
            "status" => {
                let status = self.network.status();
                Ok(json!({
                    "pid": std::process::id(),
                    "node_id": status.node_id,
                    "name": status.display_name,
                    "tcp_port": self.network.tcp_port,
                    "quic_port": self.network.quic_port,
                }))
            }
            "send" => {
                let address = arg(0)?;
                let path = arg(1)?;
                let verify = !request.args.iter().skip(2).any(|v| v == "--no-hash");
                let peer = self
                    .network
                    .add_endpoint(address, None, false)
                    .await
                    .map_err(|e| e.to_string())?;
                let id = self
                    .network
                    .send(peer.node_id, vec![path], verify, false, None)
                    .await
                    .map_err(|e| e.to_string())?;
                Ok(json!({"task_id": id}))
            }
            "receive" => {
                let target = arg(0)?;
                let receive_target = if target == "stop" {
                    None
                } else {
                    if !std::path::Path::new(&target).is_absolute() {
                        return Err("The receive directory must be an absolute path".into());
                    }
                    std::fs::create_dir_all(&target).map_err(|e| e.to_string())?;
                    Some(target.clone())
                };
                *self
                    .receive
                    .lock()
                    .map_err(|_| "CLI receive state lock is poisoned".to_string())? =
                    receive_target;
                Ok(json!({"receive": target}))
            }
            "accept" => {
                let id = arg(0)?;
                let task = self
                    .network
                    .transfers
                    .snapshots()
                    .into_iter()
                    .find(|task| task.id == id)
                    .ok_or("ACCEPT_TASK_NOT_WAITING")?;
                if task.direction != "receive" || task.status != "waiting" {
                    return Err("ACCEPT_TASK_NOT_WAITING".into());
                }
                let destination = self.network.status().receive_directory;
                crate::ipc::network::decide_incoming_transfer_service(
                    self.network.clone(),
                    id,
                    true,
                    Some(destination.clone()),
                    ConflictPolicy::Rename,
                    None,
                    Some(false),
                )
                .await
                .map_err(|error| error.to_string())?;
                Ok(json!({"accepted": true, "destination": destination}))
            }
            "tasks" => {
                let tasks = self.network.transfers.snapshots();
                serde_json::to_value(tasks).map_err(|e| e.to_string())
            }
            "task" => {
                let id = arg(0)?;
                let task = self
                    .network
                    .transfers
                    .snapshots()
                    .into_iter()
                    .find(|t| t.id == id)
                    .ok_or_else(|| cli_text("invalidTasks"))?;
                serde_json::to_value(task).map_err(|e| e.to_string())
            }
            "action" => {
                self.network
                    .action(arg(0)?, arg(1)?)
                    .await
                    .map_err(|e| e.to_string())?;
                Ok(json!({"ok": true}))
            }
            _ => Err(format!("Unknown CLI command: {}", request.command)),
        }
    }
}
async fn read_frame<T: serde::de::DeserializeOwned>(
    stream: &mut (impl AsyncRead + Unpin),
) -> io::Result<T> {
    let size = stream.read_u32().await? as usize;
    if size > MAX_FRAME {
        return Err(io::Error::other("CLI IPC frame exceeds the maximum size"));
    }
    let mut body = vec![0; size];
    stream.read_exact(&mut body).await?;
    serde_json::from_slice(&body).map_err(io::Error::other)
}
async fn write_frame<T: Serialize>(
    stream: &mut (impl AsyncWrite + Unpin),
    value: &T,
) -> io::Result<()> {
    let body = serde_json::to_vec(value).map_err(io::Error::other)?;
    if body.len() > MAX_FRAME {
        return Err(io::Error::other("CLI IPC frame exceeds the maximum size"));
    }
    stream.write_u32(body.len() as u32).await?;
    stream.write_all(&body).await?;
    stream.flush().await
}
pub(crate) async fn control(command: &str, args: Vec<String>) -> Result<Value, String> {
    remote_once(Request {
        command: command.into(),
        args,
    })
    .await
}

async fn remote(request: Request) -> Result<Value, String> {
    let read_only = matches!(
        request.command.as_str(),
        "task" | "tasks" | "status" | "pending_count"
    ) || request.command == "invoke"
        && request
            .args
            .first()
            .is_some_and(|name| name.starts_with("get_") || name.starts_with("list_"));
    for attempt in 0..3 {
        match remote_once(request.clone()).await {
            Err(error)
                if read_only
                    && attempt < 2
                    && (error.contains("IPC")
                        || error.contains("timed out")
                        || error.contains("pipe")
                        || error.contains("pipe")) =>
            {
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
            result => return result,
        }
    }
    unreachable!()
}

async fn remote_once(request: Request) -> Result<Value, String> {
    #[cfg(windows)]
    let name = pipe_name().map_err(|e| e.to_string())?;
    let deadline = tokio::time::Instant::now()
        + if ["role_probe", "gui_probe"].contains(&request.command.as_str()) {
            Duration::from_millis(500)
        } else {
            Duration::from_secs(10)
        };
    let mut pipe = loop {
        #[cfg(windows)]
        let attempt = ClientOptions::new().open(&name);
        #[cfg(unix)]
        let attempt =
            tokio::net::UnixStream::connect(local_unix::socket_path().map_err(|e| e.to_string())?)
                .await;
        match attempt {
            Ok(pipe) => break pipe,
            Err(error)
                if tokio::time::Instant::now() < deadline
                    && (error.kind() == io::ErrorKind::NotFound
                        || error.kind() == io::ErrorKind::ConnectionRefused
                        || error.raw_os_error() == Some(231)) =>
            {
                tokio::time::sleep(Duration::from_millis(100)).await
            }
            Err(error) => {
                return Err(format!(
                    "Could not connect to the NodeSend service over IPC. Start the service with `nodesend daemon` and retry: {}",
                    crate::error::io_error_text(&error)
                ))
            }
        }
    };
    write_frame(&mut pipe, &request)
        .await
        .map_err(|e| crate::error::io_error_text(&e))?;
    let timeout = if request.command == "invoke" {
        3600
    } else if ["role_probe", "gui_probe"].contains(&request.command.as_str()) {
        1
    } else {
        60
    };
    let reply: Reply = tokio::time::timeout(Duration::from_secs(timeout), read_frame(&mut pipe))
        .await
        .map_err(|_| "Timed out waiting for the NodeSend service response".to_string())?
        .map_err(|e| crate::error::io_error_text(&e))?;
    match reply.error {
        Some(error) => Err(error),
        None => Ok(reply.result),
    }
}
pub fn run(owner: bool) -> i32 {
    #[cfg(windows)]
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn AttachConsole(process: u32) -> i32;
    }
    #[cfg(windows)]
    unsafe {
        AttachConsole(u32::MAX);
    }
    let runtime = match tokio::runtime::Runtime::new() {
        Ok(value) => value,
        Err(error) => {
            eprintln!("{error}");
            return 1;
        }
    };
    match runtime.block_on(run_command(owner)) {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("{error}");
            1
        }
    }
}
async fn run_command(_owner: bool) -> Result<(), String> {
    let mut args = env::args().skip(1);
    let command = args.next().unwrap_or_else(|| "help".into());
    let mut args: Vec<String> = args.collect();
    let detached_send = command == "send" && args.iter().any(|v| v == "--detach");
    if ![
        "daemon", "status", "receive", "accept", "send", "tasks", "task", "watch", "action",
        "exit", "help", "-h", "--help",
    ]
    .contains(&command.as_str())
    {
        help();
        return Err(cli_text_args("unknownCommand", &[("command", &command)]));
    }
    if matches!(command.as_str(), "help" | "-h" | "--help") {
        if !args.is_empty() {
            return Err(cli_text("unexpectedArguments"));
        }
        help();
        return Ok(());
    }
    let usage_error = match command.as_str() {
        "status" | "daemon" | "exit" if !args.is_empty() => Some(cli_text("unexpectedArguments")),
        "receive" if args.len() != 1 => Some(cli_text("usageReceive")),
        "accept" if args.len() != 1 => Some(cli_text("usageAccept")),
        "tasks"
            if args.len() > 1
                || args.first().is_some_and(|category| {
                    !["all", "active", "send", "receive"].contains(&category.as_str())
                }) =>
        {
            Some(cli_text("usageTasks"))
        }
        "task" if args.len() != 1 => Some(cli_text("usageTask")),
        "watch" if args.len() != 1 => Some(cli_text("usageWatch")),
        "action" if args.len() != 2 => Some(cli_text("usageAction")),
        "action"
            if !["pause", "resume", "retry", "cancel", "finish"].contains(&args[1].as_str()) =>
        {
            Some(cli_text("usageAction"))
        }
        "send"
            if args.len() < 2
                || args[2..]
                    .iter()
                    .any(|arg| !["--no-hash", "--detach"].contains(&arg.as_str())) =>
        {
            Some(cli_text("usageSend"))
        }
        _ => None,
    };
    if let Some(error) = usage_error {
        return Err(error);
    }
    if command == "send" {
        args.retain(|value| value != "--detach");
    }
    let path_index = match command.as_str() {
        "send" => Some(1),
        "receive" if args[0] != "stop" => Some(0),
        _ => None,
    };
    if let Some(index) = path_index {
        let path = std::path::absolute(&args[index]).map_err(|e| e.to_string())?;
        args[index] = path.to_string_lossy().into_owned();
    }
    if command == "exit" {
        if control("role_probe", vec![]).await.is_err() {
            println!("{}", cli_text("cliNotRunning"));
            return Ok(());
        }
        let result = control("exit_cli", vec![]).await?;
        if result.get("running").and_then(Value::as_bool) == Some(true) {
            println!("{}", cli_text("exitRequested"));
        } else {
            println!("{}", cli_text("cliNotRunning"));
        }
        return Ok(());
    }
    crate::process_host::ensure_cli().await?;
    if command == "send" {
        println!("{}", cli_text("startingSend"));
    }
    let mut request_args = args.clone();
    let display_task_name = if ["task", "watch", "action", "accept"].contains(&command.as_str()) {
        let snapshots = remote(Request {
            command: "tasks".into(),
            args: Vec::new(),
        })
        .await?;
        let active_only = command != "task";
        let index_category = if command == "task" { "all" } else { "active" };
        let selector = request_args.first().cloned().unwrap_or_default();
        let selected = resolve_task_selector(&snapshots, &selector, index_category, active_only)?;
        request_args[0] = task_str(selected, "id");
        Some(content_title(selected))
    } else {
        None
    };
    let response = remote(Request {
        command: if command == "daemon" {
            "status".into()
        } else if command == "watch" {
            "task".into()
        } else {
            command.clone()
        },
        args: if command == "tasks" {
            Vec::new()
        } else {
            request_args.clone()
        },
    })
    .await
    .map_err(|error| {
        if command == "accept" && error == "ACCEPT_TASK_NOT_WAITING" {
            cli_text("acceptOnlyWaiting")
        } else {
            error
        }
    })?;
    match command.as_str() {
        "status" | "daemon" => print_status(&response),
        "receive" => {
            let target = args.first().cloned().unwrap_or_default();
            if target == "stop" {
                println!("{}", cli_text("receiveStopped"));
            } else {
                println!("{}", field(&cli_text("receiveFolder"), &target));
            }
        }
        "accept" => {
            let name = display_task_name.as_deref().unwrap_or("Unknown task");
            println!("{}", cli_text_args("acceptedTask", &[("name", name)]));
            if let Some(destination) = response.get("destination").and_then(Value::as_str) {
                println!("{}", field(&cli_text("destinationLabel"), destination));
            }
        }
        "tasks" => print_tasks(&response, args.first().map(String::as_str).unwrap_or("all"))?,
        "task" => print_task(&response),
        "watch" => watch_send(&request_args[0]).await?,
        "action" => print_action(
            display_task_name.as_deref().unwrap_or("Unknown task"),
            args.get(1).map(String::as_str).unwrap_or_default(),
        ),
        "send" => {
            let id = response
                .get("task_id")
                .and_then(Value::as_str)
                .ok_or("The service did not return a task ID")?
                .to_string();
            if detached_send {
                let name = std::path::Path::new(&args[1])
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or(&args[1]);
                println!("{}", cli_text_args("sendCreatedNamed", &[("name", name)]));
                println!("{}", cli_text("tasksActiveHint"));
            } else {
                println!("{}", cli_text("sendCreated"));
                watch_send(&id).await?;
            }
        }
        _ => help(),
    }
    Ok(())
}

fn help() {
    println!("{}", cli_text("helpTitle"));
    println!();
    println!("{}", cli_text("usage"));
    for key in [
        "daemon",
        "status",
        "exit",
        "acceptHelp",
        "sendHelp",
        "sendHelpDetail",
        "receiveHelp",
        "tasksHelp",
        "taskHelp",
        "watchHelp",
        "actionHelp",
        "detachHelp",
    ] {
        println!("{}", cli_text(key));
    }
}

fn print_tasks(tasks: &Value, category: &str) -> Result<(), String> {
    let Some(all) = tasks.as_array() else {
        return Err(cli_text("invalidTasks"));
    };
    let visible = tasks_in_category(all, category)?;
    if visible.is_empty() {
        println!("{}", cli_text("noTasks"));
        return Ok(());
    }
    let heading = match category {
        "all" => "allTasks",
        "active" => "activeTasks",
        "send" => "sendTasks",
        "receive" => "receiveTasks",
        _ => unreachable!(),
    };
    println!("{}{}", cli_text(heading), cli_text("headingColon"));
    for (index, task) in visible.iter().enumerate() {
        let direction = if task_str(task, "direction") == "send" {
            cli_text("directionSend")
        } else {
            cli_text("directionReceive")
        };
        println!(
            "  [{}] {}  {}  {}  {}",
            index + 1,
            content_title(task),
            direction,
            status_text(&task_str(task, "status")),
            progress_summary(task)
        );
    }
    println!("{}", cli_text("tasksActiveHint"));
    Ok(())
}

fn is_active_task(task: &Value) -> bool {
    !matches!(
        task_str(task, "status").as_str(),
        "completed"
            | "partial_completed"
            | "finishing"
            | "rejected"
            | "cancelling"
            | "cancelled"
            | "incompatible"
    )
}

fn tasks_in_category<'a>(tasks: &'a [Value], category: &str) -> Result<Vec<&'a Value>, String> {
    if !["all", "active", "send", "receive"].contains(&category) {
        return Err(cli_text("usageTasks"));
    }
    Ok(tasks
        .iter()
        .filter(|task| match category {
            "active" => is_active_task(task),
            "send" => task_str(task, "direction") == "send",
            "receive" => task_str(task, "direction") == "receive",
            _ => true,
        })
        .collect())
}

fn resolve_task_selector<'a>(
    tasks: &'a Value,
    selector: &str,
    index_category: &str,
    active_only: bool,
) -> Result<&'a Value, String> {
    let all = tasks.as_array().ok_or_else(|| cli_text("invalidTasks"))?;
    let category = if active_only {
        "active"
    } else {
        index_category
    };
    let candidates = tasks_in_category(all, category)?;
    if let Ok(index) = selector.parse::<usize>() {
        return index
            .checked_sub(1)
            .and_then(|index| candidates.get(index).copied())
            .ok_or_else(|| cli_text_args("invalidIndex", &[("index", selector)]));
    }
    let matches = candidates
        .iter()
        .filter(|task| content_title(task) == selector)
        .copied()
        .collect::<Vec<_>>();
    match matches.as_slice() {
        [task] => Ok(*task),
        [] if active_only => Err(cli_text_args("activeTaskNotFound", &[("task", selector)])),
        [] => Err(cli_text_args("taskNotFound", &[("task", selector)])),
        _ => Err(cli_text_args(
            "duplicateTask",
            &[("task", selector), ("category", category)],
        )),
    }
}

/// 发送任务的状态轮询与终端进度显示。
async fn watch_send(id: &str) -> Result<(), String> {
    let mut console = Console::new();
    let mut keys = KeyInput::new();
    let mut speed = SpeedTracker::new();
    let started = Instant::now();
    let mut transfer_start: Option<Instant> = None;
    let mut last_status = String::new();
    let mut header = false;
    loop {
        let task = remote(Request {
            command: "task".into(),
            args: vec![id.to_string()],
        })
        .await?;
        let status = task_str(&task, "status");
        let done = task_u64(&task, "completed_bytes");
        let total = task_u64(&task, "total_bytes");
        speed.sample(done);
        if !header {
            console.line(&cli_text_args(
                "sendTitle",
                &[
                    ("name", &content_title(&task)),
                    ("peer", &peer_label(&task)),
                ],
            ));
            if keys.enabled {
                console.line(&cli_text("watchKeys"));
            }
            header = true;
        }
        if status != last_status {
            if let Some(message) = transition_message(&task) {
                console.line(&message);
            }
            last_status = status.clone();
        }
        match status.as_str() {
            "completed" => {
                console.finish();
                // 平均速度按实际传输时长计算，不把等待确认的时间算进去。
                let elapsed = transfer_start.unwrap_or(started).elapsed().as_secs_f64();
                let size = if total > 0 { total } else { done };
                let average = if elapsed > 0.0 {
                    size as f64 / elapsed
                } else {
                    0.0
                };
                println!(
                    "{}",
                    cli_text_args("transferComplete", &[("name", &content_title(&task))])
                );
                println!("{}", field(&cli_text("size"), format_bytes(size)));
                println!("{}", field(&cli_text("elapsed"), format_duration(elapsed)));
                println!(
                    "{}",
                    field(&cli_text("averageSpeed"), format_speed(average))
                );
                return Ok(());
            }
            "cancelled" => {
                console.finish();
                println!(
                    "{}",
                    cli_text_args("taskCancelled", &[("name", &content_title(&task))])
                );
                return Ok(());
            }
            "rejected" => {
                console.finish();
                println!(
                    "{}",
                    cli_text_args("peerDeclined", &[("name", &content_title(&task))])
                );
                return Ok(());
            }
            "partial_completed" => {
                console.finish();
                return Err(task_error(&task).unwrap_or_else(|| cli_text("partialFailed")));
            }
            "incompatible" => {
                console.finish();
                return Err(task_error(&task).unwrap_or_else(|| cli_text("incompatibleFailed")));
            }
            "failed" => {
                console.finish();
                return Err(task_error(&task).unwrap_or_else(|| cli_text("transferFailed")));
            }
            "transferring" => {
                if transfer_start.is_none() {
                    transfer_start = Some(Instant::now());
                }
                console.progress(progress_key(&task), &progress_line(&task, speed.value()));
            }
            _ => {}
        }
        if let Some(key) = keys.poll().map_err(|error| error.to_string())? {
            let action = match key {
                Key::Pause => Some(("pause", "pauseRequest")),
                Key::Resume => Some(("resume", "resumeRequest")),
                Key::Cancel => Some(("cancel", "cancelRequest")),
                Key::Detach => {
                    console.finish();
                    println!("{}", cli_text("watchStopped"));
                    return Ok(());
                }
            };
            if let Some((action, message)) = action {
                console.line(&cli_text(message));
                match remote(Request {
                    command: "action".into(),
                    args: vec![id.to_string(), action.to_string()],
                })
                .await
                {
                    Ok(_) => console.line(&cli_text(match action {
                        "pause" => "pauseRequested",
                        "resume" => "resumeRequested",
                        _ => "cancelRequested",
                    })),
                    Err(error) => {
                        console.line(&cli_text_args("actionFailed", &[("error", &error)]))
                    }
                }
            }
        }
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_secs(1)) => {},
            signal = tokio::signal::ctrl_c() => {
                console.finish();
                signal.map_err(|e| e.to_string())?;
                println!("{}", cli_text("watchStoppedAgain"));
                return Ok(());
            }
        }
    }
}

#[derive(Clone, Copy)]
enum Key {
    Pause,
    Resume,
    Cancel,
    Detach,
}

struct KeyInput {
    enabled: bool,
}

impl KeyInput {
    fn new() -> Self {
        let enabled = io::stdin().is_terminal()
            && io::stdout().is_terminal()
            && crossterm::terminal::enable_raw_mode().is_ok();
        Self { enabled }
    }

    fn poll(&mut self) -> io::Result<Option<Key>> {
        if !self.enabled || !crossterm::event::poll(Duration::ZERO)? {
            return Ok(None);
        }
        let crossterm::event::Event::Key(event) = crossterm::event::read()? else {
            return Ok(None);
        };
        if event.kind != crossterm::event::KeyEventKind::Press {
            return Ok(None);
        }
        let key = match event.code {
            crossterm::event::KeyCode::Char('p' | 'P') => Some(Key::Pause),
            crossterm::event::KeyCode::Char('r' | 'R') => Some(Key::Resume),
            crossterm::event::KeyCode::Char('x' | 'X') => Some(Key::Cancel),
            crossterm::event::KeyCode::Char('q' | 'Q') => Some(Key::Detach),
            crossterm::event::KeyCode::Char('c' | 'C')
                if event
                    .modifiers
                    .contains(crossterm::event::KeyModifiers::CONTROL) =>
            {
                Some(Key::Detach)
            }
            _ => None,
        };
        Ok(key)
    }
}

impl Drop for KeyInput {
    fn drop(&mut self) {
        if self.enabled {
            let _ = crossterm::terminal::disable_raw_mode();
        }
    }
}

/// 状态切换时只提示一次，避免每秒重复同一句话。
fn transition_message(task: &Value) -> Option<String> {
    let status = task_str(task, "status");
    let done = task_u64(task, "completed_bytes");
    let total = task_u64(task, "total_bytes");
    let progress = if total > 0 && done > 0 {
        cli_text_args("progress", &[("percent", &percent(done, total))])
    } else {
        String::new()
    };
    let message = match status.as_str() {
        "queued" => cli_text("waitPeer"),
        "connecting" => cli_text("connected"),
        "waiting" => cli_text("waitingAcceptance"),
        "accepted" => cli_text("peerAccepted"),
        "paused" => cli_text_args("pauseMessage", &[("progress", &progress)]),
        "remote_paused" => cli_text_args("remotePausedMessage", &[("progress", &progress)]),
        "remote_offline" | "offline" => cli_text_args("offlineMessage", &[("progress", &progress)]),
        "interrupted" => cli_text_args("interruptedMessage", &[("progress", &progress)]),
        "manual_reconnecting" => cli_text("manualReconnect"),
        "cancelling" => cli_text("cancelProgress"),
        "finishing" => cli_text("finishProgress"),
        _ => return None,
    };
    Some(match task_error(task) {
        Some(error) => format!("{message} ({error})"),
        None => message,
    })
}

fn print_status(status: &Value) {
    let pid = status
        .get("pid")
        .and_then(Value::as_u64)
        .unwrap_or(0)
        .to_string();
    println!("{}", cli_text_args("serviceRunning", &[("pid", &pid)]));
    if let Some(name) = status.get("name").and_then(Value::as_str) {
        if !name.is_empty() {
            println!("{}", field(&cli_text("deviceName"), name));
        }
    }
    println!(
        "{}",
        field(&cli_text("nodeId"), task_str(status, "node_id"))
    );
    println!(
        "{}",
        field(&cli_text("tcpPort"), task_u64(status, "tcp_port"))
    );
    println!(
        "{}",
        field(&cli_text("quicPort"), task_u64(status, "quic_port"))
    );
}

fn print_task(task: &Value) {
    println!(
        "{}{}{}",
        cli_text("taskLabel"),
        cli_text("taskColon"),
        content_title(task)
    );
    let status = task_str(task, "status");
    let done = task_u64(task, "completed_bytes");
    let total = task_u64(task, "total_bytes");
    let mut label = status_text(&status).to_string();
    if status != "completed" && total > 0 && done > 0 {
        let progress = cli_text_args("progress", &[("percent", &percent(done, total))]);
        let (open, close) = if matches!(cli_language(), "zh-CN" | "zh-TW" | "ja-JP") {
            ("（", "）")
        } else {
            (" (", ")")
        };
        label = format!("{label}{open}{progress}{close}");
    }
    println!("{}", field(&cli_text("statusLabel"), label));
    let direction = if task_str(task, "direction") == "send" {
        cli_text("directionSend")
    } else {
        cli_text("directionReceive")
    };
    println!(
        "{}",
        field(
            &cli_text("directionLabel"),
            format!(
                "{direction}, {} {}",
                cli_text("peerLabel"),
                peer_label(task)
            )
        )
    );
    println!("{}", field(&cli_text("contentLabel"), content_line(task)));
    println!(
        "{}",
        field(&cli_text("progressLabel"), progress_summary(task))
    );
    if let Some(destination) = task.get("destination").and_then(Value::as_str) {
        if !destination.is_empty() {
            println!("{}", field(&cli_text("destinationLabel"), destination));
        }
    }
    if let Some(error) = task_error(task) {
        println!("{}", field(&cli_text("errorLabel"), error));
    }
    if let Some(event) = task
        .get("events")
        .and_then(Value::as_array)
        .and_then(|events| events.last())
        .and_then(Value::as_str)
    {
        println!("{}", field(&cli_text("latestEvent"), event));
    }
}

fn print_action(task_name: &str, action: &str) {
    let message = match action {
        "pause" => cli_text("paused"),
        "resume" => cli_text("resumeDone"),
        "retry" => cli_text("retryStarted"),
        "cancel" => cli_text("cancelled"),
        "finish" => cli_text("finishStarted"),
        _ => cli_text("actionDone"),
    };
    println!("{}{}{task_name}", message, cli_text("taskColon"));
}

fn status_text(status: &str) -> String {
    let key = match status {
        "queued" => "queued",
        "connecting" => "statusConnecting",
        "waiting" => "waiting",
        "accepted" => "accepted",
        "transferring" => "transferring",
        "paused" => "paused",
        "remote_paused" => "remotePaused",
        "remote_offline" | "offline" => "offline",
        "interrupted" => "interrupted",
        "manual_reconnecting" => "reconnecting",
        "failed" => "failed",
        "incompatible" => "incompatible",
        "partial_completed" => "partial",
        "finishing" => "finishing",
        "completed" => "completed",
        "rejected" => "rejected",
        "cancelling" => "cancelling",
        "cancelled" => "cancelled",
        _ => "unknown",
    };
    cli_text(key)
}

fn task_str(task: &Value, key: &str) -> String {
    task.get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

fn task_u64(task: &Value, key: &str) -> u64 {
    task.get(key).and_then(Value::as_u64).unwrap_or(0)
}

fn task_error(task: &Value) -> Option<String> {
    task.get("error")
        .and_then(Value::as_str)
        .filter(|error| !error.is_empty())
        .map(str::to_string)
}

fn content_title(task: &Value) -> String {
    let name = task_str(task, "file_name");
    if name.is_empty() {
        task_str(task, "id")
    } else {
        name
    }
}

fn peer_label(task: &Value) -> String {
    let name = task_str(task, "peer_name");
    if name.is_empty() {
        task_str(task, "peer_id")
    } else {
        name
    }
}

fn content_line(task: &Value) -> String {
    let files = task_u64(task, "file_count");
    let directories = task_u64(task, "directory_count");
    let total = task_u64(task, "total_bytes");
    let mut parts = vec![cli_text_args("fileCount", &[("count", &files.to_string())])];
    if directories > 0 {
        parts.push(cli_text_args(
            "directoryCount",
            &[("count", &directories.to_string())],
        ));
    }
    if total > 0 {
        parts.push(format_bytes(total));
    }
    let (open, close, separator) = match cli_language() {
        "zh-CN" | "zh-TW" => ("（", "）", "，"),
        "ja-JP" => ("（", "）", "、"),
        _ => (" (", ")", ", "),
    };
    format!(
        "{}{open}{}{close}",
        content_title(task),
        parts.join(separator)
    )
}

fn progress_summary(task: &Value) -> String {
    let done = task_u64(task, "completed_bytes");
    let total = task_u64(task, "total_bytes");
    if total > 0 {
        let punctuation = if cli_language() == "en-US" {
            " ("
        } else {
            "（"
        };
        let closing = if cli_language() == "en-US" {
            ")"
        } else {
            "）"
        };
        format!(
            "{} / {}{punctuation}{}{closing}",
            format_bytes(done),
            format_bytes(total),
            percent(done, total)
        )
    } else if done > 0 {
        cli_text_args("receivedBytes", &[("bytes", &format_bytes(done))])
    } else {
        cli_text("notStarted")
    }
}

fn progress_line(task: &Value, speed: f64) -> String {
    let done = task_u64(task, "completed_bytes");
    let total = task_u64(task, "total_bytes");
    let mut line = cli_text("transferring");
    if total > 0 {
        let ratio = (done as f64 / total as f64).clamp(0.0, 1.0);
        let width = 16usize;
        let filled = (ratio * width as f64).round() as usize;
        line.push('[');
        line.push_str(&"█".repeat(filled));
        line.push_str(&"░".repeat(width - filled));
        line.push_str(&format!("]  {}  ", percent(done, total)));
        line.push_str(&format!("{} / {}", format_bytes(done), format_bytes(total)));
    } else {
        line.push_str(&cli_text_args(
            "receivedBytes",
            &[("bytes", &format_bytes(done))],
        ));
    }
    if speed > 0.0 {
        line.push_str(&format!("  {}", format_speed(speed)));
        if total > done {
            line.push_str("  ");
            line.push_str(&cli_text_args(
                "remaining",
                &[("time", &format_duration((total - done) as f64 / speed))],
            ));
        }
    }
    line
}

fn progress_key(task: &Value) -> u64 {
    let done = task_u64(task, "completed_bytes");
    let total = task_u64(task, "total_bytes");
    if total > 0 {
        done.saturating_mul(100) / total
    } else {
        done >> 20
    }
}

fn percent(done: u64, total: u64) -> String {
    if total == 0 {
        return "0%".into();
    }
    let value = (done as f64 * 100.0 / total as f64)
        .floor()
        .clamp(0.0, 100.0);
    format!("{value:.0}%")
}

fn format_bytes(n: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    if n < 1024 {
        return format!("{n} B");
    }
    let mut value = n as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    format!("{value:.1} {}", UNITS[unit])
}

fn format_speed(bytes_per_second: f64) -> String {
    if !bytes_per_second.is_finite() || bytes_per_second <= 0.0 {
        return "0 B/s".into();
    }
    format!("{}/s", format_bytes(bytes_per_second as u64))
}

fn format_duration(seconds: f64) -> String {
    if !seconds.is_finite() || seconds <= 0.0 {
        return cli_text_args("durationSeconds", &[("count", "1")]);
    }
    let seconds = seconds.round() as u64;
    if seconds < 60 {
        cli_text_args("durationSeconds", &[("count", &seconds.to_string())])
    } else if seconds < 3600 {
        cli_text_args(
            "durationMinutes",
            &[
                ("minutes", &(seconds / 60).to_string()),
                ("seconds", &(seconds % 60).to_string()),
            ],
        )
    } else {
        cli_text_args(
            "durationHours",
            &[
                ("hours", &(seconds / 3600).to_string()),
                ("minutes", &(seconds % 3600 / 60).to_string()),
            ],
        )
    }
}

fn field(label: &str, value: impl std::fmt::Display) -> String {
    let pad = 12usize.saturating_sub(display_width(label));
    format!("  {label}{}{value}", " ".repeat(pad))
}

/// 终端显示宽度：CJK 与全角字符按两列计算，用于原地刷新时清除残留。
fn display_width(text: &str) -> usize {
    text.chars().map(|c| if is_wide(c) { 2 } else { 1 }).sum()
}

fn is_wide(c: char) -> bool {
    matches!(
        c as u32,
        0x1100..=0x115F
            | 0x2E80..=0xA4CF
            | 0xAC00..=0xD7A3
            | 0xF900..=0xFAFF
            | 0xFE10..=0xFE19
            | 0xFE30..=0xFE6F
            | 0xFF00..=0xFF60
            | 0xFFE0..=0xFFE6
            | 0x1F300..=0x1F64F
            | 0x1F900..=0x1F9FF
            | 0x20000..=0x3FFFD
    )
}

/// 交互式终端原地刷新进度行；重定向到文件时只在百分比变化时输出。
struct Console {
    interactive: bool,
    width: usize,
    used: bool,
    last_key: Option<u64>,
}

impl Console {
    fn new() -> Self {
        Self {
            interactive: io::stdout().is_terminal(),
            width: 0,
            used: false,
            last_key: None,
        }
    }
    fn line(&mut self, text: &str) {
        self.finish();
        println!("{text}");
    }
    fn progress(&mut self, key: u64, text: &str) {
        if self.interactive {
            let pad = self.width.saturating_sub(display_width(text));
            let mut out = io::stdout();
            let _ = write!(out, "\r{text}{}", " ".repeat(pad));
            let _ = out.flush();
            self.width = display_width(text);
            self.used = true;
        } else {
            if self.last_key == Some(key) {
                return;
            }
            self.last_key = Some(key);
            println!("{text}");
        }
    }
    fn finish(&mut self) {
        if self.interactive && self.used {
            println!();
            self.width = 0;
            self.used = false;
        }
    }
}

/// 以最近一次采样估算瞬时速度，并做指数平滑避免数字跳动。
struct SpeedTracker {
    last: Instant,
    last_bytes: u64,
    speed: f64,
}

impl SpeedTracker {
    fn new() -> Self {
        Self {
            last: Instant::now(),
            last_bytes: 0,
            speed: 0.0,
        }
    }
    fn sample(&mut self, bytes: u64) {
        let now = Instant::now();
        let elapsed = now.duration_since(self.last).as_secs_f64();
        if elapsed < 0.2 {
            return;
        }
        let delta = bytes.saturating_sub(self.last_bytes);
        let instant = delta as f64 / elapsed;
        self.speed = if self.speed <= 0.0 {
            instant
        } else {
            self.speed * 0.7 + instant * 0.3
        };
        self.last = now;
        self.last_bytes = bytes;
    }
    fn value(&self) -> f64 {
        self.speed
    }
}

#[cfg(unix)]
pub mod local_unix {
    use super::*;
    use fs2::FileExt;
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    use std::{
        fs::{File, OpenOptions},
        path::PathBuf,
        sync::OnceLock,
    };

    pub fn socket_path() -> io::Result<PathBuf> {
        let root = dirs::data_dir()
            .ok_or_else(|| io::Error::other("Application data directory is unavailable"))?
            .join("NodeSend")
            .join("control");
        std::fs::create_dir_all(&root)?;
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700))?;
        Ok(root.join("service.sock"))
    }
    pub fn acquire_owner() -> io::Result<bool> {
        static OWNER: OnceLock<File> = OnceLock::new();
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .mode(0o600)
            .open(socket_path()?.with_file_name("service.lock"))?;
        match file.try_lock_exclusive() {
            Ok(()) => {
                let _ = OWNER.set(file);
                Ok(true)
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => Ok(false),
            Err(error) => Err(error),
        }
    }
    pub fn acquire_gui() -> io::Result<bool> {
        static GUI_OWNER: OnceLock<File> = OnceLock::new();
        let path = socket_path()?.with_file_name("gui.lock");
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .mode(0o600)
            .open(path)?;
        match file.try_lock_exclusive() {
            Ok(()) => {
                let _ = GUI_OWNER.set(file);
                Ok(true)
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                std::fs::write(socket_path()?.with_file_name("gui.activate"), [])?;
                Ok(false)
            }
            Err(error) => Err(error),
        }
    }
    pub fn acquire_cli() -> io::Result<bool> {
        static CLI_OWNER: OnceLock<File> = OnceLock::new();
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .mode(0o600)
            .open(socket_path()?.with_file_name("cli.lock"))?;
        match file.try_lock_exclusive() {
            Ok(()) => {
                let _ = CLI_OWNER.set(file);
                Ok(true)
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => Ok(false),
            Err(error) => Err(error),
        }
    }
    pub fn start_gui_listener(app: tauri::AppHandle) {
        tauri::async_runtime::spawn(async move {
            let Ok(path) = socket_path().map(|p| p.with_file_name("gui.activate")) else {
                return;
            };
            loop {
                if std::fs::remove_file(&path).is_ok() {
                    if let Some(window) = app.get_webview_window("main") {
                        let _ = window.show();
                        let _ = window.unminimize();
                        let _ = window.set_focus();
                    }
                }
                tokio::time::sleep(Duration::from_millis(250)).await;
            }
        });
    }
    pub fn listen() -> io::Result<tokio::net::UnixListener> {
        let path = socket_path()?;
        match std::fs::remove_file(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        tokio::net::UnixListener::bind(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn progress_line_reports_percent_size_speed_and_eta() {
        let task =
            json!({"status": "transferring", "completed_bytes": 100u64, "total_bytes": 200u64});
        let line = progress_line(&task, 50.0);
        assert!(line.contains("50%"), "{line}");
        assert!(line.contains("100 B / 200 B"), "{line}");
        assert!(line.contains("50 B/s"), "{line}");
        assert!(line.contains(&format_duration(2.0)), "{line}");
    }

    #[test]
    fn status_messages_only_cover_known_states() {
        for state in [
            "queued",
            "connecting",
            "waiting",
            "accepted",
            "paused",
            "remote_paused",
            "interrupted",
            "completed",
        ] {
            assert_ne!(status_text(state), cli_text("unknown"), "{state}");
        }
        assert!(transition_message(&json!({"status": "transferring"})).is_none());
        assert!(transition_message(
            &json!({"status": "interrupted", "completed_bytes": 1u64, "total_bytes": 4u64})
        )
        .unwrap()
        .contains("25%"));
    }

    #[test]
    fn sizes_and_durations_read_as_plain_text() {
        assert_eq!(format_bytes(0), "0 B");
        assert_eq!(format_bytes(1536), "1.5 KB");
        assert_eq!(
            format_duration(2.0),
            cli_text_args("durationSeconds", &[("count", "2")])
        );
        assert_eq!(
            format_duration(90.0),
            cli_text_args("durationMinutes", &[("minutes", "1"), ("seconds", "30")])
        );
        assert_eq!(
            format_duration(3700.0),
            cli_text_args("durationHours", &[("hours", "1"), ("minutes", "1")])
        );
    }

    #[test]
    fn cli_task_selection_requires_indices_for_duplicate_names() {
        let tasks = json!([
            {"id":"hidden-1","file_name":"archive.zip","direction":"send","status":"transferring"},
            {"id":"hidden-2","file_name":"archive.zip","direction":"receive","status":"paused"},
            {"id":"hidden-3","file_name":"old.zip","direction":"send","status":"completed"}
        ]);
        let all = tasks.as_array().unwrap();
        assert_eq!(
            resolve_task_selector(&tasks, "2", "active", true).unwrap()["id"],
            "hidden-2"
        );
        assert!(resolve_task_selector(&tasks, "archive.zip", "active", true).is_err());
        assert!(resolve_task_selector(&tasks, "old.zip", "active", true).is_err());
        assert_eq!(tasks_in_category(all, "active").unwrap().len(), 2);
        assert_eq!(tasks_in_category(all, "all").unwrap().len(), 3);
    }

    #[tokio::test]
    async fn request_and_response_use_the_same_ipc_stream() {
        let (mut caller, mut owner) = tokio::io::duplex(4096);
        let server = tokio::spawn(async move {
            let request: Request = read_frame(&mut owner).await.unwrap();
            assert_eq!(request.command, "send");
            assert_eq!(request.args, [r"10.1.2.2", r"D:\NodeSend\Test\archive.zip"]);
            write_frame(
                &mut owner,
                &Reply {
                    result: json!({"task_id": "test-task", "pid": std::process::id()}),
                    error: None,
                },
            )
            .await
            .unwrap();
        });
        write_frame(
            &mut caller,
            &Request {
                command: "send".into(),
                args: vec!["10.1.2.2".into(), r"D:\NodeSend\Test\archive.zip".into()],
            },
        )
        .await
        .unwrap();
        let reply: Reply = read_frame(&mut caller).await.unwrap();
        assert_eq!(reply.result["pid"], std::process::id());
        assert!(reply.error.is_none());
        server.await.unwrap();
    }

    #[tokio::test]
    async fn oversized_ipc_request_is_rejected_before_allocating_body() {
        let (mut caller, mut owner) = tokio::io::duplex(16);
        caller.write_u32((MAX_FRAME + 1) as u32).await.unwrap();
        assert!(read_frame::<Request>(&mut owner).await.is_err());
    }
}
