//! 文件功能：身份模块入口（基线 §2/§3）。
//!
//! - 首次初始化自动生成 Node Identity（ECDSA P-256）；
//! - Node ID 由本机公钥派生，与 IP/hostname/port 无关，私钥只保存在本机；
//! - 自动生成自签名证书，证书与 node_id 绑定，供后续 QUIC TLS 1.3 使用；
//! - 正常证书续签不改变 Node ID（初版仅初始化，续签/身份变化检测为后续迭代）。
pub mod node_identity;

pub use node_identity::NodeIdentity;
