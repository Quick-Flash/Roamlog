use super::state::UiState;
use zgui::prelude::*;
use zgui_ui::prelude::*;

/// Map layers: background and trip overlays.
#[component]
pub fn ViewOptions() -> impl IntoView {
    let s = UiState::current();
    let gps_label = NodeRef::new();
    let roads_label = NodeRef::new();
    let heat_label = NodeRef::new();
    view! {
        Dialog {
            DialogTrigger(variant=ButtonVariant::Secondary,class="map-chip") {"Layers"}
            DialogContent(class="sheet") {
                DialogHeader {DialogTitle {"Layers"} DialogDescription {"What the map shows. Saved on this device."}}
                text(class="section-title") {"Background"}
                row(class="segmented") {
                    if move ||s.satellite.get() {
                        Button(variant=ButtonVariant::Ghost,class="layer-map",on:click=move |_|s.set_satellite(false)) {"Map"}
                        Button(variant=ButtonVariant::Secondary,class="layer-satellite seg-on",on:click=move |_|s.set_satellite(true)) {"Satellite"}
                    } else {
                        Button(variant=ButtonVariant::Secondary,class="layer-map seg-on",on:click=move |_|s.set_satellite(false)) {"Map"}
                        Button(variant=ButtonVariant::Ghost,class="layer-satellite",on:click=move |_|s.set_satellite(true)) {"Satellite"}
                    }
                }
                text(class="section-title") {"Trips"}
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
                text(class="muted") {"Tap a traveled road for trips and first date. Ambiguous routes never mark guessed roads."}
            }
        }
    }
}
