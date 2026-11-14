use crate::area;

/// Street name from coverage road id.
pub fn road_name(road_id: &str) -> String {
    area::name(road_id).unwrap_or("Unnamed road").into()
}

/// Local calendar fields of `ms`: year, month 1–12, day, weekday (0 = Monday), hour, minute.
pub fn local_time(ms: u64) -> (i64, u32, u32, u32, u32, u32) {
    let seconds = (ms / 1000) as libc::time_t;
    // SAFETY: localtime_r writes only into `tm`, which is zeroed plain data.
    let tm = unsafe {
        let mut tm: libc::tm = std::mem::zeroed();
        libc::localtime_r(&seconds, &mut tm);
        tm
    };
    (
        i64::from(tm.tm_year) + 1900,
        tm.tm_mon as u32 + 1,
        tm.tm_mday as u32,
        (tm.tm_wday as u32 + 6) % 7,
        tm.tm_hour as u32,
        tm.tm_min as u32,
    )
}

pub fn date(ms: u64) -> String {
    let (y, m, d, ..) = local_time(ms);
    format!("{y}-{m:02}-{d:02}")
}

pub fn clock(ms: u64) -> String {
    let (.., h, min) = local_time(ms);
    format!("{h:02}:{min:02}")
}

/// Days since 1970-01-01 for civil date (proleptic Gregorian).
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (i64::from(m) + 9) % 12;
    let doy = (153 * mp + 2) / 5 + i64::from(d) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// Distance per local week (Monday start) or month, newest first, at most 12 periods.
pub fn periods(trips: &[(u64, f64)], weekly: bool) -> Vec<(String, f64)> {
    let mut totals: std::collections::BTreeMap<i64, f64> = Default::default();
    for &(started_ms, meters) in trips {
        let (y, m, d, weekday, ..) = local_time(started_ms);
        let key = if weekly {
            days_from_civil(y, m, d) - i64::from(weekday)
        } else {
            y * 12 + i64::from(m) - 1
        };
        *totals.entry(key).or_default() += meters;
    }
    totals
        .into_iter()
        .rev()
        .take(12)
        .map(|(key, meters)| {
            let label = if weekly {
                let (_, m, d) = civil_from_days(key);
                format!("Week of {} {d}", MONTHS[m as usize - 1])
            } else {
                format!(
                    "{} {}",
                    MONTHS[key.rem_euclid(12) as usize],
                    key.div_euclid(12)
                )
            };
            (label, meters)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_days_round_trip_and_monday_weeks() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(
            civil_from_days(days_from_civil(2026, 11, 13)),
            (2026, 11, 13)
        );
        assert_eq!(civil_from_days(days_from_civil(2024, 2, 29)), (2024, 2, 29));
        // 2026-11-13 is Friday: week starts Monday 2026-11-09.
        assert_eq!(
            civil_from_days(days_from_civil(2026, 11, 13) - 4),
            (2026, 11, 9)
        );
    }

    #[test]
    fn months_group_and_sort_newest_first() {
        let day = 86_400_000;
        let rows = periods(&[(0, 1000.0), (40 * day, 500.0), (41 * day, 250.0)], false);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].1, 750.0);
    }
}
