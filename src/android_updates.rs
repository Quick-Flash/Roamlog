//! Narrow APK installer bridge; Zgui still owns Android host and background recorder.
use jni::{
    JavaVM, jni_sig, jni_str,
    objects::{Global, JClass, JObject, JString, JValue},
};
use std::sync::OnceLock;
use zgui::app::AndroidApp;
static APP: OnceLock<AndroidApp> = OnceLock::new();
pub fn init(app: &AndroidApp) {
    let _ = APP.set(app.clone());
}
pub fn call(op: &str, path: &str) -> Result<serde_json::Value, String> {
    let app = APP.get().ok_or("Android activity unavailable")?;
    // SAFETY: AndroidApp owns VM and activity global reference throughout this call.
    let vm = unsafe { JavaVM::from_raw(app.vm_as_ptr().cast()) };
    let reply = vm.attach_current_thread(|env| {
        let raw = app.activity_as_ptr().cast();
        // SAFETY: borrow AndroidApp's global reference without taking ownership.
        let activity = unsafe { env.as_cast_raw::<Global<JObject>>(&raw)? };
        let loader = env.call_method(activity.as_ref(), jni_str!("getClassLoader"), jni_sig!("()Ljava/lang/ClassLoader;"), &[])?.l()?;
        let name = JString::from_str(env, "dev.roamlog.android.Updates")?;
        let class = env.call_method(&loader, jni_str!("loadClass"), jni_sig!("(Ljava/lang/String;)Ljava/lang/Class;"), &[JValue::Object(name.as_ref())])?.l()?;
        let class = env.cast_local::<JClass>(class)?;
        let op = JString::from_str(env, op)?;
        let path = JString::from_str(env, path)?;
        let value = env.call_static_method(&class, jni_str!("call"), jni_sig!("(Landroid/app/Activity;Ljava/lang/String;Ljava/lang/String;)Ljava/lang/String;"), &[JValue::Object(activity.as_ref()), JValue::Object(op.as_ref()), JValue::Object(path.as_ref())])?.l()?;
        env.cast_local::<JString>(value)?.try_to_string(env)
    }).map_err(|e| e.to_string())?;
    let value: serde_json::Value = serde_json::from_str(&reply).map_err(|e| e.to_string())?;
    if let Some(error) = value["error"].as_str() {
        return Err(error.into());
    }
    Ok(value)
}
