//! Fallback impl for unsupported targets — errors at runtime so the binary
//! still compiles for `rushtle server` use on those platforms.

use anyhow::{bail, Result};
use tokio::net::TcpStream;

pub fn install(
    _chain: &str,
    _tcp_port: u16,
    _dns_port: u16,
    _v4: &[String],
    _v6: &[String],
    _dns: bool,
) -> Result<()> {
    bail!("rushtle client mode is not supported on this platform")
}

pub fn remove(_chain: &str, _has_v6: bool) -> Result<()> {
    Ok(())
}

pub fn original_dst(_sock: &TcpStream, _is_v6: bool) -> Result<(String, u16)> {
    bail!("original_dst not available on this platform")
}
