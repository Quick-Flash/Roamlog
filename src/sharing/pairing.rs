//! Invitation pairing over authenticated QUIC. Pairing never grants location access.
use super::{
    Result,
    groups::{self, GroupOps, Policy, State},
    peers::Permission,
    peers::{Friends, validate_name},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use iroh::{
    Endpoint, EndpointAddr, EndpointId, SecretKey,
    endpoint::{Connection, RecvStream, SendStream},
};
use serde::{Deserialize, Serialize};
use std::time::Duration;
use subtle::ConstantTimeEq;

pub const ALPN: &[u8] = b"roamlog/pair/2";
const PREFIX: &str = "roamlog://pair/";
const MAX_LINK: usize = 2048;
const MAX_FRAME: usize = 256 * 1024;
const NETWORK_TIMEOUT: Duration = Duration::from_secs(30);
pub const CONFIRM_TIMEOUT: Duration = Duration::from_secs(300);

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FriendInvite {
    pub version: u8,
    pub peer_id: EndpointId,
    pub display_name: String,
    pub address: EndpointAddr,
    token: [u8; 32],
    /// Group invitation: joiner becomes member and shares with every member.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<GroupRef>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GroupRef {
    pub id: String,
    pub name: String,
}

/// Relay plus a few IPv4 hints keep QR codes scannable. IPv6 and remaining direct
/// addresses are found after connecting; discovery also resolves the peer ID.
fn compact(mut address: EndpointAddr) -> EndpointAddr {
    let mut ipv4 = 0;
    address.addrs.retain(|addr| match addr {
        iroh::TransportAddr::Relay(_) => true,
        iroh::TransportAddr::Ip(ip) if ip.is_ipv4() && ipv4 < 3 => {
            ipv4 += 1;
            true
        }
        _ => false,
    });
    address
}

impl FriendInvite {
    pub fn new(endpoint: &Endpoint, display_name: String) -> Result<Self> {
        validate_name(&display_name)?;
        let invite = Self {
            version: 2,
            peer_id: endpoint.id(),
            display_name,
            address: compact(endpoint.addr()),
            token: SecretKey::generate().to_bytes(),
            group: None,
        };
        invite.to_link()?;
        Ok(invite)
    }

    fn validate(&self) -> Result<()> {
        validate_name(&self.display_name)?;
        if let Some(group) = &self.group {
            validate_name(&group.name)?;
            if group.id.len() != 64 || !group.id.bytes().all(|b| b.is_ascii_hexdigit()) {
                return Err("invalid group invitation".into());
            }
        }
        if self.version != 2 || self.address.id != self.peer_id || self.address.addrs.len() > 16 {
            return Err("invalid invitation version, identity, or addresses".into());
        }
        Ok(())
    }

    pub fn to_link(&self) -> Result<String> {
        self.validate()?;
        let link = format!(
            "{PREFIX}{}",
            URL_SAFE_NO_PAD.encode(serde_json::to_vec(self)?)
        );
        if link.len() > MAX_LINK {
            return Err("invitation too large".into());
        }
        Ok(link)
    }

    pub fn from_link(link: &str) -> Result<Self> {
        if link.len() > MAX_LINK {
            return Err("invitation too large".into());
        }
        let encoded = link
            .strip_prefix(PREFIX)
            .ok_or("not a Roamlog invitation")?;
        let invite: Self = serde_json::from_slice(&URL_SAFE_NO_PAD.decode(encoded)?)?;
        invite.validate()?;
        Ok(invite)
    }
}

/// QR module rows, excluding quiet zone. Caller must render four white modules on each side.
pub fn qr_modules(link: &str) -> Result<Vec<String>> {
    FriendInvite::from_link(link)?;
    let code = qrcodegen::QrCode::encode_text(link, qrcodegen::QrCodeEcc::Medium)?;
    Ok((0..code.size())
        .map(|y| {
            (0..code.size())
                .map(|x| if code.get_module(x, y) { '1' } else { '0' })
                .collect()
        })
        .collect())
}

/// Decode selected QR image. Pixel and payload limits apply before parsing invitations.
pub fn decode_qr(width: usize, height: usize, gray: &[u8]) -> Result<String> {
    if width == 0
        || height == 0
        || width > 2048
        || height > 2048
        || width.checked_mul(height) != Some(gray.len())
    {
        return Err("invalid QR image size".into());
    }
    let mut decoder = quircs::Quirc::default();
    let mut found = None;
    for code in decoder.identify(width, height, gray) {
        let Ok(code) = code else { continue };
        let Ok(data) = code.decode() else { continue };
        let Ok(link) = String::from_utf8(data.payload) else {
            continue;
        };
        if FriendInvite::from_link(&link).is_ok() {
            if found.is_some() {
                return Err("image contains multiple Roamlog invitations".into());
            }
            found = Some(link);
        }
    }
    found.ok_or_else(|| "no valid Roamlog invitation found".into())
}

pub async fn bind(key: SecretKey) -> Result<Endpoint> {
    Ok(Endpoint::builder(iroh::endpoint::presets::N0)
        .secret_key(key)
        .alpns(vec![ALPN.to_vec()])
        .bind()
        .await?)
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
enum Message {
    Request {
        version: u8,
        token: [u8; 32],
        name: String,
    },
    Offer {
        version: u8,
        name: String,
        group: Option<GroupOps>,
    },
    Admission(GroupOps),
    Decision {
        accepted: bool,
    },
    Complete,
}

async fn send(stream: &mut SendStream, message: &Message) -> Result<()> {
    let bytes = postcard::to_allocvec(message)?;
    if bytes.len() > MAX_FRAME {
        return Err("pairing frame too large".into());
    }
    stream
        .write_all(&(bytes.len() as u32).to_be_bytes())
        .await?;
    stream.write_all(&bytes).await?;
    Ok(())
}

async fn receive(stream: &mut RecvStream) -> Result<Message> {
    let mut size = [0; 4];
    stream.read_exact(&mut size).await?;
    let size = u32::from_be_bytes(size) as usize;
    if size == 0 || size > MAX_FRAME {
        return Err("invalid pairing frame size".into());
    }
    let mut bytes = vec![0; size];
    stream.read_exact(&mut bytes).await?;
    Ok(postcard::from_bytes(&bytes)?)
}

pub struct Pending {
    pub peer_id: EndpointId,
    pub display_name: String,
    /// TLS exporter binds verification to this connection, not just copied names or IDs.
    pub verification_code: String,
    /// Group invitation and own member name.
    pub group: Option<GroupRef>,
    pub group_state: Option<State>,
    inviter: bool,
    key: SecretKey,
    me: String,
    local_id: EndpointId,
    connection: Connection,
    send: SendStream,
    recv: RecvStream,
}

impl Drop for Pending {
    fn drop(&mut self) {
        self.connection.close(0u32.into(), b"pairing ended");
    }
}

impl Pending {
    pub fn is_inviter(&self) -> bool {
        self.inviter
    }

    fn new(
        key: SecretKey,
        inviter: bool,
        connection: Connection,
        name: String,
        group: (Option<GroupRef>, String, Option<State>),
        send: SendStream,
        recv: RecvStream,
    ) -> Result<Self> {
        let mut code = [0; 6];
        connection
            .export_keying_material(&mut code, b"roamlog-pair-verification-v1", b"")
            .map_err(|_| "pairing verification unavailable")?;
        Ok(Self {
            peer_id: connection.remote_id(),
            display_name: name,
            verification_code: format!(
                "{:02X}{:02X} {:02X}{:02X} {:02X}{:02X}",
                code[0], code[1], code[2], code[3], code[4], code[5]
            ),
            group: group.0,
            group_state: group.2,
            inviter,
            local_id: key.public(),
            key,
            me: group.1,
            connection,
            send,
            recv,
        })
    }

    /// Local user must compare code on both phones before explicitly accepting.
    /// Persists only after both votes are affirmative; all new permissions stay Off.
    pub async fn confirm(
        mut self,
        accepted: bool,
        friends: &Friends,
        share: Permission,
    ) -> Result<()> {
        if friends.owner() != self.local_id {
            return Err("friend store identity mismatch".into());
        }
        tokio::time::timeout(CONFIRM_TIMEOUT, async {
            let accepted = accepted && friends.may_pair(self.peer_id)?;
            send(&mut self.send, &Message::Decision { accepted }).await?;
            if !accepted {
                return Err("pairing declined".into());
            }
            match receive(&mut self.recv).await? {
                Message::Decision { accepted: true } => {}
                _ => return Err("pairing declined or invalid decision".into()),
            }
            if let Some(group) = &self.group {
                if self.inviter {
                    let admission = friends.admit_member(
                        &self.key,
                        &group.id,
                        self.peer_id,
                        &self.display_name,
                    )?;
                    send(&mut self.send, &Message::Admission(admission)).await?;
                } else {
                    let Message::Admission(admission) = receive(&mut self.recv).await? else {
                        return Err("missing signed group admission".into());
                    };
                    if groups::id_string(admission.group) != group.id {
                        return Err("wrong group admission".into());
                    }
                    friends.join_group(&admission, &self.me, share)?;
                }
            }
            friends.add_confirmed(self.peer_id, self.display_name.clone())?;
            send(&mut self.send, &Message::Complete).await?;
            self.send.finish()?;
            if !matches!(receive(&mut self.recv).await?, Message::Complete) {
                return Err("invalid pairing completion".into());
            }
            self.send.stopped().await?;
            Ok(())
        })
        .await?
    }
}

pub async fn request(
    endpoint: &Endpoint,
    invite: &FriendInvite,
    name: &str,
    friends: &Friends,
) -> Result<Pending> {
    invite.validate()?;
    validate_name(name)?;
    let own_name = name.to_owned();
    if friends.owner() != endpoint.id() || !friends.may_pair(invite.peer_id)? {
        return Err("pairing not allowed".into());
    }
    tokio::time::timeout(NETWORK_TIMEOUT, async {
        let connection = endpoint.connect(invite.address.clone(), ALPN).await?;
        let (mut send_stream, mut recv) = connection.open_bi().await?;
        send(
            &mut send_stream,
            &Message::Request {
                version: 2,
                token: invite.token,
                name: name.to_owned(),
            },
        )
        .await?;
        let Message::Offer {
            version: 2,
            name,
            group,
        } = receive(&mut recv).await?
        else {
            return Err("invalid pairing offer".into());
        };
        validate_name(&name)?;
        let state = match (&invite.group, &group) {
            (Some(reference), Some(log)) if groups::id_string(log.group) == reference.id => {
                let ops = groups::merge(log.group, &[], &log.ops, super::manager::now_ms() as u64)?;
                let state = groups::derive(log.group, &ops, super::manager::now_ms() as u64)?
                    .ok_or("missing Create")?;
                if !state.members.contains_key(&invite.peer_id)
                    || (state.policy == Policy::Managed && !state.admins.contains(&invite.peer_id))
                {
                    return Err("inviter cannot admit members".into());
                }
                Some(state)
            }
            (None, None) => None,
            _ => return Err("group invitation mismatch".into()),
        };
        Pending::new(
            endpoint.secret_key().clone(),
            false,
            connection,
            name,
            (invite.group.clone(), own_name, state),
            send_stream,
            recv,
        )
    })
    .await?
}

pub async fn accept(
    endpoint: &Endpoint,
    invite: &FriendInvite,
    friends: &Friends,
) -> Result<Pending> {
    invite.validate()?;
    if invite.peer_id != endpoint.id() || friends.owner() != endpoint.id() {
        return Err("pairing identity mismatch".into());
    }
    let incoming = endpoint.accept().await.ok_or("endpoint closed")?;
    tokio::time::timeout(NETWORK_TIMEOUT, async {
        let connection = incoming.await?;
        if connection.alpn() != ALPN || !friends.may_pair(connection.remote_id())? {
            connection.close(1u32.into(), b"pairing denied");
            return Err("pairing not allowed".into());
        }
        let (mut send_stream, mut recv) = connection.accept_bi().await?;
        let Message::Request {
            version: 2,
            token,
            name,
        } = receive(&mut recv).await?
        else {
            return Err("invalid pairing request".into());
        };
        if !bool::from(token.ct_eq(&invite.token)) {
            return Err("invalid invitation token".into());
        }
        validate_name(&name)?;
        let group = if let Some(reference) = &invite.group {
            let state = friends
                .group_state(&reference.id)?
                .ok_or("recreate legacy group")?;
            if !state.members.contains_key(&endpoint.id())
                || (state.policy == Policy::Managed && !state.admins.contains(&endpoint.id()))
            {
                return Err("inviter cannot admit members".into());
            }
            let _lock = friends.lock()?;
            friends
                .sync_for_locked(endpoint.id())?
                .into_iter()
                .find(|g| groups::id_string(g.group) == reference.id)
        } else {
            None
        };
        let state = group
            .as_ref()
            .map(|g| groups::derive(g.group, &g.ops, super::manager::now_ms() as u64))
            .transpose()?
            .flatten();
        send(
            &mut send_stream,
            &Message::Offer {
                version: 2,
                name: invite.display_name.clone(),
                group,
            },
        )
        .await?;
        Pending::new(
            endpoint.secret_key().clone(),
            true,
            connection,
            name,
            (invite.group.clone(), invite.display_name.clone(), state),
            send_stream,
            recv,
        )
    })
    .await?
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn endpoint() -> Endpoint {
        Endpoint::builder(iroh::endpoint::presets::Minimal)
            .alpns(vec![ALPN.to_vec()])
            .bind()
            .await
            .unwrap()
    }

    async fn pending(a: &Endpoint, b: &Endpoint, sa: &Friends, sb: &Friends) -> (Pending, Pending) {
        let invite = FriendInvite::new(a, "Alice".into()).unwrap();
        let (a, b) = tokio::join!(accept(a, &invite, sa), request(b, &invite, "Bob", sb));
        (a.unwrap(), b.unwrap())
    }

    #[tokio::test]
    async fn signed_group_pairing_preserves_creator_access_and_uses_joiner_choice() {
        let a = endpoint().await;
        let b = endpoint().await;
        let da = tempfile::tempdir().unwrap();
        let db = tempfile::tempdir().unwrap();
        let sa = Friends::open(da.path(), a.id()).unwrap();
        let sb = Friends::open(db.path(), b.id()).unwrap();
        let id = sa
            .create_group(
                a.secret_key(),
                "Family",
                "Alice",
                Policy::Managed,
                Permission::Always,
            )
            .unwrap();
        let mut invite = FriendInvite::new(&a, "Alice".into()).unwrap();
        invite.group = Some(GroupRef {
            id: id.clone(),
            name: "Family".into(),
        });
        let (pa, pb) = tokio::join!(accept(&a, &invite, &sa), request(&b, &invite, "Bob", &sb));
        let (pa, pb) = (pa.unwrap(), pb.unwrap());
        assert!(pa.is_inviter() && !pb.is_inviter());
        assert_eq!(pb.group_state.as_ref().unwrap().policy, Policy::Managed);
        assert!(sb.groups().unwrap().is_empty());
        let (ra, rb) = tokio::join!(
            pa.confirm(true, &sa, Permission::Off),
            pb.confirm(true, &sb, Permission::TripsOnly)
        );
        ra.unwrap();
        rb.unwrap();
        assert!(sa.may_share(b.id(), false).unwrap());
        assert!(!sb.may_share(a.id(), false).unwrap());
        assert!(sb.may_share(a.id(), true).unwrap());
        let log = sa.sync_for_locked(b.id()).unwrap().remove(0);
        sb.join_group(&log, "Bob", Permission::Off).unwrap();
        assert!(!sb.may_share(a.id(), true).unwrap());
        assert_eq!(
            sb.group_state(&id).unwrap().unwrap(),
            sa.group_state(&id).unwrap().unwrap()
        );
        assert!(
            sb.admit_member(
                b.secret_key(),
                &id,
                SecretKey::generate().public(),
                "Forged"
            )
            .is_err()
        );
        assert!(
            sb.remove_member(b.secret_key(), &id, a.id(), false)
                .is_err()
        );
        a.close().await;
        b.close().await;
    }

    #[tokio::test]
    async fn both_users_must_confirm_and_pairing_grants_nothing() {
        let a = endpoint().await;
        let b = endpoint().await;
        let da = tempfile::tempdir().unwrap();
        let db = tempfile::tempdir().unwrap();
        let sa = Friends::open(da.path(), a.id()).unwrap();
        let sb = Friends::open(db.path(), b.id()).unwrap();
        let (pa, pb) = pending(&a, &b, &sa, &sb).await;
        assert_eq!(pa.verification_code, pb.verification_code);
        assert_eq!(pa.peer_id, b.id());
        assert_eq!(pb.peer_id, a.id());
        assert!(sa.list().unwrap().is_empty() && sb.list().unwrap().is_empty());
        let code = pa.verification_code.clone();
        let (ra, rb) = tokio::join!(
            pa.confirm(true, &sa, Permission::TripsOnly),
            pb.confirm(true, &sb, Permission::TripsOnly)
        );
        ra.unwrap();
        rb.unwrap();
        assert!(!sa.may_share(b.id(), true).unwrap());
        assert!(!sb.may_share(a.id(), true).unwrap());
        let (pa, pb) = pending(&a, &b, &sa, &sb).await;
        assert_ne!(code, pa.verification_code);
        let (ra, rb) = tokio::join!(
            pa.confirm(true, &sa, Permission::TripsOnly),
            pb.confirm(false, &sb, Permission::TripsOnly)
        );
        assert!(ra.is_err() && rb.is_err());
        assert_eq!(sa.list().unwrap().len(), 1);
        a.close().await;
        b.close().await;
    }

    #[tokio::test]
    async fn rejection_or_abandoned_session_creates_no_friend() {
        let a = endpoint().await;
        let b = endpoint().await;
        let da = tempfile::tempdir().unwrap();
        let db = tempfile::tempdir().unwrap();
        let sa = Friends::open(da.path(), a.id()).unwrap();
        let sb = Friends::open(db.path(), b.id()).unwrap();
        let (pa, pb) = pending(&a, &b, &sa, &sb).await;
        let (ra, rb) = tokio::join!(
            pa.confirm(false, &sa, Permission::TripsOnly),
            pb.confirm(true, &sb, Permission::TripsOnly)
        );
        assert!(ra.is_err() && rb.is_err());
        assert!(sa.list().unwrap().is_empty() && sb.list().unwrap().is_empty());
        let (pa, pb) = pending(&a, &b, &sa, &sb).await;
        drop(pa);
        assert!(pb.confirm(true, &sb, Permission::TripsOnly).await.is_err());
        assert!(sb.list().unwrap().is_empty());
        a.close().await;
        b.close().await;
    }

    #[tokio::test]
    async fn invitation_validation_and_qr_round_trip() {
        let a = endpoint().await;
        let invite = FriendInvite::new(&a, "Alice".into()).unwrap();
        let link = invite.to_link().unwrap();
        assert_eq!(FriendInvite::from_link(&link).unwrap().peer_id, a.id());
        let mut invalid = invite.clone();
        invalid.version = 3;
        assert!(invalid.to_link().is_err());
        invalid = invite.clone();
        invalid.peer_id = SecretKey::generate().public();
        assert!(invalid.to_link().is_err());
        assert!(FriendInvite::from_link(&"x".repeat(MAX_LINK + 1)).is_err());
        assert!(FriendInvite::from_link("roamlog://pair/invalid").is_err());
        assert!(FriendInvite::new(&a, "Name\nForged".into()).is_err());
        let qr = qr_modules(&link).unwrap();
        let size = (qr.len() + 8) * 4;
        let mut gray = vec![255; size * size];
        for (y, row) in qr.iter().enumerate() {
            for (x, module) in row.bytes().enumerate() {
                if module == b'1' {
                    for dy in 0..4 {
                        for dx in 0..4 {
                            gray[((y + 4) * 4 + dy) * size + (x + 4) * 4 + dx] = 0;
                        }
                    }
                }
            }
        }
        assert_eq!(decode_qr(size, size, &gray).unwrap(), link);
        assert!(decode_qr(4096, 1, &[]).is_err());
        assert!(decode_qr(1, 1, &[]).is_err());
        a.close().await;
    }

    #[tokio::test]
    async fn wrong_token_and_oversized_frames_are_rejected() {
        let a = endpoint().await;
        let b = endpoint().await;
        let da = tempfile::tempdir().unwrap();
        let db = tempfile::tempdir().unwrap();
        let sa = Friends::open(da.path(), a.id()).unwrap();
        let sb = Friends::open(db.path(), b.id()).unwrap();
        let invite = FriendInvite::new(&a, "Alice".into()).unwrap();
        let mut wrong = invite.clone();
        wrong.token[0] ^= 1;
        let (ra, rb) = tokio::join!(accept(&a, &invite, &sa), request(&b, &wrong, "Bob", &sb));
        assert!(ra.is_err() && rb.is_err());
        let malformed = async {
            let connection = b.connect(a.addr(), ALPN).await.unwrap();
            let (mut send, _) = connection.open_bi().await.unwrap();
            send.write_all(&((MAX_FRAME + 1) as u32).to_be_bytes())
                .await
                .unwrap();
            connection.closed().await;
        };
        let (ra, _) = tokio::join!(accept(&a, &invite, &sa), malformed);
        assert!(ra.is_err());
        assert!(sa.list().unwrap().is_empty() && sb.list().unwrap().is_empty());
        a.close().await;
        b.close().await;
    }

    #[tokio::test]
    async fn blocking_during_confirmation_prevents_repair() {
        let a = endpoint().await;
        let b = endpoint().await;
        let da = tempfile::tempdir().unwrap();
        let db = tempfile::tempdir().unwrap();
        let sa = Friends::open(da.path(), a.id()).unwrap();
        let sb = Friends::open(db.path(), b.id()).unwrap();
        let (pa, pb) = pending(&a, &b, &sa, &sb).await;
        let (ra, rb) = tokio::join!(
            pa.confirm(true, &sa, Permission::TripsOnly),
            pb.confirm(true, &sb, Permission::TripsOnly)
        );
        ra.unwrap();
        rb.unwrap();
        let (pa, pb) = pending(&a, &b, &sa, &sb).await;
        sa.block(b.id()).unwrap();
        let (ra, rb) = tokio::join!(
            pa.confirm(true, &sa, Permission::TripsOnly),
            pb.confirm(true, &sb, Permission::TripsOnly)
        );
        assert!(ra.is_err() && rb.is_err());
        assert!(!sa.may_pair(b.id()).unwrap());
        assert!(!sa.may_share(b.id(), true).unwrap());
        assert!(!sb.may_share(a.id(), true).unwrap());
        a.close().await;
        b.close().await;
    }
}
