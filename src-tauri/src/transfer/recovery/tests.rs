use super::*;
use crate::{
    config::AppPaths,
    network::{
        protocol::{Request, Response},
        transport, NetworkManager, NetworkOptions,
    },
    service::AppService,
    transfer::{
        model::{incoming_id, random_id, Manifest},
        paths, source,
    },
};
use std::{path::PathBuf, sync::Arc, time::Duration};

struct Fixture {
    root: PathBuf,
    service: Arc<TransferService>,
    id: String,
}
impl Fixture {
    async fn new() -> Self {
        let root = std::env::temp_dir().join(format!("rc-{}", &random_id()[..8]));
        let app = AppService::init(AppPaths::new(root.join("data"))).unwrap();
        app.set_setting("cache_directory", "").unwrap();
        let service = TransferService::new(app).unwrap();
        std::fs::write(root.join("a"), vec![7; 33]).unwrap();
        std::fs::write(root.join("b"), vec![8; 19]).unwrap();
        let files = vec![
            root.join("a").to_string_lossy().into(),
            root.join("b").to_string_lossy().into(),
        ];
        let (manifest, _) = source::collect(files.clone(), false).unwrap();
        let id = service
            .create_outgoing(
                "peer".into(),
                "peer".into(),
                "test".into(),
                files,
                false,
                false,
                None,
            )
            .await
            .unwrap();
        service
            .edit(&id, |t| {
                t.attach_manifest(Manifest::new(manifest.entries, 8, false));
                t.scan_complete = true;
                t.digest = t.manifest.digest();
            })
            .unwrap();
        Self { root, service, id }
    }
    fn receive(&self) {
        let inbox = self.root.join("inbox");
        std::fs::create_dir_all(&inbox).unwrap();
        let inbox = inbox.canonicalize().unwrap();
        self.service
            .update_cache_root(Some(self.root.join("cache")));
        self.service
            .edit(&self.id, |t| {
                t.direction = "receive".into();
                t.status = "paused".into();
                t.destination = Some(inbox.to_string_lossy().into());
                t.targets = t
                    .manifest
                    .entries
                    .iter()
                    .map(|e| Some(inbox.join(&e.path).to_string_lossy().into()))
                    .collect();
                t.use_cache = true;
                t.cache_files = [0, 1].into_iter().collect();
                t.relay_target_bytes = t.total_size;
            })
            .unwrap();
        self.service.save_decision(&self.id).unwrap();
    }
    fn cache_file(&self, file: usize) {
        let task = self.service.record(&self.id).unwrap();
        let data = std::fs::read(self.root.join(&task.manifest.entries[file].path)).unwrap();
        let cache = self.root.join("cache");
        let seg = paths::cache_segment(&cache, &task, file, 0);
        std::fs::create_dir_all(seg.parent().unwrap()).unwrap();
        std::fs::write(seg, &data).unwrap();
        let base = task.manifest.file_base(file);
        let count = data.len().div_ceil(task.chunk_size as usize);
        for i in base..base + count {
            let c = task.manifest.chunk(i).unwrap();
            self.service
                .mark_chunk_done(&self.id, i, c.length as u64)
                .unwrap();
        }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.service.shutdown.cancel();
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[tokio::test]
async fn cached_empty_file_is_created_before_final_manifest_check() {
    let f = Fixture::new().await;
    f.receive();
    f.service
        .edit(&f.id, |task| {
            let mut entries = task.manifest.entries.clone();
            for entry in &mut entries {
                entry.size = 0;
            }
            task.attach_manifest(Manifest::new(entries, 8, false));
            task.scan_complete = true;
            task.digest = task.manifest.digest();
            task.status = "accepted".into();
            task.relay_target_bytes = 0;
        })
        .unwrap();
    assert!(matches!(
        f.service.commit(&f.id).await.unwrap(),
        Response::Ack
    ));
    let task = f.service.record(&f.id).unwrap();
    assert_eq!(task.status, "completed");
    for target in task.targets.iter().flatten() {
        let metadata = std::fs::metadata(target).unwrap();
        assert!(metadata.is_file());
        assert_eq!(metadata.len(), 0);
    }
}

#[tokio::test]
async fn host_exit_publishes_complete_cache_without_waiting_for_incomplete_files() {
    let f = Fixture::new().await;
    f.receive();
    f.cache_file(0);
    f.service
        .edit(&f.id, |task| task.status = "offline".into())
        .unwrap();
    tokio::time::timeout(Duration::from_secs(10), f.service.drain_completed_cache())
        .await
        .unwrap()
        .unwrap();
    let task = f.service.record(&f.id).unwrap();
    assert_eq!(
        std::fs::read(task.targets[0].as_ref().unwrap()).unwrap(),
        vec![7; 33]
    );
    assert!(!std::path::Path::new(task.targets[1].as_ref().unwrap()).exists());
}

#[tokio::test]
async fn receive_progress_advances_before_cached_file_is_published() {
    let f = Fixture::new().await;
    f.receive();
    f.service.mark_chunk_done(&f.id, 0, 8).unwrap();
    let task = f.service.record(&f.id).unwrap();
    assert_eq!(task.relayed_total, 0);
    assert_eq!(task.snapshot().completed_bytes, 8);
    assert_ne!(task.snapshot().status, "completed");
    f.cache_file(0);
    f.cache_file(1);
    let task = f.service.record(&f.id).unwrap();
    assert_eq!(task.snapshot().completed_bytes, task.total_size);
    assert_ne!(task.snapshot().status, "completed");
}

#[tokio::test]
async fn invalidating_cached_file_resets_its_in_memory_relay_cursor() {
    let f = Fixture::new().await;
    f.receive();
    f.cache_file(0);
    let task = f.service.record(&f.id).unwrap();
    let target = task.targets[0].as_deref().unwrap();
    std::fs::write(target, vec![7; task.manifest.entries[0].size as usize]).unwrap();
    f.service
        .journal_transition(
            &f.id,
            0,
            task.manifest.entries[0].size,
            "published",
            Some(target),
        )
        .unwrap();
    f.service
        .set_relayed(&f.id, 0, task.manifest.entries[0].size)
        .unwrap();

    assert_eq!(f.service.relayed_at(&f.id, 0), 33);
    f.service.invalidate_file(&f.id, 0).unwrap();

    assert_eq!(f.service.relayed_at(&f.id, 0), 0);
    assert_eq!(f.service.record(&f.id).unwrap().relayed_total, 0);
}

#[tokio::test]
async fn admission_reserves_whole_files_across_tasks_and_survives_restart() {
    let f = Fixture::new().await;
    let limit = reservation_bytes(33);
    assert!(f.service.reserve_cached_file(&f.id, 0, 33, limit).unwrap());
    assert!(f.service.reserve_cached_file(&f.id, 0, 33, limit).unwrap());
    assert!(!f
        .service
        .reserve_cached_file("second-task", 0, 8, limit)
        .unwrap());
    let reopened = TransferService::new(f.service.app.clone()).unwrap();
    assert!(!reopened
        .reserve_cached_file("second-task", 0, 8, limit)
        .unwrap());
    reopened.shutdown.cancel();
    f.service
        .journal_transition(&f.id, 0, 33, "assembled", None)
        .unwrap();
    assert!(!f
        .service
        .reserve_cached_file("second-task", 0, 8, limit)
        .unwrap());
    f.service
        .journal_transition(&f.id, 0, 33, "published", None)
        .unwrap();
    assert!(f
        .service
        .reserve_cached_file("second-task", 0, 8, limit)
        .unwrap());
}

#[tokio::test]
async fn cache_admission_switches_an_unstarted_oversized_file_to_direct_write() {
    let f = Fixture::new().await;
    let task = f.service.record(&f.id).unwrap();
    let mut entries = task.manifest.entries.clone();
    entries[0].size = 64 * 1024 * 1024 * 1024;
    let manifest = Manifest::new(entries, task.chunk_size, false);
    f.service
        .edit(&f.id, |record| {
            record.attach_manifest(manifest);
            record.scan_complete = true;
        })
        .unwrap();
    f.receive();
    f.service
        .edit(&f.id, |record| record.status = "accepted".into())
        .unwrap();

    f.service.admit_cached_file(&f.id, 0).await.unwrap();

    let task = f.service.record(&f.id).unwrap();
    assert!(!task.cache_files.contains(&0));
    assert!(task.cache_files.contains(&1));
    assert!(task.use_cache);
    assert_eq!(
        f.service.journal_state(&f.id, 0).unwrap().as_deref(),
        Some("direct")
    );
}

#[tokio::test]
async fn cache_writes_remain_available_while_transfer_is_paused_or_offline() {
    let f = Fixture::new().await;
    f.receive();

    assert!(f.service.wait_for_cache_space(&f.id, 1).await.unwrap());
    f.service
        .edit(&f.id, |record| record.status = "offline".into())
        .unwrap();
    assert!(f.service.wait_for_cache_space(&f.id, 1).await.unwrap());
}

#[tokio::test]
async fn interrupted_cache_drains_and_missing_source_can_finish_without_discarding_output() {
    let f = Fixture::new().await;
    f.receive();
    f.cache_file(0);
    f.service.set_source_issue(&f.id, 1, "missing").unwrap();
    f.service.finish_partial(&f.id).await.unwrap();
    let task = f.service.record(&f.id).unwrap();
    assert_eq!(task.status, "partial_completed");
    assert_eq!(task.abandoned_files, [1].into_iter().collect());
    assert_eq!(std::fs::read(f.root.join("inbox/a")).unwrap(), vec![7; 33]);
    assert!(!f.root.join("inbox/b").exists());
    assert!(!paths::cache_task(&f.root.join("cache"), &f.service.record(&f.id).unwrap()).exists());
    f.service.finish_partial(&f.id).await.unwrap();
}

#[tokio::test]
async fn source_version_replacement_preserves_other_files_when_chunk_indices_shift() {
    let f = Fixture::new().await;
    let task = f.service.record(&f.id).unwrap();
    for (i, c) in task.manifest.chunks().iter().enumerate() {
        f.service
            .mark_chunk_done(&f.id, i, c.length as u64)
            .unwrap();
    }
    let mut replacement = task.manifest.entries[0].clone();
    replacement.size = 81;
    f.service.replace_file_entry(&f.id, 0, replacement).unwrap();
    let updated = f.service.record(&f.id).unwrap();
    let base = updated.manifest.file_base(1);
    assert_eq!(updated.completed, (base..base + 3).collect());
    assert_eq!(updated.completed_bytes, 19);
    assert_eq!(updated.wire_id, task.wire_id);
}

#[tokio::test]
async fn publishing_after_crash_uses_assembled_file_without_segments() {
    let f = Fixture::new().await;
    f.receive();
    f.cache_file(0);
    let cache = f.root.join("cache");
    let task = f.service.record(&f.id).unwrap();
    let assembled = paths::cache_file(&cache, &task, 0);
    std::fs::write(&assembled, vec![7; 33]).unwrap();
    f.service
        .journal_transition(&f.id, 0, 33, "publishing", None)
        .unwrap();
    std::fs::remove_file(paths::cache_segment(&cache, &task, 0, 0)).unwrap();
    std::fs::write(f.root.join("inbox/a"), [99; 3]).unwrap();
    let plan = f.service.relay_plan(&f.id).unwrap();
    super::super::relay::relay_file_step_shared(&f.service, &plan, &cache, &f.id, 0, 33, 0)
        .await
        .unwrap();
    assert_eq!(std::fs::read(f.root.join("inbox/a")).unwrap(), vec![7; 33]);
    assert!(!assembled.exists());
    assert_eq!(
        f.service.journal_state(&f.id, 0).unwrap().as_deref(),
        Some("published")
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn native_protocol_repairs_deleted_publication_and_ack_is_idempotent() {
    let f = Fixture::new().await;
    let receiver_root = f.root.join("receiver");
    let app = AppService::init(AppPaths::new(receiver_root.join("data"))).unwrap();
    app.set_setting("cache_directory", "").unwrap();
    let net = NetworkManager::start_with(
        app,
        NetworkOptions {
            bind: "127.0.0.1:0".parse().unwrap(),
            discovery: false,
        },
    )
    .await
    .unwrap();
    let sender_app = AppService::init(AppPaths::new(f.root.join("sender-data"))).unwrap();
    sender_app.set_setting("cache_directory", "").unwrap();
    let sender = NetworkManager::start_with(
        sender_app.clone(),
        NetworkOptions {
            bind: "127.0.0.1:0".parse().unwrap(),
            discovery: false,
        },
    )
    .await
    .unwrap();
    let peer = sender
        .add_endpoint(format!("127.0.0.1:{}", net.tcp_port), None, false)
        .await
        .unwrap();
    let endpoint = sender.peers.get(&peer.node_id).unwrap().endpoints[0].clone();
    let original = f.service.record(&f.id).unwrap();
    let wire = original.wire_id.clone();
    let incoming = incoming_id(&sender_app.identity.node_id, &wire);
    let inbox = receiver_root.join("inbox");
    std::fs::create_dir_all(&inbox).unwrap();
    let inbox = inbox.canonicalize().unwrap();
    let mut task = original.clone();
    task.id = incoming.clone();
    task.peer_id = sender_app.identity.node_id.clone();
    task.direction = "receive".into();
    task.status = "accepted".into();
    task.use_cache = true;
    task.cache_files = [0, 1].into_iter().collect();
    task.relay_target_bytes = task.total_size;
    task.destination = Some(inbox.to_string_lossy().into());
    task.targets = task
        .manifest
        .entries
        .iter()
        .map(|e| Some(inbox.join(&e.path).to_string_lossy().into()))
        .collect();
    net.transfers.insert(task).unwrap();
    net.transfers.save_decision(&incoming).unwrap();
    net.transfers
        .update_cache_root(Some(receiver_root.join("cache")));
    let mut channel = transport::connect(&sender_app, &endpoint, Some(&peer.node_id), true, false)
        .await
        .unwrap();
    transport::hello(&mut channel, &sender_app).await.unwrap();
    assert!(matches!(
        channel
            .request(&Request::Poll { id: wire.clone() })
            .await
            .unwrap(),
        Response::State { .. }
    ));
    for (index, c) in original.manifest.chunks().iter().enumerate() {
        let bytes = f.service.read_chunk_cached(&original, *c).await.unwrap();
        assert!(matches!(
            channel
                .chunk(
                    &Request::VersionedChunk {
                        id: wire.clone(),
                        index,
                        length: bytes.len(),
                        sha256: None,
                        file_version: crate::transfer::model::file_version(
                            c.file,
                            &original.manifest.entries[c.file]
                        )
                    },
                    &bytes
                )
                .await
                .unwrap(),
            Response::Ack
        ));
    }
    tokio::time::timeout(Duration::from_secs(10), async {
        while net.transfers.relayed_at(&incoming, 1) < 19 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert!(
        matches!(channel.request(&Request::RecoveryProbe{id:wire.clone(),epoch:0}).await.unwrap(),Response::RecoveryFiles{files,..} if files.is_empty())
    );
    net.transfers
        .edit(&incoming, |t| {
            t.recovery_epoch = 1;
        })
        .unwrap();
    std::fs::write(inbox.join("a"), vec![0; 33]).unwrap();
    let Response::RecoveryFiles { epoch, files } = channel
        .request(&Request::RecoveryProbe {
            id: wire.clone(),
            epoch: 0,
        })
        .await
        .unwrap()
    else {
        panic!("missing recovery response");
    };
    assert_eq!(epoch, 1);
    assert_eq!(files.len(), 2);
    assert_ne!(
        files.iter().find(|(i, _)| *i == 0).unwrap().1.as_deref(),
        Some(hash_file(&f.root.join("a")).unwrap().as_str())
    );
    assert!(matches!(
        channel
            .request(&Request::RecoveryAck {
                id: wire.clone(),
                epoch,
                repair: vec![0]
            })
            .await
            .unwrap(),
        Response::Ack
    ));
    assert!(
        matches!(channel.request(&Request::RecoveryProbe{id:wire.clone(),epoch:0}).await.unwrap(),Response::RecoveryFiles{files,..} if files.is_empty())
    );
    for (index, c) in original
        .manifest
        .chunks()
        .iter()
        .enumerate()
        .filter(|(_, c)| c.file == 0)
    {
        let bytes = f.service.read_chunk_cached(&original, *c).await.unwrap();
        assert!(matches!(
            channel
                .chunk(
                    &Request::VersionedChunk {
                        id: wire.clone(),
                        index,
                        length: bytes.len(),
                        sha256: None,
                        file_version: crate::transfer::model::file_version(
                            c.file,
                            &original.manifest.entries[c.file]
                        )
                    },
                    &bytes
                )
                .await
                .unwrap(),
            Response::Ack
        ));
    }
    tokio::time::timeout(Duration::from_secs(10), async {
        while net.transfers.relayed_at(&incoming, 0) < 33 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    std::fs::remove_file(inbox.join("a")).unwrap();
    let response = channel
        .request(&Request::Commit { id: wire.clone() })
        .await
        .unwrap();
    assert!(matches!(response,Response::Repair{ref files} if files==&vec![0]));
    assert_eq!(net.transfers.relayed_at(&incoming, 0), 0);
    let first_chunk = original.manifest.chunk(0).unwrap();
    let stale = vec![0; first_chunk.length];
    assert!(channel
        .chunk(
            &Request::VersionedChunk {
                id: wire.clone(),
                index: 0,
                length: stale.len(),
                sha256: None,
                file_version: "obsolete".into()
            },
            &stale
        )
        .await
        .is_err());
    assert!(!net.transfers.is_completed(&incoming, 0));
    for (index, c) in original
        .manifest
        .chunks()
        .iter()
        .enumerate()
        .filter(|(_, c)| c.file == 0)
    {
        let bytes = f.service.read_chunk_cached(&original, *c).await.unwrap();
        assert!(matches!(
            channel
                .chunk(
                    &Request::VersionedChunk {
                        id: wire.clone(),
                        index,
                        length: bytes.len(),
                        sha256: None,
                        file_version: crate::transfer::model::file_version(
                            c.file,
                            &original.manifest.entries[c.file]
                        )
                    },
                    &bytes
                )
                .await
                .unwrap(),
            Response::Ack
        ));
    }
    assert!(matches!(
        channel
            .request(&Request::Commit { id: wire.clone() })
            .await
            .unwrap(),
        Response::Ack
    ));
    assert!(matches!(
        channel
            .request(&Request::Commit { id: wire.clone() })
            .await
            .unwrap(),
        Response::Ack
    ));
    assert_eq!(std::fs::read(inbox.join("a")).unwrap(), vec![7; 33]);
    assert_eq!(std::fs::read(inbox.join("b")).unwrap(), vec![8; 19]);
    net.shutdown().await;
    sender.shutdown().await;
}

async fn actual_sender_source_issue(missing: bool, finish_at_sender: bool) {
    let root = std::env::temp_dir().join(format!("v-{}", &random_id()[..8]));
    let sender_app = AppService::init(AppPaths::new(root.join("s"))).unwrap();
    sender_app.set_setting("cache_directory", "").unwrap();
    let receiver_app = AppService::init(AppPaths::new(root.join("r"))).unwrap();
    receiver_app.set_setting("cache_directory", "").unwrap();
    let options = || NetworkOptions {
        bind: "127.0.0.1:0".parse().unwrap(),
        discovery: false,
    };
    let sender = NetworkManager::start_with(sender_app.clone(), options())
        .await
        .unwrap();
    let receiver = NetworkManager::start_with(receiver_app.clone(), options())
        .await
        .unwrap();
    let peer = sender
        .add_endpoint(format!("127.0.0.1:{}", receiver.tcp_port), None, false)
        .await
        .unwrap();
    let first = root.join("first.bin");
    let second = root.join("second.bin");
    std::fs::write(&first, vec![3; 33]).unwrap();
    std::fs::write(&second, vec![9; 19]).unwrap();
    let id = sender
        .transfers
        .create_outgoing(
            peer.node_id,
            peer.display_name,
            "test".into(),
            vec![
                first.to_string_lossy().into(),
                second.to_string_lossy().into(),
            ],
            false,
            false,
            None,
        )
        .await
        .unwrap();
    sender
        .transfers
        .edit(&id, |t| {
            t.scan_complete = true;
            t.digest = t.manifest.digest();
        })
        .unwrap();
    let original = sender.transfers.record(&id).unwrap();
    let wire = original.wire_id.clone();
    let incoming = incoming_id(&sender_app.identity.node_id, &wire);
    let inbox = root.join("inbox");
    std::fs::create_dir_all(&inbox).unwrap();
    let inbox = inbox.canonicalize().unwrap();
    let mut received = original.clone();
    received.id = incoming.clone();
    received.direction = "receive".into();
    received.peer_id = sender_app.identity.node_id.clone();
    received.status = "accepted".into();
    received.use_cache = true;
    received.cache_files = [0, 1].into_iter().collect();
    received.relay_target_bytes = received.total_size;
    received.destination = Some(inbox.to_string_lossy().into());
    received.targets = received
        .manifest
        .entries
        .iter()
        .map(|e| Some(inbox.join(&e.path).to_string_lossy().into()))
        .collect();
    receiver.transfers.insert(received).unwrap();
    receiver.transfers.save_decision(&incoming).unwrap();
    receiver
        .transfers
        .update_cache_root(Some(root.join("cache")));
    if missing {
        std::fs::remove_file(&first).unwrap();
    } else {
        std::fs::write(&first, vec![5; 81]).unwrap();
    }
    sender
        .transfers
        .start_task(sender.peers.clone(), id.clone());
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let task = sender.transfers.record(&id).unwrap();
            if task.status == "paused"
                && task
                    .source_issue
                    .as_ref()
                    .is_some_and(|i| i.kind == if missing { "missing" } else { "changed" })
                && sender
                    .transfers
                    .runtime(&id)
                    .state_sessions
                    .load(std::sync::atomic::Ordering::SeqCst)
                    > 0
                && receiver
                    .transfers
                    .record(&incoming)
                    .unwrap()
                    .source_issue
                    .is_some()
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert!(!inbox.join("first.bin").exists());
    if missing {
        let finish_node = if finish_at_sender { &sender } else { &receiver };
        let finish_id = if finish_at_sender {
            id.clone()
        } else {
            incoming.clone()
        };
        finish_node
            .action(finish_id, "finish".into())
            .await
            .unwrap();
    } else {
        sender.action(id.clone(), "resume".into()).await.unwrap();
    }
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let task = sender.transfers.record(&id).unwrap();
            if task.status
                == if missing {
                    "partial_completed"
                } else {
                    "completed"
                }
                && receiver.transfers.record(&incoming).unwrap().status
                    == if missing {
                        "partial_completed"
                    } else {
                        "completed"
                    }
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap_or_else(|_| {
        let sender_task = sender.transfers.record(&id).unwrap();
        let receiver_task = receiver.transfers.record(&incoming).unwrap();
        panic!(
            "Timed out waiting for partial completion: sender_status={} sender_error={:?} sender_remote_revision={} receiver_status={} receiver_revision={} sender_state_sessions={} sender_state_sent_revision={} receiver_state_sessions={} receiver_state_sent_revision={}",
            sender_task.status,
            sender_task.error,
            sender_task.remote_state_revision,
            receiver_task.status,
            receiver_task.state_revision,
            sender.transfers.runtime(&id).state_sessions.load(std::sync::atomic::Ordering::SeqCst),
            sender.transfers.runtime(&id).state_sent_revision.load(std::sync::atomic::Ordering::SeqCst),
            receiver.transfers.runtime(&incoming).state_sessions.load(std::sync::atomic::Ordering::SeqCst),
            receiver.transfers.runtime(&incoming).state_sent_revision.load(std::sync::atomic::Ordering::SeqCst),
        )
    });
    assert_eq!(sender.transfers.record(&id).unwrap().wire_id, wire);
    if missing {
        assert_eq!(
            receiver.transfers.record(&incoming).unwrap().status,
            "partial_completed"
        );
        assert!(receiver
            .transfers
            .record(&incoming)
            .unwrap()
            .abandoned_files
            .contains(&0));
        assert!(!inbox.join("first.bin").exists());
    } else {
        assert_eq!(std::fs::read(inbox.join("first.bin")).unwrap(), vec![5; 81]);
        assert_eq!(
            std::fs::read(inbox.join("second.bin")).unwrap(),
            vec![9; 19]
        );
    }
    sender.shutdown().await;
    receiver.shutdown().await;
    let _ = std::fs::remove_dir_all(root);
}

#[cfg(windows)]
#[tokio::test]
async fn active_source_handle_denies_write_but_allows_reads() {
    let f = Fixture::new().await;
    let task = f.service.record(&f.id).unwrap();
    let handle = f
        .service
        .send_handle(&f.id, 0, &task.sources, &task.manifest.entries[0])
        .await
        .unwrap();
    assert!(std::fs::File::open(f.root.join("a")).is_ok());
    assert!(std::fs::OpenOptions::new()
        .write(true)
        .open(f.root.join("a"))
        .is_err());
    assert!(std::fs::remove_file(f.root.join("a")).is_err());
    f.service.release_send_files(&f.id);
    drop(handle);
    assert!(std::fs::OpenOptions::new()
        .write(true)
        .open(f.root.join("a"))
        .is_ok());
}

#[tokio::test]
async fn optional_cache_hash_rejects_same_size_segment_corruption() {
    use sha2::{Digest, Sha256};
    let f = Fixture::new().await;
    f.receive();
    f.cache_file(0);
    f.service
        .edit(&f.id, |t| {
            t.verify_hash = true;
            t.manifest = Manifest::new(t.manifest.entries.clone(), t.chunk_size, true);
        })
        .unwrap();
    let task = f.service.record(&f.id).unwrap();
    for (index, c) in task
        .manifest
        .chunks()
        .iter()
        .enumerate()
        .filter(|(_, c)| c.file == 0)
    {
        let bytes = vec![7; c.length];
        let digest = format!("{:x}", Sha256::digest(&bytes));
        f.service
            .app
            .db
            .with_conn(|db| {
                db.execute(
                    "INSERT INTO transfer_chunk_hash(task_id,chunk_index,digest) VALUES(?1,?2,?3)",
                    params![f.id, index as i64, digest],
                )?;
                Ok(())
            })
            .unwrap();
    }
    let cache = f.root.join("cache");
    std::fs::write(paths::cache_segment(&cache, &task, 0, 0), vec![0; 33]).unwrap();
    let plan = f.service.relay_plan(&f.id).unwrap();
    assert!(super::super::relay::relay_file_step_shared(
        &f.service, &plan, &cache, &f.id, 0, 33, 0
    )
    .await
    .unwrap()
    .is_none());
    let task = f.service.record(&f.id).unwrap();
    assert!(!task.cache_files.contains(&0));
    assert!(task.repair_files.contains(&0));
    assert_eq!(
        f.service.journal_state(&f.id, 0).unwrap().as_deref(),
        Some("direct")
    );
    assert!(!f.root.join("inbox/a").exists());
    let task_cache_dirs = paths::cache_task_dirs(&cache, &task);
    assert!(task_cache_dirs.iter().any(|dir| dir.exists()));
    f.service.cleanup_cache_dirs(&task);
    assert!(task_cache_dirs.iter().all(|dir| !dir.exists()));
}

#[tokio::test]
async fn missing_cached_segment_requests_direct_file_retransmission() {
    let f = Fixture::new().await;
    f.receive();
    f.cache_file(0);
    let task = f.service.record(&f.id).unwrap();
    let cache = f.root.join("cache");
    std::fs::remove_file(paths::cache_segment(&cache, &task, 0, 0)).unwrap();
    let plan = f.service.relay_plan(&f.id).unwrap();

    assert!(super::super::relay::relay_file_step_shared(
        &f.service, &plan, &cache, &f.id, 0, 33, 0
    )
    .await
    .unwrap()
    .is_none());

    let task = f.service.record(&f.id).unwrap();
    assert!(!task.cache_files.contains(&0));
    assert!(task.repair_files.contains(&0));
    assert_eq!(
        f.service.journal_state(&f.id, 0).unwrap().as_deref(),
        Some("direct")
    );
    assert!(!f.root.join("inbox/a").exists());
}

#[tokio::test]
async fn abnormal_probe_hashes_last_three_publications_plus_inflight_copy() {
    let f = Fixture::new().await;
    f.receive();
    let mut entries = f.service.record(&f.id).unwrap().manifest.entries;
    for index in 2..5 {
        let mut entry = entries[0].clone();
        entry.path = format!("file{index}");
        entries.push(entry);
    }
    let inbox = f.root.join("inbox").canonicalize().unwrap();
    f.service
        .edit(&f.id, |t| {
            t.attach_manifest(Manifest::new(entries, 8, false));
            t.targets = t
                .manifest
                .entries
                .iter()
                .map(|e| Some(inbox.join(&e.path).to_string_lossy().into()))
                .collect();
            t.recovery_epoch = 1;
        })
        .unwrap();
    let task = f.service.record(&f.id).unwrap();
    for file in [4, 0, 3, 1, 2] {
        let target = task.targets[file].as_ref().unwrap();
        std::fs::write(
            target,
            vec![file as u8; task.manifest.entries[file].size as usize],
        )
        .unwrap();
        f.service
            .journal_transition(
                &f.id,
                file,
                task.manifest.entries[file].size,
                "published",
                Some(target),
            )
            .unwrap();
    }
    f.service
        .journal_transition(&f.id, 4, 33, "publishing", task.targets[4].as_deref())
        .unwrap();
    let Response::RecoveryFiles { files, epoch } = f.service.recovery_probe(&f.id).await.unwrap()
    else {
        panic!("probe response missing");
    };
    assert_eq!(epoch, 1);
    assert_eq!(
        files.iter().map(|(i, _)| *i).collect::<Vec<_>>(),
        vec![2, 1, 3, 4]
    );
    assert!(files.iter().all(|(_, hash)| hash.is_some()));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn actual_sender_pauses_on_source_change_and_resume_replaces_only_that_version() {
    actual_sender_source_issue(false, false).await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn receiver_can_finish_missing_source_over_the_existing_duplex_session() {
    actual_sender_source_issue(true, false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sender_can_finish_missing_source_over_the_existing_duplex_session() {
    actual_sender_source_issue(true, true).await;
}

#[tokio::test]
async fn startup_reclaims_payload_left_after_durable_publication() {
    let f = Fixture::new().await;
    f.receive();
    f.cache_file(0);
    let cache = f.root.join("cache");
    let task = f.service.record(&f.id).unwrap();
    let assembled = paths::cache_file(&cache, &task, 0);
    std::fs::write(&assembled, vec![7; 33]).unwrap();
    std::fs::write(f.root.join("inbox/a"), vec![7; 33]).unwrap();
    f.service
        .app
        .set_setting("cache_directory", cache.to_string_lossy().as_ref())
        .unwrap();
    f.service
        .journal_transition(&f.id, 0, 33, "published", None)
        .unwrap();
    f.service.shutdown.cancel();
    let reopened = TransferService::new(f.service.app.clone()).unwrap();
    assert!(!assembled.exists());
    assert!(!paths::cache_segment(&cache, &task, 0, 0).exists());
    assert_eq!(std::fs::read(f.root.join("inbox/a")).unwrap(), vec![7; 33]);
    reopened.shutdown.cancel();
}

#[tokio::test]
async fn cache_root_refresh_pins_active_tasks_and_updates_future_tasks() {
    let f = Fixture::new().await;
    f.receive();

    let old_root = f.root.join("cache");
    let new_root = f.root.join("cache-next");
    std::fs::create_dir_all(&new_root).unwrap();
    f.service
        .app
        .set_setting("cache_directory", new_root.to_string_lossy().as_ref())
        .unwrap();
    f.service.refresh_cache_root_from_settings();

    assert_eq!(f.service.current_cache_root(), Some(new_root.clone()));
    assert_eq!(f.service.cache_root_for_task(&f.id), Some(old_root));
    assert_eq!(f.service.cache_root_for_task("future-task"), Some(new_root));
}
