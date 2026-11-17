//! Bounded UI-to-recorder requests; results describe fresh GPS, not cached datagrams.
use super::{Result, peers::Friends};
use iroh::EndpointId;
use serde::{Deserialize, Serialize};
use std::{
    io::{Read, Write},
    path::Path,
};

#[derive(Clone, Serialize, Deserialize)]
pub struct Request {
    pub at_ms: i64,
    pub attempts: u8,
    pub sent_at_ms: i64,
    pub status: String,
}
fn path(friends: &Friends, peer: EndpointId) -> std::path::PathBuf {
    friends
        .directory()
        .join("location-requests")
        .join(format!("{peer}.json"))
}
pub fn read(friends: &Friends, peer: EndpointId) -> Option<Request> {
    let file = std::fs::File::open(path(friends, peer)).ok()?;
    let mut bytes = Vec::new();
    file.take(2049).read_to_end(&mut bytes).ok()?;
    if bytes.len() > 2048 {
        return None;
    }
    serde_json::from_slice(&bytes).ok()
}
pub(super) fn write(friends: &Friends, peer: EndpointId, request: &Request) -> Result<()> {
    let path = path(friends, peer);
    let dir = path.parent().ok_or("invalid location request")?;
    std::fs::create_dir_all(dir)?;
    atomic_json(&path, &serde_json::to_vec(request)?)
}
pub(super) fn atomic_json(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut file = tempfile::NamedTempFile::new_in(path.parent().ok_or("invalid path")?)?;
    file.write_all(bytes)?;
    file.as_file().sync_all()?;
    file.persist(path)?;
    Ok(())
}
pub fn queue(friends: &Friends, peer: EndpointId, now: i64) -> Result<()> {
    let _lock = friends.lock()?;
    if !friends
        .list_locked()?
        .iter()
        .any(|f| f.peer_id == peer && !f.blocked)
    {
        return Err("friend removed or blocked".into());
    }
    if read(friends, peer).is_some_and(|r| now.saturating_sub(r.at_ms) < 60_000) {
        return Err("Try again in one minute".into());
    }
    write(
        friends,
        peer,
        &Request {
            at_ms: now,
            attempts: 0,
            sent_at_ms: 0,
            status: "Locating…".into(),
        },
    )
}
