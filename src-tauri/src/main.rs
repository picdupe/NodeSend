#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
fn main() {
    match std::env::args().nth(1).as_deref() {
        Some("--service-host") => std::process::exit(nodesend_lib::process_host::run_service()),
        Some("--cli-entry") => std::process::exit(nodesend_lib::process_host::run_cli_entry()),
        Some("--gui-client") => {
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
            std::process::exit(nodesend_lib::cli::run(false))
        }
        _ => {
            if !nodesend_lib::cli::has_gui_environment() {
                eprintln!("{}", nodesend_lib::cli::cli_translate("noGuiEnvironment"));
                std::process::exit(1);
            }
            std::process::exit(nodesend_lib::process_host::launch_gui());
        }
    }
}
