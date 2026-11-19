//! Networking related utilities

// Roamlog vendor copy: lints capped as for any registry dependency.
#![allow(warnings)]
pub mod interfaces;
#[cfg(target_os = "ios")]
mod ios_libc;
pub mod ip;
mod ip_family;
pub mod netmon;
#[cfg(not(wasm_browser))]
mod udp;

pub use self::ip_family::IpFamily;
#[cfg(not(wasm_browser))]
pub use self::udp::{UdpSender, UdpSocket};
