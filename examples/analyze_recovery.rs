//! Local-only journal replay, recovery and coverage timing. Does not modify journal.
use roamlog::{
    coverage::Coverage, model::TripFilter, pmtiles::Archive, recovery::Recovery, roads,
    store::Store,
};
use std::{collections::BTreeSet, path::PathBuf, time::Instant};

fn main() -> Result<(), String> {
    let mut args = std::env::args_os().skip(1);
    let journal = PathBuf::from(
        args.next()
            .ok_or("Usage: analyze_recovery JOURNAL MAP [STATE_OUTPUT]")?,
    );
    let map = PathBuf::from(args.next().ok_or("Missing map")?);
    let store = Store::open(journal)?;
    let start = Instant::now();
    let archive = Archive::open(&map)?;
    let tiles = roads::tiles(&store.state, archive.max_zoom);
    let roads = roads::roads(&archive, &tiles)?;
    eprintln!(
        "{} map tiles, {} roads loaded in {:?}",
        tiles.len(),
        roads.len(),
        start.elapsed()
    );
    let t = Instant::now();
    let mut recovery = Recovery::new(&roads);
    eprintln!("road graph built in {:?}", t.elapsed());
    let t = Instant::now();
    let recovered = recovery.apply(&store.state);
    eprintln!("gap recovery in {:?}", t.elapsed());
    let t = Instant::now();
    let _ = recovery.apply(&store.state);
    eprintln!("cached recovery in {:?}", t.elapsed());
    let t = Instant::now();
    let mut coverage = Coverage::new(&roads);
    let matched = recovery.coverage_state(&store.state);
    coverage.update_filtered(&matched, TripFilter::All);
    eprintln!(
        "coverage {} sections in {:?}",
        coverage.sections.len(),
        t.elapsed()
    );
    let covered: BTreeSet<_> = coverage
        .sections
        .iter()
        .filter(|s| s.traveled)
        .map(|s| &s.road_id)
        .collect();
    for trip in &recovered.trips {
        let raw = store.state.trips.iter().find(|t| t.id == trip.id).unwrap();
        println!(
            "trip {}: {:.1} m -> {:.1} m, {:.1} m inferred, {} raw / {} rendered points, {} road-matched segments",
            trip.id,
            raw.distance_m,
            trip.distance_m,
            trip.estimated_distance_m,
            raw.points.len(),
            trip.points.len(),
            trip.points.iter().filter(|p| p.road_matched).count()
        );
    }
    println!(
        "{} traveled road ids, {} traveled sections, {:.1} m covered",
        covered.len(),
        coverage.sections.iter().filter(|s| s.traveled).count(),
        coverage
            .sections
            .iter()
            .filter(|s| s.traveled)
            .map(|s| s.start.distance(s.end))
            .sum::<f64>()
    );
    if let Some(output) = args.next() {
        std::fs::write(
            output,
            serde_json::to_vec(&recovered).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
    }
    if let Some(baseline) = args.next() {
        let value: serde_json::Value =
            serde_json::from_slice(&std::fs::read(baseline).map_err(|e| e.to_string())?)
                .map_err(|e| e.to_string())?;
        let baseline: roamlog::model::State =
            serde_json::from_value(value.get("state").unwrap_or(&value).clone())
                .map_err(|e| e.to_string())?;
        let mut old = Coverage::new(&roads);
        old.update(&baseline);
        let summarize = |sections: &[roamlog::coverage::Section]| {
            let distance: f64 = sections
                .iter()
                .filter(|s| s.traveled)
                .map(|s| s.start.distance(s.end))
                .sum();
            let ids: BTreeSet<_> = sections
                .iter()
                .filter(|s| s.traveled)
                .map(|s| s.road_id.as_str())
                .collect();
            (
                distance,
                ids.len(),
                sections.iter().filter(|s| s.traveled).count(),
            )
        };
        println!(
            "baseline vs recovered coverage (metres, road ids, sections): {:?} -> {:?}",
            summarize(&old.sections),
            summarize(&coverage.sections)
        );
    }
    Ok(())
}
