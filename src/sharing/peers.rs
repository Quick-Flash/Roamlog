//! Local authorization records, separate from trip history and location data.
use super::Result;
use iroh::EndpointId;
use serde::{Deserialize, Serialize};
use std::{
    fs::{File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

pub(super) const MAX_FILE: u64 = 256 * 1024;
pub(super) const MAX_FRIENDS: usize = 256;
pub(super) const MAX_GROUPS: usize = 32;
/// Keep signed leave available until acknowledgement or seven days.
pub const LEAVE_GRACE_MS: u64 = 7 * 24 * 3600 * 1000;

/// Ordered by how much it shares: Off < TripsOnly < Always.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum Permission {
    #[default]
    Off,
    TripsOnly,
    Always,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Friend {
    pub peer_id: EndpointId,
    pub display_name: String,
    pub permission: Permission,
    pub blocked: bool,
    pub paired_at_ms: u64,
    /// Group IDs this friend belongs to. Membership shares location automatically.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub groups: Vec<String>,
    /// Added by group membership, not direct pairing. Dropped when last group ends.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub group_only: bool,
}

/// Named sharing circle, e.g. family. Every member shares with every other member.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Group {
    /// Hash of signed Create, lowercase hex.
    #[serde(default = "legacy_version")]
    pub version: u8,
    /// Removed peers still need one acknowledged operation-set delivery.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pending_removals: Vec<EndpointId>,
    pub id: String,
    pub name: String,
    /// Own display name shown to members.
    pub me: String,
    /// Members known to have left; never re-added from gossip.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub left: Vec<EndpointId>,
    /// Local user is leaving: tombstone still being delivered.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub leaving_since_ms: Option<u64>,
    /// What every member may see of own location. Local choice; members choose their own.
    #[serde(default = "always")]
    pub share: Permission,
}

fn legacy_version() -> u8 {
    1
}

fn always() -> Permission {
    Permission::Always
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Records {
    pub(super) version: u8,
    pub(super) owner: EndpointId,
    pub(super) friends: Vec<Friend>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(super) groups: Vec<Group>,
}

fn valid_group_id(id: &str) -> bool {
    id.len() == 64
        && id
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

pub(super) fn now() -> Result<u64> {
    Ok(SystemTime::now()
        .duration_since(UNIX_EPOCH)?
        .as_millis()
        .try_into()?)
}

impl Friend {
    /// Highest of this friend's own grant and the grants of active groups they share.
    pub fn effective_permission(&self, groups: &[Group]) -> Permission {
        if self.blocked {
            return Permission::Off;
        }
        self.group_permission(groups).max(self.permission)
    }

    /// Highest grant from active groups this friend belongs to.
    pub fn group_permission(&self, groups: &[Group]) -> Permission {
        groups
            .iter()
            .filter(|g| {
                g.version == 2 && self.groups.contains(&g.id) && g.leaving_since_ms.is_none()
            })
            .map(|g| g.share)
            .max()
            .unwrap_or(Permission::Off)
    }
}

/// Reopens records under a cross-process lock for every operation. No permission cache.
#[derive(Clone)]
pub struct Friends {
    pub(super) directory: PathBuf,
    pub(super) owner: EndpointId,
}

pub(super) struct Lock(File);
impl Drop for Lock {
    fn drop(&mut self) {
        crate::flock::unlock(&self.0);
    }
}

pub(super) fn validate_name(name: &str) -> Result<()> {
    if name.trim().is_empty() || name.len() > 80 || name.chars().any(char::is_control) {
        return Err("friend name must contain 1–80 bytes without control characters".into());
    }
    Ok(())
}

impl Friends {
    pub(super) fn owner(&self) -> EndpointId {
        self.owner
    }
    pub fn open(directory: impl AsRef<Path>, owner: EndpointId) -> Result<Self> {
        let store = Self {
            directory: directory.as_ref().join("sharing"),
            owner,
        };
        std::fs::create_dir_all(&store.directory)?;
        store.list()?;
        Ok(store)
    }

    /// Shared with location metadata and packet authorization. Never await while held.
    pub(super) fn lock(&self) -> Result<Lock> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(self.directory.join("friends.lock"))?;
        crate::flock::lock(&file).map_err(std::io::Error::other)?;
        Ok(Lock(file))
    }

    pub(super) fn read(&self) -> Result<Records> {
        let file = match File::open(self.directory.join("friends.json")) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Records {
                    version: 1,
                    owner: self.owner,
                    friends: vec![],
                    groups: vec![],
                });
            }
            Err(error) => return Err(error.into()),
        };
        let mut bytes = Vec::new();
        file.take(MAX_FILE + 1).read_to_end(&mut bytes)?;
        if bytes.len() as u64 > MAX_FILE {
            return Err("friends file too large".into());
        }
        let mut records: Records = serde_json::from_slice(&bytes)?;
        if records.version != 1
            || records.owner != self.owner
            || records.friends.len() > MAX_FRIENDS
            || records.groups.len() > MAX_GROUPS
        {
            return Err("friends file version, identity, or size mismatch".into());
        }
        let mut seen = std::collections::HashSet::new();
        for friend in &records.friends {
            validate_name(&friend.display_name)?;
            if friend.peer_id == self.owner
                || !seen.insert(friend.peer_id)
                || (friend.blocked && friend.permission != Permission::Off)
                || friend.groups.len() > MAX_GROUPS
                || friend
                    .groups
                    .iter()
                    .any(|id| !records.groups.iter().any(|g| &g.id == id))
            {
                return Err("invalid friend record".into());
            }
        }
        let mut ids = std::collections::HashSet::new();
        for group in &records.groups {
            validate_name(&group.name)?;
            validate_name(&group.me)?;
            if !valid_group_id(&group.id) || !ids.insert(&group.id) || group.left.len() > 1024 {
                return Err("invalid group record".into());
            }
        }
        self.rebuild(&mut records)?;
        Ok(records)
    }

    pub(super) fn write(&self, records: &Records) -> Result<()> {
        let bytes = serde_json::to_vec(records)?;
        if bytes.len() as u64 > MAX_FILE {
            return Err("friends file too large".into());
        }
        let mut file = tempfile::NamedTempFile::new_in(&self.directory)?;
        file.write_all(&bytes)?;
        file.as_file().sync_all()?;
        file.persist(self.directory.join("friends.json"))?;
        File::open(&self.directory)?.sync_all()?;
        Ok(())
    }

    pub fn list(&self) -> Result<Vec<Friend>> {
        let _lock = self.lock()?;
        Ok(self.read()?.friends)
    }

    pub fn directory(&self) -> &Path {
        &self.directory
    }

    /// Caller holds `lock()`; avoids recursive flock during atomic authorization.
    pub(super) fn list_locked(&self) -> Result<Vec<Friend>> {
        Ok(self.read()?.friends)
    }

    /// Only call after both users confirmed this authenticated pairing session.
    /// Re-pairing never changes existing local permissions or unblocks a friend.
    pub(super) fn add_confirmed(&self, peer_id: EndpointId, display_name: String) -> Result<()> {
        validate_name(&display_name)?;
        if peer_id == self.owner {
            return Err("cannot pair with this device".into());
        }
        let _lock = self.lock()?;
        let mut records = self.read()?;
        if let Some(friend) = records
            .friends
            .iter_mut()
            .find(|friend| friend.peer_id == peer_id)
        {
            if friend.blocked {
                return Err("friend blocked".into());
            }
            friend.group_only = false;
            return self.write(&records);
        }
        if records.friends.len() >= MAX_FRIENDS {
            return Err("friend limit reached".into());
        }
        records.friends.push(Friend {
            peer_id,
            display_name,
            permission: Permission::Off,
            blocked: false,
            paired_at_ms: now()?,
            groups: vec![],
            group_only: false,
        });
        self.write(&records)
    }

    /// Local nickname; never sent to anyone.
    pub fn rename(&self, peer: EndpointId, name: &str) -> Result<()> {
        validate_name(name)?;
        let _lock = self.lock()?;
        let mut records = self.read()?;
        records
            .friends
            .iter_mut()
            .find(|f| f.peer_id == peer)
            .ok_or("unknown friend")?
            .display_name = name.trim().to_owned();
        self.write(&records)
    }

    pub fn may_pair(&self, peer: EndpointId) -> Result<bool> {
        Ok(peer != self.owner
            && !self
                .list()?
                .iter()
                .any(|friend| friend.peer_id == peer && friend.blocked))
    }

    pub fn set_permission(&self, peer: EndpointId, permission: Permission) -> Result<()> {
        let _lock = self.lock()?;
        let mut records = self.read()?;
        let friend = records
            .friends
            .iter_mut()
            .find(|friend| friend.peer_id == peer)
            .ok_or("unknown friend")?;
        if friend.blocked && permission != Permission::Off {
            return Err("friend blocked".into());
        }
        friend.permission = permission;
        self.write(&records)
    }

    pub fn block(&self, peer: EndpointId) -> Result<()> {
        let _lock = self.lock()?;
        let mut records = self.read()?;
        let friend = records
            .friends
            .iter_mut()
            .find(|friend| friend.peer_id == peer)
            .ok_or("unknown friend")?;
        friend.permission = Permission::Off;
        friend.blocked = true;
        self.write(&records)
    }

    pub fn remove(&self, peer: EndpointId) -> Result<()> {
        let _lock = self.lock()?;
        let mut records = self.read()?;
        records.friends.retain(|friend| friend.peer_id != peer);
        self.write(&records)
    }

    pub fn may_share(&self, peer: EndpointId, recording: bool) -> Result<bool> {
        let _lock = self.lock()?;
        let records = self.read()?;
        Ok(records.friends.iter().any(|friend| {
            friend.peer_id == peer
                && match friend.effective_permission(&records.groups) {
                    Permission::Off => false,
                    Permission::TripsOnly => recording,
                    Permission::Always => true,
                }
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use iroh::SecretKey;

    #[test]
    fn permissions_revocation_and_identity_binding_survive_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let owner = SecretKey::generate().public();
        let peer = SecretKey::generate().public();
        let store = Friends::open(dir.path(), owner).unwrap();
        assert!(!store.may_share(peer, true).unwrap());
        store.add_confirmed(peer, "Friend".into()).unwrap();
        assert!(!store.may_share(peer, true).unwrap());
        store.set_permission(peer, Permission::TripsOnly).unwrap();
        let other = Friends::open(dir.path(), owner).unwrap();
        assert!(!other.may_share(peer, false).unwrap());
        assert!(other.may_share(peer, true).unwrap());
        store.set_permission(peer, Permission::Always).unwrap();
        store.add_confirmed(peer, "Renamed remote".into()).unwrap();
        assert_eq!(store.list().unwrap()[0].display_name, "Friend");
        assert!(other.may_share(peer, false).unwrap());
        store.block(peer).unwrap();
        assert!(!other.may_share(peer, true).unwrap());
        assert!(!other.may_pair(peer).unwrap());
        assert!(other.set_permission(peer, Permission::Always).is_err());
        assert!(other.add_confirmed(peer, "Again".into()).is_err());
        store.remove(peer).unwrap();
        assert!(!other.may_share(peer, true).unwrap());
        assert!(other.list().unwrap().is_empty());
        assert!(Friends::open(dir.path(), SecretKey::generate().public()).is_err());
    }

    #[test]
    fn corrupt_storage_is_preserved_and_never_grants_access() {
        let dir = tempfile::tempdir().unwrap();
        let store = Friends::open(dir.path(), SecretKey::generate().public()).unwrap();
        let path = dir.path().join("sharing/friends.json");
        std::fs::write(&path, b"broken").unwrap();
        assert!(
            store
                .may_share(SecretKey::generate().public(), true)
                .is_err()
        );
        assert!(store.remove(SecretKey::generate().public()).is_err());
        assert_eq!(std::fs::read(path).unwrap(), b"broken");
    }

    #[test]
    fn concurrent_writers_do_not_lose_friends() {
        let dir = tempfile::tempdir().unwrap();
        let store = Friends::open(dir.path(), SecretKey::generate().public()).unwrap();
        std::thread::scope(|scope| {
            for _ in 0..8 {
                let store = store.clone();
                scope.spawn(move || {
                    store
                        .add_confirmed(SecretKey::generate().public(), "Friend".into())
                        .unwrap()
                });
            }
        });
        assert_eq!(store.list().unwrap().len(), 8);
    }

    #[test]
    fn duplicate_or_self_records_are_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let owner = SecretKey::generate().public();
        let store = Friends::open(dir.path(), owner).unwrap();
        let peer = SecretKey::generate().public();
        store.add_confirmed(peer, "Friend".into()).unwrap();
        let mut records = store.read().unwrap();
        records.friends.push(records.friends[0].clone());
        let path = dir.path().join("sharing/friends.json");
        std::fs::write(&path, serde_json::to_vec(&records).unwrap()).unwrap();
        assert!(store.list().is_err());
        records.friends.pop();
        records.friends[0].peer_id = owner;
        std::fs::write(&path, serde_json::to_vec(&records).unwrap()).unwrap();
        assert!(store.list().is_err());
    }
}

#[cfg(test)]
mod group_permission_tests {
    use super::*;
    use crate::sharing::groups::Policy;
    use iroh::SecretKey;

    #[test]
    fn friend_sees_highest_of_own_and_group_grant() {
        let dir = tempfile::tempdir().unwrap();
        let key = SecretKey::generate();
        let store = Friends::open(dir.path(), key.public()).unwrap();
        let peer = SecretKey::generate().public();
        store.add_confirmed(peer, "Mum".into()).unwrap();
        let group = store
            .create_group(&key, "Family", "Me", Policy::Managed, Permission::Always)
            .unwrap();
        store.admit_member(&key, &group, peer, "Mum").unwrap();
        assert!(store.may_share(peer, false).unwrap());
        store
            .set_group_permission(&group, Permission::TripsOnly)
            .unwrap();
        assert!(!store.may_share(peer, false).unwrap());
        assert!(store.may_share(peer, true).unwrap());
        store.set_group_permission(&group, Permission::Off).unwrap();
        assert!(!store.may_share(peer, true).unwrap());
        store.set_permission(peer, Permission::Always).unwrap();
        assert!(store.may_share(peer, false).unwrap());
        store.block(peer).unwrap();
        assert!(!store.may_share(peer, true).unwrap());
    }
}

#[cfg(test)]
mod signed_group_tests {
    use super::*;
    use crate::sharing::groups::Policy;
    use iroh::SecretKey;

    #[test]
    fn block_survives_group_only_members_leaving_and_readmission() {
        use crate::sharing::groups::{OpKind, SignedOp};
        let dir = tempfile::tempdir().unwrap();
        let key = SecretKey::generate();
        let peer = SecretKey::generate();
        let relay = SecretKey::generate();
        let store = Friends::open(dir.path(), key.public()).unwrap();
        let id = store
            .create_group(&key, "Family", "Me", Policy::Managed, Permission::Always)
            .unwrap();
        store.admit_member(&key, &id, peer.public(), "Sam").unwrap();
        store
            .admit_member(&key, &id, relay.public(), "Relay")
            .unwrap();
        store.block(peer.public()).unwrap();
        let mut log = store.sync_for_locked(relay.public()).unwrap().remove(0);
        let at = now()
            .unwrap()
            .max(log.ops.iter().map(|o| o.op.at_ms).max().unwrap() + 1);
        log.ops
            .push(SignedOp::sign(&peer, log.group, at, OpKind::Leave).unwrap());
        store.apply_sync_locked(relay.public(), &[log]).unwrap();
        let records = store.list().unwrap();
        let blocked = records.iter().find(|f| f.peer_id == peer.public()).unwrap();
        assert!(blocked.group_only && blocked.blocked && blocked.groups.is_empty());
        assert!(!store.may_pair(peer.public()).unwrap());
        store.admit_member(&key, &id, peer.public(), "Sam").unwrap();
        assert!(!store.may_share(peer.public(), true).unwrap());
        assert!(!store.may_pair(peer.public()).unwrap());
    }

    #[test]
    fn durable_membership_remove_cascade_offline_leave_and_ack() {
        let keys: Vec<_> = (0..4).map(|_| SecretKey::generate()).collect();
        let dirs: Vec<_> = (0..4).map(|_| tempfile::tempdir().unwrap()).collect();
        let stores: Vec<_> = (0..4)
            .map(|i| Friends::open(dirs[i].path(), keys[i].public()).unwrap())
            .collect();
        let id = stores[0]
            .create_group(
                &keys[0],
                "Family",
                "Alex",
                Policy::Managed,
                Permission::Always,
            )
            .unwrap();
        for i in 1..4 {
            let admission = stores[0]
                .admit_member(&keys[0], &id, keys[i].public(), &format!("M{i}"))
                .unwrap();
            stores[i]
                .join_group(&admission, &format!("M{i}"), Permission::Always)
                .unwrap();
        }
        stores[0]
            .promote_member(&keys[0], &id, keys[1].public())
            .unwrap();
        let sync = stores[0].sync_for_locked(keys[1].public()).unwrap();
        stores[1]
            .apply_sync_locked(keys[0].public(), &sync)
            .unwrap();
        // Re-admit C through B, giving cascade a signed provenance chain.
        stores[0]
            .remove_member(&keys[0], &id, keys[2].public(), false)
            .unwrap();
        let sync = stores[0].sync_for_locked(keys[1].public()).unwrap();
        stores[1]
            .apply_sync_locked(keys[0].public(), &sync)
            .unwrap();
        let admission = stores[1]
            .admit_member(&keys[1], &id, keys[2].public(), "C")
            .unwrap();
        stores[0]
            .apply_sync_locked(keys[1].public(), &[admission])
            .unwrap();
        stores[0]
            .remove_member(&keys[0], &id, keys[1].public(), true)
            .unwrap();
        assert!(!stores[0].may_share(keys[1].public(), true).unwrap());
        assert!(!stores[0].may_share(keys[2].public(), true).unwrap());
        let removal = stores[0].sync_for_locked(keys[1].public()).unwrap();
        let (_, ack) = stores[1]
            .apply_sync_locked(keys[0].public(), &removal)
            .unwrap();
        assert!(
            !stores[1]
                .group_state(&id)
                .unwrap()
                .unwrap()
                .members
                .contains_key(&keys[1].public())
        );
        stores[0]
            .acknowledge_locked(keys[1].public(), &ack)
            .unwrap();
        assert!(
            stores[0]
                .sync_for_locked(keys[1].public())
                .unwrap()
                .is_empty()
        );
        // D learns removals through gossip, then tells C while A is absent.
        let gossip = stores[0].sync_for_locked(keys[3].public()).unwrap();
        stores[3]
            .apply_sync_locked(keys[0].public(), &gossip)
            .unwrap();
        let relayed_removal = stores[3].sync_for_locked(keys[2].public()).unwrap();
        assert!(!relayed_removal.is_empty());
        let (_, ack) = stores[2]
            .apply_sync_locked(keys[3].public(), &relayed_removal)
            .unwrap();
        stores[3]
            .acknowledge_locked(keys[2].public(), &ack)
            .unwrap();
        assert!(
            !stores[2]
                .group_state(&id)
                .unwrap()
                .unwrap()
                .members
                .contains_key(&keys[2].public())
        );
        // D leaves while A is offline. C stores leave, then carries it to A later.
        let admission = stores[0]
            .admit_member(&keys[0], &id, keys[2].public(), "C")
            .unwrap();
        stores[2]
            .join_group(&admission, "C", Permission::Always)
            .unwrap();
        stores[3]
            .apply_sync_locked(keys[2].public(), &[admission])
            .unwrap();
        assert!(
            !stores[3].groups().unwrap()[0]
                .pending_removals
                .contains(&keys[2].public())
        );
        stores[3].leave_group(&keys[3], &id).unwrap();
        assert!(!stores[3].may_share(keys[0].public(), true).unwrap());
        assert!(!stores[3].finish_leaving().unwrap());
        let leave = stores[3].sync_for_locked(keys[2].public()).unwrap();
        let (_, ack) = stores[2]
            .apply_sync_locked(keys[3].public(), &leave)
            .unwrap();
        stores[3]
            .acknowledge_locked(keys[2].public(), &ack)
            .unwrap();
        assert!(stores[3].groups().unwrap().is_empty());
        let restarted = Friends::open(dirs[2].path(), keys[2].public()).unwrap();
        let gossip = restarted.sync_for_locked(keys[0].public()).unwrap();
        stores[0]
            .apply_sync_locked(keys[2].public(), &gossip)
            .unwrap();
        assert!(!stores[0].may_share(keys[3].public(), true).unwrap());
    }

    #[test]
    fn membership_notifications_checkpoint_and_respect_hidden_access() {
        let dir = tempfile::tempdir().unwrap();
        let key = SecretKey::generate();
        let store = Friends::open(dir.path(), key.public()).unwrap();
        let id = store
            .create_group(&key, "Family", "Alex", Policy::Open, Permission::Always)
            .unwrap();
        assert!(store.membership_notices().unwrap().is_empty());
        let peer = SecretKey::generate().public();
        store.admit_member(&key, &id, peer, "Sam").unwrap();
        let notices = store.membership_notices().unwrap();
        assert_eq!(notices.len(), 1);
        assert_eq!(
            notices[0].1,
            "Sam joined Family. They can see your location (Always)."
        );
        assert!(store.membership_notices().unwrap().is_empty());
        store.set_group_permission(&id, Permission::Off).unwrap();
        store
            .admit_member(&key, &id, SecretKey::generate().public(), "Hidden")
            .unwrap();
        assert!(store.membership_notices().unwrap().is_empty());
        store.set_group_permission(&id, Permission::Always).unwrap();
        assert!(store.membership_notices().unwrap().is_empty());
        store.remove_member(&key, &id, peer, false).unwrap();
        store.admit_member(&key, &id, peer, "Sam").unwrap();
        assert_eq!(store.membership_notices().unwrap().len(), 1);
    }

    #[test]
    fn signed_leave_recovers_delivery_state_after_metadata_write_interruption() {
        let dir = tempfile::tempdir().unwrap();
        let key = SecretKey::generate();
        let peer = SecretKey::generate().public();
        let store = Friends::open(dir.path(), key.public()).unwrap();
        let id = store
            .create_group(&key, "Family", "Me", Policy::Managed, Permission::Always)
            .unwrap();
        store.admit_member(&key, &id, peer, "Sam").unwrap();
        let path = store.directory().join("friends.json");
        let before = std::fs::read(&path).unwrap();
        store.leave_group(&key, &id).unwrap();
        std::fs::write(path, before).unwrap();
        let reopened = Friends::open(dir.path(), key.public()).unwrap();
        assert!(reopened.groups().unwrap()[0].leaving_since_ms.is_some());
        assert!(!reopened.may_share(peer, true).unwrap());
        assert!(!reopened.sync_for_locked(peer).unwrap().is_empty());
    }

    #[test]
    fn leave_requires_matching_ack_or_seven_days_then_deletes_log() {
        use crate::sharing::groups::{self, GroupAck};
        let dir = tempfile::tempdir().unwrap();
        let key = SecretKey::generate();
        let peer = SecretKey::generate().public();
        let store = Friends::open(dir.path(), key.public()).unwrap();
        let id = store
            .create_group(&key, "Family", "Me", Policy::Managed, Permission::Always)
            .unwrap();
        store.admit_member(&key, &id, peer, "Sam").unwrap();
        store.leave_group(&key, &id).unwrap();
        assert!(
            !store
                .acknowledge_locked(
                    peer,
                    &[GroupAck {
                        group: groups::parse_id(&id).unwrap(),
                        digest: [0; 32]
                    }]
                )
                .unwrap()
        );
        assert_eq!(store.groups().unwrap().len(), 1);
        assert!(!store.finish_leaving().unwrap());
        let mut records = store.read().unwrap();
        records.groups[0].leaving_since_ms = Some(now().unwrap() - LEAVE_GRACE_MS - 1);
        store.write(&records).unwrap();
        assert!(store.finish_leaving().unwrap());
        assert!(store.groups().unwrap().is_empty());
        assert!(store.list().unwrap().is_empty());
        assert!(
            !store
                .directory()
                .join("groups")
                .join(format!("{id}.ops"))
                .exists()
        );
    }

    #[test]
    fn unsigned_legacy_membership_never_grants_access() {
        let dir = tempfile::tempdir().unwrap();
        let key = SecretKey::generate();
        let peer = SecretKey::generate().public();
        let store = Friends::open(dir.path(), key.public()).unwrap();
        store.add_confirmed(peer, "Friend".into()).unwrap();
        let mut records = store.read().unwrap();
        let id = "01".repeat(32);
        records.groups.push(Group {
            version: 1,
            pending_removals: vec![],
            id: id.clone(),
            name: "Old".into(),
            me: "Me".into(),
            left: vec![],
            leaving_since_ms: None,
            share: Permission::Always,
        });
        records.friends[0].groups.push(id.clone());
        store.write(&records).unwrap();
        let reopened = Friends::open(dir.path(), key.public()).unwrap();
        assert!(!reopened.may_share(peer, true).unwrap());
        reopened.set_permission(peer, Permission::Always).unwrap();
        assert!(reopened.may_share(peer, true).unwrap());
        assert!(reopened.group_state(&id).unwrap().is_none());
        reopened.forget_inactive_group(&id).unwrap();
        assert!(reopened.groups().unwrap().is_empty());
    }
}
