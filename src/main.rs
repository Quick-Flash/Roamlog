use roamlog::{coverage, model::Event, store::Store};
use std::{
    io::{self, BufRead},
    path::PathBuf,
};

fn main() {
    #[cfg(feature = "ui")]
    if std::env::args_os().len() == 1 {
        let directory = std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".local/share")
            })
            // Existing desktop data stays in same directory after branding rename.
            .join("traveler");
        if let Err(error) = roamlog::ui::run(directory) {
            eprintln!("{error}");
            std::process::exit(1);
        }
        return;
    }
    if let Err(error) = run() {
        eprintln!("{error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let mut args = std::env::args_os().skip(1);
    let path = PathBuf::from(
        args.next()
            .ok_or("Usage: roamlog JOURNAL [ROADS.geojson] < events.jsonl")?,
    );
    let roads = args
        .next()
        .map(std::fs::read)
        .transpose()
        .map_err(|e| e.to_string())?
        .map(|b| coverage::import_geojson(&b))
        .transpose()?;
    let mut store = Store::open(path)?;
    store.recover_recording()?;
    for line in io::stdin().lock().lines() {
        let line = line.map_err(|e| e.to_string())?;
        if line.trim().is_empty() {
            continue;
        }
        let event: Event = serde_json::from_str(&line).map_err(|e| e.to_string())?;
        store.commit(event)?;
    }
    let road_sections = roads.map(|r| {
        let matched = roamlog::recovery::Recovery::new(&r).coverage_state(&store.state);
        coverage::sections(&r, &matched)
    });
    println!(
        "{}",
        serde_json::to_string_pretty(
            &serde_json::json!({"state": store.state, "sections": road_sections})
        )
        .map_err(|e| e.to_string())?
    );
    Ok(())
}
