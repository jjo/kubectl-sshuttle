//! Per-OS firewall + original-destination plumbing.
//!
//! - Linux: iptables/ip6tables NAT REDIRECT + getsockopt SO_ORIGINAL_DST.
//! - macOS: pfctl rdr rule + DIOCNATLOOK ioctl on /dev/pf.
//! - Other: stub that errors at runtime; binary still builds.

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
pub use linux::*;

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
pub use macos::*;

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
mod stub;
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub use stub::*;
