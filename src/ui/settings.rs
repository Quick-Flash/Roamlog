//! App settings: appearance, units, offline map, data (backup, restore, GPX).
use super::{state::UiState, update_controls::UpdateControlsProps};
use zgui::prelude::*;
use zgui_ui::prelude::*;

#[component]
pub fn Settings() -> impl IntoView {
    let s = UiState::current();
    // Size picked for a manual download, so its button reads as chosen.
    let size = RwSignal::new_local(0_u32);
    let plan = move |km: u32| {
        size.set(km);
        s.plan_download(f64::from(km));
    };
    view! {
        if move ||s.settings_open.get() {
            column(class="tab-page settings-page") {
                text(class="page-title") {"Settings"}
                scroll(class="sheet-scroll settings-scroll") {
                    column(class="tab-body") {
                        text(class="section-title") {"Appearance"}
                        row(class="segmented") {
                            if move ||s.dark.get() {
                                Button(variant=ButtonVariant::Ghost,class="theme-light",on:click=move |_|s.set_dark(false)) {"Light"}
                                Button(variant=ButtonVariant::Secondary,class="theme-dark seg-on",on:click=move |_|s.set_dark(true)) {"Dark"}
                            } else {
                                Button(variant=ButtonVariant::Secondary,class="theme-light seg-on",on:click=move |_|s.set_dark(false)) {"Light"}
                                Button(variant=ButtonVariant::Ghost,class="theme-dark",on:click=move |_|s.set_dark(true)) {"Dark"}
                            }
                        }
                        text(class="section-title") {"Units"}
                        row(class="segmented") {
                            if move ||s.metric.get() {
                                Button(variant=ButtonVariant::Secondary,class="unit seg-on",on:click=move |_|s.set_units(true)) {"Kilometers"}
                                Button(variant=ButtonVariant::Ghost,class="unit",on:click=move |_|s.set_units(false)) {"Miles"}
                            } else {
                                Button(variant=ButtonVariant::Ghost,class="unit",on:click=move |_|s.set_units(true)) {"Kilometers"}
                                Button(variant=ButtonVariant::Secondary,class="unit seg-on",on:click=move |_|s.set_units(false)) {"Miles"}
                            }
                        }
                        text(class="section-title") {"Offline map"}
                        text(class="muted") {"Needed for road coverage and offline use. Only missing tiles download."}
                        text(class="muted") {"Download automatically where you look and along shown trips:"}
                        row(class="segmented") {
                            Button(variant=ButtonVariant::Ghost,class:seg-on=move ||s.auto_map_mode.get()=="off",on:click=move |_|s.set_auto_map("off")) {"Off"}
                            Button(variant=ButtonVariant::Ghost,class:seg-on=move ||s.auto_map_mode.get()=="wifi",on:click=move |_|s.set_auto_map("wifi")) {"Wi‑Fi only"}
                            Button(variant=ButtonVariant::Ghost,class:seg-on=move ||s.auto_map_mode.get()=="any",on:click=move |_|s.set_auto_map("any")) {"Any network"}
                        }
                        text(class="muted") {"Or download a square around map center now:"}
                        row(class="segmented") {
                            Button(variant=ButtonVariant::Ghost,class:seg-on=move ||size.get()==10,on:click=move |_|plan(10)) {"10 km"}
                            Button(variant=ButtonVariant::Ghost,class:seg-on=move ||size.get()==30,on:click=move |_|plan(30)) {"30 km"}
                            Button(variant=ButtonVariant::Ghost,class:seg-on=move ||size.get()==80,on:click=move |_|plan(80)) {"80 km"}
                        }
                        text(class="muted") {{move ||{let info=s.plan_info.get();if info.is_empty(){"Pick a size to see how much it downloads.".to_string()}else{info}}}}
                        row(class="actions") {
                            Button(variant=ButtonVariant::Outline,on:click=move |_|{s.settings_open.set(false);s.import_basemap()}) {"Import file"}
                            Button(class="download-map",disabled=Signal::derive_local(move ||!s.plan_ready.get()),on:click=move |_|s.start_download()) {"Download"}
                        }
                        text(class="section-title") {"Data"}
                        text(class="muted") {"Backup holds all trips, categories and areas. Restore replaces them and keeps current data aside. GPX exports trips for other apps."}
                        row(class="actions") {
                            Button(variant=ButtonVariant::Outline,class="backup",on:click=move |_|s.backup()) {"Back up"}
                            Button(variant=ButtonVariant::Outline,class="restore",disabled=Signal::derive_local(move ||s.status().is_some()),on:click=move |_|if s.confirm_restore.get_untracked(){s.restore()}else{s.confirm_restore.set(true)}) {{move ||if s.confirm_restore.get(){"Confirm"}else{"Restore"}}}
                            Button(variant=ButtonVariant::Outline,on:click=move |_|s.export_gpx()) {"GPX"}
                        }
                        if move ||cfg!(any(target_os="android",target_os="ios")) {
                            text(class="section-title") {"About"}
                            Button(variant=ButtonVariant::Outline,on:click=move |_|s.open_settings()) {{move ||if cfg!(target_os="ios"){"iOS app permissions"}else{"Android app permissions"}}}
                            if move ||cfg!(target_os="android") {UpdateControls()}
                        }
                    }
                }
            }
        }
    }
}
