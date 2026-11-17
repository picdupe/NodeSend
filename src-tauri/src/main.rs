#![cfg_attr(windows, windows_subsystem = "windows")]
fn main() {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    if args.first().map(String::as_str) == Some("--web-entry") {
        detach_console();
        let Some(uri) = args.get(1) else {
            std::process::exit(2);
        };
        std::process::exit(nodesend_lib::process_host::run_web_protocol(uri.clone()));
    }
    if let Some(uri) = args.first().and_then(|arg| web_protocol_uri(arg)) {
        detach_console();
        std::process::exit(nodesend_lib::process_host::launch_web_protocol(uri));
    }

    match args.first().map(String::as_str) {
        Some("--service-host") => std::process::exit(nodesend_lib::process_host::run_service()),
        Some("--cli-entry") => std::process::exit(nodesend_lib::process_host::run_cli_entry()),
        Some("--gui-client") => {
            detach_console();
            nodesend_lib::process_host::init_gui_log();
            tracing::info!(pid = std::process::id(), "GUI entry process started");
            #[cfg(windows)]
            if !nodesend_lib::single_instance::acquire_gui() {
                tracing::info!("Another GUI instance owns the GUI lock");
                return;
            }
            #[cfg(unix)]
            if !nodesend_lib::cli::local_unix::acquire_gui().unwrap_or(false) {
                tracing::info!("Another GUI instance owns the GUI lock");
                return;
            }
            tracing::info!("GUI instance lock acquired");
            nodesend_lib::run();
        }
        _ if nodesend_lib::cli::is_cli_invocation() => {
            #[cfg(windows)]
            attach_console();
            std::process::exit(nodesend_lib::cli::run(false))
        }
        _ => {
            if !nodesend_lib::cli::has_gui_environment() {
                #[cfg(windows)]
                attach_console();
                eprintln!("{}", nodesend_lib::cli::cli_translate("noGuiEnvironment"));
                std::process::exit(1);
            }
            detach_console();
            std::process::exit(nodesend_lib::process_host::launch_gui());
        }
    }
}

fn web_protocol_uri(value: &str) -> Option<String> {
    let marker = value.find("://")?;
    let start = marker.checked_sub(2)?;
    if !value.get(start..marker)?.eq_ignore_ascii_case("ns") {
        return None;
    }
    let candidate = value.get(start..)?;
    let end = candidate.find('"').unwrap_or(candidate.len());
    let candidate = &candidate[..end];
    url::Url::parse(candidate)
        .ok()
        .filter(|uri| {
            uri.scheme() == "ns"
                && uri
                    .host_str()
                    .is_some_and(|host| host.eq_ignore_ascii_case("download"))
        })
        .map(|_| candidate.to_string())
}

#[cfg(windows)]
fn attach_console() {
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn AttachConsole(process_id: u32) -> i32;
        fn AllocConsole() -> i32;
    }
    unsafe {
        if AttachConsole(u32::MAX) == 0 {
            AllocConsole();
        }
    }
}

#[cfg(windows)]
fn detach_console() {
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn FreeConsole() -> i32;
    }
    unsafe {
        FreeConsole();
    }
}

#[cfg(not(windows))]
fn detach_console() {}
