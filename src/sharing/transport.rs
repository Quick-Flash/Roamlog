//! Authenticated QUIC sessions. Reliable Hello; bounded encrypted datagrams thereafter.
use super::{
    Result, connect_alpn,
    manager::{Manager, now_ms},
    protocol::{self, ALPN, Availability, MAX_FRAME, Message},
};
use iroh::{
    Endpoint, EndpointAddr, SecretKey,
    endpoint::{Connection, ConnectionError},
};
use std::{io, time::Duration};
use tokio::{
    task::JoinSet,
    time::{Instant, MissedTickBehavior},
};

const MAX_GROUP_SYNC: usize = 256 * 1024;
const MAX_TRIPS: usize = 16 * 1024 * 1024;
const TRIPS_REQUEST: &[u8] = b"trips/1";
const TRIP_SYNC: Duration = Duration::from_secs(6 * 3600);
const AVATAR_REQUEST: &[u8] = b"avatar/1";
const AVATAR_SYNC: Duration = Duration::from_secs(30 * 60);
const MAX_AVATAR: usize = 256 * 1024;

/// UI process asks for a friend's trips by creating this file; node answers in `friend-trips`.
pub fn trip_request_path(
    directory: &std::path::Path,
    peer: iroh::EndpointId,
) -> std::path::PathBuf {
    directory.join("trip-requests").join(peer.to_string())
}

/// Friend's trips as a read-only journal, so History computes their miles and coverage
/// with the same code as own trips. Never merged into own journal.
pub fn friend_history_path(
    directory: &std::path::Path,
    peer: iroh::EndpointId,
) -> std::path::PathBuf {
    directory
        .join("friend-history")
        .join(format!("{peer}.jsonl"))
}

/// Replays shared trips (oldest first) as Start, Fix…, Finish, RenameTrip events.
pub fn history_journal(trips: &[u8]) -> Result<Vec<u8>> {
    #[derive(serde::Deserialize)]
    struct Shared {
        #[serde(default)]
        name: String,
        fixes: Vec<crate::model::Fix>,
    }
    #[derive(serde::Deserialize)]
    struct Reply {
        trips: Vec<Shared>,
    }
    use crate::model::Event;
    let mut reply: Reply = serde_json::from_slice(trips)?;
    reply.trips.retain(|t| !t.fixes.is_empty());
    reply.trips.sort_by_key(|t| t.fixes[0].timestamp_ms);
    let mut state = crate::model::State::default();
    let mut out = Vec::new();
    for trip in reply.trips {
        let first = trip.fixes[0].timestamp_ms;
        let last = trip.fixes[trip.fixes.len() - 1].timestamp_ms;
        let mut events = vec![Event::Start {
            timestamp_ms: first,
            category_id: None,
        }];
        events.extend(trip.fixes.into_iter().map(|fix| Event::Fix { fix }));
        events.push(Event::Finish { timestamp_ms: last });
        // Trip that does not replay (overlap, bad fix) is skipped, not the whole history.
        let mut trial = state.clone();
        if events.iter().all(|e| {
            let ok = trial.validate(e).is_ok();
            if ok {
                trial.apply(e.clone());
            }
            ok
        }) {
            if !trip.name.is_empty()
                && let Some(id) = trial.trips.last().map(|t| t.id)
            {
                events.push(Event::RenameTrip {
                    trip_id: id,
                    name: trip.name.chars().take(120).collect(),
                });
                trial.apply(events.last().cloned().ok_or("empty")?);
            }
            for event in &events {
                out.extend(serde_json::to_vec(event)?);
                out.push(b'\n');
            }
            state = trial;
        }
    }
    Ok(out)
}

fn write_atomic(path: &std::path::Path, body: &[u8]) {
    if let Some(parent) = path.parent()
        && std::fs::create_dir_all(parent).is_ok()
        && let Ok(mut file) = tempfile::NamedTempFile::new_in(parent)
        && std::io::Write::write_all(&mut file, body).is_ok()
    {
        let _ = file.persist(path);
    }
}

pub fn friend_trips_path(
    directory: &std::path::Path,
    peer: iroh::EndpointId,
) -> std::path::PathBuf {
    directory.join("friend-trips").join(format!("{peer}.json"))
}

/// Fetches `connection` peer's trips; denial is stored as an error message for the UI.
async fn fetch_trips(connection: Connection, directory: std::path::PathBuf) {
    let peer = connection.remote_id();
    let result: Result<Vec<u8>> = tokio::time::timeout(Duration::from_secs(120), async {
        let (mut send, mut recv) = connection.open_bi().await?;
        send.write_all(TRIPS_REQUEST).await?;
        send.finish()?;
        let bytes = recv.read_to_end(MAX_TRIPS).await?;
        serde_json::from_slice::<serde_json::Value>(&bytes)?;
        Ok(bytes)
    })
    .await
    .unwrap_or_else(|_| Err("timed out".into()));
    match result {
        Ok(body) => {
            if let Ok(journal) = history_journal(&body) {
                write_atomic(&friend_history_path(&directory, peer), &journal);
            }
            write_atomic(&friend_trips_path(&directory, peer), &body);
        }
        // Keep last good copy; only say why when there is nothing to show.
        Err(_) if !friend_trips_path(&directory, peer).exists() => write_atomic(
            &friend_trips_path(&directory, peer),
            br#"{"error":"Friend does not share trips with you, or is unreachable"}"#,
        ),
        Err(_) => {}
    }
    let _ = std::fs::remove_file(trip_request_path(&directory, peer));
}

/// Own profile picture: small PNG friends show as this phone's map marker.
pub fn own_avatar_path(directory: &std::path::Path) -> std::path::PathBuf {
    directory.join("avatar.png")
}

/// Friend's picture as last fetched; absent when they have none.
pub fn friend_avatar_path(
    directory: &std::path::Path,
    peer: iroh::EndpointId,
) -> std::path::PathBuf {
    directory.join("avatars").join(format!("{peer}.png"))
}

async fn fetch_avatar(connection: Connection, directory: std::path::PathBuf) {
    let peer = connection.remote_id();
    let result: Result<Vec<u8>> = tokio::time::timeout(Duration::from_secs(60), async {
        let (mut send, mut recv) = connection.open_bi().await?;
        send.write_all(AVATAR_REQUEST).await?;
        send.finish()?;
        Ok(recv.read_to_end(MAX_AVATAR).await?)
    })
    .await
    .unwrap_or_else(|_| Err("timed out".into()));
    let path = friend_avatar_path(&directory, peer);
    match result {
        // PNG signature only; UI decodes it again with size limits.
        Ok(bytes) if bytes.starts_with(b"\x89PNG") => write_atomic(&path, &bytes),
        Ok(bytes) if bytes.is_empty() => {
            let _ = std::fs::remove_file(path);
        }
        _ => {}
    }
}

async fn answer_trips(
    manager: Manager,
    peer: iroh::EndpointId,
    mut send: iroh::endpoint::SendStream,
    mut recv: iroh::endpoint::RecvStream,
) -> Result<()> {
    let request = recv.read_to_end(64).await?;
    if request == AVATAR_REQUEST {
        // Any trusted friend may see the picture; session is already authorized.
        let path = own_avatar_path(manager.friends().directory());
        let bytes = std::fs::read(path).unwrap_or_default();
        send.write_all(&bytes[..bytes.len().min(MAX_AVATAR)])
            .await?;
        send.finish()?;
        send.stopped().await?;
        return Ok(());
    }
    if request != TRIPS_REQUEST {
        return Err("unknown request".into());
    }
    let bytes = tokio::task::spawn_blocking(move || manager.trips_for(peer)).await??;
    send.write_all(&bytes).await?;
    send.finish()?;
    send.stopped().await?;
    Ok(())
}

pub async fn bind(key: SecretKey) -> Result<Endpoint> {
    Ok(Endpoint::builder(iroh::endpoint::presets::N0)
        .secret_key(key)
        .alpns(vec![ALPN.to_vec()])
        .bind()
        .await?)
}

struct Session {
    manager: Manager,
    connection: Connection,
}
impl Drop for Session {
    fn drop(&mut self) {
        self.manager.unregister(&self.connection);
        self.connection
            .close(0u32.into(), b"location session ended");
    }
}

async fn hello(connection: &Connection, initiator: bool) -> Result<()> {
    tokio::time::timeout(Duration::from_secs(30), async {
        let (mut send, mut recv) = if initiator {
            connection.open_bi().await?
        } else {
            connection.accept_bi().await?
        };
        if initiator {
            send.write_all(&protocol::encode(&Message::Hello)?).await?;
            send.finish()?;
        }
        let message = protocol::decode(&recv.read_to_end(MAX_FRAME).await?, now_ms())?;
        if message != Message::Hello {
            return Err("expected location hello".into());
        }
        if !initiator {
            send.write_all(&protocol::encode(&Message::Hello)?).await?;
            send.finish()?;
            send.stopped().await?;
        }
        Ok(())
    })
    .await?
}

pub async fn session(
    endpoint: &Endpoint,
    connection: Connection,
    manager: Manager,
    initiator: bool,
) -> Result<()> {
    if endpoint.id() != manager.owner()
        || connection.alpn() != ALPN
        || !manager.is_trusted(connection.remote_id())?
    {
        connection.close(1u32.into(), b"location access denied");
        return Err("untrusted location session".into());
    }
    let _session = Session {
        manager: manager.clone(),
        connection: connection.clone(),
    };
    let result = async {
        hello(&connection, initiator).await?;
        manager.register(&connection, initiator)?;
        // Datagrams may reorder or drop. Initial request is repeated on next periodic check
        // until a location arrives; movement/rate policy still applies on sender.
        manager.control(&connection, &Message::LocationRequest)?;
        let mut changed = manager.changes();
        let mut tick = tokio::time::interval(Duration::from_secs(10));
        tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
        let mut last_status: Option<(bool, Availability)> = None;
        let mut last_ping = Instant::now();
        let mut nonce = 0u64;
        let mut synced: Option<Vec<u8>> = None;
        let mut fetching = false;
        let mut last_sync: Option<Instant> = None;
        let mut last_avatar: Option<Instant> = None;
        loop {
            let mut requested = false;
            tokio::select! {
                biased;
                _ = endpoint.closed() => return Ok(()),
                stream = connection.accept_bi() => {
                    let (send, recv) = stream?;
                    let (manager, peer) = (manager.clone(), connection.remote_id());
                    tokio::spawn(async move {
                        let _ = answer_trips(manager, peer, send, recv).await;
                    });
                },
                stream = connection.accept_uni() => {
                    // Group member list; reliable stream, bounded like friends file entries.
                    let bytes = tokio::time::timeout(Duration::from_secs(30), stream?.read_to_end(MAX_GROUP_SYNC)).await??;
                    manager.apply_group_sync(connection.remote_id(), &bytes)?;
                },
                packet = connection.read_datagram() => {
                    let message = protocol::decode(&packet?, now_ms())?;
                    if !manager.ingest(connection.remote_id(), &message, now_ms())? { continue; }
                    match message {
                        Message::LocationRequest => requested = true,
                        Message::Ping { nonce } => manager.control(&connection, &Message::Pong { nonce })?,
                        Message::Hello => return Err("unexpected hello".into()),
                        _ => {},
                    }
                },
                result = changed.changed() => { result?; },
                _ = tick.tick() => {},
            }
            if !manager.is_trusted(connection.remote_id())? { return Err("friend removed or blocked".into()); }
            let status = manager.availability(connection.remote_id(), now_ms())?;
            if last_status != Some(status) || last_ping.elapsed() >= Duration::from_secs(30) {
                manager.control(&connection, &Message::PermissionUpdate { allowed: status.0 })?;
                manager.control(&connection, &Message::SharingStatus(status.1))?;
                last_status = Some(status);
            }
            manager.send_latest(&connection, now_ms(), requested)?;
            let directory = manager.friends().directory().to_owned();
            // Sync friend's trips on connect and every 6 hours; UI can also ask for a refresh.
            let auto = last_sync.is_none_or(|at: Instant| at.elapsed() >= TRIP_SYNC);
            if (auto || trip_request_path(&directory, connection.remote_id()).exists()) && !fetching {
                fetching = true;
                last_sync = Some(Instant::now());
                tokio::spawn(fetch_trips(connection.clone(), directory.clone()));
            } else if !trip_request_path(&directory, connection.remote_id()).exists()
                && last_sync.is_some_and(|at| at.elapsed() > Duration::from_secs(120))
            {
                fetching = false;
            }
            if last_avatar.is_none_or(|at: Instant| at.elapsed() >= AVATAR_SYNC) {
                last_avatar = Some(Instant::now());
                tokio::spawn(fetch_avatar(connection.clone(), directory.clone()));
            }
            let sync = manager.group_sync(connection.remote_id())?;
            if synced.as_ref() != Some(&sync) && (synced.is_some() || sync != b"[]") {
                let mut stream = connection.open_uni().await?;
                stream.write_all(&sync).await?;
                stream.finish()?;
                synced = Some(sync);
            }
            if last_ping.elapsed() >= Duration::from_secs(30) {
                nonce = nonce.checked_add(1).ok_or("ping counter exhausted")?;
                manager.control(&connection, &Message::Ping { nonce })?;
                if !manager.has_location(connection.remote_id(), now_ms())? {
                    manager.control(&connection, &Message::LocationRequest)?;
                }
                last_ping = Instant::now();
            }
        }
    }.await;
    // Inspect transport state before Session's local close hides original reason.
    if result.is_err()
        && connection
            .close_reason()
            .is_some_and(|reason| !matches!(reason, ConnectionError::ApplicationClosed(_)))
    {
        return Err(
            io::Error::new(io::ErrorKind::ConnectionAborted, "location connection lost").into(),
        );
    }
    result
}

/// Reconnect only after network loss; protocol/auth failures stop this dialer.
pub async fn client(endpoint: &Endpoint, address: EndpointAddr, manager: Manager) -> Result<()> {
    if endpoint.id() != manager.owner() || !manager.is_trusted(address.id)? {
        return Err("peer not trusted".into());
    }
    loop {
        let connection = connect_alpn(endpoint, address.clone(), ALPN).await?;
        match session(endpoint, connection, manager.clone(), true).await {
            Ok(()) => return Ok(()),
            Err(error) => {
                let retry = error
                    .downcast_ref::<io::Error>()
                    .is_some_and(|error| error.kind() == io::ErrorKind::ConnectionAborted)
                    || error
                        .downcast_ref::<tokio::time::error::Elapsed>()
                        .is_some();
                if !retry || !manager.is_trusted(address.id)? {
                    return Err(error);
                }
            }
        }
        tokio::select! { _ = endpoint.closed() => return Ok(()), _ = tokio::time::sleep(Duration::from_secs(1)) => {} }
    }
}

/// Bounded concurrent sessions allow independent friends without blocking on one peer.
pub async fn server(endpoint: &Endpoint, manager: Manager) -> Result<()> {
    if endpoint.id() != manager.owner() {
        return Err("sharing identity mismatch".into());
    }
    let mut tasks = JoinSet::new();
    loop {
        tokio::select! {
            incoming = endpoint.accept() => {
                let Some(incoming) = incoming else { return Ok(()) };
                if tasks.len() >= 256 { incoming.refuse(); continue; }
                let endpoint = endpoint.clone(); let manager = manager.clone();
                tasks.spawn(async move {
                    if let Ok(Ok(connection)) = tokio::time::timeout(Duration::from_secs(30), async { incoming.await }).await {
                        let _ = session(&endpoint, connection, manager, false).await;
                    }
                });
            },
            _ = tasks.join_next(), if !tasks.is_empty() => {},
        }
    }
}
