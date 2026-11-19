//! Platform-neutral parts of mobile integration: GPS fix rules, trip status text, sharing
//! interval, simulated demo GPS, and document import/export bodies. Platform modules
//! (`android`) own permissions, services, pickers and notifications, and call into here.
// Desktop builds compile this only for tests.
#![cfg_attr(not(target_os = "android"), allow(dead_code))]
use crate::backend::request;
use crate::geo::Point;
use crate::model::Fix;
use serde_json::{Value, json};
use std::io::{Read, Write};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime};

/// Largest picked image read into memory.
const IMAGE_LIMIT: u64 = 32 * 1024 * 1024;

pub(crate) fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

/// Shows `message` in the UI on its next refresh; empty clears it. Works from any process.
pub(crate) fn status(directory: &Path, message: &str) {
    let _ = request(
        directory.to_owned(),
        json!({"op": "platform_status", "message": message}),
    );
}

/// First launch: miles where the region uses them, written before the UI reads units.
pub(crate) fn default_units(directory: &Path, country: Option<&str>) {
    let path = directory.join("units");
    if path.exists() {
        return;
    }
    let miles = matches!(country, Some("US" | "GB" | "LR" | "MM"));
    let _ = std::fs::write(path, if miles { "imperial" } else { "metric" });
}

/// One position report from the platform location service.
pub(crate) struct Reading {
    pub lat: f64,
    pub lon: f64,
    pub accuracy_m: Option<f32>,
    pub speed: Option<f32>,
    pub bearing: Option<f32>,
    pub time_ms: u64,
    /// How old the fix was when delivered.
    pub age: Duration,
}

impl Reading {
    /// Fix when fresh enough and carrying accuracy; cached fixes delivered on (re)start are older.
    fn fix(&self, max_age: Duration) -> Option<Fix> {
        let accuracy = self.accuracy_m?;
        (self.age <= max_age).then_some(Fix {
            point: Point {
                lat: self.lat,
                lon: self.lon,
            },
            timestamp_ms: self.time_ms,
            accuracy_m: f64::from(accuracy),
        })
    }

    /// Fix for trip recording: rejects cached fixes after starting or resuming GPS.
    pub fn recording_fix(&self) -> Option<Fix> {
        self.fix(Duration::from_secs(15))
    }

    /// Fix for location sharing, which tolerates older positions.
    pub fn sharing_fix(&self) -> Option<Fix> {
        self.fix(Duration::from_secs(120))
    }

    /// Map-follow command, or `None` for stale fixes.
    pub fn position_command(&self) -> Option<Value> {
        (self.age <= Duration::from_secs(120)).then(|| {
            json!({"op": "position", "lat": self.lat, "lon": self.lon,
                   "accuracy_m": self.accuracy_m.unwrap_or(100.0)})
        })
    }
}

/// Recorder journal event for start/pause/resume/finish; `payload` carries start's category.
pub(crate) fn command_event(op: &str, payload: &str, timestamp_ms: u64) -> Value {
    let mut event = json!({"op": op, "timestamp_ms": timestamp_ms});
    if op == "start" {
        let category = serde_json::from_str::<Value>(payload)
            .ok()
            .and_then(|p| p["category_id"].as_u64())
            .filter(|c| *c != 0);
        event["category_id"] = json!(category);
    }
    event
}

/// Commits recording fix to journal; returns trip status text for the notification.
pub(crate) fn record_fix(directory: &Path, fix: &Fix) -> Result<String, String> {
    // Accuracy came from the platform as f32; journal keeps that precision.
    let event = json!({"op": "fix", "fix": {"lat": fix.point.lat, "lon": fix.point.lon,
        "accuracy_m": fix.accuracy_m as f32, "timestamp_ms": fix.timestamp_ms}});
    let result = request(directory.to_owned(), event)?;
    let miles =
        std::fs::read_to_string(directory.join("units")).is_ok_and(|u| u.trim() == "imperial");
    Ok(trip_text(&result, fix.accuracy_m, miles))
}

/// "1.23 km · GPS ±5 m" from a fix commit result.
fn trip_text(result: &Value, accuracy_m: f64, miles: bool) -> String {
    let meters = result["distance_m"].as_f64().unwrap_or(0.0);
    let distance = if miles {
        format!("{:.2} mi", meters / 1609.344)
    } else {
        format!("{:.2} km", meters / 1000.0)
    };
    let gps = if accuracy_m > 30.0 {
        " · weak GPS".to_owned()
    } else {
        format!(" · GPS ±{} m", accuracy_m.round())
    };
    let estimated = if result["estimated_distance_m"].as_f64().unwrap_or(0.0) > 0.0 {
        " · includes inferred gaps"
    } else {
        ""
    };
    format!("{distance}{gps}{estimated}")
}

/// Adaptive sharing interval: slow when stationary, user's update interval while moving.
pub(crate) fn share_interval(minimum_interval_ms: i64, speed: Option<f32>) -> Duration {
    let moving = Duration::from_millis(minimum_interval_ms.clamp(1000, 600_000) as u64);
    match speed {
        Some(speed) if speed >= 5.0 => moving,
        Some(speed) if speed >= 0.5 => moving.max(Duration::from_secs(30)),
        _ => Duration::from_secs(300),
    }
}

/// Stops simulated GPS thread when dropped.
pub(crate) struct Simulated(Arc<AtomicBool>);
impl Drop for Simulated {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Relaxed);
    }
}

/// Demo GPS: reports position along `route` once per second until dropped.
pub(crate) fn simulate(
    route: crate::demo::Route,
    callback: impl Fn(Reading) + Send + 'static,
) -> Simulated {
    let stop = Arc::new(AtomicBool::new(false));
    let stopped = stop.clone();
    std::thread::spawn(move || {
        while !stopped.load(Ordering::Relaxed) {
            let time_ms = now_ms();
            let p = route.at(time_ms);
            callback(Reading {
                lat: p.lat,
                lon: p.lon,
                accuracy_m: Some(5.0),
                speed: None,
                bearing: None,
                time_ms,
                age: Duration::ZERO,
            });
            std::thread::sleep(Duration::from_secs(1));
        }
    });
    Simulated(stop)
}

/// Copies picked PMTiles into private storage; Rust validates it on reopen.
pub(crate) fn import_basemap(directory: &Path, mut input: impl Read) -> Result<(), String> {
    let target = directory.join("basemap.pmtiles");
    let partial = directory.join("basemap.pmtiles.tmp");
    let copied = (|| {
        let mut output = std::fs::File::create(&partial)?;
        std::io::copy(&mut input, &mut output)?;
        output.sync_all()?;
        std::fs::rename(&partial, &target)
    })();
    if let Err(error) = copied {
        let _ = std::fs::remove_file(&partial);
        return Err(error.to_string());
    }
    request(directory.to_owned(), json!({"op": "basemap_imported"})).map(drop)
}

/// Writes all trips as GPX.
pub(crate) fn export_gpx(directory: &Path, mut output: impl Write) -> Result<(), String> {
    let value = request(directory.to_owned(), json!({"op": "gpx"}))?;
    let gpx = value["gpx"].as_str().unwrap_or_default();
    output.write_all(gpx.as_bytes()).map_err(|e| e.to_string())
}

/// Writes journal (trips, categories, edits, areas) and syncs it to storage.
pub(crate) fn backup(directory: &Path, output: &mut std::fs::File) -> Result<(), String> {
    let value = request(directory.to_owned(), json!({"op": "backup"}))?;
    let journal = value["journal"].as_str().unwrap_or_default();
    output
        .write_all(journal.as_bytes())
        .and_then(|()| output.sync_all())
        .map_err(|e| e.to_string())
}

/// Replaces journal with backup after Rust validates every line; returns trip count.
pub(crate) fn restore(directory: &Path, mut input: impl Read) -> Result<u64, String> {
    let partial = directory.join("trips.jsonl.restore");
    (|| {
        let mut output = std::fs::File::create(&partial)?;
        std::io::copy(&mut input, &mut output)?;
        output.sync_all()
    })()
    .map_err(|e| e.to_string())?;
    let value = request(directory.to_owned(), json!({"op": "restore"}))?;
    Ok(value["trips"].as_u64().unwrap_or(0))
}

/// Reads a picked image, capped so a huge file cannot exhaust memory.
pub(crate) fn read_image(input: impl Read) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    input
        .take(IMAGE_LIMIT)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    Ok(bytes)
}

/// Picked photo for the crop step: shrunk so longer side is at most 1024 px.
pub(crate) struct PickedImage {
    pub image: image::DynamicImage,
    /// PNG of `image`, for showing it while cropping.
    pub png: Vec<u8>,
}

pub(crate) fn picked_image(bytes: &[u8]) -> Result<PickedImage, String> {
    let image =
        image::load_from_memory(bytes).map_err(|e| format!("Could not use picture: {e}"))?;
    let image = if image.width().max(image.height()) > 1024 {
        image.resize(1024, 1024, image::imageops::FilterType::Triangle)
    } else {
        image
    };
    Ok(PickedImage {
        png: png(&image)?,
        image,
    })
}

/// Square `[x, y, side]` of `image` as 128 px PNG profile picture.
pub(crate) fn avatar_png(
    image: &image::DynamicImage,
    [x, y, side]: [u32; 3],
) -> Result<Vec<u8>, String> {
    let small = image.crop_imm(x, y, side.max(1), side.max(1)).resize_exact(
        128,
        128,
        image::imageops::FilterType::Lanczos3,
    );
    png(&small)
}

fn png(image: &image::DynamicImage) -> Result<Vec<u8>, String> {
    let mut png = Vec::new();
    image
        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .map_err(|e| e.to_string())?;
    Ok(png)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reading(accuracy_m: Option<f32>, age_s: u64) -> Reading {
        Reading {
            lat: 45.5,
            lon: -122.6,
            accuracy_m,
            speed: None,
            bearing: None,
            time_ms: 1_000,
            age: Duration::from_secs(age_s),
        }
    }

    #[test]
    fn fix_rules_reject_stale_or_unmeasured_fixes() {
        assert!(reading(Some(5.0), 15).recording_fix().is_some());
        assert!(reading(Some(5.0), 16).recording_fix().is_none());
        assert!(reading(None, 0).recording_fix().is_none());
        assert!(reading(Some(5.0), 120).sharing_fix().is_some());
        assert!(reading(Some(5.0), 121).sharing_fix().is_none());
        assert!(reading(None, 0).position_command().is_some());
        assert!(reading(None, 121).position_command().is_none());
    }

    #[test]
    fn start_event_keeps_only_real_category() {
        assert_eq!(
            command_event("start", r#"{"category_id":3}"#, 9)["category_id"],
            3
        );
        assert!(command_event("start", r#"{"category_id":0}"#, 9)["category_id"].is_null());
        assert!(
            command_event("pause", r#"{"category_id":3}"#, 9)
                .get("category_id")
                .is_none()
        );
    }

    #[test]
    fn trip_text_formats_units_accuracy_and_gaps() {
        let result = json!({"distance_m": 1609.344, "estimated_distance_m": 0.0});
        assert_eq!(trip_text(&result, 4.6, true), "1.00 mi · GPS ±5 m");
        assert_eq!(trip_text(&result, 31.0, false), "1.61 km · weak GPS");
        let gaps = json!({"distance_m": 0.0, "estimated_distance_m": 10.0});
        assert_eq!(
            trip_text(&gaps, 5.0, false),
            "0.00 km · GPS ±5 m · includes inferred gaps"
        );
    }

    #[test]
    fn share_interval_slows_when_stationary() {
        assert_eq!(share_interval(5_000, Some(10.0)), Duration::from_secs(5));
        assert_eq!(share_interval(5_000, Some(1.0)), Duration::from_secs(30));
        assert_eq!(share_interval(5_000, None), Duration::from_secs(300));
        assert_eq!(share_interval(0, Some(10.0)), Duration::from_secs(1));
    }

    #[test]
    fn default_units_follow_region_once() {
        let dir = tempfile::tempdir().unwrap();
        default_units(dir.path(), Some("US"));
        default_units(dir.path(), Some("DE"));
        assert_eq!(
            std::fs::read_to_string(dir.path().join("units")).unwrap(),
            "imperial"
        );
    }
}
