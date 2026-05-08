//! sshuttle ssnet wire protocol.
//!
//! Frame: 8-byte header + payload.
//!   magic:     b"SS"  (2 bytes)
//!   channel:   u16 BE
//!   cmd:       u16 BE
//!   datalen:   u16 BE
//!   payload:   datalen bytes
//!
//! Matches sshuttle/ssnet.py: HDR_FMT = '!ccHHH', HDR_LEN = 8.
//!
//! Payload conventions (from sshuttle/server.py):
//!
//! * `CMD_TCP_CONNECT` — `b"family,dstip,dstport"` ASCII, family is a
//!   socket family int (AF_INET=2 on Linux, AF_INET6=10).
//! * `CMD_UDP_OPEN`    — `b"family"` ASCII int.
//! * `CMD_UDP_DATA`    — `b"peerip,peerport,<rawbytes>"`. The first 2 commas
//!   delimit (`split(b',', 2)` on the peer side); the remainder is raw.
//! * `CMD_UDP_CLOSE`   — empty.
//! * `CMD_HOST_REQ`    — ASCII whitespace-separated seed hostnames.
//! * `CMD_HOST_LIST`   — `\n`-joined `name,ip` lines.
//! * `CMD_ROUTES`      — `\n`-joined `family,ip,width` lines.
//! * `CMD_DNS_REQ`     — raw DNS query bytes.
//! * `CMD_DNS_RESPONSE` — raw DNS response bytes.

use anyhow::{anyhow, bail, Result};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

pub const HDR_LEN: usize = 8;
pub const MAGIC: [u8; 2] = *b"SS";
pub const MAX_PAYLOAD: usize = u16::MAX as usize;

/// Per-frame payload cap for TCP_DATA reads. Kept well under 1 KB so the
/// total frame (HDR_LEN + payload) survives middleware that drops single
/// writes/WebSocket frames above ~1 KB — observed on tailscale-fronted
/// kubernetes apiservers (see prior debug session).
pub const CHUNK: usize = 768;

/// Inter-frame sleep, microseconds. Mutable at runtime so the link probe in
/// `client::run` can switch on chunking on the fly (no need to restart
/// rushtle). Reads are `Relaxed` because the value is advisory — at most
/// one frame is sent at the wrong delay across a transition, which has no
/// correctness impact.
///
/// 0 = no delay (default, fastest). Set to e.g. 2000 (2 ms) on truncating
/// clusters so kubectl's stdin pipe drains between frames and each frame
/// goes out as its own websocket frame.
static FRAME_DELAY_US: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Initialise FRAME_DELAY_US from `RUSHTLE_FRAME_DELAY_US`. Call once at
/// process startup (before any `write_frame` activity). If the env var is
/// unset or unparseable the delay stays at 0.
pub fn init_frame_delay_from_env() {
    if let Ok(s) = std::env::var("RUSHTLE_FRAME_DELAY_US") {
        if let Ok(v) = s.parse::<u64>() {
            FRAME_DELAY_US.store(v, std::sync::atomic::Ordering::Relaxed);
        }
    }
}

/// Override the inter-frame delay at runtime. Used by the link probe to
/// turn on chunking after detecting a truncating intermediary.
pub fn set_frame_delay_us(us: u64) {
    FRAME_DELAY_US.store(us, std::sync::atomic::Ordering::Relaxed);
}

/// Current delay in microseconds. Exposed for diagnostic logging.
pub fn frame_delay_us() -> u64 {
    FRAME_DELAY_US.load(std::sync::atomic::Ordering::Relaxed)
}

fn frame_delay() -> std::time::Duration {
    std::time::Duration::from_micros(frame_delay_us())
}

/// Synchronization header sshuttle server writes before the first ssnet
/// frame. Stock sshuttle client waits for two NULs followed by the literal
/// `SSHUTTLE0001` before reading frames. See sshuttle/server.py:299 and
/// sshuttle/client.py:625.
pub const SYNC_HEADER: &[u8] = b"\0\0SSHUTTLE0001";

// Linux socket family numbers (server.py treats anything != 2 as AF_INET6).
pub const AF_INET: u16 = 2;
pub const AF_INET6: u16 = 10;

// Command codes from sshuttle/ssnet.py.
pub const CMD_EXIT: u16 = 0x4200;
pub const CMD_PING: u16 = 0x4201;
pub const CMD_PONG: u16 = 0x4202;
pub const CMD_TCP_CONNECT: u16 = 0x4203;
pub const CMD_TCP_STOP_SENDING: u16 = 0x4204;
pub const CMD_TCP_EOF: u16 = 0x4205;
pub const CMD_TCP_DATA: u16 = 0x4206;
pub const CMD_ROUTES: u16 = 0x4207;
pub const CMD_HOST_REQ: u16 = 0x4208;
pub const CMD_HOST_LIST: u16 = 0x4209;
pub const CMD_DNS_REQ: u16 = 0x420a;
pub const CMD_DNS_RESPONSE: u16 = 0x420b;
pub const CMD_UDP_OPEN: u16 = 0x420c;
pub const CMD_UDP_DATA: u16 = 0x420d;
pub const CMD_UDP_CLOSE: u16 = 0x420e;

#[derive(Debug, Clone)]
pub struct Frame {
    pub channel: u16,
    pub cmd: u16,
    pub data: Vec<u8>,
}

impl Frame {
    pub fn new(channel: u16, cmd: u16, data: Vec<u8>) -> Self {
        Self { channel, cmd, data }
    }

    pub fn cmd_name(cmd: u16) -> &'static str {
        match cmd {
            CMD_EXIT => "EXIT",
            CMD_PING => "PING",
            CMD_PONG => "PONG",
            CMD_TCP_CONNECT => "TCP_CONNECT",
            CMD_TCP_STOP_SENDING => "TCP_STOP_SENDING",
            CMD_TCP_EOF => "TCP_EOF",
            CMD_TCP_DATA => "TCP_DATA",
            CMD_ROUTES => "ROUTES",
            CMD_HOST_REQ => "HOST_REQ",
            CMD_HOST_LIST => "HOST_LIST",
            CMD_DNS_REQ => "DNS_REQ",
            CMD_DNS_RESPONSE => "DNS_RESPONSE",
            CMD_UDP_OPEN => "UDP_OPEN",
            CMD_UDP_DATA => "UDP_DATA",
            CMD_UDP_CLOSE => "UDP_CLOSE",
            _ => "UNKNOWN",
        }
    }
}

pub async fn read_frame<R: AsyncReadExt + Unpin>(r: &mut R) -> Result<Option<Frame>> {
    let mut hdr = [0u8; HDR_LEN];
    match r.read_exact(&mut hdr).await {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e.into()),
    }
    if hdr[0..2] != MAGIC {
        bail!("bad magic: {:02x}{:02x}", hdr[0], hdr[1]);
    }
    let channel = u16::from_be_bytes([hdr[2], hdr[3]]);
    let cmd = u16::from_be_bytes([hdr[4], hdr[5]]);
    let datalen = u16::from_be_bytes([hdr[6], hdr[7]]) as usize;
    let mut data = vec![0u8; datalen];
    if datalen > 0 {
        r.read_exact(&mut data).await?;
    }
    Ok(Some(Frame { channel, cmd, data }))
}

pub async fn write_frame<W: AsyncWriteExt + Unpin>(w: &mut W, f: &Frame) -> Result<()> {
    if f.data.len() > MAX_PAYLOAD {
        bail!("payload {} exceeds {} max", f.data.len(), MAX_PAYLOAD);
    }
    let mut hdr = [0u8; HDR_LEN];
    hdr[0..2].copy_from_slice(&MAGIC);
    hdr[2..4].copy_from_slice(&f.channel.to_be_bytes());
    hdr[4..6].copy_from_slice(&f.cmd.to_be_bytes());
    hdr[6..8].copy_from_slice(&(f.data.len() as u16).to_be_bytes());
    w.write_all(&hdr).await?;
    if !f.data.is_empty() {
        w.write_all(&f.data).await?;
    }
    w.flush().await?;
    let d = frame_delay();
    if !d.is_zero() {
        tokio::time::sleep(d).await;
    }
    Ok(())
}

/// Read sshuttle's `\0\0SSHUTTLE0001` sync header. Mirror sshuttle/client.py
/// — skip arbitrary bytes until first NUL, again until second NUL, then
/// consume the literal.
pub async fn read_sync_header<R: AsyncReadExt + Unpin>(r: &mut R) -> Result<()> {
    let expected = b"SSHUTTLE0001";
    for _ in 0..2 {
        loop {
            let mut b = [0u8; 1];
            r.read_exact(&mut b).await?;
            if b[0] == 0 {
                break;
            }
        }
    }
    let mut buf = vec![0u8; expected.len()];
    r.read_exact(&mut buf).await?;
    if buf != expected {
        bail!("sync header mismatch: {buf:?}");
    }
    Ok(())
}

// --- TCP_CONNECT payload ---------------------------------------------------

/// Parse sshuttle CONNECT payload. Modern sshuttle sends 3 parts
/// `family,ip,port`; older clients (and rushtle <=0.1) sent `ip,port`. Accept
/// both.
pub fn parse_connect(payload: &[u8]) -> Result<ConnectTarget> {
    let s = std::str::from_utf8(payload)?;
    let s = s.trim_end_matches('\0').trim();
    let parts: Vec<&str> = s.splitn(3, ',').collect();
    let (family, host, port) = match parts.as_slice() {
        [f, h, p] => (Some(f.parse::<u16>()?), h.to_string(), p.parse::<u16>()?),
        [h, p] => (None, h.to_string(), p.parse::<u16>()?),
        _ => return Err(anyhow!("bad CONNECT payload: {s:?}")),
    };
    Ok(ConnectTarget { family, host, port })
}

#[derive(Debug, Clone)]
pub struct ConnectTarget {
    /// Socket family hint from the wire — None means 2-part legacy form.
    /// Currently informational; tokio resolves dual-stack.
    #[allow(dead_code)]
    pub family: Option<u16>,
    pub host: String,
    pub port: u16,
}

pub fn encode_connect(family: u16, host: &str, port: u16) -> Vec<u8> {
    format!("{family},{host},{port}").into_bytes()
}

// --- UDP_DATA payload ------------------------------------------------------

/// Parse `peerip,peerport,<rawbytes>` (sshuttle UDP_DATA).
/// Returns (peer_ip, peer_port, payload_slice).
pub fn parse_udp_data(payload: &[u8]) -> Result<(String, u16, &[u8])> {
    let mut commas = 0;
    let mut idx_ip_end = 0usize;
    let mut idx_port_end = 0usize;
    for (i, &b) in payload.iter().enumerate() {
        if b == b',' {
            commas += 1;
            if commas == 1 {
                idx_ip_end = i;
            } else if commas == 2 {
                idx_port_end = i;
                break;
            }
        }
    }
    if commas < 2 {
        bail!("UDP_DATA needs 2 commas: {:?}", payload.first_chunk::<32>());
    }
    let ip = std::str::from_utf8(&payload[..idx_ip_end])?.to_string();
    let port: u16 = std::str::from_utf8(&payload[idx_ip_end + 1..idx_port_end])?.parse()?;
    Ok((ip, port, &payload[idx_port_end + 1..]))
}

pub fn encode_udp_data(peer_ip: &str, peer_port: u16, body: &[u8]) -> Vec<u8> {
    let prefix = format!("{peer_ip},{peer_port},");
    let mut out = Vec::with_capacity(prefix.len() + body.len());
    out.extend_from_slice(prefix.as_bytes());
    out.extend_from_slice(body);
    out
}

// --- HOST_LIST payload -----------------------------------------------------

pub fn encode_host_list(entries: impl IntoIterator<Item = (String, String)>) -> Vec<u8> {
    let mut out = Vec::new();
    let mut first = true;
    for (name, ip) in entries {
        if !first {
            out.push(b'\n');
        }
        first = false;
        out.extend_from_slice(name.as_bytes());
        out.push(b',');
        out.extend_from_slice(ip.as_bytes());
    }
    out
}
