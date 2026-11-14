use crate::ui::state::UiState;
use zgui::prelude::*;
use zgui_ui::prelude::*;

use super::clock;
use crate::{
    geo::Point,
    model::{Status, TripFilter},
};
use serde_json::json;

#[component]
pub fn TripEditor(id: u64) -> impl IntoView {
    let s = UiState::current();
    // Observed fixes only: inferred road points have interpolated times.
    let fixes: Vec<(u64, Point, bool)> = s.data.with_untracked(|d| {
        d.trips
            .iter()
            .find(|t| t.id == id && t.status == Status::Finished)
            .map(|t| {
                t.points
                    .iter()
                    .filter(|p| !p.road_matched)
                    .map(|p| (p.fix.timestamp_ms, p.fix.point, p.connected))
                    .collect()
            })
            .unwrap_or_default()
    });
    let n = fixes.len();
    let category = RwSignal::new_local(s.data.with_untracked(|d| {
        d.trips
            .iter()
            .find(|t| t.id == id)
            .and_then(|t| t.category_id)
            .unwrap_or(0)
            .to_string()
    }));
    let start = RwSignal::new_local(0.0);
    let end = RwSignal::new_local(n.saturating_sub(1) as f64);
    let split = RwSignal::new_local((n / 2) as f64);
    let fixes = StoredValue::new_local(fixes);
    let index = move |v: f64| (v.round().max(0.0) as usize).min(n.saturating_sub(1));
    let length = move |a: usize, b: usize| {
        fixes.with_value(|f| {
            f.get(a..=b).map_or(0.0, |range| {
                range
                    .windows(2)
                    .filter(|p| p[1].2)
                    .map(|p| p[0].1.distance(p[1].1))
                    .sum::<f64>()
            })
        })
    };
    let time = move |i: usize| fixes.with_value(|f| f.get(i).map_or(0, |p| p.0));
    let trim_ok = move || {
        let (a, b) = (index(start.get()), index(end.get()));
        b > a && (a > 0 || b + 1 < n)
    };
    let split_ok = move || {
        let i = index(split.get());
        i >= 2 && i + 2 <= n
    };
    let max = n.saturating_sub(1) as f64;
    view! {
        if move ||n<4 {
            DialogHeader {DialogTitle {{format!("Edit trip #{id}")}} DialogDescription {"Trip too short to edit."}}
        } else {
        DialogHeader {DialogTitle {{format!("Edit trip #{id}")}} DialogDescription {"Edits are saved with your trips and included in backups."}}
        text {"Category"}
        row(class="category-row") {
            Select(value=category) {
                SelectTrigger(class="category-select",label="Edited trip category") {SelectValue()}
                SelectContent {
                    SelectItem(value="0") {"Uncategorized"}
                    for c in move ||s.data.with(|d|d.categories.clone()),key=|c|c.id {
                        SelectItem(value=c.id.to_string()) {{c.name}}
                    }
                }
            }
            Button(class="save-category",on:click=move |_|{
                let category_id=category.get_untracked().parse::<u64>().ok().filter(|c|*c!=0);
                s.send(json!({"op":"set_category","trip_id":id,"category_id":category_id}),"Category changed.");
            }) {"Save"}
        }
        text {"Trim start and end"}
        Slider(value=start,min=0.0,max=max,step=1.0,label="Trim start")
        Slider(value=end,min=0.0,max=max,step=1.0,label="Trim end")
        text(class="muted") {{move ||{let(a,b)=(index(start.get()),index(end.get()));format!("Keep {}–{} · {}",clock(time(a)),clock(time(b)),s.distance(length(a,b)))}}}
        Button(variant=ButtonVariant::Outline,class="trim-trip",disabled=Signal::derive_local(move ||!trim_ok()),on:click=move |_|{
            let (a,b)=(index(start.get_untracked()),index(end.get_untracked()));
            s.edit_open.set(false);
            s.send(json!({"op":"trim_trip","trip_id":id,"start_ms":time(a),"end_ms":time(b)}),"Trip trimmed.");
        }) {"Trim"}
        text {"Split into two trips"}
        Slider(value=split,min=0.0,max=max,step=1.0,label="Split point")
        text(class="muted") {{move ||{let i=index(split.get());format!("At {} · {} + {}",clock(time(i)),s.distance(length(0,i.saturating_sub(1))),s.distance(length(i,n-1)))}}}
        Button(variant=ButtonVariant::Outline,class="split-trip",disabled=Signal::derive_local(move ||!split_ok()),on:click=move |_|{
            let at=time(index(split.get_untracked()));
            s.edit_open.set(false);
            s.history_filter.set(TripFilter::All);
            s.send(json!({"op":"split_trip","trip_id":id,"at_ms":at}),"Trip split.");
        }) {"Split"}
        text(class="section-title") {"Delete"}
        Button(variant=ButtonVariant::Destructive,class="delete-trip wide",on:click=move |_|{
            if s.confirm_delete.get_untracked()==Some(id) {
                s.edit_open.set(false);
                s.delete_trip(id);
            } else {
                s.confirm_delete.set(Some(id));
            }
        }) {{move ||if s.confirm_delete.get()==Some(id){"Tap again to delete trip"}else{"Delete trip"}}}
        }
    }
}
