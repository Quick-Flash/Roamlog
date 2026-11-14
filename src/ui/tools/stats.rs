use crate::ui::state::UiState;
use zgui::prelude::*;
use zgui_ui::prelude::*;

use super::format::periods;

/// Period label, distance, and bar width percent.
fn stat_rows(s: UiState, weekly: bool) -> Vec<(String, f64, f64)> {
    let filter = s.history_filter.get();
    let trips: Vec<(u64, f64)> = s.data.with(|d| {
        d.trips_for(filter)
            .map(|t| (t.started_ms, t.distance_m))
            .collect()
    });
    let rows = periods(&trips, weekly);
    let max = rows.iter().map(|r| r.1).fold(0.0, f64::max).max(1.0);
    rows.into_iter()
        .map(|(label, meters)| (label, meters, meters / max * 100.0))
        .collect()
}

/// Distance per week or month for current selection, with category totals.
#[component]
pub fn StatsTab() -> impl IntoView {
    let s = UiState::current();
    let weekly = RwSignal::new_local(false);
    view! {
        column(class="tab-body") {
            row(class="stat-cards") {
                column(class="stat-card") {
                    text(class="stat-value") {{move ||s.data.with(|d|s.distance(d.total_distance(None)))}}
                    text(class="muted") {"Lifetime"}
                }
                column(class="stat-card") {
                    text(class="stat-value") {{move ||{let(total,traveled)=s.road_totals.get();if total>0.0{s.distance(traveled)}else{"–".into()}}}}
                    text(class="muted") {"Roads traveled"}
                }
            }
            row(class="segmented") {
                if move ||weekly.get() {
                    Button(variant=ButtonVariant::Ghost,on:click=move |_|weekly.set(false)) {"Months"}
                    Button(variant=ButtonVariant::Secondary,class="seg-on",on:click=move |_|weekly.set(true)) {"Weeks"}
                } else {
                    Button(variant=ButtonVariant::Secondary,class="seg-on",on:click=move |_|weekly.set(false)) {"Months"}
                    Button(variant=ButtonVariant::Ghost,on:click=move |_|weekly.set(true)) {"Weeks"}
                }
            }
            text(class="muted") {{move ||format!("{} · newest first",s.history_label())}}
            scroll(class="sheet-scroll") {
                column(class="list") {
                    for row in move ||stat_rows(s,weekly.get()),key=|r|(r.0.clone(),r.1.to_bits()) {
                        column(class="stat-row") {
                            row(class="history-row") {text {{row.0.clone()}} spacer() text(class="muted") {{s.distance(row.1)}}}
                            box(class="stat-track") {box(class="stat-bar",style:width=format!("{:.1}%",row.2)) {}}
                        }
                    }
                    text(class="section-title") {"By category · all history"}
                    if move ||s.data.with(|d|d.trips.iter().any(|t|t.category_id.is_none())) {
                        row(class="history-row") {text {"Uncategorized"} spacer() text(class="muted") {{move ||s.data.with(|d|s.distance(d.trips.iter().filter(|t|t.category_id.is_none()).map(|t|t.distance_m).sum::<f64>().max(0.0)))}}}
                    }
                    for category in move ||s.data.with(|d|d.categories.clone()),key=|c|c.id {
                        row(class="history-row") {text {{category.name}} spacer() text(class="muted") {{move ||s.data.with(|d|s.distance(d.total_distance(Some(category.id))))}}}
                    }
                }
            }
        }
    }
}
