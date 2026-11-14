use crate::geo::Point;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct Category {
    pub id: u64,
    pub name: String,
}

/// Saved map area for road coverage percentage.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct Area {
    pub id: u64,
    pub name: String,
    pub points: Vec<Point>,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Recording,
    Paused,
    Finished,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq)]
pub struct Fix {
    #[serde(flatten)]
    pub point: Point,
    pub timestamp_ms: u64,
    pub accuracy_m: f64,
}

impl Fix {
    pub fn valid(self) -> bool {
        self.point.valid() && self.accuracy_m.is_finite() && self.accuracy_m >= 0.0
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct TrackPoint {
    #[serde(flatten)]
    pub fix: Fix,
    /// False starts a new path after pause, lost GPS, or rejected jump.
    pub connected: bool,
    /// Incoming segment inferred between real fixes, rather than continuously observed.
    #[serde(default)]
    pub estimated: bool,
    /// Inferred segment follows matched road centerline; exact lane remains inferred.
    #[serde(default)]
    pub road_matched: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct Trip {
    pub id: u64,
    pub category_id: Option<u64>,
    pub started_ms: u64,
    pub finished_ms: Option<u64>,
    pub status: Status,
    pub distance_m: f64,
    #[serde(default)]
    pub estimated_distance_m: f64,
    pub elapsed_ms: u64,
    pub recording_since_ms: Option<u64>,
    pub paused_at_ms: Option<u64>,
    pub points: Vec<TrackPoint>,
    #[serde(skip)]
    pub(crate) anchor: Option<Fix>,
    #[serde(skip)]
    pub(crate) latest_fix_ms: Option<u64>,
    #[serde(skip)]
    pub(crate) gap_pending: bool,
}

impl Trip {
    pub fn elapsed_at(&self, now_ms: u64) -> u64 {
        self.elapsed_ms.saturating_add(
            self.recording_since_ms
                .map_or(0, |start| now_ms.saturating_sub(start)),
        )
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub struct State {
    pub categories: Vec<Category>,
    pub trips: Vec<Trip>,
    #[serde(default)]
    pub areas: Vec<Area>,
    /// Ids never reused after deletion; equals count+1 for journals without deletes.
    #[serde(skip)]
    next_trip: u64,
}

/// History selection only; recording category remains independent.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum TripFilter {
    #[default]
    All,
    Category {
        category_id: Option<u64>,
    },
    Trip {
        trip_id: u64,
    },
}

impl TripFilter {
    pub fn matches(self, trip: &Trip) -> bool {
        match self {
            Self::All => true,
            Self::Category { category_id } => trip.category_id == category_id,
            Self::Trip { trip_id } => trip.id == trip_id,
        }
    }
}

impl State {
    pub fn trips_for(&self, filter: TripFilter) -> impl Iterator<Item = &Trip> {
        self.trips.iter().filter(move |t| filter.matches(t))
    }

    pub fn active(&self) -> Option<&Trip> {
        self.trips.last().filter(|t| t.status != Status::Finished)
    }

    pub fn total_distance(&self, category: Option<u64>) -> f64 {
        self.trips
            .iter()
            .filter(|t| category.is_none() || t.category_id == category)
            .map(|t| t.distance_m)
            .sum::<f64>()
            .max(0.0)
    }

    pub(crate) fn validate(&self, event: &Event) -> Result<(), String> {
        match event {
            Event::Category { name } => {
                if name.trim().is_empty() || name.len() > 80 {
                    return Err("Category needs 1–80 bytes".into());
                }
                if self
                    .categories
                    .iter()
                    .any(|c| c.name.eq_ignore_ascii_case(name.trim()))
                {
                    return Err("Category already exists".into());
                }
            }
            Event::Start { category_id, .. } => {
                if self.active().is_some() {
                    return Err("Finish current trip first".into());
                }
                if category_id.is_some_and(|id| !self.categories.iter().any(|c| c.id == id)) {
                    return Err("Unknown category".into());
                }
            }
            Event::Fix { fix } => {
                if !fix.valid() {
                    return Err("Invalid GPS fix".into());
                }
                if self.active().is_none_or(|t| t.status != Status::Recording) {
                    return Err("No recording trip".into());
                }
            }
            Event::Pause { timestamp_ms } | Event::Finish { timestamp_ms } => {
                let trip = self.active().ok_or("No active trip")?;
                if *timestamp_ms < trip.started_ms
                    || trip.recording_since_ms.is_some_and(|s| *timestamp_ms < s)
                    || trip.paused_at_ms.is_some_and(|s| *timestamp_ms < s)
                {
                    return Err("Trip time moved backwards".into());
                }
                if matches!(event, Event::Pause { .. }) && trip.status != Status::Recording {
                    return Err("Trip already paused".into());
                }
            }
            Event::DeleteTrip { trip_id } => {
                if !self
                    .trips
                    .iter()
                    .any(|t| t.id == *trip_id && t.status == Status::Finished)
                {
                    return Err("Only finished trips can be deleted".into());
                }
            }
            Event::SaveArea { name, points } => {
                if name.trim().is_empty() || name.len() > 80 {
                    return Err("Area name needs 1–80 bytes".into());
                }
                if points.len() < 3 || points.len() > 200 || !points.iter().all(|p| p.valid()) {
                    return Err("Area needs 3–200 valid corners".into());
                }
            }
            Event::DeleteArea { area_id } => {
                if !self.areas.iter().any(|a| a.id == *area_id) {
                    return Err("Unknown area".into());
                }
            }
            Event::UpdateArea { area_id, points } => {
                if !self.areas.iter().any(|a| a.id == *area_id) {
                    return Err("Unknown area".into());
                }
                if points.len() < 3 || points.len() > 200 || !points.iter().all(|p| p.valid()) {
                    return Err("Area needs 3–200 valid corners".into());
                }
            }
            Event::SetCategory {
                trip_id,
                category_id,
            } => {
                self.finished(*trip_id)?;
                if category_id.is_some_and(|id| !self.categories.iter().any(|c| c.id == id)) {
                    return Err("Unknown category".into());
                }
            }
            Event::TrimTrip {
                trip_id,
                start_ms,
                end_ms,
            } => {
                let trip = self.finished(*trip_id)?;
                let kept = trip
                    .points
                    .iter()
                    .filter(|p| (*start_ms..=*end_ms).contains(&p.fix.timestamp_ms))
                    .count();
                if kept < 2 || kept == trip.points.len() {
                    return Err("Trim must remove points and keep at least two".into());
                }
            }
            Event::SplitTrip { trip_id, at_ms } => {
                let trip = self.finished(*trip_id)?;
                let before = trip
                    .points
                    .iter()
                    .filter(|p| p.fix.timestamp_ms < *at_ms)
                    .count();
                if before < 2 || trip.points.len() - before < 2 {
                    return Err("Each part needs at least two points".into());
                }
            }
            Event::Resume { timestamp_ms } => {
                let trip = self.active().ok_or("No active trip")?;
                if trip.status != Status::Paused
                    || *timestamp_ms < trip.paused_at_ms.unwrap_or(trip.started_ms)
                {
                    return Err("Trip must be paused with valid time".into());
                }
            }
        }
        Ok(())
    }

    fn finished(&self, trip_id: u64) -> Result<&Trip, String> {
        self.trips
            .iter()
            .find(|t| t.id == trip_id && t.status == Status::Finished)
            .ok_or_else(|| "Only finished trips can be edited".into())
    }

    pub(crate) fn apply(&mut self, event: Event) {
        match event {
            Event::Category { name } => self.categories.push(Category {
                id: self.categories.len() as u64 + 1,
                name: name.trim().into(),
            }),
            Event::Start {
                timestamp_ms,
                category_id,
            } => self.trips.push(Trip {
                id: {
                    self.next_trip = self.next_trip.max(self.trips.len() as u64) + 1;
                    self.next_trip
                },
                category_id,
                started_ms: timestamp_ms,
                finished_ms: None,
                status: Status::Recording,
                distance_m: 0.0,
                estimated_distance_m: 0.0,
                elapsed_ms: 0,
                recording_since_ms: Some(timestamp_ms),
                paused_at_ms: None,
                points: Vec::new(),
                anchor: None,
                latest_fix_ms: None,
                gap_pending: false,
            }),
            Event::Pause { timestamp_ms } | Event::Finish { timestamp_ms } => {
                let trip = self.trips.last_mut().unwrap();
                trip.elapsed_ms = trip.elapsed_at(timestamp_ms);
                trip.recording_since_ms = None;
                trip.paused_at_ms = Some(timestamp_ms);
                trip.anchor = None;
                trip.gap_pending = false;
                if matches!(event, Event::Finish { .. }) {
                    trip.status = Status::Finished;
                    trip.finished_ms = Some(timestamp_ms);
                } else {
                    trip.status = Status::Paused;
                }
            }
            Event::Resume { timestamp_ms } => {
                let trip = self.trips.last_mut().unwrap();
                trip.status = Status::Recording;
                trip.recording_since_ms = Some(timestamp_ms);
                trip.paused_at_ms = None;
                trip.anchor = None;
                trip.gap_pending = false;
                trip.latest_fix_ms = None;
            }
            Event::Fix { fix } => self.trips.last_mut().unwrap().record(fix),
            Event::DeleteTrip { trip_id } => self.trips.retain(|t| t.id != trip_id),
            Event::SaveArea { name, points } => self.areas.push(Area {
                id: self.areas.iter().map(|a| a.id).max().unwrap_or(0) + 1,
                name: name.trim().into(),
                points,
            }),
            Event::DeleteArea { area_id } => self.areas.retain(|a| a.id != area_id),
            Event::UpdateArea { area_id, points } => {
                if let Some(area) = self.areas.iter_mut().find(|a| a.id == area_id) {
                    area.points = points;
                }
            }
            Event::SetCategory {
                trip_id,
                category_id,
            } => {
                if let Some(trip) = self.trips.iter_mut().find(|t| t.id == trip_id) {
                    trip.category_id = category_id;
                }
            }
            Event::TrimTrip {
                trip_id,
                start_ms,
                end_ms,
            } => {
                let trip = self.trips.iter_mut().find(|t| t.id == trip_id).unwrap();
                let (first, last) = trip.span();
                trip.points
                    .retain(|p| (start_ms..=end_ms).contains(&p.fix.timestamp_ms));
                let (new_first, new_last) = trip.span();
                // Recording time removed from ends; pauses inside them are not tracked.
                trip.elapsed_ms = trip
                    .elapsed_ms
                    .saturating_sub(new_first - first + (last - new_last));
                if new_first > first {
                    trip.started_ms = new_first;
                }
                if new_last < last {
                    trip.finished_ms = Some(new_last);
                    trip.paused_at_ms = Some(new_last);
                }
                trip.remeasure();
            }
            Event::SplitTrip { trip_id, at_ms } => {
                let index = self.trips.iter().position(|t| t.id == trip_id).unwrap();
                self.next_trip = self.next_trip.max(self.trips.len() as u64) + 1;
                let trip = &mut self.trips[index];
                let (first, last) = trip.span();
                let at = trip.points.partition_point(|p| p.fix.timestamp_ms < at_ms);
                let mut second = trip.clone();
                second.points = trip.points.split_off(at);
                let first_end = trip.points.last().unwrap().fix.timestamp_ms;
                // Recording time divides by GPS time span; pauses are not tracked.
                let share = (first_end - first) as f64 / (last - first).max(1) as f64;
                let elapsed = trip.elapsed_ms;
                trip.elapsed_ms = (elapsed as f64 * share).round() as u64;
                trip.finished_ms = Some(first_end);
                trip.paused_at_ms = Some(first_end);
                trip.remeasure();
                second.id = self.next_trip;
                second.started_ms = second.points[0].fix.timestamp_ms;
                second.elapsed_ms = elapsed - trip.elapsed_ms;
                second.remeasure();
                self.trips.insert(index + 1, second);
            }
        }
    }
}

impl Trip {
    /// First and last fix times; trips edited only after finishing have points.
    fn span(&self) -> (u64, u64) {
        (
            self.points.first().map_or(0, |p| p.fix.timestamp_ms),
            self.points.last().map_or(0, |p| p.fix.timestamp_ms),
        )
    }

    /// Distance after editing: every kept connected segment, as recorded.
    fn remeasure(&mut self) {
        if let Some(first) = self.points.first_mut() {
            first.connected = false;
            first.estimated = false;
        }
        let (mut distance, mut estimated) = (0.0, 0.0);
        for pair in self.points.windows(2).filter(|p| p[1].connected) {
            let d = pair[0].fix.point.distance(pair[1].fix.point);
            distance += d;
            if pair[1].estimated {
                estimated += d;
            }
        }
        self.distance_m = distance;
        self.estimated_distance_m = estimated;
    }

    fn record(&mut self, fix: Fix) {
        if fix.timestamp_ms < self.recording_since_ms.unwrap_or(self.started_ms)
            || self.latest_fix_ms.is_some_and(|t| fix.timestamp_ms <= t)
        {
            return;
        }
        self.latest_fix_ms = Some(fix.timestamp_ms);
        if fix.accuracy_m > 30.0 {
            // Weak fixes are not evidence that travel stopped. Keep last trustworthy
            // endpoint so return of GPS can recover short outages.
            self.gap_pending = true;
            return;
        }
        let mut connected = false;
        let mut estimated = false;
        if let Some(anchor) = self.anchor {
            let dt = (fix.timestamp_ms - anchor.timestamp_ms) as f64 / 1000.0;
            let distance = anchor.point.distance(fix.point);
            if dt <= 180.0 {
                if distance / dt > 100.0 {
                    // Reject isolated teleports without making them new anchors.
                    self.gap_pending = true;
                    return;
                }
                // Suppress stationary jitter; keep anchor so slow motion accumulates.
                if distance < 3.0_f64.max((anchor.accuracy_m + fix.accuracy_m) * 0.25) {
                    return;
                }
                self.distance_m += distance;
                estimated = self.gap_pending || dt > 10.0;
                if estimated {
                    self.estimated_distance_m += distance;
                }
                connected = true;
            }
        }
        self.points.push(TrackPoint {
            fix,
            connected,
            estimated,
            road_matched: false,
        });
        self.anchor = Some(fix);
        self.gap_pending = false;
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum Event {
    Category {
        name: String,
    },
    Start {
        timestamp_ms: u64,
        category_id: Option<u64>,
    },
    Pause {
        timestamp_ms: u64,
    },
    Resume {
        timestamp_ms: u64,
    },
    Finish {
        timestamp_ms: u64,
    },
    Fix {
        fix: Fix,
    },
    DeleteTrip {
        trip_id: u64,
    },
    SaveArea {
        name: String,
        points: Vec<Point>,
    },
    DeleteArea {
        area_id: u64,
    },
    UpdateArea {
        area_id: u64,
        points: Vec<Point>,
    },
    SetCategory {
        trip_id: u64,
        category_id: Option<u64>,
    },
    /// Keeps fixes from `start_ms` through `end_ms`.
    TrimTrip {
        trip_id: u64,
        start_ms: u64,
        end_ms: u64,
    },
    /// Fixes from `at_ms` become a new trip right after this one.
    SplitTrip {
        trip_id: u64,
        at_ms: u64,
    },
}
