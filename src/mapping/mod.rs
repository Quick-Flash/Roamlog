//! Shared Enlumi map foundations, vendored for standalone builds.
//! Source: Enlumi; reused here under Apache-2.0 by project owner.
#[allow(dead_code)]
#[path = "../../vendor/enlumi/camera.rs"]
pub mod camera;
#[allow(dead_code)]
#[allow(clippy::collapsible_match)] // Shared Enlumi source; keep unrelated style unchanged.
#[path = "../../vendor/enlumi/tiles.rs"]
pub mod tiles;

pub mod basemap;
pub mod download;
mod gestures;
mod labels;
mod labels_gpu;
pub mod map_gpu;
mod map_surface;
mod style;
mod vector_tiles;
pub mod view;

mod terrain;
mod terrain_gestures;
mod terrain_gpu;
mod terrain_surface;
mod terrain_tiles;
mod terrain_view;
mod throttle;
