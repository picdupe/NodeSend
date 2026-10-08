//! 文件功能：IPC 模块入口（基线 §46 Service/UI/IPC）。
//!
//! - UI 不直接操作 SQLite / 网络 / 传输，一律通过本模块注册的 Tauri 命令调用 Service；
//! - IPC 仅本机，命令在 lib.rs 通过 invoke_handler 注册；
//! - 命令实现见 [`commands`]，全部返回 `Result<T, AppError>`（错误可序列化到前端）。
pub mod commands;

pub mod browser;
pub mod network;
