use super::{
    history::{age, duration},
    state::UiState,
};
use crate::model::{Status, TripFilter};
use std::collections::BTreeSet;
use zgui::prelude::*;
use zgui_ui::prelude::*;

/// Checkbox and label share whole row's selection target; Edit remains separate.
#[component]
pub fn HistoryTrip(id: u64, selected: RwSignal<BTreeSet<u64>, LocalStorage>) -> impl IntoView {
    let s = UiState::current();
    let checkbox = NodeRef::new();
    let title = NodeRef::new();
    view! {
        row(class="list-row",class:trip-selected=move ||selected.with(|ids|ids.contains(&id))) {
            Checkbox(node_ref=checkbox,labelled_by=title,class=format!("select-trip select-trip-{id}"),checked=Binding::controlled(
                Signal::derive_local(move ||if selected.with(|ids|ids.contains(&id)){Checked::Yes}else{Checked::No}),
                move |value|selected.update(|ids|{if value==Checked::Yes {ids.insert(id);}else{ids.remove(&id);}})
            ))
            Label(control=checkbox,class="list-main trip-select-label",on:click=move |_|selected.update(|ids|{if !ids.remove(&id){ids.insert(id);}})) {
                row(class="trip-title") {
                    text(node_ref=title,class="list-title") {{move ||s.data.with(|d|d.trips.iter().find(|t|t.id==id).map(|t|t.title()).unwrap_or_default())}}
                    spacer()
                    text(class="muted") {{move ||s.data.with(|d|d.trips.iter().find(|t|t.id==id).map(|t|age(s.now.get(),t.started_ms)).unwrap_or_default())}}
                }
                text(class="muted") {{move ||s.data.with(|d|{
                    let Some(t)=d.trips.iter().find(|t|t.id==id) else {return String::new()};
                    let category=d.categories.iter().find(|c|Some(c.id)==t.category_id).map_or("Uncategorized",|c|c.name.as_str());
                    format!("{category} · {} · {}",s.distance(t.distance_m),duration(t.elapsed_at(s.now.get())))
                })}}
                text(class="muted") {{move ||s.data.with(|d|d.trips.iter().find(|t|t.id==id).map(|t|{
                    let inferred=if t.estimated_distance_m>0.0 {format!("{} inferred",s.distance(t.estimated_distance_m))}else{String::new()};
                    let new=if s.coverage_filter.get()==TripFilter::All&&t.status==Status::Finished {s.sections.with(|_|format!(" · {} new roads",s.distance(s.new_roads(id))))}else{String::new()};
                    let state=if t.status==Status::Finished {String::new()}else{" · in progress".into()};
                    format!("{inferred}{new}{state}").trim_start_matches(" · ").to_string()
                }).unwrap_or_default())}}
            }
            if move ||s.people.with(Vec::is_empty) {
                Button(variant=ButtonVariant::Ghost,class=format!("row-action edit-trip edit-trip-{id}"),on:click=move |_|s.edit_trip(id),a11y:label=format!("Edit trip #{id}")) {"Edit"}
            }
        }
    }
}
