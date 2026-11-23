//! Decode bulky history/road responses on worker, never on input/render thread.
use crate::{
    backend,
    coverage::Section,
    model::{State, TripFilter},
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::path::PathBuf;

#[derive(Deserialize)]
pub(super) struct Snapshot {
    pub state: State,
    pub sections: Option<Vec<Section>>,
    #[serde(default)]
    pub road_total_m: f64,
    #[serde(default)]
    pub road_traveled_m: f64,
    pub basemap: u64,
    pub platform_status: String,
    pub position: Value,
    /// Shown trips that came from friends; each can be added to own history.
    #[serde(default)]
    pub friend_trips: Vec<u64>,
}

pub(super) async fn load(
    directory: PathBuf,
    filter: TripFilter,
    map: bool,
    people: Vec<(String, String)>,
) -> Result<Snapshot, String> {
    zgui::prelude::blocking(move || {
        let value = backend::request(
            directory,
            json!({"op":"snapshot","map":map,"trip_filter":filter,"people":people}),
        )?;
        serde_json::from_value(value).map_err(|e| e.to_string())
    })
    .await
}
