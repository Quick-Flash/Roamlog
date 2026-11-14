use super::{state::UiState, tools::StatsTabProps};
use crate::model::{Status, TripFilter};
use zgui::prelude::*;
use zgui_ui::prelude::*;

/// "3 min ago", "5 h ago", "2 d ago": no timezone needed.
fn age(now_ms: u64, then_ms: u64) -> String {
    let minutes = now_ms.saturating_sub(then_ms) / 60_000;
    match minutes {
        0..60 => format!("{minutes} min ago"),
        60..1440 => format!("{} h ago", minutes / 60),
        _ => format!("{} d ago", minutes / 1440),
    }
}

fn duration(ms: u64) -> String {
    let minutes = ms / 60_000;
    format!("{}:{:02} h", minutes / 60, minutes % 60)
}

/// History sheet: Trips (selection, list, edit) and Stats tabs.
#[component]
pub fn History() -> impl IntoView {
    let s = UiState::current();
    view! {
        Dialog(open=s.trips_open) {
            DialogTrigger(variant=ButtonVariant::Outline) {"History"}
            DialogContent(class="sheet") {
                DialogHeader {
                    DialogTitle {"History"}
                    DialogDescription {{move ||s.data.with(|d|format!("{} trips · {} lifetime",d.trips.len(),s.distance(d.total_distance(None))))}}
                }
                Tabs(default_value="trips".to_string(),class="history-tabs") {
                    TabsList(class="tabs-list") {
                        TabsTrigger(value="trips") {"Trips"}
                        TabsTrigger(value="stats") {"Stats"}
                    }
                    TabsContent(value="trips") {TripsTab()}
                    TabsContent(value="stats") {StatsTab()}
                }
            }
        }
    }
}

fn filter_value(filter: TripFilter) -> String {
    match filter {
        TripFilter::All => "all".into(),
        TripFilter::Category { category_id: None } => "uncategorized".into(),
        TripFilter::Category {
            category_id: Some(id),
        } => format!("category:{id}"),
        TripFilter::Trip { trip_id } => format!("trip:{trip_id}"),
    }
}

fn parse_filter(value: &str) -> Option<TripFilter> {
    if value == "all" {
        Some(TripFilter::All)
    } else if value == "uncategorized" {
        Some(TripFilter::Category { category_id: None })
    } else if let Some(id) = value.strip_prefix("category:") {
        id.parse().ok().map(|id| TripFilter::Category {
            category_id: Some(id),
        })
    } else {
        value
            .strip_prefix("trip:")
            .and_then(|id| id.parse().ok())
            .map(|trip_id| TripFilter::Trip { trip_id })
    }
}

#[component]
fn TripsTab() -> impl IntoView {
    let s = UiState::current();
    let coverage = move || {
        if s.coverage_filter.get() != s.history_filter.get() {
            return "Updating road coverage…".to_string();
        }
        let (total, traveled) = s.road_totals.get();
        if total == 0.0 {
            "Road coverage appears after recording with offline map.".to_string()
        } else {
            format!(
                "{} of {} nearby roads traveled ({:.0}%)",
                s.distance(traveled),
                s.distance(total),
                traveled / total * 100.0
            )
        }
    };
    view! {
        column(class="tab-body") {
            row(class="filter-row") {
                Select(value=Binding::controlled(
                    Signal::derive_local(move ||filter_value(s.history_filter.get())),
                    move |value:String| if let Some(filter)=parse_filter(&value) {s.set_history_filter(filter)}
                )) {
                    SelectTrigger(label="Trips shown on map",class="history-filter") {SelectValue()}
                    SelectContent {
                        SelectItem(value="all") {"All trips"}
                        SelectItem(value="uncategorized") {"Uncategorized"}
                        for category in move ||s.data.with(|d|d.categories.clone()),key=|c|c.id {
                            SelectItem(value=format!("category:{}",category.id)) {{category.name}}
                        }
                        for id in move ||match s.history_filter.get(){TripFilter::Trip {trip_id}=>vec![trip_id],_=>vec![]},key=|id|*id {
                            SelectItem(value=format!("trip:{id}")) {{format!("Trip #{id}")}}
                        }
                    }
                }
                Button(class="show-history",on:click=move |_|s.show_history()) {"Show on map"}
            }
            text(class="muted") {{move ||format!("{} · {}",s.history_summary(),coverage())}}
            scroll(class="sheet-scroll") {
                column(class="list") {
                    for id in move ||{let filter=s.history_filter.get();s.data.with(|d|d.trips.iter().rev().filter(|t|filter.matches(t)).map(|t|t.id).collect::<Vec<_>>())},key=|id|*id {
                        row(class="list-row") {
                            column(class="list-main",on:click=move |_|s.show_trip(id)) {
                                row(class="trip-title") {
                                    text(class="list-title") {{move ||s.data.with(|d|{
                                        let Some(t)=d.trips.iter().find(|t|t.id==id) else {return String::new()};
                                        let name=d.categories.iter().find(|c|Some(c.id)==t.category_id).map_or("Uncategorized",|c|c.name.as_str());
                                        format!("{name} · #{id}")
                                    })}}
                                    spacer()
                                    text(class="muted") {{move ||s.data.with(|d|d.trips.iter().find(|t|t.id==id).map(|t|age(s.now.get(),t.started_ms)).unwrap_or_default())}}
                                }
                                text(class="muted") {{move ||s.data.with(|d|d.trips.iter().find(|t|t.id==id).map(|t|{
                                    let inferred=if t.estimated_distance_m>0.0 {format!(" · {} inferred",s.distance(t.estimated_distance_m))}else{String::new()};
                                    // First-traveled attribution needs all-trip coverage.
                                    let new=if s.coverage_filter.get()==TripFilter::All&&t.status==Status::Finished {s.sections.with(|_|format!(" · {} new",s.distance(s.new_roads(id))))}else{String::new()};
                                    let state=if t.status==Status::Finished {String::new()} else {" · in progress".into()};
                                    format!("{} · {}{inferred}{new}{state}",s.distance(t.distance_m),duration(t.elapsed_at(s.now.get())))
                                }).unwrap_or_default())}}
                            }
                            if move ||s.data.with(|d|d.trips.iter().any(|t|t.id==id&&t.status==Status::Finished)) {
                                Button(variant=ButtonVariant::Ghost,class="row-action edit-trip",on:click=move |_|s.edit_trip(id)) {"Edit"}
                            }
                        }
                    }
                    if move ||{let filter=s.history_filter.get();s.data.with(|d|d.trips_for(filter).next().is_none())} {
                        text(class="empty") {"No trips in this selection. Trips stay on this device."}
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ages_and_durations_read_naturally() {
        assert_eq!(age(3 * 60_000, 0), "3 min ago");
        assert_eq!(age(5 * 3_600_000, 0), "5 h ago");
        assert_eq!(age(50 * 3_600_000, 0), "2 d ago");
        assert_eq!(duration(95 * 60_000), "1:35 h");
    }
}
