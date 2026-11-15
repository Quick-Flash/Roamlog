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

#[cfg(feature = "sharing")]
pub mod sharing;

#[cfg(any(feature = "ui", test))]
mod backend;

#[cfg(feature = "ui")]
pub mod mapping;
#[cfg(feature = "ui")]
pub mod ui;

#[cfg(all(target_os = "android", feature = "ui"))]
mod android;
#[cfg(all(target_os = "android", feature = "ui", not(feature = "sharing")))]
compile_error!("Android app build needs --features sharing (android/package.sh passes it)");

#[cfg(all(target_os = "android", feature = "ui"))]
mod android_updates;

#[cfg(feature = "ui")]
mod updates;
