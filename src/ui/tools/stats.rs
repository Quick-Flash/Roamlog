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

/// Category name, distance, and bar width percent (relative to largest), uncategorized first.
fn category_rows(s: UiState) -> Vec<(String, f64, f64)> {
    s.data.with(|d| {
        let mut rows = Vec::new();
        let none: f64 = d
            .trips
            .iter()
            .filter(|t| t.category_id.is_none())
            .map(|t| t.distance_m)
            .sum();
        if d.trips.iter().any(|t| t.category_id.is_none()) {
            rows.push(("Uncategorized".to_string(), none));
        }
        for c in &d.categories {
            rows.push((c.name.clone(), d.total_distance(Some(c.id))));
        }
        let max = rows.iter().map(|r| r.1).fold(0.0, f64::max).max(1.0);
        rows.into_iter()
            .map(|(name, meters)| (name, meters, meters / max * 100.0))
            .collect()
    })
}

/// Totals, distance per category, and distance per month or week for current selection.
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
                    text(class="stat-value") {{move ||s.data.with(|d|d.trips.len()).to_string()}}
                    text(class="muted") {"Trips"}
                }
                column(class="stat-card") {
                    text(class="stat-value") {{move ||s.coverage_percent().unwrap_or_else(||"–".into())}}
                    text(class="muted") {"Nearby roads"}
                }
            }
            scroll(class="sheet-scroll") {
                column(class="list") {
                    text(class="section-title") {"By category · all history"}
                    for row in move ||category_rows(s),key=|r|(r.0.clone(),r.1.to_bits()) {
                        column(class="stat-row") {
                            row(class="history-row") {text {{row.0.clone()}} spacer() text(class="muted") {{s.distance(row.1)}}}
                            box(class="stat-track") {box(class="stat-bar",style:width=format!("{:.1}%",row.2)) {}}
                        }
                    }
                    row(class="stat-period-head") {
                        text(class="section-title grow") {"Distance over time"}
                        row(class="segmented stat-period") {
                            Button(variant=ButtonVariant::Ghost,class:seg-on=move ||!weekly.get(),on:click=move |_|weekly.set(false)) {"Months"}
                            Button(variant=ButtonVariant::Ghost,class:seg-on=move ||weekly.get(),on:click=move |_|weekly.set(true)) {"Weeks"}
                        }
                    }
                    for row in move ||stat_rows(s,weekly.get()),key=|r|(r.0.clone(),r.1.to_bits()) {
                        column(class="stat-row") {
                            row(class="history-row") {text {{row.0.clone()}} spacer() text(class="muted") {{s.distance(row.1)}}}
                            box(class="stat-track") {box(class="stat-bar",style:width=format!("{:.1}%",row.2)) {}}
                        }
                    }
                    text(class="section-title") {"Roads traveled"}
                    text {{move ||{let(total,traveled)=s.road_totals.get();s.coverage_percent().map_or_else(||"Road coverage unavailable".into(),|p|format!("{p} of nearby roads · {} of {}",s.distance(traveled),s.distance(total)))}}}
                    box(class="stat-track") {
                        box(class="stat-bar",style:width=move ||{let(total,traveled)=s.road_totals.get();Some(format!("{:.0}%",if total>0.0{(100.0*traveled/total).clamp(0.0,100.0)}else{0.0}))}) {}
                    }
                }
            }
        }
    }
}
