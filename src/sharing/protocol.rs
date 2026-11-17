//! Versioned, bounded binary location protocol. Sender identity comes from QUIC.
use super::{Result, groups::GroupId};
use iroh::{EndpointId, SecretKey, Signature};
use serde::{Deserialize, Serialize};

pub const ALPN: &[u8] = b"roamlog/location/2";
pub const MAX_FRAME: usize = 256;
/// Above sender's current-fix window; sequence numbers, not age, prevent replay.
pub const MAX_AGE_MS: i64 = 1_200_000;
pub const MAX_FORWARD_AGE_MS: i64 = 86_400_000;
pub const FUTURE_SKEW_MS: i64 = 120_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocationUpdate {
    pub sequence: u64,
    pub timestamp_ms: i64,
    pub latitude_e7: i32,
    pub longitude_e7: i32,
    pub accuracy_cm: u32,
    pub speed_cm_s: Option<u32>,
    pub heading_cdeg: Option<u16>,
}

impl LocationUpdate {
    pub fn validate(&self, now_ms: i64) -> Result<()> {
        self.validate_age(now_ms, MAX_AGE_MS)
    }

    fn validate_age(&self, now_ms: i64, max_age_ms: i64) -> Result<()> {
        if self.sequence == 0
            || self.timestamp_ms <= 0
            || now_ms <= 0
            || self.timestamp_ms > now_ms.saturating_add(FUTURE_SKEW_MS)
            || self.timestamp_ms < now_ms.saturating_sub(max_age_ms)
            || !(-900_000_000..=900_000_000).contains(&self.latitude_e7)
            || !(-1_800_000_000..=1_800_000_000).contains(&self.longitude_e7)
            || self.accuracy_cm > 10_000_000
            || self.speed_cm_s.is_some_and(|speed| speed > 100_000)
            || self.heading_cdeg.is_some_and(|heading| heading >= 36_000)
        {
            return Err("invalid or stale location update".into());
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Availability {
    Off,
    TripInactive,
    NoFix,
    Active,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum GroupPayload {
    Fix(LocationUpdate),
    Unavailable(Availability),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GroupFix {
    pub group: GroupId,
    pub origin: EndpointId,
    pub sequence: u64,
    /// Needed for Unavailable retention and admission cutoffs as well as fixes.
    pub timestamp_ms: i64,
    pub payload: GroupPayload,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignedGroupFix {
    pub fix: GroupFix,
    pub sig: Signature,
}

fn fix_message(fix: &GroupFix) -> Result<Vec<u8>> {
    let mut bytes = b"roamlog/group-fix/1".to_vec();
    bytes.extend(postcard::to_allocvec(fix)?);
    Ok(bytes)
}

impl SignedGroupFix {
    pub fn sign(key: &SecretKey, fix: GroupFix) -> Result<Self> {
        if key.public() != fix.origin {
            return Err("group fix signing identity mismatch".into());
        }
        let sig = key.sign(&fix_message(&fix)?);
        Ok(Self { fix, sig })
    }

    pub fn validate(&self, now_ms: i64) -> Result<()> {
        let fix = &self.fix;
        if fix.sequence == 0
            || fix.timestamp_ms <= 0
            || now_ms <= 0
            || fix.timestamp_ms < now_ms.saturating_sub(MAX_FORWARD_AGE_MS)
            || fix.timestamp_ms > now_ms.saturating_add(FUTURE_SKEW_MS)
        {
            return Err("invalid or stale group fix".into());
        }
        match fix.payload {
            GroupPayload::Fix(update) => {
                update.validate_age(now_ms, MAX_FORWARD_AGE_MS)?;
                if update.sequence != fix.sequence || update.timestamp_ms != fix.timestamp_ms {
                    return Err("group fix metadata mismatch".into());
                }
            }
            GroupPayload::Unavailable(Availability::Active) => {
                return Err("invalid unavailable marker".into());
            }
            _ => {}
        }
        fix.origin.verify(&fix_message(fix)?, &self.sig)?;
        Ok(())
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Message {
    Hello,
    LocationUpdate(LocationUpdate),
    GroupFix(SignedGroupFix),
    LocationRequest,
    SharingStatus(Availability),
    /// Remote notification only. Never changes this device's outgoing permissions.
    PermissionUpdate {
        allowed: bool,
    },
    Ping {
        nonce: u64,
    },
    Pong {
        nonce: u64,
    },
}

#[derive(Serialize, Deserialize)]
struct Envelope {
    version: u8,
    message: Message,
}

pub fn encode(message: &Message) -> Result<Vec<u8>> {
    let frame = postcard::to_allocvec(&Envelope {
        version: 1,
        message: message.clone(),
    })?;
    if frame.len() > MAX_FRAME {
        return Err("location frame too large".into());
    }
    Ok(frame)
}

pub fn decode(frame: &[u8], now_ms: i64) -> Result<Message> {
    if frame.is_empty() || frame.len() > MAX_FRAME {
        return Err("invalid location frame size".into());
    }
    let (envelope, trailing): (Envelope, _) = postcard::take_from_bytes(frame)?;
    if envelope.version != 1 || !trailing.is_empty() {
        return Err("unsupported or malformed location frame".into());
    }
    match &envelope.message {
        Message::LocationUpdate(location) => location.validate(now_ms)?,
        Message::GroupFix(fix) => fix.validate(now_ms)?,
        _ => {}
    }
    Ok(envelope.message)
}

#[cfg(test)]
mod tests {
    use super::*;
    pub(super) fn location() -> LocationUpdate {
        LocationUpdate {
            sequence: 1,
            timestamp_ms: 1_000_000,
            latitude_e7: 400_000_000,
            longitude_e7: -1_110_000_000,
            accuracy_cm: 500,
            speed_cm_s: Some(1000),
            heading_cdeg: Some(35999),
        }
    }

    #[test]
    fn binary_frames_are_bounded_versioned_and_strict() {
        let location = location();
        for message in [
            Message::Hello,
            Message::LocationUpdate(location),
            Message::LocationRequest,
            Message::SharingStatus(Availability::Off),
            Message::PermissionUpdate { allowed: false },
            Message::Ping { nonce: 4 },
            Message::Pong { nonce: 4 },
        ] {
            let frame = encode(&message).unwrap();
            assert!(decode(&frame, location.timestamp_ms).unwrap() == message);
            let mut trailing = frame.clone();
            trailing.push(0);
            assert!(decode(&trailing, location.timestamp_ms).is_err());
            let mut future_version = frame;
            future_version[0] = 2;
            assert!(decode(&future_version, location.timestamp_ms).is_err());
        }
        assert!(decode(&[], 1).is_err());
        assert!(decode(&[0; MAX_FRAME + 1], 1).is_err());
        let mut invalid = location;
        invalid.latitude_e7 = 900_000_001;
        assert!(
            decode(
                &encode(&Message::LocationUpdate(invalid)).unwrap(),
                location.timestamp_ms
            )
            .is_err()
        );
        assert!(
            location
                .validate(location.timestamp_ms + MAX_AGE_MS + 1)
                .is_err()
        );
        assert!(
            location
                .validate(location.timestamp_ms - FUTURE_SKEW_MS - 1)
                .is_err()
        );
    }
}

#[cfg(test)]
mod relay_tests {
    use super::*;
    #[test]
    fn worst_case_group_frame_fits_and_signature_binds_payload() {
        let key = SecretKey::generate();
        let update = LocationUpdate {
            sequence: u64::MAX,
            timestamp_ms: i64::MAX,
            latitude_e7: -900_000_000,
            longitude_e7: -1_800_000_000,
            accuracy_cm: 10_000_000,
            speed_cm_s: Some(100_000),
            heading_cdeg: Some(35_999),
        };
        let signed = SignedGroupFix::sign(
            &key,
            GroupFix {
                group: [255; 32],
                origin: key.public(),
                sequence: u64::MAX,
                timestamp_ms: i64::MAX,
                payload: GroupPayload::Fix(update),
            },
        )
        .unwrap();
        let bytes = encode(&Message::GroupFix(signed.clone())).unwrap();
        assert!(bytes.len() <= MAX_FRAME);
        assert!(matches!(decode(&bytes,i64::MAX).unwrap(),Message::GroupFix(fix) if fix==signed));
        let mut forged = signed;
        forged.fix.group = [0; 32];
        assert!(forged.validate(i64::MAX).is_err());
    }
}
