use crate::{
    coverage::Coverage,
    geo::Point,
    model::{Event, State, TripFilter},
    pmtiles::Archive,
    recovery::Recovery,
    roads,
    store::Store,
};
use serde_json::{Value, json};
use std::{collections::BTreeSet, path::PathBuf, sync::Mutex};

struct Session {
    directory: PathBuf,
    store: Store,
    coverage: Coverage,
    road_count: usize,
    position: Value,
    basemap: u64,
    /// Offline map; coverage roads come from its tiles near GPS history.
    archive: Option<Archive>,
    road_tiles: BTreeSet<(u32, u32)>,
    recovered: State,
    recovered_revision: u64,
    coverage_revision: u64,
    recovery: Recovery,
    /// Road-matched history for coverage, valid for `matched_revision`.
    matched: State,
    matched_revision: u64,
}

/// Tile limit for area/nearest road loading: about 20 km square at z15.
const AREA_TILES: usize = 600;
/// Largest area: totals only, about 100 km square at z15.
const LARGE_AREA_TILES: usize = 12_000;
/// Loaded road tiles before older areas are dropped (history tiles reload on snapshot).
const ROAD_TILES: usize = 1500;
/// Nearest untraveled road search radius.
const NEAREST_M: f64 = 1500.0;

fn trip_filter(command: &Value) -> Result<TripFilter, String> {
    command
        .get("trip_filter")
        .map(|v| serde_json::from_value(v.clone()))
        .transpose()
        .map_err(|e| e.to_string())
        .map(Option::unwrap_or_default)
}

impl Session {
    fn max_zoom(&self) -> Result<u8, String> {
        self.archive
            .as_ref()
            .map(|a| a.max_zoom)
            .ok_or_else(|| "Download offline map (Get map) for road data".into())
    }

    /// Adds road tiles, rebuilding roads only when something new is needed.
    fn load_tiles(&mut self, needed: BTreeSet<(u32, u32)>) -> Result<(), String> {
        let Some(archive) = &self.archive else {
            return Ok(());
        };
        if needed.is_subset(&self.road_tiles) {
            return Ok(());
        }
        if self.road_tiles.len() + needed.len() > ROAD_TILES {
            self.road_tiles.clear();
        }
        self.road_tiles.extend(needed);
        let roads = roads::roads(archive, &self.road_tiles)?;
        self.road_count = roads.len();
        self.coverage = Coverage::new(&roads);
        self.recovery = Recovery::new(&roads);
        self.recovered_revision = u64::MAX;
        self.coverage_revision = u64::MAX;
        self.matched_revision = u64::MAX;
        Ok(())
    }

    fn update_coverage(&mut self, filter: TripFilter) {
        let revision = self.store.revision();
        if self.matched_revision != revision {
            self.matched = self.recovery.coverage_state(&self.store.state);
            self.matched_revision = revision;
        }
        if self.coverage_revision != revision {
            self.coverage.rebuild_filtered(&self.matched, filter);
            self.coverage_revision = revision;
        } else {
            self.coverage.update_filtered(&self.matched, filter);
        }
    }

    fn reset_roads(&mut self) {
        self.road_tiles.clear();
        self.coverage = Coverage::new(&[]);
        self.recovery = Recovery::new(&[]);
        self.recovered_revision = u64::MAX;
        self.coverage_revision = u64::MAX;
        self.matched_revision = u64::MAX;
        self.road_count = 0;
    }
}

/// Held by process recording GPS (Android `:recorder` service process). Lock outlives
/// nothing: process death releases it, which is how UI process detects dead recorder.
static RECORDER: Mutex<Option<std::fs::File>> = Mutex::new(None);

fn lock_file(directory: &std::path::Path) -> Result<std::fs::File, String> {
    std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(directory.join("recorder.lock"))
        .map_err(|e| e.to_string())
}

fn hold_recorder(directory: &std::path::Path) -> Result<(), String> {
    let mut held = RECORDER.lock().map_err(|_| "Recorder lock failed")?;
    if held.is_none() {
        let file = lock_file(directory)?;
        if !crate::flock::try_lock(&file) {
            return Err("Another process is recording".into());
        }
        *held = Some(file);
    }
    Ok(())
}

fn release_recorder() {
    if let Ok(mut held) = RECORDER.lock() {
        *held = None;
    }
}

/// Some process (this one or another) is recording.
fn recorder_alive(directory: &std::path::Path) -> bool {
    if RECORDER.lock().is_ok_and(|h| h.is_some()) {
        return true;
    }
    let Ok(file) = lock_file(directory) else {
        return false;
    };
    // Dropping file releases probe lock.
    !crate::flock::try_lock(&file)
}

/// Messages from either process (GPS errors from recorder, permission notes from UI).
fn status_path(directory: &std::path::Path) -> PathBuf {
    directory.join("platform_status")
}

fn open_basemap(directory: &std::path::Path) -> Option<Archive> {
    Archive::open(&directory.join("basemap.pmtiles")).ok()
}

static SESSION: Mutex<Option<Session>> = Mutex::new(None);

pub(crate) fn request(directory: PathBuf, command: Value) -> Result<Value, String> {
    let mut guard = SESSION.lock().map_err(|_| "Recording lock failed")?;
    if guard.is_none() {
        let mut store = Store::open(directory.join("trips.jsonl"))?;
        // Live recorder process owns active trip; otherwise restart interrupted it.
        if !recorder_alive(&directory) {
            store.recover_recording()?;
        }
        *guard = Some(Session {
            archive: open_basemap(&directory),
            directory: directory.clone(),
            store,
            coverage: Coverage::new(&[]),
            road_count: 0,
            position: Value::Null,
            basemap: 0,
            road_tiles: BTreeSet::new(),
            recovered: State::default(),
            recovered_revision: u64::MAX,
            coverage_revision: u64::MAX,
            recovery: Recovery::new(&[]),
            matched: State::default(),
            matched_revision: u64::MAX,
        });
    }
    let session = guard.as_mut().unwrap();
    if session.directory != directory {
        return Err("Recording directory changed".into());
    }
    let op = command["op"].as_str().ok_or("Missing operation")?;
    match op {
        "platform_status" => {
            let message = command["message"].as_str().unwrap_or_default();
            let partial = directory.join("platform_status.tmp");
            std::fs::write(&partial, message)
                .and_then(|_| std::fs::rename(&partial, status_path(&directory)))
                .map_err(|e| e.to_string())?;
            Ok(json!({"ok": true}))
        }
        "position" => {
            // Live map position only; trip distance comes from journaled fixes.
            session.position = json!({"lat": command["lat"], "lon": command["lon"], "accuracy_m": command["accuracy_m"]});
            Ok(json!({"ok": true}))
        }
        "basemap_imported" => {
            session.basemap += 1;
            session.archive = open_basemap(&directory);
            session.reset_roads();
            Ok(json!({"ok": true}))
        }
        "gpx" => {
            session.store.catch_up()?;
            if session.recovered_revision != session.store.revision() {
                session.recovered = session.recovery.apply(&session.store.state);
                session.recovered_revision = session.store.revision();
            }
            Ok(json!({"gpx": crate::gpx::export(&session.recovered)}))
        }
        "interrupt" => {
            session.store.recover_recording()?;
            release_recorder();
            Ok(json!({"ok": true}))
        }
        "snapshot" => {
            let filter = trip_filter(&command)?;
            session.store.catch_up()?;
            // Recorder process died mid-trip: pause at last fix, as on restart.
            if session
                .store
                .state
                .active()
                .is_some_and(|t| t.status == crate::model::Status::Recording)
                && !recorder_alive(&directory)
            {
                session.store.recover_recording()?;
            }
            let map = command["map"].as_bool().unwrap_or(false);
            if map && let Some(archive) = &session.archive {
                let needed = roads::tiles(&session.store.state, archive.max_zoom);
                session.load_tiles(needed)?;
            }
            if session.recovered_revision != session.store.revision() {
                session.recovered = session.recovery.apply(&session.store.state);
                session.recovered_revision = session.store.revision();
            }
            let (mut sections, mut total, mut traveled) = (None, 0.0, 0.0);
            if map {
                session.update_coverage(filter.clone());
                // Untraveled sections stay in backend; areas can load thousands of tiles.
                let mut kept = Vec::new();
                for section in &session.coverage.sections {
                    let length = section.start.distance(section.end);
                    total += length;
                    if section.traveled {
                        traveled += length;
                        kept.push(section);
                    }
                }
                sections = Some(kept);
            }
            Ok(
                json!({"state": session.recovered, "total_distance_m": session.recovered.total_distance(None), "sections": sections, "road_total_m": total, "road_traveled_m": traveled, "coverage_filter":filter, "road_count": session.road_count, "platform_status": std::fs::read_to_string(status_path(&directory)).unwrap_or_default(), "position": session.position, "basemap": session.basemap}),
            )
        }
        "area" => {
            let filter = trip_filter(&command)?;
            let points: Vec<Point> =
                serde_json::from_value(command["points"].clone()).map_err(|e| e.to_string())?;
            if points.len() < 3 {
                return Err("Area needs at least 3 corners".into());
            }
            let z = session.max_zoom()?;
            let tiles = roads::bbox_tiles(&points, 0.0, z, LARGE_AREA_TILES)
                .ok_or("Area too large; draw area under about 100 km across")?;
            session.store.catch_up()?;
            // Small areas load full coverage so untraveled roads can be drawn. Large ones
            // only need traveled sections, which all lie near history.
            let detailed = tiles.len() <= AREA_TILES;
            let history = roads::tiles(&session.store.state, z);
            session.load_tiles(if detailed { tiles.clone() } else { history })?;
            session.update_coverage(filter.clone());
            let mut area = crate::area::Area::new(&points);
            let archive = session.archive.as_ref().ok_or("Offline map missing")?;
            let tiles: Vec<_> = tiles.into_iter().collect();
            // Totals stream from map tiles in batches; never all roads in memory.
            for chunk in tiles.chunks(200) {
                area.add_roads(&roads::roads(archive, &chunk.iter().copied().collect())?);
            }
            area.add_sections(&session.coverage.sections, detailed);
            let mut stats = area.finish();
            stats.large = !detailed;
            Ok(json!(stats))
        }
        "nearest" => {
            let filter = trip_filter(&command)?;
            let from: Point = serde_json::from_value(command.clone()).map_err(|e| e.to_string())?;
            let z = session.max_zoom()?;
            let tiles = roads::bbox_tiles(&[from], NEAREST_M, z, AREA_TILES).unwrap_or_default();
            session.store.catch_up()?;
            session.load_tiles(tiles)?;
            session.update_coverage(filter.clone());
            // Prefer roads never traveled; small holes in ridden roads only as fallback.
            let sections = &session.coverage.sections;
            let ridden: std::collections::HashSet<&str> = sections
                .iter()
                .filter(|s| s.traveled)
                .map(|s| s.road_id.as_str())
                .collect();
            let closest = |fresh: bool| {
                sections
                    .iter()
                    .filter(|s| !s.traveled && (!fresh || !ridden.contains(s.road_id.as_str())))
                    .map(|s| (s, from.distance(s.start.interpolate(s.end, 0.5))))
                    .min_by(|a, b| a.1.total_cmp(&b.1))
                    .filter(|(_, d)| *d <= NEAREST_M)
            };
            let nearest = closest(true)
                .or_else(|| closest(false))
                .ok_or("No untraveled road in offline map nearby")?;
            Ok(json!({"section": nearest.0, "distance_m": nearest.1}))
        }
        "backup" => Ok(json!({"journal": String::from_utf8(session.store.backup()?)
            .map_err(|e| e.to_string())?})),
        "restore" => {
            session.store.catch_up()?;
            if session.store.state.active().is_some() || recorder_alive(&directory) {
                return Err("Finish current trip before restoring".into());
            }
            let incoming = directory.join("trips.jsonl.restore");
            // Parse whole backup before touching current journal.
            let checked = Store::open(&incoming).map(|store| store.state.trips.len());
            if let Err(error) = checked {
                let _ = std::fs::remove_file(&incoming);
                return Err(format!("Not a Roamlog backup: {error}"));
            }
            let journal = directory.join("trips.jsonl");
            std::fs::copy(&journal, directory.join("trips.jsonl.before-restore"))
                .and_then(|_| std::fs::rename(&incoming, &journal))
                .map_err(|e| e.to_string())?;
            session.store = Store::open(&journal)?;
            session.store.recover_recording()?;
            session.reset_roads();
            Ok(json!({"trips": checked.unwrap_or(0)}))
        }
        _ => {
            let event: Event = serde_json::from_value(command).map_err(|e| e.to_string())?;
            // Coverage indexes trips by position; edits renumber or reshape them.
            let delete = matches!(
                event,
                Event::DeleteTrip { .. } | Event::SplitTrip { .. } | Event::TrimTrip { .. }
            );
            let (begin, end) = (
                matches!(event, Event::Start { .. } | Event::Resume { .. }),
                matches!(event, Event::Pause { .. } | Event::Finish { .. }),
            );
            // Lock before commit, so other process never sees recording without live lock.
            if begin {
                hold_recorder(&directory)?;
            }
            if let Err(error) = session.store.commit(event) {
                if begin {
                    release_recorder();
                }
                return Err(error);
            }
            if end {
                release_recorder();
            }
            if delete {
                // Coverage indexes trips by position; rebuild from remaining history.
                session.reset_roads();
            }
            Ok(
                json!({"ok": true, "distance_m": session.store.state.active().map(|t| t.distance_m), "estimated_distance_m":session.store.state.active().map(|t|t.estimated_distance_m)}),
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn android_commands_persist_trips_and_cover_basemap_roads() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_path_buf();
        let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/downtown.pmtiles");
        std::fs::copy(&fixture, path.join("basemap.pmtiles")).unwrap();
        // Walk longest piece of a real street from fixture map.
        let archive = Archive::open(&fixture).unwrap();
        let street = roads::roads(&archive, &BTreeSet::from([(5217, 11720)]))
            .unwrap()
            .into_iter()
            .filter(|r| r.id == "secondary:Southwest Broadway")
            .max_by_key(|r| r.points.len())
            .unwrap();
        let call = |command| request(path.clone(), command).unwrap();
        call(json!({"op": "category", "name": "Running"}));
        call(json!({"op": "start", "timestamp_ms": 0, "category_id": 1}));
        let mut t = 1000;
        for pair in street.points.windows(2) {
            let steps = (pair[0].distance(pair[1]) / 8.0).ceil() as usize;
            for i in 0..steps {
                let f = i as f64 / steps as f64;
                let (lat, lon) = (
                    pair[0].lat + f * (pair[1].lat - pair[0].lat),
                    pair[0].lon + f * (pair[1].lon - pair[0].lon),
                );
                call(
                    json!({"op": "fix", "fix": {"lat":lat,"lon":lon,"timestamp_ms":t,"accuracy_m":3}}),
                );
                t += 2000;
            }
        }
        let result = call(json!({"op": "snapshot"}));
        assert!(result["total_distance_m"].as_f64().unwrap() > 100.0);
        assert!(result["sections"].is_null());
        let result = call(json!({"op":"snapshot", "map":true}));
        assert!(result["road_count"].as_u64().unwrap() > 50);
        let traveled: Vec<_> = result["sections"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|s| s["traveled"] == true)
            .collect();
        assert!(traveled.len() > 3, "{} traveled", traveled.len());
        assert!(
            traveled
                .iter()
                .all(|s| s["road_id"] == "secondary:Southwest Broadway")
        );
        let result = call(
            json!({"op":"snapshot","map":true,"trip_filter":{"kind":"category","category_id":999}}),
        );
        assert!(
            result["sections"]
                .as_array()
                .unwrap()
                .iter()
                .all(|s| s["traveled"] == false)
        );
        assert_eq!(result["state"]["trips"].as_array().unwrap().len(), 1);
        let result =
            call(json!({"op":"snapshot","map":true,"trip_filter":{"kind":"trip","trip_id":1}}));
        assert!(
            result["sections"]
                .as_array()
                .unwrap()
                .iter()
                .any(|s| s["traveled"] == true)
        );
        assert!(
            call(json!({"op":"snapshot","map":true}))["sections"]
                .as_array()
                .unwrap()
                .iter()
                .any(|s| s["traveled"] == true)
        );
        // Area around walked street: some of its roads traveled, all loaded.
        let lat = street.points.iter().map(|p| p.lat);
        let lon = street.points.iter().map(|p| p.lon);
        let (s0, n0) = (lat.clone().fold(90.0, f64::min), lat.fold(-90.0, f64::max));
        let (w0, e0) = (
            lon.clone().fold(180.0, f64::min),
            lon.fold(-180.0, f64::max),
        );
        let area = call(json!({"op":"area","points":[
            {"lat":s0-0.001,"lon":w0-0.001},{"lat":s0-0.001,"lon":e0+0.001},
            {"lat":n0+0.001,"lon":e0+0.001},{"lat":n0+0.001,"lon":w0-0.001}]}));
        let (total, traveled) = (
            area["total_m"].as_f64().unwrap(),
            area["traveled_m"].as_f64().unwrap(),
        );
        assert!(traveled > 50.0 && traveled < total, "{traveled} of {total}");
        assert!(!area["untraveled"].as_array().unwrap().is_empty());
        let start = street.points[0];
        let nearest = call(json!({"op":"nearest","lat":start.lat,"lon":start.lon}));
        assert_eq!(nearest["section"]["traveled"], false);
        assert!(nearest["distance_m"].as_f64().unwrap() < 200.0);
        // Snapshot keeps untraveled sections in backend, but reports totals.
        let result = call(json!({"op":"snapshot","map":true}));
        assert!(
            result["sections"]
                .as_array()
                .unwrap()
                .iter()
                .all(|s| s["traveled"] == true)
        );
        assert!(
            result["road_total_m"].as_f64().unwrap() > result["road_traveled_m"].as_f64().unwrap()
        );
        call(json!({"op":"position", "lat":1.5, "lon":2.5, "accuracy_m":8}));
        assert_eq!(call(json!({"op":"snapshot"}))["position"]["lat"], 1.5);
        // Recording stays live while recorder lock is held...
        let result = call(json!({"op":"snapshot"}));
        assert_eq!(result["state"]["trips"][0]["status"], "recording");
        // ...and pauses once recorder process is gone (lock released).
        release_recorder();
        let result = call(json!({"op":"snapshot"}));
        assert_eq!(result["state"]["trips"][0]["status"], "paused");
        call(json!({"op":"platform_status", "message":"GPS disabled"}));
        assert_eq!(
            call(json!({"op":"snapshot"}))["platform_status"],
            "GPS disabled"
        );
        // Backup, change history, restore: backup wins; old journal kept aside.
        call(json!({"op":"finish","timestamp_ms":t}));
        let backup = call(json!({"op":"backup"}))["journal"]
            .as_str()
            .unwrap()
            .to_owned();
        call(json!({"op":"category","name":"Later"}));
        std::fs::write(path.join("trips.jsonl.restore"), "not json\n").unwrap();
        assert!(request(path.clone(), json!({"op":"restore"})).is_err());
        assert_eq!(
            call(json!({"op":"snapshot"}))["state"]["categories"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
        std::fs::write(path.join("trips.jsonl.restore"), &backup).unwrap();
        assert_eq!(call(json!({"op":"restore"}))["trips"], 1);
        let restored = call(json!({"op":"snapshot"}));
        assert_eq!(restored["state"]["categories"].as_array().unwrap().len(), 1);
        assert!(path.join("trips.jsonl.before-restore").exists());
        // Release journal before TempDir cleanup; only this test uses singleton.
        *SESSION.lock().unwrap() = None;
        let persisted = Store::open(path.join("trips.jsonl")).unwrap();
        assert_eq!(persisted.state.categories[0].name, "Running");
    }
}
