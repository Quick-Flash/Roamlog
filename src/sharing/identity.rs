//! Android Keystore-backed identity. No endpoint is started by loading a key.

use iroh::SecretKey;
use jni::{
    Env, jni_sig, jni_str,
    objects::{JByteArray, JClass, JObject, JString, JValue},
};
#[cfg(feature = "ui")]
use zgui::app::AndroidApp;

/// Load/create device identity on a worker thread. Java serializes access across
/// activity and recorder processes and refuses to replace unreadable identities.
#[cfg(feature = "ui")]
pub fn load_android(app: &AndroidApp) -> Result<SecretKey, String> {
    // SAFETY: AndroidApp keeps VM and activity global reference alive during call.
    let vm = unsafe { jni::JavaVM::from_raw(app.vm_as_ptr().cast()) };
    vm.attach_current_thread(|env| {
        let raw = app.activity_as_ptr().cast();
        // SAFETY: borrow AndroidApp's global reference without taking ownership.
        let activity = unsafe { env.as_cast_raw::<jni::objects::Global<JObject>>(&raw)? };
        load_context(env, activity.as_ref())
    })
    .map_err(|_: jni::errors::Error| {
        "Android sharing identity unavailable; stored identity preserved".to_owned()
    })
}

/// Deletes stored identity and wrapping key; next load creates a new identity.
pub fn reset_context(env: &mut Env<'_>, context: &JObject<'_>) -> jni::errors::Result<()> {
    let class = bridge(env, context)?;
    env.call_static_method(
        &class,
        jni_str!("reset"),
        jni_sig!("(Landroid/content/Context;)V"),
        &[JValue::Object(context)],
    )?;
    Ok(())
}

fn bridge<'local>(
    env: &mut Env<'local>,
    context: &JObject<'_>,
) -> jni::errors::Result<JClass<'local>> {
    let loader = env
        .call_method(
            context,
            jni_str!("getClassLoader"),
            jni_sig!("()Ljava/lang/ClassLoader;"),
            &[],
        )?
        .l()?;
    let name = JString::from_str(env, "dev.roamlog.android.SharingIdentity")?;
    let class = env
        .call_method(
            &loader,
            jni_str!("loadClass"),
            jni_sig!("(Ljava/lang/String;)Ljava/lang/Class;"),
            &[JValue::Object(name.as_ref())],
        )?
        .l()?;
    env.cast_local::<JClass>(class)
}

/// Context-based entry for activity-less platform callers on an attached JNI thread.
pub fn load_context(env: &mut Env<'_>, context: &JObject<'_>) -> jni::errors::Result<SecretKey> {
    let class = bridge(env, context)?;
    let bytes = env
        .call_static_method(
            &class,
            jni_str!("loadOrCreate"),
            jni_sig!("(Landroid/content/Context;)[B"),
            &[JValue::Object(context)],
        )?
        .l()?;
    let bytes = env.cast_local::<JByteArray>(bytes)?;
    let mut seed = env.convert_byte_array(&bytes)?;
    let result = <[u8; 32]>::try_from(seed.as_slice()).map(|mut bytes| {
        let key = SecretKey::from_bytes(&bytes);
        bytes.fill(0);
        key
    });
    seed.fill(0);
    bytes.set_region(env, 0, &vec![0; bytes.len(env)?])?;
    result.map_err(|_| jni::errors::Error::JniCall(jni::errors::JniError::InvalidArguments))
}
