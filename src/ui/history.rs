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
        if move ||s.trips_open.get() {
            column(class="tab-page history-sheet") {
                text(class="page-title") {"History"}
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
        #[cfg(any(target_os = "android", target_os = "ios"))]
        let found = blocking(move || super::friends::history_sources(&directory)).await;
        #[cfg(not(any(target_os = "android", target_os = "ios")))]
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
        // Coverage is computed for what the map shows; only quote it when the list matches.
        if period.get() != "all" || parse_filter(&browse.get()) != Some(s.history_filter.get()) {
            return "Show these trips on map to see their road coverage.".to_string();
        }
        if s.coverage_filter.get() != s.history_filter.get() {
            return "Updating road coverage…".to_string();
        }
        let (total, traveled) = s.road_totals.get();
        s.coverage_percent().map_or_else(
            || "Road coverage appears after recording with offline map.".to_string(),
            |percent| {
                format!(
                    "{percent} of nearby roads traveled · {} of {}",
                    s.distance(traveled),
                    s.distance(total)
                )
            },
        )
    };
    let weeks = Memo::new(move |_| {
        let ids = listed();
        let now = s.now.get();
        s.data.with(|data| {
            let mut weeks = std::collections::BTreeMap::<u64, Vec<u64>>::new();
            for id in ids {
                if let Some(trip) = data.trips.iter().find(|t| t.id == id) {
                    let days = now.saturating_sub(trip.started_ms) / 86_400_000;
                    weeks.entry(days / 7).or_default().push(id);
                }
            }
            weeks.into_iter().collect::<Vec<_>>()
        })
    });
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
                HistoryDates(period=period,days=days,range=range)
            }
            row(class="selection-row") {
                text(class="muted grow") {{move ||{let n=listed().len();let picked=selected.with(BTreeSet::len);
                    format!("{n} {}{}",if n==1 {"trip"} else {"trips"},if picked>0 {format!(" · {picked} selected")} else {String::new()})}}}
                Button(variant=ButtonVariant::Ghost,class="select-all-trips",on:click=move |_|selected.update(|ids|ids.extend(listed()))) {"Select all"}
                if move ||selected.with(|ids|!ids.is_empty()) {
                    Button(variant=ButtonVariant::Ghost,class="clear-trip-selection",on:click=move |_|selected.set(BTreeSet::new())) {"Clear"}
                }
            }
            text(class="muted small coverage-line") {{coverage}}
            scroll(class="sheet-scroll") {
                column(class="list") {
                    for week in move ||weeks.get(),key=|week|week.clone() {
                        text(class="section-title") {{match week.0 {
                            0 => "Last 7 days".into(),
                            1 => "1 week ago".into(),
                            weeks => format!("{weeks} weeks ago"),
                        }}}
                        for id in move ||week.1.clone(),key=|id|*id {
                            HistoryTrip(id=id,selected=selected)
                        }
                    }
                    if move ||listed().is_empty() {
                        text(class="empty") {"No trips match these filters. Trips stay on this device."}
                    }
                }
            }
            // One action: show the selection, or else everything this list shows.
            Button(class="show-history wide",disabled=Signal::derive_local(move ||listed().is_empty()),on:click=move |_|{
                let picked=selected.with_untracked(|ids|ids.iter().copied().collect::<Vec<_>>());
                let ids=if picked.is_empty() {listed()} else {picked};
                let whole=selected.with_untracked(BTreeSet::is_empty)&&period.get_untracked()=="all";
                let filter=match parse_filter(&browse.get_untracked()) {
                    Some(filter) if whole => filter,
                    _ if ids.len()==1 => TripFilter::Trip {trip_id:ids[0]},
                    _ => TripFilter::Trips {trip_ids:ids},
                };
                s.set_history_filter(filter);
                s.show_history();
            }) {{move ||match selected.with(BTreeSet::len) {
                0 => {let n=listed().len();format!("Show {n} {} on map",if n==1 {"trip"} else {"trips"})}
                n => format!("Show {n} selected on map"),
            }}}
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
pub(crate) fn PersonPicker() -> impl IntoView {
    let s = UiState::current();
    type People = Vec<(String, String)>;
    let people = RwSignal::new_local(People::new());
    let groups = RwSignal::new_local(Vec::<(String, People)>::new());
    let directory = s.directory.get_value();
    spawn_local(async move {
        #[cfg(any(target_os = "android", target_os = "ios"))]
        let found = blocking(move || {
            (
                super::friends::people(&directory),
                super::friends::groups_with_trips(&directory),
            )
        })
        .await;
        #[cfg(not(any(target_os = "android", target_os = "ios")))]
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
