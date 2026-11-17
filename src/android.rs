//! Android integration through zgui: the activity entry point, platform requests from the UI,
//! and the trip recorder, which runs in the `:recorder` service process so trips continue after
//! the activity closes.
use crate::backend::request;
use crate::sharing::{android_app as sharing, manager::Mode};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::time::{Duration, SystemTime};
use zgui::app::android::{background, document, location, notification, permission, system};

#[unsafe(no_mangle)]
pub fn android_main(app: zgui::app::AndroidApp) {
    static ENTERED: AtomicBool = AtomicBool::new(false);
    if ENTERED.swap(true, Ordering::SeqCst) {
        std::process::exit(0);
    }
    sharing::install_vm(app.vm_as_ptr());
    crate::android_updates::init(&app);
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

fn trip_notification(directory: &Path, text: &str) -> notification::Notification {
    let mut actions = vec![("finish".into(), "Finish trip".into())];
    if sharing::mode(directory) != Mode::Off {
        actions.push(("pause_sharing".into(), "Pause sharing".into()));
    }
    notification::Notification {
        id: 1,
        title: "Roamlog".into(),
        text: text.into(),
        ongoing: true,
        actions,
        ..Default::default()
    }
}

/// Foreground notification while sharing runs without a trip. Same id as trip notification:
/// one service, one notification.
fn sharing_notification() -> notification::Notification {
    notification::Notification {
        id: 1,
        title: "Roamlog".into(),
        text: "Location sharing on".into(),
        ongoing: true,
        actions: vec![("pause_sharing".into(), "Pause sharing".into())],
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
        return background::start_service(
            &op,
            &payload,
            &trip_notification(&directory, "Demo GPS"),
        );
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
                let started = background::start_service(
                    &op,
                    &payload,
                    &trip_notification(&directory, "Waiting for GPS"),
                );
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

/// Starts or stops the sharing node after the UI changed mode, friends or permissions.
/// Sharing needs location permission: the service shares fixes and is a location service.
pub(crate) fn sharing_changed(directory: PathBuf) -> Result<(), String> {
    if sharing::mode(&directory) == Mode::Off {
        return background::send_service("sharing", "");
    }
    permission::request(
        &[permission::FINE_LOCATION, permission::COARSE_LOCATION],
        move |granted| {
            if !granted {
                status(&directory, "Location permission needed to share location.");
                return;
            }
            let begin = move || {
                if let Err(error) =
                    background::start_service("sharing", "", &sharing_notification())
                {
                    status(&directory, &error);
                }
            };
            if permission::granted(permission::NOTIFICATIONS) {
                begin();
            } else {
                let _ = permission::request(&[permission::NOTIFICATIONS], move |_| begin());
            }
        },
    )
}

/// App launch: restarts sharing after force-stop, process death, or refused boot start.
pub(crate) fn resume_sharing(directory: PathBuf) {
    if sharing::mode(&directory) != Mode::Off {
        let _ = notification::cancel(2);
        if let Err(error) = sharing_changed(directory.clone()) {
            status(&directory, &error);
        }
    }
}

/// Wakes the running node after friends or permissions changed in this process.
pub(crate) fn nudge_sharing(directory: &Path) {
    if sharing::mode(directory) != Mode::Off {
        let _ = background::send_service("sharing", "");
    }
}

/// Invitation link from an opened `roamlog://pair/` link or a scanned image, for the UI.
pub(crate) static PENDING_INVITE: Mutex<Option<String>> = Mutex::new(None);

/// Camera apps open scanned invitation links with Roamlog through the manifest filter.
pub(crate) fn listen_invites() {
    zgui::app::android::intent::on_intent(|intent| {
        if let Some(link) = intent
            .data
            .or(intent.text)
            .filter(|l| l.starts_with("roamlog://pair/"))
        {
            *PENDING_INVITE.lock().expect("invite lock") = Some(link);
        }
    });
}

/// Decodes invitation QR from a picked photo or screenshot.
pub(crate) fn scan_invite(directory: PathBuf) -> Result<(), String> {
    document::open("image/*", move |picked| {
        let doc = match picked {
            Ok(Some(doc)) => doc,
            Ok(None) => return,
            Err(error) => return status(&directory, &format!("Could not open image: {error}")),
        };
        std::thread::spawn(move || {
            let mut bytes = Vec::new();
            let read = std::io::Read::read_to_end(
                &mut std::io::Read::take(doc.file, 32 * 1024 * 1024),
                &mut bytes,
            );
            match read
                .map_err(|e| e.to_string())
                .and_then(|_| sharing::decode_qr_image(&bytes).map_err(|e| e.to_string()))
            {
                Ok(link) => *PENDING_INVITE.lock().expect("invite lock") = Some(link),
                Err(_) => status(&directory, "No Roamlog invitation found in image"),
            }
        });
    })
}

/// Profile picture: center square of picked photo, 128 px PNG, sent to friends.
pub(crate) fn pick_avatar(directory: PathBuf) -> Result<(), String> {
    document::open("image/*", move |picked| {
        let doc = match picked {
            Ok(Some(doc)) => doc,
            Ok(None) => return,
            Err(error) => return status(&directory, &format!("Could not open image: {error}")),
        };
        std::thread::spawn(move || {
            let saved = (|| -> Result<(), String> {
                let mut bytes = Vec::new();
                std::io::Read::read_to_end(
                    &mut std::io::Read::take(doc.file, 32 * 1024 * 1024),
                    &mut bytes,
                )
                .map_err(|e| e.to_string())?;
                let image = image::load_from_memory(&bytes).map_err(|e| e.to_string())?;
                let side = image.width().min(image.height());
                let square = image.crop_imm(
                    (image.width() - side) / 2,
                    (image.height() - side) / 2,
                    side,
                    side,
                );
                let small = square.resize_exact(128, 128, image::imageops::FilterType::Lanczos3);
                let mut png = Vec::new();
                small
                    .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
                    .map_err(|e| e.to_string())?;
                let path = crate::sharing::transport::own_avatar_path(&directory.join("sharing"));
                let partial = path.with_extension("tmp");
                std::fs::write(&partial, png)
                    .and_then(|()| std::fs::rename(&partial, &path))
                    .map_err(|e| e.to_string())
            })();
            status(
                &directory,
                &saved.map_or_else(
                    |e| format!("Could not use picture: {e}"),
                    |()| "Profile picture saved; friends see it within 30 minutes.".into(),
                ),
            );
        });
    })
}

pub(crate) fn share_text(text: &str) {
    zgui::app::android::share(text);
}

/// New sharing identity. Service drops old key before UI deletes it.
pub(crate) fn reset_identity(directory: &Path) -> Result<(), String> {
    let _ = background::send_service("identity_reset", "");
    // Mode file goes with identity; service sees Off and stops node on its own too.
    sharing::reset_identity(directory).map_err(|e| e.to_string())?;
    let _ = background::send_service("identity_reset", "");
    Ok(())
}

/// Pairing runs its own endpoint with the same identity; close the service's one meanwhile.
pub(crate) fn suspend_sharing(directory: &Path) {
    if sharing::mode(directory) != Mode::Off {
        let _ = background::send_service("sharing_suspend", "");
    }
}

pub(crate) fn open_settings() -> Result<(), String> {
    system::open_settings(system::Settings::App)
}

// --- Recorder process ---------------------------------------------------------------------

/// GPS watch while recording.
static GPS: Mutex<Option<Watch>> = Mutex::new(None);
static GPS_WAITING: AtomicBool = AtomicBool::new(false);
/// Sharing-only GPS watch and its interval, while sharing Always without a trip.
static FRESH_UNTIL: AtomicI64 = AtomicI64::new(0);
static SHARE_GPS: Mutex<Option<(Watch, Duration)>> = Mutex::new(None);

fn recorder(event: background::Event) {
    use background::Event;
    let Ok((directory, _)) = system::dirs() else {
        return;
    };
    match event {
        Event::Service { action, .. } if action == "sharing" => settle(&directory),
        Event::Service { action, .. } if action == "locate" => refresh_gps(&directory),
        Event::Alarm { id } if id == LOCATE_DONE => {
            FRESH_UNTIL.store(0, Ordering::Relaxed);
            SHARE_GPS.lock().expect("sharing lock").take();
            settle(&directory);
        }
        Event::Service { action, .. } if action == "identity_reset" => {
            SHARE_GPS.lock().expect("sharing lock").take();
            sharing::stop_node();
            sharing::forget_identity();
            settle(&directory);
        }
        Event::Service { action, .. } if action == "sharing_suspend" => {
            SHARE_GPS.lock().expect("sharing lock").take();
            sharing::stop_node();
        }
        Event::Service { action, payload } => command(directory, &action, &payload),
        Event::Action { action, .. } if action == "finish" => command(directory, "finish", "{}"),
        Event::Action { action, .. } if action == "pause_sharing" => {
            // Blocks on Keystore and file lock; receiver runs this on a worker thread.
            let paused = sharing::manager(&directory).and_then(|m| m.set_mode(Mode::Off));
            if paused.is_err() {
                status(&directory, "Could not pause sharing");
            }
            settle(&directory);
        }
        Event::ServiceFailed { action, error } if action == "sharing" => {
            // After reboot or process death: Android refuses background location start.
            post_paused();
            status(&directory, &error);
        }
        Event::ServiceFailed { error, .. } => {
            status(&directory, &error);
            let _ = request(directory, json!({"op": "interrupt"}));
        }
        Event::ServiceStopped => {
            SHARE_GPS.lock().expect("sharing lock").take();
            sharing::stop_node();
            let recording = GPS.lock().expect("recorder lock").take().is_some();
            if recording {
                let _ = request(directory, json!({"op": "interrupt"}));
            }
        }
        Event::Boot if sharing::mode(&directory) != Mode::Off => {
            let _ = background::start_service("sharing", "", &sharing_notification());
        }
        // Watchdog: Android does not restart a killed service; alarm survives the process.
        Event::Alarm { id } if id == WATCHDOG => {
            if sharing::mode(&directory) == Mode::Off {
                return;
            }
            if sharing::node_running() {
                schedule_watchdog();
            } else if background::start_service("sharing", "", &sharing_notification()).is_err() {
                post_paused();
            }
        }
        _ => {}
    }
}

fn refresh_gps(directory: &Path) {
    if sharing::mode(directory) != Mode::Off && GPS.lock().expect("recorder lock").is_none() {
        FRESH_UNTIL.store(
            crate::sharing::manager::now_ms() + 20_000,
            Ordering::Relaxed,
        );
        share_gps(directory.to_owned(), Duration::from_secs(1));
        let _ =
            background::schedule_alarm(LOCATE_DONE, SystemTime::now() + Duration::from_secs(20));
    }
}

const WATCHDOG: i32 = 7;
const LOCATE_DONE: i32 = 8;

pub(crate) fn sharing_timer_expired() {
    let _ = background::send_service("sharing", "");
}

fn schedule_watchdog() {
    let at = SystemTime::now() + Duration::from_secs(15 * 60);
    let _ = background::schedule_alarm(WATCHDOG, at);
}

fn post_paused() {
    let _ = notification::post(&notification::Notification {
        id: 2,
        title: "Roamlog".into(),
        text: "Location sharing paused. Open Roamlog to resume.".into(),
        ..Default::default()
    });
}

/// Matches service to recording and sharing mode: runs node, sharing GPS and notification,
/// or stops the service when neither trip nor sharing needs it.
fn settle(directory: &Path) {
    let recording = GPS.lock().expect("recorder lock").is_some();
    sharing::set_recording(recording);
    let running = match sharing::sync_node(directory) {
        Ok(running) => running,
        Err(error) => {
            status(directory, &format!("Location sharing unavailable: {error}"));
            false
        }
    };
    if running {
        schedule_watchdog();
    } else {
        let _ = background::cancel_alarm(WATCHDOG);
    }
    if recording {
        // Trip GPS feeds sharing; separate watch would duplicate GPS work.
        SHARE_GPS.lock().expect("sharing lock").take();
        return;
    }
    if !running {
        SHARE_GPS.lock().expect("sharing lock").take();
        let _ = background::stop_service();
        return;
    }
    let _ = notification::post(&sharing_notification());
    if sharing::mode(directory) == Mode::Always {
        if SHARE_GPS.lock().expect("sharing lock").is_none() {
            share_gps(directory.to_owned(), Duration::from_secs(30));
        }
    } else {
        SHARE_GPS.lock().expect("sharing lock").take();
    }
}

/// Adaptive interval: slow when stationary, user's update interval while moving.
fn share_interval(directory: &Path, speed: Option<f32>) -> Duration {
    let moving = Duration::from_millis(
        sharing::policy(directory)
            .minimum_interval_ms
            .clamp(1000, 600_000) as u64,
    );
    match speed {
        Some(speed) if speed >= 5.0 => moving,
        Some(speed) if speed >= 0.5 => moving.max(Duration::from_secs(30)),
        _ => Duration::from_secs(300),
    }
}

fn share_gps(directory: PathBuf, interval: Duration) {
    {
        let mut current = SHARE_GPS.lock().expect("sharing lock");
        if current.as_ref().is_some_and(|(_, i)| *i == interval) {
            return;
        }
        current.take();
    }
    let watched = directory.clone();
    let request = location::Request {
        source: location::Source::Any,
        interval,
        last_known: false,
    };
    // Watch created outside lock: its callback takes the same lock.
    match watch(&directory, request, move |update| {
        share_fix(&watched, update)
    }) {
        Ok(gps) => *SHARE_GPS.lock().expect("sharing lock") = Some((gps, interval)),
        Err(error) => status(&directory, &format!("Sharing GPS unavailable: {error}")),
    }
}

fn share_fix(directory: &Path, update: location::Update) {
    let fix = match update {
        location::Update::Fix(fix) => fix,
        location::Update::Disabled(_)
            if !location::enabled(location::Source::Gps)
                && !location::enabled(location::Source::Network) =>
        {
            return sharing::clear_fix();
        }
        _ => return,
    };
    let Some(accuracy) = fix.accuracy_m else {
        return;
    };
    if fix.age > Duration::from_secs(120) {
        return;
    }
    sharing::fix(
        crate::model::Fix {
            point: crate::geo::Point {
                lat: fix.lat,
                lon: fix.lon,
            },
            timestamp_ms: fix.time_ms,
            accuracy_m: f64::from(accuracy),
        },
        fix.speed.map(f64::from),
        fix.bearing.map(f64::from),
        false,
    );
    let interval = if crate::sharing::manager::now_ms() < FRESH_UNTIL.load(Ordering::Relaxed) {
        Duration::from_secs(1)
    } else {
        share_interval(directory, fix.speed)
    };
    let changed = SHARE_GPS
        .lock()
        .expect("sharing lock")
        .as_ref()
        .is_some_and(|(_, current)| *current != interval);
    if changed {
        // Replace watch outside its own callback.
        let directory = directory.to_owned();
        std::thread::spawn(move || {
            if SHARE_GPS.lock().expect("sharing lock").is_some() {
                share_gps(directory, interval);
            }
        });
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
        settle(&directory);
        return;
    }
    let _ = notification::post(&trip_notification(
        &directory,
        "Recording · waiting for GPS",
    ));
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
        Ok(gps) => {
            *GPS.lock().expect("recorder lock") = Some(gps);
            settle(&directory);
        }
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
    sharing::fix(
        crate::model::Fix {
            point: crate::geo::Point {
                lat: fix.lat,
                lon: fix.lon,
            },
            timestamp_ms: fix.time_ms,
            accuracy_m: f64::from(accuracy),
        },
        fix.speed.map(f64::from),
        fix.bearing.map(f64::from),
        true,
    );
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
            let _ = notification::post(&trip_notification(
                directory,
                &format!("{distance}{gps}{estimated}"),
            ));
        }
        Err(error) => fail(directory, &error),
    }
}

/// Ends recording after an error; service stops unless sharing still needs it.
fn fail(directory: &Path, error: &str) {
    status(directory, error);
    GPS.lock().expect("recorder lock").take();
    let _ = request(directory.to_owned(), json!({"op": "interrupt"}));
    settle(directory);
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
