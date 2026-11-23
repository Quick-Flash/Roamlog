//! Check-ins while Roamlog is closed. Without a server, two phones exchange only while both
//! are online, so every phone comes online at the same clock times, exchanges, then
//! disconnects fully so radio, GPS and CPU sleep in between. The app being open, or a
//! friend's locate request, keeps the node connected instead.
//!
//! Clocks: phones send their (aligned) time in pings. Each phone runs its schedule on the
//! furthest-ahead clock among its friends, so friends converge on one schedule even when a
//! phone's clock was set by hand, with no time server and no one deciding which is right.
use iroh::EndpointId;
use std::{collections::BTreeMap, sync::Mutex};

/// Check-in every 5 minutes, on the (aligned) clock.
pub const SLOT_MS: i64 = 5 * 60_000;
/// GPS starts this long before a check-in so a fix is ready to send.
pub const LEAD_MS: i64 = 15_000;
/// Online this long from check-in: covers remaining clock skew and connection setup.
/// Exchanges took under 10 s in tests; CPU stays awake for lead plus window.
pub const WINDOW_MS: i64 = 25_000;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Phase {
    /// App open or answering a friend's locate request: stay connected.
    Live,
    /// GPS warming up for the check-in at this (aligned) time.
    Prep(i64),
    /// Online for the check-in that started at this (aligned) time.
    Window(i64),
    /// Everything off until the check-in at this (aligned) time.
    Idle(i64),
}

impl Phase {
    /// Phase at aligned time `now`; `live` keeps the node connected.
    pub fn at(now: i64, live: bool) -> Self {
        if live {
            return Self::Live;
        }
        let start = now.div_euclid(SLOT_MS) * SLOT_MS;
        if now - start < WINDOW_MS {
            return Self::Window(start);
        }
        let next = start + SLOT_MS;
        if next - now <= LEAD_MS {
            Self::Prep(next)
        } else {
            Self::Idle(next)
        }
    }

    /// Aligned time this phase ends; `None` for Live (ends when the app closes).
    pub fn end(self) -> Option<i64> {
        match self {
            Self::Live => None,
            Self::Prep(at) => Some(at),
            Self::Window(at) => Some(at + WINDOW_MS),
            Self::Idle(at) => Some(at - LEAD_MS),
        }
    }

    pub fn online(self) -> bool {
        matches!(self, Self::Live | Self::Window(_))
    }

    /// Check-in this phase belongs to, for one GPS fix per check-in.
    pub fn slot(self) -> Option<i64> {
        match self {
            Self::Prep(at) | Self::Window(at) => Some(at),
            Self::Live | Self::Idle(_) => None,
        }
    }
}

/// Ignored below this: network delay and normal clock jitter.
const SKEW_MS: i64 = 3_000;
/// Forget a friend's clock after a day without hearing it.
const CLOCK_TTL_MS: i64 = 24 * 3_600_000;
/// Older app versions send counters in pings; real clock readings are far above this.
const MIN_TIME_MS: u64 = 1_000_000_000_000;

/// Per friend: (their aligned time minus our own clock, when measured on our clock).
static CLOCKS: Mutex<BTreeMap<EndpointId, (i64, i64)>> = Mutex::new(BTreeMap::new());

/// Friend `peer` sent its aligned time `their_ms` (a ping nonce) at our own time `own_ms`.
pub fn heard(peer: EndpointId, their_ms: u64, own_ms: i64) {
    if their_ms < MIN_TIME_MS {
        return;
    }
    if let (Ok(mut clocks), Ok(their)) = (CLOCKS.lock(), i64::try_from(their_ms)) {
        clocks.insert(peer, (their - own_ms, own_ms));
    }
}

/// How far to run ahead of our own clock: the furthest-ahead friend, if notably ahead.
pub fn correction(own_ms: i64) -> i64 {
    let Ok(clocks) = CLOCKS.lock() else {
        return 0;
    };
    clocks
        .values()
        .filter(|(_, at)| (0..CLOCK_TTL_MS).contains(&(own_ms - at)))
        .map(|(offset, _)| *offset)
        .filter(|offset| *offset >= SKEW_MS)
        .max()
        .unwrap_or(0)
}

/// Aligned time: what check-ins and outgoing pings use.
pub fn now(own_ms: i64) -> i64 {
    own_ms + correction(own_ms)
}

/// Phase at own time `own_ms` (live until `live_until`, own clock), and when it ends on the
/// own clock, for timers and alarms.
pub fn schedule(own_ms: i64, live_until: i64) -> (Phase, i64) {
    let correction = correction(own_ms);
    let phase = Phase::at(own_ms + correction, own_ms < live_until);
    let end = phase.end().map_or(live_until, |at| at - correction);
    (phase, end)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phases_follow_the_shared_schedule() {
        let slot = 10 * SLOT_MS;
        assert_eq!(Phase::at(slot - LEAD_MS - 1, false), Phase::Idle(slot));
        assert_eq!(Phase::at(slot - LEAD_MS, false), Phase::Prep(slot));
        assert_eq!(Phase::at(slot, false), Phase::Window(slot));
        assert_eq!(Phase::at(slot + WINDOW_MS - 1, false), Phase::Window(slot));
        assert_eq!(
            Phase::at(slot + WINDOW_MS, false),
            Phase::Idle(slot + SLOT_MS)
        );
        assert_eq!(Phase::at(slot + 1, true), Phase::Live);
        assert_eq!(Phase::Prep(slot).end(), Some(slot));
        assert_eq!(Phase::Window(slot).end(), Some(slot + WINDOW_MS));
        assert_eq!(Phase::Idle(slot).end(), Some(slot - LEAD_MS));
        assert!(Phase::Window(slot).online() && !Phase::Prep(slot).online());
        // Long before any recorded friend clock: own clock, no correction.
        assert_eq!(schedule(slot, 0), (Phase::Window(slot), slot + WINDOW_MS));
        assert_eq!(schedule(slot, slot + 9), (Phase::Live, slot + 9));
    }

    #[test]
    fn clocks_align_to_the_furthest_ahead_friend() {
        let key = |n: u8| iroh::SecretKey::from_bytes(&[n; 32]).public();
        let (ahead, close, old) = (key(1), key(2), key(3));
        let own = 1_800_000_000_000;
        heard(old, 7, own); // older version: ping counter, not a time
        heard(close, (own + 1_000) as u64, own); // network delay: ignored
        assert_eq!(correction(own), 0);
        heard(ahead, (own + 60_000) as u64, own);
        assert_eq!(correction(own), 60_000);
        assert_eq!(now(own), own + 60_000);
        // A day without hearing it: back to own clock.
        assert_eq!(correction(own + CLOCK_TTL_MS), 0);
    }
}
