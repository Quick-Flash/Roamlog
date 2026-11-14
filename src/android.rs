//! Android integration through zgui: the activity entry point, platform requests from the UI,
//! and the trip recorder, which runs in the `:recorder` service process so trips continue after
//! the activity closes.
use crate::backend::request;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime};
use zgui::app::android::{background, document, location, notification, permission, system};

#[unsafe(no_mangle)]
pub fn android_main(app: zgui::app::AndroidApp) {
    static ENTERED: AtomicBool = AtomicBool::new(false);
    if ENTERED.swap(true, Ordering::SeqCst) {
        std::process::exit(0);
    }
    let directory = app.internal_data_path().expect("Android internal storage");
    default_units(&directory, app.config().country().as_deref());
    crate::ui::run_android(app, directory);
}

zgui::app::android_background!(recorder);

/// First launch: miles where the region uses them, written before the UI reads units.
fn default_units(directory: &Path, country: Option<&str>) {
    let path = directory.join("units");
    if path.exists() {
        return;
    }
    let miles = matches!(country, Some("US" | "GB" | "LR" | "MM"));
    let _ = std::fs::write(path, if miles { "imperial" } else { "metric" });
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

/// Shows `message` in the UI on its next refresh; empty clears it. Works from any process.
fn status(directory: &Path, message: &str) {
    let _ = request(
        directory.to_owned(),
        json!({"op": "platform_status", "message": message}),
    );
}

fn trip_notification(text: &str) -> notification::Notification {
    notification::Notification {
        id: 1,
        title: "Roamlog".into(),
        text: text.into(),
        ongoing: true,
        actions: vec![("finish".into(), "Finish trip".into())],
        ..Default::default()
    }
}

/// Real location watch, or simulated one in demo app. Dropping it stops updates.
type Watch = Box<dyn Send>;

/// Stops simulated feed thread when dropped.
struct Simulated(std::sync::Arc<AtomicBool>);
impl Drop for Simulated {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Relaxed);
    }
}

fn watch(
    directory: &Path,
    request: location::Request,
    callback: impl Fn(location::Update) + Send + 'static,
) -> Result<Watch, String> {
    let Some(route) = crate::demo::Route::load(directory) else {
        return Ok(Box::new(location::watch(request, callback)?));
    };
    let stop = std::sync::Arc::new(AtomicBool::new(false));
    let stopped = stop.clone();
    std::thread::spawn(move || {
        while !stopped.load(Ordering::Relaxed) {
            let time_ms = now_ms();
            let p = route.at(time_ms);
            callback(location::Update::Fix(location::Location {
                lat: p.lat,
                lon: p.lon,
                accuracy_m: Some(5.0),
                altitude_m: None,
                speed: None,
                bearing: None,
                time_ms,
                age: Duration::ZERO,
                provider: "gps".into(),
            }));
            std::thread::sleep(Duration::from_secs(1));
        }
    });
    Ok(Box::new(Simulated(stop)))
}

fn demo(directory: &Path) -> bool {
    crate::demo::Route::load(directory).is_some()
}

// --- UI process ---------------------------------------------------------------------------

/// Starts, pauses, resumes or finishes recording in the recorder service.
pub(crate) fn record(directory: PathBuf, op: &str, category_id: u64) -> Result<(), String> {
    let payload = json!({"category_id": category_id}).to_string();
    if !matches!(op, "start" | "resume") {
        return background::send_service(op, &payload);
    }
    let op = op.to_owned();
    if demo(&directory) {
        // Simulated GPS: no location permission, no device GPS.
        return background::start_service(&op, &payload, &trip_notification("Demo GPS"));
    }
    permission::request(
        &[permission::FINE_LOCATION, permission::COARSE_LOCATION],
        move |granted| {
            if !granted || !permission::granted(permission::FINE_LOCATION) {
                status(
                    &directory,
                    "Precise location permission needed to record trip.",
                );
                return;
            }
            if !location::enabled(location::Source::Gps) {
                status(&directory, "Turn on device GPS before recording");
                return;
            }
            let begin = move || {
                status(&directory, "");
                let started =
                    background::start_service(&op, &payload, &trip_notification("Waiting for GPS"));
                if let Err(error) = started {
                    status(&directory, &error);
                }
            };
            // Ask each start until granted; Android stops prompting after repeated denial.
            // Denial still records, without a visible notification.
            if permission::granted(permission::NOTIFICATIONS) {
                begin();
            } else {
                let _ = permission::request(&[permission::NOTIFICATIONS], move |_| begin());
            }
        },
    )
}

/// Live position for map follow. Android pauses it while the activity is hidden.
static FOLLOW: Mutex<Option<Watch>> = Mutex::new(None);

/// Starts following position once location permission exists. `ask` requests it first.
pub(crate) fn follow(directory: PathBuf, ask: bool) -> Result<(), String> {
    if FOLLOW.lock().expect("follow lock").is_some() {
        return Ok(());
    }
    if demo(&directory) || permission::granted(permission::FINE_LOCATION) {
        return start_follow(directory);
    }
    if !ask {
        return Ok(());
    }
    permission::request(
        &[permission::FINE_LOCATION, permission::COARSE_LOCATION],
        move |granted| {
            if granted && let Err(error) = start_follow(directory.clone()) {
                status(&directory, &error);
            }
        },
    )
}

fn start_follow(directory: PathBuf) -> Result<(), String> {
    let watched = directory.clone();
    let guard = watch(&watched, location::Request::default(), move |update| {
        let location::Update::Fix(fix) = update else {
            return;
        };
        if fix.age > Duration::from_secs(120) {
            return;
        }
        let command = json!({"op": "position", "lat": fix.lat, "lon": fix.lon,
               "accuracy_m": fix.accuracy_m.unwrap_or(100.0)});
        // Android's main thread also delivers touch input; journal work runs elsewhere.
        let directory = directory.clone();
        std::thread::spawn(move || drop(request(directory, command)));
    })?;
    *FOLLOW.lock().expect("follow lock") = Some(guard);
    Ok(())
}

/// Copies a picked PMTiles file into private storage; Rust validates it on reopen.
pub(crate) fn import_basemap(directory: PathBuf) -> Result<(), String> {
    document::open("*/*", move |picked| {
        let doc = match picked {
            Ok(Some(doc)) => doc,
            Ok(None) => return,
            Err(error) => {
                return status(&directory, &format!("Offline map import failed: {error}"));
            }
        };
        status(&directory, "Importing offline map…");
        // Own thread: a large copy must not delay the main thread.
        std::thread::spawn(move || {
            let target = directory.join("basemap.pmtiles");
            let partial = directory.join("basemap.pmtiles.tmp");
            let copied = (|| {
                let mut input = doc.file;
                let mut output = std::fs::File::create(&partial)?;
                std::io::copy(&mut input, &mut output)?;
                output.sync_all()?;
                std::fs::rename(&partial, &target)
            })();
            if let Err(error) = copied {
                let _ = std::fs::remove_file(&partial);
                return status(&directory, &format!("Offline map import failed: {error}"));
            }
            match request(directory.clone(), json!({"op": "basemap_imported"})) {
                Ok(_) => status(&directory, "Offline map imported."),
                Err(error) => status(&directory, &error),
            }
        });
    })
}

/// Writes trips as GPX to a document the user creates.
pub(crate) fn export_gpx(directory: PathBuf) -> Result<(), String> {
    document::create("roamlog-trips.gpx", "application/gpx+xml", move |picked| {
        let mut doc = match picked {
            Ok(Some(doc)) => doc,
            Ok(None) => return,
            Err(error) => return status(&directory, &format!("Export failed: {error}")),
        };
        std::thread::spawn(move || {
            let written = request(directory.clone(), json!({"op": "gpx"})).and_then(|value| {
                let gpx = value["gpx"].as_str().unwrap_or_default();
                std::io::Write::write_all(&mut doc.file, gpx.as_bytes()).map_err(|e| e.to_string())
            });
            status(
                &directory,
                &written.map_or_else(
                    |e| format!("Export failed: {e}"),
                    |()| "Trips exported as GPX.".into(),
                ),
            );
        });
    })
}

/// Writes journal (trips, categories, edits, areas) to a document the user creates.
pub(crate) fn backup(directory: PathBuf) -> Result<(), String> {
    document::create(
        "roamlog-backup.jsonl",
        "application/octet-stream",
        move |picked| {
            let mut doc = match picked {
                Ok(Some(doc)) => doc,
                Ok(None) => return,
                Err(error) => return status(&directory, &format!("Backup failed: {error}")),
            };
            std::thread::spawn(move || {
                let written =
                    request(directory.clone(), json!({"op": "backup"})).and_then(|value| {
                        let journal = value["journal"].as_str().unwrap_or_default();
                        std::io::Write::write_all(&mut doc.file, journal.as_bytes())
                            .and_then(|()| doc.file.sync_all())
                            .map_err(|e| e.to_string())
                    });
                status(
                    &directory,
                    &written.map_or_else(
                        |e| format!("Backup failed: {e}"),
                        |()| "Backup saved.".into(),
                    ),
                );
            });
        },
    )
}

/// Replaces journal with picked backup after Rust validates every line.
pub(crate) fn restore(directory: PathBuf) -> Result<(), String> {
    document::open("*/*", move |picked| {
        let doc = match picked {
            Ok(Some(doc)) => doc,
            Ok(None) => return,
            Err(error) => return status(&directory, &format!("Restore failed: {error}")),
        };
        std::thread::spawn(move || {
            let partial = directory.join("trips.jsonl.restore");
            let copied = (|| {
                let mut input = doc.file;
                let mut output = std::fs::File::create(&partial)?;
                std::io::copy(&mut input, &mut output)?;
                output.sync_all()
            })();
            let result = copied
                .map_err(|e| e.to_string())
                .and_then(|()| request(directory.clone(), json!({"op": "restore"})));
            status(
                &directory,
                &result.map_or_else(
                    |e| format!("Restore failed: {e}"),
                    |v| format!("Restored {} trips.", v["trips"].as_u64().unwrap_or(0)),
                ),
            );
        });
    })
}

pub(crate) fn open_settings() -> Result<(), String> {
    system::open_settings(system::Settings::App)
}

// --- Recorder process ---------------------------------------------------------------------

/// GPS watch while recording.
static GPS: Mutex<Option<Watch>> = Mutex::new(None);
static GPS_WAITING: AtomicBool = AtomicBool::new(false);

fn recorder(event: background::Event) {
    use background::Event;
    let Ok((directory, _)) = system::dirs() else {
        return;
    };
    match event {
        Event::Service { action, payload } => command(directory, &action, &payload),
        Event::Action { action, .. } if action == "finish" => command(directory, "finish", "{}"),
        Event::ServiceFailed { error, .. } => {
            status(&directory, &error);
            let _ = request(directory, json!({"op": "interrupt"}));
        }
        Event::ServiceStopped => {
            let recording = GPS.lock().expect("recorder lock").take().is_some();
            if recording {
                let _ = request(directory, json!({"op": "interrupt"}));
            }
        }
        _ => {}
    }
}

fn command(directory: PathBuf, op: &str, payload: &str) {
    // Any command replaces the previous GPS watch.
    GPS.lock().expect("recorder lock").take();
    GPS_WAITING.store(false, Ordering::Relaxed);
    let mut event = json!({"op": op, "timestamp_ms": now_ms()});
    if op == "start" {
        let category = serde_json::from_str::<Value>(payload)
            .ok()
            .and_then(|p| p["category_id"].as_u64())
            .filter(|c| *c != 0);
        event["category_id"] = json!(category);
    }
    if let Err(error) = request(directory.clone(), event) {
        return fail(&directory, &error);
    }
    if !matches!(op, "start" | "resume") {
        let _ = background::stop_service();
        return;
    }
    let _ = notification::post(&trip_notification("Recording · waiting for GPS"));
    let watch_directory = directory.clone();
    let gps = watch(
        &directory,
        location::Request {
            source: location::Source::Gps,
            interval: Duration::from_secs(1),
            last_known: false,
        },
        move |update| fix(&watch_directory, update),
    );
    match gps {
        Ok(gps) => *GPS.lock().expect("recorder lock") = Some(gps),
        Err(error) => interrupt(&directory, &error),
    }
}

fn fix(directory: &Path, update: location::Update) {
    let fix = match update {
        location::Update::Fix(fix) => fix,
        location::Update::Disabled(provider) if provider == "gps" => {
            GPS_WAITING.store(true, Ordering::Relaxed);
            status(directory, "GPS unavailable · recording waits for recovery");
            return;
        }
        location::Update::Disabled(_) => return,
        location::Update::Failed(error) => return interrupt(directory, &error),
    };
    // Reject cached fixes delivered after starting or resuming GPS updates.
    let Some(accuracy) = fix.accuracy_m else {
        return;
    };
    if fix.age > Duration::from_secs(15) {
        return;
    }
    if accuracy <= 30.0 && GPS_WAITING.swap(false, Ordering::Relaxed) {
        status(directory, "");
    }
    let event = json!({"op": "fix", "fix": {"lat": fix.lat, "lon": fix.lon,
        "accuracy_m": accuracy, "timestamp_ms": fix.time_ms}});
    match request(directory.to_owned(), event) {
        Ok(result) => {
            let meters = result["distance_m"].as_f64().unwrap_or(0.0);
            let miles = std::fs::read_to_string(directory.join("units"))
                .is_ok_and(|u| u.trim() == "imperial");
            let distance = if miles {
                format!("{:.2} mi", meters / 1609.344)
            } else {
                format!("{:.2} km", meters / 1000.0)
            };
            let gps = if accuracy > 30.0 {
                " · weak GPS".to_owned()
            } else {
                format!(" · GPS ±{} m", accuracy.round())
            };
            let estimated = if result["estimated_distance_m"].as_f64().unwrap_or(0.0) > 0.0 {
                " · includes inferred gaps"
            } else {
                ""
            };
            let _ = notification::post(&trip_notification(&format!("{distance}{gps}{estimated}")));
        }
        Err(error) => fail(directory, &error),
    }
}

/// Ends recording after an error and stops the service.
fn fail(directory: &Path, error: &str) {
    status(directory, error);
    GPS.lock().expect("recorder lock").take();
    let _ = request(directory.to_owned(), json!({"op": "interrupt"}));
    let _ = background::stop_service();
}

/// Pauses trip after permission or location-watch failure.
fn interrupt(directory: &Path, reason: &str) {
    GPS.lock().expect("recorder lock").take();
    let paused = request(
        directory.to_owned(),
        json!({"op": "pause", "timestamp_ms": now_ms()}),
    );
    match paused {
        Ok(_) => fail(directory, reason),
        Err(error) => fail(directory, &format!("{reason}: {error}")),
    }
}
