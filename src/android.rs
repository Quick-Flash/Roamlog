//! Android integration through zgui: the activity entry point, platform requests from the UI,
//! and the trip recorder, which runs in the `:recorder` service process so trips continue after
//! the activity closes.
use crate::backend::request;
use crate::mobile::{self, Reading, now_ms, status};
use crate::sharing::{
    app as sharing,
    checkin::{self, Phase},
    manager::Mode,
};
use serde_json::json;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::time::{Duration, SystemTime};
use zgui::app::android::{background, document, location, notification, permission, power, system};

#[unsafe(no_mangle)]
pub fn android_main(app: zgui::app::AndroidApp) {
    static ENTERED: AtomicBool = AtomicBool::new(false);
    if ENTERED.swap(true, Ordering::SeqCst) {
        std::process::exit(0);
    }
    sharing::install_vm(app.vm_as_ptr());
    crate::android_updates::init(&app);
    let directory = app.internal_data_path().expect("Android internal storage");
    mobile::default_units(&directory, app.config().country().as_deref());
    crate::ui::run_android(app, directory);
}

zgui::app::android_background!(recorder);

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

fn watch(
    directory: &Path,
    request: location::Request,
    callback: impl Fn(location::Update) + Send + 'static,
) -> Result<Watch, String> {
    let Some(route) = crate::demo::Route::load(directory) else {
        return Ok(Box::new(location::watch(request, callback)?));
    };
    Ok(Box::new(mobile::simulate(route, move |r| {
        callback(location::Update::Fix(location::Location {
            lat: r.lat,
            lon: r.lon,
            accuracy_m: r.accuracy_m,
            altitude_m: None,
            speed: r.speed,
            bearing: r.bearing,
            time_ms: r.time_ms,
            age: r.age,
            provider: "gps".into(),
        }))
    })))
}

fn reading(fix: &location::Location) -> Reading {
    Reading {
        lat: fix.lat,
        lon: fix.lon,
        accuracy_m: fix.accuracy_m,
        speed: fix.speed,
        bearing: fix.bearing,
        time_ms: fix.time_ms,
        age: fix.age,
    }
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
        let Some(command) = reading(&fix).position_command() else {
            return;
        };
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
            let imported = mobile::import_basemap(&directory, doc.file);
            status(
                &directory,
                &imported.map_or_else(
                    |e| format!("Offline map import failed: {e}"),
                    |()| "Offline map imported.".into(),
                ),
            );
        });
    })
}

/// Writes trips as GPX to a document the user creates.
pub(crate) fn export_gpx(directory: PathBuf) -> Result<(), String> {
    document::create("roamlog-trips.gpx", "application/gpx+xml", move |picked| {
        let doc = match picked {
            Ok(Some(doc)) => doc,
            Ok(None) => return,
            Err(error) => return status(&directory, &format!("Export failed: {error}")),
        };
        std::thread::spawn(move || {
            let written = mobile::export_gpx(&directory, doc.file);
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
                let written = mobile::backup(&directory, &mut doc.file);
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
            status(
                &directory,
                &mobile::restore(&directory, doc.file).map_or_else(
                    |e| format!("Restore failed: {e}"),
                    |trips| format!("Restored {trips} trips."),
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
                    // UI asks: app is open.
                    background::start_service("sharing", "visible", &sharing_notification())
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
        let _ = background::send_service("sharing", "visible");
    }
}

/// Camera apps open scanned invitation links with Roamlog through the manifest filter.
pub(crate) fn listen_invites() {
    zgui::app::android::intent::on_intent(|intent| {
        if let Some(link) = intent
            .data
            .or(intent.text)
            .filter(|l| l.starts_with("roamlog://pair/"))
        {
            *sharing::PENDING_INVITE.lock().expect("invite lock") = Some(link);
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
            match mobile::read_image(doc.file)
                .and_then(|bytes| sharing::decode_qr_image(&bytes).map_err(|e| e.to_string()))
            {
                Ok(link) => *sharing::PENDING_INVITE.lock().expect("invite lock") = Some(link),
                Err(_) => status(&directory, "No Roamlog invitation found in image"),
            }
        });
    })
}

/// Opens image picker; picked photo lands in `PICKED_AVATAR`.
pub(crate) fn pick_avatar() -> Result<(), String> {
    document::open("image/*", move |picked| {
        let done = |result| *sharing::PICKED_AVATAR.lock().expect("avatar lock") = Some(result);
        let doc = match picked {
            Ok(Some(doc)) => doc,
            Ok(None) => return,
            Err(error) => return done(Err(format!("Could not open image: {error}"))),
        };
        std::thread::spawn(move || {
            done(mobile::read_image(doc.file).and_then(|bytes| mobile::picked_image(&bytes)));
        });
    })
}

/// Battery percentage for friends who see it; `None` when unknown.
pub(crate) fn battery_percent() -> Option<u8> {
    sharing::battery_percent().ok().flatten()
}

/// One-off notification, e.g. a friend entering an alert area.
pub(crate) fn notify(id: i32, title: &str, text: &str) -> Result<(), String> {
    notification::post(&notification::Notification {
        id,
        title: title.into(),
        text: text.into(),
        ..Default::default()
    })
    .map(drop)
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

/// Open app keeps sharing connected for quick updates; closed, it only checks in.
/// Repeated while open: a UI that dies silently stops counting after `VISIBLE_MS`.
pub(crate) fn app_visible(directory: &Path, visible: bool) {
    if sharing::mode(directory) != Mode::Off {
        let _ = background::send_service(if visible { "visible" } else { "hidden" }, "");
    }
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

/// UI counts as open this long after its last signal.
const VISIBLE_MS: i64 = 10 * 60_000;
static VISIBLE_UNTIL: AtomicI64 = AtomicI64::new(0);
/// Pairing holds the identity; node stays off until pairing ends or this passes.
static SUSPENDED_UNTIL: AtomicI64 = AtomicI64::new(0);
/// Check-in whose GPS fix was already taken.
static SLOT_FIX: AtomicI64 = AtomicI64::new(0);
/// Keeps CPU running through GPS lead and online window while the screen is off.
static WAKE: Mutex<Option<zgui::app::android::Guard>> = Mutex::new(None);
/// Service alive in this process; watchdog restarts it otherwise.
static RUNNING: AtomicBool = AtomicBool::new(false);

/// Check-in phase (see `sharing::checkin`) and when it ends on this phone's own clock.
fn phase() -> (Phase, i64) {
    let live_until = VISIBLE_UNTIL
        .load(Ordering::Relaxed)
        .max(FRESH_UNTIL.load(Ordering::Relaxed));
    checkin::schedule(crate::sharing::manager::now_ms(), live_until)
}

fn at_ms(ms: i64) -> SystemTime {
    SystemTime::UNIX_EPOCH + Duration::from_millis(ms.max(0) as u64)
}

fn recorder(event: background::Event) {
    use background::Event;
    let Ok((directory, _)) = system::dirs() else {
        return;
    };
    match event {
        Event::Service { action, payload } if action == "sharing" => {
            SUSPENDED_UNTIL.store(0, Ordering::Relaxed);
            if payload == "visible" {
                VISIBLE_UNTIL.store(now_ms() as i64 + VISIBLE_MS, Ordering::Relaxed);
            }
            settle(&directory);
        }
        Event::Service { action, .. } if action == "visible" => {
            VISIBLE_UNTIL.store(now_ms() as i64 + VISIBLE_MS, Ordering::Relaxed);
            settle(&directory);
        }
        Event::Service { action, .. } if action == "hidden" => {
            VISIBLE_UNTIL.store(0, Ordering::Relaxed);
            settle(&directory);
        }
        Event::Alarm { id } if id == CHECKIN => settle(&directory),
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
            SUSPENDED_UNTIL.store(now_ms() as i64 + VISIBLE_MS, Ordering::Relaxed);
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
            RUNNING.store(false, Ordering::Relaxed);
            WAKE.lock().expect("wake lock").take();
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
            if RUNNING.load(Ordering::Relaxed) {
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
const CHECKIN: i32 = 9;

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

/// Matches service to recording, sharing mode and check-in phase: runs node, sharing GPS and
/// notification, or stops the service when neither trip nor sharing needs it.
fn settle(directory: &Path) {
    let recording = GPS.lock().expect("recorder lock").is_some();
    sharing::set_recording(recording);
    let mode = sharing::mode(directory);
    let now = crate::sharing::manager::now_ms();
    let (phase, end) = phase();
    let online =
        mode != Mode::Off && phase.online() && now >= SUSPENDED_UNTIL.load(Ordering::Relaxed);
    if online {
        if let Err(error) = sharing::sync_node(directory) {
            status(directory, &format!("Location sharing unavailable: {error}"));
        }
    } else if now >= SUSPENDED_UNTIL.load(Ordering::Relaxed) {
        sharing::stop_node();
    }
    {
        let mut wake = WAKE.lock().expect("wake lock");
        match phase {
            Phase::Prep(_) | Phase::Window(_) if mode != Mode::Off => {
                if wake.is_none() {
                    // Through window end; next settle releases it.
                    let window_end = match phase {
                        Phase::Prep(_) => end + checkin::WINDOW_MS,
                        _ => end,
                    };
                    let left = (window_end - now).max(0) as u64 + 2_000;
                    *wake = power::wake_lock(Duration::from_millis(left)).ok();
                }
            }
            _ => *wake = None,
        }
    }
    if mode == Mode::Off {
        let _ = background::cancel_alarm(CHECKIN);
        let _ = background::cancel_alarm(WATCHDOG);
    } else {
        let _ = background::schedule_alarm(CHECKIN, at_ms(end));
        schedule_watchdog();
    }
    if recording {
        // Trip GPS feeds sharing; separate watch would duplicate GPS work.
        RUNNING.store(true, Ordering::Relaxed);
        SHARE_GPS.lock().expect("sharing lock").take();
        return;
    }
    if mode == Mode::Off {
        RUNNING.store(false, Ordering::Relaxed);
        SHARE_GPS.lock().expect("sharing lock").take();
        let _ = background::stop_service();
        return;
    }
    RUNNING.store(true, Ordering::Relaxed);
    let _ = notification::post(&sharing_notification());
    let gps = mode == Mode::Always
        && match phase {
            Phase::Live => true,
            // One fix per check-in while the app is closed.
            Phase::Prep(_) | Phase::Window(_) => {
                phase.slot() != Some(SLOT_FIX.load(Ordering::Relaxed))
            }
            Phase::Idle(_) => false,
        };
    if !gps {
        SHARE_GPS.lock().expect("sharing lock").take();
    } else if SHARE_GPS.lock().expect("sharing lock").is_none() {
        let interval = if phase == Phase::Live { 30 } else { 1 };
        share_gps(directory.to_owned(), Duration::from_secs(interval));
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
    let reading = reading(&fix);
    let Some(shared) = reading.sharing_fix() else {
        return;
    };
    sharing::fix(
        shared,
        reading.speed.map(f64::from),
        reading.bearing.map(f64::from),
        false,
    );
    if let Some(at) = phase().0.slot() {
        // App closed: one good fix per check-in, then GPS off until the next.
        if shared.accuracy_m <= 50.0 {
            SLOT_FIX.store(at, Ordering::Relaxed);
            std::thread::spawn(|| SHARE_GPS.lock().expect("sharing lock").take());
        }
        return;
    }
    let interval = if crate::sharing::manager::now_ms() < FRESH_UNTIL.load(Ordering::Relaxed) {
        Duration::from_secs(1)
    } else {
        mobile::share_interval(
            crate::sharing::location::UpdatePolicy::default().minimum_interval_ms,
            reading.speed,
        )
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
    let event = mobile::command_event(op, payload, now_ms());
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
    let reading = reading(&fix);
    let Some(recorded) = reading.recording_fix() else {
        return;
    };
    if recorded.accuracy_m <= 30.0 && GPS_WAITING.swap(false, Ordering::Relaxed) {
        status(directory, "");
    }
    sharing::fix(
        recorded,
        reading.speed.map(f64::from),
        reading.bearing.map(f64::from),
        true,
    );
    match mobile::record_fix(directory, &recorded) {
        Ok(text) => {
            let _ = notification::post(&trip_notification(directory, &text));
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
