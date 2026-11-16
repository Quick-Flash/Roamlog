//! Which friend each added trip came from, kept beside the journal (trip ID → peer ID).
//! Journal stays unchanged; a missing or unreadable file means no friend filters.
use std::{collections::BTreeMap, path::Path};

fn path(directory: &Path) -> std::path::PathBuf {
    directory.join("trip-sources.json")
}

pub fn load(directory: &Path) -> BTreeMap<u64, String> {
    std::fs::read(path(directory))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

#[cfg_attr(not(target_os = "android"), allow(dead_code))]
pub fn add(directory: &Path, trip_id: u64, peer: &str) {
    let mut sources = load(directory);
    sources.insert(trip_id, peer.to_owned());
    let partial = path(directory).with_extension("tmp");
    if let Ok(bytes) = serde_json::to_vec(&sources)
        && std::fs::write(&partial, bytes).is_ok()
    {
        let _ = std::fs::rename(partial, path(directory));
    }
}
