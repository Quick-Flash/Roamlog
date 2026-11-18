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
    let name = RwSignal::new_local(s.data.with_untracked(|d| {
        d.trips
            .iter()
            .find(|t| t.id == id)
            .map(|t| t.name.clone())
            .unwrap_or_default()
    }));
    let finished = s.data.with_untracked(|d| {
        d.trips
            .iter()
            .any(|t| t.id == id && t.status == Status::Finished)
    });
    // Last saved values, so Save enables only after a real change.
    let saved_name = RwSignal::new_local(name.get_untracked());
    let saved_category = RwSignal::new_local(category.get_untracked());
    let renamed = move || {
        let value = name.get();
        let value = value.trim();
        !value.is_empty() && value.len() <= 80 && saved_name.with(|saved| saved != value)
    };
    let recategorized = move || finished && saved_category.with(|saved| *saved != category.get());
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
        DialogHeader {DialogTitle {{format!("Edit trip #{id}")}} DialogDescription {"Changes stay on this device and are included in backups."}}
        scroll(class="trip-editor-scroll") {
        column(class="tab-body") {
        text(class="section-title") {"Name"}
        Input(value=name,label="Trip name",placeholder="Morning walk…",class="wide")
        text(class="section-title") {"Category"}
        Select(value=category) {
            SelectTrigger(class="category-select wide",label="Edited trip category") {SelectValue()}
            SelectContent {
                SelectItem(value="0") {"Uncategorized"}
                for c in move ||s.data.with(|d|d.categories.clone()),key=|c|c.id {
                    SelectItem(value=c.id.to_string()) {{c.name}}
                }
            }
        }
        // One save for name and category; only changed fields are sent.
        Button(class="save-category rename-trip wide",disabled=Signal::derive_local(move ||!renamed()&&!recategorized()),on:click=move |_|{
            if renamed() {
                s.send(json!({"op":"rename_trip","trip_id":id,"name":name.get_untracked().trim()}),"Trip saved.");
                saved_name.set(name.get_untracked().trim().to_owned());
            }
            if recategorized() {
                let category_id=category.get_untracked().parse::<u64>().ok().filter(|c|*c!=0);
                s.send(json!({"op":"set_category","trip_id":id,"category_id":category_id}),"Trip saved.");
                saved_category.set(category.get_untracked());
            }
        }) {"Save changes"}
        if move ||finished && n>=2 {
        text(class="section-title") {"Trim start and end"}
        text(class="muted small") {{move ||format!("Start · {}",clock(time(index(start.get()))))}}
        Slider(value=start,min=0.0,max=max,step=1.0,label="Trim start")
        text(class="muted small") {{move ||format!("End · {}",clock(time(index(end.get()))))}}
        Slider(value=end,min=0.0,max=max,step=1.0,label="Trim end")
        text(class="muted") {{move ||{let(a,b)=(index(start.get()),index(end.get()));format!("Keep {}–{} · {}",clock(time(a)),clock(time(b)),s.distance(length(a,b)))}}}
        Button(variant=ButtonVariant::Outline,class="trim-trip",disabled=Signal::derive_local(move ||!trim_ok()),on:click=move |_|{
            let (a,b)=(index(start.get_untracked()),index(end.get_untracked()));
            s.edit_open.set(false);
            s.send(json!({"op":"trim_trip","trip_id":id,"start_ms":time(a),"end_ms":time(b)}),"Trip trimmed.");
        }) {"Trim"}
        }
        if move ||finished && n>=4 {
        text(class="section-title") {"Split into two trips"}
        Slider(value=split,min=0.0,max=max,step=1.0,label="Split point")
        text(class="muted") {{move ||{let i=index(split.get());format!("At {} · {} + {}",clock(time(i)),s.distance(length(0,i.saturating_sub(1))),s.distance(length(i,n-1)))}}}
        Button(variant=ButtonVariant::Outline,class="split-trip",disabled=Signal::derive_local(move ||!split_ok()),on:click=move |_|{
            let at=time(index(split.get_untracked()));
            s.edit_open.set(false);
            s.history_filter.set(TripFilter::All);
            s.send(json!({"op":"split_trip","trip_id":id,"at_ms":at}),"Trip split.");
        }) {"Split"}
        }
        if move ||finished {
        Button(variant=ButtonVariant::Outline,class="delete-trip wide danger-text",on:click=move |_|{
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
    }
}
