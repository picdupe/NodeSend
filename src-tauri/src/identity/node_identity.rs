//! 文件功能：Node 身份的生成、持久化与加载（基线 §2.1、§3）。
//!
//! 身份格式约定：
//! - 密钥算法：ECDSA P-256（NIST secp256r1）；
//! - Node ID = hex(SHA-256(公钥 SEC1 未压缩点))，共 64 个十六进制字符；
//! - 私钥以 PKCS#8 PEM 保存（node.key.pem），证书为自签名 PEM（node.cert.pem）；
//! - 私钥不通过 IPC 传输、不导出（基线 §46、§20）。
use std::sync::Arc;

use p256::ecdsa::SigningKey;
use p256::pkcs8::{DecodePrivateKey, EncodePrivateKey, LineEnding};
use rand::rngs::OsRng;
use rcgen::{CertificateParams, DistinguishedName, DnType, KeyPair, KeyUsagePurpose};
use sha2::{Digest, Sha256};

use crate::config::AppPaths;
use crate::error::{AppError, AppResult};

#[derive(Debug)]
pub struct NodeIdentity {
    /// 设备 / Node 唯一标识（公钥派生，不可复制、不可同步）
    pub node_id: String,
    /// 自签名证书 PEM（与 node_id 绑定）；初版已生成，供后续 QUIC 层使用
    #[allow(dead_code)]
    pub cert_pem: String,
    // 私钥 PEM 仅在需要时通过方法读取，不在这里长期暴露字段。
}

impl NodeIdentity {
    /// 读取与证书对应的私钥。仅供 Service 内部构造 TLS/QUIC 端点，绝不通过 IPC 暴露。
    pub fn key_pem(paths: &AppPaths) -> AppResult<String> {
        std::fs::read_to_string(&paths.identity_key).map_err(AppError::from)
    }
}

impl NodeIdentity {
    /// 加载已有身份；不存在则生成并持久化。
    pub fn load_or_create(paths: &AppPaths) -> AppResult<Arc<Self>> {
        if paths.identity_key.exists() && !paths.identity_cert.exists() {
            let key_pem = std::fs::read_to_string(&paths.identity_key)?;
            let key = SigningKey::from_pkcs8_pem(&key_pem)
                .map_err(|e| AppError::Crypto(e.to_string()))?;
            std::fs::write(
                &paths.identity_cert,
                self_signed_cert(&key_pem, &node_id_from_key(&key))?,
            )?;
        }
        if paths.identity_cert.exists() && !paths.identity_key.exists() {
            return Err(AppError::Crypto(
                "身份私钥缺失；拒绝自动覆盖已有 Node 身份".into(),
            ));
        }
        if paths.identity_key.exists() && paths.identity_cert.exists() {
            return Self::load(paths);
        }
        Self::create_new(paths)
    }

    /// 从磁盘加载身份并由私钥重新计算 Node ID。
    fn load(paths: &AppPaths) -> AppResult<Arc<Self>> {
        let key_pem = std::fs::read_to_string(&paths.identity_key)?;
        let cert_pem = std::fs::read_to_string(&paths.identity_cert)?;
        let signing_key = SigningKey::from_pkcs8_pem(&key_pem)
            .map_err(|err| AppError::Crypto(format!("加载身份私钥失败: {err}")))?;
        let node_id = node_id_from_key(&signing_key);
        tracing::info!(%node_id, "已加载本机 Node Identity");
        Ok(Arc::new(Self { node_id, cert_pem }))
    }

    /// 生成全新 ECDSA P-256 身份与自签名证书并落盘。
    fn create_new(paths: &AppPaths) -> AppResult<Arc<Self>> {
        let signing_key = SigningKey::random(&mut OsRng);
        let node_id = node_id_from_key(&signing_key);

        let key_pem = signing_key
            .to_pkcs8_pem(LineEnding::LF)
            .map_err(|err| AppError::Crypto(format!("私钥编码失败: {err}")))?;

        let cert_pem = self_signed_cert(&key_pem, &node_id)?;

        // 先写临时文件再落位，降低写一半损坏身份文件的风险。
        std::fs::write(&paths.identity_key, key_pem.as_bytes())?;
        std::fs::write(&paths.identity_cert, cert_pem.as_bytes())?;

        tracing::info!(%node_id, "已生成新的 Node Identity 与自签名证书");
        Ok(Arc::new(Self { node_id, cert_pem }))
    }
}

/// 由公钥 SEC1 未压缩点（65 字节）计算 Node ID。
fn node_id_from_key(signing_key: &SigningKey) -> String {
    let point = signing_key.verifying_key().to_encoded_point(false);
    let digest = Sha256::digest(point.as_bytes());
    hex_encode(&digest)
}

/// 生成与 node_id 绑定的 ECDSA P-256 自签名证书（基线 §3）。
fn self_signed_cert(key_pem: &str, node_id: &str) -> AppResult<String> {
    let key_pair = KeyPair::from_pem(key_pem)
        .map_err(|err| AppError::Certificate(format!("密钥导入 rcgen 失败: {err}")))?;

    let mut params = CertificateParams::new(Vec::<String>::new())
        .map_err(|err| AppError::Certificate(format!("证书参数初始化失败: {err}")))?;
    params.key_usages = vec![
        KeyUsagePurpose::DigitalSignature,
        KeyUsagePurpose::KeyAgreement,
    ];
    let mut dn = DistinguishedName::new();
    dn.push(DnType::CommonName, node_id);
    params.distinguished_name = dn;
    params.subject_alt_names = vec![]; // 证书与 Node ID 绑定，不写 IP/主机名

    let cert = params
        .self_signed(&key_pair)
        .map_err(|err| AppError::Certificate(format!("自签名证书签发失败: {err}")))?;
    Ok(cert.pem())
}

/// 小写十六进制编码（不引入额外依赖）。
fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0f) as usize] as char);
    }
    out
}
