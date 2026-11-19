//! Group persistence and local membership actions, under friends.lock.
use crate::sharing::{
    Result,
    groups::{self, GroupAck, GroupOps, OpKind, Policy, SignedOp, State},
    peers::{
        Friend, Friends, Group, LEAVE_GRACE_MS, MAX_FILE, MAX_FRIENDS, MAX_GROUPS, Permission,
        Records, now, validate_name,
    },
};
use iroh::{EndpointId, SecretKey};
use std::{
    fs::File,
    io::{Read, Write},
};

impl Friends {
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
        self.finish_leaving()?;
        let _lock = self.lock()?;
        Ok(self.read()?.groups)
    }

    /// Caller holds `lock()`.
    pub(in crate::sharing) fn groups_locked(&self) -> Result<Vec<Group>> {
        Ok(self.read()?.groups)
    }

    fn read_ops(&self, id: &str) -> Result<Vec<SignedOp>> {
        groups::parse_id(id)?;
        let file = File::open(self.directory.join("groups").join(format!("{id}.ops")))?;
        let mut bytes = Vec::new();
        file.take(MAX_FILE + 1).read_to_end(&mut bytes)?;
        if bytes.len() as u64 > MAX_FILE {
            return Err("group log too large".into());
        }
        let ops: Vec<SignedOp> = postcard::from_bytes(&bytes)?;
        groups::merge(groups::parse_id(id)?, &[], &ops, now()?)
    }

    fn write_ops(&self, id: &str, ops: &[SignedOp]) -> Result<()> {
        groups::parse_id(id)?;
        let directory = self.directory.join("groups");
        std::fs::create_dir_all(&directory)?;
        let bytes = postcard::to_allocvec(ops)?;
        if bytes.len() as u64 > MAX_FILE {
            return Err("group log too large".into());
        }
        let mut file = tempfile::NamedTempFile::new_in(&directory)?;
        file.write_all(&bytes)?;
        file.as_file().sync_all()?;
        file.persist(directory.join(format!("{id}.ops")))?;
        File::open(directory)?.sync_all()?;
        Ok(())
    }

    fn delete_ops(&self, ids: &[String]) -> Result<()> {
        for id in ids {
            groups::parse_id(id)?;
            match std::fs::remove_file(self.directory.join("groups").join(format!("{id}.ops"))) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
        }
        Ok(())
    }

    /// No unsigned friends.json membership can grant group access.
    pub(in crate::sharing) fn rebuild(&self, records: &mut Records) -> Result<()> {
        for friend in &mut records.friends {
            friend.groups.clear();
        }
        for group in &mut records.groups {
            if group.version != 2 {
                continue;
            }
            let ops = self.read_ops(&group.id)?;
            let state = groups::derive(groups::parse_id(&group.id)?, &ops, now()?)?
                .ok_or("group missing Create")?;
            // Log is durable before local metadata. Recover interrupted leave so its
            // routes survive a crash between the two atomic file replacements.
            if !state.members.contains_key(&self.owner)
                && !state.removed.contains_key(&self.owner)
                && group.leaving_since_ms.is_none()
            {
                group.leaving_since_ms = ops
                    .iter()
                    .filter(|o| o.op.author == self.owner && matches!(o.op.kind, OpKind::Leave))
                    .map(|o| o.op.at_ms)
                    .max();
            }
            // Re-admission supersedes old removal deliveries. Keeping a stale route
            // would retain a group-only friend after their later voluntary leave.
            group
                .pending_removals
                .retain(|peer| state.removed.contains_key(peer));
            for peer in &group.pending_removals {
                if *peer == self.owner || records.friends.iter().any(|f| f.peer_id == *peer) {
                    continue;
                }
                if let Some(removed) = state.removed.get(peer)
                    && records.friends.len() < MAX_FRIENDS
                {
                    records.friends.push(Friend {
                        peer_id: *peer,
                        display_name: removed.member.name.clone(),
                        permission: Permission::Off,
                        blocked: false,
                        paired_at_ms: removed.member.admitted_at_ms,
                        groups: vec![],
                        group_only: true,
                    });
                }
            }
            // Leavers keep routes to remaining members solely for delivery. Their group
            // grants are disabled independently by leaving_since_ms.
            if !state.members.contains_key(&self.owner) && group.leaving_since_ms.is_none() {
                continue;
            }
            for (peer, member) in &state.members {
                if *peer == self.owner {
                    continue;
                }
                if let Some(friend) = records.friends.iter_mut().find(|f| f.peer_id == *peer) {
                    friend.groups.push(group.id.clone());
                } else if records.friends.len() < MAX_FRIENDS {
                    records.friends.push(Friend {
                        peer_id: *peer,
                        display_name: if member.name.is_empty() {
                            peer.to_string()
                        } else {
                            member.name.clone()
                        },
                        permission: Permission::Off,
                        blocked: false,
                        paired_at_ms: member.admitted_at_ms,
                        groups: vec![group.id.clone()],
                        group_only: true,
                    });
                } else {
                    return Err("friend limit reached".into());
                }
            }
        }
        records.friends.retain(|f| {
            !f.group_only
                || f.blocked
                || !f.groups.is_empty()
                || records
                    .groups
                    .iter()
                    .any(|g| g.pending_removals.contains(&f.peer_id))
        });
        Ok(())
    }

    pub(in crate::sharing) fn group_views_locked(&self) -> Result<Vec<(Group, State)>> {
        self.read()?
            .groups
            .into_iter()
            .filter(|g| g.version == 2)
            .map(|g| {
                let state =
                    groups::derive(groups::parse_id(&g.id)?, &self.read_ops(&g.id)?, now()?)?
                        .ok_or("missing Create")?;
                Ok((g, state))
            })
            .collect()
    }

    pub fn group_state(&self, id: &str) -> Result<Option<State>> {
        let _lock = self.lock()?;
        let records = self.read()?;
        let group = records
            .groups
            .iter()
            .find(|g| g.id == id)
            .ok_or("unknown group")?;
        if group.version != 2 {
            return Ok(None);
        }
        groups::derive(groups::parse_id(id)?, &self.read_ops(id)?, now()?)
    }

    pub fn create_group(
        &self,
        key: &SecretKey,
        name: &str,
        me: &str,
        policy: Policy,
        share: Permission,
    ) -> Result<String> {
        if key.public() != self.owner {
            return Err("group signing identity mismatch".into());
        }
        validate_name(name)?;
        validate_name(me)?;
        let op = SignedOp::sign(
            key,
            [0; 32],
            now()?,
            OpKind::Create {
                name: name.into(),
                policy,
                creator_name: me.into(),
            },
        )?;
        let id = groups::id_string(op.digest()?);
        let _lock = self.lock()?;
        let mut records = self.read()?;
        if records.groups.len() >= MAX_GROUPS {
            return Err("group limit reached".into());
        }
        if records.groups.iter().any(|g| g.id == id) {
            return Err("group already exists".into());
        }
        self.write_ops(&id, &[op])?;
        records.groups.push(Group {
            version: 2,
            pending_removals: vec![],
            id: id.clone(),
            name: name.into(),
            me: me.into(),
            left: vec![],
            leaving_since_ms: None,
            share,
        });
        self.write(&records)?;
        Ok(id)
    }

    /// Explicit confirmed admission by an authorized inviter, never by the joiner.
    pub fn admit_member(
        &self,
        key: &SecretKey,
        id: &str,
        member: EndpointId,
        name: &str,
    ) -> Result<GroupOps> {
        validate_name(name)?;
        self.append_op(
            key,
            id,
            OpKind::Admit {
                member,
                name: name.into(),
            },
        )
    }

    fn append_op(&self, key: &SecretKey, id: &str, kind: OpKind) -> Result<GroupOps> {
        if key.public() != self.owner {
            return Err("group signing identity mismatch".into());
        }
        let _lock = self.lock()?;
        let mut records = self.read()?;
        let group = records
            .groups
            .iter_mut()
            .find(|g| g.id == id && g.version == 2 && g.leaving_since_ms.is_none())
            .ok_or("group unavailable")?;
        let ops = self.read_ops(id)?;
        let group_id = groups::parse_id(id)?;
        let state = groups::derive(group_id, &ops, now()?)?.ok_or("missing Create")?;
        let admin = state.admins.contains(&self.owner);
        let allowed = match &kind {
            OpKind::Admit { member, .. } => {
                state.members.contains_key(&self.owner)
                    && (admin || state.policy == Policy::Open)
                    && (state.members.contains_key(member)
                        || state.members.len() < groups::MAX_MEMBERS)
            }
            OpKind::Remove { member } => {
                admin && *member != state.creator && state.members.contains_key(member)
            }
            OpKind::Promote { member } => admin && state.members.contains_key(member),
            OpKind::Leave => state.members.contains_key(&self.owner),
            OpKind::Create { .. } => false,
        };
        if !allowed {
            return Err("group operation not allowed".into());
        }
        // Local operations must sort after all known operations, even within one ms.
        let at = now()?.max(
            ops.iter()
                .map(|o| o.op.at_ms)
                .max()
                .unwrap_or(0)
                .saturating_add(1),
        );
        if let OpKind::Remove { member } = &kind
            && !group.pending_removals.contains(member)
        {
            group.pending_removals.push(*member);
        }
        if matches!(kind, OpKind::Leave) {
            group.leaving_since_ms = Some(now()?);
        }
        // Persist removal delivery routes before the operation. Reads still derive
        // access from the old signed log until the new log replacement succeeds.
        if matches!(kind, OpKind::Remove { .. }) {
            self.write(&records)?;
        }
        let op = SignedOp::sign(key, group_id, at, kind)?;
        let merged = groups::merge(group_id, &ops, &[op], now()?)?;
        self.write_ops(id, &merged)?;
        self.rebuild(&mut records)?;
        self.write(&records)?;
        Ok(GroupOps {
            group: group_id,
            ops: merged,
        })
    }

    pub fn remove_member(
        &self,
        key: &SecretKey,
        id: &str,
        member: EndpointId,
        cascade: bool,
    ) -> Result<()> {
        let state = self.group_state(id)?.ok_or("legacy group")?;
        self.append_op(key, id, OpKind::Remove { member })?;
        if cascade {
            // Descendants are taken from current provenance, never unsigned claims.
            let mut removed = std::collections::BTreeSet::from([member]);
            loop {
                let children: Vec<_> = state
                    .members
                    .iter()
                    .filter(|(peer, m)| {
                        !removed.contains(*peer)
                            && removed.contains(&m.added_by)
                            && **peer != state.creator
                    })
                    .map(|(peer, _)| *peer)
                    .collect();
                if children.is_empty() {
                    break;
                }
                for child in children {
                    self.append_op(key, id, OpKind::Remove { member: child })?;
                    removed.insert(child);
                }
            }
        }
        Ok(())
    }

    pub fn promote_member(&self, key: &SecretKey, id: &str, member: EndpointId) -> Result<()> {
        self.append_op(key, id, OpKind::Promote { member })
            .map(drop)
    }

    /// Imports only a verified log that already admits this device.
    pub(in crate::sharing) fn join_group(
        &self,
        sync: &GroupOps,
        me: &str,
        share: Permission,
    ) -> Result<()> {
        validate_name(me)?;
        let ops = groups::merge(sync.group, &[], &sync.ops, now()?)?;
        let state = groups::derive(sync.group, &ops, now()?)?.ok_or("missing Create")?;
        if !state.members.contains_key(&self.owner) {
            return Err("not admitted to group".into());
        }
        let id = groups::id_string(sync.group);
        let _lock = self.lock()?;
        let mut records = self.read()?;
        if let Some(group) = records.groups.iter_mut().find(|g| g.id == id) {
            if group.version != 2 {
                return Err("recreate legacy group".into());
            }
            let stored = self.read_ops(&id)?;
            let merged = groups::merge(sync.group, &stored, &ops, now()?)?;
            if groups::derive(sync.group, &merged, now()?)?
                .is_none_or(|s| !s.members.contains_key(&self.owner))
            {
                return Err("group admission superseded; request a new invitation".into());
            }
            self.write_ops(&id, &merged)?;
            group.leaving_since_ms = None;
            group.me = me.into();
            group.share = share;
        } else {
            if records.groups.len() >= MAX_GROUPS {
                return Err("group limit reached".into());
            }
            self.write_ops(&id, &ops)?;
            records.groups.push(Group {
                version: 2,
                pending_removals: vec![],
                id,
                name: state.name,
                me: me.into(),
                left: vec![],
                leaving_since_ms: None,
                share,
            });
        }
        self.rebuild(&mut records)?;
        self.write(&records)
    }

    pub fn leave_group(&self, key: &SecretKey, id: &str) -> Result<()> {
        self.append_op(key, id, OpKind::Leave).map(drop)
    }

    /// Legacy groups cannot grant access; their sole action is local removal.
    pub fn forget_inactive_group(&self, id: &str) -> Result<()> {
        let _lock = self.lock()?;
        let mut records = self.read()?;
        let group = records
            .groups
            .iter()
            .find(|g| g.id == id)
            .ok_or("unknown group")?;
        if group.version == 2
            && groups::derive(groups::parse_id(id)?, &self.read_ops(id)?, now()?)?
                .is_some_and(|s| s.members.contains_key(&self.owner))
        {
            return Err("active group must be left".into());
        }
        records.groups.retain(|g| g.id != id);
        self.rebuild(&mut records)?;
        self.write(&records)?;
        self.delete_ops(&[id.to_owned()])
    }

    pub(in crate::sharing) fn finish_leaving(&self) -> Result<bool> {
        let _lock = self.lock()?;
        let mut records = self.read()?;
        let now = now()?;
        let done: Vec<_> = records
            .groups
            .iter()
            .filter(|g| {
                g.leaving_since_ms
                    .is_some_and(|at| now.saturating_sub(at) >= LEAVE_GRACE_MS)
            })
            .map(|g| g.id.clone())
            .collect();
        if done.is_empty() {
            return Ok(false);
        }
        records.groups.retain(|g| !done.contains(&g.id));
        self.rebuild(&mut records)?;
        self.write(&records)?;
        self.delete_ops(&done)?;
        Ok(true)
    }

    pub(in crate::sharing) fn sync_for_locked(&self, peer: EndpointId) -> Result<Vec<GroupOps>> {
        let records = self.read()?;
        if records
            .friends
            .iter()
            .any(|f| f.peer_id == peer && f.blocked)
        {
            return Ok(vec![]);
        }
        let mut syncs = vec![];
        for group in &records.groups {
            if group.version != 2 {
                continue;
            }
            let ops = self.read_ops(&group.id)?;
            let id = groups::parse_id(&group.id)?;
            let state = groups::derive(id, &ops, now()?)?.ok_or("missing Create")?;
            if state.members.contains_key(&peer) || group.pending_removals.contains(&peer) {
                syncs.push(GroupOps { group: id, ops });
            }
        }
        Ok(syncs)
    }

    pub(in crate::sharing) fn apply_sync_locked(
        &self,
        _sender: EndpointId,
        syncs: &[GroupOps],
    ) -> Result<(bool, Vec<GroupAck>)> {
        if syncs.len() > MAX_GROUPS {
            return Err("too many groups".into());
        }
        let mut records = self.read()?;
        let mut changed = false;
        let mut acks = vec![];
        for sync in syncs {
            let id = groups::id_string(sync.group);
            if !records.groups.iter().any(|g| g.id == id && g.version == 2) {
                continue;
            }
            let stored = self.read_ops(&id)?;
            let merged = groups::merge(sync.group, &stored, &sync.ops, now()?)?;
            if merged != stored {
                let old = groups::derive(sync.group, &stored, now()?)?.ok_or("missing Create")?;
                let new = groups::derive(sync.group, &merged, now()?)?.ok_or("missing Create")?;
                let group = records
                    .groups
                    .iter_mut()
                    .find(|g| g.id == id)
                    .ok_or("unknown group")?;
                for (peer, removed) in &new.removed {
                    if *peer != self.owner
                        && old.removed.get(peer) != Some(removed)
                        && !group.pending_removals.contains(peer)
                    {
                        group.pending_removals.push(*peer);
                    }
                }
                self.write(&records)?;
                self.write_ops(&id, &merged)?;
                changed = true;
            }
            acks.push(GroupAck {
                group: sync.group,
                digest: groups::log_digest(&sync.ops)?,
            });
        }
        if changed {
            self.rebuild(&mut records)?;
            self.write(&records)?;
        }
        Ok((changed, acks))
    }

    pub(in crate::sharing) fn acknowledge_locked(
        &self,
        peer: EndpointId,
        acks: &[GroupAck],
    ) -> Result<bool> {
        if acks.len() > MAX_GROUPS {
            return Err("too many acknowledgements".into());
        }
        let mut records = self.read()?;
        let mut changed = false;
        let mut forgotten = vec![];
        for ack in acks {
            let id = groups::id_string(ack.group);
            let Some(group) = records
                .groups
                .iter_mut()
                .find(|g| g.id == id && g.version == 2)
            else {
                continue;
            };
            let ops = self.read_ops(&id)?;
            if groups::log_digest(&ops)? != ack.digest {
                continue;
            }
            let state = groups::derive(ack.group, &ops, now()?)?.ok_or("missing Create")?;
            if group.leaving_since_ms.is_some() && state.members.contains_key(&peer) {
                forgotten.push(id);
                changed = true;
            } else if group.pending_removals.contains(&peer) {
                group.pending_removals.retain(|p| *p != peer);
                changed = true;
            }
        }
        if changed {
            records.groups.retain(|g| !forgotten.contains(&g.id));
            self.rebuild(&mut records)?;
            self.write(&records)?;
            self.delete_ops(&forgotten)?;
        }
        Ok(changed)
    }

    /// Recorder checkpoints membership so cross-process admits also trigger notices.
    #[cfg(any(
        test,
        all(any(target_os = "android", target_os = "ios"), feature = "ui")
    ))]
    pub(in crate::sharing) fn membership_notices(&self) -> Result<Vec<(i32, String)>> {
        let _lock = self.lock()?;
        let records = self.read()?;
        let path = self.directory.join("group-notices.postcard");
        type Seen = std::collections::BTreeSet<(String, EndpointId, u64)>;
        let previous: Seen = match File::open(&path) {
            Ok(file) => {
                let mut bytes = Vec::new();
                file.take(MAX_FILE + 1).read_to_end(&mut bytes)?;
                if bytes.len() as u64 > MAX_FILE {
                    return Err("group notices too large".into());
                }
                postcard::from_bytes(&bytes)?
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Seen::new(),
            Err(e) => return Err(e.into()),
        };
        let mut current = Seen::new();
        let mut notices = vec![];
        for group in records
            .groups
            .iter()
            .filter(|g| g.version == 2 && g.leaving_since_ms.is_none())
        {
            let state = groups::derive(
                groups::parse_id(&group.id)?,
                &self.read_ops(&group.id)?,
                now()?,
            )?
            .ok_or("missing Create")?;
            if !state.members.contains_key(&self.owner) {
                continue;
            }
            for (peer, member) in state.members {
                if peer == self.owner {
                    continue;
                }
                let entry = (group.id.clone(), peer, member.admitted_at_ms);
                if !previous.contains(&entry) && group.share != Permission::Off {
                    let hash = blake3::hash(&postcard::to_allocvec(&entry)?);
                    let id = (u32::from_le_bytes(hash.as_bytes()[..4].try_into()?) & 0x3fff_ffff)
                        as i32
                        + 10;
                    let access = if group.share == Permission::Always {
                        "Always"
                    } else {
                        "Trips"
                    };
                    notices.push((
                        id,
                        format!(
                            "{} joined {}. They can see your location ({access}).",
                            member.name, group.name
                        ),
                    ));
                }
                current.insert(entry);
            }
        }
        if current != previous {
            let mut file = tempfile::NamedTempFile::new_in(&self.directory)?;
            file.write_all(&postcard::to_allocvec(&current)?)?;
            file.as_file().sync_all()?;
            file.persist(path)?;
        }
        Ok(notices)
    }
}
