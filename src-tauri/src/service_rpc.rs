use serde_json::Value;
use std::sync::Arc;
pub struct Backend {
    pub service: Arc<crate::service::AppService>,
    pub network: Arc<crate::network::NetworkManager>,
    pub http: Arc<crate::httpapi::HttpManager>,
    pub links: Arc<crate::httpapi::ShareLinkManager>,
    pub protocol: Arc<crate::share::servers::ProtocolManager>,
    pub browser: Arc<crate::browser::BrowserManager>,
}
fn argument<T: serde::de::DeserializeOwned>(args: &Value, name: &str) -> Result<T, String> {
    serde_json::from_value(args.get(name).cloned().unwrap_or(Value::Null))
        .map_err(|e| format!("Invalid argument `{name}`: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn service_arguments_preserve_camel_case_and_optional_values() {
        let args = serde_json::json!({"peerId":"peer", "paths":["D:/folder"]});
        assert_eq!(argument::<String>(&args, "peerId").unwrap(), "peer");
        assert_eq!(
            argument::<Vec<String>>(&args, "paths").unwrap(),
            vec!["D:/folder"]
        );
        assert_eq!(
            argument::<Option<String>>(&args, "preferredAddress").unwrap(),
            None
        );
        assert!(argument::<bool>(&args, "verifyHash").is_err());
    }
    #[test]
    fn shared_dispatch_includes_all_service_domains_and_excludes_ui_operations() {
        for name in [
            "get_node_info",
            "get_network_status",
            "send_files",
            "transfer_action",
            "set_network_settings",
            "list_shares",
            "browse_browser",
        ] {
            assert!(is_service_command(name), "{name}");
        }
        for name in [
            "choose_send_files",
            "read_clipboard_files",
            "check_update",
            "invalid",
        ] {
            assert!(!is_service_command(name), "{name}");
        }
    }
}
impl Backend {
    pub async fn invoke(&self, command: &str, args: Value) -> Result<Value, String> {
        match command {
            "get_ui_language" => serde_json::to_value(
                self.service
                    .get_setting("ui_language")
                    .map_err(|e| e.to_string())?
                    .unwrap_or_else(|| "system".to_string()),
            )
            .map_err(|e| e.to_string()),
            "set_ui_language" => {
                let language: String = argument(&args, "language")?;
                if !["system", "zh-CN", "zh-TW", "en-US", "ja-JP"].contains(&language.as_str()) {
                    return Err("Unsupported UI language".into());
                }
                self.service
                    .set_setting("ui_language", &language)
                    .map_err(|e| e.to_string())?;
                serde_json::to_value(()).map_err(|e| e.to_string())
            }
            "get_node_info" => serde_json::to_value(
                crate::ipc::commands::get_node_info_service(
                    self.service.clone(),
                    self.http.clone(),
                )
                .await
                .map_err(|e| e.to_string())?,
            )
            .map_err(|e| e.to_string()),
            "start_share_link" => serde_json::to_value(
                crate::ipc::commands::start_share_link_service(
                    self.service.clone(),
                    self.links.clone(),
                )
                .await
                .map_err(|e| e.to_string())?,
            )
            .map_err(|e| e.to_string()),
            "set_http_config" => serde_json::to_value(
                crate::ipc::commands::set_http_config_service(
                    self.service.clone(),
                    self.http.clone(),
                    argument(&args, "config")?,
                )
                .await
                .map_err(|e| e.to_string())?,
            )
            .map_err(|e| e.to_string()),
            "list_shares" => serde_json::to_value(
                crate::ipc::commands::list_shares_service(self.service.clone())
                    .await
                    .map_err(|e| e.to_string())?,
            )
            .map_err(|e| e.to_string()),
            "share_protocol_status" => serde_json::to_value(
                crate::ipc::commands::share_protocol_status_service(self.protocol.clone())
                    .await
                    .map_err(|e| e.to_string())?,
            )
            .map_err(|e| e.to_string()),
            "create_share" => serde_json::to_value(
                crate::ipc::commands::create_share_service(
                    self.service.clone(),
                    self.protocol.clone(),
                    argument(&args, "input")?,
                )
                .await
                .map_err(|e| e.to_string())?,
            )
            .map_err(|e| e.to_string()),
            "update_share" => serde_json::to_value(
                crate::ipc::commands::update_share_service(
                    self.service.clone(),
                    self.protocol.clone(),
                    argument(&args, "id")?,
                    argument(&args, "input")?,
                )
                .await
                .map_err(|e| e.to_string())?,
            )
            .map_err(|e| e.to_string()),
            "toggle_share" => serde_json::to_value(
                crate::ipc::commands::toggle_share_service(
                    self.service.clone(),
                    self.protocol.clone(),
                    argument(&args, "id")?,
                    argument(&args, "enabled")?,
                )
                .await
                .map_err(|e| e.to_string())?,
            )
            .map_err(|e| e.to_string()),
            "rotate_share_token" => serde_json::to_value(
                crate::ipc::commands::rotate_share_token_service(
                    self.service.clone(),
                    self.protocol.clone(),
                    argument(&args, "id")?,
                )
                .await
                .map_err(|e| e.to_string())?,
            )
            .map_err(|e| e.to_string()),
            "delete_share" => serde_json::to_value(
                crate::ipc::commands::delete_share_service(
                    self.service.clone(),
                    self.protocol.clone(),
                    argument(&args, "id")?,
                )
                .await
                .map_err(|e| e.to_string())?,
            )
            .map_err(|e| e.to_string()),
            "list_trusted_nodes" => serde_json::to_value(
                crate::ipc::commands::list_trusted_nodes_service(self.service.clone())
                    .await
                    .map_err(|e| e.to_string())?,
            )
            .map_err(|e| e.to_string()),
            "add_trusted_node" => serde_json::to_value(
                crate::ipc::commands::add_trusted_node_service(
                    self.service.clone(),
                    argument(&args, "nodeId")?,
                    argument(&args, "displayName")?,
                )
                .await
                .map_err(|e| e.to_string())?,
            )
            .map_err(|e| e.to_string()),
            "remove_trusted_node" => serde_json::to_value(
                crate::ipc::commands::remove_trusted_node_service(
                    self.service.clone(),
                    argument(&args, "nodeId")?,
                )
                .await
                .map_err(|e| e.to_string())?,
            )
            .map_err(|e| e.to_string()),
            "list_task_entries" => serde_json::to_value(
                crate::ipc::network::list_task_entries_service(
                    self.network.clone(),
                    argument(&args, "id")?,
                    argument(&args, "offset")?,
                    argument(&args, "limit")?,
                )
                .await
                .map_err(|e| e.to_string())?,
            )
            .map_err(|e| e.to_string()),
            "get_network_status" => serde_json::to_value(
                crate::ipc::network::get_network_status_service(self.network.clone())
                    .await
                    .map_err(|e| e.to_string())?,
            )
            .map_err(|e| e.to_string()),
            "add_endpoint" => serde_json::to_value(
                crate::ipc::network::add_endpoint_service(
                    self.network.clone(),
                    argument(&args, "address")?,
                    argument(&args, "expectedNodeId")?,
                    argument(&args, "allowPublic")?,
                )
                .await
                .map_err(|e| e.to_string())?,
            )
            .map_err(|e| e.to_string()),
            "send_files" => serde_json::to_value(
                crate::ipc::network::send_files_service(
                    self.network.clone(),
                    argument(&args, "peerId")?,
                    argument(&args, "paths")?,
                    argument(&args, "verifyHash")?,
                    argument(&args, "allowPublic")?,
                    argument(&args, "preferredAddress")?,
                )
                .await
                .map_err(|e| e.to_string())?,
            )
            .map_err(|e| e.to_string()),
            "cancel_send_preflight" => serde_json::to_value(
                crate::ipc::network::cancel_send_preflight_service(
                    self.network.clone(),
                    argument(&args, "peerId")?,
                )
                .await
                .map_err(|e| e.to_string())?,
            )
            .map_err(|e| e.to_string()),
            "decide_incoming_transfer" => serde_json::to_value(
                crate::ipc::network::decide_incoming_transfer_service(
                    self.network.clone(),
                    argument(&args, "id")?,
                    argument(&args, "accepted")?,
                    argument(&args, "destination")?,
                    argument(&args, "conflict")?,
                    argument(&args, "selected")?,
                    argument(&args, "trustDevice")?,
                )
                .await
                .map_err(|e| e.to_string())?,
            )
            .map_err(|e| e.to_string()),
            "transfer_action" => serde_json::to_value(
                crate::ipc::network::transfer_action_service(
                    self.network.clone(),
                    argument(&args, "id")?,
                    argument(&args, "action")?,
                )
                .await
                .map_err(|e| e.to_string())?,
            )
            .map_err(|e| e.to_string()),
            "set_network_settings" => serde_json::to_value(
                crate::ipc::network::set_network_settings_service(
                    self.network.clone(),
                    argument(&args, "name")?,
                    argument(&args, "directory")?,
                    argument(&args, "policy")?,
                )
                .await
                .map_err(|e| e.to_string())?,
            )
            .map_err(|e| e.to_string()),
            "set_android_media_library_enabled" => serde_json::to_value(
                crate::ipc::network::set_android_media_library_enabled_service(
                    self.network.clone(),
                    argument(&args, "enabled")?,
                )
                .map_err(|e| e.to_string())?,
            )
            .map_err(|e| e.to_string()),
            "remove_peer" => serde_json::to_value(
                crate::ipc::network::remove_peer_service(
                    self.network.clone(),
                    argument(&args, "nodeId")?,
                )
                .await
                .map_err(|e| e.to_string())?,
            )
            .map_err(|e| e.to_string()),
            "connect_browser" => serde_json::to_value(
                crate::ipc::browser::connect_browser_service(
                    self.browser.clone(),
                    argument(&args, "input")?,
                )
                .await
                .map_err(|e| e.to_string())?,
            )
            .map_err(|e| e.to_string()),
            "list_browser_connections" => serde_json::to_value(
                crate::ipc::browser::list_browser_connections_service(self.browser.clone())
                    .await
                    .map_err(|e| e.to_string())?,
            )
            .map_err(|e| e.to_string()),
            "close_browser" => serde_json::to_value(
                crate::ipc::browser::close_browser_service(
                    self.browser.clone(),
                    argument(&args, "id")?,
                )
                .await
                .map_err(|e| e.to_string())?,
            )
            .map_err(|e| e.to_string()),
            "reopen_browser" => serde_json::to_value(
                crate::ipc::browser::reopen_browser_service(
                    self.browser.clone(),
                    argument(&args, "id")?,
                    argument(&args, "password")?,
                )
                .await
                .map_err(|e| e.to_string())?,
            )
            .map_err(|e| e.to_string()),
            "test_browser_connection" => serde_json::to_value(
                crate::ipc::browser::test_browser_connection_service(
                    self.browser.clone(),
                    argument(&args, "input")?,
                )
                .await
                .map_err(|e| e.to_string())?,
            )
            .map_err(|e| e.to_string()),
            "update_browser_connection" => serde_json::to_value(
                crate::ipc::browser::update_browser_connection_service(
                    self.browser.clone(),
                    argument(&args, "id")?,
                    argument(&args, "input")?,
                )
                .await
                .map_err(|e| e.to_string())?,
            )
            .map_err(|e| e.to_string()),
            "delete_browser_connection" => serde_json::to_value(
                crate::ipc::browser::delete_browser_connection_service(
                    self.browser.clone(),
                    argument(&args, "id")?,
                )
                .await
                .map_err(|e| e.to_string())?,
            )
            .map_err(|e| e.to_string()),
            "browse_browser" => serde_json::to_value(
                crate::ipc::browser::browse_browser_service(
                    self.browser.clone(),
                    argument(&args, "id")?,
                    argument(&args, "path")?,
                )
                .await
                .map_err(|e| e.to_string())?,
            )
            .map_err(|e| e.to_string()),
            "download_browser_file" => serde_json::to_value(
                crate::ipc::browser::download_browser_file_service(
                    self.browser.clone(),
                    argument(&args, "id")?,
                    argument(&args, "path")?,
                    argument(&args, "destination")?,
                )
                .await
                .map_err(|e| e.to_string())?,
            )
            .map_err(|e| e.to_string()),
            "upload_browser_file" => serde_json::to_value(
                crate::ipc::browser::upload_browser_file_service(
                    self.browser.clone(),
                    argument(&args, "id")?,
                    argument(&args, "local")?,
                    argument(&args, "path")?,
                )
                .await
                .map_err(|e| e.to_string())?,
            )
            .map_err(|e| e.to_string()),
            "create_browser_directory" => serde_json::to_value(
                crate::ipc::browser::create_browser_directory_service(
                    self.browser.clone(),
                    argument(&args, "id")?,
                    argument(&args, "path")?,
                )
                .await
                .map_err(|e| e.to_string())?,
            )
            .map_err(|e| e.to_string()),
            "delete_browser_entry" => serde_json::to_value(
                crate::ipc::browser::delete_browser_entry_service(
                    self.browser.clone(),
                    argument(&args, "id")?,
                    argument(&args, "path")?,
                )
                .await
                .map_err(|e| e.to_string())?,
            )
            .map_err(|e| e.to_string()),
            "rename_browser_entry" => serde_json::to_value(
                crate::ipc::browser::rename_browser_entry_service(
                    self.browser.clone(),
                    argument(&args, "id")?,
                    argument(&args, "path")?,
                    argument(&args, "newName")?,
                )
                .await
                .map_err(|e| e.to_string())?,
            )
            .map_err(|e| e.to_string()),
            "move_browser_entry" => serde_json::to_value(
                crate::ipc::browser::move_browser_entry_service(
                    self.browser.clone(),
                    argument(&args, "id")?,
                    argument(&args, "path")?,
                    argument(&args, "target")?,
                )
                .await
                .map_err(|e| e.to_string())?,
            )
            .map_err(|e| e.to_string()),
            _ => Err(format!("Unknown service command: {command}")),
        }
    }
}

pub fn is_service_command(command: &str) -> bool {
    matches!(
        command,
        "get_ui_language"
            | "set_ui_language"
            | "get_node_info"
            | "start_share_link"
            | "set_http_config"
            | "list_shares"
            | "share_protocol_status"
            | "create_share"
            | "update_share"
            | "toggle_share"
            | "rotate_share_token"
            | "delete_share"
            | "list_trusted_nodes"
            | "add_trusted_node"
            | "remove_trusted_node"
            | "list_task_entries"
            | "get_network_status"
            | "add_endpoint"
            | "send_files"
            | "cancel_send_preflight"
            | "decide_incoming_transfer"
            | "transfer_action"
            | "set_network_settings"
            | "set_android_media_library_enabled"
            | "remove_peer"
            | "connect_browser"
            | "list_browser_connections"
            | "close_browser"
            | "reopen_browser"
            | "test_browser_connection"
            | "update_browser_connection"
            | "delete_browser_connection"
            | "browse_browser"
            | "download_browser_file"
            | "upload_browser_file"
            | "create_browser_directory"
            | "delete_browser_entry"
            | "rename_browser_entry"
            | "move_browser_entry"
    )
}
