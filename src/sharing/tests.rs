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

fn manager(dir: &std::path::Path, owner: iroh::EndpointId, peer: iroh::EndpointId) -> Manager {
    let friends = Friends::open(dir, owner).unwrap();
    friends.add_confirmed(peer, "Friend".into()).unwrap();
    Manager::new(friends).unwrap()
}

#[tokio::test]
async fn sender_gates_every_packet_and_sequences_survive_restart() {
    let a = endpoint().await;
    let b = endpoint().await;
    let da = tempfile::tempdir().unwrap();
    let db = tempfile::tempdir().unwrap();
    let ma = manager(da.path(), a.id(), b.id());
    let mb = manager(db.path(), b.id(), a.id());
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

    let restarted_sender = Manager::new(Friends::open(da.path(), a.id()).unwrap()).unwrap();
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
    let restarted_receiver = Manager::new(Friends::open(db.path(), b.id()).unwrap()).unwrap();
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
    let owner = SecretKey::generate().public();
    let peer = SecretKey::generate().public();
    let manager = manager(dir.path(), owner, peer);
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
    assert!(
        !manager
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
    assert!(Manager::new(manager.friends().clone()).is_err());
    assert_eq!(std::fs::read(path).unwrap(), b"corrupt");
}

#[tokio::test]
async fn authenticated_sessions_exchange_locations_and_survive_reconnect() {
    let a = endpoint().await;
    let b = endpoint().await;
    let da = tempfile::tempdir().unwrap();
    let db = tempfile::tempdir().unwrap();
    let ma = manager(da.path(), a.id(), b.id());
    let mb = manager(db.path(), b.id(), a.id());
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
    let manager = Manager::new(Friends::open(dir.path(), a.id()).unwrap()).unwrap();
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
    let reply = serde_json::json!({"trips": [walk(1_000_000, "Later"), walk(1_000, "Earlier"), {"fixes": []}]});
    let journal = super::transport::history_journal(&serde_json::to_vec(&reply).unwrap()).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("friend.jsonl");
    std::fs::write(&path, journal).unwrap();
    let store = crate::store::Store::open(&path).unwrap();
    let names: Vec<_> = store.state.trips.iter().map(|t| t.name.as_str()).collect();
    assert_eq!(names, ["Earlier", "Later"]);
    assert!(
        store
            .state
            .trips
            .iter()
            .all(|t| t.status == crate::model::Status::Finished && t.distance_m > 50.0)
    );
}
