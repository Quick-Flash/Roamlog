mod history;
mod history_dates;
mod history_trip;
pub(crate) mod icons;
mod recorder;
pub(crate) mod settings;
mod snapshot;
pub(crate) mod state;
pub(crate) mod tools;
pub(crate) mod view_options;

use crate::mapping::view::MapViewProps;
use recorder::RecorderProps;
use state::{UiState, now_ms};
use std::{path::PathBuf, time::Duration};
use zgui::prelude::*;
use zgui_ui_tokens::prelude::*;

#[component]
pub fn Roamlog() -> impl IntoView {
    let s = UiState::current();
    let tick = StoredValue::new_local(0_u64);
    s.refresh(true);
    s.reload_basemap();
    let timer = set_interval(Duration::from_secs(1), move || {
        s.now.set(now_ms());
        tick.update_value(|t| *t += 1);
        s.refresh(tick.get_value().is_multiple_of(5));
        s.download_status();
        s.start_position();
    });
    on_cleanup_local(move || drop(timer));
    #[cfg(target_os = "android")]
    {
        let bars = zgui::reactive::RenderEffect::new(move |_| {
            zgui::app::android::system_bar_theme(Some(s.dark.get()))
        });
        on_cleanup_local(move || drop(bars));
    }
    view! {
        ThemeProvider(light=Signal::stored_local(light_theme()),dark=Signal::stored_local(dark_theme()),scheme=Signal::derive_local(move ||if s.dark.get(){ColorScheme::Dark}else{ColorScheme::Light})) {
            column(class="roamlog") {
                MapView()
                Recorder()
            }
        }
    }
}

/// Night map: cool slate surfaces matching dark basemap; blue for actions and selection,
/// apart from map data colors (green traveled, red untraveled, purple area).
fn dark_theme() -> Theme {
    Theme::dark().with_css(
        "--zui-color-background: oklch(0.165 0.012 255);
         --zui-color-foreground: oklch(0.95 0.006 255);
         --zui-color-card: oklch(0.205 0.014 255);
         --zui-color-card-foreground: oklch(0.95 0.006 255);
         --zui-color-popover: oklch(0.215 0.015 255);
         --zui-color-popover-foreground: oklch(0.95 0.006 255);
         --zui-color-primary: oklch(0.55 0.17 262);
         --zui-color-primary-foreground: oklch(0.99 0.005 262);
         --zui-color-secondary: oklch(0.29 0.018 255);
         --zui-color-secondary-foreground: oklch(0.95 0.006 255);
         --zui-color-muted: oklch(0.25 0.015 255);
         --zui-color-muted-foreground: oklch(0.72 0.018 255);
         --zui-color-accent: oklch(0.32 0.06 262);
         --zui-color-accent-foreground: oklch(0.86 0.08 262);
         --zui-color-border: oklch(0.31 0.016 255);
         --zui-color-input: oklch(0.34 0.016 255);
         --zui-color-ring: oklch(0.68 0.15 262);
         --zui-radius-base: 12px;",
    )
}

/// Day map: near-white cool surfaces, same blue darkened for contrast.
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
         --zui-color-accent: oklch(0.94 0.035 262);
         --zui-color-accent-foreground: oklch(0.42 0.15 262);
         --zui-color-border: oklch(0.9 0.01 250);
         --zui-color-input: oklch(0.88 0.01 250);
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
    app()
        .with_title("Roamlog")
        .with_size(460.0, 900.0)
        .with_icon(window_icon())
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
