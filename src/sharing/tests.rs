use super::{
    location::{MAX_FIX_AGE_MS, RETENTION_MS, Sample},
    manager::{Manager, Mode, now_ms},
    peers::{Friends, Permission},
    protocol::{self, Message},
    transport,
};
use crate::{geo::Point, model::Fix};
use iroh::{Endpoint, SecretKey, endpoint::presets};
use std::time::Duration;

async fn endpoint() -> Endpoint {
    Endpoint::builder(presets::Minimal)
        .alpns(vec![protocol::ALPN.to_vec()])
        .bind()
        .await
        .unwrap()
}

fn sample(now: i64) -> Sample {
    Sample {
        fix: Fix {
            point: Point {
                lat: 40.0,
                lon: -111.0,
            },
            timestamp_ms: now as u64,
            accuracy_m: 5.0,
        },
        speed_m_s: Some(10.0),
        heading_degrees: Some(90.0),
    }
}

fn manager(dir: &std::path::Path, key: &SecretKey, peer: iroh::EndpointId) -> Manager {
    let friends = Friends::open(dir, key.public()).unwrap();
    friends.add_confirmed(peer, "Friend".into()).unwrap();
    Manager::new(friends, key.clone()).unwrap()
}

#[tokio::test]
async fn sender_gates_every_packet_and_sequences_survive_restart() {
    let a = endpoint().await;
    let b = endpoint().await;
    let da = tempfile::tempdir().unwrap();
    let db = tempfile::tempdir().unwrap();
    let ma = manager(da.path(), a.secret_key(), b.id());
    let mb = manager(db.path(), b.secret_key(), a.id());
    let (client, server) = tokio::join!(a.connect(b.addr(), protocol::ALPN), async {
        b.accept().await.unwrap().await
    });
    let client = client.unwrap();
    let server = server.unwrap();
    let now = now_ms();
    ma.update_fix(sample(now), true, now).unwrap();
    assert!(!ma.send_latest(&client, now, true).unwrap()); // All sharing defaults Off.
    ma.set_mode(Mode::Always).unwrap();
    assert!(!ma.send_latest(&client, now, true).unwrap()); // Friend permission remains Off.
    ma.friends()
        .set_permission(b.id(), Permission::TripsOnly)
        .unwrap();
    assert!(ma.send_latest(&client, now, true).unwrap());
    let first = protocol::decode(&server.read_datagram().await.unwrap(), now).unwrap();
    let Message::LocationUpdate(first_location) = first.clone() else {
        panic!("expected location")
    };
    assert!(mb.ingest(a.id(), &first, now).unwrap());
    assert!(!mb.ingest(a.id(), &first, now).unwrap());
    assert!(!mb.friends().may_share(a.id(), true).unwrap()); // Receive does not grant send.
    mb.ingest(a.id(), &Message::PermissionUpdate { allowed: true }, now)
        .unwrap();
    assert!(!mb.friends().may_share(a.id(), true).unwrap());
    ma.set_recording(false).unwrap();
    assert!(!ma.send_latest(&client, now + 30_000, true).unwrap());
    ma.set_recording(true).unwrap();
    ma.friends()
        .set_permission(b.id(), Permission::Off)
        .unwrap();
    assert!(!ma.send_latest(&client, now + 30_000, true).unwrap());
    ma.friends()
        .set_permission(b.id(), Permission::Always)
        .unwrap();
    ma.set_mode(Mode::Off).unwrap();
    assert!(!ma.send_latest(&client, now + 30_000, true).unwrap());

    let restarted_sender = Manager::new(
        Friends::open(da.path(), a.id()).unwrap(),
        a.secret_key().clone(),
    )
    .unwrap();
    restarted_sender.set_mode(Mode::Always).unwrap();
    restarted_sender
        .update_fix(sample(now + 1), false, now + 1)
        .unwrap();
    assert!(
        restarted_sender
            .send_latest(&client, now + 1, true)
            .unwrap()
    );
    let second = protocol::decode(&server.read_datagram().await.unwrap(), now + 1).unwrap();
    let Message::LocationUpdate(second_location) = second.clone() else {
        panic!("expected location")
    };
    assert!(second_location.sequence > first_location.sequence);
    let restarted_receiver = Manager::new(
        Friends::open(db.path(), b.id()).unwrap(),
        b.secret_key().clone(),
    )
    .unwrap();
    assert!(!restarted_receiver.ingest(a.id(), &first, now + 1).unwrap());
    assert!(restarted_receiver.ingest(a.id(), &second, now + 1).unwrap());
    restarted_sender.friends().remove(b.id()).unwrap();
    assert!(
        !restarted_sender
            .send_latest(&client, now + 60_000, true)
            .unwrap()
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(50), server.read_datagram())
            .await
            .is_err()
    );
    let disk = std::fs::read_to_string(da.path().join("sharing/location-state.json")).unwrap();
    assert!(!disk.contains("latitude") && !disk.contains("longitude"));
    a.close().await;
    b.close().await;
}

#[test]
fn replay_validation_retention_and_fail_closed_storage() {
    let dir = tempfile::tempdir().unwrap();
    let key = SecretKey::generate();
    let peer = SecretKey::generate().public();
    let manager = manager(dir.path(), &key, peer);
    let now = now_ms();
    let update = sample(now).to_update(4, now).unwrap();
    let message = Message::LocationUpdate(update);
    assert!(
        manager
            .ingest(SecretKey::generate().public(), &message, now)
            .is_err()
    );
    assert!(manager.ingest(peer, &message, now).unwrap());
    let mut old = update;
    old.sequence = 3;
    assert!(
        !manager
            .ingest(peer, &Message::LocationUpdate(old), now)
            .unwrap()
    );
    old.sequence = 5;
    old.timestamp_ms -= 1;
    // Sequence wins across both direct and forwarded updates.
    assert!(
        manager
            .ingest(peer, &Message::LocationUpdate(old), now)
            .unwrap()
    );
    old.timestamp_ms = now + protocol::FUTURE_SKEW_MS + 1;
    assert!(
        manager
            .ingest(peer, &Message::LocationUpdate(old), now)
            .is_err()
    );
    assert!(manager.snapshots(now + MAX_FIX_AGE_MS + 1).unwrap()[0].stale);
    assert!(
        manager.snapshots(now + RETENTION_MS + 1).unwrap()[0]
            .latest
            .is_none()
    );
    manager.friends().block(peer).unwrap();
    assert!(manager.ingest(peer, &message, now).is_err());
    assert!(manager.snapshots(now).unwrap().is_empty());
    let path = dir.path().join("sharing/location-state.json");
    std::fs::write(&path, b"corrupt").unwrap();
    assert!(manager.set_mode(Mode::Always).is_err());
    assert!(Manager::new(manager.friends().clone(), key.clone()).is_err());
    assert_eq!(std::fs::read(path).unwrap(), b"corrupt");
}

#[tokio::test]
async fn authenticated_sessions_exchange_locations_and_survive_reconnect() {
    let a = endpoint().await;
    let b = endpoint().await;
    let da = tempfile::tempdir().unwrap();
    let db = tempfile::tempdir().unwrap();
    let ma = manager(da.path(), a.secret_key(), b.id());
    let mb = manager(db.path(), b.secret_key(), a.id());
    ma.friends()
        .set_permission(b.id(), Permission::Always)
        .unwrap();
    ma.set_mode(Mode::Always).unwrap();
    ma.update_fix(sample(now_ms()), false, now_ms()).unwrap();
    let serving = transport::server(&a, ma.clone());
    let scenario = async {
        for round in 0..2 {
            let connection = b.connect(a.addr(), protocol::ALPN).await.unwrap();
            let session = transport::session(&b, connection.clone(), mb.clone(), true);
            let wait = async {
                loop {
                    if mb
                        .snapshots(now_ms())
                        .unwrap()
                        .iter()
                        .any(|snapshot| snapshot.latest.is_some() && snapshot.connected)
                    {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
                if round == 0 {
                    connection.close(0u32.into(), b"test reconnect");
                }
            };
            tokio::select! { result = session => panic!("session ended early: {}", result.is_ok()), _ = wait => {} }
        }
    };
    tokio::time::timeout(Duration::from_secs(5), async {
        tokio::select! { result = serving => panic!("server ended: {}", result.is_ok()), _ = scenario => {} }
    }).await.unwrap();
    assert!(mb.snapshots(now_ms()).unwrap()[0].latest.is_some());
    a.close().await;
    b.close().await;
}

#[tokio::test]
async fn unknown_peers_cannot_request_or_receive_locations() {
    let a = endpoint().await;
    let stranger = endpoint().await;
    let dir = tempfile::tempdir().unwrap();
    let manager = Manager::new(
        Friends::open(dir.path(), a.id()).unwrap(),
        a.secret_key().clone(),
    )
    .unwrap();
    let serving = transport::server(&a, manager.clone());
    let scenario = async {
        let connection = stranger.connect(a.addr(), protocol::ALPN).await.unwrap();
        assert!(
            tokio::time::timeout(Duration::from_secs(2), connection.closed())
                .await
                .is_ok()
        );
    };
    tokio::time::timeout(Duration::from_secs(3), async {
        tokio::select! { result = serving => panic!("server ended: {}", result.is_ok()), _ = scenario => {} }
    }).await.unwrap();
    assert!(manager.snapshots(now_ms()).unwrap().is_empty());
    a.close().await;
    stranger.close().await;
}

#[test]
fn shared_trips_replay_as_friend_journal_oldest_first() {
    let fix = |t: u64, lat: f64| serde_json::json!({"lat": lat, "lon": -111.0, "timestamp_ms": t, "accuracy_m": 4.0});
    let walk = |start: u64, name: &str| serde_json::json!({"name": name, "fixes": (0..10).map(|i| fix(start + i * 2000, 40.0 + i as f64 * 1e-4)).collect::<Vec<_>>()});
    // Newest first, as friends send them, plus one empty trip that is skipped.
    let mut later = walk(1_000_000, "Later");
    later["category"] = "Hike".into();
    let mut earlier = walk(1_000, "Earlier");
    earlier["category"] = "Hike".into();
    let reply = serde_json::json!({"trips": [later, walk(500_000, ""), earlier, {"fixes": []}]});
    let journal = super::transport::history_journal(&serde_json::to_vec(&reply).unwrap()).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("friend.jsonl");
    std::fs::write(&path, journal).unwrap();
    let store = crate::store::Store::open(&path).unwrap();
    let names: Vec<_> = store.state.trips.iter().map(|t| t.name.as_str()).collect();
    assert_eq!(names, ["Earlier", "", "Later"]);
    // Friend's category names carried over, declared once.
    assert_eq!(store.state.categories.len(), 1);
    let categories: Vec<_> = store.state.trips.iter().map(|t| t.category_id).collect();
    assert_eq!(categories, [Some(1), None, Some(1)]);
    assert!(
        store
            .state
            .trips
            .iter()
            .all(|t| t.status == crate::model::Status::Finished && t.distance_m > 50.0)
    );
}

#[tokio::test]
async fn v1_peer_rejected_with_update_required() {
    let old = Endpoint::builder(presets::Minimal)
        .alpns(vec![b"roamlog/location/1".to_vec()])
        .bind()
        .await
        .unwrap();
    let new = endpoint().await;
    let (result, _) = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(
            super::connect_alpn(&new, old.addr(), protocol::ALPN),
            async {
                let _ = old.accept().await.unwrap().await;
            }
        )
    })
    .await
    .expect("ALPN mismatch must fail without reconnect delay");
    let error = result.unwrap_err();
    assert!(
        error
            .downcast_ref::<std::io::Error>()
            .is_some_and(|e| e.kind() == std::io::ErrorKind::Unsupported),
        "{error:?}"
    );
    old.close().await;
    new.close().await;
}

#[tokio::test]
async fn offline_mailbox_relay_survives_restart_and_withdrawal_wins() {
    use super::{
        groups::Policy,
        protocol::{GroupFix, GroupPayload, SignedGroupFix},
    };
    let a = endpoint().await;
    let b = endpoint().await;
    let c = endpoint().await;
    let dirs: Vec<_> = (0..3).map(|_| tempfile::tempdir().unwrap()).collect();
    let endpoints = [&a, &b, &c];
    let stores: Vec<_> = (0..3)
        .map(|i| Friends::open(dirs[i].path(), endpoints[i].id()).unwrap())
        .collect();
    let id = stores[0]
        .create_group(
            a.secret_key(),
            "Family",
            "A",
            Policy::Managed,
            Permission::Always,
        )
        .unwrap();
    for i in 1..3 {
        let admission = stores[0]
            .admit_member(a.secret_key(), &id, endpoints[i].id(), &format!("M{i}"))
            .unwrap();
        stores[i]
            .join_group(&admission, &format!("M{i}"), Permission::Always)
            .unwrap();
    }
    let logs = stores[0].sync_for_locked(b.id()).unwrap();
    stores[1].apply_sync_locked(a.id(), &logs).unwrap();
    let ma = Manager::new(stores[0].clone(), a.secret_key().clone()).unwrap();
    let mb = Manager::new(stores[1].clone(), b.secret_key().clone()).unwrap();
    let mc = Manager::new(stores[2].clone(), c.secret_key().clone()).unwrap();
    for m in [&ma, &mb, &mc] {
        m.set_mode(Mode::Always).unwrap();
    }
    let now = now_ms() + 100;
    ma.update_fix(sample(now), false, now).unwrap();
    let (ab, ba) = tokio::join!(a.connect(b.addr(), protocol::ALPN), async {
        b.accept().await.unwrap().await
    });
    let (ab, ba) = (ab.unwrap(), ba.unwrap());
    assert!(ma.send_latest(&ab, now, true).unwrap());
    let message = protocol::decode(&ba.read_datagram().await.unwrap(), now).unwrap();
    mb.ingest(a.id(), &message, now).unwrap();
    let Message::GroupFix(signed) = message else {
        panic!("expected signed group fix")
    };
    assert!(
        tokio::time::timeout(Duration::from_millis(100), ba.read_datagram())
            .await
            .is_err()
    );
    ma.clear_fix().unwrap();
    assert!(!ma.send_latest(&ab, now + 1, false).unwrap());
    assert!(
        tokio::time::timeout(Duration::from_millis(100), ba.read_datagram())
            .await
            .is_err()
    );
    a.close().await; // A and C never connect, even indirectly through a simultaneous session.
    drop(mb);
    let mb = Manager::new(stores[1].clone(), b.secret_key().clone()).unwrap();
    let (bc, cb) = tokio::join!(b.connect(c.addr(), protocol::ALPN), async {
        c.accept().await.unwrap().await
    });
    let (bc, cb) = (bc.unwrap(), cb.unwrap());
    let later = now + 3_600_000; // Beyond direct-fix maximum age, within mailbox retention.
    assert!(!mb.send_latest(&bc, later, true).unwrap());
    let message = protocol::decode(&cb.read_datagram().await.unwrap(), later).unwrap();
    assert!(matches!(&message, Message::GroupFix(f) if f == &signed));
    assert!(mc.ingest(b.id(), &message, later).unwrap());
    let snapshot = mc
        .snapshots(later)
        .unwrap()
        .into_iter()
        .find(|s| s.peer_id == a.id())
        .unwrap();
    assert_eq!(snapshot.via, Some(b.id()));
    assert_eq!(snapshot.age_ms, Some(3_600_000));
    assert!(snapshot.stale);
    assert!(!mc.ingest(b.id(), &message, later).unwrap());
    let mut forged = signed.clone();
    forged.fix.sequence += 1;
    assert!(
        mc.ingest(b.id(), &Message::GroupFix(forged), later)
            .is_err()
    );
    let withdrawal = SignedGroupFix::sign(
        a.secret_key(),
        GroupFix {
            group: signed.fix.group,
            origin: a.id(),
            sequence: signed.fix.sequence + 10,
            timestamp_ms: now,
            payload: GroupPayload::Unavailable(protocol::Availability::Off),
        },
    )
    .unwrap();
    assert!(
        mb.ingest(a.id(), &Message::GroupFix(withdrawal.clone()), later)
            .unwrap()
    );
    mb.send_latest(&bc, later, false).unwrap();
    let message = protocol::decode(&cb.read_datagram().await.unwrap(), later).unwrap();
    assert!(mc.ingest(b.id(), &message, later).unwrap());
    let snapshot = mc
        .snapshots(later)
        .unwrap()
        .into_iter()
        .find(|s| s.peer_id == a.id())
        .unwrap();
    assert!(snapshot.latest.is_none() && snapshot.withdrawn);
    let restarted = Manager::new(stores[2].clone(), c.secret_key().clone()).unwrap();
    assert!(
        restarted
            .snapshots(later)
            .unwrap()
            .iter()
            .find(|s| s.peer_id == a.id())
            .unwrap()
            .withdrawn
    );
    let direct = Message::LocationUpdate(
        sample(later)
            .to_update(withdrawal.fix.sequence + 1, later)
            .unwrap(),
    );
    assert!(mc.ingest(a.id(), &direct, later).unwrap());
    assert!(!mc.ingest(b.id(), &message, later).unwrap());
    assert!(
        mc.snapshots(later)
            .unwrap()
            .iter()
            .find(|s| s.peer_id == a.id())
            .unwrap()
            .latest
            .is_some()
    );
    b.close().await;
    c.close().await;
}

#[tokio::test]
async fn relay_admission_cutoff_block_gate_and_trip_withdrawal() {
    use super::{
        groups::{GroupOps, OpKind, Policy, SignedOp},
        protocol::{GroupFix, GroupPayload, SignedGroupFix},
    };
    let a = endpoint().await;
    let b = endpoint().await;
    let c_endpoint = endpoint().await;
    let c = c_endpoint.secret_key().clone();
    let da = tempfile::tempdir().unwrap();
    let db = tempfile::tempdir().unwrap();
    let sa = Friends::open(da.path(), a.id()).unwrap();
    let sb = Friends::open(db.path(), b.id()).unwrap();
    let start = now_ms() - 10_000;
    let root = SignedOp::sign(
        a.secret_key(),
        [0; 32],
        start as u64,
        OpKind::Create {
            name: "Family".into(),
            policy: Policy::Managed,
            creator_name: "A".into(),
        },
    )
    .unwrap();
    let group = root.digest().unwrap();
    let ops = vec![
        root,
        SignedOp::sign(
            a.secret_key(),
            group,
            (start + 1) as u64,
            OpKind::Admit {
                member: b.id(),
                name: "B".into(),
            },
        )
        .unwrap(),
        SignedOp::sign(
            a.secret_key(),
            group,
            (start + 5000) as u64,
            OpKind::Admit {
                member: c.public(),
                name: "C".into(),
            },
        )
        .unwrap(),
    ];
    let log = GroupOps { group, ops };
    sa.join_group(&log, "A", Permission::TripsOnly).unwrap();
    sb.join_group(&log, "B", Permission::Always).unwrap();
    let ma = Manager::new(sa.clone(), a.secret_key().clone()).unwrap();
    let mb = Manager::new(sb.clone(), b.secret_key().clone()).unwrap();
    ma.set_mode(Mode::Always).unwrap();
    mb.set_mode(Mode::Always).unwrap();
    let old = SignedGroupFix::sign(
        a.secret_key(),
        GroupFix {
            group,
            origin: a.id(),
            sequence: 1,
            timestamp_ms: start + 100,
            payload: GroupPayload::Fix(sample(start + 100).to_update(1, start + 100).unwrap()),
        },
    )
    .unwrap();
    assert!(
        mb.ingest(a.id(), &Message::GroupFix(old.clone()), now_ms())
            .unwrap()
    );
    // A newly admitted receiver rejects pre-admission history even from a malicious forwarder.
    let dc = tempfile::tempdir().unwrap();
    let sc = Friends::open(dc.path(), c.public()).unwrap();
    sc.join_group(&log, "C", Permission::Always).unwrap();
    let mc = Manager::new(sc, c.clone()).unwrap();
    assert!(
        !mc.ingest(b.id(), &Message::GroupFix(old), now_ms())
            .unwrap()
    );
    let (bc, cb) = tokio::join!(b.connect(c_endpoint.addr(), protocol::ALPN), async {
        c_endpoint.accept().await.unwrap().await
    });
    let (bc, cb) = (bc.unwrap(), cb.unwrap());
    mb.send_latest(&bc, now_ms(), true).unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(100), cb.read_datagram())
            .await
            .is_err()
    );
    // No signed group fix may be originated while any member is locally blocked.
    sa.block(c.public()).unwrap();
    let now = now_ms();
    ma.update_fix(sample(now), true, now).unwrap();
    let (ab, ba) = tokio::join!(a.connect(b.addr(), protocol::ALPN), async {
        b.accept().await.unwrap().await
    });
    let (ab, ba) = (ab.unwrap(), ba.unwrap());
    assert!(ma.send_latest(&ab, now, true).unwrap());
    assert!(matches!(
        protocol::decode(&ba.read_datagram().await.unwrap(), now).unwrap(),
        Message::LocationUpdate(_)
    ));
    assert!(
        tokio::time::timeout(Duration::from_millis(100), ba.read_datagram())
            .await
            .is_err()
    );
    // Clear local block through existing explicit remove/re-pair flow.
    sa.remove(c.public()).unwrap();
    let resumed = now + 30_000;
    ma.update_fix(sample(resumed), true, resumed).unwrap();
    ma.send_latest(&ab, resumed, true).unwrap();
    let Message::GroupFix(fix) =
        protocol::decode(&ba.read_datagram().await.unwrap(), resumed).unwrap()
    else {
        panic!("expected signed group fix")
    };
    assert!(matches!(fix.fix.payload, GroupPayload::Fix(_)));
    let sequence = fix.fix.sequence;
    ma.set_recording(false).unwrap();
    assert!(!ma.send_latest(&ab, resumed + 1, false).unwrap());
    let message = protocol::decode(&ba.read_datagram().await.unwrap(), resumed + 1).unwrap();
    assert!(
        matches!(message,Message::GroupFix(fix) if fix.fix.sequence>sequence && matches!(fix.fix.payload,GroupPayload::Unavailable(protocol::Availability::TripInactive)))
    );
    a.close().await;
    b.close().await;
    c_endpoint.close().await;
}

#[tokio::test]
async fn refresh_requests_collect_fresh_fix_under_permissions_and_report_result() {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    let a = endpoint().await;
    let b = endpoint().await;
    let da = tempfile::tempdir().unwrap();
    let db = tempfile::tempdir().unwrap();
    let ma = manager(da.path(), a.secret_key(), b.id());
    let mb = manager(db.path(), b.secret_key(), a.id());
    let (client, server) = tokio::join!(a.connect(b.addr(), protocol::ALPN), async {
        b.accept().await.unwrap().await
    });
    let client = client.unwrap();
    let server = server.unwrap();
    ma.register(&client, true).unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    mb.set_refresh_source(Arc::new({
        let calls = calls.clone();
        move || {
            calls.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }));
    let now = now_ms();
    assert!(!mb.refresh_requested(a.id(), now).unwrap());
    mb.set_mode(Mode::Always).unwrap();
    mb.friends()
        .set_permission(a.id(), Permission::Always)
        .unwrap();
    // Older than the clock-skew allowance, so it does not answer the request below.
    mb.update_fix(sample(now - 10_000), false, now).unwrap();
    assert!(mb.send_latest(&server, now, true).unwrap());
    let old = protocol::decode(&client.read_datagram().await.unwrap(), now).unwrap();
    ma.ingest(b.id(), &old, now).unwrap();
    ma.set_mode(Mode::Always).unwrap();
    super::requests::queue(ma.friends(), b.id(), now).unwrap();
    ma.poll_requests(now).unwrap();
    assert_eq!(
        protocol::decode(&server.read_datagram().await.unwrap(), now).unwrap(),
        Message::LocationRequest
    );
    assert!(mb.refresh_requested(a.id(), now).unwrap());
    assert!(!mb.refresh_requested(a.id(), now + 1).unwrap());
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    mb.update_fix(sample(now + 1), false, now + 1).unwrap();
    assert!(mb.send_latest(&server, now + 1, false).unwrap()); // Fresh request bypasses 30s movement cadence once.
    let fresh = protocol::decode(&client.read_datagram().await.unwrap(), now + 1).unwrap();
    ma.ingest(b.id(), &fresh, now + 1).unwrap();
    ma.poll_requests(now + 1).unwrap();
    assert_eq!(
        super::requests::read(ma.friends(), b.id()).unwrap().status,
        "Fresh location received"
    );
    assert!(!mb.send_latest(&server, now + 2, false).unwrap());
    assert!(super::requests::queue(ma.friends(), b.id(), now + 2).is_err());
    // Older fix sent after the request (as on a just-opened session) still lets the fresh one out.
    let t = now + 130_000;
    mb.update_fix(sample(t - 500), false, t).unwrap();
    assert!(mb.refresh_requested(a.id(), t).unwrap());
    assert!(mb.send_latest(&server, t + 10, true).unwrap());
    client.read_datagram().await.unwrap();
    mb.update_fix(sample(t + 20), false, t + 20).unwrap();
    assert!(mb.send_latest(&server, t + 30, false).unwrap());
    client.read_datagram().await.unwrap();
    mb.friends().block(a.id()).unwrap();
    assert!(!mb.refresh_requested(a.id(), t + 70_000).unwrap());
    // No session yet: keeps locating while dialer connects, gives up after the window.
    ma.unregister(&client);
    let later = now + 60_000;
    super::requests::queue(ma.friends(), b.id(), later).unwrap();
    ma.poll_requests(later).unwrap();
    assert_eq!(
        super::requests::read(ma.friends(), b.id()).unwrap().status,
        "Locating…"
    );
    ma.poll_requests(later + 6 * 60_000).unwrap();
    assert_eq!(
        super::requests::read(ma.friends(), b.id()).unwrap().status,
        "Locating…"
    );
    ma.poll_requests(later + 7 * 60_000).unwrap();
    assert_eq!(
        super::requests::read(ma.friends(), b.id()).unwrap().status,
        "Not connected; try again when online"
    );
}

#[tokio::test]
async fn battery_is_opt_in_permission_gated_and_cleared_when_disabled() {
    let a = endpoint().await;
    let b = endpoint().await;
    let da = tempfile::tempdir().unwrap();
    let db = tempfile::tempdir().unwrap();
    let ma = manager(da.path(), a.secret_key(), b.id());
    let mb = manager(db.path(), b.secret_key(), a.id());
    let (client, server) = tokio::join!(a.connect(b.addr(), protocol::ALPN), async {
        b.accept().await.unwrap().await
    });
    let client = client.unwrap();
    let server = server.unwrap();
    let mut now = now_ms();
    ma.set_mode(Mode::Always).unwrap();
    ma.friends()
        .set_permission(b.id(), Permission::Always)
        .unwrap();
    ma.update_battery(Some(73), now).unwrap();
    assert!(!ma.shares_battery().unwrap());
    for (enabled, permission, expected) in [
        (false, Permission::Always, None),
        (true, Permission::Always, Some(73)),
        (true, Permission::Off, None),
        (true, Permission::TripsOnly, None),
        (false, Permission::Always, None),
    ] {
        // Unchanged level is resent every 5 minutes.
        now += 300_000;
        ma.update_battery(Some(73), now).unwrap();
        ma.set_share_battery(enabled).unwrap();
        ma.friends().set_permission(b.id(), permission).unwrap();
        ma.send_battery(&client, now).unwrap();
        let message = protocol::decode(&server.read_datagram().await.unwrap(), now).unwrap();
        assert_eq!(
            message,
            Message::BatteryStatus(protocol::BatteryStatus {
                percent: expected,
                timestamp_ms: now
            })
        );
        mb.ingest(a.id(), &message, now).unwrap();
        assert_eq!(
            mb.snapshots(now).unwrap()[0].battery.unwrap().percent,
            expected
        );
    }
    // A received battery packet must not trigger an endless reply loop.
    ma.send_battery(&client, now + 1).unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(50), server.read_datagram())
            .await
            .is_err()
    );
    assert!(ma.update_battery(Some(101), now).is_err());
    assert!(
        protocol::decode(
            &protocol::encode(&Message::BatteryStatus(protocol::BatteryStatus {
                percent: Some(255),
                timestamp_ms: now
            }))
            .unwrap(),
            now
        )
        .is_err()
    );
}
