use super::tools::date;
use zgui::prelude::*;
use zgui_ui::prelude::*;

/// Calendar-day bounds use device timezone, including both chosen dates.
pub(super) fn matches(
    started_ms: u64,
    now_ms: u64,
    period: &str,
    days: &str,
    range: Option<DateRange>,
) -> bool {
    if period == "all" {
        return true;
    }
    let trip = Date::from_iso(&date(started_ms)).expect("formatted local date");
    if period == "range" {
        return range
            .filter(|r| r.is_settled())
            .is_none_or(|r| trip >= r.from && trip <= r.to.unwrap());
    }
    let count = if period == "days" { days } else { period }
        .parse::<i32>()
        .ok()
        .filter(|n| (1..=36500).contains(n));
    let Some(count) = count else {
        return false;
    };
    let today = Date::from_iso(&date(now_ms)).expect("formatted local date");
    trip >= today.add_days(-(count - 1)) && trip <= today
}

#[component]
pub fn HistoryDates(
    period: RwSignal<String, LocalStorage>,
    days: RwSignal<String, LocalStorage>,
    range: RwSignal<Option<DateRange>, LocalStorage>,
) -> impl IntoView {
    let s = super::state::UiState::current();
    let today = Date::from_iso(&date(s.now.get_untracked()));
    view! {
        column(class="history-dates-block") {
        row(class="filter-row history-dates") {
            Select(value=period) {
                SelectTrigger(label="History date filter",class="history-filter history-period") {SelectValue()}
                SelectContent {
                    SelectItem(value="all") {"Any time"}
                    SelectItem(value="7") {"Last 7 days"}
                    SelectItem(value="30") {"Last 30 days"}
                    SelectItem(value="90") {"Last 90 days"}
                    SelectItem(value="days") {"Last X days"}
                    SelectItem(value="range") {"Date range"}
                }
            }
            if move ||period.get()=="days" {
                Input(value=days,label="Number of days",placeholder="Days",class="history-days")
            }
            if move ||period.get()=="range" {
                DatePicker(mode=CalendarMode::Range,range=range,today=today,label="History date range",placeholder="Choose dates",class="history-range")
            }
        }
        if move ||period.get()=="days" && !days.with(|v|v.parse::<u32>().is_ok_and(|n|(1..=36500).contains(&n))) {
            text(class="muted") {"Enter 1–36500 days."}
        }
        if move ||period.get()=="range" {
        text(class="muted") {{move ||{
            if period.get()=="range" {
                if let Some(r)=range.get() {
                    return r.to.map_or_else(||"Choose end date. Dates use device timezone.".into(),|to|format!("{} – {} · Both dates included.",r.from.to_iso(),to.to_iso()));
                }
                "Choose start and end dates. Dates use device timezone.".into()
            } else {String::new()}
        }}}
        }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    // Noon UTC stays on same day in Denver; adjacent days test local boundaries.
    const DAY: u64 = 86_400_000;
    const NOW: u64 = 1_794_657_600_000;
    #[test]
    fn presets_and_custom_days_include_today_and_reject_invalid_counts() {
        assert!(matches(NOW, NOW, "7", "", None));
        assert!(matches(NOW - 6 * DAY, NOW, "7", "", None));
        assert!(!matches(NOW - 7 * DAY, NOW, "7", "", None));
        assert!(!matches(NOW + DAY, NOW, "7", "", None));
        assert!(matches(NOW - 29 * DAY, NOW, "30", "", None));
        assert!(!matches(NOW - 30 * DAY, NOW, "30", "", None));
        assert!(matches(NOW - 89 * DAY, NOW, "90", "", None));
        assert!(matches(NOW - 9 * DAY, NOW, "days", "10", None));
        assert!(!matches(NOW - 10 * DAY, NOW, "days", "10", None));
        for input in ["", "0", "-1", "abc", "36501"] {
            assert!(!matches(NOW, NOW, "days", input, None));
        }
    }
    #[test]
    fn calendar_range_includes_both_end_dates() {
        let from = Date::from_iso(&date(NOW - 3 * DAY)).unwrap();
        let to = Date::from_iso(&date(NOW - DAY)).unwrap();
        assert!(matches(
            NOW,
            NOW,
            "range",
            "",
            Some(DateRange::starting(from))
        ));
        let range = Some(DateRange::between(from, to));
        assert!(matches(NOW - 3 * DAY, NOW, "range", "", range));
        assert!(matches(NOW - DAY, NOW, "range", "", range));
        assert!(!matches(NOW - 4 * DAY, NOW, "range", "", range));
        assert!(!matches(NOW, NOW, "range", "", range));
    }
}
