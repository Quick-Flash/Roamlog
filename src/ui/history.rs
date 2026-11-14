use super::{history_dates::HistoryDatesProps, history_trip::HistoryTripProps};
use super::{state::UiState, tools::StatsTabProps};
use crate::model::TripFilter;
use std::collections::BTreeSet;
use zgui::prelude::*;
use zgui_ui::prelude::*;

/// "3 min ago", "5 h ago", "2 d ago": no timezone needed.
pub(super) fn age(now_ms: u64, then_ms: u64) -> String {
    let minutes = now_ms.saturating_sub(then_ms) / 60_000;
    match minutes {
        0..60 => format!("{minutes} min ago"),
        60..1440 => format!("{} h ago", minutes / 60),
        _ => format!("{} d ago", minutes / 1440),
    }
}

pub(super) fn duration(ms: u64) -> String {
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
        TripFilter::Trip { .. } | TripFilter::Trips { .. } => "all".into(),
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
        None
    }
}

#[component]
fn TripsTab() -> impl IntoView {
    let s = UiState::current();
    let browse = RwSignal::new_local(filter_value(s.history_filter.get_untracked()));
    let selected = RwSignal::new_local(match s.history_filter.get_untracked() {
        TripFilter::Trip { trip_id } => BTreeSet::from([trip_id]),
        TripFilter::Trips { trip_ids } => trip_ids.into_iter().collect(),
        _ => BTreeSet::new(),
    });
    let period = RwSignal::new_local("all".to_string());
    let days = RwSignal::new_local("7".to_string());
    let range = RwSignal::new_local(None::<DateRange>);
    let listed = move || {
        let filter = parse_filter(&browse.get()).unwrap_or_default();
        s.data.with(|d| {
            d.trips
                .iter()
                .rev()
                .filter(|t| {
                    filter.matches(t)
                        && super::history_dates::matches(
                            t.started_ms,
                            s.now.get(),
                            &period.get(),
                            &days.get(),
                            range.get(),
                        )
                })
                .map(|t| t.id)
                .collect::<Vec<_>>()
        })
    };
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
                Select(value=browse) {
                    SelectTrigger(label="Filter history by category",class="history-filter") {SelectValue()}
                    SelectContent {
                        SelectItem(value="all") {"All trips"}
                        SelectItem(value="uncategorized") {"Uncategorized"}
                        for category in move ||s.data.with(|d|d.categories.clone()),key=|c|c.id {
                            SelectItem(value=format!("category:{}",category.id)) {{category.name}}
                        }
                    }
                }
                Button(variant=ButtonVariant::Outline,class="select-all-trips",on:click=move |_|selected.update(|ids|ids.extend(listed()))) {"Select all"}
                Button(variant=ButtonVariant::Ghost,class="clear-trip-selection",on:click=move |_|selected.set(BTreeSet::new())) {"Clear"}
            }
            HistoryDates(period=period,days=days,range=range)
            text(class="muted") {{move ||format!("{} matching · {} selected",listed().len(),selected.with(BTreeSet::len))}}
            text(class="muted") {{move ||format!("On map: {} · {}",s.history_summary(),coverage())}}
            scroll(class="sheet-scroll") {
                column(class="list") {
                    for id in move ||listed().into_iter(),key=|id|*id {
                        HistoryTrip(id=id,selected=selected)
                    }
                    if move ||listed().is_empty() {
                        text(class="empty") {"No trips match these filters. Trips stay on this device."}
                    }
                }
            }
            row(class="actions") {
                Button(class="show-history",disabled=Signal::derive_local(move ||selected.with(BTreeSet::is_empty)),on:click=move |_|{
                    let ids=selected.with_untracked(|ids|ids.iter().copied().collect::<Vec<_>>());
                    let filter=if ids.len()==1 {TripFilter::Trip {trip_id:ids[0]}} else {TripFilter::Trips {trip_ids:ids}};
                    s.set_history_filter(filter);
                    s.show_history();
                }) {{move ||format!("Show selected ({})",selected.with(BTreeSet::len))}}
                Button(variant=ButtonVariant::Outline,class="show-all-trips",on:click=move |_|{
                    s.set_history_filter(TripFilter::All);
                    s.show_history();
                }) {"Show all trips"}
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
