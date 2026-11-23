//! iOS integration. Recording runs in the app process: background location updates keep the app
//! alive while a trip records. If iOS ends the app mid-trip, the journal pauses the trip at its
//! last fix on relaunch (recorder lock released with the process), as on Android.
//! Journal work runs on one worker thread, in order, never on the main (UI) thread.
//! Location sharing runs its node in this process too. iOS suspends a backgrounded app unless
//! location updates keep it running: while sharing is on, a near-free cell/Wi-Fi location
//! session keeps it alive, and timers run the same check-ins as Android (`sharing::checkin`):
//! one precise fix and a short online window every 5 minutes, connected throughout while open.
mod files;
mod location;
mod notify;

use crate::backend::request;
use crate::mobile::{self, now_ms, status};
use crate::sharing::{
    app as sharing,
    checkin::{self, Phase},
    manager::Mode,
};
use location::{Update, Watch};
use objc2::MainThreadMarker;
use serde_json::json;
use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::mpsc::{Sender, channel};

thread_local! {
    /// GPS watch while recording.
    static GPS: RefCell<Option<Watch>> = const { RefCell::new(None) };
    /// Foreground watch for the map's own-position marker.
    static FOLLOW: RefCell<Option<Watch>> = const { RefCell::new(None) };
    /// Watch while sharing Always without a trip and the app is open.
    static SHARE_GPS: RefCell<Option<Watch>> = const { RefCell::new(None) };
    /// Near-free location session keeping the closed app alive for check-ins.
    static KEEP_ALIVE: RefCell<Option<Watch>> = const { RefCell::new(None) };
    /// Precise watch for the one fix per check-in while closed.
    static CHECKIN_FIX: RefCell<Option<Watch>> = const { RefCell::new(None) };
    /// Short precise watch after a friend asked for a fresh position.
    static LOCATE: RefCell<Option<Watch>> = const { RefCell::new(None) };
}

/// App counts as open until this time (own clock); refreshed while open.
static VISIBLE_UNTIL: AtomicI64 = AtomicI64::new(0);
const VISIBLE_MS: i64 = 10 * 60_000;
/// Answering a friend's locate request: stay connected until this time.
static FRESH_UNTIL: AtomicI64 = AtomicI64::new(0);
/// Pairing holds the identity; node stays off until pairing ends or this passes.
static SUSPENDED_UNTIL: AtomicI64 = AtomicI64::new(0);
/// Check-in whose fix was already taken.
static SLOT_FIX: AtomicI64 = AtomicI64::new(0);
/// Only the newest check-in timer acts.
static TIMER: AtomicU64 = AtomicU64::new(0);

fn own_ms() -> i64 {
    crate::sharing::manager::now_ms()
}

/// Last battery level read on the main thread (UIDevice is main-thread only); 255 = unknown.
static BATTERY: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(u8::MAX);

fn read_battery(mtm: MainThreadMarker) {
    let device = objc2_ui_kit::UIDevice::currentDevice(mtm);
    device.setBatteryMonitoringEnabled(true);
    let level = device.batteryLevel();
    let percent = if level < 0.0 {
        u8::MAX
    } else {
        (level * 100.0).round() as u8
    };
    BATTERY.store(percent, std::sync::atomic::Ordering::Relaxed);
}

/// Battery percentage for friends who see it; `None` when unknown. Any thread.
pub(crate) fn battery_percent() -> Option<u8> {
    Some(BATTERY.load(std::sync::atomic::Ordering::Relaxed)).filter(|p| *p <= 100)
}

/// A friend asked for a fresh position: precise fixes for 20 s, unless a trip already records.
pub(crate) fn locate() {
    on_main(|mtm| {
        if GPS.with_borrow(Option::is_some) || LOCATE.with_borrow(Option::is_some) {
            return;
        }
        let watch = location::watch(mtm, true, true, |update| {
            let Update::Fix(reading) = update else { return };
            let Some(fix) = reading.sharing_fix() else {
                return;
            };
            let (speed, bearing) = (reading.speed, reading.bearing);
            serial(move || {
                sharing::fix(fix, speed.map(f64::from), bearing.map(f64::from), false);
            });
        });
        LOCATE.with_borrow_mut(|locate| *locate = Some(watch));
        FRESH_UNTIL.store(own_ms() + 20_000, Ordering::Relaxed);
        std::thread::spawn(|| {
            std::thread::sleep(std::time::Duration::from_secs(20));
            on_main(|mtm| {
                LOCATE.with_borrow_mut(|locate| drop(locate.take()));
                if let Some(directory) = DIRECTORY.get() {
                    settle(mtm, directory);
                }
            });
        });
    });
}

/// App data directory, for sharing work started by the node rather than the UI.
static DIRECTORY: OnceLock<PathBuf> = OnceLock::new();

type Job = Box<dyn FnOnce() + Send>;

/// Runs `job` on the journal worker, after every job queued before it.
fn serial(job: impl FnOnce() + Send + 'static) {
    static QUEUE: OnceLock<Sender<Job>> = OnceLock::new();
    let queue = QUEUE.get_or_init(|| {
        let (send, receive) = channel::<Job>();
        std::thread::spawn(move || receive.into_iter().for_each(|job| job()));
        send
    });
    let _ = queue.send(Box::new(job));
}

/// Runs `job` on the main thread, for UIKit work after a worker finished.
fn on_main(job: impl FnOnce(MainThreadMarker) + Send + 'static) {
    dispatch2::DispatchQueue::main().exec_async(move || {
        job(MainThreadMarker::new().expect("main queue runs on main thread"));
    });
}

fn main_thread() -> Result<MainThreadMarker, String> {
    MainThreadMarker::new().ok_or_else(|| "iOS call made off the main thread".into())
}

fn report(directory: &Path, result: Result<(), String>) {
    if let Err(error) = result {
        status(directory, &error);
    }
}

/// Starts, pauses, resumes or finishes recording.
pub(crate) fn record(directory: PathBuf, op: &str, category_id: u64) -> Result<(), String> {
    let mtm = main_thread()?;
    // Any command replaces the previous GPS watch.
    GPS.with_borrow_mut(|gps| gps.take());
    let payload = json!({"category_id": category_id}).to_string();
    let event = mobile::command_event(op, &payload, now_ms());
    let starting = matches!(op, "start" | "resume");
    if starting && location::denied() {
        return Err("Location permission needed to record trip. Allow it in Settings.".into());
    }
    let committed = directory.clone();
    serial(move || report(&committed, request(committed.clone(), event).map(drop)));
    if !starting {
        settle(mtm, &directory);
        return Ok(());
    }
    // Fixes queue behind the start event on the same worker.
    let watched = directory.clone();
    let watch = location::watch(mtm, true, true, move |update| match update {
        Update::Fix(reading) => {
            let Some(fix) = reading.recording_fix() else {
                return;
            };
            let (speed, bearing) = (reading.speed, reading.bearing);
            let directory = watched.clone();
            serial(move || {
                sharing::fix(fix, speed.map(f64::from), bearing.map(f64::from), true);
                report(&directory, mobile::record_fix(&directory, &fix).map(drop));
            });
        }
        Update::Failed(error) => {
            let directory = watched.clone();
            serial(move || {
                let paused = request(
                    directory.clone(),
                    json!({"op": "pause", "timestamp_ms": now_ms()}),
                );
                status(
                    &directory,
                    &paused.map_or_else(|e| format!("{error}: {e}"), |_| error.clone()),
                );
            });
        }
    });
    GPS.with_borrow_mut(|gps| *gps = Some(watch));
    settle(mtm, &directory);
    Ok(())
}

/// Starts following position once location permission exists. `ask` requests it first.
pub(crate) fn follow(directory: PathBuf, ask: bool) -> Result<(), String> {
    let mtm = main_thread()?;
    if FOLLOW.with_borrow(Option::is_some) || !(ask || location::authorized()) {
        return Ok(());
    }
    let watch = location::watch(mtm, false, true, move |update| {
        if let Update::Fix(reading) = update
            && let Some(command) = reading.position_command()
        {
            let directory = directory.clone();
            serial(move || drop(request(directory, command)));
        }
    });
    FOLLOW.with_borrow_mut(|follow| *follow = Some(watch));
    Ok(())
}

/// Copies a picked PMTiles file into private storage; Rust validates it on reopen.
pub(crate) fn import_basemap(directory: PathBuf) -> Result<(), String> {
    files::open(main_thread()?, move |picked| {
        let Some(path) = picked else { return };
        status(&directory, "Importing offline map…");
        std::thread::spawn(move || {
            let imported = std::fs::File::open(&path)
                .map_err(|e| e.to_string())
                .and_then(|file| mobile::import_basemap(&directory, file));
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

/// Replaces journal with a picked backup after Rust validates every line.
pub(crate) fn restore(directory: PathBuf) -> Result<(), String> {
    files::open(main_thread()?, move |picked| {
        let Some(path) = picked else { return };
        serial(move || {
            let restored = std::fs::File::open(&path)
                .map_err(|e| e.to_string())
                .and_then(|file| mobile::restore(&directory, file));
            status(
                &directory,
                &restored.map_or_else(
                    |e| format!("Restore failed: {e}"),
                    |trips| format!("Restored {trips} trips."),
                ),
            );
        });
    })
}

/// Writes `name` with `write` on the worker, then offers it to save where the user chooses.
fn export(
    directory: PathBuf,
    name: &'static str,
    what: &'static str,
    write: impl FnOnce(&Path, &mut std::fs::File) -> Result<(), String> + Send + 'static,
) -> Result<(), String> {
    main_thread()?;
    serial(move || {
        let file = std::env::temp_dir().join(name);
        let written = std::fs::File::create(&file)
            .map_err(|e| e.to_string())
            .and_then(|mut out| write(&directory, &mut out));
        if let Err(error) = written {
            return status(&directory, &format!("{what} failed: {error}"));
        }
        on_main(move |mtm| {
            let saved_directory = directory.clone();
            let shown = files::export(mtm, &file, move |saved| {
                if saved {
                    status(&saved_directory, &format!("{what} saved."));
                }
            });
            report(&directory, shown);
        });
    });
    Ok(())
}

pub(crate) fn export_gpx(directory: PathBuf) -> Result<(), String> {
    export(
        directory,
        "roamlog-trips.gpx",
        "GPX export",
        |directory, out| mobile::export_gpx(directory, out),
    )
}

pub(crate) fn backup(directory: PathBuf) -> Result<(), String> {
    export(directory, "roamlog-backup.jsonl", "Backup", mobile::backup)
}

pub(crate) fn open_settings() -> Result<(), String> {
    files::open_settings(main_thread()?)
}

// --- Location sharing -----------------------------------------------------------------------

/// Hands a sharing fix to the node (or keeps it until the node starts).
fn share(reading: &mobile::Reading) -> bool {
    let Some(fix) = reading.sharing_fix() else {
        return false;
    };
    let (speed, bearing) = (reading.speed, reading.bearing);
    serial(move || {
        sharing::fix(fix, speed.map(f64::from), bearing.map(f64::from), false);
    });
    fix.accuracy_m <= 50.0
}

/// Matches node, recording flag, location sessions and check-in timer to sharing mode and
/// check-in phase.
fn settle(mtm: MainThreadMarker, directory: &Path) {
    read_battery(mtm);
    let recording = GPS.with_borrow(Option::is_some);
    let mode = sharing::mode(directory);
    let own = own_ms();
    let live_until = VISIBLE_UNTIL
        .load(Ordering::Relaxed)
        .max(FRESH_UNTIL.load(Ordering::Relaxed));
    let (phase, end) = checkin::schedule(own, live_until);
    let always = mode == Mode::Always && !recording;
    // Recording watch keeps the app alive and feeds sharing during trips.
    let alive = mode != Mode::Off && !recording;
    if alive && KEEP_ALIVE.with_borrow(Option::is_none) {
        let watch = location::keep_alive(mtm);
        KEEP_ALIVE.with_borrow_mut(|keep| *keep = Some(watch));
    } else if !alive {
        KEEP_ALIVE.with_borrow_mut(|keep| keep.take());
    }
    if always && phase == Phase::Live {
        if SHARE_GPS.with_borrow(Option::is_none) {
            let watch = location::watch(mtm, true, false, move |update| {
                read_battery(mtm);
                if let Update::Fix(reading) = update {
                    share(&reading);
                }
            });
            SHARE_GPS.with_borrow_mut(|gps| *gps = Some(watch));
        }
    } else {
        SHARE_GPS.with_borrow_mut(|gps| gps.take());
    }
    // Closed: one good fix per check-in, taken just before it, then GPS off.
    let slot = phase
        .slot()
        .filter(|at| always && SLOT_FIX.load(Ordering::Relaxed) != *at);
    match slot {
        Some(at) if CHECKIN_FIX.with_borrow(Option::is_none) => {
            let watch = location::watch(mtm, true, true, move |update| {
                let Update::Fix(reading) = update else { return };
                if share(&reading) {
                    SLOT_FIX.store(at, Ordering::Relaxed);
                    on_main(|_| CHECKIN_FIX.with_borrow_mut(|fix| drop(fix.take())));
                }
            });
            CHECKIN_FIX.with_borrow_mut(|fix| *fix = Some(watch));
        }
        Some(_) => {}
        None => CHECKIN_FIX.with_borrow_mut(|fix| drop(fix.take())),
    }
    let online =
        mode != Mode::Off && phase.online() && own >= SUSPENDED_UNTIL.load(Ordering::Relaxed);
    let suspended = own < SUSPENDED_UNTIL.load(Ordering::Relaxed);
    let directory = directory.to_owned();
    serial(move || {
        sharing::set_recording(recording);
        if online {
            if let Err(error) = sharing::sync_node(&directory) {
                status(
                    &directory,
                    &format!("Location sharing unavailable: {error}"),
                );
            }
        } else if !suspended {
            sharing::stop_node();
        }
    });
    if mode != Mode::Off {
        wake_at(end);
    }
}

/// Runs `settle` again at own time `at_ms`; replaces any earlier timer. Runs while the
/// keep-alive session keeps the app awake; if iOS suspended it anyway, settle runs on resume.
fn wake_at(at_ms: i64) {
    let id = TIMER.fetch_add(1, Ordering::Relaxed) + 1;
    std::thread::spawn(move || {
        let wait = (at_ms - own_ms()).max(0) as u64;
        std::thread::sleep(std::time::Duration::from_millis(wait));
        if TIMER.load(Ordering::Relaxed) != id {
            return;
        }
        on_main(|mtm| {
            if let Some(directory) = DIRECTORY.get() {
                settle(mtm, directory);
            }
        });
    });
}

/// Starts or stops sharing after the UI changed mode, friends or permissions.
pub(crate) fn sharing_changed(directory: PathBuf) -> Result<(), String> {
    let mtm = main_thread()?;
    let _ = DIRECTORY.set(directory.clone());
    SUSPENDED_UNTIL.store(0, Ordering::Relaxed);
    VISIBLE_UNTIL.store(own_ms() + VISIBLE_MS, Ordering::Relaxed);
    if sharing::mode(&directory) != Mode::Off {
        if location::denied() {
            return Err(
                "Location permission needed to share location. Allow it in Settings.".into(),
            );
        }
        notify::request_permission(mtm);
    }
    settle(mtm, &directory);
    Ok(())
}

/// App launch: restarts sharing that was on.
pub(crate) fn resume_sharing(directory: PathBuf) {
    let _ = DIRECTORY.set(directory.clone());
    if sharing::mode(&directory) != Mode::Off
        && let Err(error) = sharing_changed(directory.clone())
    {
        status(&directory, &error);
    }
}

/// Wakes the running node after friends or permissions changed.
pub(crate) fn nudge_sharing(directory: &Path) {
    if sharing::mode(directory) != Mode::Off
        && let Ok(mtm) = main_thread()
    {
        settle(mtm, directory);
    }
}

/// Open app keeps sharing connected for quick updates; closed, it only checks in.
pub(crate) fn app_visible(directory: &Path, visible: bool) {
    let until = if visible { own_ms() + VISIBLE_MS } else { 0 };
    VISIBLE_UNTIL.store(until, Ordering::Relaxed);
    if sharing::mode(directory) != Mode::Off
        && let Ok(mtm) = main_thread()
    {
        settle(mtm, directory);
    }
}

/// Pairing runs its own endpoint with the same identity; close the node's meanwhile.
pub(crate) fn suspend_sharing(_directory: &Path) {
    SUSPENDED_UNTIL.store(own_ms() + VISIBLE_MS, Ordering::Relaxed);
    serial(sharing::stop_node);
}

/// Called from the node when a timed sharing mode ran out.
pub(crate) fn sharing_timer_expired() {
    on_main(|mtm| {
        if let Some(directory) = DIRECTORY.get() {
            settle(mtm, directory);
        }
    });
}

/// New sharing identity. Runs off the main thread (UI calls it through `blocking`).
pub(crate) fn reset_identity(directory: &Path) -> Result<(), String> {
    sharing::stop_node();
    sharing::reset_identity(directory).map_err(|e| e.to_string())?;
    let directory = directory.to_owned();
    on_main(move |mtm| settle(mtm, &directory));
    Ok(())
}

/// Delivers `roamlog://pair/` links opened from other apps (camera, messages).
pub(crate) fn listen_invites() {
    if let Ok(mtm) = main_thread() {
        notify::listen_for_links(mtm);
    }
}

/// Decodes an invitation QR code from a picked photo or screenshot.
pub(crate) fn scan_invite(directory: PathBuf) -> Result<(), String> {
    files::open(main_thread()?, move |picked| {
        let Some(path) = picked else { return };
        std::thread::spawn(move || {
            let link = std::fs::File::open(&path)
                .map_err(|e| e.to_string())
                .and_then(mobile::read_image)
                .and_then(|bytes| sharing::decode_qr_image(&bytes).map_err(|e| e.to_string()));
            match link {
                Ok(link) => *sharing::PENDING_INVITE.lock().expect("invite lock") = Some(link),
                Err(_) => status(&directory, "No Roamlog invitation found in image"),
            }
        });
    })
}

/// Opens a picker for the profile picture; the photo lands in `PICKED_AVATAR`.
pub(crate) fn pick_avatar() -> Result<(), String> {
    files::open(main_thread()?, move |picked| {
        let Some(path) = picked else { return };
        std::thread::spawn(move || {
            let picked = std::fs::File::open(&path)
                .map_err(|e| e.to_string())
                .and_then(mobile::read_image)
                .and_then(|bytes| mobile::picked_image(&bytes));
            *sharing::PICKED_AVATAR.lock().expect("avatar lock") = Some(picked);
        });
    })
}

pub(crate) fn share_text(text: &str) {
    if let Ok(mtm) = main_thread() {
        let _ = files::share(mtm, text);
    }
}

/// One-off notification, e.g. a friend entering an alert area. Any thread.
pub(crate) fn notify(id: i32, title: &str, text: &str) -> Result<(), String> {
    notify::post(id, title, text);
    Ok(())
}
