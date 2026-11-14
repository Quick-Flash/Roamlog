//! Provisional geometric coverage derived from saved trip history.
use crate::{
    geo::{Point, local, segment_distance},
    model::{State, TripFilter},
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Road {
    pub id: String,
    pub name: String,
    pub points: Vec<Point>,
    /// Nominal centerline-to-road-edge allowance; zero when road class is unknown.
    #[serde(default)]
    pub half_width_m: f64,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct Section {
    pub road_id: String,
    pub start: Point,
    pub end: Point,
    pub traveled: bool,
    /// Trips that each covered this section on their own.
    #[serde(default)]
    pub visits: u32,
    /// Trip whose evidence first completed this section.
    #[serde(default)]
    pub first_trip: Option<u64>,
}

/// Latest trip index touching a section, that trip's own evidence, and whether it counted.
type Visit = (Option<usize>, Vec<(f64, f64)>, bool);

/// Retains interval evidence while immutable trip histories grow.
pub struct Coverage {
    pub sections: Vec<Section>,
    parents: Vec<(usize, Point, Point, f64)>,
    evidence: Vec<Vec<(f64, f64)>>,
    visit: Vec<Visit>,
    processed: Vec<usize>,
    /// Last confidently followed road for each trip; cleared across unobserved segments.
    context: Vec<Option<usize>>,
    filter: TripFilter,
    index: crate::spatial::Index,
}

pub fn import_geojson(bytes: &[u8]) -> Result<Vec<Road>, String> {
    let value: serde_json::Value = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
    if value["type"] != "FeatureCollection" {
        return Err("Expected GeoJSON FeatureCollection".into());
    }
    let mut roads = Vec::new();
    let mut ids = BTreeSet::new();
    for feature in value["features"].as_array().ok_or("Missing features")? {
        if feature["geometry"]["type"] != "LineString" {
            return Err("Road features must use LineString geometry".into());
        }
        let id_value = feature
            .get("id")
            .or_else(|| feature["properties"].get("osm_id"))
            .ok_or("Each road needs stable feature id or properties.osm_id")?;
        let id = match id_value {
            serde_json::Value::String(s) if !s.is_empty() => s.clone(),
            serde_json::Value::Number(n) => n.to_string(),
            _ => return Err("Road id must be nonempty string or number".into()),
        };
        if !ids.insert(id.clone()) {
            return Err(format!("Duplicate road id: {id}"));
        }
        let mut points = Vec::new();
        for coordinate in feature["geometry"]["coordinates"]
            .as_array()
            .ok_or("Missing coordinates")?
        {
            let c = coordinate.as_array().ok_or("Invalid coordinate")?;
            let point = Point {
                lon: c
                    .first()
                    .and_then(|v| v.as_f64())
                    .ok_or("Missing longitude")?,
                lat: c
                    .get(1)
                    .and_then(|v| v.as_f64())
                    .ok_or("Missing latitude")?,
            };
            if !point.valid() {
                return Err("Road coordinate out of range".into());
            }
            points.push(point);
        }
        if points.len() < 2 || points.windows(2).any(|p| p[0].distance(p[1]) > 10_000.0) {
            return Err("Road needs 2+ points with segments under 10 km".into());
        }
        roads.push(Road {
            half_width_m: 0.0,
            id,
            name: feature["properties"]["name"]
                .as_str()
                .unwrap_or("Unnamed road")
                .into(),
            points,
        });
    }
    Ok(roads)
}

/// Sections <=20 m. Ambiguous parallel roads remain unclaimed.
pub fn sections(roads: &[Road], state: &State) -> Vec<Section> {
    let mut coverage = Coverage::new(roads);
    coverage.update(state);
    coverage.sections
}

impl Coverage {
    pub fn new(roads: &[Road]) -> Self {
        let mut result = Vec::new();
        let mut parents = Vec::new();
        // Pieces sharing id (one street split across map tiles) match as one road.
        let mut groups = std::collections::HashMap::new();
        for (index, road) in roads.iter().enumerate() {
            let r = *groups.entry(road.id.as_str()).or_insert(index);
            for pair in road.points.windows(2) {
                let count = (pair[0].distance(pair[1]) / 20.0).ceil().max(1.0) as usize;
                let delta = (pair[1].lon - pair[0].lon + 180.0).rem_euclid(360.0) - 180.0;
                let interpolate = |t: f64| Point {
                    lat: pair[0].lat + t * (pair[1].lat - pair[0].lat),
                    lon: (pair[0].lon + t * delta + 180.0).rem_euclid(360.0) - 180.0,
                };
                for i in 0..count {
                    result.push(Section {
                        road_id: road.id.clone(),
                        start: interpolate(i as f64 / count as f64),
                        end: interpolate((i + 1) as f64 / count as f64),
                        traveled: false,
                        visits: 0,
                        first_trip: None,
                    });
                    parents.push((r, pair[0], pair[1], road.half_width_m));
                }
            }
        }
        let evidence = vec![Vec::new(); result.len()];
        let visit = vec![(None, Vec::new(), false); result.len()];
        let mut index = crate::spatial::Index::default();
        for (id, section) in result.iter().enumerate() {
            index.insert(id, section.start, section.end);
        }
        Self {
            sections: result,
            parents,
            evidence,
            visit,
            processed: Vec::new(),
            context: Vec::new(),
            filter: TripFilter::All,
            index,
        }
    }

    pub fn update(&mut self, state: &State) {
        self.update_filtered(state, TripFilter::All);
    }

    /// Look-ahead matching can revise recent derived geometry as new fixes arrive.
    pub fn rebuild_filtered(&mut self, state: &State, filter: TripFilter) {
        self.reset();
        self.update_filtered(state, filter);
    }

    fn reset(&mut self) {
        self.processed.clear();
        self.context.clear();
        for section in &mut self.sections {
            section.traveled = false;
            section.visits = 0;
            section.first_trip = None;
        }
        for intervals in &mut self.evidence {
            intervals.clear();
        }
        for visit in &mut self.visit {
            *visit = (None, Vec::new(), false);
        }
    }

    pub fn update_filtered(&mut self, state: &State, filter: TripFilter) {
        if self.filter != filter {
            self.filter = filter;
            self.reset();
        }
        let result = &mut self.sections;
        let parents = &self.parents;
        let evidence = &mut self.evidence;
        let visit = &mut self.visit;
        self.processed.resize(state.trips.len(), 0);
        self.context.resize(state.trips.len(), None);
        for (trip_index, trip) in state.trips.iter().enumerate() {
            if !filter.matches(trip) {
                continue;
            }
            let previous = self.processed[trip_index];
            self.processed[trip_index] = trip.points.len();
            let mut touched = Vec::new();
            for track in trip.points.windows(2).skip(previous.saturating_sub(1)) {
                if !track[1].connected || (track[1].estimated && !track[1].road_matched) {
                    self.context[trip_index] = None;
                    continue;
                }
                let matched = track[1].road_matched;
                // Reconstructed route already chose road geometry. Do not treat exact
                // centerline as noisy GPS and reject it beside nearby lane/frontage road.
                let tolerance = if matched {
                    1.5
                } else {
                    track[0]
                        .fix
                        .accuracy_m
                        .max(track[1].fix.accuracy_m)
                        .clamp(12.0, 30.0)
                };
                let ambiguity = if matched { 0.2 } else { 10.0 };
                let origin = track[0].fix.point;
                let b = local(origin, track[1].fix.point);
                let length = b[0].hypot(b[1]);
                if length < 0.01 {
                    continue;
                }
                let mut candidates = Vec::new();
                for i in self.index.near(origin, track[1].fix.point) {
                    let section = &result[i];
                    let a1 = local(origin, section.start);
                    let b1 = local(origin, section.end);
                    let midpoint = [(a1[0] + b1[0]) * 0.5, (a1[1] + b1[1]) * 0.5];
                    let width = if matched { 0.0 } else { parents[i].3 };
                    if segment_distance(midpoint, [0.0, 0.0], b) > tolerance + width + 10.0 {
                        continue;
                    }
                    let direction = [b1[0] - a1[0], b1[1] - a1[1]];
                    let road_length = direction[0].hypot(direction[1]);
                    if road_length < 0.01 {
                        continue;
                    }
                    let alignment =
                        (b[0] * direction[0] + b[1] * direction[1]).abs() / (length * road_length);
                    if alignment < 0.866 {
                        continue;
                    }
                    let projection = |p: [f64; 2]| {
                        ((p[0] - a1[0]) * direction[0] + (p[1] - a1[1]) * direction[1])
                            / (road_length * road_length)
                    };
                    let x = projection([0.0, 0.0]);
                    let y = projection(b);
                    let interval = (x.min(y).clamp(0.0, 1.0), x.max(y).clamp(0.0, 1.0));
                    if interval.1 <= interval.0 {
                        continue;
                    }
                    // Compare only part of GPS segment overlapping this section. Whole-fix
                    // distances reject short map edges crossed between fast motorcycle fixes.
                    let t = |projection: f64| (projection - x) / (y - x);
                    let span = (t(0.0).min(t(1.0)).max(0.0), t(0.0).max(t(1.0)).min(1.0));
                    let at = |t: f64| [b[0] * t, b[1] * t];
                    let score = (segment_distance(at(span.0), a1, b1) - width).max(0.0)
                        + (segment_distance(at(span.1), a1, b1) - width).max(0.0);
                    if score <= tolerance * 2.0 {
                        candidates.push((i, score, span, x, y));
                    }
                }
                // A previously established road resolves a close parallel competitor only
                // while it remains nearest (within 1 m). Never carry context through a
                // pause/outage or onto a clearly closer different road.
                let best_end = candidates
                    .iter()
                    .filter(|c| c.2.1 >= 1.0 - 1e-6)
                    .map(|c| c.1)
                    .min_by(f64::total_cmp);
                let preferred = self.context[trip_index].filter(|road| {
                    candidates.iter().any(|c| {
                        parents[c.0].0 == *road
                            && c.2.1 >= 1.0 - 1e-6
                            && best_end.is_some_and(|score| c.1 <= score + 2.0)
                    })
                });
                self.context[trip_index] = None;
                for &(i, score, span, x, y) in &candidates {
                    let mut spans = vec![span];
                    // Ambiguous parallel roads remove only overlapping evidence. Sequential
                    // named roads share heading but occupy different parts of same GPS fix.
                    for &(other, other_score, conflict, _, _) in &candidates {
                        if parents[other].0 == parents[i].0
                            || other_score > score + ambiguity
                            || (preferred == Some(parents[i].0) && other_score >= score - 2.0)
                        {
                            continue;
                        }
                        spans = spans
                            .into_iter()
                            .flat_map(|(a, b)| {
                                let mut keep = Vec::new();
                                if conflict.1 <= a || conflict.0 >= b {
                                    keep.push((a, b));
                                } else {
                                    if a < conflict.0 {
                                        keep.push((a, conflict.0));
                                    }
                                    if conflict.1 < b {
                                        keep.push((conflict.1, b));
                                    }
                                }
                                keep
                            })
                            .collect();
                    }
                    for (a, b) in spans {
                        if b >= 1.0 - 1e-6 && (b - a) * length >= 3.0 {
                            self.context[trip_index] = Some(parents[i].0);
                        }
                        let (a, b) = (x + a * (y - x), x + b * (y - x));
                        let interval = (a.min(b).clamp(0.0, 1.0), a.max(b).clamp(0.0, 1.0));
                        if visit[i].0 != Some(trip_index) {
                            visit[i] = (Some(trip_index), Vec::new(), false);
                        }
                        visit[i].1.push(interval);
                        if !result[i].traveled {
                            evidence[i].push(interval);
                        }
                        touched.push(i);
                    }
                }
            }
            // Finish each trip's sections before next trip, so first completion is known.
            touched.sort_unstable();
            touched.dedup();
            for i in touched {
                let (_, own, counted) = &mut visit[i];
                if !*counted && merge(own) >= 0.8 {
                    *counted = true;
                    result[i].visits += 1;
                }
                if !result[i].traveled && merge(&mut evidence[i]) >= 0.8 {
                    result[i].traveled = true;
                    result[i].first_trip = Some(trip.id);
                }
            }
        }
    }
}

/// Merges intervals in place; returns covered fraction.
fn merge(intervals: &mut Vec<(f64, f64)>) -> f64 {
    intervals.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut merged: Vec<(f64, f64)> = Vec::new();
    for &(a, b) in intervals.iter() {
        if let Some(last) = merged.last_mut().filter(|last| a <= last.1) {
            last.1 = last.1.max(b);
        } else {
            merged.push((a, b));
        }
    }
    *intervals = merged;
    intervals.iter().map(|(a, b)| b - a).sum()
}
