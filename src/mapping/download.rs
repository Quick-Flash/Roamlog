//! Downloads offline map for an area from latest Protomaps daily build using HTTP range
//! requests, like `pmtiles extract`. Reads only directories and tiles inside area.
use crate::pmtiles::{self, Archive};
use std::{
    collections::HashMap,
    io::Write,
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

const BUILDS: &str = "https://build-metadata.protomaps.dev/builds.json";
const BASE: &str = "https://build.protomaps.com/";
/// Refuse areas whose tiles exceed this; keeps phone storage and data use sane.
const MAX_BYTES: u64 = 1 << 30;
/// Ranges closer than this merge into one request.
const GAP: u64 = 256 << 10;
const CHUNK: u64 = 16 << 20;

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(120)))
        .timeout_connect(Some(Duration::from_secs(15)))
        .https_only(true)
        .build()
        .into()
}

fn get(agent: &ureq::Agent, url: &str, range: Option<(u64, u64)>) -> Result<Vec<u8>, String> {
    let mut request = agent.get(url).header(
        "User-Agent",
        concat!(env!("CARGO_PKG_NAME"), "/", env!("CARGO_PKG_VERSION")),
    );
    let limit = match range {
        Some((offset, length)) => {
            request = request.header("Range", format!("bytes={offset}-{}", offset + length - 1));
            length
        }
        None => 16 << 20,
    };
    let mut response = request.call().map_err(|e| format!("Map download: {e}"))?;
    if range.is_some() && response.status().as_u16() != 206 {
        return Err("Map server ignored range request".into());
    }
    let bytes = response
        .body_mut()
        .with_config()
        .limit(limit)
        .read_to_vec()
        .map_err(|e| format!("Map download: {e}"))?;
    if range.is_some_and(|(_, length)| bytes.len() as u64 != length) {
        return Err("Map download truncated".into());
    }
    Ok(bytes)
}

/// Newest build file name from Protomaps build list.
fn latest(agent: &ureq::Agent) -> Result<String, String> {
    let builds: serde_json::Value =
        serde_json::from_slice(&get(agent, BUILDS, None)?).map_err(|e| e.to_string())?;
    builds
        .as_array()
        .and_then(|b| b.last())
        .and_then(|b| b["key"].as_str())
        .map(str::to_owned)
        .ok_or_else(|| "Map build list empty".into())
}

/// Tile x/y range covering `bounds` [min lon, min lat, max lon, max lat] at zoom `z`.
fn range(bounds: [f64; 4], z: u8) -> (u32, u32, u32, u32) {
    let n = f64::from(1_u32 << z);
    let x = |lon: f64| (((lon + 180.0) / 360.0 * n).floor().clamp(0.0, n - 1.0)) as u32;
    let y = |lat: f64| {
        let r = lat.clamp(-85.05, 85.05).to_radians();
        (((1.0 - r.tan().asinh() / std::f64::consts::PI) / 2.0 * n)
            .floor()
            .clamp(0.0, n - 1.0)) as u32
    };
    (x(bounds[0]), y(bounds[3]), x(bounds[2]), y(bounds[1]))
}

/// Download progress in tile bytes, polled by UI.
#[derive(Default)]
pub struct Progress {
    pub done: AtomicU64,
    pub total: AtomicU64,
}

/// Tiles to fetch for one area, sized before committing to download.
pub struct Plan {
    remote: Archive,
    /// (tile id, remote offset, length).
    tiles: Vec<(u64, u64, u32)>,
    bounds: [f64; 4],
    /// Tile bytes to download.
    pub bytes: u64,
}

/// Square `side_km` wide around `center` (lon, lat).
pub fn square(center: [f64; 2], side_km: f64) -> [f64; 4] {
    let half = side_km * 500.0 / 111_320.0;
    let wide = half / center[1].to_radians().cos().max(0.01);
    [
        (center[0] - wide).max(-180.0),
        (center[1] - half).max(-85.0),
        (center[0] + wide).min(180.0),
        (center[1] + half).min(85.0),
    ]
}

/// Looks up tiles for `bounds` in latest build; network reads directories only.
static REMOTE: std::sync::Mutex<Option<Arc<Archive>>> = std::sync::Mutex::new(None);

/// Latest Protomaps build read by HTTP range requests, for tiles beyond offline map.
/// Kept after first success; failures retry on next call.
pub fn remote() -> Result<Arc<Archive>, String> {
    if let Some(archive) = REMOTE.lock().map_err(|e| e.to_string())?.clone() {
        return Ok(archive);
    }
    let agent = agent();
    let url = format!("{BASE}{}", latest(&agent)?);
    let archive = Arc::new(Archive::from_source(Box::new(move |offset, length| {
        get(&agent, &url, Some((offset, length)))
    }))?);
    *REMOTE.lock().map_err(|e| e.to_string())? = Some(archive.clone());
    Ok(archive)
}

pub fn plan(bounds: [f64; 4]) -> Result<Plan, String> {
    // Directory lookups per tile get slow for huge areas; refuse before network use.
    let (x0, y0, x1, y1) = range(bounds, 15);
    if u64::from(x1 - x0 + 1) * u64::from(y1 - y0 + 1) > 20_000 {
        return Err("Area too large".into());
    }
    let agent = agent();
    let url = format!("{BASE}{}", latest(&agent)?);
    let remote = Archive::from_source(Box::new(move |offset, length| {
        get(&agent, &url, Some((offset, length)))
    }))?;
    let mut tiles = Vec::new();
    for z in 0..=remote.max_zoom {
        let (x0, y0, x1, y1) = range(bounds, z);
        for y in y0..=y1 {
            for x in x0..=x1 {
                if let Some((offset, length)) = remote.entry(z, x, y)? {
                    tiles.push((pmtiles::tile_id(z, x, y), offset, length));
                }
            }
        }
    }
    let mut unique: Vec<(u64, u32)> = tiles.iter().map(|t| (t.1, t.2)).collect();
    unique.sort_unstable();
    unique.dedup();
    let bytes = unique.iter().map(|r| u64::from(r.1)).sum();
    if bytes > MAX_BYTES {
        return Err(format!("Area needs {} MB; pick smaller area", bytes >> 20));
    }
    Ok(Plan {
        remote,
        tiles,
        bounds,
        bytes,
    })
}

/// Downloads `plan` and writes `out`, keeping tiles of `existing` map outside new area.
pub fn fetch(plan: Plan, existing: &Path, out: &Path, progress: &Progress) -> Result<(), String> {
    let Plan {
        remote,
        tiles,
        mut bounds,
        bytes,
    } = plan;
    progress.total.store(bytes, Ordering::Relaxed);
    let mut ranges: Vec<(u64, u32)> = tiles.iter().map(|t| (t.1, t.2)).collect();
    ranges.sort_unstable();
    ranges.dedup();
    let data_path = out.with_extension("data");
    let mut data = std::fs::File::create(&data_path).map_err(|e| e.to_string())?;
    let mut local: HashMap<u64, u64> = HashMap::new();
    let mut written = 0_u64;
    let mut i = 0;
    while i < ranges.len() {
        // Merge nearby ranges into one request.
        let start = ranges[i].0;
        let mut j = i + 1;
        while j < ranges.len()
            && ranges[j].0 <= ranges[j - 1].0 + u64::from(ranges[j - 1].1) + GAP
            && ranges[j].0 + u64::from(ranges[j].1) - start <= CHUNK
        {
            j += 1;
        }
        let end = ranges[j - 1].0 + u64::from(ranges[j - 1].1);
        let chunk = remote.read(start, end - start)?;
        for &(offset, length) in &ranges[i..j] {
            let at = (offset - start) as usize;
            data.write_all(&chunk[at..at + length as usize])
                .map_err(|e| e.to_string())?;
            local.insert(offset, written);
            written += u64::from(length);
            progress
                .done
                .fetch_add(u64::from(length), Ordering::Relaxed);
        }
        i = j;
    }
    let mut entries: Vec<(u64, u64, u32)> = tiles
        .iter()
        .map(|&(id, offset, length)| (id, local[&offset], length))
        .collect();
    // Keep previously downloaded areas: copy old tiles the new area doesn't replace.
    if let Ok(old) = Archive::open(existing)
        && old.compatible(&remote)
    {
        let fresh: std::collections::HashSet<u64> = entries.iter().map(|e| e.0).collect();
        let mut copied: HashMap<u64, u64> = HashMap::new();
        for (id, offset, length) in old.entries()? {
            if fresh.contains(&id) {
                continue;
            }
            let at = match copied.get(&offset) {
                Some(&at) => at,
                None => {
                    data.write_all(&old.read(offset, u64::from(length))?)
                        .map_err(|e| e.to_string())?;
                    copied.insert(offset, written);
                    written += u64::from(length);
                    written - u64::from(length)
                }
            };
            entries.push((id, at, length));
        }
        bounds = [
            bounds[0].min(old.bounds[0]),
            bounds[1].min(old.bounds[1]),
            bounds[2].max(old.bounds[2]),
            bounds[3].max(old.bounds[3]),
        ];
    }
    data.sync_all().map_err(|e| e.to_string())?;
    drop(data);
    entries.sort_unstable();
    let result = pmtiles::write(
        out,
        &remote,
        &entries,
        &data_path,
        (0, remote.max_zoom),
        bounds,
    );
    let _ = std::fs::remove_file(&data_path);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tile_range_covers_bounds() {
        // Downtown Portland fixture tile at z15.
        let r = range([-122.679, 45.515, -122.676, 45.517], 15);
        assert_eq!((r.0, r.1), (5217, 11720));
        assert!(r.2 >= r.0 && r.3 >= r.1);
        assert_eq!(range([-180.0, -85.0, 180.0, 85.0], 0), (0, 0, 0, 0));
    }

    #[test]
    fn fetch_merges_new_area_into_existing_map() {
        let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/downtown.pmtiles");
        let dir = tempfile::tempdir().unwrap();
        let plan_for = |z: u8, x: u32, y: u32| {
            let remote = Archive::open(&fixture).unwrap();
            let (offset, length) = remote.entry(z, x, y).unwrap().unwrap();
            Plan {
                remote,
                tiles: vec![(pmtiles::tile_id(z, x, y), offset, length)],
                bounds: [-122.68, 45.51, -122.67, 45.52],
                bytes: u64::from(length),
            }
        };
        let (map, partial) = (dir.path().join("map.pmtiles"), dir.path().join("tmp"));
        let progress = Progress::default();
        fetch(plan_for(15, 5217, 11720), &map, &partial, &progress).unwrap();
        std::fs::rename(&partial, &map).unwrap();
        fetch(plan_for(14, 2608, 5860), &map, &partial, &progress).unwrap();
        let merged = Archive::open(&partial).unwrap();
        assert!(
            merged.tile(15, 5217, 11720).unwrap().is_some(),
            "old area kept"
        );
        assert!(
            merged.tile(14, 2608, 5860).unwrap().is_some(),
            "new area added"
        );
        assert_eq!(merged.entries().unwrap().len(), 2);
    }

    #[test]
    fn square_is_side_km_wide() {
        let b = square([-122.67, 45.52], 30.0);
        let south_west = crate::geo::Point {
            lon: b[0],
            lat: b[1],
        };
        let south_east = crate::geo::Point {
            lon: b[2],
            lat: b[1],
        };
        let north_west = crate::geo::Point {
            lon: b[0],
            lat: b[3],
        };
        assert!((south_west.distance(south_east) / 1000.0 - 30.0).abs() < 0.5);
        assert!((south_west.distance(north_west) / 1000.0 - 30.0).abs() < 0.5);
    }
}
