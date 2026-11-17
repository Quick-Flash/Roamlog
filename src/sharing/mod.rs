//! Opt-in encrypted friend pairing and permission-controlled location sharing.

#[cfg(all(target_os = "android", feature = "ui"))]
pub mod android_app;
#[cfg(target_os = "android")]
mod android_friends;
#[cfg(target_os = "android")]
mod android_locations;
#[cfg(target_os = "android")]
mod android_probe;
pub mod groups;
#[cfg(target_os = "android")]
pub mod identity;
pub mod location;
#[cfg(test)]
#[path = "tests.rs"]
mod location_tests;
pub mod manager;
pub mod node;
pub mod pairing;
pub mod peers;
pub mod places;
pub mod probe;
pub mod protocol;
pub mod requests;
pub mod transport;

use std::{error::Error, io, time::Duration};

use iroh::{
    Endpoint, EndpointAddr, EndpointId, SecretKey,
    endpoint::{Connection, presets},
};

pub type Result<T> = std::result::Result<T, Box<dyn Error + Send + Sync>>;

/// Changing wire format requires a new ALPN.
pub const ALPN: &[u8] = b"roamlog/probe/1";
const VERSION: u8 = 1;
const FRAME_LEN: usize = 10;
const TIMEOUT: Duration = Duration::from_secs(30);
const CONNECT_ATTEMPTS: u32 = 6;

fn retry_delay(failure: u32) -> Duration {
    Duration::from_secs(1u64 << failure.min(4))
}

/// Caller owns secure identity storage. Binding never enables location sharing.
pub async fn bind(secret_key: SecretKey) -> Result<Endpoint> {
    Ok(Endpoint::builder(presets::N0)
        .secret_key(secret_key)
        .alpns(vec![ALPN.to_vec()])
        .bind()
        .await?)
}

/// Dial or reconnect to the same authenticated identity, with bounded backoff.
/// Six attempts, each limited to 30 seconds; delays are 1, 2, 4, 8, 16 seconds.
/// Closing the endpoint or dropping this future cancels pending attempts/waits.
/// Application protocol and authorization errors are never retried here.
pub async fn connect(endpoint: &Endpoint, address: EndpointAddr) -> Result<Connection> {
    connect_alpn(endpoint, address, ALPN).await
}

/// TLS no_application_protocol alert (0x100 + 120), surfaced by QUIC.
fn protocol_mismatch(mut error: &(dyn Error + 'static)) -> bool {
    loop {
        if let Some(error) = error.downcast_ref::<iroh::endpoint::ConnectionError>() {
            match error {
                iroh::endpoint::ConnectionError::TransportError(e)
                    if u64::from(e.code) == 0x178 =>
                {
                    return true;
                }
                iroh::endpoint::ConnectionError::ConnectionClosed(e)
                    if u64::from(e.error_code) == 0x178 =>
                {
                    return true;
                }
                _ => {}
            }
        }
        match error.source() {
            Some(source) => error = source,
            None => return false,
        }
    }
}

pub(super) async fn connect_alpn(
    endpoint: &Endpoint,
    address: EndpointAddr,
    alpn: &[u8],
) -> Result<Connection> {
    tokio::select! {
        biased;
        _ = endpoint.closed() => {
            Err(io::Error::new(io::ErrorKind::NotConnected, "sharing endpoint closed").into())
        }
        result = async {
            for attempt in 0..CONNECT_ATTEMPTS {
                let error: Box<dyn Error + Send + Sync> =
                    match tokio::time::timeout(TIMEOUT, endpoint.connect(address.clone(), alpn)).await {
                        Ok(Ok(connection)) => return Ok(connection),
                        Ok(Err(error)) => error.into(),
                        Err(error) => error.into(),
                    };
                if protocol_mismatch(error.as_ref()) {
                    return Err(io::Error::new(io::ErrorKind::Unsupported, "Update Roamlog").into());
                }
                if attempt + 1 == CONNECT_ATTEMPTS {
                    return Err(error);
                }
                tokio::time::sleep(retry_delay(attempt)).await;
            }
            unreachable!("at least one connection attempt")
        } => result,
    }
}

fn frame(kind: u8, nonce: u64) -> [u8; FRAME_LEN] {
    let mut bytes = [0; FRAME_LEN];
    bytes[0] = VERSION;
    bytes[1] = kind;
    bytes[2..].copy_from_slice(&nonce.to_be_bytes());
    bytes
}

fn parse(bytes: &[u8], kind: u8) -> Result<u64> {
    if bytes.len() != FRAME_LEN || bytes[0] != VERSION || bytes[1] != kind {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "invalid probe frame").into());
    }
    Ok(u64::from_be_bytes(bytes[2..].try_into()?))
}

/// Exchange bounded, versioned messages over an authenticated connection.
pub async fn ping(connection: &Connection, nonce: u64) -> Result<()> {
    tokio::time::timeout(TIMEOUT, async {
        let (mut send, mut recv) = connection.open_bi().await?;
        send.write_all(&frame(1, nonce)).await?;
        send.finish()?;
        let response = recv.read_to_end(FRAME_LEN).await?;
        if parse(&response, 2)? != nonce {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "probe nonce mismatch").into());
        }
        Ok(())
    })
    .await?
}

/// Reject unapproved identities before opening an application stream.
pub async fn respond(connection: &Connection, expected: EndpointId) -> Result<()> {
    if connection.remote_id() != expected {
        connection.close(1u32.into(), b"peer not authorized");
        return Err(io::Error::new(io::ErrorKind::PermissionDenied, "peer not authorized").into());
    }
    tokio::time::timeout(TIMEOUT, async {
        let (mut send, mut recv) = connection.accept_bi().await?;
        let request = recv.read_to_end(FRAME_LEN).await?;
        send.write_all(&frame(2, parse(&request, 1)?)).await?;
        send.finish()?;
        send.stopped().await?;
        Ok(())
    })
    .await?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_invalid_frames() {
        assert_eq!(parse(&frame(1, 42), 1).unwrap(), 42);
        for bytes in [vec![], vec![1; 11], frame(2, 42).to_vec()] {
            assert!(parse(&bytes, 1).is_err());
        }
        let mut wrong_version = frame(1, 42);
        wrong_version[0] = 2;
        assert!(parse(&wrong_version, 1).is_err());
    }

    #[test]
    fn retry_delays_are_capped() {
        let seconds: Vec<_> = (0..7).map(|n| retry_delay(n).as_secs()).collect();
        assert_eq!(seconds, [1, 2, 4, 8, 16, 16, 16]);
        assert_eq!(retry_delay(u32::MAX), Duration::from_secs(16));
    }

    async fn local_endpoint() -> Endpoint {
        Endpoint::builder(presets::Minimal)
            .alpns(vec![ALPN.to_vec()])
            .bind()
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn authenticated_round_trip() {
        let server = local_endpoint().await;
        let client = local_endpoint().await;
        let expected = client.id();
        let accepted = async {
            let connection = server.accept().await.unwrap().await.unwrap();
            assert_eq!(connection.remote_id(), expected);
            respond(&connection, expected).await.unwrap();
        };
        let dialing = async {
            let connection = client.connect(server.addr(), ALPN).await.unwrap();
            ping(&connection, 42).await.unwrap();
        };
        tokio::time::timeout(TIMEOUT, async {
            tokio::join!(accepted, dialing);
        })
        .await
        .unwrap();
        client.close().await;
        server.close().await;
    }

    #[tokio::test]
    async fn rejects_unapproved_peer() {
        let server = local_endpoint().await;
        let client = local_endpoint().await;
        let expected = SecretKey::generate().public();
        let accepted = async {
            let connection = server.accept().await.unwrap().await.unwrap();
            assert!(respond(&connection, expected).await.is_err());
        };
        let dialing = async {
            let connection = client.connect(server.addr(), ALPN).await.unwrap();
            assert!(ping(&connection, 42).await.is_err());
        };
        tokio::time::timeout(TIMEOUT, async {
            tokio::join!(accepted, dialing);
        })
        .await
        .unwrap();
        client.close().await;
        server.close().await;
    }

    #[tokio::test]
    async fn retries_refused_connection() {
        let server = local_endpoint().await;
        let client = local_endpoint().await;
        let accepted = async {
            server.accept().await.unwrap().refuse();
            let connection = server.accept().await.unwrap().await.unwrap();
            respond(&connection, client.id()).await.unwrap();
        };
        let dialing = async {
            let connection = connect(&client, server.addr()).await.unwrap();
            ping(&connection, 99).await.unwrap();
        };
        tokio::time::timeout(Duration::from_secs(5), async {
            tokio::join!(accepted, dialing);
        })
        .await
        .unwrap();
        client.close().await;
        server.close().await;
    }

    #[tokio::test]
    async fn closing_endpoint_cancels_retry_wait() {
        let server = local_endpoint().await;
        let client = local_endpoint().await;
        let accepted = async {
            server.accept().await.unwrap().refuse();
            // Allow failed attempt to enter its one-second retry wait.
            tokio::time::sleep(Duration::from_millis(100)).await;
            client.close().await;
        };
        let dialing = async {
            assert!(connect(&client, server.addr()).await.is_err());
        };
        tokio::time::timeout(Duration::from_millis(750), async {
            tokio::join!(accepted, dialing);
        })
        .await
        .unwrap();
        server.close().await;
    }
}
