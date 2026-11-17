//! JNI entry points for the isolated foreground-only Android probe app.
use std::sync::Mutex;
use std::time::Duration;

use jni::{
    Env, EnvUnowned, jni_sig, jni_str,
    objects::{JByteArray, JClass, JObject, JString, JValue},
    sys::{jboolean, jint},
};
use tokio::sync::watch;

use super::{
    Result, android_friends, android_locations, bind, identity,
    manager::{Manager, Mode},
    pairing, probe, transport,
};

struct Controls {
    stop: watch::Sender<bool>,
    vote: watch::Sender<Option<bool>>,
    awaiting_vote: bool,
    manager: Option<Manager>,
}
static SESSION: Mutex<Option<Controls>> = Mutex::new(None);
// Serialize UI commands with final pause, without blocking main-thread stop signal.
static COMMANDS: Mutex<()> = Mutex::new(());

struct Session;
impl Drop for Session {
    fn drop(&mut self) {
        *SESSION.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }
}

fn report(env: &mut Env<'_>, activity: &JObject<'_>, message: &str) -> Result<()> {
    if message.starts_with("confirm:")
        && let Some(state) = SESSION.lock().unwrap_or_else(|e| e.into_inner()).as_mut()
    {
        state.awaiting_vote = true;
    }
    env.with_local_frame(8, |env| -> Result<()> {
        let message = JString::from_str(env, message)?;
        env.call_method(
            activity,
            jni_str!("progress"),
            jni_sig!("(Ljava/lang/String;)V"),
            &[JValue::Object(message.as_ref())],
        )?;
        Ok(())
    })
}

#[allow(non_snake_case)]
#[unsafe(no_mangle)]
extern "system" fn Java_dev_roamlog_android_SharingProbe_prepare(
    _env: EnvUnowned<'_>,
    _class: JClass<'_>,
) -> jboolean {
    let mut state = SESSION.lock().unwrap_or_else(|e| e.into_inner());
    if state.is_some() {
        return false;
    }
    *state = Some(Controls {
        stop: watch::channel(false).0,
        vote: watch::channel(None).0,
        awaiting_vote: false,
        manager: None,
    });
    true
}

#[allow(non_snake_case)]
#[unsafe(no_mangle)]
extern "system" fn Java_dev_roamlog_android_SharingProbe_command(
    mut env: EnvUnowned<'_>,
    activity: JObject<'_>,
    command: JString<'_>,
    target: JString<'_>,
) {
    env.with_env(|env| -> jni::errors::Result<()> {
        let _command = COMMANDS.lock().unwrap_or_else(|e| e.into_inner());
        let manager = SESSION
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .filter(|state| !*state.stop.borrow())
            .and_then(|state| state.manager.clone());
        let result: Result<()> = (|| {
            let manager = manager.ok_or("no location session")?;
            let command = command.try_to_string(env)?;
            let target = target.try_to_string(env)?;
            if android_friends::local(&command, &target, manager.friends())? {
                report(
                    env,
                    &activity,
                    &format!(
                        "friends:{}",
                        serde_json::to_string(&manager.friends().list()?)?
                    ),
                )?;
            } else {
                android_locations::command(&manager, &command, &target)?;
            }
            report(
                env,
                &activity,
                &format!("mode:{}", serde_json::to_string(&manager.mode()?)?),
            )?;
            Ok(())
        })();
        if result.is_err() {
            let _ = report(env, &activity, "Sharing control failed; no location queued");
        }
        Ok(())
    })
    .resolve::<jni::errors::ThrowRuntimeExAndDefault>();
}

#[allow(non_snake_case)]
#[unsafe(no_mangle)]
extern "system" fn Java_dev_roamlog_android_SharingProbe_stop(
    _env: EnvUnowned<'_>,
    _class: JClass<'_>,
) {
    if let Some(state) = SESSION.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
        state.stop.send_replace(true);
    }
}

#[allow(non_snake_case)]
#[unsafe(no_mangle)]
extern "system" fn Java_dev_roamlog_android_SharingProbe_vote(
    _env: EnvUnowned<'_>,
    _class: JClass<'_>,
    accepted: jboolean,
) {
    if let Some(state) = SESSION.lock().unwrap_or_else(|e| e.into_inner()).as_mut()
        && state.awaiting_vote
    {
        state.awaiting_vote = false;
        state.vote.send_replace(Some(accepted));
    }
}

#[allow(non_snake_case)]
#[unsafe(no_mangle)]
extern "system" fn Java_dev_roamlog_android_SharingProbe_decode<'a>(
    mut env: EnvUnowned<'a>,
    _class: JClass<'a>,
    width: jint,
    height: jint,
    gray: JByteArray<'a>,
) -> JString<'a> {
    env.with_env(|env| -> jni::errors::Result<_> {
        let invalid = || jni::errors::Error::JniCall(jni::errors::JniError::InvalidArguments);
        if width <= 0
            || height <= 0
            || width > 2048
            || height > 2048
            || gray.len(env)? != width as usize * height as usize
        {
            return Err(invalid());
        }
        let bytes = env.convert_byte_array(&gray)?;
        let link =
            pairing::decode_qr(width as usize, height as usize, &bytes).map_err(|_| invalid())?;
        JString::from_str(env, link)
    })
    .resolve::<jni::errors::ThrowRuntimeExAndDefault>()
}

#[allow(non_snake_case)]
#[unsafe(no_mangle)]
extern "system" fn Java_dev_roamlog_android_SharingProbe_run(
    mut env: EnvUnowned<'_>,
    activity: JObject<'_>,
    mode: JString<'_>,
    target: JString<'_>,
    name: JString<'_>,
) {
    let _session = Session;
    env.with_env(|env| -> jni::errors::Result<()> {
        let result = run(env, &activity, &mode, &target, &name);
        // Avoid forwarding provider errors or sensitive data through logs/UI.
        let message = if result.is_ok() {
            "Done; networking stopped"
        } else {
            "Operation failed or pairing declined; check identity, peer, and network"
        };
        let _ = report(env, &activity, message);
        Ok(())
    })
    .resolve::<jni::errors::ThrowRuntimeExAndDefault>();
}

fn run(
    env: &mut Env<'_>,
    activity: &JObject<'_>,
    mode: &JString<'_>,
    target: &JString<'_>,
    name: &JString<'_>,
) -> Result<()> {
    let (mut stop, mut votes) = {
        let controls = SESSION.lock().unwrap_or_else(|e| e.into_inner());
        let controls = controls.as_ref().ok_or("probe not prepared")?;
        (controls.stop.subscribe(), controls.vote.subscribe())
    };
    if *stop.borrow() {
        return Ok(());
    }
    let mode = mode.try_to_string(env)?;
    let target = target.try_to_string(env)?;
    let name = name.try_to_string(env)?;
    let key = identity::load_context(env, activity)?;
    report(env, activity, &format!("peer: {}", key.public()))?;
    let friends = android_friends::store(env, activity, key.public())?;
    if android_friends::local(&mode, &target, &friends)? || *stop.borrow() {
        report(
            env,
            activity,
            &format!("friends:{}", serde_json::to_string(&friends.list()?)?),
        )?;
        return Ok(());
    }
    let is_pairing = matches!(mode.as_str(), "invite" | "pair");
    let is_locations = matches!(mode.as_str(), "location_listen" | "location_connect");
    let manager = if is_locations {
        let manager = Manager::new(friends.clone(), key.clone())?;
        if let Some(state) = SESSION.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
            state.manager = Some(manager.clone());
        }
        report(
            env,
            activity,
            &format!("mode:{}", serde_json::to_string(&manager.mode()?)?),
        )?;
        Some(manager)
    } else {
        None
    };
    let invite = if mode == "pair" {
        Some(pairing::FriendInvite::from_link(&target)?)
    } else {
        None
    };
    #[cfg(not(feature = "ui"))]
    init_dns(env, activity)?;
    let runtime = tokio::runtime::Runtime::new()?;
    let result = runtime.block_on(async {
        let endpoint = tokio::select! {
            biased;
            _ = stop.changed() => return Ok(()),
            result = async {
                if is_pairing { pairing::bind(key).await }
                else if is_locations { transport::bind(key).await }
                else { bind(key).await }
            } => result?,
        };
        let result = tokio::select! {
            biased;
            _ = stop.changed() => Ok(()),
            result = async {
                report(env, activity, "Waiting for relay (up to 30 seconds)")?;
                if tokio::time::timeout(Duration::from_secs(30), endpoint.online()).await.is_err() {
                    report(env, activity, "Relay unavailable; using available addresses")?;
                }
                report(env, activity, &format!("address: {}", serde_json::to_string(&endpoint.addr())?))?;
                if is_pairing {
                    android_friends::pair(&endpoint, &name, invite, &friends, &mut votes,
                        |message| report(env, activity, message)).await?;
                    report(env, activity, &format!("friends:{}", serde_json::to_string(&friends.list()?)?))?;
                    return Ok(());
                }
                if let Some(manager) = &manager {
                    return android_locations::run(&endpoint, manager.clone(), &target, mode == "location_connect",
                        |message| report(env, activity, message)).await;
                }
                match mode.as_str() {
                    "listen" => probe::server(&endpoint, target.parse()?, true,
                        |message| report(env, activity, message)).await,
                    "connect" => probe::client(&endpoint, serde_json::from_str(&target)?, true,
                        |message| report(env, activity, message)).await,
                    _ => Err("unknown probe mode".into()),
                }
            } => result,
        };
        endpoint.close().await;
        result
    });
    if let Some(manager) = manager {
        let _command = COMMANDS.lock().unwrap_or_else(|e| e.into_inner());
        manager.set_mode(Mode::Off)?;
    }
    result
}

#[cfg(not(feature = "ui"))]
fn init_dns(env: &mut Env<'_>, activity: &JObject<'_>) -> Result<()> {
    use jni::objects::Global;
    use std::sync::OnceLock;
    static CONTEXT: OnceLock<Global<JObject<'static>>> = OnceLock::new();
    if CONTEXT.get().is_none() {
        let context = env
            .call_method(
                activity,
                jni_str!("getApplicationContext"),
                jni_sig!("()Landroid/content/Context;"),
                &[],
            )?
            .l()?;
        let global = env.new_global_ref(context)?;
        let vm = env.get_java_vm()?.get_raw();
        CONTEXT.get_or_init(|| {
            // SAFETY: application global reference is held until process exit;
            // Android VM has process lifetime. Standalone probe has no glue init.
            unsafe {
                iroh::dns::install_android_jni_context(vm.cast(), global.as_raw().cast());
            }
            global
        });
    }
    Ok(())
}
