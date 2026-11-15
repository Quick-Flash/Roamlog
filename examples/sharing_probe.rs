//! Desktop-only connectivity harness. Identity file is private but NOT encrypted.
use std::{io::Write, path::Path, time::Duration};

use iroh::{EndpointAddr, EndpointId, SecretKey};
use roamlog::sharing::{self, Result};

fn report(message: &str) -> Result<()> {
    println!("{message}");
    Ok(())
}

async fn probe(endpoint: &iroh::Endpoint, address: EndpointAddr, watch: bool) -> Result<()> {
    sharing::probe::client(endpoint, address, watch, report).await
}

fn identity(path: &Path) -> Result<SecretKey> {
    match std::fs::read(path) {
        Ok(bytes) => {
            let bytes: [u8; 32] = bytes.try_into().map_err(|_| "invalid identity length")?;
            Ok(SecretKey::from_bytes(&bytes))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let key = SecretKey::generate();
            let parent = path
                .parent()
                .filter(|p| !p.as_os_str().is_empty())
                .unwrap_or(Path::new("."));
            let mut file = tempfile::NamedTempFile::new_in(parent)?;
            file.write_all(&key.to_bytes())?;
            file.as_file().sync_all()?;
            file.persist_noclobber(path)?;
            Ok(key)
        }
        Err(error) => Err(error.into()),
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    let identity_only = args.len() == 2 && args[1] == "identity";
    if !identity_only
        && (args.len() != 3
            || !matches!(args[1].as_str(), "listen" | "serve" | "connect" | "watch"))
    {
        return Err(
            "usage: sharing_probe KEY_FILE identity | KEY_FILE listen|serve EXPECTED_PEER_ID | KEY_FILE connect|watch ADDRESS_JSON"
                .into(),
        );
    }
    let key = identity(Path::new(&args[0]))?;
    if identity_only {
        println!("{}", key.public());
        return Ok(());
    }
    let endpoint = sharing::bind(key).await?;
    println!("peer: {}", endpoint.id());
    let result = async {
        if tokio::time::timeout(Duration::from_secs(30), endpoint.online())
            .await
            .is_err()
        {
            eprintln!("relay not online; continuing with available addresses");
        }
        println!("address: {}", serde_json::to_string(&endpoint.addr())?);
        if matches!(args[1].as_str(), "listen" | "serve") {
            let expected: EndpointId = args[2].parse()?;
            sharing::probe::server(&endpoint, expected, args[1] == "serve", report).await?;
        } else {
            let address: EndpointAddr = serde_json::from_str(&args[2])?;
            probe(&endpoint, address, args[1] == "watch").await?;
        }
        Ok(())
    }
    .await;
    endpoint.close().await;
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_survives_restart_and_rejects_corruption() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("identity");
        let first = identity(&path).unwrap().public();
        assert_eq!(identity(&path).unwrap().public(), first);
        std::fs::write(&path, b"broken").unwrap();
        assert!(identity(&path).is_err());
    }

    #[tokio::test]
    async fn watch_stops_when_peer_denies_access() {
        let server = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
            .alpns(vec![sharing::ALPN.to_vec()])
            .bind()
            .await
            .unwrap();
        let client = iroh::Endpoint::bind(iroh::endpoint::presets::Minimal)
            .await
            .unwrap();
        let accepted = async {
            let connection = server.accept().await.unwrap().await.unwrap();
            assert!(
                sharing::respond(&connection, SecretKey::generate().public())
                    .await
                    .is_err()
            );
        };
        let dialing = async {
            assert!(probe(&client, server.addr(), true).await.is_err());
        };
        tokio::time::timeout(Duration::from_secs(3), async {
            tokio::join!(accepted, dialing);
        })
        .await
        .unwrap();
        client.close().await;
        server.close().await;
    }

    #[tokio::test]
    async fn watch_stops_on_oversized_response() {
        let server = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
            .alpns(vec![sharing::ALPN.to_vec()])
            .bind()
            .await
            .unwrap();
        let client = iroh::Endpoint::bind(iroh::endpoint::presets::Minimal)
            .await
            .unwrap();
        let accepted = async {
            let connection = server.accept().await.unwrap().await.unwrap();
            let (mut send, mut recv) = connection.accept_bi().await.unwrap();
            recv.read_to_end(10).await.unwrap();
            send.write_all(&[0; 11]).await.unwrap();
            send.finish().unwrap();
            // Keep connection alive so watch must reject bad response itself.
            connection.closed().await;
        };
        let dialing = async {
            assert!(probe(&client, server.addr(), true).await.is_err());
            client.close().await;
        };
        tokio::time::timeout(Duration::from_secs(3), async {
            tokio::join!(accepted, dialing);
        })
        .await
        .unwrap();
        server.close().await;
    }
}
