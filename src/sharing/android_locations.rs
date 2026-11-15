//! Foreground protocol test controls. Samples are explicitly supplied, never collected here.
use super::{
    Result,
    location::Sample,
    manager::{Manager, Mode, now_ms},
    transport,
};
use iroh::Endpoint;
use serde::Deserialize;
use std::time::Duration;

pub(super) fn command(manager: &Manager, command: &str, target: &str) -> Result<()> {
    match command {
        "mode_off" => manager.set_mode(Mode::Off),
        "mode_trips" => manager.set_mode(Mode::TripsOnly),
        "mode_always" => manager.set_mode(Mode::Always),
        "recording" => manager.set_recording(serde_json::from_str(target)?),
        "clear_fix" => manager.clear_fix(),
        "sample" => {
            #[derive(Deserialize)]
            #[serde(deny_unknown_fields)]
            struct Input {
                latitude: f64,
                longitude: f64,
                recording: bool,
            }
            let input: Input = serde_json::from_str(target)?;
            let now = now_ms();
            manager.update_fix(
                Sample {
                    fix: crate::model::Fix {
                        point: crate::geo::Point {
                            lat: input.latitude,
                            lon: input.longitude,
                        },
                        timestamp_ms: now.try_into()?,
                        accuracy_m: 5.0,
                    },
                    speed_m_s: None,
                    heading_degrees: None,
                },
                input.recording,
                now,
            )?;
            Ok(())
        }
        _ => Err("unknown location control".into()),
    }
}

pub(super) async fn run(
    endpoint: &Endpoint,
    manager: Manager,
    target: &str,
    client: bool,
    mut report: impl FnMut(&str) -> Result<()>,
) -> Result<()> {
    let work = async {
        if client {
            transport::client(endpoint, serde_json::from_str(target)?, manager.clone()).await
        } else {
            transport::server(endpoint, manager.clone()).await
        }
    };
    tokio::pin!(work);
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            result = &mut work => return result,
            _ = tick.tick() => {
                report(&format!("locations:{}", serde_json::to_string(&manager.snapshots(now_ms())?)?))?;
            },
        }
    }
}
