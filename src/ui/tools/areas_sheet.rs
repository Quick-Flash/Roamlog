use crate::ui::state::UiState;
use zgui::prelude::*;
use zgui_ui::prelude::*;

/// Saved areas: draw new, show (measures it), delete.
#[component]
pub fn AreasSheet() -> impl IntoView {
    let s = UiState::current();
    let percent = move |id: u64| {
        s.area_percent.with(|p| {
            p.get(&id)
                .map_or_else(|| "Tap to measure".into(), |v| format!("{v:.0}% traveled"))
        })
    };
    view! {
        Dialog(open=s.areas_open) {
            DialogTrigger(variant=ButtonVariant::Secondary,class="map-chip") {"Areas"}
            DialogContent(class="sheet") {
                DialogHeader {DialogTitle {"Areas"} DialogDescription {"Share of roads you have traveled inside an area."}}
                Button(class="new-area wide",on:click=move |_|{s.areas_open.set(false);s.start_area()}) {"Draw new area"}
                if move ||s.area_stats.with(Option::is_some) {
                    Button(variant=ButtonVariant::Secondary,class="wide",on:click=move |_|{s.areas_open.set(false);s.area_open.set(true)}) {"Current area details"}
                }
                text(class="section-title") {"Saved"}
                scroll(class="sheet-scroll") {
                    column(class="list") {
                        for area in move ||s.data.with(|d|d.areas.clone()),key=|a|a.id {
                            row(class="list-row") {
                                column(class="list-main",on:click=move |_|s.show_area(area.id)) {
                                    text(class="list-title") {{area.name.clone()}}
                                    text(class="muted") {{move ||format!("{} corners · {}",area.points.len(),percent(area.id))}}
                                }
                                Button(variant=ButtonVariant::Ghost,class="row-action",on:click=move |_|s.show_area(area.id)) {"Show"}
                                Button(variant=ButtonVariant::Ghost,class="row-action",on:click=move |_|if s.confirm_area_delete.get_untracked()==Some(area.id){s.confirm_area_delete.set(None);s.delete_area(area.id)}else{s.confirm_area_delete.set(Some(area.id))}) {{move ||if s.confirm_area_delete.get()==Some(area.id){"Confirm"}else{"Delete"}}}
                            }
                        }
                        if move ||s.data.with(|d|d.areas.is_empty()) {
                            text(class="empty") {"No saved areas yet. Draw one, then choose Save area."}
                        }
                    }
                }
            }
        }
    }
}
