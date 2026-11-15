use super::state::UiState;
use crate::updates::{self, Update};
use std::path::PathBuf;
use zgui::{prelude::*, reactive::Owner};

#[derive(Clone, Copy)]
pub struct Updater {
    pub automatic: RwSignal<bool, LocalStorage>,
    pub busy: RwSignal<bool, LocalStorage>,
    pub available: RwSignal<Option<Update>, LocalStorage>,
    pub ready: RwSignal<Option<PathBuf>, LocalStorage>,
    pub message: RwSignal<String, LocalStorage>,
    info: StoredValue<(u64, bool, String), LocalStorage>,
    last: StoredValue<Option<u64>, LocalStorage>,
    owner: StoredValue<Owner, LocalStorage>,
}
impl Updater {
    pub fn new(directory: &std::path::Path) -> Self {
        let info = installed();
        Self {
            automatic: RwSignal::new_local(
                std::fs::read_to_string(directory.join("automatic-updates"))
                    .map_or(true, |v| v != "off"),
            ),
            busy: RwSignal::new_local(false),
            available: RwSignal::new_local(None),
            ready: RwSignal::new_local(None),
            message: RwSignal::new_local(String::new()),
            info: StoredValue::new_local(info),
            last: StoredValue::new_local(None),
            owner: StoredValue::new_local(Owner::current().expect("updater needs app owner")),
        }
    }
    fn spawn(self, work: impl std::future::Future<Output = ()> + 'static) {
        self.owner
            .with_value(|owner| owner.with(|| spawn_local(work)));
    }
    pub fn set_automatic(self, enabled: bool) {
        self.automatic.set(enabled);
        let path = UiState::current()
            .directory
            .get_value()
            .join("automatic-updates");
        self.spawn(async move {
            if let Err(error) =
                blocking(move || std::fs::write(path, if enabled { "on" } else { "off" })).await
            {
                self.message
                    .set(format!("Could not save update preference: {error}"));
            }
        });
    }
    pub fn version(self) -> String {
        self.info.with_value(|i| i.2.clone())
    }
    pub fn tick(self, now: u64) {
        if cfg!(target_os = "android")
            && self.automatic.get_untracked()
            && self.last.with_value(|last| {
                last.is_none_or(|last| now.saturating_sub(last) >= 6 * 3600 * 1000)
            })
        {
            self.check(now);
        }
    }
    pub fn check(self, now: u64) {
        if self.busy.get_untracked() {
            return;
        }
        self.last.set_value(Some(now));
        self.busy.set(true);
        self.message.set("Checking GitHub releases…".into());
        let (code, demo, _) = self.info.get_value();
        if code == 0 {
            self.busy.set(false);
            self.message
                .set("Installed Android version unavailable.".into());
            return;
        }
        self.spawn(async move {
            let result = blocking(move || updates::latest(code, demo)).await;
            match result {
                Ok(update) => {
                    if update != self.available.get_untracked() {
                        self.ready.set(None);
                    }
                    self.message.set(
                        if update.is_some() {
                            "Update available."
                        } else {
                            "Up to date."
                        }
                        .into(),
                    );
                    self.available.set(update);
                }
                Err(error) => self.message.set(format!("Update check failed: {error}")),
            }
            self.busy.set(false);
        });
    }
    pub fn download(self) {
        if self.busy.get_untracked() {
            return;
        }
        let Some(update) = self.available.get_untracked() else {
            return;
        };
        let directory = shared_dir();
        let Ok(directory) = directory else {
            self.message.set(directory.unwrap_err());
            return;
        };
        self.busy.set(true);
        self.ready.set(None);
        self.message.set("Downloading and verifying APK…".into());
        self.spawn(async move {
            match blocking(move || updates::download(&update, &directory)).await {
                Ok(path) => {
                    self.ready.set(Some(path));
                    self.message.set("APK verified. Ready to install.".into());
                }
                Err(error) => self.message.set(format!("Download failed: {error}")),
            }
            self.busy.set(false);
        });
    }
    pub fn install(self) {
        let s = UiState::current();
        if self.busy.get_untracked() {
            return;
        }
        if s.status().is_some() {
            self.message
                .set("Finish trip before installing update.".into());
            return;
        }
        let Some(path) = self.ready.get_untracked() else {
            return;
        };
        // Re-read journal: UI snapshot may lag recorder service.
        self.spawn(async move {
            match s
                .request(serde_json::json!({"op":"snapshot","map":false}))
                .await
            {
                Ok(value) => {
                    let state =
                        serde_json::from_value::<crate::model::State>(value["state"].clone());
                    let Ok(state) = state else {
                        self.message
                            .set("Could not verify trip state; update not installed.".into());
                        return;
                    };
                    let active = state
                        .trips
                        .iter()
                        .any(|t| t.status != crate::model::Status::Finished);
                    if active {
                        self.message
                            .set("Finish trip before installing update.".into());
                        return;
                    }
                    match install(&path) {
                        Ok(message) => self.message.set(message),
                        Err(error) => self.message.set(error),
                    }
                }
                Err(error) => self.message.set(error),
            }
        });
    }
}
fn installed() -> (u64, bool, String) {
    #[cfg(target_os = "android")]
    if let Ok(value) = crate::android_updates::call("info", "") {
        return (
            value["version_code"].as_u64().unwrap_or(1),
            value["demo"].as_bool().unwrap_or(false),
            value["version_name"]
                .as_str()
                .unwrap_or("Unknown build")
                .into(),
        );
    }
    (0, false, env!("CARGO_PKG_VERSION").into())
}
fn shared_dir() -> Result<PathBuf, String> {
    #[cfg(target_os = "android")]
    {
        zgui::app::android::document::shared_dir()
    }
    #[cfg(not(target_os = "android"))]
    {
        Err("APK updates are available on Android.".into())
    }
}
fn install(path: &std::path::Path) -> Result<String, String> {
    #[cfg(target_os = "android")]
    {
        crate::android_updates::call("install", path.to_str().ok_or("Invalid APK path")?)
            .map(|v| v["message"].as_str().unwrap_or("Installer opened").into())
    }
    #[cfg(not(target_os = "android"))]
    {
        let _ = path;
        Err("APK updates are available on Android.".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zgui::reactive::Mounted;
    #[test]
    fn closing_settings_keeps_update_completion_alive() {
        let window = zgui_testkit_view::Window::open();
        let directory = tempfile::tempdir().unwrap();
        let updater = window.scope.with(|| Updater::new(directory.path()));
        let settings = window.scope.with(Mounted::new);
        let (sender, receiver) = async_channel::bounded(1);
        settings.with(|| {
            updater.spawn(async move {
                receiver.recv().await.unwrap();
                updater.message.set("Finished after Settings closed".into());
            })
        });
        window.frame();
        settings.unmount();
        sender.try_send(()).unwrap();
        window.frame();
        assert_eq!(
            updater.message.get_untracked(),
            "Finished after Settings closed"
        );
    }
}
