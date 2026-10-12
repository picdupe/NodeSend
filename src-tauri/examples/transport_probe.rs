//! Diagnostic sink: discards bytes, so results exclude receiver disk/SQLite costs.
//! Never use for real files. Run with: <bind IP:port> <tcp|quic|quic-disk> <test peer node ID>.
//! quic-disk uses the production receiver, including durable writes and task DB.
use nodesend_lib::{
    config::AppPaths,
    network::{
        protocol::{self, Request, Response},
        tls, transport,
    },
    service::AppService,
};
use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite};

struct Probe {
    app: Arc<AppService>,
    peer: String,
    tcp: u16,
    quic: u16,
    totals: Mutex<(Option<Instant>, u64)>,
}

async fn session<R: AsyncRead + Unpin, W: AsyncWrite + Unpin>(
    mut r: R,
    mut w: W,
    p: Arc<Probe>,
) -> anyhow::Result<()> {
    let request: Request = protocol::read_frame(&mut r).await?;
    anyhow::ensure!(
        matches!(request, Request::Hello { node_id, .. } if node_id == p.peer),
        "unexpected test peer"
    );
    protocol::write_frame(
        &mut w,
        &Response::Hello {
            version: protocol::VERSION,
            node_id: p.app.identity.node_id.clone(),
            display_name: "NodeSend diagnostic sink (discards files)".into(),
            tcp_port: p.tcp,
            quic_port: p.quic,
        },
    )
    .await?;
    let mut buffer = vec![0u8; 256 * 1024];
    loop {
        let request: Request = protocol::read_frame(&mut r).await?;
        let response = match request {
            Request::Addresses => Response::Addresses {
                addresses: vec![],
                tcp_port: p.tcp,
                quic_port: p.quic,
            },
            Request::Offer { .. } => {
                *p.totals.lock().unwrap() = (None, 0);
                Response::State {
                    status: "accepted".into(),
                    completed: Default::default(),
                }
            }
            Request::Poll { .. } => Response::State {
                status: "accepted".into(),
                completed: Default::default(),
            },
            Request::Chunk { length, .. } => {
                anyhow::ensure!(length <= 8 * 1024 * 1024, "oversized chunk");
                p.totals.lock().unwrap().0.get_or_insert_with(Instant::now);
                let mut remaining = length;
                while remaining > 0 {
                    let n = remaining.min(buffer.len());
                    r.read_exact(&mut buffer[..n]).await?;
                    remaining -= n;
                }
                p.totals.lock().unwrap().1 += length as u64;
                Response::Ack
            }
            Request::Commit { .. } => {
                let (start, bytes) = *p.totals.lock().unwrap();
                let seconds = start.map(|s| s.elapsed().as_secs_f64()).unwrap_or(0.0);
                println!(
                    "RESULT bytes={bytes} seconds={seconds:.3} MB/s={:.2}",
                    bytes as f64 / seconds / 1_000_000.0
                );
                Response::Ack
            }
            _ => Response::Error {
                message: "diagnostic sink only".into(),
            },
        };
        protocol::write_frame(&mut w, &response).await?;
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args: Vec<_> = std::env::args().collect();
    anyhow::ensure!(
        args.len() == 4 && ["tcp", "quic", "quic-disk"].contains(&args[2].as_str()),
        "usage: transport_probe <bind IP:port> <tcp|quic|quic-disk> <test peer node ID>"
    );
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target/perf-diagnosis")
        .join(format!("probe-{}", std::process::id()));
    let app = AppService::init(AppPaths::new(root.clone()))?;
    let listeners = transport::bind(&app, args[1].parse()?)?;
    if args[2] == "quic-disk" {
        app.set_setting("display_name", "NodeSend diagnostic disk receiver")?;
        app.set_setting(
            "receive_directory",
            &root.join("received").to_string_lossy(),
        )?;
        app.set_setting("receive_policy", "trusted")?;
        nodesend_lib::registry::add(&app.db, &args[3], Some("test peer".into()))?;
        let transfers = nodesend_lib::transfer::TransferService::new(app.clone())?;
        let peers = nodesend_lib::network::peers::PeerStore::new(app.clone())?;
        println!("READY node={} tcp={} quic={} directory={} (production receiver; stops after 5 minutes)",
            app.identity.node_id, listeners.tcp_port, listeners.quic_port, root.join("received").display());
        let _ = tokio::time::timeout(
            Duration::from_secs(300),
            transport::serve(listeners, transfers, peers),
        )
        .await;
        return Ok(());
    }
    let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(tls::server(&app)?));
    let p = Arc::new(Probe {
        app,
        peer: args[3].clone(),
        tcp: listeners.tcp_port,
        quic: if args[2] == "quic" {
            listeners.quic_port
        } else {
            0
        },
        totals: Mutex::new((None, 0)),
    });
    println!(
        "READY node={} tcp={} quic={} (discards files; stops after 5 minutes)",
        p.app.identity.node_id, p.tcp, p.quic
    );
    let stop = tokio::time::sleep(Duration::from_secs(300));
    tokio::pin!(stop);
    loop {
        tokio::select! {
            _ = &mut stop => break,
            result = listeners.tcp.accept() => {
                let (stream, _) = result?;
                stream.set_nodelay(true)?;
                let p = p.clone(); let acceptor = acceptor.clone();
                tokio::spawn(async move {
                    if let Ok(stream) = acceptor.accept(stream).await {
                        let verified = stream.get_ref().1.peer_certificates().and_then(|c| c.first()).and_then(|c| tls::certificate_node_id(c).ok());
                        if verified.as_deref() != Some(&p.peer) { return; }
                        let (r, w) = tokio::io::split(stream);
                        let _ = session(r, w, p).await;
                    }
                });
            },
            Some(incoming) = listeners.quic.accept(), if p.quic != 0 => {
                let p = p.clone();
                tokio::spawn(async move {
                    if let Ok(c) = incoming.await {
                        let certs = c.peer_identity().and_then(|v| v.downcast::<Vec<rustls::pki_types::CertificateDer<'static>>>().ok());
                        let verified = certs.and_then(|v| v.first().and_then(|c| tls::certificate_node_id(c).ok()));
                        if verified.as_deref() != Some(&p.peer) { return; }
                        if let Ok((w, r)) = c.accept_bi().await { let _ = session(r, w, p).await; }
                    }
                });
            },
        }
    }
    Ok(())
}
