//! 文件功能：NodeSend 可执行程序入口。
//!
//! - 只负责调用 lib 中的 run()，不写任何业务逻辑（基线 §53：
//!   Root/main 文件只负责组合、注册、启动）；
//! - 桌面端（Windows/Linux/macOS）由本入口启动 Tauri UI + Service；
//!   Android/iOS 由 Tauri 移动端构建直接使用 lib，不走本文件。
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    nodesend_lib::run();
}
