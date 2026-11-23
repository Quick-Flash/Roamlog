#[cfg(any(target_os = "android", target_os = "ios"))]
mod friends;
mod history;
mod history_dates;
mod history_trip;
pub(crate) mod icons;
#[cfg(any(target_os = "android", target_os = "ios"))]
pub(crate) mod place_alerts;
mod recorder;
pub(crate) mod settings;
mod snapshot;
pub(crate) mod state;
pub(crate) mod tools;
mod trip_sources;
mod update_controls;
mod update_notice;
mod update_state;
pub(crate) mod view_options;

use crate::mapping::view::MapViewProps;
use history::HistoryProps;
use recorder::RecorderProps;
use settings::SettingsProps;
use state::{UiState, now_ms};
use std::{path::PathBuf, time::Duration};
use update_notice::UpdateNoticeProps;
use zgui::prelude::*;
use zgui_ui::prelude::*;
use zgui_ui_icons::prelude::*;
use zgui_ui_tokens::prelude::*;

#[component]
pub fn Roamlog() -> impl IntoView {
    let s = UiState::current();
    let tick = StoredValue::new_local(0_u64);
    s.updates.tick(now_ms());
    s.refresh(true);
    s.reload_basemap();
    let timer = set_interval(Duration::from_secs(1), move || {
        s.now.set(now_ms());
        s.updates.tick(now_ms());
        tick.update_value(|t| *t += 1);
        s.refresh(tick.get_value().is_multiple_of(5));
        s.download_status();
        s.start_position();
        s.refresh_friends();
        if tick.get_value().is_multiple_of(5) {
            s.auto_map();
        }
        // Service forgets an open app after a while; keep saying so.
        #[cfg(any(target_os = "android", target_os = "ios"))]
        if tick.get_value().is_multiple_of(240) {
            crate::platform::app_visible(&s.directory.get_value(), true);
        }
    });
    #[cfg(any(target_os = "android", target_os = "ios"))]
    {
        crate::platform::listen_invites();
        // Rebuilt after hiding mid-pairing: pairing endpoint still holds the identity.
        if !friends::pairing_open() {
            crate::platform::resume_sharing(s.directory.get_value());
        }
        // Open app: stay connected for quick updates. Hidden (view torn down): check-ins only.
        crate::platform::app_visible(&s.directory.get_value(), true);
        on_cleanup_local(move || crate::platform::app_visible(&s.directory.get_value(), false));
        place_alerts::follow_selected_friend(s);
    }
    on_cleanup_local(move || drop(timer));
    #[cfg(target_os = "android")]
    {
        let bars = zgui::reactive::RenderEffect::new(move |_| {
            zgui::app::android::system_bar_theme(Some(s.dark.get()))
        });
        on_cleanup_local(move || drop(bars));
    }
    #[cfg(target_os = "ios")]
    {
        let bars = zgui::reactive::RenderEffect::new(move |_| {
            zgui::app::ios::status_bar_dark(s.dark.get())
        });
        on_cleanup_local(move || drop(bars));
    }
    let navigate = move |tab| {
        s.places_open.set(false);
        s.trips_open.set(tab == 1);
        s.friends_open.set(tab == 2);
        s.settings_open.set(tab == 3);
    };
    // Alert areas page belongs to Friends, even when reached from the map.
    let map_selected = move || {
        !s.trips_open.get()
            && !s.friends_open.get()
            && !s.settings_open.get()
            && !s.places_open.get()
    };
    let circle_setup = move || s.setup.with(Option::is_some);
    view! {
        ThemeProvider(light=Signal::stored_local(light_theme()),dark=Signal::stored_local(dark_theme()),scheme=Signal::derive_local(move ||if s.dark.get(){ColorScheme::Dark}else{ColorScheme::Light})) {
            column(class="roamlog",class:circle-setup=circle_setup) {
                column(class="tab-stage") {
                    MapView()
                    UpdateNotice()
                    if move ||!circle_setup() {Recorder()}
                    History()
                    Settings()
                    FriendsSheet()
                }
                if move ||!circle_setup() { row(class="bottom-tabs") {
                    Button(variant=ButtonVariant::Ghost,class:tab-selected=map_selected,a11y:label="Map",on:click=move |_|navigate(0)) {
                        Icon(icon=icons::COMPASS)
                        row(class="map-tab-label") {
                            text {"Map"}
                            if move ||s.status()==Some(crate::model::Status::Recording) {box(class="record-dot") {}}
                        }
                    }
                    Button(variant=ButtonVariant::Ghost,class:tab-selected=move ||s.trips_open.get(),a11y:label="History",on:click=move |_|navigate(1)) {Icon(icon=icons::HISTORY) text {"History"}}
                    if move ||cfg!(any(target_os="android",target_os="ios")) {
                        Button(variant=ButtonVariant::Ghost,class:tab-selected=move ||s.friends_open.get()||s.places_open.get(),a11y:label="Friends",on:click=move |_|navigate(2)) {Icon(icon=icons::PEOPLE) text {"Friends"}}
                    }
                    Button(variant=ButtonVariant::Ghost,class:tab-selected=move ||s.settings_open.get(),a11y:label="Settings",on:click=move |_|navigate(3)) {Icon(icon=icons::SLIDERS) text {"Settings"}}
                } }
            }
        }
    }
}

/// Location sharing exists on Android and iOS only.
#[component]
fn FriendsSheet() -> impl IntoView {
    #[cfg(any(target_os = "android", target_os = "ios"))]
    return friends::sheet();
    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    view! {box {}}
}

/// Night map: mid-dark cool slate surfaces (lifted from near-black for softer contrast); blue for actions and selection,
/// apart from map data colors (green traveled, red untraveled, purple area).
fn dark_theme() -> Theme {
    Theme::dark().with_css(
        "--zui-color-background: oklch(0.225 0.014 255);
         --zui-color-foreground: oklch(0.95 0.006 255);
         --zui-color-card: oklch(0.27 0.016 255);
         --zui-color-card-foreground: oklch(0.95 0.006 255);
         --zui-color-popover: oklch(0.28 0.017 255);
         --zui-color-popover-foreground: oklch(0.95 0.006 255);
         --zui-color-primary: oklch(0.55 0.17 262);
         --zui-color-primary-foreground: oklch(0.99 0.005 262);
         --zui-color-secondary: oklch(0.35 0.02 255);
         --zui-color-secondary-foreground: oklch(0.95 0.006 255);
         --zui-color-muted: oklch(0.315 0.017 255);
         --zui-color-muted-foreground: oklch(0.76 0.018 255);
         --zui-color-accent: oklch(0.38 0.07 262);
         --zui-color-accent-foreground: oklch(0.86 0.08 262);
         --zui-color-border: oklch(0.37 0.018 255);
         --zui-color-input: oklch(0.40 0.018 255);
         --zui-color-ring: oklch(0.68 0.15 262);
         --zui-radius-base: 12px;",
    )
}

/// Day map: near-white cool surfaces, same blue darkened for contrast. Accent (selection) is a
/// clear blue tint: a paler one vanishes against the grey segment tracks.
fn light_theme() -> Theme {
    Theme::light().with_css(
        "--zui-color-background: oklch(0.985 0.004 250);
         --zui-color-foreground: oklch(0.22 0.02 255);
         --zui-color-card: oklch(1 0 0);
         --zui-color-card-foreground: oklch(0.22 0.02 255);
         --zui-color-popover: oklch(1 0 0);
         --zui-color-popover-foreground: oklch(0.22 0.02 255);
         --zui-color-primary: oklch(0.52 0.19 262);
         --zui-color-primary-foreground: oklch(0.99 0.005 262);
         --zui-color-secondary: oklch(1 0 0);
         --zui-color-secondary-foreground: oklch(0.25 0.02 255);
         --zui-color-muted: oklch(0.955 0.007 250);
         --zui-color-muted-foreground: oklch(0.5 0.02 255);
         --zui-color-accent: oklch(0.88 0.07 262);
         --zui-color-accent-foreground: oklch(0.38 0.17 262);
         --zui-color-border: oklch(0.87 0.012 250);
         --zui-color-input: oklch(0.83 0.014 250);
         --zui-color-ring: oklch(0.55 0.17 262);
         --zui-radius-base: 12px;",
    )
}

#[cfg(test)]
mod tests;

fn window_icon() -> WindowIcon {
    let icon = image::load_from_memory(include_bytes!("../../assets/logo.png"))
        .expect("Bundled Roamlog icon is valid PNG")
        .resize_exact(128, 128, image::imageops::FilterType::Lanczos3)
        .into_rgba8();
    WindowIcon::from_rgba(icon.into_raw(), 128, 128).expect("RGBA icon dimensions")
}

pub fn run(directory: PathBuf) -> Result<(), zgui::Error> {
    std::fs::create_dir_all(&directory).expect("Roamlog data directory is writable");
    let app = app().with_title("Roamlog");
    // winit on iOS makes the UIWindow exactly this size; leaving it unset fills the screen.
    #[cfg(not(target_os = "ios"))]
    let app = app.with_size(460.0, 900.0);
    app.with_icon(window_icon())
        .with_stylesheet(include_str!("style.css"))
        .with_context(move || provide_local_context(UiState::new(directory.clone())))
        .run(|| view! {Roamlog()})
}

#[cfg(target_os = "android")]
pub(crate) fn run_android(android_app: zgui::app::AndroidApp, directory: PathBuf) {
    // Same NativeActivity driver and lifecycle entry used by local Zgui mobile lab.
    app()
        .with_title("Roamlog")
        .with_application_id("dev.traveler")
        .with_stylesheet(include_str!("style.css"))
        .with_context(move || provide_local_context(UiState::new(directory.clone())))
        .run_on(
            move |handler| zgui::app::run_android(android_app, handler),
            || view! {Roamlog()},
        )
        .expect("Roamlog Android app runs");
}
