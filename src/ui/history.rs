use super::{history_dates::HistoryDatesProps, history_trip::HistoryTripProps};
use super::{state::UiState, tools::StatsTabProps};
use crate::model::TripFilter;
use std::collections::BTreeSet;
use zgui::prelude::*;
use zgui::reactive::UnsyncCallback;
use zgui_ui::prelude::*;
use zgui_ui_icons::{prelude::*, set::chevron::CHEVRON_DOWN, set::mark::CHECK};

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
            DialogContent(class="sheet history-sheet") {
                DialogHeader {
                    DialogTitle {"History"}
                    DialogDescription {{move ||s.data.with(|d|format!("{} trips · {} lifetime",d.trips.len(),s.distance(d.total_distance(None))))}}
                }
                PersonPicker()
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
    // Friend and group filters: trips added from friends' history.
    let sources = RwSignal::new_local(Vec::<(String, String, Vec<u64>)>::new());
    let directory = s.directory.get_value();
    spawn_local(async move {
        #[cfg(target_os = "android")]
        let found = blocking(move || super::friends::history_sources(&directory)).await;
        #[cfg(not(target_os = "android"))]
        let found = {
            let _ = directory;
            Vec::new()
        };
        sources.set(found);
    });
    let listed = move || {
        let value = browse.get();
        let filter = parse_filter(&value)
            .or_else(|| {
                sources.with(|all| {
                    all.iter()
                        .find(|source| source.0 == value)
                        .map(|source| TripFilter::Trips {
                            trip_ids: source.2.clone(),
                        })
                })
            })
            .unwrap_or_default();
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
                    SelectTrigger(label="Filter history by category, friend or group",class="history-filter") {SelectValue()}
                    SelectContent {
                        SelectItem(value="all") {"All trips"}
                        SelectItem(value="uncategorized") {"Uncategorized"}
                        for category in move ||s.data.with(|d|d.categories.clone()),key=|c|c.id {
                            SelectItem(value=format!("category:{}",category.id)) {{category.name}}
                        }
                        for source in move ||if s.people.with(|p|!p.is_empty()){Vec::new()}else{sources.get()},key=|source|source.0.clone() {
                            SelectItem(value=source.0.clone()) {{source.1.clone()}}
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

/// Whose trips History and map show: you by default; tap friends to add them, or a group
/// to show you and all its members together.
#[component]
fn PersonPicker() -> impl IntoView {
    let s = UiState::current();
    type People = Vec<(String, String)>;
    let people = RwSignal::new_local(People::new());
    let groups = RwSignal::new_local(Vec::<(String, People)>::new());
    let directory = s.directory.get_value();
    spawn_local(async move {
        #[cfg(target_os = "android")]
        let found = blocking(move || {
            (
                super::friends::people(&directory),
                super::friends::groups_with_trips(&directory),
            )
        })
        .await;
        #[cfg(not(target_os = "android"))]
        let found = {
            let _ = directory;
            (People::new(), Vec::new())
        };
        people.set(found.0);
        groups.set(found.1);
    });
    let me = || ("me".to_string(), "Me".to_string());
    // Empty selection means own trips only.
    let shown = move |id: &str| {
        s.people.with(|p| {
            if p.is_empty() {
                id == "me"
            } else {
                p.iter().any(|x| x.0 == id)
            }
        })
    };
    let toggle = move |person: (String, String)| {
        let mut current = s.people.get_untracked();
        if current.is_empty() {
            current.push(me());
        }
        if let Some(at) = current.iter().position(|p| p.0 == person.0) {
            current.remove(at);
        } else {
            current.push(person);
        }
        s.set_people(current);
    };
    let label = move || {
        let names = s.people_label();
        if names.is_empty() {
            "Showing: My trips".to_string()
        } else if s.people.with(Vec::len) > 3 {
            format!("Showing: {} people", s.people.with(Vec::len))
        } else {
            format!("Showing: {names}")
        }
    };
    view! {
        if move ||!people.with(Vec::is_empty) {
            DropdownMenu {
                DropdownMenuTrigger(variant=ButtonVariant::Outline,class="person-select",a11y:label="Choose whose trips to show") {
                    text(class="person-select-label") {{label}}
                    Icon(icon=CHEVRON_DOWN)
                }
                DropdownMenuContent(class="person-menu") {
                    MenuLabel {"People"}
                    MenuItem(close_on_select=false,class="person-item",on_select=UnsyncCallback::new(move |()|toggle(me()))) {
                        CheckSquare(on=Signal::derive_local(move ||shown("me")))
                        text {"Me"}
                    }
                    for person in move ||people.get(),key=|p|p.0.clone() {
                        MenuItem(close_on_select=false,class="person-item",on_select=UnsyncCallback::new({let p=person.clone();move |()|toggle(p.clone())})) {
                            CheckSquare(on=Signal::derive_local({let id=person.0.clone();move ||shown(&id)}))
                            text {{person.1.clone()}}
                        }
                    }
                    if move ||!groups.with(Vec::is_empty) {
                        MenuSeparator()
                        MenuLabel {"Groups"}
                    }
                    for group in move ||groups.get(),key=|g|g.0.clone() {
                        MenuItem(close_on_select=false,on_select=UnsyncCallback::new({let members=group.1.clone();move |()|{let mut all=vec![me()];all.extend(members.clone());s.set_people(all)}})) {{format!("Me + {}",group.0)}}
                    }
                    MenuSeparator()
                    MenuItem(close_on_select=false,on_select=UnsyncCallback::new(move |()|s.set_people(Vec::new()))) {"Only my trips"}
                }
            }
        }
    }
}

/// Always-visible checkbox square for menu rows; filled with a tick when on.
#[component]
fn CheckSquare(on: Signal<bool, LocalStorage>) -> impl IntoView {
    view! {
        box(class="check-square",class:check-square-on=move ||on.get()) {
            if move ||on.get() {
                Icon(icon=CHECK,class="check-square-tick")
            }
        }
    }
}
