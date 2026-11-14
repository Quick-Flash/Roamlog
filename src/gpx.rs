//! GPX 1.1 export of recorded trips. Path breaks (pause, GPS gap) start new segments.
use crate::model::State;
use std::fmt::Write;

pub fn export(state: &State) -> String {
    let mut out = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <gpx version=\"1.1\" creator=\"Roamlog\" xmlns=\"http://www.topografix.com/GPX/1/1\" xmlns:traveler=\"urn:traveler:track:1\">\n",
    );
    for trip in &state.trips {
        let category = state
            .categories
            .iter()
            .find(|c| Some(c.id) == trip.category_id)
            .map_or("Uncategorized", |c| c.name.as_str());
        let _ = write!(
            out,
            "<trk><name>{} · {}</name><type>{}</type>",
            escape(&trip.title()),
            escape(category),
            escape(category)
        );
        for (i, p) in trip.points.iter().enumerate() {
            if i == 0 || !p.connected {
                if i > 0 {
                    out.push_str("</trkseg>");
                }
                out.push_str("<trkseg>");
            }
            let _ = write!(
                out,
                "<trkpt lat=\"{:.7}\" lon=\"{:.7}\"><time>{}</time>",
                p.fix.point.lat,
                p.fix.point.lon,
                iso(p.fix.timestamp_ms)
            );
            if p.estimated {
                let _ = write!(
                    out,
                    "<extensions><traveler:estimated>true</traveler:estimated><traveler:road_matched>{}</traveler:road_matched></extensions>",
                    p.road_matched
                );
            }
            out.push_str("</trkpt>");
        }
        if !trip.points.is_empty() {
            out.push_str("</trkseg>");
        }
        out.push_str("</trk>\n");
    }
    out.push_str("</gpx>\n");
    out
}

fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// UTC timestamp `YYYY-MM-DDTHH:MM:SSZ` (Howard Hinnant's civil-from-days).
fn iso(ms: u64) -> String {
    let secs = ms / 1000;
    let (days, rem) = ((secs / 86_400) as i64, secs % 86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        rem / 60 % 60,
        rem % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iso_dates_match_known_instants() {
        assert_eq!(iso(0), "1970-01-01T00:00:00Z");
        assert_eq!(iso(951_782_400_000), "2000-02-29T00:00:00Z");
        assert_eq!(iso(1_794_528_000_000), "2026-11-13T00:00:00Z");
    }

    #[test]
    fn segments_split_at_path_breaks_and_names_escape() {
        let mut state: State = serde_json::from_value(serde_json::json!({
            "categories": [{"id": 1, "name": "Walk & run"}],
            "trips": [{
                "id": 1, "category_id": 1, "started_ms": 0, "finished_ms": 9, "status": "finished",
                "distance_m": 0.0, "elapsed_ms": 0, "recording_since_ms": null, "paused_at_ms": 9,
                "points": [
                    {"lat": 1.0, "lon": 2.0, "timestamp_ms": 0, "accuracy_m": 3.0, "connected": false},
                    {"lat": 1.1, "lon": 2.0, "timestamp_ms": 1000, "accuracy_m": 3.0, "connected": true},
                    {"lat": 1.2, "lon": 2.0, "timestamp_ms": 9000, "accuracy_m": 3.0, "connected": false}
                ]
            }]
        }))
        .unwrap();
        let gpx = export(&state);
        assert_eq!(gpx.matches("<trkseg>").count(), 2);
        assert_eq!(gpx.matches("<trkpt").count(), 3);
        assert!(gpx.contains("Walk &amp; run"));
        state.trips.clear();
        assert!(!export(&state).contains("<trk>"));
    }
}
