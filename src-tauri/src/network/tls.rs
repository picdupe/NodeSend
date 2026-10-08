//! Mutual TLS 1.3 using device keys. Discovery data never establishes Trust.
use crate::{
    error::{AppError, AppResult},
    service::AppService,
};
use rustls::{
    client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier},
    pki_types::{CertificateDer, ServerName, UnixTime},
    server::danger::{ClientCertVerified, ClientCertVerifier},
    DigitallySignedStruct, SignatureScheme,
};
use sha2::{Digest, Sha256};
use std::{io::BufReader, sync::Arc};
use x509_parser::prelude::*;

pub fn certificate_node_id(der: &[u8]) -> Result<String, rustls::Error> {
    let invalid = || rustls::Error::General("无效的 Node 身份证书".into());
    let (rest, cert) = X509Certificate::from_der(der).map_err(|_| invalid())?;
    if !rest.is_empty() || !cert.validity().is_valid() {
        return Err(invalid());
    }
    cert.verify_signature(None).map_err(|_| invalid())?;
    let raw = cert.public_key().subject_public_key.data.as_ref();
    let key = p256::PublicKey::from_sec1_bytes(raw).map_err(|_| invalid())?;
    use p256::elliptic_curve::sec1::ToEncodedPoint;
    let node_id = format!(
        "{:x}",
        Sha256::digest(key.to_encoded_point(false).as_bytes())
    );
    let cn = cert
        .subject()
        .iter_common_name()
        .next()
        .and_then(|v| v.as_str().ok());
    if cn != Some(node_id.as_str()) {
        return Err(invalid());
    }
    Ok(node_id)
}

#[derive(Debug)]
struct NodeVerifier {
    expected: Option<String>,
}
impl NodeVerifier {
    fn validate(
        &self,
        cert: &CertificateDer<'_>,
        chain: &[CertificateDer<'_>],
    ) -> Result<(), rustls::Error> {
        if !chain.is_empty() {
            return Err(rustls::Error::General("只接受 Node 自签名身份".into()));
        }
        let id = certificate_node_id(cert.as_ref())?;
        if self
            .expected
            .as_ref()
            .is_some_and(|expected| expected != &id)
        {
            return Err(rustls::Error::General("Node ID 与目标证书不匹配".into()));
        }
        Ok(())
    }
}
fn signature(
    message: &[u8],
    cert: &CertificateDer<'_>,
    dss: &DigitallySignedStruct,
) -> Result<HandshakeSignatureValid, rustls::Error> {
    rustls::crypto::verify_tls13_signature(
        message,
        cert,
        dss,
        &rustls::crypto::ring::default_provider().signature_verification_algorithms,
    )
}
impl ServerCertVerifier for NodeVerifier {
    fn verify_server_cert(
        &self,
        cert: &CertificateDer<'_>,
        chain: &[CertificateDer<'_>],
        _: &ServerName<'_>,
        _: &[u8],
        _: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        self.validate(cert, chain)?;
        Ok(ServerCertVerified::assertion())
    }
    fn verify_tls12_signature(
        &self,
        _: &[u8],
        _: &CertificateDer<'_>,
        _: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Err(rustls::Error::General("需要 TLS 1.3".into()))
    }
    fn verify_tls13_signature(
        &self,
        m: &[u8],
        c: &CertificateDer<'_>,
        s: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        signature(m, c, s)
    }
    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        vec![SignatureScheme::ECDSA_NISTP256_SHA256]
    }
}
impl ClientCertVerifier for NodeVerifier {
    fn root_hint_subjects(&self) -> &[rustls::DistinguishedName] {
        &[]
    }
    fn verify_client_cert(
        &self,
        cert: &CertificateDer<'_>,
        chain: &[CertificateDer<'_>],
        _: UnixTime,
    ) -> Result<ClientCertVerified, rustls::Error> {
        self.validate(cert, chain)?;
        Ok(ClientCertVerified::assertion())
    }
    fn verify_tls12_signature(
        &self,
        _: &[u8],
        _: &CertificateDer<'_>,
        _: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Err(rustls::Error::General("需要 TLS 1.3".into()))
    }
    fn verify_tls13_signature(
        &self,
        m: &[u8],
        c: &CertificateDer<'_>,
        s: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        signature(m, c, s)
    }
    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        vec![SignatureScheme::ECDSA_NISTP256_SHA256]
    }
}
fn material(
    s: &AppService,
) -> AppResult<(
    Vec<CertificateDer<'static>>,
    rustls::pki_types::PrivateKeyDer<'static>,
)> {
    let certs = rustls_pemfile::certs(&mut BufReader::new(s.identity.cert_pem.as_bytes()))
        .collect::<Result<Vec<_>, _>>()?;
    let key = std::fs::read(&s.paths.identity_key)?;
    let key = rustls_pemfile::private_key(&mut BufReader::new(key.as_slice()))?
        .ok_or_else(|| AppError::Certificate("身份私钥缺失".into()))?;
    Ok((certs, key))
}
pub fn server(s: &AppService) -> AppResult<rustls::ServerConfig> {
    let (certs, key) = material(s)?;
    let mut config = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_protocol_versions(&[&rustls::version::TLS13])
    .map_err(tls_error)?
    .with_client_cert_verifier(Arc::new(NodeVerifier { expected: None }))
    .with_single_cert(certs, key)
    .map_err(tls_error)?;
    config.alpn_protocols = vec![b"nodesend/1".to_vec()];
    Ok(config)
}
pub fn client(s: &AppService, expected: Option<String>) -> AppResult<rustls::ClientConfig> {
    let (certs, key) = material(s)?;
    let mut config = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_protocol_versions(&[&rustls::version::TLS13])
    .map_err(tls_error)?
    .dangerous()
    .with_custom_certificate_verifier(Arc::new(NodeVerifier { expected }))
    .with_client_auth_cert(certs, key)
    .map_err(tls_error)?;
    config.alpn_protocols = vec![b"nodesend/1".to_vec()];
    Ok(config)
}
pub fn remember(s: &AppService, certificates: &[CertificateDer<'_>]) -> AppResult<String> {
    let cert = certificates
        .first()
        .ok_or_else(|| AppError::Certificate("对端未提供身份".into()))?;
    let id = certificate_node_id(cert.as_ref()).map_err(tls_error)?;
    s.db.conn().execute("INSERT INTO node_certificates(node_id,certificate,updated_at) VALUES (?1,?2,?3) ON CONFLICT(node_id) DO UPDATE SET certificate=excluded.certificate,updated_at=excluded.updated_at", rusqlite::params![id,cert.as_ref(),chrono::Utc::now().to_rfc3339()])?;
    Ok(id)
}
fn tls_error(e: impl std::fmt::Display) -> AppError {
    AppError::Certificate(e.to_string())
}
