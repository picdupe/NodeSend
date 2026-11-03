use super::{
    peers::PeerStore,
    protocol::{self, Response},
    transport::Channel,
};
use crate::{
    error::{AppError, AppResult},
    transfer::{TaskRuntime, TransferService},
};
use std::sync::{atomic::Ordering, Arc};

struct SessionGuard(Arc<TaskRuntime>);
impl Drop for SessionGuard {
    fn drop(&mut self) {
        self.0.state_sessions.fetch_sub(1, Ordering::SeqCst);
        self.0.state_updates.send_replace(());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        config::AppPaths,
        network::{NetworkManager, NetworkOptions},
        service::AppService,
        transfer::model::{random_id, TaskRecord},
    };
    use std::{path::PathBuf, time::Duration};

    #[test]
    fn state_disconnect_does_not_override_an_explicit_pause() {
        assert!(!super::should_mark_interrupted("paused"));
        assert!(!super::should_mark_interrupted("remote_paused"));
        assert!(super::should_mark_interrupted("transferring"));
    }

    struct Node {
        net: Arc<NetworkManager>,
        root: PathBuf,
    }
    impl Node {
        async fn new() -> Self {
            let root = std::env::temp_dir().join(format!("nodesend-state-{}", random_id()));
            let app = AppService::init(AppPaths::new(root.join("data"))).unwrap();
            let net = NetworkManager::start_with(
                app,
                NetworkOptions {
                    bind: "127.0.0.1:0".parse().unwrap(),
                    discovery: false,
                },
            )
            .await
            .unwrap();
            Self { net, root }
        }
        async fn stop(self) {
            self.net.shutdown().await;
            let _ = std::fs::remove_dir_all(self.root);
        }
    }
    async fn until(
        net: &NetworkManager,
        id: &str,
        predicate: impl Fn(&TaskRecord) -> bool,
    ) -> TaskRecord {
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let task = net.transfers.record(id).unwrap();
                if predicate(&task) {
                    return task;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap_or_else(|_| {
            panic!(
                "Unexpected task status after state transition: {:?}",
                net.transfers.record(id).unwrap().status
            )
        })
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn either_peer_controls_the_same_session_after_a_long_pause() {
        let sender = Node::new().await;
        let receiver = Node::new().await;
        let peer = sender
            .net
            .add_endpoint(format!("127.0.0.1:{}", receiver.net.tcp_port), None, false)
            .await
            .unwrap();
        let source = sender.root.join("session.bin");
        std::fs::write(&source, b"state-session-fixture").unwrap();
        let id = sender
            .net
            .transfers
            .create_outgoing(
                peer.node_id.clone(),
                peer.display_name,
                "test".into(),
                vec![source.to_string_lossy().into()],
                false,
                false,
                None,
            )
            .await
            .unwrap();
        let task = sender.net.transfers.record(&id).unwrap();
        let incoming =
            crate::transfer::model::incoming_id(&sender.net.app.identity.node_id, &task.wire_id);
        let mut received = task.clone();
        received.id = incoming.clone();
        received.peer_id = sender.net.app.identity.node_id.clone();
        received.direction = "receive".into();
        received.status = "accepted".into();
        received.destination = Some(receiver.root.join("inbox").to_string_lossy().into());
        receiver.net.transfers.insert(received).unwrap();
        let runtime = sender.net.transfers.runtime(&id);
        runtime.active.store(true, Ordering::SeqCst);
        let endpoint = sender.net.peers.get(&peer.node_id).unwrap().endpoints[0].clone();
        let mut channel = crate::network::transport::connect(
            &sender.net.app,
            &endpoint,
            Some(&peer.node_id),
            true,
            false,
        )
        .await
        .unwrap();
        crate::network::transport::hello(&mut channel, &sender.net.app)
            .await
            .unwrap();
        assert!(matches!(
            channel
                .request(&protocol::Request::WatchTask {
                    id: task.wire_id,
                    revision: 0,
                })
                .await
                .unwrap(),
            Response::Ack
        ));
        let _session = spawn(
            channel,
            sender.net.transfers.clone(),
            sender.net.peers.clone(),
            id.clone(),
            0,
        );
        sender.net.action(id.clone(), "pause".into()).await.unwrap();
        until(&receiver.net, &incoming, |t| t.status == "remote_paused").await;
        receiver
            .net
            .action(incoming.clone(), "resume".into())
            .await
            .unwrap();
        until(&sender.net, &id, |t| t.status == "transferring").await;
        receiver
            .net
            .action(incoming.clone(), "pause".into())
            .await
            .unwrap();
        until(&sender.net, &id, |t| t.status == "remote_paused").await;
        tokio::time::sleep(Duration::from_secs(21)).await;
        assert_eq!(runtime.state_sessions.load(Ordering::SeqCst), 1);
        sender
            .net
            .action(id.clone(), "resume".into())
            .await
            .unwrap();
        until(&receiver.net, &incoming, |t| t.status == "accepted").await;
        assert_eq!(runtime.state_sessions.load(Ordering::SeqCst), 1);
        let latest = sender.net.transfers.record(&id).unwrap();
        sender
            .net
            .transfers
            .apply_remote_state(
                &peer.node_id,
                &latest.wire_id,
                latest.remote_state_revision.saturating_sub(1),
                "paused",
            )
            .await
            .unwrap();
        assert_eq!(
            sender.net.transfers.record(&id).unwrap().status,
            "transferring"
        );
        sender.stop().await;
        receiver.stop().await;
    }
}

pub(crate) struct StateSession(Option<tokio::task::JoinHandle<()>>);
impl StateSession {
    pub(crate) fn detach(mut self) {
        self.0.take();
    }
}
impl Drop for StateSession {
    fn drop(&mut self) {
        if let Some(handle) = self.0.take() {
            handle.abort();
        }
    }
}

pub(crate) fn spawn(
    mut channel: Channel,
    transfers: Arc<TransferService>,
    peers: Arc<PeerStore>,
    id: String,
    revision: u64,
) -> StateSession {
    let runtime = transfers.runtime(&id);
    runtime.state_sessions.fetch_add(1, Ordering::SeqCst);
    let guard = SessionGuard(runtime);
    StateSession(Some(tokio::spawn(async move {
        let _guard = guard;
        if let Err(error) = exchange(&mut channel, transfers.clone(), peers, &id, revision).await {
            disconnected(&transfers, &id, &error);
        }
    })))
}

pub(crate) async fn serve(
    channel: &mut Channel,
    transfers: Arc<TransferService>,
    peers: Arc<PeerStore>,
    id: &str,
    revision: u64,
) -> AppResult<()> {
    let runtime = transfers.runtime(id);
    runtime.state_sessions.fetch_add(1, Ordering::SeqCst);
    let _guard = SessionGuard(runtime);
    let result = exchange(channel, transfers.clone(), peers, id, revision).await;
    if let Err(error) = &result {
        disconnected(&transfers, id, error);
    }
    result
}

fn disconnected(transfers: &TransferService, id: &str, error: &AppError) {
    if transfers.shutdown.is_cancelled()
        || transfers.runtime(id).state_sessions.load(Ordering::SeqCst) > 1
    {
        return;
    }
    let mut interrupted = false;
    let _ = transfers.edit(id, |task| {
        if task.source_issue.is_none() && should_mark_interrupted(&task.status) {
            task.recovery_epoch = task.recovery_epoch.saturating_add(1);
            task.status = "interrupted".into();
            task.error = None;
            task.event("Connection interrupted");
            interrupted = true;
        }
    });
    if interrupted {
        tracing::warn!(task = %id, %error, "Task state connection was interrupted");
    }
}

fn should_mark_interrupted(status: &str) -> bool {
    [
        "connecting",
        "queued",
        "waiting",
        "accepted",
        "transferring",
    ]
    .contains(&status)
}

async fn exchange(
    channel: &mut Channel,
    transfers: Arc<TransferService>,
    peers: Arc<PeerStore>,
    id: &str,
    mut revision: u64,
) -> AppResult<()> {
    let runtime = transfers.runtime(id);
    let mut events = runtime.control_events.subscribe();
    let task = transfers.record(id)?;
    let peer = task.peer_id.clone();
    let wire_id = task.wire_id.clone();
    let receive = async {
        loop {
            let response: Response =
                protocol::read_frame_with_timeout(&mut *channel.reader, protocol::IO_TIMEOUT)
                    .await?;
            match response {
                Response::StateHeartbeat => {}
                Response::TaskEvent { revision, status } => {
                    transfers
                        .apply_remote_state(&peer, &wire_id, revision, &status)
                        .await?;
                    if status == "resumed"
                        && transfers.record(id)?.direction == "send"
                        && !transfers.is_active(id)
                    {
                        transfers.start_task(peers.clone(), id.to_string());
                    }
                }
                _ => return Err(AppError::BadRequest("Unsupported task action".into())),
            }
        }
        #[allow(unreachable_code)]
        Ok::<(), AppError>(())
    };
    let send = async {
        if task.state_revision > revision {
            if let Some(status) = TransferService::control_status(&task) {
                protocol::write_frame(
                    &mut *channel.writer,
                    &Response::TaskEvent {
                        revision: task.state_revision,
                        status: status.into(),
                    },
                )
                .await?;
                revision = task.state_revision;
                runtime
                    .state_sent_revision
                    .store(revision, Ordering::SeqCst);
                runtime.state_updates.send_replace(());
            }
        }
        let mut heartbeat = tokio::time::interval(crate::tuning::QUIC_KEEPALIVE);
        loop {
            tokio::select! {
                _ = heartbeat.tick() => protocol::write_frame(
                    &mut *channel.writer, &Response::StateHeartbeat,
                ).await?,
                changed = events.changed() => {
                    changed.map_err(|_| AppError::Other("Task state event stream closed".into()))?;
                    let event = events.borrow_and_update().clone();
                    if let Some((next, status)) = event {
                        if next > revision {
                            protocol::write_frame(&mut *channel.writer, &Response::TaskEvent {
                                revision: next, status,
                            }).await?;
                            revision = next;
                            runtime.state_sent_revision.store(revision,Ordering::SeqCst);
                            runtime.state_updates.send_replace(());
                        }
                    }
                }
            }
        }
        #[allow(unreachable_code)]
        Ok::<(), AppError>(())
    };
    tokio::select! {
        _ = transfers.shutdown.cancelled() => Ok(()),
        result = receive => result,
        result = send => result,
    }
}
