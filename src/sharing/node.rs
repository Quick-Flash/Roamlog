//! Long-running sharing node: accepts friend sessions, dials every trusted friend, and
//! publishes received locations to a file the UI process reads.
use super::{
    Result,
    location::RETENTION_MS,
    manager::{Manager, now_ms},
    protocol::LocationUpdate,
    transport,
};
use iroh::{Endpoint, EndpointAddr, EndpointId, SecretKey};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    io::Write,
    path::{Path, PathBuf},
    time::Duration,
};
use tokio::task::JoinHandle;

/// UI treats an older file as "sharing service not running".
pub const HEARTBEAT_MS: i64 = 60_000;
const FIRST_RETRY: Duration = Duration::from_secs(15);
const MAX_RETRY: Duration = Duration::from_secs(120);

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FriendState {
    pub peer_id: EndpointId,
    pub name: String,
    /// Last received position; kept while disconnected until retention expires.
    pub location: Option<LocationUpdate>,
    #[serde(default)]
    pub battery: Option<super::protocol::BatteryStatus>,
    #[serde(default)]
    pub via: Option<EndpointId>,
    pub connected: bool,
    #[serde(default)]
    pub update_required: bool,
    /// Friend currently allows us to see a live position.
    pub sharing_active: bool,
    /// Selected path while connected: "direct" or "relay".
    #[serde(default)]
    pub path: Option<String>,
}

#[derive(Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Published {
    pub updated_ms: i64,
    pub friends: Vec<FriendState>,
}

pub fn published_path(directory: &Path) -> PathBuf {
    directory.join("sharing").join("friend-locations.json")
}

/// Missing or unreadable file reads as empty: UI shows no friend positions.
pub fn read_published(directory: &Path) -> Published {
    std::fs::read(published_path(directory))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

fn write_published(path: &Path, published: &Published) -> Result<()> {
    let directory = path.parent().ok_or("invalid sharing path")?;
    let mut file = tempfile::NamedTempFile::new_in(directory)?;
    file.write_all(&serde_json::to_vec(published)?)?;
    file.persist(path)?;
    Ok(())
}

/// Merge live session state into last published state. Disconnected friends keep their
/// last position until retention expires; removed and blocked friends disappear.
fn merge(manager: &Manager, previous: &[FriendState], now: i64) -> Result<Vec<FriendState>> {
    let live: BTreeMap<_, _> = manager
        .snapshots(now)?
        .into_iter()
        .map(|s| (s.peer_id, s))
        .collect();
    let mut friends = Vec::new();
    for friend in manager.friends().list()? {
        if friend.blocked {
            continue;
        }
        let old = previous.iter().find(|f| f.peer_id == friend.peer_id);
        let current = live.get(&friend.peer_id);
        let location = if current.is_some_and(|s| s.withdrawn) {
            None
        } else {
            current
                .and_then(|s| s.latest)
                .or_else(|| old.and_then(|f| f.location))
        }
        .filter(|l| now.saturating_sub(l.timestamp_ms) <= RETENTION_MS);
        let connected = manager.connected(friend.peer_id);
        friends.push(FriendState {
            peer_id: friend.peer_id,
            name: friend.display_name,
            location,
            battery: current.filter(|s| s.connected).and_then(|s| s.battery),
            via: if current.is_some_and(|s| s.latest.is_some() || s.withdrawn) {
                current.and_then(|s| s.via)
            } else {
                old.and_then(|s| s.via)
            },
            connected,
            update_required: manager.update_required(friend.peer_id),
            sharing_active: connected && current.is_some_and(|s| s.sharing_active),
            path: manager.path(friend.peer_id).map(str::to_owned),
        });
    }
    Ok(friends)
}

async fn publish(manager: Manager, path: PathBuf) {
    let mut previous = std::fs::read(&path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Published>(&bytes).ok())
        .unwrap_or_default();
    let mut changes = manager.changes();
    let mut tick = tokio::time::interval(Duration::from_secs(2));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            _ = tick.tick() => {},
            _ = changes.changed() => {},
        }
        #[cfg(all(target_os = "android", feature = "ui"))]
        if let Ok(notices) = manager.friends().membership_notices() {
            for (id, text) in notices {
                let _ = zgui::app::android::notification::post(
                    &zgui::app::android::notification::Notification {
                        id,
                        title: "Group membership".into(),
                        text,
                        ..Default::default()
                    },
                );
            }
        }
        let now = now_ms();
        if manager.expire_timer(now).unwrap_or(false) {
            manager.flush_group_fixes();
            #[cfg(all(target_os = "android", feature = "ui"))]
            crate::android::sharing_timer_expired();
        }
        let _ = manager.poll_requests(now);
        #[cfg(all(target_os = "android", feature = "ui"))]
        if manager.shares_battery().unwrap_or(false) {
            let _ =
                manager.update_battery(super::android_app::battery_percent().ok().flatten(), now);
        }
        let Ok(friends) = merge(&manager, &previous.friends, now) else {
            continue;
        };
        if let Ok(notices) = super::places::observe(manager.friends(), &friends, now) {
            #[cfg(all(target_os = "android", feature = "ui"))]
            for (id, text) in notices {
                let _ = zgui::app::android::notification::post(
                    &zgui::app::android::notification::Notification {
                        id,
                        title: "Place alert".into(),
                        text,
                        ..Default::default()
                    },
                );
            }
            #[cfg(not(all(target_os = "android", feature = "ui")))]
            let _ = notices;
        }
        let _ = super::places::confirmation_requests(manager.friends(), now);
        if friends == previous.friends && now - previous.updated_ms < HEARTBEAT_MS {
            continue;
        }
        let next = Published {
            updated_ms: now,
            friends,
        };
        if write_published(&path, &next).is_ok() {
            previous = next;
        }
    }
}

/// Keeps dialing one friend while no session is open. Removing or blocking the friend
/// makes `transport::client` fail and the supervisor aborts this task.
async fn dial(endpoint: Endpoint, manager: Manager, peer: EndpointId) {
    let mut retry = FIRST_RETRY;
    loop {
        if manager.connected(peer) {
            retry = FIRST_RETRY;
            tokio::time::sleep(FIRST_RETRY).await;
            continue;
        }
        let started = tokio::time::Instant::now();
        let result = transport::client(&endpoint, EndpointAddr::new(peer), manager.clone()).await;
        if result.as_ref().err().is_some_and(|e| {
            e.downcast_ref::<std::io::Error>()
                .is_some_and(|e| e.kind() == std::io::ErrorKind::Unsupported)
        }) {
            manager.require_update(peer);
        }
        if result.is_ok() || endpoint.is_closed() {
            return;
        }
        // Long session that ended counts as success: reconnect promptly.
        if started.elapsed() > MAX_RETRY {
            retry = FIRST_RETRY;
        }
        tokio::time::sleep(retry).await;
        retry = (retry * 2).min(MAX_RETRY);
    }
}

async fn supervise(endpoint: Endpoint, manager: Manager) {
    let mut dialers: BTreeMap<EndpointId, JoinHandle<()>> = BTreeMap::new();
    let mut changes = manager.changes();
    let mut tick = tokio::time::interval(Duration::from_secs(15));
    loop {
        tokio::select! {
            _ = tick.tick() => {},
            _ = changes.changed() => {},
        }
        if manager.friends().finish_leaving().unwrap_or(false) {
            manager.notify();
        }
        let Ok(friends) = manager.friends().list() else {
            continue;
        };
        let wanted: Vec<_> = friends
            .iter()
            .filter(|f| !f.blocked)
            .map(|f| f.peer_id)
            .collect();
        dialers.retain(|peer, task| {
            let keep = wanted.contains(peer) && !task.is_finished();
            if !keep {
                task.abort();
            }
            keep
        });
        for peer in wanted {
            dialers
                .entry(peer)
                .or_insert_with(|| tokio::spawn(dial(endpoint.clone(), manager.clone(), peer)));
        }
    }
}

/// Running endpoint and its tasks. Dropping it closes every session.
pub struct Node {
    runtime: Option<tokio::runtime::Runtime>,
    endpoint: Endpoint,
    manager: Manager,
}

impl Node {
    pub fn start(key: SecretKey, manager: Manager, directory: &Path) -> Result<Self> {
        if key.public() != manager.owner() {
            return Err("sharing identity mismatch".into());
        }
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()?;
        let endpoint = runtime.block_on(transport::bind(key))?;
        Self::spawn(runtime, endpoint, manager, published_path(directory))
    }

    fn spawn(
        runtime: tokio::runtime::Runtime,
        endpoint: Endpoint,
        manager: Manager,
        output: PathBuf,
    ) -> Result<Self> {
        let server = endpoint.clone();
        let accepted = manager.clone();
        runtime.spawn(async move {
            let _ = transport::server(&server, accepted).await;
        });
        runtime.spawn(supervise(endpoint.clone(), manager.clone()));
        runtime.spawn(publish(manager.clone(), output));
        Ok(Self {
            runtime: Some(runtime),
            endpoint,
            manager,
        })
    }

    pub fn manager(&self) -> &Manager {
        &self.manager
    }
}

impl Drop for Node {
    fn drop(&mut self) {
        self.manager.flush_group_fixes();
        if let Some(runtime) = self.runtime.take() {
            let endpoint = self.endpoint.clone();
            // Tell peers sessions ended instead of letting them time out.
            let _ = runtime.block_on(async {
                tokio::time::timeout(Duration::from_secs(3), endpoint.close()).await
            });
            runtime.shutdown_timeout(Duration::from_secs(1));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sharing::{
        location::Sample,
        manager::Mode,
        peers::{Friends, Permission},
        protocol::ALPN,
    };

    fn endpoint(runtime: &tokio::runtime::Runtime, key: SecretKey) -> Endpoint {
        runtime.block_on(async {
            Endpoint::builder(iroh::endpoint::presets::Minimal)
                .secret_key(key)
                .alpns(vec![ALPN.to_vec()])
                .bind()
                .await
                .unwrap()
        })
    }

    fn sample(lat: f64) -> Sample {
        Sample {
            fix: crate::model::Fix {
                point: crate::geo::Point { lat, lon: -111.0 },
                timestamp_ms: now_ms() as u64,
                accuracy_m: 5.0,
            },
            speed_m_s: None,
            heading_degrees: None,
        }
    }

    /// Both nodes dial each other; one session survives and locations reach the file.
    #[test]
    fn nodes_converge_on_one_session_and_publish_locations() {
        let dirs = [tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap()];
        let keys = [SecretKey::generate(), SecretKey::generate()];
        let ids = [keys[0].public(), keys[1].public()];
        let managers: Vec<_> = (0..2)
            .map(|i| {
                let friends = Friends::open(dirs[i].path(), ids[i]).unwrap();
                friends
                    .add_confirmed(ids[1 - i], format!("Friend {i}"))
                    .unwrap();
                friends
                    .set_permission(ids[1 - i], Permission::Always)
                    .unwrap();
                let manager = Manager::new(friends, keys[i].clone()).unwrap();
                manager.set_mode(Mode::Always).unwrap();
                manager
            })
            .collect();
        let runtimes: Vec<_> = (0..2)
            .map(|_| {
                tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(2)
                    .enable_all()
                    .build()
                    .unwrap()
            })
            .collect();
        let endpoints: Vec<_> = (0..2)
            .map(|i| endpoint(&runtimes[i], keys[i].clone()))
            .collect();
        // Minimal preset has no discovery: tell each endpoint where the other lives.
        let addrs: Vec<_> = endpoints.iter().map(|e| e.addr()).collect();
        let lookup = |i: usize| {
            let memory = iroh::address_lookup::memory::MemoryLookup::new();
            memory.add_endpoint_info(addrs[1 - i].clone());
            memory
        };
        for (i, endpoint) in endpoints.iter().enumerate() {
            endpoint.address_lookup().unwrap().add(lookup(i));
        }
        let mut nodes = Vec::new();
        for (i, runtime) in runtimes.into_iter().enumerate() {
            nodes.push(
                Node::spawn(
                    runtime,
                    endpoints[i].clone(),
                    managers[i].clone(),
                    published_path(dirs[i].path()),
                )
                .unwrap(),
            );
        }
        // Friend 1 has a profile picture; friend 0 should fetch it on connect.
        let picture = b"\x89PNG\r\n\x1a\nfake".to_vec();
        std::fs::write(
            crate::sharing::transport::own_avatar_path(&dirs[1].path().join("sharing")),
            &picture,
        )
        .unwrap();
        managers[0]
            .update_fix(sample(40.0), false, now_ms())
            .unwrap();
        managers[1]
            .update_fix(sample(41.0), false, now_ms())
            .unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(60);
        loop {
            let published: Vec<_> = dirs.iter().map(|d| read_published(d.path())).collect();
            let done = published.iter().all(|p| {
                p.friends.len() == 1
                    && p.friends[0].connected
                    && p.friends[0].sharing_active
                    && p.friends[0].location.is_some()
            });
            if done {
                assert_eq!(
                    published[0].friends[0].location.unwrap().latitude_e7,
                    410_000_000
                );
                assert_eq!(published[1].friends[0].name, "Friend 1");
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "nodes did not exchange"
            );
            std::thread::sleep(Duration::from_millis(200));
        }
        let avatar =
            crate::sharing::transport::friend_avatar_path(&dirs[0].path().join("sharing"), ids[1]);
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        while std::fs::read(&avatar).ok() != Some(picture.clone()) {
            assert!(std::time::Instant::now() < deadline, "avatar not fetched");
            std::thread::sleep(Duration::from_millis(200));
        }
        // Converged: stays connected (no replacement loop) over a few dial periods.
        std::thread::sleep(Duration::from_secs(3));
        assert!(managers[0].connected(ids[1]) && managers[1].connected(ids[0]));
        // Removing friend drops session and its published entry.
        managers[0].friends().remove(ids[1]).unwrap();
        managers[0].notify();
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        while !read_published(dirs[0].path()).friends.is_empty() || managers[1].connected(ids[0]) {
            assert!(std::time::Instant::now() < deadline, "removal not applied");
            std::thread::sleep(Duration::from_millis(200));
        }
        drop(nodes);
    }

    /// A invites B and C into a group; B and C never paired, yet learn each other through
    /// A's member list and share automatically. Leaving removes C everywhere.
    #[test]
    fn group_members_gossip_share_and_leave() {
        let n = 3;
        let dirs: Vec<_> = (0..n).map(|_| tempfile::tempdir().unwrap()).collect();
        let keys: Vec<_> = (0..n).map(|_| SecretKey::generate()).collect();
        let ids: Vec<_> = keys.iter().map(SecretKey::public).collect();
        let managers: Vec<_> = (0..n)
            .map(|i| {
                let manager = Manager::new(
                    Friends::open(dirs[i].path(), ids[i]).unwrap(),
                    keys[i].clone(),
                )
                .unwrap();
                manager.set_mode(Mode::Always).unwrap();
                manager
            })
            .collect();
        let group = managers[0]
            .friends()
            .create_group(
                &keys[0],
                "Family",
                "A",
                super::super::groups::Policy::Managed,
                Permission::Always,
            )
            .unwrap();
        for j in 1..n {
            // Same records confirmed pairing with a group invitation writes.
            managers[0]
                .friends()
                .add_confirmed(ids[j], format!("M{j}"))
                .unwrap();
            let admission = managers[0]
                .friends()
                .admit_member(&keys[0], &group, ids[j], &format!("M{j}"))
                .unwrap();
            managers[j]
                .friends()
                .add_confirmed(ids[0], "A".into())
                .unwrap();
            managers[j]
                .friends()
                .join_group(&admission, &format!("M{j}"), Permission::Always)
                .unwrap();
        }
        let runtimes: Vec<_> = (0..n)
            .map(|_| {
                tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(2)
                    .enable_all()
                    .build()
                    .unwrap()
            })
            .collect();
        let endpoints: Vec<_> = (0..n)
            .map(|i| endpoint(&runtimes[i], keys[i].clone()))
            .collect();
        let addrs: Vec<_> = endpoints.iter().map(|e| e.addr()).collect();
        let mut lookups = Vec::new();
        for (i, endpoint) in endpoints.iter().enumerate() {
            let memory = iroh::address_lookup::memory::MemoryLookup::new();
            for (j, addr) in addrs.iter().enumerate() {
                if i != j {
                    memory.add_endpoint_info(addr.clone());
                }
            }
            lookups.push(memory.clone());
            endpoint.address_lookup().unwrap().add(memory);
        }
        let mut nodes = Vec::new();
        for (i, runtime) in runtimes.into_iter().enumerate() {
            managers[i]
                .update_fix(sample(40.0 + i as f64), false, now_ms())
                .unwrap();
            nodes.push(
                Node::spawn(
                    runtime,
                    endpoints[i].clone(),
                    managers[i].clone(),
                    published_path(dirs[i].path()),
                )
                .unwrap(),
            );
        }
        let wait = |what: &str, done: &dyn Fn() -> bool| {
            let deadline = std::time::Instant::now() + Duration::from_secs(60);
            while !done() {
                assert!(std::time::Instant::now() < deadline, "{what}");
                std::thread::sleep(Duration::from_millis(200));
            }
        };
        wait("B and C share through group", &|| {
            [(1, 2), (2, 1)].iter().all(|&(i, j)| {
                read_published(dirs[i].path())
                    .friends
                    .iter()
                    .any(|f| f.peer_id == ids[j] && f.sharing_active && f.location.is_some())
            })
        });
        let b = managers[1].friends().list().unwrap();
        let c_on_b = b.iter().find(|f| f.peer_id == ids[2]).unwrap();
        assert!(c_on_b.group_only && c_on_b.permission == Permission::Off);
        assert_eq!(c_on_b.display_name, "M2");
        // Admin remove stops B's group grant; C receives its own signed removal.
        managers[0]
            .friends()
            .remove_member(&keys[0], &group, ids[2], false)
            .unwrap();
        managers[0].notify();
        wait("admin removal propagates to B and C", &|| {
            !managers[1].friends().may_share(ids[2], true).unwrap()
                && !managers[2]
                    .friends()
                    .group_state(&group)
                    .unwrap()
                    .unwrap()
                    .members
                    .contains_key(&ids[2])
        });
        managers[0]
            .friends()
            .admit_member(&keys[0], &group, ids[2], "M2")
            .unwrap();
        managers[0].notify();
        wait("readmission restores membership", &|| {
            managers[1].friends().may_share(ids[2], true).unwrap()
                && managers[2].friends().may_share(ids[1], true).unwrap()
        });
        // B is genuinely offline. C gets A's acknowledgement and forgets the group
        // before B returns, so only A can carry the leave to B.
        drop(nodes.remove(1));
        managers[2].friends().leave_group(&keys[2], &group).unwrap();
        managers[2].notify();
        wait("A acknowledges leave while B is offline", &|| {
            managers[0]
                .friends()
                .list()
                .unwrap()
                .iter()
                .all(|f| f.groups.is_empty() || f.peer_id != ids[2])
                && managers[2].friends().groups().unwrap().is_empty()
                && !managers[2]
                    .friends()
                    .list()
                    .unwrap()
                    .iter()
                    .any(|f| f.peer_id == ids[1])
        });
        assert!(managers[1].friends().may_share(ids[2], true).unwrap());
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let resumed = endpoint(&runtime, keys[1].clone());
        resumed.address_lookup().unwrap().add(lookups[1].clone());
        for i in [0, 2] {
            let _ = lookups[i].set_endpoint_info(resumed.addr());
        }
        nodes.push(
            Node::spawn(
                runtime,
                resumed,
                managers[1].clone(),
                published_path(dirs[1].path()),
            )
            .unwrap(),
        );
        wait("A carries leave to returning B", &|| {
            !managers[1]
                .friends()
                .list()
                .unwrap()
                .iter()
                .any(|f| f.peer_id == ids[2])
        });
        // A still pairs C directly, but group no longer grants access.
        assert!(!managers[0].friends().may_share(ids[2], true).unwrap());
        assert!(managers[0].friends().may_share(ids[1], false).unwrap());
        drop(nodes);
    }
}
