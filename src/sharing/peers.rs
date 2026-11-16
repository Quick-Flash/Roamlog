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

const MAX_FILE: u64 = 256 * 1024;
const MAX_FRIENDS: usize = 256;
const MAX_GROUPS: usize = 32;
/// Leaving members get a few seconds for running sessions to deliver the tombstone.
pub const LEAVE_GRACE_MS: u64 = 20_000;

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
    /// 32 random bytes, lowercase hex.
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

fn always() -> Permission {
    Permission::Always
}

/// Group view sent to a member over an authenticated session.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GroupSync {
    pub id: String,
    pub name: String,
    pub members: Vec<(EndpointId, String)>,
    pub left: Vec<EndpointId>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Records {
    version: u8,
    owner: EndpointId,
    friends: Vec<Friend>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    groups: Vec<Group>,
}

fn valid_group_id(id: &str) -> bool {
    id.len() == 64
        && id
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn now() -> Result<u64> {
    Ok(SystemTime::now()
        .duration_since(UNIX_EPOCH)?
        .as_millis()
        .try_into()?)
}

/// Removes `group` tag from friends matching `pred`; drops group-only friends left in none.
fn untag(records: &mut Records, group: &str, pred: impl Fn(&Friend) -> bool) {
    for friend in records.friends.iter_mut().filter(|f| pred(f)) {
        friend.groups.retain(|g| g != group);
    }
    records
        .friends
        .retain(|f| !(f.group_only && f.groups.is_empty()));
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
            .filter(|g| self.groups.contains(&g.id) && g.leaving_since_ms.is_none())
            .map(|g| g.share)
            .max()
            .unwrap_or(Permission::Off)
    }
}

/// Reopens records under a cross-process lock for every operation. No permission cache.
#[derive(Clone)]
pub struct Friends {
    directory: PathBuf,
    owner: EndpointId,
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

    fn read(&self) -> Result<Records> {
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
        let records: Records = serde_json::from_slice(&bytes)?;
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
        Ok(records)
    }

    fn write(&self, records: &Records) -> Result<()> {
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
            .iter()
            .find(|friend| friend.peer_id == peer_id)
        {
            return if friend.blocked {
                Err("friend blocked".into())
            } else {
                Ok(())
            };
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

    /// Local group label. Members keep their own names for it.
    pub fn rename_group(&self, id: &str, name: &str) -> Result<()> {
        validate_name(name)?;
        let _lock = self.lock()?;
        let mut records = self.read()?;
        records
            .groups
            .iter_mut()
            .find(|g| g.id == id)
            .ok_or("unknown group")?
            .name = name.trim().to_owned();
        self.write(&records)
    }

    pub fn set_group_permission(&self, id: &str, share: Permission) -> Result<()> {
        let _lock = self.lock()?;
        let mut records = self.read()?;
        records
            .groups
            .iter_mut()
            .find(|g| g.id == id)
            .ok_or("unknown group")?
            .share = share;
        self.write(&records)
    }

    pub fn groups(&self) -> Result<Vec<Group>> {
        let _lock = self.lock()?;
        Ok(self.read()?.groups)
    }

    /// Caller holds `lock()`.
    pub(super) fn groups_locked(&self) -> Result<Vec<Group>> {
        Ok(self.read()?.groups)
    }

    pub fn create_group(&self, name: &str, me: &str) -> Result<String> {
        validate_name(name)?;
        validate_name(me)?;
        let id: String = iroh::SecretKey::generate()
            .to_bytes()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        let _lock = self.lock()?;
        let mut records = self.read()?;
        if records.groups.len() >= MAX_GROUPS {
            return Err("group limit reached".into());
        }
        records.groups.push(Group {
            id: id.clone(),
            name: name.to_owned(),
            me: me.to_owned(),
            left: vec![],
            leaving_since_ms: None,
            share: Permission::Always,
        });
        self.write(&records)?;
        Ok(id)
    }

    /// After confirmed pairing through a group invitation: joins group (if new) and adds
    /// `peer` as member. Other members arrive through that member's group sync.
    pub(super) fn join_group(
        &self,
        id: &str,
        name: &str,
        me: &str,
        peer: EndpointId,
    ) -> Result<()> {
        validate_name(name)?;
        validate_name(me)?;
        if !valid_group_id(id) {
            return Err("invalid group".into());
        }
        let _lock = self.lock()?;
        let mut records = self.read()?;
        let count = records.groups.len();
        match records.groups.iter_mut().find(|g| g.id == id) {
            Some(group) => {
                group.leaving_since_ms = None;
                group.left.retain(|p| *p != peer && *p != self.owner);
            }
            None if count >= MAX_GROUPS => return Err("group limit reached".into()),
            None => records.groups.push(Group {
                id: id.to_owned(),
                name: name.to_owned(),
                me: me.to_owned(),
                left: vec![],
                leaving_since_ms: None,
                share: Permission::Always,
            }),
        }
        let friend = records
            .friends
            .iter_mut()
            .find(|f| f.peer_id == peer)
            .ok_or("unknown friend")?;
        if friend.blocked {
            return Err("friend blocked".into());
        }
        if !friend.groups.iter().any(|g| g == id) {
            friend.groups.push(id.to_owned());
        }
        self.write(&records)
    }

    /// Starts leaving: group stops granting access now; tombstone goes to connected
    /// members, then `finish_leaving` forgets group. Without running sessions, forget now.
    pub fn leave_group(&self, id: &str, immediately: bool) -> Result<()> {
        let _lock = self.lock()?;
        let mut records = self.read()?;
        let group = records
            .groups
            .iter_mut()
            .find(|g| g.id == id)
            .ok_or("unknown group")?;
        if immediately {
            records.groups.retain(|g| g.id != id);
            untag(&mut records, id, |_| true);
        } else {
            group.leaving_since_ms = Some(now()?);
        }
        self.write(&records)
    }

    /// Forgets groups whose leave grace period passed. Returns whether anything changed.
    pub(super) fn finish_leaving(&self) -> Result<bool> {
        let _lock = self.lock()?;
        let mut records = self.read()?;
        let now = now()?;
        let done: Vec<String> = records
            .groups
            .iter()
            .filter(|g| {
                g.leaving_since_ms
                    .is_some_and(|t| now.saturating_sub(t) >= LEAVE_GRACE_MS)
            })
            .map(|g| g.id.clone())
            .collect();
        if done.is_empty() {
            return Ok(false);
        }
        for id in &done {
            records.groups.retain(|g| &g.id != id);
            untag(&mut records, id, |_| true);
        }
        self.write(&records)?;
        Ok(true)
    }

    /// Groups shared with `peer`, as that member should see them. Caller holds `lock()`.
    pub(super) fn sync_for_locked(&self, peer: EndpointId) -> Result<Vec<GroupSync>> {
        let records = self.read()?;
        let Some(friend) = records
            .friends
            .iter()
            .find(|f| f.peer_id == peer && !f.blocked)
        else {
            return Ok(vec![]);
        };
        Ok(records
            .groups
            .iter()
            .filter(|g| friend.groups.contains(&g.id))
            .map(|g| {
                let mut members = vec![(self.owner, g.me.clone())];
                members.extend(
                    records
                        .friends
                        .iter()
                        .filter(|f| !f.blocked && f.groups.contains(&g.id))
                        .map(|f| (f.peer_id, f.display_name.clone())),
                );
                let mut left = g.left.clone();
                if g.leaving_since_ms.is_some() {
                    members.retain(|(p, _)| *p != self.owner);
                    left.push(self.owner);
                }
                GroupSync {
                    id: g.id.clone(),
                    name: g.name.clone(),
                    members,
                    left,
                }
            })
            .collect())
    }

    /// Merges member list from `sender`. Trust is vouched: any member may admit others,
    /// but only into groups both share, and never blocked identities or tombstoned ones.
    /// Caller holds `lock()`. Returns whether records changed.
    pub(super) fn apply_sync_locked(
        &self,
        sender: EndpointId,
        syncs: &[GroupSync],
    ) -> Result<bool> {
        if syncs.len() > MAX_GROUPS {
            return Err("too many groups".into());
        }
        let mut records = self.read()?;
        let before = serde_json::to_vec(&records)?;
        for sync in syncs {
            if sync.members.len() > MAX_FRIENDS || sync.left.len() > 1024 {
                return Err("group sync too large".into());
            }
            let Some(group) = records
                .groups
                .iter_mut()
                .find(|g| g.id == sync.id && g.leaving_since_ms.is_none())
            else {
                continue;
            };
            let vouched = records
                .friends
                .iter()
                .any(|f| f.peer_id == sender && !f.blocked && f.groups.contains(&sync.id));
            if !vouched {
                continue;
            }
            for peer in &sync.left {
                if *peer != self.owner && !group.left.contains(peer) && group.left.len() < 1024 {
                    group.left.push(*peer);
                }
            }
            let left = group.left.clone();
            untag(&mut records, &sync.id, |f| left.contains(&f.peer_id));
            for (peer, name) in &sync.members {
                if *peer == self.owner || left.contains(peer) || validate_name(name).is_err() {
                    continue;
                }
                let count = records.friends.len();
                match records.friends.iter_mut().find(|f| f.peer_id == *peer) {
                    Some(friend) if friend.blocked => {}
                    Some(friend) if !friend.groups.contains(&sync.id) => {
                        friend.groups.push(sync.id.clone());
                    }
                    Some(_) => {}
                    None if count < MAX_FRIENDS => records.friends.push(Friend {
                        peer_id: *peer,
                        display_name: name.clone(),
                        permission: Permission::Off,
                        blocked: false,
                        paired_at_ms: now()?,
                        groups: vec![sync.id.clone()],
                        group_only: true,
                    }),
                    None => {}
                }
            }
        }
        if serde_json::to_vec(&records)? == before {
            return Ok(false);
        }
        self.write(&records)?;
        Ok(true)
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
    use iroh::SecretKey;

    #[test]
    fn friend_sees_highest_of_own_and_group_grant() {
        let dir = tempfile::tempdir().unwrap();
        let store = Friends::open(dir.path(), SecretKey::generate().public()).unwrap();
        let peer = SecretKey::generate().public();
        store.add_confirmed(peer, "Mum".into()).unwrap();
        let group = store.create_group("Family", "Me").unwrap();
        store.join_group(&group, "Family", "Me", peer).unwrap();
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
