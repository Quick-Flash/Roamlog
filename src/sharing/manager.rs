//! Sender policy, latest-only samples, replay protection, and volatile friend positions.
use super::{
    Result,
    location::{MAX_FIX_AGE_MS, RETENTION_MS, Sample, UpdatePolicy},
    peers::{Friend, Friends, Group, GroupSync, Permission},
    protocol::{self, Availability, LocationUpdate, Message},
};
use iroh::{EndpointId, endpoint::Connection};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs::File,
    io::{Read, Write},
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};
use tokio::sync::watch;

#[derive(Clone, Copy, Default, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    #[default]
    Off,
    TripsOnly,
    Always,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Cursor {
    peer: EndpointId,
    sequence: u64,
    timestamp_ms: i64,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Metadata {
    version: u8,
    owner: EndpointId,
    mode: Mode,
    sequence: u64,
    policy: UpdatePolicy,
    received: Vec<Cursor>,
}

#[derive(Clone, Serialize)]
pub struct PeerLocation {
    pub peer_id: EndpointId,
    pub latest: Option<LocationUpdate>,
    pub age_ms: Option<i64>,
    pub stale: bool,
    pub connected: bool,
    pub sharing_active: bool,
}

struct Remote {
    latest: Option<LocationUpdate>,
    received_at_ms: i64,
    availability: Availability,
    permission: bool,
}
impl Default for Remote {
    fn default() -> Self {
        Self {
            latest: None,
            received_at_ms: 0,
            availability: Availability::Off,
            permission: false,
        }
    }
}

#[derive(Default)]
struct Memory {
    latest: Option<Sample>,
    recording: bool,
    sent: BTreeMap<EndpointId, (crate::geo::Point, i64)>,
    remote: BTreeMap<EndpointId, Remote>,
    /// Live session per friend and whether lower identity opened it (preferred on both sides).
    sessions: BTreeMap<EndpointId, (Connection, bool)>,
}

/// Produces own finished trips as JSON for friends allowed to view them.
pub type TripSource = Arc<dyn Fn() -> Result<Vec<u8>> + Send + Sync>;

struct Inner {
    friends: Friends,
    memory: Mutex<Memory>,
    changed: watch::Sender<()>,
    trips: Mutex<Option<TripSource>>,
}

#[derive(Clone)]
pub struct Manager(Arc<Inner>);

pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|time| time.as_millis().min(i64::MAX as u128) as i64)
        .unwrap_or(0)
}

fn trusted(records: &[Friend], peer: EndpointId) -> bool {
    records
        .iter()
        .any(|friend| friend.peer_id == peer && !friend.blocked)
}

fn availability(
    records: &[Friend],
    groups: &[Group],
    peer: EndpointId,
    mode: Mode,
    memory: &Memory,
    now_ms: i64,
) -> (bool, Availability) {
    let Some(friend) = records
        .iter()
        .find(|friend| friend.peer_id == peer && !friend.blocked)
    else {
        return (false, Availability::Off);
    };
    let permission = friend.effective_permission(groups);
    if mode == Mode::Off || permission == Permission::Off {
        return (false, Availability::Off);
    }
    if !memory.recording && (mode == Mode::TripsOnly || permission == Permission::TripsOnly) {
        return (false, Availability::TripInactive);
    }
    let current = memory.latest.is_some_and(|sample| {
        let timestamp = i64::try_from(sample.fix.timestamp_ms).unwrap_or(0);
        timestamp > 0
            && timestamp >= now_ms.saturating_sub(MAX_FIX_AGE_MS)
            && timestamp <= now_ms.saturating_add(protocol::FUTURE_SKEW_MS)
    });
    (
        true,
        if current {
            Availability::Active
        } else {
            Availability::NoFix
        },
    )
}

impl Manager {
    pub fn new(friends: Friends) -> Result<Self> {
        let manager = Self(Arc::new(Inner {
            friends,
            memory: Mutex::new(Memory::default()),
            changed: watch::channel(()).0,
            trips: Mutex::new(None),
        }));
        {
            let _lock = manager.0.friends.lock()?;
            manager.read_metadata()?;
        }
        Ok(manager)
    }

    pub fn owner(&self) -> EndpointId {
        self.0.friends.owner()
    }
    pub fn friends(&self) -> &Friends {
        &self.0.friends
    }
    pub(super) fn changes(&self) -> watch::Receiver<()> {
        self.0.changed.subscribe()
    }

    // Every metadata access holds friends.lock. Permission changes, global settings,
    // sequence allocation, and synchronous QUIC enqueue therefore serialize together.
    fn read_metadata(&self) -> Result<Metadata> {
        let file = match File::open(self.0.friends.directory().join("location-state.json")) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Metadata {
                    version: 1,
                    owner: self.owner(),
                    mode: Mode::Off,
                    sequence: 0,
                    policy: UpdatePolicy::default(),
                    received: vec![],
                });
            }
            Err(error) => return Err(error.into()),
        };
        let mut bytes = Vec::new();
        file.take(262145).read_to_end(&mut bytes)?;
        if bytes.len() > 262144 {
            return Err("sharing metadata too large".into());
        }
        let metadata: Metadata = serde_json::from_slice(&bytes)?;
        if metadata.version != 1 || metadata.owner != self.owner() || metadata.received.len() > 1024
        {
            return Err("invalid sharing metadata".into());
        }
        metadata.policy.validate()?;
        let mut seen = std::collections::HashSet::new();
        for cursor in &metadata.received {
            if cursor.peer == self.owner()
                || cursor.sequence == 0
                || cursor.timestamp_ms <= 0
                || !seen.insert(cursor.peer)
            {
                return Err("invalid replay cursor".into());
            }
        }
        Ok(metadata)
    }

    fn write_metadata(&self, metadata: &Metadata) -> Result<()> {
        let mut file = tempfile::NamedTempFile::new_in(self.0.friends.directory())?;
        file.write_all(&serde_json::to_vec(metadata)?)?;
        file.as_file().sync_all()?;
        file.persist(self.0.friends.directory().join("location-state.json"))?;
        File::open(self.0.friends.directory())?.sync_all()?;
        Ok(())
    }

    pub fn mode(&self) -> Result<Mode> {
        let _lock = self.0.friends.lock()?;
        Ok(self.read_metadata()?.mode)
    }

    pub fn set_mode(&self, mode: Mode) -> Result<()> {
        let _lock = self.0.friends.lock()?;
        let mut metadata = self.read_metadata()?;
        metadata.mode = mode;
        self.write_metadata(&metadata)?;
        if mode == Mode::Off {
            self.0
                .memory
                .lock()
                .map_err(|_| "sharing lock failed")?
                .latest = None;
        }
        self.0.changed.send_replace(());
        Ok(())
    }

    pub fn set_policy(&self, policy: UpdatePolicy) -> Result<()> {
        policy.validate()?;
        let _lock = self.0.friends.lock()?;
        let mut metadata = self.read_metadata()?;
        metadata.policy = policy;
        self.write_metadata(&metadata)?;
        self.0.changed.send_replace(());
        Ok(())
    }

    /// Existing GPS event source supplies samples. Only newest fix is retained offline.
    pub fn update_fix(&self, sample: Sample, recording: bool, now_ms: i64) -> Result<bool> {
        let mut memory = self.0.memory.lock().map_err(|_| "sharing lock failed")?;
        // Trip transitions must take effect even when GPS payload is invalid or old.
        memory.recording = recording;
        self.0.changed.send_replace(());
        sample.to_update(1, now_ms)?;
        if memory
            .latest
            .is_some_and(|last| last.fix.timestamp_ms > sample.fix.timestamp_ms)
        {
            return Ok(false);
        }
        memory.latest = Some(sample);
        self.0.changed.send_replace(());
        Ok(true)
    }

    pub fn set_recording(&self, recording: bool) -> Result<()> {
        self.0
            .memory
            .lock()
            .map_err(|_| "sharing lock failed")?
            .recording = recording;
        self.0.changed.send_replace(());
        Ok(())
    }

    pub fn clear_fix(&self) -> Result<()> {
        self.0
            .memory
            .lock()
            .map_err(|_| "sharing lock failed")?
            .latest = None;
        self.0.changed.send_replace(());
        Ok(())
    }

    pub fn is_trusted(&self, peer: EndpointId) -> Result<bool> {
        Ok(trusted(&self.0.friends.list()?, peer))
    }

    pub(super) fn availability(
        &self,
        peer: EndpointId,
        now_ms: i64,
    ) -> Result<(bool, Availability)> {
        let _lock = self.0.friends.lock()?;
        let records = self.0.friends.list_locked()?;
        let groups = self.0.friends.groups_locked()?;
        let metadata = self.read_metadata()?;
        let memory = self.0.memory.lock().map_err(|_| "sharing lock failed")?;
        Ok(availability(
            &records,
            &groups,
            peer,
            metadata.mode,
            &memory,
            now_ms,
        ))
    }

    pub(super) fn control(&self, connection: &Connection, message: &Message) -> Result<()> {
        if matches!(message, Message::LocationUpdate(_) | Message::Hello) {
            return Err("invalid control message".into());
        }
        let _lock = self.0.friends.lock()?;
        if !trusted(&self.0.friends.list_locked()?, connection.remote_id()) {
            return Err("peer not trusted".into());
        }
        connection.send_datagram(protocol::encode(message)?.into())?;
        Ok(())
    }

    /// Gate immediately before synchronous datagram enqueue; never buffer coordinates in streams.
    pub(super) fn send_latest(
        &self,
        connection: &Connection,
        now_ms: i64,
        requested: bool,
    ) -> Result<bool> {
        let _lock = self.0.friends.lock()?;
        let records = self.0.friends.list_locked()?;
        let groups = self.0.friends.groups_locked()?;
        let mut metadata = self.read_metadata()?;
        let mut memory = self.0.memory.lock().map_err(|_| "sharing lock failed")?;
        let peer = connection.remote_id();
        if availability(&records, &groups, peer, metadata.mode, &memory, now_ms)
            != (true, Availability::Active)
        {
            return Ok(false);
        }
        let sample = memory.latest.ok_or("location unavailable")?;
        if !metadata
            .policy
            .due(memory.sent.get(&peer).copied(), sample, now_ms, requested)
        {
            return Ok(false);
        }
        metadata.sequence = metadata
            .sequence
            .checked_add(1)
            .ok_or("sharing sequence exhausted")?;
        let update = sample.to_update(metadata.sequence, now_ms)?;
        let bytes = protocol::encode(&Message::LocationUpdate(update))?;
        self.write_metadata(&metadata)?; // Reserve before enqueue; crashes may skip numbers, never reuse them.
        connection.send_datagram(bytes.into())?;
        memory.sent.insert(peer, (sample.fix.point, now_ms));
        Ok(true)
    }

    pub(super) fn ingest(&self, peer: EndpointId, message: &Message, now_ms: i64) -> Result<bool> {
        let _lock = self.0.friends.lock()?;
        if !trusted(&self.0.friends.list_locked()?, peer) {
            return Err("peer not trusted".into());
        }
        if let Message::LocationUpdate(update) = message {
            update.validate(now_ms)?;
            let mut metadata = self.read_metadata()?;
            if metadata.received.iter().any(|cursor| {
                cursor.peer == peer
                    && (update.sequence <= cursor.sequence
                        || update.timestamp_ms < cursor.timestamp_ms)
            }) {
                return Ok(false);
            }
            metadata.received.retain(|cursor| {
                cursor.peer != peer && cursor.timestamp_ms >= now_ms.saturating_sub(RETENTION_MS)
            });
            if metadata.received.len() >= 1024 {
                return Err("replay cursor limit reached".into());
            }
            metadata.received.push(Cursor {
                peer,
                sequence: update.sequence,
                timestamp_ms: update.timestamp_ms,
            });
            self.write_metadata(&metadata)?;
        }
        let mut memory = self.0.memory.lock().map_err(|_| "sharing lock failed")?;
        let remote = memory.remote.entry(peer).or_default();
        match message {
            Message::LocationUpdate(update) => {
                remote.latest = Some(*update);
                remote.received_at_ms = now_ms;
            }
            Message::SharingStatus(status) => remote.availability = *status,
            Message::PermissionUpdate { allowed } => remote.permission = *allowed,
            Message::Hello => return Err("unexpected hello".into()),
            _ => {}
        }
        Ok(true)
    }

    pub fn snapshots(&self, now_ms: i64) -> Result<Vec<PeerLocation>> {
        let _lock = self.0.friends.lock()?;
        let records = self.0.friends.list_locked()?;
        let mut memory = self.0.memory.lock().map_err(|_| "sharing lock failed")?;
        memory.remote.retain(|peer, _| trusted(&records, *peer));
        let mut snapshots = Vec::new();
        for (peer, remote) in &mut memory.remote {
            if now_ms.saturating_sub(remote.received_at_ms) > RETENTION_MS {
                remote.latest = None;
            }
            let age_ms = remote
                .latest
                .map(|location| now_ms.saturating_sub(location.timestamp_ms).max(0));
            snapshots.push(PeerLocation {
                peer_id: *peer,
                latest: remote.latest,
                age_ms,
                stale: age_ms.is_none_or(|age| age > MAX_FIX_AGE_MS),
                connected: false,
                sharing_active: remote.permission && remote.availability == Availability::Active,
            });
        }
        for snapshot in &mut snapshots {
            snapshot.connected = memory
                .sessions
                .get(&snapshot.peer_id)
                .is_some_and(|(connection, _)| connection.close_reason().is_none());
            snapshot.sharing_active &= snapshot.connected;
        }
        Ok(snapshots)
    }

    pub(super) fn has_location(&self, peer: EndpointId, now_ms: i64) -> Result<bool> {
        let memory = self.0.memory.lock().map_err(|_| "sharing lock failed")?;
        Ok(memory.remote.get(&peer).is_some_and(|remote| {
            remote.latest.is_some() && now_ms.saturating_sub(remote.received_at_ms) <= RETENTION_MS
        }))
    }

    /// Both friends dial each other. Keep connection opened by lower identity so both
    /// sides converge on one session instead of replacing each other indefinitely.
    pub(super) fn register(&self, connection: &Connection, initiator: bool) -> Result<()> {
        let _lock = self.0.friends.lock()?;
        let peer = connection.remote_id();
        if !trusted(&self.0.friends.list_locked()?, peer) {
            return Err("peer not trusted".into());
        }
        let preferred = initiator == (self.owner() < peer);
        let mut memory = self.0.memory.lock().map_err(|_| "sharing lock failed")?;
        if let Some((old, old_preferred)) = memory.sessions.get(&peer)
            && old.close_reason().is_none()
            && *old_preferred
            && !preferred
        {
            return Err("duplicate location session".into());
        }
        // New session starts fresh: an update sent on a replaced connection may be lost.
        memory.sent.remove(&peer);
        if let Some((old, _)) = memory
            .sessions
            .insert(peer, (connection.clone(), preferred))
        {
            old.close(0u32.into(), b"session replaced");
        }
        Ok(())
    }

    /// True while an authenticated session with `peer` is open.
    pub fn connected(&self, peer: EndpointId) -> bool {
        self.0.memory.lock().is_ok_and(|memory| {
            memory
                .sessions
                .get(&peer)
                .is_some_and(|(connection, _)| connection.close_reason().is_none())
        })
    }

    pub fn set_trip_source(&self, source: TripSource) {
        if let Ok(mut trips) = self.0.trips.lock() {
            *trips = Some(source);
        }
    }

    /// Own finished trips for `peer`, only while sharing is on and peer may see location.
    pub(super) fn trips_for(&self, peer: EndpointId) -> Result<Vec<u8>> {
        {
            let _lock = self.0.friends.lock()?;
            let records = self.0.friends.list_locked()?;
            let groups = self.0.friends.groups_locked()?;
            let allowed = self.read_metadata()?.mode != Mode::Off
                && records.iter().any(|f| {
                    f.peer_id == peer && f.effective_permission(&groups) != Permission::Off
                });
            if !allowed {
                return Err("trips not shared with this friend".into());
            }
        }
        let source = self
            .0
            .trips
            .lock()
            .map_err(|_| "sharing lock failed")?
            .clone()
            .ok_or("trips unavailable")?;
        source()
    }

    /// Group member lists shared with `peer`, JSON for a reliable stream.
    pub(super) fn group_sync(&self, peer: EndpointId) -> Result<Vec<u8>> {
        let _lock = self.0.friends.lock()?;
        Ok(serde_json::to_vec(&self.0.friends.sync_for_locked(peer)?)?)
    }

    /// Applies member lists from an authenticated session; wakes dialers on change.
    pub(super) fn apply_group_sync(&self, peer: EndpointId, bytes: &[u8]) -> Result<()> {
        let syncs: Vec<GroupSync> = serde_json::from_slice(bytes)?;
        let changed = {
            let _lock = self.0.friends.lock()?;
            if !trusted(&self.0.friends.list_locked()?, peer) {
                return Err("peer not trusted".into());
            }
            self.0.friends.apply_sync_locked(peer, &syncs)?
        };
        if changed {
            self.0.changed.send_replace(());
        }
        Ok(())
    }

    /// Selected network path of open session: "direct" or "relay". Never addresses.
    pub fn path(&self, peer: EndpointId) -> Option<&'static str> {
        let memory = self.0.memory.lock().ok()?;
        let (connection, _) = memory.sessions.get(&peer)?;
        let paths = connection.paths();
        let selected = paths.iter().find(|path| path.is_selected())?;
        if selected.is_ip() {
            Some("direct")
        } else if selected.is_relay() {
            Some("relay")
        } else {
            None
        }
    }

    /// Wake sessions after another process changed friends or settings on disk.
    pub fn notify(&self) {
        self.0.changed.send_replace(());
    }

    pub(super) fn unregister(&self, connection: &Connection) {
        if let Ok(mut memory) = self.0.memory.lock()
            && memory
                .sessions
                .get(&connection.remote_id())
                .is_some_and(|(active, _)| active.stable_id() == connection.stable_id())
        {
            memory.sessions.remove(&connection.remote_id());
        }
    }
}
