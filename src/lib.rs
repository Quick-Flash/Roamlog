pub mod area;
pub mod coverage;
pub mod demo;
mod flock;
pub mod geo;
pub mod gpx;
pub mod model;
pub mod mvt;
pub mod pmtiles;
pub mod recovery;
pub mod roads;
mod spatial;
pub mod store;

#[cfg(any(feature = "ui", target_os = "android", test))]
mod backend;

#[cfg(feature = "ui")]
pub mod mapping;
#[cfg(feature = "ui")]
pub mod ui;

#[cfg(all(target_os = "android", feature = "ui"))]
mod android;
