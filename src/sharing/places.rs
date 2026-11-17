//! Local arrival/departure subscriptions. Place definitions never leave this phone.
use super::{Result, node::FriendState, peers::Friends, requests::atomic_json};
use crate::geo::Point;
use iroh::EndpointId;
use serde::{Deserialize, Serialize};
use std::io::Read;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Place {
    pub id: u32,
    pub name: String,
    pub center: Point,
    pub radius_m: f64,
    pub peer: EndpointId,
    #[serde(default)]
    pub polygon: Vec<Point>,
    #[serde(default)]
    pub trigger: Trigger,
    #[serde(default)]
    state: State,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub enum Trigger {
    Entering,
    Exiting,
    #[default]
    Both,
}
pub fn configure(friends: &Friends, id: u32, polygon: Vec<Point>, trigger: Trigger) -> Result<()> {
    if polygon.len() < 3 || polygon.len() > 256 || polygon.iter().any(|p| !p.valid()) {
        return Err("Draw 3–256 valid corners".into());
    }
    let _lock = friends.lock()?;
    let mut saved = load(friends)?;
    let place = saved
        .places
        .iter_mut()
        .find(|p| p.id == id)
        .ok_or("Place missing")?;
    place.polygon = polygon;
    place.trigger = trigger;
    save(friends, &saved)
}
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
struct State {
    inside: Option<bool>,
    candidate: Option<bool>,
    since_ms: i64,
    last_ms: i64,
}
#[derive(Default, Serialize, Deserialize)]
struct Saved {
    #[serde(default)]
    next_id: u32,
    places: Vec<Place>,
}
fn load(friends: &Friends) -> Result<Saved> {
    let file = match std::fs::File::open(friends.directory().join("places.json")) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Saved::default()),
        Err(e) => return Err(e.into()),
    };
    let mut bytes = Vec::new();
    file.take(65_537).read_to_end(&mut bytes)?;
    if bytes.len() > 65_536 {
        return Err("places file too large".into());
    }
    let saved: Saved = serde_json::from_slice(&bytes)?;
    if saved.places.len() > 64 {
        return Err("too many places".into());
    }
    for p in &saved.places {
        validate(&p.name, p.center, p.radius_m)?;
    }
    Ok(saved)
}
fn save(friends: &Friends, saved: &Saved) -> Result<()> {
    atomic_json(
        &friends.directory().join("places.json"),
        &serde_json::to_vec(saved)?,
    )
}
fn validate(name: &str, center: Point, radius: f64) -> Result<()> {
    if name.trim().is_empty()
        || name.chars().count() > 64
        || name.chars().any(char::is_control)
        || !center.valid()
        || !radius.is_finite()
        || !(100. ..=10_000.).contains(&radius)
    {
        return Err("Use a name up to 64 characters and radius 100–10000 m".into());
    }
    Ok(())
}
pub fn list(friends: &Friends) -> Result<Vec<Place>> {
    let _lock = friends.lock()?;
    Ok(load(friends)?.places)
}
pub fn add(
    friends: &Friends,
    name: &str,
    center: Point,
    radius: f64,
    peer: EndpointId,
) -> Result<()> {
    validate(name, center, radius)?;
    let _lock = friends.lock()?;
    if !friends
        .list_locked()?
        .iter()
        .any(|f| f.peer_id == peer && !f.blocked)
    {
        return Err("Choose a current, unblocked friend".into());
    }
    let mut saved = load(friends)?;
    if saved.places.len() >= 64 {
        return Err("Maximum 64 place alerts".into());
    }
    saved.next_id = saved
        .next_id
        .checked_add(1)
        .filter(|id| *id < 1_000_000)
        .ok_or("place ID limit reached")?;
    saved.places.push(Place {
        polygon: Vec::new(),
        trigger: Trigger::Both,
        id: saved.next_id,
        name: name.trim().into(),
        center,
        radius_m: radius,
        peer,
        state: State::default(),
    });
    save(friends, &saved)
}
pub fn remove(friends: &Friends, id: u32) -> Result<()> {
    let _lock = friends.lock()?;
    let mut saved = load(friends)?;
    saved.places.retain(|p| p.id != id);
    save(friends, &saved)
}

impl Place {
    /// First observation establishes baseline. Two confident, distinct fixes at least
    /// 15 s apart confirm transition. Gaps > 5 min reset baseline without an alert.
    fn observe(&mut self, friend: &FriendState, now: i64) -> Option<String> {
        let fix = friend.location?;
        if !friend.connected
            || !friend.sharing_active
            || friend.via.is_some()
            || now.saturating_sub(fix.timestamp_ms) > 120_000
            || fix.timestamp_ms > now
            || fix.timestamp_ms <= self.state.last_ms
        {
            return None;
        }
        let point = Point {
            lat: f64::from(fix.latitude_e7) / 1e7,
            lon: f64::from(fix.longitude_e7) / 1e7,
        };
        let accuracy = f64::from(fix.accuracy_cm) / 100.;
        let distance = self.center.distance(point);
        let inside = if self.polygon.len() >= 3 {
            let points: Vec<_> = self
                .polygon
                .iter()
                .map(|p| crate::geo::local(point, *p))
                .collect();
            let mut inside = false;
            let mut edge_distance = f64::INFINITY;
            for i in 0..points.len() {
                let a = points[i];
                let b = points[(i + 1) % points.len()];
                let dx = b[0] - a[0];
                let dy = b[1] - a[1];
                let length = dx * dx + dy * dy;
                let t = if length > 0. {
                    (-(a[0] * dx + a[1] * dy) / length).clamp(0., 1.)
                } else {
                    0.
                };
                edge_distance = edge_distance.min((a[0] + t * dx).hypot(a[1] + t * dy));
                if (a[1] > 0.) != (b[1] > 0.) && 0. < a[0] - a[1] * dx / dy {
                    inside = !inside;
                }
            }
            if edge_distance <= accuracy + 25. {
                None
            } else {
                Some(inside)
            }
        } else if distance + accuracy < self.radius_m {
            Some(true)
        } else if distance - accuracy > self.radius_m + 25. {
            Some(false)
        } else {
            None
        };
        let gap = fix.timestamp_ms.saturating_sub(self.state.last_ms) > 300_000;
        self.state.last_ms = fix.timestamp_ms;
        if gap {
            self.state.inside = None;
        }
        let Some(inside) = inside else {
            self.state.candidate = None;
            return None;
        };
        if gap || self.state.inside.is_none() {
            self.state.inside = Some(inside);
            self.state.candidate = None;
            return None;
        }
        if self.state.inside == Some(inside) {
            self.state.candidate = None;
            return None;
        }
        if self.state.candidate != Some(inside) {
            self.state.candidate = Some(inside);
            self.state.since_ms = fix.timestamp_ms;
            return None;
        }
        if fix.timestamp_ms.saturating_sub(self.state.since_ms) < 15_000 {
            return None;
        }
        self.state.inside = Some(inside);
        self.state.candidate = None;
        if (inside && self.trigger == Trigger::Exiting)
            || (!inside && self.trigger == Trigger::Entering)
        {
            return None;
        }
        Some(format!(
            "{} {} {}",
            friend.name,
            if inside { "arrived at" } else { "left" },
            self.name
        ))
    }
}

pub(super) fn observe(
    friends: &Friends,
    observations: &[FriendState],
    now: i64,
) -> Result<Vec<(i32, String)>> {
    let _lock = friends.lock()?;
    let records = friends.list_locked()?;
    let mut saved = load(friends)?;
    let mut changed = false;
    let mut notices = Vec::new();
    for place in &mut saved.places {
        if !records
            .iter()
            .any(|f| f.peer_id == place.peer && !f.blocked)
        {
            if place.state != State::default() {
                place.state = State::default();
                changed = true;
            }
            continue;
        }
        if let Some(friend) = observations.iter().find(|f| f.peer_id == place.peer) {
            let old = place.state.clone();
            if let Some(text) = place.observe(friend, now) {
                notices.push((1_000_000 + place.id as i32, text));
            }
            changed |= old != place.state;
        }
    }
    if changed {
        save(friends, &saved)?;
    }
    Ok(notices)
}

/// Stationary senders normally send rarely. Ask for confirmation rather than wait
/// ten minutes after an arrival; existing request cooldown bounds GPS work.
pub(super) fn confirmation_requests(friends: &Friends, now: i64) -> Result<()> {
    let peers: std::collections::BTreeSet<_> = {
        let _lock = friends.lock()?;
        load(friends)?
            .places
            .into_iter()
            .filter(|p| {
                p.state.candidate.is_some()
                    && now.saturating_sub(p.state.since_ms) >= 15_000
                    && now.saturating_sub(p.state.last_ms) <= 120_000
            })
            .map(|p| p.peer)
            .collect()
    };
    for peer in peers {
        let _ = super::requests::queue(friends, peer, now);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sharing::protocol::LocationUpdate;
    fn observation(peer: EndpointId, timestamp: i64, lon: f64) -> FriendState {
        FriendState {
            peer_id: peer,
            name: "Sam".into(),
            location: Some(LocationUpdate {
                sequence: timestamp as u64,
                timestamp_ms: timestamp,
                latitude_e7: 0,
                longitude_e7: (lon * 1e7) as i32,
                accuracy_cm: 500,
                speed_cm_s: None,
                heading_cdeg: None,
            }),
            battery: None,
            via: None,
            connected: true,
            update_required: false,
            sharing_active: true,
            path: None,
        }
    }
    #[test]
    fn places_confirm_transitions_persist_and_ignore_stale_relay_uncertain_fixes() {
        let dir = tempfile::tempdir().unwrap();
        let key = iroh::SecretKey::generate();
        let peer = iroh::SecretKey::generate().public();
        let friends = Friends::open(dir.path(), key.public()).unwrap();
        friends.add_confirmed(peer, "Sam".into()).unwrap();
        add(&friends, "Home", Point { lat: 0., lon: 0. }, 250., peer).unwrap();
        assert!(
            observe(&friends, &[observation(peer, 100_000, 0.01)], 100_000)
                .unwrap()
                .is_empty()
        );
        assert!(
            observe(&friends, &[observation(peer, 130_000, 0.)], 130_000)
                .unwrap()
                .is_empty()
        );
        let notices = observe(&friends, &[observation(peer, 145_000, 0.)], 145_000).unwrap();
        assert_eq!(notices.len(), 1);
        assert_eq!(notices[0].1, "Sam arrived at Home");
        assert!(
            observe(&friends, &[observation(peer, 145_000, 0.)], 146_000)
                .unwrap()
                .is_empty()
        );
        let friends = Friends::open(dir.path(), key.public()).unwrap();
        assert!(
            observe(&friends, &[observation(peer, 175_000, 0.01)], 175_000)
                .unwrap()
                .is_empty()
        );
        let notices = observe(&friends, &[observation(peer, 190_000, 0.01)], 190_000).unwrap();
        assert_eq!(notices[0].1, "Sam left Home");
        let mut relayed = observation(peer, 220_000, 0.);
        relayed.via = Some(key.public());
        assert!(observe(&friends, &[relayed], 220_000).unwrap().is_empty());
        assert!(
            observe(&friends, &[observation(peer, 220_000, 0.)], 500_000)
                .unwrap()
                .is_empty()
        );
        // Reconnect after long gap establishes new baseline; no invented arrival.
        assert!(
            observe(&friends, &[observation(peer, 600_000, 0.)], 600_000)
                .unwrap()
                .is_empty()
        );
        let mut uncertain = observation(peer, 630_000, 0.002);
        uncertain.location.as_mut().unwrap().accuracy_cm = 20_000;
        assert!(observe(&friends, &[uncertain], 630_000).unwrap().is_empty());
        friends.block(peer).unwrap();
        assert!(
            observe(&friends, &[observation(peer, 660_000, 0.01)], 660_000)
                .unwrap()
                .is_empty()
        );
        assert_eq!(list(&friends).unwrap()[0].state, State::default());
    }
    #[test]
    fn invalid_places_fail_and_remove_clears_subscription() {
        let dir = tempfile::tempdir().unwrap();
        let key = iroh::SecretKey::generate();
        let peer = iroh::SecretKey::generate().public();
        let friends = Friends::open(dir.path(), key.public()).unwrap();
        friends.add_confirmed(peer, "Sam".into()).unwrap();
        assert!(add(&friends, "", Point { lat: 0., lon: 0. }, 250., peer).is_err());
        assert!(add(&friends, "Home", Point { lat: 0., lon: 0. }, f64::NAN, peer).is_err());
        assert!(add(&friends, "Home", Point { lat: 91., lon: 0. }, 250., peer).is_err());
        add(&friends, "Home", Point { lat: 0., lon: 0. }, 250., peer).unwrap();
        remove(&friends, list(&friends).unwrap()[0].id).unwrap();
        assert!(list(&friends).unwrap().is_empty());
    }

    #[test]
    fn pending_transition_requests_fresh_confirmation_fix() {
        let dir = tempfile::tempdir().unwrap();
        let key = iroh::SecretKey::generate();
        let peer = iroh::SecretKey::generate().public();
        let friends = Friends::open(dir.path(), key.public()).unwrap();
        friends.add_confirmed(peer, "Sam".into()).unwrap();
        add(&friends, "Home", Point { lat: 0., lon: 0. }, 250., peer).unwrap();
        observe(&friends, &[observation(peer, 100_000, 0.)], 100_000).unwrap();
        observe(&friends, &[observation(peer, 130_000, 0.01)], 130_000).unwrap();

        confirmation_requests(&friends, 145_000).unwrap();
        let request = super::super::requests::read(&friends, peer).unwrap();
        assert_eq!(request.at_ms, 145_000);
        assert_eq!(request.status, "Locating…");
    }
}
