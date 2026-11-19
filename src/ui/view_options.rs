use super::icons::LAYERS;
use super::{history::PersonPickerProps, state::UiState};
use zgui::prelude::*;
use zgui_ui::prelude::*;
use zgui_ui_icons::prelude::*;

/// Map layers: background and trip overlays.
#[component]
pub fn ViewOptions() -> impl IntoView {
    let s = UiState::current();
    let three_d = s.terrain_enabled;
    let online_label = NodeRef::new();
    let gps_label = NodeRef::new();
    let roads_label = NodeRef::new();
    let heat_label = NodeRef::new();
    let friends_label = NodeRef::new();
    let open = RwSignal::new_local(false);
    let show_alert_areas = move || {
        open.set(false);
        #[cfg(any(target_os = "android", target_os = "ios"))]
        super::place_alerts::show_on_map(s, None);
    };
    view! {
        Dialog(open=open) {
            DialogTrigger(variant=ButtonVariant::Secondary,class="map-chip layers-button",a11y:label="Layers and areas") {Icon(icon=LAYERS)}
            DialogContent(class="sheet") {
                DialogHeader {DialogTitle {"Layers"} DialogDescription {"What the map shows. Saved on this device."}}
                text(class="section-title") {"Background"}
                row(class="segmented") {
                    Button(variant=ButtonVariant::Ghost,class="layer-map",class:seg-on=move ||!three_d.get()&&!s.satellite.get(),on:click=move |_|{three_d.set(false);s.set_satellite(false)}) {"Map"}
                    Button(variant=ButtonVariant::Ghost,class="layer-satellite",class:seg-on=move ||!three_d.get()&&s.satellite.get(),on:click=move |_|{three_d.set(false);s.set_satellite(true)}) {"Satellite"}
                    Button(variant=ButtonVariant::Ghost,class:seg-on=move ||three_d.get(),disabled=Signal::derive_local(move ||s.area_drawing.get()),on:click=move |_|three_d.set(true)) {"3D terrain"}
                }
                if move ||three_d.get()||s.satellite.get() {
                    text(class="section-title") {"Imagery"}
                    row(class="segmented") {
                        for provider in move ||super::state::IMAGERY.to_vec(),key=|p|*p {
                            Button(variant=ButtonVariant::Ghost,class="imagery-option",class:imagery-google=move ||provider==crate::mapping::tiles::Provider::GoogleSatellite,class:seg-on=move ||s.imagery.get()==provider,on:click=move |_|s.set_imagery(provider)) {{imagery_label(provider)}}
                        }
                    }
                }
                row(class="view-toggle") {
                    Label(node_ref=online_label) {"Load map tiles online"}
                    Switch(labelled_by=online_label,checked=Binding::controlled(
                        Signal::derive_local(move ||!s.map_offline.get()),move |enabled: bool|s.map_offline.set(!enabled)))
                }
                text(class="section-title") {"Trips"}
                PersonPicker()
                row(class="view-toggle") {
                    column(class="toggle-text") {
                        Label(node_ref=roads_label) {"Traveled roads"}
                        text(class="muted") {"Wide lines on roads you have covered."}
                    }
                    Switch(class="show-roads",labelled_by=roads_label,checked=Binding::controlled(s.show_roads,move |value|s.set_show_roads(value)))
                }
                row(class="view-toggle") {
                    column(class="toggle-text") {
                        Label(node_ref=heat_label) {"Color by trips on road"}
                        text(class="muted") {"Green 1 · blue 2 · yellow 3–4 · orange 5+"}
                    }
                    Switch(class="show-heatmap",labelled_by=heat_label,checked=Binding::controlled(s.heatmap,move |value|s.set_heatmap(value)))
                }
                row(class="view-toggle") {
                    column(class="toggle-text") {
                        Label(node_ref=gps_label) {"GPS paths"}
                        text(class="muted") {"Thin lines; dashes mark inferred gaps."}
                    }
                    Switch(class="show-gps",labelled_by=gps_label,checked=Binding::controlled(s.show_gps,move |value|s.set_show_gps(value)))
                }
                if move ||cfg!(any(target_os="android",target_os="ios")) {
                    text(class="section-title") {"Friends"}
                    row(class="view-toggle") {
                        column(class="toggle-text") {
                            Label(node_ref=friends_label) {"Friend locations"}
                            text(class="muted") {"Positions friends share with you. Gray: not live."}
                        }
                        Switch(class="show-friends",labelled_by=friends_label,checked=Binding::controlled(s.show_friends,move |value|s.set_show_friends(value)))
                    }
                    text(class="section-title") {"Alert areas"}
                    row(class="actions") {
                        Button(variant=ButtonVariant::Outline,on:click=move |_|{open.set(false);s.place_peer.set(None);s.places_open.set(true)}) {"Alert areas"}
                        Button(variant=ButtonVariant::Outline,on:click=move |_|show_alert_areas()) {"Show on map"}
                    }
                }
                text(class="section-title") {"Areas"}
                row(class="actions") {
                    Button(variant=ButtonVariant::Outline,on:click=move |_|{open.set(false);s.areas_open.set(true)}) {"Areas"}
                    Button(variant=ButtonVariant::Outline,on:click=move |_|{open.set(false);s.show_areas()}) {"Show on map"}
                }
            }
        }
    }
}

/// Short name for the imagery picker.
fn imagery_label(provider: crate::mapping::tiles::Provider) -> &'static str {
    use crate::mapping::tiles::Provider;
    match provider {
        Provider::GoogleSatellite => "Google",
        Provider::BingSatellite => "Bing",
        _ => "Esri",
    }
}
