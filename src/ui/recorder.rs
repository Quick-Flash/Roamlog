use super::state::UiState;
use crate::model::Status;
use serde_json::json;
use zgui::prelude::*;
use zgui_ui::prelude::*;

#[component]
pub fn Recorder() -> impl IntoView {
    let s = UiState::current();
    // "New category…" lives in the dropdown: picking it opens the dialog and keeps the
    // previous choice selected.
    let chosen = StoredValue::new_local(s.selected.get_untracked());
    let pick = zgui::reactive::RenderEffect::new(move |_| {
        let value = s.selected.get();
        if value == "new" {
            s.selected.set(chosen.get_value());
            s.dialog.set(true);
        } else {
            chosen.set_value(value);
        }
    });
    on_cleanup_local(move || drop(pick));
    let create = move |_: &mut EventCx<'_, events::Click>| {
        let name = s.draft.get_untracked();
        spawn_local(async move {
            match s.request(json!({"op":"category","name":name})).await {
                Ok(_) => {
                    s.dialog.set(false);
                    s.draft.set(String::new());
                    s.message.set(String::new());
                    s.refresh(false);
                }
                Err(error) => s.message.set(error),
            }
        });
    };
    view! {
        row(class="trip-bar") {
            if move ||s.status().is_some() {
            column(class="trip-metrics") {
            row(class="metric-row") {
                box(class="record-dot",class:paused=move ||s.status()==Some(Status::Paused)) {}
                text(class="trip-distance") {{move ||s.data.with(|d|d.active().map_or_else(||"Ready".into(),|t|s.distance(t.distance_m)))}}
                text(class="trip-clock") {{move || {let now=s.now.get();s.data.with(|d|d.active().map_or_else(String::new,|t|{let seconds=t.elapsed_at(now)/1000;format!("{}:{:02}:{:02}",seconds/3600,seconds/60%60,seconds%60)}))}}}
            }
                text(class="muted trip-state") {{move ||s.data.with(|data|data.active().map(|trip|{
                    let category=data.categories.iter().find(|c|Some(c.id)==trip.category_id).map_or("Uncategorized",|c|c.name.as_str());
                    if trip.status==Status::Paused {format!("{category} · Paused")} else {format!("{category} · {} new",s.distance(s.new_roads(trip.id)))}
                }).unwrap_or_default())}}
            }
            } else {
            row(class="category-row") {
                Select(value=s.selected) {
                    SelectTrigger(disabled=Signal::derive_local(move ||s.status().is_some()),class="category-select",label="Trip category") {SelectValue()}
                    SelectContent {
                        SelectItem(value="0") {"Uncategorized"}
                        for category in move ||s.data.with(|d|d.categories.clone()),key=|c|c.id {
                            SelectItem(value=category.id.to_string()) {{category.name}}
                        }
                        SelectItem(value="new",class="new-category") {"New category…"}
                    }
                }
                Dialog(open=s.dialog) {
                    DialogContent(class="category-dialog") {
                        DialogHeader {DialogTitle {"New category"} DialogDescription {"Organize trips your way. Road history includes every category."}}
                        Input(value=s.draft,label="Category name",placeholder="Walking, running, motorcycle…")
                        DialogFooter {DialogClose(variant=ButtonVariant::Outline) {"Cancel"} Button(on:click=create) {"Create category"}}
                    }
                }
            }
            }
            // Only actions valid now: idle Start; recording Pause + Finish; paused Resume + Finish.
            row(class="trip-actions") {
                if move ||s.status()==Some(Status::Recording) {
                    Button(variant=ButtonVariant::Outline,class="pause-trip",on:click=move |_|s.record("pause"),a11y:label="Pause") {"Ⅱ"}
                } else {
                    Button(class="start-trip",on:click=move |_|s.record(if s.status()==Some(Status::Paused){"resume"}else{"start"})) {{move ||if s.status()==Some(Status::Paused){"Resume"}else{"Start trip"}}}
                }
                if move ||s.status().is_some() {
                    Button(variant=ButtonVariant::Outline,class="finish-trip",on:click=move |_|s.record("finish")) {"Finish"}
                }
            }
        }
    }
}
