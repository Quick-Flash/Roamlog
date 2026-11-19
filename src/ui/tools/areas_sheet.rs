use super::shape_preview;
use crate::ui::state::UiState;
use zgui::prelude::*;
use zgui_ui::prelude::*;

/// Saved areas: new, show all on map, open one (measures it).
#[component]
pub fn AreasSheet() -> impl IntoView {
    let s = UiState::current();
    let percent = move |id: u64| {
        s.area_percent
            .with(|p| p.get(&id).map(|v| format!(" · {v:.0}% traveled")))
            .unwrap_or_default()
    };
    let empty = move || s.data.with(|d| d.areas.is_empty());
    view! {
        Dialog(open=s.areas_open) {
            DialogContent(class="sheet") {
                DialogHeader {DialogTitle {"Areas"} DialogDescription {"Share of roads you have traveled inside an area."}}
                row(class="actions") {
                    Button(class="new-area",on:click=move |_|s.new_area()) {"+ New area"}
                    Button(variant=ButtonVariant::Outline,class="show-areas",disabled=Signal::derive_local(empty),on:click=move |_|s.show_areas()) {"Show all on map"}
                }
                if move ||s.data.with(|d|d.areas.is_empty()) {
                    column(class="alert-card") {
                        text(class="list-title") {"No areas yet"}
                        text(class="muted") {"Tap + New area, place a circle or draw a shape on the map, then name it."}
                    }
                }
                scroll(class="sheet-scroll") {
                    column(class="list") {
                        for area in move ||s.data.with(|d|d.areas.clone()),key=|a|(a.id,a.name.clone(),a.points.len(),a.circle.map(|c|c.1.to_bits())) {
                            Button(variant=ButtonVariant::Ghost,class="alert-card alert-card-button",a11y:label=format!("Open {}",area.name),on:click=move |_|s.show_area(area.id)) {
                                row(class="page-heading") {
                                    {shape_preview(area.circle.map_or(area.points[0],|c|c.0),area.points.clone(),[52.,52.])}
                                    column(class="grow") {
                                        text(class="list-title") {{area.name.clone()}}
                                        text(class="muted") {{move ||format!("{}{}",match area.circle {
                                            Some((_,r))=>format!("Circle · {r:.0} m radius"),
                                            None=>format!("{} corners",area.points.len()),
                                        },percent(area.id))}}
                                    }
                                    text(class="chevron") {"›"}
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}
