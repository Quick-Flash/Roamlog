//! Versioned, bounded binary location protocol. Sender identity comes from QUIC.
use super::Result;
use serde::{Deserialize, Serialize};

pub const ALPN: &[u8] = b"roamlog/location/1";
pub const MAX_FRAME: usize = 256;
/// Above sender's current-fix window; sequence numbers, not age, prevent replay.
pub const MAX_AGE_MS: i64 = 1_200_000;
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
        if self.sequence == 0
            || self.timestamp_ms <= 0
            || now_ms <= 0
            || self.timestamp_ms > now_ms.saturating_add(FUTURE_SKEW_MS)
            || self.timestamp_ms < now_ms.saturating_sub(MAX_AGE_MS)
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

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Message {
    Hello,
    LocationUpdate(LocationUpdate),
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
    if let Message::LocationUpdate(location) = &envelope.message {
        location.validate(now_ms)?;
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
