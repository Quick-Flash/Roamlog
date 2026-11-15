//! Foreground connectivity diagnostics shared by desktop and Android harnesses.
use std::time::Duration;

use iroh::{
    Endpoint, EndpointAddr, EndpointId,
    endpoint::{Connection, ConnectionError},
};

use super::{Result, connect, ping, respond};

fn selected_path(connection: &Connection) -> &'static str {
    let paths = connection.paths();
    match paths.iter().find(|path| path.is_selected()) {
        Some(path) if path.is_ip() => "authenticated probe succeeded; selected path: direct",
        Some(path) if path.is_relay() => "authenticated probe succeeded; selected path: relay",
        _ => "authenticated probe succeeded; selected path: unknown",
    }
}

pub async fn client(
    endpoint: &Endpoint,
    address: EndpointAddr,
    watch: bool,
    mut report: impl FnMut(&str) -> Result<()>,
) -> Result<()> {
    let mut nonce = 0u64;
    loop {
        let connection = connect(endpoint, address.clone()).await?;
        loop {
            nonce = nonce.checked_add(1).ok_or("probe nonce exhausted")?;
            if let Err(error) = ping(&connection, nonce).await {
                let closed = connection.close_reason();
                if !watch
                    || matches!(closed, Some(ConnectionError::ApplicationClosed(_)))
                    || (closed.is_none()
                        && error
                            .downcast_ref::<tokio::time::error::Elapsed>()
                            .is_none())
                {
                    return Err(error);
                }
                connection.close(0u32.into(), b"probe reconnecting");
                report("probe connection lost; reconnecting")?;
                tokio::time::sleep(Duration::from_secs(1)).await;
                break;
            }
            report(selected_path(&connection))?;
            if !watch {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_secs(10)).await;
        }
    }
}

pub async fn server(
    endpoint: &Endpoint,
    expected: EndpointId,
    continuous: bool,
    mut report: impl FnMut(&str) -> Result<()>,
) -> Result<()> {
    loop {
        let incoming = endpoint.accept().await.ok_or("endpoint closed")?;
        let result: Result<()> = async {
            let connection =
                tokio::time::timeout(Duration::from_secs(30), async { incoming.await }).await??;
            loop {
                respond(&connection, expected).await?;
                report(selected_path(&connection))?;
                if !continuous {
                    return Ok(());
                }
            }
        }
        .await;
        if !continuous {
            return result;
        }
        if result.is_err() {
            report("probe session ended; waiting for next connection")?;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sharing::{ALPN, ping};
    use iroh::endpoint::presets;

    async fn endpoint() -> Endpoint {
        Endpoint::builder(presets::Minimal)
            .alpns(vec![ALPN.to_vec()])
            .bind()
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn listener_survives_unapproved_peer() {
        let listener = endpoint().await;
        let approved = endpoint().await;
        let stranger = endpoint().await;
        let serving = server(&listener, approved.id(), true, |_| Ok(()));
        let scenario = async {
            let connection = stranger.connect(listener.addr(), ALPN).await.unwrap();
            assert!(ping(&connection, 1).await.is_err());
            let connection = approved.connect(listener.addr(), ALPN).await.unwrap();
            ping(&connection, 2).await.unwrap();
            // Multiple probes must reuse the approved connection.
            ping(&connection, 3).await.unwrap();
        };
        tokio::time::timeout(Duration::from_secs(5), async {
            tokio::select! {
                result = serving => panic!("listener stopped: {result:?}"),
                _ = scenario => {},
            }
        })
        .await
        .unwrap();
        stranger.close().await;
        approved.close().await;
        listener.close().await;
    }
}
