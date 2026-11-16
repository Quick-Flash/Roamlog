//! App settings: appearance, units, offline map, data (backup, restore, GPX).
use super::{state::UiState, update_controls::UpdateControlsProps};
use zgui::prelude::*;
use zgui_ui::prelude::*;

#[component]
pub fn Settings() -> impl IntoView {
    let s = UiState::current();
    view! {
        Dialog(open=s.settings_open) {
            DialogContent(class="sheet") {
                DialogHeader {DialogTitle {"Settings"} DialogDescription {"Saved on this device."}}
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
                        text(class="muted") {"Road coverage and offline use need offline map. Areas you already have are kept; only missing tiles download."}
                        text(class="muted") {"Download automatically where you look and along trips shown in History."}
                        row(class="segmented") {
                            Button(variant=ButtonVariant::Ghost,class:seg-on=move ||s.auto_map_mode.get()=="off",on:click=move |_|s.set_auto_map("off")) {"Off"}
                            Button(variant=ButtonVariant::Ghost,class:seg-on=move ||s.auto_map_mode.get()=="wifi",on:click=move |_|s.set_auto_map("wifi")) {"Wi‑Fi only"}
                            Button(variant=ButtonVariant::Ghost,class:seg-on=move ||s.auto_map_mode.get()=="any",on:click=move |_|s.set_auto_map("any")) {"Any network"}
                        }
                        text(class="muted") {"Or download a square around map center now:"}
                        row(class="segmented") {
                            Button(variant=ButtonVariant::Ghost,on:click=move |_|s.plan_download(10.0)) {"10 km"}
                            Button(variant=ButtonVariant::Ghost,on:click=move |_|s.plan_download(30.0)) {"30 km"}
                            Button(variant=ButtonVariant::Ghost,on:click=move |_|s.plan_download(80.0)) {"80 km"}
                        }
                        text(class="muted") {{move ||{let info=s.plan_info.get();if info.is_empty(){"Pick size to check download size.".to_string()}else{info}}}}
                        row(class="actions") {
                            Button(variant=ButtonVariant::Outline,on:click=move |_|{s.settings_open.set(false);s.import_basemap()}) {"Import file"}
                            Button(class="download-map",disabled=Signal::derive_local(move ||!s.plan_ready.get()),on:click=move |_|s.start_download()) {"Download"}
                        }
                        if move ||cfg!(target_os="android") {UpdateControls()}
                        text(class="section-title") {"Data"}
                        text(class="muted") {"Backup holds every trip, category, edit and saved area. Restore replaces them; current data is kept aside. GPX exports trips for other apps."}
                        row(class="actions") {
                            Button(variant=ButtonVariant::Outline,class="backup",on:click=move |_|s.backup()) {"Back up"}
                            Button(variant=ButtonVariant::Outline,class="restore",disabled=Signal::derive_local(move ||s.status().is_some()),on:click=move |_|if s.confirm_restore.get_untracked(){s.restore()}else{s.confirm_restore.set(true)}) {{move ||if s.confirm_restore.get(){"Confirm"}else{"Restore"}}}
                            Button(variant=ButtonVariant::Outline,on:click=move |_|s.export_gpx()) {"GPX"}
                        }
                    }
                }
            }
        }
    }
}
