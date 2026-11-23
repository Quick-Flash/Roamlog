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
    // Friend's trip copied into own history, under own category of the same name.
    let added = RwSignal::new_local(false);
    let add = move || {
        added.set(true);
        zgui::reactive::spawn_detached(async move {
            match s
                .request(serde_json::json!({"op":"import_view_trip","trip_id":id}))
                .await
            {
                Ok(reply) => {
                    if let (Some(trip), Some(peer)) =
                        (reply["trip_id"].as_u64(), reply["peer"].as_str())
                    {
                        super::trip_sources::add(&s.directory.get_value(), trip, peer);
                    }
                    s.message.set("Trip added to your history".into());
                }
                Err(error) => {
                    added.set(false);
                    s.message.set(error);
                }
            }
        });
    };
    view! {
        row(class="list-row",class:trip-selected=move ||selected.with(|ids|ids.contains(&id))) {
            Checkbox(node_ref=checkbox,labelled_by=title,class=format!("select-trip select-trip-{id}"),checked=Binding::controlled(
                Signal::derive_local(move ||if selected.with(|ids|ids.contains(&id)){Checked::Yes}else{Checked::No}),
                move |value|selected.update(|ids|{if value==Checked::Yes {ids.insert(id);}else{ids.remove(&id);}})
            ))
            Label(control=checkbox,class="list-main trip-select-label",on:click=move |_|selected.update(|ids|{if !ids.remove(&id){ids.insert(id);}})) {
                // Title: trip name if set, else category; meta: distance, time, new roads.
                row(class="trip-title") {
                    text(node_ref=title,class="list-title") {{move ||s.data.with(|d|d.trips.iter().find(|t|t.id==id).map(|t|if t.name.is_empty(){
                        d.categories.iter().find(|c|Some(c.id)==t.category_id).map_or("Uncategorized".into(),|c|c.name.clone())
                    }else{t.name.clone()}).unwrap_or_default())}}
                    text(class="muted trip-number") {{format!("#{id}")}}
                    spacer()
                    text(class="muted") {{move ||s.data.with(|d|d.trips.iter().find(|t|t.id==id).map(|t|age(s.now.get(),t.started_ms)).unwrap_or_default())}}
                }
                row(class="trip-meta") {
                    text(class="muted") {{move ||s.data.with(|d|{
                        let Some(t)=d.trips.iter().find(|t|t.id==id) else {return String::new()};
                        let category=if t.name.is_empty() {String::new()} else {
                            format!("{} · ",d.categories.iter().find(|c|Some(c.id)==t.category_id).map_or("Uncategorized",|c|c.name.as_str()))
                        };
                        format!("{category}{} · {}",s.distance(t.distance_m),duration(t.elapsed_at(s.now.get())))
                    })}}
                    if move ||s.coverage_filter.get()==TripFilter::All&&s.data.with(|d|d.trips.iter().any(|t|t.id==id&&t.status==Status::Finished)) {
                        text(class="trip-new") {{move ||s.sections.with(|_|format!("{} new",s.distance(s.new_roads(id))))}}
                    }
                }
                if move ||s.data.with(|d|d.trips.iter().any(|t|t.id==id&&(t.estimated_distance_m>0.0||t.status!=Status::Finished))) {
                    text(class="muted small") {{move ||s.data.with(|d|d.trips.iter().find(|t|t.id==id).map(|t|{
                        let inferred=if t.estimated_distance_m>0.0 {format!("{} inferred across GPS gaps",s.distance(t.estimated_distance_m))}else{String::new()};
                        let state=if t.status==Status::Finished {""} else if inferred.is_empty() {"In progress"} else {" · in progress"};
                        format!("{inferred}{state}")
                    }).unwrap_or_default())}}
                }
            }
            if move ||s.people.with(Vec::is_empty) {
                Button(variant=ButtonVariant::Ghost,class=format!("row-action edit-trip edit-trip-{id}"),on:click=move |_|s.edit_trip(id),a11y:label=format!("Edit trip #{id}")) {"Edit"}
            }
            if move ||s.friend_trip_ids.with(|ids|ids.contains(&id)) {
                Button(variant=ButtonVariant::Ghost,class=format!("row-action add-trip add-trip-{id}"),disabled=Signal::derive_local(move ||added.get()),on:click=move |_|add(),a11y:label=format!("Add trip #{id} to my trips")) {{move ||if added.get(){"Added"}else{"Add to mine"}}}
            }
        }
    }
}
