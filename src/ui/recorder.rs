use super::{history::HistoryProps, state::UiState};
use crate::model::Status;
use serde_json::json;
use zgui::prelude::*;
use zgui_ui::prelude::*;
use zgui_ui_icons::{prelude::*, set::mark::PLUS};

#[component]
pub fn Recorder() -> impl IntoView {
    let s = UiState::current();
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
        column(class="trip-bar") {
            row(class="metric-row") {
                text(class="trip-distance") {{move ||s.data.with(|d|d.active().map_or_else(||"Ready".into(),|t|s.distance(t.distance_m)))}}
                text(class="trip-clock") {{move || {let now=s.now.get();s.data.with(|d|d.active().map_or_else(String::new,|t|{let seconds=t.elapsed_at(now)/1000;format!("{}:{:02}:{:02}",seconds/3600,seconds/60%60,seconds%60)}))}}}
                spacer()
                text(class="muted") {{move ||match s.status(){Some(Status::Recording)=>"Recording",Some(Status::Paused)=>"Paused",_=>"Not recording"}}}
            }
            row(class="category-row") {
                Select(value=s.selected) {
                    SelectTrigger(disabled=Signal::derive_local(move ||s.status().is_some()),class="category-select",label="Trip category") {SelectValue()}
                    SelectContent {
                        SelectItem(value="0") {"Uncategorized"}
                        for category in move ||s.data.with(|d|d.categories.clone()),key=|c|c.id {
                            SelectItem(value=category.id.to_string()) {{category.name}}
                        }
                    }
                }
                Dialog(open=s.dialog) {
                    DialogTrigger(variant=ButtonVariant::Outline,class="icon-button new-category",a11y:label="New category") {Icon(icon=PLUS)}
                    DialogContent(class="category-dialog") {
                        DialogHeader {DialogTitle {"New category"} DialogDescription {"Organize trips your way. Road history includes every category."}}
                        Input(value=s.draft,label="Category name",placeholder="Walking, running, motorcycle…")
                        DialogFooter {DialogClose(variant=ButtonVariant::Outline) {"Cancel"} Button(on:click=create) {"Create category"}}
                    }
                }
            }
            row(class="trip-actions") {
                Button(class="start-trip",disabled=Signal::derive_local(move ||s.status()==Some(Status::Recording)),on:click=move |_|s.record(if s.status()==Some(Status::Paused){"resume"}else{"start"})) {{move ||if s.status()==Some(Status::Paused){"Resume"}else{"Start"}}}
                Button(variant=ButtonVariant::Outline,disabled=Signal::derive_local(move ||s.status()!=Some(Status::Recording)),on:click=move |_|s.record("pause")) {"Pause"}
                Button(variant=ButtonVariant::Outline,disabled=Signal::derive_local(move ||s.status().is_none()),on:click=move |_|s.record("finish")) {"Finish"}
                History()
            }
        }
    }
}
