//! Local arrival/departure subscriptions. Place definitions never leave this phone.
use super::{
    Result,
    node::FriendState,
    peers::{Friend, Friends},
    requests::atomic_json,
};
use crate::geo::Point;
use iroh::EndpointId;
use serde::{Deserialize, Serialize};
use std::io::Read;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Place {
    pub id: u32,
    #[serde(default)]
    pub area_id: u32,
    pub name: String,
    pub center: Point,
    pub radius_m: f64,
    pub peer: EndpointId,
    #[serde(default)]
    pub no_rule: bool,
    #[serde(default)]
    pub polygon: Vec<Point>,
    #[serde(default)]
    pub trigger: Trigger,
    /// Group rule: watches every current member of this group (`peer` is unused).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    /// Group members this rule skips.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub opt_out: Vec<EndpointId>,
    #[serde(default)]
    state: State,
    /// Group rule: arrival state per member.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    members: Vec<(EndpointId, State)>,
}
/// Whom a rule watches.
#[derive(Clone, Debug, PartialEq)]
pub enum Who {
    Friend(EndpointId),
    /// Group id and members to skip.
    Group(String, Vec<EndpointId>),
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub enum Trigger {
    Entering,
    Exiting,
    #[default]
    Both,
}
/// Last known side of the boundary, and when this rule last alerted.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
struct State {
    inside: Option<bool>,
    last_ms: i64,
    alerted_ms: Option<i64>,
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
    let bytes = serde_json::to_vec(saved)?;
    if bytes.len() > 65_536 {
        return Err("Alert area storage is full".into());
    }
    atomic_json(&friends.directory().join("places.json"), &bytes)
}
pub use crate::geo::{MAX_RADIUS, MIN_RADIUS, circle_outline};
fn validate(name: &str, center: Point, radius: f64) -> Result<()> {
    if name.trim().is_empty()
        || name.chars().count() > 64
        || name.chars().any(char::is_control)
        || !center.valid()
        || !radius.is_finite()
        || !(MIN_RADIUS..=MAX_RADIUS).contains(&radius)
    {
        return Err("Use a name up to 64 characters and radius 10–10000 m".into());
    }
    Ok(())
}
pub fn list(friends: &Friends) -> Result<Vec<Place>> {
    let _lock = friends.lock()?;
    Ok(load(friends)?.places)
}
#[cfg(test)]
fn add(
    friends: &Friends,
    name: &str,
    center: Point,
    radius_m: f64,
    peer: EndpointId,
) -> Result<()> {
    set_rule(
        friends,
        None,
        name,
        center,
        radius_m,
        Some(Who::Friend(peer)),
        Trigger::Both,
        Vec::new(),
    )
    .map(|_| ())
}

impl Place {
    pub fn area(&self) -> u32 {
        if self.area_id == 0 {
            self.id
        } else {
            self.area_id
        }
    }

    /// Last side seen for `peer` (true: inside) and that fix's time.
    pub fn side(&self, peer: EndpointId) -> Option<(bool, i64)> {
        let state = match self.group {
            None if self.peer == peer => &self.state,
            None => return None,
            Some(_) => &self.members.iter().find(|(m, _)| *m == peer)?.1,
        };
        state.inside.map(|inside| (inside, state.last_ms))
    }

    /// Whether this rule alerts about `friend`.
    pub fn watches(&self, friend: &Friend) -> bool {
        !self.no_rule
            && !friend.blocked
            && match &self.group {
                None => self.peer == friend.peer_id,
                Some(id) => friend.groups.contains(id) && !self.opt_out.contains(&friend.peer_id),
            }
    }

    pub fn outline(&self) -> Vec<Point> {
        if !self.polygon.is_empty() {
            return self.polygon.clone();
        }
        circle_outline(self.center, self.radius_m)
    }
}

pub fn set_rule(
    friends: &Friends,
    existing: Option<u32>,
    name: &str,
    center: Point,
    radius: f64,
    who: Option<Who>,
    trigger: Trigger,
    polygon: Vec<Point>,
) -> Result<u32> {
    if !polygon.is_empty()
        && (polygon.len() < 3 || polygon.len() > 256 || polygon.iter().any(|point| !point.valid()))
    {
        return Err("Draw a boundary with 3–256 valid corners".into());
    }
    let _lock = friends.lock()?;
    match &who {
        Some(Who::Friend(peer))
            if !friends
                .list_locked()?
                .iter()
                .any(|f| f.peer_id == *peer && !f.blocked) =>
        {
            return Err("Choose a current, unblocked friend".into());
        }
        Some(Who::Group(id, _))
            if !friends
                .groups_locked()?
                .iter()
                .any(|g| g.id == *id && g.leaving_since_ms.is_none()) =>
        {
            return Err("Choose a current group".into());
        }
        None if existing.is_some() => return Err("Choose a friend for this rule".into()),
        _ => {}
    }
    let no_rule = who.is_none();
    let (peer, group, opt_out) = match who {
        Some(Who::Friend(peer)) => (peer, None, Vec::new()),
        Some(Who::Group(id, skip)) => (friends.owner, Some(id), skip),
        None => (friends.owner, None, Vec::new()),
    };
    let same = |p: &Place| match &group {
        Some(id) => p.group.as_ref() == Some(id),
        None => p.group.is_none() && p.peer == peer,
    };
    let mut saved = load(friends)?;
    let area = if let Some(id) = existing {
        let area = saved
            .places
            .iter()
            .find(|p| p.area() == id)
            .ok_or("Place was removed")?
            .clone();
        if let Some(rule) = saved
            .places
            .iter_mut()
            .find(|p| p.area() == id && !p.no_rule && same(p))
        {
            rule.trigger = trigger;
            rule.opt_out = opt_out;
            rule.members.retain(|(m, _)| !rule.opt_out.contains(m));
            save(friends, &saved)?;
            return Ok(id);
        }
        Some(area)
    } else {
        validate(name, center, radius)?;
        None
    };
    if let Some(id) = existing
        && let Some(rule) = saved
            .places
            .iter_mut()
            .find(|p| p.area() == id && p.no_rule)
    {
        rule.peer = peer;
        rule.group = group;
        rule.opt_out = opt_out;
        rule.trigger = trigger;
        rule.no_rule = false;
        save(friends, &saved)?;
        return Ok(id);
    }
    if saved.places.len() >= 64 {
        return Err("Maximum 64 alerts across alert areas".into());
    }
    saved.next_id = saved
        .next_id
        .checked_add(1)
        .filter(|id| *id < 1_000_000)
        .ok_or("place ID limit reached")?;
    saved.places.push(Place {
        polygon: area.as_ref().map_or(polygon, |p| p.polygon.clone()),
        trigger,
        area_id: existing.unwrap_or(saved.next_id),
        id: saved.next_id,
        name: area
            .as_ref()
            .map(|p| p.name.clone())
            .unwrap_or_else(|| name.trim().into()),
        center: area.as_ref().map_or(center, |p| p.center),
        radius_m: area.as_ref().map_or(radius, |p| p.radius_m),
        peer,
        no_rule,
        group,
        opt_out,
        state: State::default(),
        members: Vec::new(),
    });
    save(friends, &saved)?;
    Ok(existing.unwrap_or(saved.next_id))
}
pub fn remove(friends: &Friends, id: u32) -> Result<()> {
    let _lock = friends.lock()?;
    let mut saved = load(friends)?;
    if let Some(rule) = saved.places.iter().find(|p| p.id == id) {
        let area = rule.area();
        if saved.places.iter().any(|p| p.area() == area && p.id != id) {
            saved.places.retain(|p| p.id != id);
        } else if let Some(rule) = saved.places.iter_mut().find(|p| p.id == id) {
            rule.no_rule = true;
            rule.group = None;
            rule.opt_out.clear();
            rule.state = State::default();
            rule.members.clear();
        }
    }
    save(friends, &saved)
}

pub fn edit_area(
    friends: &Friends,
    area: u32,
    name: &str,
    center: Point,
    radius: f64,
    polygon: Option<Vec<Point>>,
) -> Result<()> {
    validate(name, center, radius)?;
    if polygon
        .as_ref()
        .is_some_and(|p| p.len() < 3 || p.len() > 256 || p.iter().any(|point| !point.valid()))
    {
        return Err("Draw a boundary with 3–256 valid corners".into());
    }
    let _lock = friends.lock()?;
    let mut saved = load(friends)?;
    let mut found = false;
    for place in saved.places.iter_mut().filter(|p| p.area() == area) {
        found = true;
        place.name = name.trim().to_owned();
        if let Some(points) = polygon.as_ref() {
            if place.polygon != *points {
                place.polygon = points.clone();
                place.center = center;
                place.state = State::default();
                place.members.clear();
            }
        } else if !place.polygon.is_empty() || place.center != center || place.radius_m != radius {
            // Drawn shape switched to circle, or circle moved.
            place.polygon.clear();
            place.center = center;
            place.radius_m = radius;
            place.state = State::default();
            place.members.clear();
        }
    }
    if !found {
        return Err("Alert area was removed".into());
    }
    save(friends, &saved)
}

pub fn remove_area(friends: &Friends, area: u32) -> Result<()> {
    let _lock = friends.lock()?;
    let mut saved = load(friends)?;
    saved.places.retain(|p| p.area() != area);
    save(friends, &saved)
}

/// One line to Android's log (tag "roamlog"). The sharing service runs in its own
/// process whose stderr is not forwarded, so `eprintln!` would be lost there.
pub(super) fn log(message: &str) {
    #[cfg(target_os = "android")]
    {
        #[link(name = "log")]
        unsafe extern "C" {
            fn __android_log_write(
                priority: std::ffi::c_int,
                tag: *const std::ffi::c_char,
                text: *const std::ffi::c_char,
            ) -> std::ffi::c_int;
        }
        if let Ok(text) = std::ffi::CString::new(message) {
            // SAFETY: both strings are NUL-terminated and outlive the call; 4 = INFO.
            unsafe { __android_log_write(4, c"roamlog".as_ptr(), text.as_ptr()) };
        }
    }
    #[cfg(not(target_os = "android"))]
    eprintln!("{message}");
}

/// At most one alert per rule and person in this window; quick back-and-forth only
/// updates the remembered side.
const ALERT_COOLDOWN_MS: i64 = 300_000;

impl Place {
    /// Alerts when a fix lands on the other side from the last known one. Inside: the
    /// reported point is in the area (indoor fixes are often ±100 m, wider than a home
    /// circle). Outside: the whole accuracy circle (+25 m) is out, so a wandering
    /// indoor fix cannot fake a departure. In between keeps the last side. First fix
    /// only sets the side.
    /// Any new fix counts, however it arrived and whether or not still connected; only
    /// age (stale map cutoff) and the sender's clock skew limit it, so a late fix can
    /// delay an alert but not invent one.
    fn observe(&self, state: &mut State, friend: &FriendState, now: i64) -> Option<String> {
        let fix = friend.location?;
        if fix.timestamp_ms <= state.last_ms {
            return None;
        }
        if now.saturating_sub(fix.timestamp_ms) > super::location::MAX_FIX_AGE_MS
            || fix.timestamp_ms > now + super::protocol::FUTURE_SKEW_MS
        {
            log(&format!(
                "alert area {}: {} fix ignored, {} s old",
                self.name,
                friend.name,
                now.saturating_sub(fix.timestamp_ms) / 1000
            ));
            // Old fix never becomes fresh: log it once, not on every check.
            if fix.timestamp_ms <= now {
                state.last_ms = fix.timestamp_ms;
            }
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
            if inside {
                Some(true)
            } else if edge_distance > accuracy + 25. {
                Some(false)
            } else {
                None
            }
        } else if distance < self.radius_m {
            Some(true)
        } else if distance - accuracy > self.radius_m + 25. {
            Some(false)
        } else {
            None
        };
        state.last_ms = fix.timestamp_ms;
        log(&format!(
            "alert area {}: {} {} (±{accuracy:.0} m), was {}",
            self.name,
            friend.name,
            match inside {
                Some(true) => "inside",
                Some(false) => "outside",
                None => "near edge, keeping last side",
            },
            match state.inside {
                Some(true) => "inside",
                Some(false) => "outside",
                None => "unknown",
            }
        ));
        let inside = inside?;
        if state.inside.replace(inside) != Some(!inside)
            || (inside && self.trigger == Trigger::Exiting)
            || (!inside && self.trigger == Trigger::Entering)
            || state
                .alerted_ms
                .is_some_and(|at| fix.timestamp_ms.saturating_sub(at) < ALERT_COOLDOWN_MS)
        {
            return None;
        }
        state.alerted_ms = Some(fix.timestamp_ms);
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
    for place in saved.places.iter_mut().filter(|p| !p.no_rule) {
        let watched: Vec<_> = records.iter().filter(|f| place.watches(f)).collect();
        // Notification per rule and member, so group alerts do not replace each other.
        let id = |peer: EndpointId| {
            1_000_000 + place.id as i32 * 64 + i32::from(peer.as_bytes()[0] % 64)
        };
        if place.group.is_some() {
            let before = place.members.len();
            place
                .members
                .retain(|(m, _)| watched.iter().any(|f| f.peer_id == *m));
            changed |= before != place.members.len();
            for friend in observations
                .iter()
                .filter(|o| watched.iter().any(|f| f.peer_id == o.peer_id))
            {
                let mut members = std::mem::take(&mut place.members);
                let at = members
                    .iter()
                    .position(|(m, _)| *m == friend.peer_id)
                    .unwrap_or_else(|| {
                        members.push((friend.peer_id, State::default()));
                        members.len() - 1
                    });
                let old = members[at].1.clone();
                if let Some(text) = place.observe(&mut members[at].1, friend, now) {
                    notices.push((id(friend.peer_id), text));
                }
                changed |= old != members[at].1;
                place.members = members;
            }
            continue;
        }
        if watched.is_empty() {
            if place.state != State::default() {
                place.state = State::default();
                changed = true;
            }
            continue;
        }
        if let Some(friend) = observations.iter().find(|f| f.peer_id == place.peer) {
            let mut state = std::mem::take(&mut place.state);
            let old = state.clone();
            if let Some(text) = place.observe(&mut state, friend, now) {
                notices.push((id(friend.peer_id), text));
            }
            changed |= old != state;
            place.state = state;
        }
    }
    if changed {
        save(friends, &saved)?;
    }
    Ok(notices)
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
    fn drawn_boundary_is_shared_by_rules_and_can_be_reshaped() {
        let dir = tempfile::tempdir().unwrap();
        let owner = iroh::SecretKey::generate().public();
        let peer = iroh::SecretKey::generate().public();
        let friends = Friends::open(dir.path(), owner).unwrap();
        friends.add_confirmed(peer, "Sam".into()).unwrap();
        let polygon = vec![
            Point {
                lat: -0.001,
                lon: -0.001,
            },
            Point {
                lat: -0.001,
                lon: 0.001,
            },
            Point {
                lat: 0.001,
                lon: 0.001,
            },
            Point {
                lat: 0.001,
                lon: -0.001,
            },
        ];
        let center = polygon[0];
        let id = set_rule(
            &friends,
            None,
            "Park",
            center,
            200.,
            None,
            Trigger::Both,
            polygon.clone(),
        )
        .unwrap();
        set_rule(
            &friends,
            Some(id),
            "",
            center,
            200.,
            Some(Who::Friend(peer)),
            Trigger::Entering,
            Vec::new(),
        )
        .unwrap();
        assert_eq!(list(&friends).unwrap()[0].outline(), polygon);
        observe(&friends, &[observation(peer, 100_000, 0.01)], 100_000).unwrap();
        assert_eq!(
            observe(&friends, &[observation(peer, 130_000, 0.)], 130_000).unwrap()[0].1,
            "Sam arrived at Park"
        );
        let moved = polygon
            .iter()
            .map(|p| Point {
                lat: p.lat,
                lon: p.lon + 0.01,
            })
            .collect::<Vec<_>>();
        edit_area(&friends, id, "Park", moved[0], 200., Some(moved.clone())).unwrap();
        assert_eq!(list(&friends).unwrap()[0].outline(), moved);
        assert!(
            observe(&friends, &[observation(peer, 160_000, 0.)], 160_000)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn group_rule_alerts_each_member_except_opt_outs() {
        use crate::sharing::{groups::Policy, peers::Permission};
        let dir = tempfile::tempdir().unwrap();
        let key = iroh::SecretKey::generate();
        let (sam, ann, cal) = (
            iroh::SecretKey::generate().public(),
            iroh::SecretKey::generate().public(),
            iroh::SecretKey::generate().public(),
        );
        let friends = Friends::open(dir.path(), key.public()).unwrap();
        let group = friends
            .create_group(&key, "Family", "Me", Policy::Managed, Permission::Always)
            .unwrap();
        for (peer, name) in [(sam, "Sam"), (ann, "Ann"), (cal, "Cal")] {
            friends.add_confirmed(peer, name.into()).unwrap();
            friends.admit_member(&key, &group, peer, name).unwrap();
        }
        let id = set_rule(
            &friends,
            None,
            "Home",
            Point { lat: 0., lon: 0. },
            10.,
            Some(Who::Group(group.clone(), vec![cal])),
            Trigger::Both,
            Vec::new(),
        )
        .unwrap();
        let at = |t: i64, lon: f64| {
            [(sam, "Sam"), (ann, "Ann"), (cal, "Cal")].map(|(peer, name)| FriendState {
                name: name.into(),
                ..observation(peer, t, lon)
            })
        };
        observe(&friends, &at(100_000, 0.01), 100_000).unwrap();
        let notices = observe(&friends, &at(130_000, 0.), 130_000).unwrap();
        assert_eq!(notices.len(), 2, "Cal opted out");
        assert_ne!(notices[0].0, notices[1].0);
        let mut seen: Vec<_> = notices.iter().map(|n| n.1.as_str()).collect();
        seen.sort();
        assert_eq!(seen, ["Ann arrived at Home", "Sam arrived at Home"]);
        let rule = &list(&friends).unwrap()[0];
        let record = |peer| {
            friends
                .list()
                .unwrap()
                .into_iter()
                .find(|f| f.peer_id == peer)
                .unwrap()
        };
        assert!(rule.watches(&record(ann)) && !rule.watches(&record(cal)));
        // Updating the same group rule replaces opt-outs, not a second rule.
        set_rule(
            &friends,
            Some(id),
            "",
            Point { lat: 0., lon: 0. },
            10.,
            Some(Who::Group(group, vec![])),
            Trigger::Both,
            Vec::new(),
        )
        .unwrap();
        let rules = list(&friends).unwrap();
        assert_eq!(rules.len(), 1);
        assert!(rules[0].watches(&record(cal)));
    }

    #[test]
    fn side_changes_alert_with_cooldown_and_ignore_stale_or_uncertain_fixes() {
        let dir = tempfile::tempdir().unwrap();
        let key = iroh::SecretKey::generate();
        let peer = iroh::SecretKey::generate().public();
        let friends = Friends::open(dir.path(), key.public()).unwrap();
        friends.add_confirmed(peer, "Sam".into()).unwrap();
        add(&friends, "Home", Point { lat: 0., lon: 0. }, 250., peer).unwrap();
        let seen = |friends: &Friends, fix: FriendState, now: i64| {
            observe(friends, &[fix], now)
                .unwrap()
                .into_iter()
                .map(|n| n.1)
                .collect::<Vec<_>>()
        };
        // First fix only sets the side.
        assert!(seen(&friends, observation(peer, 100_000, 0.01), 100_000).is_empty());
        assert_eq!(
            seen(&friends, observation(peer, 130_000, 0.), 130_000),
            ["Sam arrived at Home"]
        );
        // Same fix again is not news.
        assert!(seen(&friends, observation(peer, 130_000, 0.), 131_000).is_empty());
        // Side survives restart; leaving within 5 min of last alert is silent.
        let friends = Friends::open(dir.path(), key.public()).unwrap();
        assert!(seen(&friends, observation(peer, 175_000, 0.01), 175_000).is_empty());
        // Ten silent minutes outside, then back: alerts, no matter the gap.
        assert!(seen(&friends, observation(peer, 300_000, 0.01), 300_000).is_empty());
        assert_eq!(
            seen(&friends, observation(peer, 900_000, 0.), 900_000),
            ["Sam arrived at Home"]
        );
        // Stale (over 15 min) and edge-uncertain fixes change nothing.
        assert!(seen(&friends, observation(peer, 1_300_000, 0.01), 2_300_000).is_empty());
        let mut uncertain = observation(peer, 1_310_000, 0.002);
        uncertain.location.as_mut().unwrap().accuracy_cm = 20_000;
        assert!(seen(&friends, uncertain, 1_310_000).is_empty());
        // Relayed fix that arrived while disconnected still counts, as does a sender
        // clock a minute fast.
        let mut relayed = observation(peer, 1_380_000, 0.01);
        relayed.via = Some(key.public());
        relayed.connected = false;
        relayed.sharing_active = false;
        assert_eq!(seen(&friends, relayed, 1_320_000), ["Sam left Home"]);
        friends.block(peer).unwrap();
        assert!(seen(&friends, observation(peer, 1_400_000, 0.), 1_400_000).is_empty());
        assert_eq!(list(&friends).unwrap()[0].state, State::default());
    }

    #[test]
    fn coarse_indoor_fixes_still_detect_small_home_circle() {
        let dir = tempfile::tempdir().unwrap();
        let key = iroh::SecretKey::generate();
        let peer = iroh::SecretKey::generate().public();
        let friends = Friends::open(dir.path(), key.public()).unwrap();
        friends.add_confirmed(peer, "Mum".into()).unwrap();
        add(&friends, "Home", Point { lat: 0., lon: 0. }, 50., peer).unwrap();
        // ±100 m, wider than the circle. Degrees of longitude at the equator: 0.0009 ≈ 100 m.
        let at = |t: i64, lon: f64| {
            let mut fix = observation(peer, t, lon);
            fix.name = "Mum".into();
            fix.location.as_mut().unwrap().accuracy_cm = 10_000;
            observe(&friends, &[fix], t)
                .unwrap()
                .into_iter()
                .map(|n| n.1)
                .collect::<Vec<_>>()
        };
        assert!(at(100_000, 0.01).is_empty());
        assert_eq!(at(130_000, 0.0002), ["Mum arrived at Home"]);
        // Indoor fix wanders 80 m out: still home, no false departure.
        assert!(at(800_000, 0.0007).is_empty());
        assert_eq!(at(1_500_000, 0.004), ["Mum left Home"]);
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
        let place = &list(&friends).unwrap()[0];
        assert!(place.no_rule);
        remove_area(&friends, place.area()).unwrap();
        assert!(list(&friends).unwrap().is_empty());
    }
}
