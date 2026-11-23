//! Sender policy, latest-only samples, replay protection, and volatile friend positions.
use super::{
    Result,
    groups::{GroupId, Packet},
    location::{MAX_FIX_AGE_MS, RETENTION_MS, Sample, UpdatePolicy},
    peers::{Friend, Friends, Group, Permission},
    protocol::{
        self, Availability, GroupFix, GroupPayload, LocationUpdate, Message, SignedGroupFix,
    },
};
use iroh::{EndpointId, SecretKey, endpoint::Connection};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::File,
    io::{Read, Write},
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};
use tokio::sync::watch;

/// Friend counts as reachable this long after last message; sessions close when idle.
pub const HEARD_MS: i64 = 40 * 60_000;
/// A friend with Roamlog closed is reachable only at its next check-in (every 5 minutes).
const LOCATE_WAIT_MS: i64 = 7 * 60_000;

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
    #[serde(default)]
    expires_at_ms: Option<i64>,
    #[serde(default)]
    share_battery: bool,
}

#[derive(Clone, Serialize)]
pub struct PeerLocation {
    pub peer_id: EndpointId,
    pub latest: Option<LocationUpdate>,
    pub battery: Option<protocol::BatteryStatus>,
    pub via: Option<EndpointId>,
    pub withdrawn: bool,
    pub age_ms: Option<i64>,
    pub stale: bool,
    pub connected: bool,
    pub sharing_active: bool,
}

struct Remote {
    sequence: u64,
    group: Option<GroupId>,
    via: Option<EndpointId>,
    withdrawn: bool,
    latest: Option<LocationUpdate>,
    received_at_ms: i64,
    availability: Availability,
    battery: Option<protocol::BatteryStatus>,
    permission: bool,
    heard_ms: i64,
}
impl Default for Remote {
    fn default() -> Self {
        Self {
            sequence: 0,
            group: None,
            via: None,
            withdrawn: false,
            latest: None,
            received_at_ms: 0,
            availability: Availability::Off,
            battery: None,
            permission: false,
            heard_ms: 0,
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
struct CachedFix {
    signed: SignedGroupFix,
    via: EndpointId,
}

#[derive(Default)]
struct Memory {
    latest: Option<Sample>,
    battery: Option<protocol::BatteryStatus>,
    battery_sent: BTreeMap<EndpointId, (Option<u8>, i64)>,
    refresh_until_ms: i64,
    refresh_requests: BTreeMap<EndpointId, i64>,
    last_refresh_ms: Option<i64>,
    update_required: BTreeSet<EndpointId>,
    recording: bool,
    relay: Vec<CachedFix>,
    group_issued: BTreeMap<GroupId, (crate::geo::Point, i64)>,
    group_sent: BTreeMap<(EndpointId, GroupId, EndpointId), u64>,
    sent: BTreeMap<EndpointId, (crate::geo::Point, i64)>,
    /// Timestamp of the fix last sent; a refresh is answered once a newer fix went out.
    sent_fix: BTreeMap<EndpointId, u64>,
    remote: BTreeMap<EndpointId, Remote>,
    /// Live session per friend and whether lower identity opened it (preferred on both sides).
    sessions: BTreeMap<EndpointId, (Connection, bool)>,
    /// Last status and group state each friend was told; a change needs a session.
    status_sent: BTreeMap<EndpointId, (bool, Availability)>,
    group_synced: BTreeMap<EndpointId, Vec<Vec<u8>>>,
}

/// Produces own finished trips as JSON for friends allowed to view them.
pub type RefreshSource = Arc<dyn Fn() -> Result<()> + Send + Sync>;

pub type TripSource = Arc<dyn Fn() -> Result<Vec<u8>> + Send + Sync>;

struct Inner {
    key: SecretKey,
    friends: Friends,
    memory: Mutex<Memory>,
    changed: watch::Sender<()>,
    trips: Mutex<Option<TripSource>>,
    refresh: Mutex<Option<RefreshSource>>,
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
    pub fn new(friends: Friends, key: SecretKey) -> Result<Self> {
        if key.public() != friends.owner() {
            return Err("sharing signing identity mismatch".into());
        }
        let manager = Self(Arc::new(Inner {
            key,
            friends,
            memory: Mutex::new(Memory::default()),
            changed: watch::channel(()).0,
            trips: Mutex::new(None),
            refresh: Mutex::new(None),
        }));
        {
            let _lock = manager.0.friends.lock()?;
            manager.read_metadata()?;
            manager
                .0
                .memory
                .lock()
                .map_err(|_| "sharing lock failed")?
                .relay = manager.read_relay()?;
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
                    expires_at_ms: None,
                    share_battery: false,
                });
            }
            Err(error) => return Err(error.into()),
        };
        let mut bytes = Vec::new();
        file.take(262145).read_to_end(&mut bytes)?;
        if bytes.len() > 262144 {
            return Err("sharing metadata too large".into());
        }
        let mut metadata: Metadata = serde_json::from_slice(&bytes)?;
        if metadata.version != 1 || metadata.owner != self.owner() || metadata.received.len() > 1024
        {
            return Err("invalid sharing metadata".into());
        }
        metadata.policy.validate()?;
        // Not user-set any more; older files may hold a chosen interval.
        metadata.policy = UpdatePolicy::default();
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
        if metadata
            .expires_at_ms
            .is_some_and(|until| now_ms() >= until)
        {
            metadata.mode = Mode::Off;
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
        metadata.expires_at_ms = None;
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

    pub fn set_timed(&self, minutes: u32) -> Result<()> {
        if !(1..=525_600).contains(&minutes) {
            return Err("sharing duration must be 1 minute–365 days".into());
        }
        let _lock = self.0.friends.lock()?;
        let mut metadata = self.read_metadata()?;
        metadata.mode = Mode::Always;
        metadata.expires_at_ms = Some(
            now_ms()
                .checked_add(i64::from(minutes) * 60_000)
                .ok_or("invalid expiry")?,
        );
        self.write_metadata(&metadata)?;
        self.notify();
        Ok(())
    }

    pub fn expires_at_ms(&self) -> Result<Option<i64>> {
        let _lock = self.0.friends.lock()?;
        Ok(self
            .read_metadata()?
            .expires_at_ms
            .filter(|until| *until > now_ms()))
    }

    pub(super) fn expire_timer(&self, now: i64) -> Result<bool> {
        let _lock = self.0.friends.lock()?;
        let mut metadata = self.read_metadata()?;
        if metadata.expires_at_ms.is_none_or(|until| now < until) {
            return Ok(false);
        }
        metadata.mode = Mode::Off;
        metadata.expires_at_ms = None;
        self.write_metadata(&metadata)?;
        self.notify();
        Ok(true)
    }

    pub(super) fn poll_requests(&self, now: i64) -> Result<()> {
        let _lock = self.0.friends.lock()?;
        let records = self.0.friends.list_locked()?;
        let metadata = self.read_metadata()?;
        let memory = self.0.memory.lock().map_err(|_| "sharing lock failed")?;
        for friend in records.iter().filter(|f| !f.blocked) {
            let peer = friend.peer_id;
            let Some(mut request) = super::requests::read(&self.0.friends, peer) else {
                continue;
            };
            if request.status != "Locating…" {
                continue;
            }
            if memory
                .remote
                .get(&peer)
                .and_then(|r| r.latest)
                // Fix time is the friend's clock; allow a few seconds of skew between phones.
                .is_some_and(|fix| fix.timestamp_ms >= request.at_ms - 5_000)
            {
                request.status = "Fresh location received".into();
            } else if request.attempts == 0 && now.saturating_sub(request.at_ms) >= LOCATE_WAIT_MS {
                request.status = "Not connected; try again when online".into();
            } else if request.attempts > 0 && now.saturating_sub(request.sent_at_ms) >= 20_000 {
                request.status = "No fresh location received".into();
            } else if metadata.mode == Mode::Off {
                request.status = "Enable sharing connections first".into();
            } else if let Some((connection, _)) = memory
                .sessions
                .get(&peer)
                .filter(|(c, _)| c.close_reason().is_none())
            {
                if request.attempts < 3
                    && (request.attempts == 0 || now.saturating_sub(request.sent_at_ms) >= 4000)
                {
                    connection
                        .send_datagram(protocol::encode(&Message::LocationRequest)?.into())?;
                    request.attempts += 1;
                    request.sent_at_ms = now;
                }
            } else {
                // Session opens on demand; dialer is connecting now.
                continue;
            }
            super::requests::write(&self.0.friends, peer, &request)?;
        }
        Ok(())
    }

    pub fn shares_battery(&self) -> Result<bool> {
        let _lock = self.0.friends.lock()?;
        Ok(self.read_metadata()?.share_battery)
    }

    pub fn set_share_battery(&self, enabled: bool) -> Result<()> {
        let _lock = self.0.friends.lock()?;
        let mut metadata = self.read_metadata()?;
        metadata.share_battery = enabled;
        self.write_metadata(&metadata)?;
        self.notify();
        Ok(())
    }

    pub fn update_battery(&self, percent: Option<u8>, now: i64) -> Result<()> {
        if percent.is_some_and(|p| p > 100) || now <= 0 {
            return Err("invalid battery sample".into());
        }
        self.0
            .memory
            .lock()
            .map_err(|_| "sharing lock failed")?
            .battery = Some(protocol::BatteryStatus {
            percent,
            timestamp_ms: now,
        });
        Ok(())
    }

    pub fn set_refresh_source(&self, source: RefreshSource) {
        if let Ok(mut refresh) = self.0.refresh.lock() {
            *refresh = Some(source);
        }
    }

    /// Request fresh GPS only under current sender consent; limit each peer and total work.
    pub(super) fn refresh_requested(&self, peer: EndpointId, now: i64) -> Result<bool> {
        {
            let _lock = self.0.friends.lock()?;
            let records = self.0.friends.list_locked()?;
            let groups = self.0.friends.groups_locked()?;
            let metadata = self.read_metadata()?;
            let mut memory = self.0.memory.lock().map_err(|_| "sharing lock failed")?;
            if !availability(&records, &groups, peer, metadata.mode, &memory, now).0
                || memory
                    .refresh_requests
                    .get(&peer)
                    .is_some_and(|last| now.saturating_sub(*last) < 60_000)
                || memory
                    .last_refresh_ms
                    .is_some_and(|last| now.saturating_sub(last) < 15_000)
            {
                return Ok(false);
            }
            memory.refresh_requests.retain(|p, _| trusted(&records, *p));
            memory.refresh_requests.insert(peer, now);
            memory.last_refresh_ms = Some(now);
            memory.refresh_until_ms = now.saturating_add(20_000);
        }
        let source = self
            .0
            .refresh
            .lock()
            .map_err(|_| "sharing lock failed")?
            .clone();
        if let Some(source) = source {
            source()?;
        }
        self.notify();
        Ok(true)
    }

    pub(super) fn send_battery(&self, connection: &Connection, now: i64) -> Result<()> {
        let _lock = self.0.friends.lock()?;
        let records = self.0.friends.list_locked()?;
        if !trusted(&records, connection.remote_id()) {
            return Err("peer not trusted".into());
        }
        let groups = self.0.friends.groups_locked()?;
        let metadata = self.read_metadata()?;
        let mut memory = self.0.memory.lock().map_err(|_| "sharing lock failed")?;
        let allowed = metadata.share_battery
            && availability(
                &records,
                &groups,
                connection.remote_id(),
                metadata.mode,
                &memory,
                now,
            )
            .0;
        let percent = if allowed {
            memory
                .battery
                .filter(|b| now.saturating_sub(b.timestamp_ms) <= 120_000)
                .and_then(|b| b.percent)
        } else {
            None
        };
        let peer = connection.remote_id();
        if memory
            .battery_sent
            .get(&peer)
            .is_some_and(|(last, sent)| *last == percent && now.saturating_sub(*sent) < 300_000)
        {
            return Ok(());
        }
        connection.send_datagram(
            protocol::encode(&Message::BatteryStatus(protocol::BatteryStatus {
                percent,
                timestamp_ms: now,
            }))?
            .into(),
        )?;
        memory.battery_sent.insert(peer, (percent, now));
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
        if matches!(
            message,
            Message::LocationUpdate(_) | Message::GroupFix(_) | Message::Hello
        ) {
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
        let requested = requested || now_ms < memory.refresh_until_ms;
        self.prepare_group_fixes(
            &records,
            &groups,
            &mut metadata,
            &mut memory,
            now_ms,
            requested,
        )?;
        let group_sent =
            self.forward_group_fixes(connection, &records, &metadata, &mut memory, now_ms)?;
        if availability(&records, &groups, peer, metadata.mode, &memory, now_ms)
            != (true, Availability::Active)
        {
            return Ok(false);
        }
        let sample = memory.latest.ok_or("location unavailable")?;
        if let Some(update) = group_sent
            && sample.to_update(update.sequence, now_ms)? == update
        {
            // Signed group datagram already carries this fix. A second, newer direct
            // sequence would race it and could prevent the recipient caching relay data.
            memory.sent.insert(peer, (sample.fix.point, now_ms));
            memory.sent_fix.insert(peer, sample.fix.timestamp_ms);
            return Ok(true);
        }
        let force = memory.last_refresh_ms.is_some_and(|start| {
            now_ms < memory.refresh_until_ms
                && sample.fix.timestamp_ms >= start as u64
                && memory
                    .sent_fix
                    .get(&peer)
                    .is_none_or(|sent| *sent < start as u64)
        });
        if !force
            && !metadata
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
        memory.sent_fix.insert(peer, sample.fix.timestamp_ms);
        Ok(true)
    }

    fn read_relay(&self) -> Result<Vec<CachedFix>> {
        let file = match File::open(self.0.friends.directory().join("group-fixes.postcard")) {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
            Err(e) => return Err(e.into()),
        };
        let mut bytes = Vec::new();
        file.take(1_048_577).read_to_end(&mut bytes)?;
        if bytes.len() > 1_048_576 {
            return Err("group fix cache too large".into());
        }
        let fixes: Vec<CachedFix> = postcard::from_bytes(&bytes)?;
        if fixes.len() > 32 * 64 {
            return Err("too many cached group fixes".into());
        }
        Ok(fixes
            .into_iter()
            .filter(|f| f.signed.validate(now_ms()).is_ok())
            .collect())
    }

    fn write_relay(&self, fixes: &[CachedFix]) -> Result<()> {
        if fixes.len() > 32 * 64 {
            return Err("too many cached group fixes".into());
        }
        let mut file = tempfile::NamedTempFile::new_in(self.0.friends.directory())?;
        file.write_all(&postcard::to_allocvec(fixes)?)?;
        file.as_file().sync_all()?;
        file.persist(self.0.friends.directory().join("group-fixes.postcard"))?;
        File::open(self.0.friends.directory())?.sync_all()?;
        Ok(())
    }

    /// Origin signs only current, locally authorized group grants. Existing counters
    /// reserve numbers before persistence/enqueue, shared with direct locations.
    fn prepare_group_fixes(
        &self,
        records: &[Friend],
        groups: &[Group],
        metadata: &mut Metadata,
        memory: &mut Memory,
        now: i64,
        requested: bool,
    ) -> Result<()> {
        if !groups.iter().any(|g| g.version == 2) {
            return Ok(());
        }
        let views = self.0.friends.group_views_locked()?;
        let mut relay = memory.relay.clone();
        let mut issued = memory.group_issued.clone();
        issued.retain(|id, _| {
            views
                .iter()
                .any(|(g, _)| g.id == super::groups::id_string(*id))
        });
        let before = relay.len();
        relay.retain(|cached| {
            cached.signed.fix.timestamp_ms >= now.saturating_sub(RETENTION_MS)
                && views.iter().any(|(g, s)| {
                    g.id == super::groups::id_string(cached.signed.fix.group)
                        && s.members.contains_key(&cached.signed.fix.origin)
                })
        });
        let mut changed = before != relay.len();
        for (group, state) in &views {
            let id = super::groups::parse_id(&group.id)?;
            if !state.members.contains_key(&self.owner()) {
                continue;
            }
            let blocked = state
                .members
                .keys()
                .any(|peer| records.iter().any(|f| f.peer_id == *peer && f.blocked));
            if blocked {
                let len = relay.len();
                relay.retain(|c| c.signed.fix.group != id || c.signed.fix.origin != self.owner());
                issued.remove(&id);
                changed |= len != relay.len();
                continue;
            }
            let status = if metadata.mode == Mode::Off
                || group.share == Permission::Off
                || group.leaving_since_ms.is_some()
            {
                Availability::Off
            } else if !memory.recording
                && (metadata.mode == Mode::TripsOnly || group.share == Permission::TripsOnly)
            {
                Availability::TripInactive
            } else if memory.latest.is_none_or(|sample| {
                sample.to_update(1, now).is_err()
                    || sample.fix.timestamp_ms < now.saturating_sub(MAX_FIX_AGE_MS) as u64
            }) {
                Availability::NoFix
            } else {
                Availability::Active
            };
            // Missing GPS does not revoke access. Preserve last-known mailbox data.
            if status == Availability::NoFix {
                continue;
            }
            let old = relay
                .iter()
                .find(|c| c.signed.fix.group == id && c.signed.fix.origin == self.owner());
            let payload = if status == Availability::Active {
                let sample = memory.latest.ok_or("location unavailable")?;
                let force = memory.last_refresh_ms.is_some_and(|start| {
                    now < memory.refresh_until_ms
                        && sample.fix.timestamp_ms >= start as u64
                        && issued.get(&id).is_none_or(|(_, sent)| *sent <= start)
                });
                if !force
                    && !metadata
                        .policy
                        .due(issued.get(&id).copied(), sample, now, requested)
                {
                    continue;
                }
                metadata.sequence = metadata
                    .sequence
                    .checked_add(1)
                    .ok_or("sharing sequence exhausted")?;
                issued.insert(id, (sample.fix.point, now));
                GroupPayload::Fix(sample.to_update(metadata.sequence, now)?)
            } else {
                if !old.is_some_and(|c| matches!(c.signed.fix.payload, GroupPayload::Fix(_))) {
                    continue;
                }
                metadata.sequence = metadata
                    .sequence
                    .checked_add(1)
                    .ok_or("sharing sequence exhausted")?;
                issued.remove(&id);
                GroupPayload::Unavailable(status)
            };
            let timestamp_ms = match payload {
                GroupPayload::Fix(update) => update.timestamp_ms,
                _ => now,
            };
            let signed = SignedGroupFix::sign(
                &self.0.key,
                GroupFix {
                    group: id,
                    origin: self.owner(),
                    sequence: metadata.sequence,
                    timestamp_ms,
                    payload,
                },
            )?;
            relay.retain(|c| c.signed.fix.group != id || c.signed.fix.origin != self.owner());
            relay.push(CachedFix {
                signed,
                via: self.owner(),
            });
            changed = true;
        }
        if changed {
            self.write_metadata(metadata)?;
            self.write_relay(&relay)?;
            self.0.changed.send_replace(());
        }
        memory.relay = relay;
        memory.group_issued = issued;
        Ok(())
    }

    fn forward_group_fixes(
        &self,
        connection: &Connection,
        records: &[Friend],
        metadata: &Metadata,
        memory: &mut Memory,
        now: i64,
    ) -> Result<Option<LocationUpdate>> {
        let peer = connection.remote_id();
        if !trusted(records, peer) {
            return Ok(None);
        }
        let mut own: Option<LocationUpdate> = None;
        let views = self.0.friends.group_views_locked()?;
        for cached in &memory.relay {
            let fix = &cached.signed.fix;
            if peer == fix.origin
                || (fix.origin != self.owner() && !trusted(records, fix.origin))
                || fix.timestamp_ms < now.saturating_sub(RETENTION_MS)
            {
                continue;
            }
            // Off still permits best-effort delivery of own signed withdrawal.
            if metadata.mode == Mode::Off
                && !(fix.origin == self.owner()
                    && matches!(fix.payload, GroupPayload::Unavailable(_)))
            {
                continue;
            }
            let Some((_, state)) = views
                .iter()
                .find(|(g, _)| g.id == super::groups::id_string(fix.group))
            else {
                continue;
            };
            if !state.members.contains_key(&fix.origin)
                || !state.members.contains_key(&self.owner())
            {
                continue;
            }
            let Some(member) = state.members.get(&peer) else {
                continue;
            };
            if member.admitted_at_ms > fix.timestamp_ms as u64 {
                continue;
            }
            let sent = memory
                .group_sent
                .entry((peer, fix.group, fix.origin))
                .or_default();
            if fix.sequence <= *sent {
                continue;
            }
            connection.send_datagram(
                protocol::encode(&Message::GroupFix(cached.signed.clone()))?.into(),
            )?;
            *sent = fix.sequence;
            if fix.origin == self.owner()
                && let GroupPayload::Fix(update) = fix.payload
                && own.is_none_or(|previous| previous.sequence < update.sequence)
            {
                own = Some(update);
            }
        }
        Ok(own)
    }

    /// Best effort before service closes its endpoint, including Off/Pause transitions.
    pub(super) fn flush_group_fixes(&self) {
        let flush = || -> Result<()> {
            let _lock = self.0.friends.lock()?;
            let records = self.0.friends.list_locked()?;
            let groups = self.0.friends.groups_locked()?;
            let mut metadata = self.read_metadata()?;
            let mut memory = self.0.memory.lock().map_err(|_| "sharing lock failed")?;
            let now = now_ms();
            self.prepare_group_fixes(&records, &groups, &mut metadata, &mut memory, now, false)?;
            let connections: Vec<_> = memory.sessions.values().map(|(c, _)| c.clone()).collect();
            for connection in connections {
                let _ =
                    self.forward_group_fixes(&connection, &records, &metadata, &mut memory, now)?;
            }
            Ok(())
        };
        let _ = flush();
    }

    fn ingest_group_locked(
        &self,
        peer: EndpointId,
        signed: &SignedGroupFix,
        now: i64,
    ) -> Result<bool> {
        signed.validate(now)?;
        let fix = &signed.fix;
        if fix.origin == self.owner() {
            return Ok(false);
        }
        let records = self.0.friends.list_locked()?;
        if !trusted(&records, fix.origin) {
            return Ok(false);
        }
        let views = self.0.friends.group_views_locked()?;
        let Some((_, state)) = views
            .iter()
            .find(|(g, _)| g.id == super::groups::id_string(fix.group))
        else {
            return Ok(false);
        };
        if !state.members.contains_key(&fix.origin)
            || !state.members.contains_key(&peer)
            || state
                .members
                .get(&self.owner())
                .is_none_or(|m| m.admitted_at_ms > fix.timestamp_ms as u64)
        {
            // A valid datagram may have been queued before removal/leave arrived.
            // Drop it without closing the session that carries membership gossip.
            return Ok(false);
        }
        let mut metadata = self.read_metadata()?;
        if metadata
            .received
            .iter()
            .any(|c| c.peer == fix.origin && c.sequence >= fix.sequence)
        {
            return Ok(false);
        }
        metadata
            .received
            .retain(|c| c.peer != fix.origin && c.timestamp_ms >= now.saturating_sub(RETENTION_MS));
        if metadata.received.len() >= 1024 {
            return Err("replay cursor limit reached".into());
        }
        metadata.received.push(Cursor {
            peer: fix.origin,
            sequence: fix.sequence,
            timestamp_ms: fix.timestamp_ms,
        });
        self.write_metadata(&metadata)?;
        let mut memory = self.0.memory.lock().map_err(|_| "sharing lock failed")?;
        memory.relay.retain(|c| {
            (c.signed.fix.group, c.signed.fix.origin) != (fix.group, fix.origin)
                && c.signed.fix.timestamp_ms >= now.saturating_sub(RETENTION_MS)
        });
        memory.relay.push(CachedFix {
            signed: signed.clone(),
            via: peer,
        });
        self.write_relay(&memory.relay)?;
        Self::apply_cached(
            &mut memory,
            &CachedFix {
                signed: signed.clone(),
                via: peer,
            },
            now,
        );
        self.0.changed.send_replace(());
        Ok(true)
    }

    fn apply_cached(memory: &mut Memory, cached: &CachedFix, now: i64) {
        let fix = &cached.signed.fix;
        let remote = memory.remote.entry(fix.origin).or_default();
        if fix.sequence <= remote.sequence {
            return;
        }
        remote.sequence = fix.sequence;
        remote.group = Some(fix.group);
        remote.via = (cached.via != fix.origin).then_some(cached.via);
        remote.received_at_ms = now;
        match fix.payload {
            GroupPayload::Fix(update) => {
                remote.latest = Some(update);
                remote.withdrawn = false;
            }
            GroupPayload::Unavailable(_) => {
                remote.latest = None;
                remote.withdrawn = true;
            }
        }
    }

    pub(super) fn ingest(&self, peer: EndpointId, message: &Message, now_ms: i64) -> Result<bool> {
        let _lock = self.0.friends.lock()?;
        if !trusted(&self.0.friends.list_locked()?, peer) {
            return Err("peer not trusted".into());
        }
        if let Message::GroupFix(signed) = message {
            return self.ingest_group_locked(peer, signed, now_ms);
        }
        if let Message::LocationUpdate(update) = message {
            update.validate(now_ms)?;
            let mut metadata = self.read_metadata()?;
            if metadata
                .received
                .iter()
                .any(|cursor| cursor.peer == peer && update.sequence <= cursor.sequence)
            {
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
        remote.heard_ms = now_ms;
        match message {
            Message::LocationUpdate(update) => {
                remote.sequence = update.sequence;
                remote.group = None;
                remote.via = None;
                remote.withdrawn = false;
                remote.latest = Some(*update);
                remote.received_at_ms = now_ms;
            }
            Message::BatteryStatus(status) => {
                status.validate(now_ms)?;
                if remote
                    .battery
                    .is_none_or(|old| status.timestamp_ms >= old.timestamp_ms)
                {
                    remote.battery = Some(*status);
                }
            }
            Message::SharingStatus(status) => {
                remote.availability = *status;
                if matches!(status, Availability::Off | Availability::TripInactive) {
                    remote.battery = None;
                }
            }
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
        let views = self.0.friends.group_views_locked()?;
        let relay = memory.relay.clone();
        let metadata = self.read_metadata()?;
        for cached in &relay {
            let fix = &cached.signed.fix;
            if fix.origin != self.owner()
                && !metadata
                    .received
                    .iter()
                    .any(|c| c.peer == fix.origin && c.sequence > fix.sequence)
                && trusted(&records, fix.origin)
                && fix.timestamp_ms >= now_ms.saturating_sub(RETENTION_MS)
                && views.iter().any(|(g, s)| {
                    g.id == super::groups::id_string(fix.group)
                        && s.members.contains_key(&fix.origin)
                        && s.members.contains_key(&self.owner())
                })
            {
                Self::apply_cached(&mut memory, cached, now_ms);
            }
        }
        for (peer, remote) in &mut memory.remote {
            if remote.group.is_some_and(|id| {
                !views.iter().any(|(g, s)| {
                    g.id == super::groups::id_string(id)
                        && s.members.contains_key(peer)
                        && s.members.contains_key(&self.owner())
                })
            }) {
                remote.latest = None;
                remote.withdrawn = true;
            }
        }
        let mut snapshots = Vec::new();
        for (peer, remote) in &mut memory.remote {
            if now_ms.saturating_sub(remote.received_at_ms) > RETENTION_MS
                || remote
                    .latest
                    .is_some_and(|l| now_ms.saturating_sub(l.timestamp_ms) > RETENTION_MS)
            {
                remote.latest = None;
            }
            let age_ms = remote
                .latest
                .map(|location| now_ms.saturating_sub(location.timestamp_ms).max(0));
            snapshots.push(PeerLocation {
                peer_id: *peer,
                latest: remote.latest,
                battery: remote
                    .battery
                    .filter(|b| now_ms.saturating_sub(b.timestamp_ms) <= MAX_FIX_AGE_MS),
                via: remote.via,
                withdrawn: remote.withdrawn,
                age_ms,
                stale: age_ms.is_none_or(|age| age > MAX_FIX_AGE_MS),
                connected: false,
                sharing_active: remote.permission && remote.availability == Availability::Active,
            });
        }
        for snapshot in &mut snapshots {
            let heard = memory
                .remote
                .get(&snapshot.peer_id)
                .is_some_and(|r| now_ms.saturating_sub(r.heard_ms) <= HEARD_MS);
            snapshot.connected = heard
                || memory
                    .sessions
                    .get(&snapshot.peer_id)
                    .is_some_and(|(connection, _)| connection.close_reason().is_none());
            snapshot.sharing_active &= snapshot.connected;
        }
        Ok(snapshots)
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
        memory.update_required.remove(&peer);
        memory.sent.remove(&peer);
        memory.sent_fix.remove(&peer);
        memory.battery_sent.remove(&peer);
        memory.group_sent.retain(|(p, _, _), _| *p != peer);
        if let Some((old, _)) = memory
            .sessions
            .insert(peer, (connection.clone(), preferred))
        {
            old.close(0u32.into(), b"session replaced");
        }
        Ok(())
    }

    pub(super) fn require_update(&self, peer: EndpointId) {
        if let Ok(mut memory) = self.0.memory.lock() {
            memory.update_required.insert(peer);
        }
        self.notify();
    }

    pub fn update_required(&self, peer: EndpointId) -> bool {
        self.0
            .memory
            .lock()
            .is_ok_and(|m| m.update_required.contains(&peer))
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

    /// Signed operation sets shared with derived members, over bounded reliable streams.
    pub(super) fn group_sync(&self, peer: EndpointId) -> Result<Vec<Vec<u8>>> {
        let _lock = self.0.friends.lock()?;
        self.group_sync_locked(peer)
    }

    fn group_sync_locked(&self, peer: EndpointId) -> Result<Vec<Vec<u8>>> {
        self.0
            .friends
            .sync_for_locked(peer)?
            .into_iter()
            .map(|sync| Ok(postcard::to_allocvec(&Packet::Ops(vec![sync]))?))
            .collect()
    }

    /// Session delivered this status and group state to `peer`.
    pub(super) fn mark_told(
        &self,
        peer: EndpointId,
        status: (bool, Availability),
        sync: &[Vec<u8>],
    ) -> Result<()> {
        let mut memory = self.0.memory.lock().map_err(|_| "sharing lock failed")?;
        memory.status_sent.insert(peer, status);
        memory.group_synced.insert(peer, sync.to_vec());
        Ok(())
    }

    /// Something for `peer` waits on a session: due location, status or group change,
    /// or a pending "Try to locate". Otherwise no connection is held open.
    pub(super) fn wants_session(&self, peer: EndpointId, now: i64) -> Result<bool> {
        let _lock = self.0.friends.lock()?;
        let records = self.0.friends.list_locked()?;
        if !trusted(&records, peer) {
            return Ok(false);
        }
        let groups = self.0.friends.groups_locked()?;
        let metadata = self.read_metadata()?;
        let sync = self.group_sync_locked(peer)?;
        let locating =
            super::requests::read(&self.0.friends, peer).is_some_and(|r| r.status == "Locating…");
        let memory = self.0.memory.lock().map_err(|_| "sharing lock failed")?;
        let status = availability(&records, &groups, peer, metadata.mode, &memory, now);
        let due = status == (true, Availability::Active)
            && memory.latest.is_some_and(|sample| {
                metadata.policy.due(
                    memory.sent.get(&peer).copied(),
                    sample,
                    now,
                    now < memory.refresh_until_ms,
                )
            });
        Ok(locating
            || due
            || memory.status_sent.get(&peer) != Some(&status)
            || memory
                .group_synced
                .get(&peer)
                .map_or(!sync.is_empty(), |told| *told != sync))
    }

    /// Returns an acknowledgement after durable merge, before closing removed sessions.
    pub(super) fn apply_group_sync(
        &self,
        peer: EndpointId,
        bytes: &[u8],
    ) -> Result<Option<Vec<u8>>> {
        let packet: Packet = postcard::from_bytes(bytes)?;
        let (changed, response) = {
            let _lock = self.0.friends.lock()?;
            if !trusted(&self.0.friends.list_locked()?, peer) {
                return Err("peer not trusted".into());
            }
            match packet {
                Packet::Ops(syncs) => {
                    let (changed, acks) = self.0.friends.apply_sync_locked(peer, &syncs)?;
                    (changed, Some(postcard::to_allocvec(&Packet::Ack(acks))?))
                }
                Packet::Ack(acks) => (self.0.friends.acknowledge_locked(peer, &acks)?, None),
            }
        };
        if changed {
            self.0.changed.send_replace(());
        }
        Ok(response)
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
            let peer = connection.remote_id();
            memory.sessions.remove(&peer);
            memory.group_sent.retain(|(p, _, _), _| *p != peer);
        }
    }
}

#[cfg(test)]
mod relay_persistence_tests {
    use super::*;
    use crate::{geo::Point, model::Fix, sharing::groups::Policy};

    #[test]
    fn relay_state_commits_only_after_sequence_and_cache_persist() {
        let dir = tempfile::tempdir().unwrap();
        let key = SecretKey::generate();
        let friends = Friends::open(dir.path(), key.public()).unwrap();
        friends
            .create_group(&key, "Family", "Me", Policy::Managed, Permission::Always)
            .unwrap();
        let manager = Manager::new(friends.clone(), key).unwrap();
        let _lock = friends.lock().unwrap();
        let records = friends.list_locked().unwrap();
        let groups = friends.groups_locked().unwrap();
        let now = now_ms();
        let mut memory = Memory {
            latest: Some(Sample {
                fix: Fix {
                    point: Point {
                        lat: 40.,
                        lon: -111.,
                    },
                    timestamp_ms: now as u64,
                    accuracy_m: 5.,
                },
                speed_m_s: None,
                heading_degrees: None,
            }),
            ..Memory::default()
        };
        let mut metadata = manager.read_metadata().unwrap();
        metadata.mode = Mode::Always;
        let metadata_path = friends.directory().join("location-state.json");
        std::fs::create_dir(&metadata_path).unwrap();
        assert!(
            manager
                .prepare_group_fixes(&records, &groups, &mut metadata, &mut memory, now, true)
                .is_err()
        );
        assert!(memory.relay.is_empty() && memory.group_issued.is_empty());
        std::fs::remove_dir(metadata_path).unwrap();
        // Successful reservation followed by failed cache replacement must skip its
        // number on retry and must not publish uncommitted in-memory relay data.
        let cache_path = friends.directory().join("group-fixes.postcard");
        std::fs::create_dir(&cache_path).unwrap();
        let mut metadata = manager.read_metadata().unwrap();
        metadata.mode = Mode::Always;
        assert!(
            manager
                .prepare_group_fixes(&records, &groups, &mut metadata, &mut memory, now, true)
                .is_err()
        );
        assert!(memory.relay.is_empty() && memory.group_issued.is_empty());
        assert_eq!(manager.read_metadata().unwrap().sequence, 1);
        std::fs::remove_dir(cache_path).unwrap();
        let mut metadata = manager.read_metadata().unwrap();
        manager
            .prepare_group_fixes(&records, &groups, &mut metadata, &mut memory, now, true)
            .unwrap();
        assert_eq!(memory.relay.len(), 1);
        assert_eq!(memory.relay[0].signed.fix.sequence, 2);
        assert_eq!(manager.read_relay().unwrap()[0].signed.fix.sequence, 2);
        assert_eq!(manager.read_metadata().unwrap().sequence, 2);
    }
}

#[cfg(test)]
mod feature_tests {
    use super::*;
    #[test]
    fn timed_sharing_survives_restart_and_expiry_overrides_all_grants() {
        let directory = tempfile::tempdir().unwrap();
        let key = SecretKey::generate();
        let peer = SecretKey::generate().public();
        let friends = Friends::open(directory.path(), key.public()).unwrap();
        friends.add_confirmed(peer, "Friend".into()).unwrap();
        friends.set_permission(peer, Permission::Always).unwrap();
        let manager = Manager::new(friends.clone(), key.clone()).unwrap();
        manager.set_timed(1).unwrap();
        let deadline = manager.expires_at_ms().unwrap().unwrap();
        assert_eq!(manager.mode().unwrap(), Mode::Always);
        drop(manager);
        let manager = Manager::new(friends, key).unwrap();
        assert_eq!(manager.expires_at_ms().unwrap(), Some(deadline));
        {
            let _lock = manager.friends().lock().unwrap();
            let mut metadata = manager.read_metadata().unwrap();
            metadata.expires_at_ms = Some(now_ms() - 1);
            manager.write_metadata(&metadata).unwrap();
        }
        assert_eq!(manager.mode().unwrap(), Mode::Off);
        assert_eq!(
            manager.availability(peer, now_ms()).unwrap(),
            (false, Availability::Off)
        );
        assert!(manager.expire_timer(now_ms()).unwrap());
        assert_eq!(manager.expires_at_ms().unwrap(), None);
        manager.set_timed(60).unwrap();
        manager.set_mode(Mode::TripsOnly).unwrap();
        assert_eq!(manager.expires_at_ms().unwrap(), None);
        assert!(manager.set_timed(0).is_err());
        manager.set_timed(43_200).unwrap();
        assert!(manager.expires_at_ms().unwrap().unwrap() > now_ms() + 29 * 86_400_000);
        assert!(manager.set_timed(525_601).is_err());
    }
}
