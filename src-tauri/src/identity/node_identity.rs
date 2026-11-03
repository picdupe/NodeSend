//! 文件功能：Node 身份的生成、持久化与加载（基线 §2.1、§3）。
//!
//! 身份格式约定：
//! - 密钥算法：ECDSA P-256（NIST secp256r1）；
//! - Node ID = `XXXX-XXXX-XXXX-XXXX-XX`：公钥 SEC1 未压缩点的 SHA-256 摘要取前 80 bit
//!   （10 字节），按 Crockford Base32 编码为 16 个字符，再追加 2 个校验字符，共 18 字符；
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

    pub fn has_current_identity(paths: &AppPaths) -> AppResult<bool> {
        if !paths.identity_key.exists() || !paths.identity_cert.exists() {
            return Ok(false);
        }
        let key_pem = std::fs::read_to_string(&paths.identity_key)?;
        let cert_pem = std::fs::read_to_string(&paths.identity_cert)?;
        let Ok(key) = SigningKey::from_pkcs8_pem(&key_pem) else {
            return Ok(false);
        };
        let node_id = node_id_from_key(&key);
        Ok(certificate_matches(&cert_pem, &node_id))
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
                "Identity private key is missing; refusing to overwrite an existing Node identity".into(),
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
        let mut cert_pem = std::fs::read_to_string(&paths.identity_cert)?;
        let signing_key = SigningKey::from_pkcs8_pem(&key_pem)
            .map_err(|err| AppError::Crypto(format!("Failed to load identity private key: {err}")))?;
        let node_id = node_id_from_key(&signing_key);
        if !certificate_matches(&cert_pem, &node_id) {
            cert_pem = self_signed_cert(&key_pem, &node_id)?;
            std::fs::write(&paths.identity_cert, cert_pem.as_bytes())?;
            tracing::info!(%node_id, "Reissued certificate for the current Node ID");
        }
        tracing::info!(%node_id, "Loaded local Node identity");
        Ok(Arc::new(Self { node_id, cert_pem }))
    }

    /// 生成全新 ECDSA P-256 身份与自签名证书并落盘。
    fn create_new(paths: &AppPaths) -> AppResult<Arc<Self>> {
        let signing_key = SigningKey::random(&mut OsRng);
        let node_id = node_id_from_key(&signing_key);

        let key_pem = signing_key
            .to_pkcs8_pem(LineEnding::LF)
            .map_err(|err| AppError::Crypto(format!("Failed to encode private key: {err}")))?;

        let cert_pem = self_signed_cert(&key_pem, &node_id)?;

        // 先写临时文件再落位，降低写一半损坏身份文件的风险。
        std::fs::write(&paths.identity_key, key_pem.as_bytes())?;
        std::fs::write(&paths.identity_cert, cert_pem.as_bytes())?;

        tracing::info!(%node_id, "Generated a new Node identity and self-signed certificate");
        Ok(Arc::new(Self { node_id, cert_pem }))
    }
}

fn certificate_matches(pem: &str, node_id: &str) -> bool {
    let Ok(certs) = rustls_pemfile::certs(&mut std::io::BufReader::new(pem.as_bytes()))
        .collect::<Result<Vec<_>, _>>()
    else {
        return false;
    };
    certs.len() == 1
        && crate::network::tls::certificate_node_id(certs[0].as_ref()).is_ok_and(|id| id == node_id)
}

#[cfg(test)]
mod certificate_tests {
    use super::*;

    #[test]
    fn old_certificate_requires_reissue_and_new_certificate_validates() {
        let key = SigningKey::random(&mut OsRng);
        let pem = key.to_pkcs8_pem(LineEnding::LF).unwrap();
        let id = node_id_from_key(&key);
        let old = self_signed_cert(&pem, &"a".repeat(64)).unwrap();
        assert!(!certificate_matches(&old, &id));
        let current = self_signed_cert(&pem, &id).unwrap();
        assert!(certificate_matches(&current, &id));
        assert!(!certificate_matches("invalid PEM", &id));
        let other = SigningKey::random(&mut OsRng);
        assert!(!certificate_matches(&current, &node_id_from_key(&other)));
    }
}

/// 由公钥 SEC1 未压缩点（65 字节）计算 Node ID。
fn node_id_from_key(signing_key: &SigningKey) -> String {
    let point = signing_key.verifying_key().to_encoded_point(false);
    crockford_id(&Sha256::digest(point.as_bytes()))
}

pub fn crockford_id(digest: &[u8]) -> String {
    const A: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
    let mut value = 0u128;
    for byte in digest.iter().take(10) {
        value = (value << 8) | u128::from(*byte);
    }
    let mut out = String::with_capacity(18);
    for shift in (0..16).rev() {
        out.push(A[((value >> (shift * 5)) & 31) as usize] as char);
    }
    let sum = out.bytes().fold(0u16, |n, c| {
        n + A.iter().position(|x| *x == c).unwrap() as u16
    });
    out.push(A[((sum >> 5) & 31) as usize] as char);
    out.push(A[(sum & 31) as usize] as char);
    format!(
        "{}-{}-{}-{}-{}",
        &out[0..4],
        &out[4..8],
        &out[8..12],
        &out[12..16],
        &out[16..18]
    )
}

/// Node ID 使用的 Base32 字符集（Crockford：不含 I/L/O/U）。
pub const ALPHABET: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

/// 把外部输入的 Node ID 规整为规范形式 `XXXX-XXXX-XXXX-XXXX-XX`。
///
/// - 忽略 `-`、空格、下划线等分隔符；
/// - 大小写不敏感；
/// - 输入容错：`I`/`L` → `1`，`O` → `0`（自动生成时永远不会产生这几个字符）；
/// - 校验字符集、长度（16 位数据 + 2 位校验）与校验和；
/// - 任何一项不满足都返回 `None`，调用方据此拒绝该输入。
pub fn normalize_node_id(raw: &str) -> Option<String> {
    let mut data = String::with_capacity(18);
    for ch in raw.chars() {
        if ch == '-' || ch == '_' || ch.is_whitespace() {
            continue;
        }
        let upper = ch.to_ascii_uppercase();
        let mapped = match upper {
            'I' | 'L' => '1',
            'O' => '0',
            other => other,
        };
        if !mapped.is_ascii() || !ALPHABET.contains(&(mapped as u8)) {
            return None;
        }
        data.push(mapped);
    }
    if data.len() != 18 {
        return None;
    }
    let (body, checksum) = data.split_at(16);
    let sum: u16 = body
        .bytes()
        .map(|c| ALPHABET.iter().position(|x| *x == c).unwrap() as u16)
        .sum();
    let expected = format!(
        "{}{}",
        ALPHABET[((sum >> 5) & 31) as usize] as char,
        ALPHABET[(sum & 31) as usize] as char
    );
    if checksum != expected {
        return None;
    }
    Some(format!(
        "{}-{}-{}-{}-{}",
        &body[0..4],
        &body[4..8],
        &body[8..12],
        &body[12..16],
        checksum
    ))
}

/// 是否为合法 Node ID（含格式容错）。
pub fn is_node_id(raw: &str) -> bool {
    normalize_node_id(raw).is_some()
}

/// 生成与 node_id 绑定的 ECDSA P-256 自签名证书（基线 §3）。
fn self_signed_cert(key_pem: &str, node_id: &str) -> AppResult<String> {
    let key_pair = KeyPair::from_pem(key_pem).map_err(|err| {
        AppError::Certificate(format!("Failed to import key into rcgen: {err}"))
    })?;

    let mut params = CertificateParams::new(Vec::<String>::new())
        .map_err(|err| AppError::Certificate(format!("Failed to initialize certificate parameters: {err}")))?;
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
        .map_err(|err| AppError::Certificate(format!("Failed to issue self-signed certificate: {err}")))?;
    Ok(cert.pem())
}
