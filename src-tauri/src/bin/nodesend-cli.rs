//! NodeSend Linux CLI (first iteration).
//!
//! Usage:
//!   nodesend-cli receive <directory>
//!   nodesend-cli send <peer-address> <path> [--no-hash]
//!
//! The CLI uses the same profile and native transport as the desktop app. Set
//! NODESEND_DATA_DIR when it must share an existing GUI profile.

use std::{env, path::PathBuf, sync::Arc};
use nodesend_lib::{config::AppPaths, network::NetworkManager, service::AppService, transfer::model::ConflictPolicy};

fn data_dir() -> PathBuf {
    env::var_os("NODESEND_DATA_DIR").map(PathBuf::from).unwrap_or_else(|| {
        dirs::data_dir().unwrap_or_else(|| PathBuf::from("." )).join("NodeSend")
    })
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = env::args().skip(1);
    let command = args.next().unwrap_or_else(|| "help".into());
    let app = AppService::init(AppPaths::new(data_dir()))?;
    let network = NetworkManager::start(Arc::clone(&app)).await?;
    match command.as_str() {
        "daemon" => {
            println!("NodeSend CLI 在线");
            println!("Node ID: {}", app.identity.node_id);
            println!("TCP: {}, QUIC: {}", network.tcp_port, network.quic_port);
            tokio::signal::ctrl_c().await?;
        }
        "receive" => {
            let directory = args.next().ok_or("缺少接收目录")?;
            std::fs::create_dir_all(&directory)?;
            println!("NodeSend CLI 接收中: {}", directory);
            loop {
                for task in network.transfers.snapshots().into_iter().filter(|t| t.direction == "receive" && t.status == "waiting") {
                    network.decide_and_trust(task.id, true, Some(directory.clone()), ConflictPolicy::Rename, None, false).await?;
                }
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            }
        }
        "send" => {
            let address = args.next().ok_or("缺少对方地址")?;
            let path = args.next().ok_or("缺少文件路径")?;
            let verify_hash = !args.any(|arg| arg == "--no-hash");
            let peer = network.add_endpoint(address, None, false).await?;
            let id = network.send(peer.node_id.clone(), vec![path], verify_hash, false).await?;
            println!("任务已创建: {id}");
            loop {
                let task = network.transfers.snapshots().into_iter().find(|t| t.id == id);
                match task {
                    Some(t) => {
                        println!("{}: {}/{} bytes ({})", t.status, t.completed_bytes, t.total_bytes, t.transport.unwrap_or_default());
                        if ["completed", "cancelled", "rejected"].contains(&t.status.as_str()) { break; }
                    }
                    None => break,
                }
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            }
        }
        _ => {
            eprintln!("用法: nodesend-cli daemon | receive <接收目录> | send <对方地址> <文件路径> [--no-hash]");
            std::process::exit(2);
        }
    }
    Ok(())
}
