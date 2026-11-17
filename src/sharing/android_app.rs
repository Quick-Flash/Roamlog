//! Roamlog app glue. Recorder service process runs the sharing node; UI process manages
//! friends and runs pairing. Both load the same Keystore identity.
use super::{
    Result, identity,
    location::{Sample, UpdatePolicy},
    manager::{Manager, Mode, now_ms},
    node::Node,
    peers::Friends,
};
use iroh::SecretKey;
use jni::{
    JavaVM, jni_sig, jni_str,
    objects::{Global, JObject, JString, JValue},
};
use std::{
    path::Path,
    sync::{Mutex, OnceLock},
};

static KEY: Mutex<Option<SecretKey>> = Mutex::new(None);
static MANAGER: Mutex<Option<Manager>> = Mutex::new(None);
static NODE: Mutex<Option<Node>> = Mutex::new(None);

/// Call once with the VM from `android_main`; background events set it through zgui.
pub fn install_vm(vm: *mut std::ffi::c_void) {
    // SAFETY: Android passes a valid process-lifetime VM pointer.
    unsafe { JavaVM::from_raw(vm.cast()) };
}

/// Runs `f` with the Application context on an attached thread.
fn with_application<T>(
    f: impl FnOnce(&mut jni::Env<'_>, &JObject<'_>) -> jni::errors::Result<T>,
) -> Result<T> {
    let vm = JavaVM::singleton()?;
    Ok(vm.attach_current_thread(|env| -> jni::errors::Result<T> {
        let application = env
            .call_static_method(
                jni_str!("android/app/ActivityThread"),
                jni_str!("currentApplication"),
                jni_sig!("()Landroid/app/Application;"),
                &[],
            )?
            .l()?;
        f(env, &application)
    })?)
}

/// Keystore-backed identity, decrypted once per process. Blocks; never call on UI thread.
pub fn key() -> Result<SecretKey> {
    let mut cached = KEY.lock().map_err(|_| "sharing lock failed")?;
    if let Some(key) = cached.as_ref() {
        return Ok(key.clone());
    }
    let key = with_application(identity::load_context).map_err(|_| {
        "Sharing identity unavailable; stored identity preserved. Reset identity to start over."
    })?;
    *cached = Some(key.clone());
    Ok(key)
}

/// Drops cached identity and friend store, e.g. after another process reset them.
pub fn forget_identity() {
    if let Ok(mut key) = KEY.lock() {
        key.take();
    }
    if let Ok(mut manager) = MANAGER.lock() {
        manager.take();
    }
}

/// New identity: deletes key, friends, groups, and received data. Caller turns sharing
/// off first so no node uses the old key. Old friends must pair again.
pub fn reset_identity(directory: &Path) -> Result<()> {
    with_application(identity::reset_context).map_err(|_| "Could not reset sharing identity")?;
    forget_identity();
    let sharing = directory.join("sharing");
    for name in [
        "friends.json",
        "location-state.json",
        "friend-locations.json",
        "group-notices.postcard",
        "group-fixes.postcard",
        "places.json",
    ] {
        match std::fs::remove_file(sharing.join(name)) {
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => return Err(error.into()),
            _ => {}
        }
    }
    for name in [
        "friend-trips",
        "trip-requests",
        "location-requests",
        "groups",
    ] {
        let _ = std::fs::remove_dir_all(sharing.join(name));
    }
    Ok(())
}

/// Friends and sharing settings under `directory/sharing`. Same files in both processes.
pub fn manager(directory: &Path) -> Result<Manager> {
    let mut cached = MANAGER.lock().map_err(|_| "sharing lock failed")?;
    if let Some(manager) = cached.as_ref() {
        return Ok(manager.clone());
    }
    let manager = Manager::new(Friends::open(directory, key()?.public())?, key()?)?;
    *cached = Some(manager.clone());
    Ok(manager)
}

/// Mode without decrypting identity: background events check this before starting work.
pub fn mode(directory: &Path) -> Mode {
    #[derive(serde::Deserialize)]
    struct State {
        mode: Mode,
        #[serde(default)]
        expires_at_ms: Option<i64>,
    }
    std::fs::read(directory.join("sharing").join("location-state.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<State>(&bytes).ok())
        .map_or(Mode::Off, |s| {
            if s.expires_at_ms.is_some_and(|until| now_ms() >= until) {
                Mode::Off
            } else {
                s.mode
            }
        })
}

/// Service process has no android-activity glue; iroh reads DNS servers through this context.
fn install_dns() -> Result<()> {
    static DNS: OnceLock<Global<JObject<'static>>> = OnceLock::new();
    if DNS.get().is_some() {
        return Ok(());
    }
    let global = with_application(|env, application| env.new_global_ref(application))?;
    let vm = JavaVM::singleton()?.get_raw();
    DNS.get_or_init(|| {
        // SAFETY: global reference lives in DNS until process exit; VM lives as long.
        unsafe { iroh::dns::install_android_jni_context(vm.cast(), global.as_raw().cast()) };
        global
    });
    Ok(())
}

// --- Recorder service process ---------------------------------------------------------

/// Runs node while sharing mode is on, stops it otherwise. Returns whether it runs.
pub fn sync_node(directory: &Path) -> Result<bool> {
    let mut node = NODE.lock().map_err(|_| "sharing lock failed")?;
    if mode(directory) == Mode::Off {
        node.take();
        return Ok(false);
    }
    if let Some(node) = node.as_ref() {
        node.manager().notify();
        return Ok(true);
    }
    install_dns()?;
    let manager = manager(directory)?;
    let journal = directory.to_owned();
    manager.set_refresh_source(std::sync::Arc::new(|| {
        zgui::app::android::background::send_service("locate", "").map_err(Into::into)
    }));
    manager.set_trip_source(std::sync::Arc::new(move || {
        let trips =
            crate::backend::request(journal.clone(), serde_json::json!({"op": "shared_trips"}))?;
        Ok(serde_json::to_vec(&trips)?)
    }));
    *node = Some(Node::start(key()?, manager, directory)?);
    Ok(true)
}

/// Closes endpoint (pairing in UI process needs the identity alone) or on service stop.
pub fn stop_node() {
    if let Ok(mut node) = NODE.lock() {
        node.take();
    }
}

pub fn node_running() -> bool {
    NODE.lock().is_ok_and(|node| node.is_some())
}

/// Current GPS fix from recorder or sharing watch. Only the newest fix is kept.
pub fn fix(fix: crate::model::Fix, speed: Option<f64>, bearing: Option<f64>, recording: bool) {
    let Ok(node) = NODE.lock() else { return };
    let Some(node) = node.as_ref() else { return };
    let sample = Sample {
        fix,
        speed_m_s: speed,
        heading_degrees: bearing,
    };
    let _ = node.manager().update_fix(sample, recording, now_ms());
}

/// Location services off: friends see "no location" instead of an aging fix.
pub fn clear_fix() {
    if let Ok(node) = NODE.lock()
        && let Some(node) = node.as_ref()
    {
        let _ = node.manager().clear_fix();
    }
}

pub fn set_recording(recording: bool) {
    if let Ok(node) = NODE.lock()
        && let Some(node) = node.as_ref()
    {
        let _ = node.manager().set_recording(recording);
    }
}

/// Minimum seconds between sent updates; also caps sampling interval while moving.
pub fn policy(directory: &Path) -> UpdatePolicy {
    #[derive(serde::Deserialize)]
    struct State {
        policy: UpdatePolicy,
    }
    std::fs::read(directory.join("sharing").join("location-state.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<State>(&bytes).ok())
        .map_or_else(UpdatePolicy::default, |s| s.policy)
}

// --- UI process: pairing ---------------------------------------------------------------

#[derive(Clone, Debug, Default, PartialEq)]
pub enum PairStep {
    #[default]
    Idle,
    Starting,
    /// Invitation link and QR module rows (quiet zone excluded).
    Invite {
        link: String,
        qr: Vec<String>,
    },
    Confirm {
        name: String,
        code: String,
        /// Group joined on confirmation.
        group: Option<String>,
        group_details: Option<String>,
        joining: bool,
    },
    /// Local vote sent; waiting for friend.
    Waiting,
    Done(String),
    Failed(String),
}

/// One pairing session on its own thread and endpoint. Dropping it cancels the session.
pub struct Pairing {
    step: std::sync::Arc<Mutex<PairStep>>,
    stop: tokio::sync::watch::Sender<bool>,
    vote: tokio::sync::watch::Sender<Option<(bool, super::peers::Permission)>>,
}

impl Pairing {
    /// Shows an invitation when `link` is None, else requests pairing from that invitation.
    /// `group`: invitation joins invitee to that group.
    pub fn start(
        directory: std::path::PathBuf,
        name: String,
        link: Option<String>,
        group: Option<super::pairing::GroupRef>,
    ) -> Self {
        let step = std::sync::Arc::new(Mutex::new(PairStep::Starting));
        let (stop, stopped) = tokio::sync::watch::channel(false);
        let (vote, votes) = tokio::sync::watch::channel(None);
        let shared = step.clone();
        std::thread::spawn(move || {
            let set = |next: PairStep| {
                if let Ok(mut step) = shared.lock() {
                    *step = next;
                }
            };
            let result = pair(
                &directory,
                &name,
                link.as_deref(),
                group,
                stopped,
                votes,
                &set,
            );
            set(match result {
                Ok(true) => PairStep::Done(
                    "Paired. Direct friends start Hidden; group access uses your selection.".into(),
                ),
                Ok(false) => PairStep::Idle,
                Err(error) => PairStep::Failed(error.to_string()),
            });
        });
        Self { step, stop, vote }
    }

    pub fn step(&self) -> PairStep {
        self.step.lock().map(|s| s.clone()).unwrap_or_default()
    }

    pub fn vote(&self, accepted: bool, share: super::peers::Permission) {
        if matches!(self.step(), PairStep::Confirm { .. }) {
            if let Ok(mut step) = self.step.lock() {
                *step = PairStep::Waiting;
            }
            self.vote.send_replace(Some((accepted, share)));
        }
    }
}

impl Drop for Pairing {
    fn drop(&mut self) {
        self.stop.send_replace(true);
    }
}

/// Ok(false): cancelled.
fn pair(
    directory: &Path,
    name: &str,
    link: Option<&str>,
    group: Option<super::pairing::GroupRef>,
    mut stopped: tokio::sync::watch::Receiver<bool>,
    mut votes: tokio::sync::watch::Receiver<Option<(bool, super::peers::Permission)>>,
    set: &dyn Fn(PairStep),
) -> Result<bool> {
    use super::pairing::{self, FriendInvite};
    let invite = link.map(FriendInvite::from_link).transpose()?;
    let friends = manager(directory)?.friends().clone();
    let key = key()?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()?;
    runtime.block_on(async {
        let endpoint = pairing::bind(key).await?;
        let work = async {
            // Relay address lets friend reach this phone from another network.
            let _ =
                tokio::time::timeout(std::time::Duration::from_secs(30), endpoint.online()).await;
            let pending = if let Some(invite) = invite {
                pairing::request(&endpoint, &invite, name, &friends).await?
            } else {
                let mut invite = FriendInvite::new(&endpoint, name.to_owned())?;
                invite.group = group;
                let link = invite.to_link()?;
                set(PairStep::Invite {
                    qr: pairing::qr_modules(&link)?,
                    link,
                });
                loop {
                    // Wrong token or blocked peer: keep invitation open for intended friend.
                    if let Ok(pending) = pairing::accept(&endpoint, &invite, &friends).await {
                        break pending;
                    }
                }
            };
            set(PairStep::Confirm {
                name: pending.display_name.clone(),
                code: pending.verification_code.clone(),
                group: pending.group.as_ref().map(|g| g.name.clone()),
                joining: !pending.is_inviter(),
                group_details: pending.group_state.as_ref().map(|state| {
                    let creator = &state.creator_name;
                    format!(
                        "Created by {creator} · {} members · {}",
                        state.members.len(),
                        match state.policy {
                            super::groups::Policy::Managed => "Managed: admins invite and remove",
                            super::groups::Policy::Open => "Open: any member can invite",
                        }
                    )
                }),
            });
            let (accepted, share) = loop {
                if let Some(accepted) = *votes.borrow() {
                    break accepted;
                }
                votes.changed().await?;
            };
            pending.confirm(accepted, &friends, share).await?;
            Ok::<_, Box<dyn std::error::Error + Send + Sync>>(true)
        };
        let result = tokio::select! {
            biased;
            _ = stopped.changed() => Ok(false),
            result = tokio::time::timeout(pairing::CONFIRM_TIMEOUT, work) => {
                result.map_err(|_| "Pairing timed out")?
            }
        };
        endpoint.close().await;
        result
    })
}

/// Decodes one Roamlog invitation from a photo or screenshot.
pub fn decode_qr_image(bytes: &[u8]) -> Result<String> {
    let image = image::load_from_memory(bytes)?;
    // Phone photos are large; decoder limit is 2048 px per side.
    let image = if image.width() > 2048 || image.height() > 2048 {
        image.resize(2048, 2048, image::imageops::FilterType::Triangle)
    } else {
        image
    };
    let gray = image.to_luma8();
    super::pairing::decode_qr(gray.width() as usize, gray.height() as usize, gray.as_raw())
}

/// QR modules as a crisp PNG with quiet zone, for an image element.
pub fn qr_png(rows: &[String]) -> Result<Vec<u8>> {
    const SCALE: usize = 8;
    let size = rows.len() + 8;
    let px = size * SCALE;
    let mut pixels = vec![255u8; px * px];
    for (y, row) in rows.iter().enumerate() {
        for (x, module) in row.bytes().enumerate() {
            if module == b'1' {
                for dy in 0..SCALE {
                    let start = ((y + 4) * SCALE + dy) * px + (x + 4) * SCALE;
                    pixels[start..start + SCALE].fill(0);
                }
            }
        }
    }
    let mut out = Vec::new();
    let mut encoder = png::Encoder::new(&mut out, px as u32, px as u32);
    encoder.set_color(png::ColorType::Grayscale);
    encoder.write_header()?.write_image_data(&pixels)?;
    Ok(out)
}

/// BatteryManager reports percentage without a new Android permission.
pub fn battery_percent() -> Result<Option<u8>> {
    with_application(|env, application| {
        let name = JString::from_str(env, "batterymanager")?;
        let service = env
            .call_method(
                application,
                jni_str!("getSystemService"),
                jni_sig!("(Ljava/lang/String;)Ljava/lang/Object;"),
                &[JValue::Object(name.as_ref())],
            )?
            .l()?;
        let percent = env
            .call_method(
                &service,
                jni_str!("getIntProperty"),
                jni_sig!("(I)I"),
                &[JValue::Int(4)],
            )?
            .i()?;
        Ok((0..=100).contains(&percent).then_some(percent as u8))
    })
}
