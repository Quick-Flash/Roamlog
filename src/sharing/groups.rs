//! Signed membership operations and deterministic replay.
mod store;
use super::{Result, protocol::FUTURE_SKEW_MS};
use iroh::{EndpointId, SecretKey, Signature};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    io,
};

pub type GroupId = [u8; 32];
pub const MAX_OPS: usize = 512;
pub const MAX_MEMBERS: usize = 64;
const DOMAIN: &[u8] = b"roamlog/group-op/1";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Policy {
    Managed,
    Open,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum OpKind {
    Create {
        name: String,
        policy: Policy,
        creator_name: String,
    },
    Admit {
        member: EndpointId,
        name: String,
    },
    Leave,
    Remove {
        member: EndpointId,
    },
    Promote {
        member: EndpointId,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Op {
    /// Create uses zero to avoid a circular hash; other ops use its signed hash.
    pub group: GroupId,
    pub author: EndpointId,
    pub at_ms: u64,
    pub kind: OpKind,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignedOp {
    pub op: Op,
    pub sig: Signature,
}

fn message(op: &Op) -> Result<Vec<u8>> {
    let mut bytes = DOMAIN.to_vec();
    bytes.extend(postcard::to_allocvec(op)?);
    Ok(bytes)
}

impl SignedOp {
    pub fn sign(key: &SecretKey, group: GroupId, at_ms: u64, kind: OpKind) -> Result<Self> {
        let op = Op {
            group,
            author: key.public(),
            at_ms,
            kind,
        };
        let sig = key.sign(&message(&op)?);
        Ok(Self { op, sig })
    }

    pub fn digest(&self) -> Result<GroupId> {
        Ok(*blake3::hash(&postcard::to_allocvec(self)?).as_bytes())
    }

    pub(super) fn valid(&self, group: GroupId, now_ms: u64) -> Result<bool> {
        let bound = match self.op.kind {
            OpKind::Create { .. } => self.op.group == [0; 32] && self.digest()? == group,
            _ => self.op.group == group,
        };
        Ok(bound
            && self.op.at_ms <= now_ms.saturating_add(FUTURE_SKEW_MS as u64)
            && self
                .op
                .author
                .verify(&message(&self.op)?, &self.sig)
                .is_ok())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Member {
    pub name: String,
    pub added_by: EndpointId,
    pub admitted_at_ms: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemovedMember {
    pub member: Member,
    pub operation: GroupId,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct State {
    pub creator: EndpointId,
    pub creator_name: String,
    pub name: String,
    pub policy: Policy,
    pub members: BTreeMap<EndpointId, Member>,
    /// Last admitted names survive departure for the "added by" label.
    pub names: BTreeMap<EndpointId, String>,
    pub admins: BTreeSet<EndpointId>,
    pub removed: BTreeMap<EndpointId, RemovedMember>,
}

/// Verify before replay; duplicates never consume log capacity. Semantically invalid
/// signed ops remain available to callers for later merges and deterministic replay.
/// Missing Create yields no state, allowing ops to arrive before their anchor.
pub fn derive(group: GroupId, ops: &[SignedOp], now_ms: u64) -> Result<Option<State>> {
    let mut ordered = BTreeMap::new();
    for signed in ops {
        if signed.valid(group, now_ms)? {
            ordered.insert((signed.op.at_ms, signed.digest()?), &signed.op);
            if ordered.len() > MAX_OPS {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "too many group operations",
                )
                .into());
            }
        }
    }
    let mut state: Option<State> = None;
    for ((_, digest), op) in &ordered {
        let Some(s) = state.as_mut() else {
            if let OpKind::Create {
                name,
                policy,
                creator_name,
            } = &op.kind
            {
                state = Some(State {
                    creator: op.author,
                    creator_name: creator_name.clone(),
                    name: name.clone(),
                    policy: *policy,
                    members: BTreeMap::from([(
                        op.author,
                        Member {
                            name: creator_name.clone(),
                            added_by: op.author,
                            admitted_at_ms: op.at_ms,
                        },
                    )]),
                    admins: BTreeSet::from([op.author]),
                    names: BTreeMap::from([(op.author, creator_name.clone())]),
                    removed: BTreeMap::new(),
                });
            }
            continue;
        };
        match &op.kind {
            OpKind::Admit { member, name }
                if s.members.contains_key(&op.author)
                    && (s.policy == Policy::Open || s.admins.contains(&op.author))
                    && s.members.len() < MAX_MEMBERS =>
            {
                // A duplicate admit cannot rewrite admission time or provenance.
                if let std::collections::btree_map::Entry::Vacant(entry) = s.members.entry(*member)
                {
                    entry.insert(Member {
                        name: name.clone(),
                        added_by: op.author,
                        admitted_at_ms: op.at_ms,
                    });
                    s.names.insert(*member, name.clone());
                    s.removed.remove(member);
                }
            }
            OpKind::Leave if s.members.contains_key(&op.author) => {
                s.members.remove(&op.author);
                s.admins.remove(&op.author);
            }
            OpKind::Remove { member } if s.admins.contains(&op.author) && *member != s.creator => {
                if let Some(record) = s.members.remove(member) {
                    s.removed.insert(
                        *member,
                        RemovedMember {
                            member: record,
                            operation: *digest,
                        },
                    );
                } else if let Some(removed) = s.removed.get_mut(member) {
                    removed.operation = *digest;
                }
                s.admins.remove(member);
            }
            OpKind::Promote { member }
                if s.admins.contains(&op.author) && s.members.contains_key(member) =>
            {
                s.admins.insert(*member);
            }
            _ => {}
        }
    }
    Ok(state)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create(key: &SecretKey, policy: Policy) -> SignedOp {
        SignedOp::sign(
            key,
            [0; 32],
            100,
            OpKind::Create {
                name: "Family".into(),
                policy,
                creator_name: "Alex".into(),
            },
        )
        .unwrap()
    }

    fn admit(key: &SecretKey, group: GroupId, at_ms: u64, member: EndpointId) -> SignedOp {
        SignedOp::sign(
            key,
            group,
            at_ms,
            OpKind::Admit {
                member,
                name: "Sam".into(),
            },
        )
        .unwrap()
    }

    fn state(group: GroupId, ops: &[SignedOp]) -> State {
        derive(group, ops, 1000).unwrap().unwrap()
    }

    #[test]
    fn managed_authority_signatures_and_group_binding() {
        let a = SecretKey::generate();
        let b = SecretKey::generate();
        let c = SecretKey::generate();
        let root = create(&a, Policy::Managed);
        let group = root.digest().unwrap();
        let mut ops = vec![root, admit(&a, group, 110, b.public())];
        let expected = state(group, &ops);
        ops.push(admit(&b, group, 120, c.public()));
        ops.push(SignedOp::sign(&b, group, 121, OpKind::Remove { member: a.public() }).unwrap());
        let mut forged = SignedOp::sign(&b, group, 122, OpKind::Leave).unwrap();
        forged.op.author = a.public();
        ops.push(forged);
        ops.push(admit(&a, [1; 32], 123, c.public()));
        ops.push(admit(&a, group, 1001 + FUTURE_SKEW_MS as u64, c.public()));
        assert_eq!(state(group, &ops), expected);
        assert!(derive([1; 32], &ops, 1000).unwrap().is_none());
        let encoded = postcard::to_allocvec(&ops).unwrap();
        let decoded: Vec<SignedOp> = postcard::from_bytes(&encoded).unwrap();
        assert_eq!(decoded, ops);
    }

    #[test]
    fn promotion_removal_leave_and_readmission() {
        let a = SecretKey::generate();
        let b = SecretKey::generate();
        let c = SecretKey::generate();
        let root = create(&a, Policy::Managed);
        let group = root.digest().unwrap();
        let mut ops = vec![
            root,
            admit(&a, group, 110, b.public()),
            SignedOp::sign(&a, group, 120, OpKind::Promote { member: b.public() }).unwrap(),
            admit(&b, group, 130, c.public()),
            SignedOp::sign(&b, group, 140, OpKind::Remove { member: a.public() }).unwrap(),
            SignedOp::sign(&a, group, 150, OpKind::Remove { member: b.public() }).unwrap(),
        ];
        let removed = state(group, &ops);
        assert!(removed.members.contains_key(&a.public()));
        assert!(!removed.members.contains_key(&b.public()));
        assert!(!removed.admins.contains(&b.public()));
        assert_eq!(removed.names[&removed.members[&c.public()].added_by], "Sam");
        ops.push(admit(&a, group, 160, b.public()));
        let readmitted = state(group, &ops);
        assert_eq!(readmitted.members[&b.public()].admitted_at_ms, 160);
        assert!(!readmitted.admins.contains(&b.public()));
        ops.push(SignedOp::sign(&a, group, 170, OpKind::Remove { member: c.public() }).unwrap());
        ops.push(SignedOp::sign(&b, group, 180, OpKind::Leave).unwrap());
        assert_eq!(state(group, &ops).members.len(), 1);
    }

    #[test]
    fn every_arrival_order_and_duplicate_gives_same_state() {
        fn permutations(ops: &mut [SignedOp], start: usize, group: GroupId, expected: &State) {
            if start == ops.len() {
                assert_eq!(&state(group, ops), expected);
                return;
            }
            for i in start..ops.len() {
                ops.swap(start, i);
                permutations(ops, start + 1, group, expected);
                ops.swap(start, i);
            }
        }
        let a = SecretKey::generate();
        let b = SecretKey::generate();
        let c = SecretKey::generate();
        let root = create(&a, Policy::Open);
        let group = root.digest().unwrap();
        let mut ops = vec![
            root,
            admit(&a, group, 110, b.public()),
            admit(&b, group, 120, c.public()),
            SignedOp::sign(&a, group, 130, OpKind::Remove { member: b.public() }).unwrap(),
            admit(&a, group, 130, b.public()),
        ];
        let expected = state(group, &ops);
        permutations(&mut ops, 0, group, &expected);
        ops.extend(ops.clone());
        assert_eq!(state(group, &ops), expected);
    }

    #[test]
    fn duplicate_admit_preserves_history_and_leave_revokes_admin() {
        let a = SecretKey::generate();
        let b = SecretKey::generate();
        let root = create(&a, Policy::Open);
        let group = root.digest().unwrap();
        let mut ops = vec![
            root,
            admit(&a, group, 110, b.public()),
            admit(&b, group, 120, b.public()),
        ];
        assert_eq!(state(group, &ops).members[&b.public()].admitted_at_ms, 110);
        ops.push(SignedOp::sign(&a, group, 130, OpKind::Leave).unwrap());
        ops.push(admit(&b, group, 140, a.public()));
        ops.push(SignedOp::sign(&a, group, 150, OpKind::Remove { member: b.public() }).unwrap());
        let s = state(group, &ops);
        assert!(s.admins.is_empty());
        assert_eq!(s.members.len(), 2);
    }

    #[test]
    fn bounds_and_missing_anchor() {
        let a = SecretKey::generate();
        let root = create(&a, Policy::Managed);
        let group = root.digest().unwrap();
        let mut ops = vec![root];
        for i in 0..MAX_MEMBERS {
            ops.push(admit(
                &a,
                group,
                110 + i as u64,
                SecretKey::generate().public(),
            ));
        }
        assert_eq!(state(group, &ops).members.len(), MAX_MEMBERS);
        assert!(derive(group, &ops[1..], 1000).unwrap().is_none());
        let duplicates = vec![ops[0].clone(); MAX_OPS + 1];
        assert_eq!(state(group, &duplicates).members.len(), 1);
        for i in ops.len()..=MAX_OPS {
            ops.push(
                SignedOp::sign(
                    &a,
                    group,
                    200 + i as u64,
                    OpKind::Promote { member: a.public() },
                )
                .unwrap(),
            );
        }
        assert!(derive(group, &ops, 1000).is_err());
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GroupOps {
    pub group: GroupId,
    pub ops: Vec<SignedOp>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GroupAck {
    pub group: GroupId,
    pub digest: GroupId,
}

#[derive(Serialize, Deserialize)]
pub enum Packet {
    Ops(Vec<GroupOps>),
    Ack(Vec<GroupAck>),
}

pub fn id_string(id: GroupId) -> String {
    id.iter().map(|b| format!("{b:02x}")).collect()
}

pub fn parse_id(id: &str) -> Result<GroupId> {
    if id.len() != 64
        || !id
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err("invalid group id".into());
    }
    let mut bytes = [0; 32];
    for (i, byte) in bytes.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&id[i * 2..i * 2 + 2], 16)?;
    }
    Ok(bytes)
}

/// Receipt rejects invalid signatures/timestamps instead of persisting poisoned input.
/// Authorization-invalid signed operations stay in the union, as required by replay.
pub fn merge(
    group: GroupId,
    stored: &[SignedOp],
    incoming: &[SignedOp],
    now_ms: u64,
) -> Result<Vec<SignedOp>> {
    if incoming.len() > MAX_OPS || stored.len() > MAX_OPS {
        return Err("too many group operations".into());
    }
    let mut union = BTreeMap::new();
    for signed in stored.iter().chain(incoming) {
        if !signed.valid(group, now_ms)? {
            return Err("invalid group operation".into());
        }
        if let OpKind::Create { creator_name, .. } = &signed.op.kind {
            super::peers::validate_name(creator_name)?;
        }
        match &signed.op.kind {
            OpKind::Create { name, .. } | OpKind::Admit { name, .. } => {
                super::peers::validate_name(name)?
            }
            _ => {}
        }
        union.insert(signed.digest()?, signed.clone());
    }
    if union.len() > MAX_OPS {
        return Err("too many group operations".into());
    }
    Ok(union.into_values().collect())
}

pub fn log_digest(ops: &[SignedOp]) -> Result<GroupId> {
    let mut hashes = ops
        .iter()
        .map(SignedOp::digest)
        .collect::<Result<Vec<_>>>()?;
    hashes.sort_unstable();
    hashes.dedup();
    Ok(*blake3::hash(&postcard::to_allocvec(&hashes)?).as_bytes())
}
