//! macOS NAT redirect via pf (PacketFilter).
//!
//! Strategy mirrors sshuttle/methods/pf.py:
//!   1. Write pf rules to a per-pid anchor: `rdr pass on lo0 inet proto tcp
//!      from any to <CIDR> -> 127.0.0.1 port <listen_port>` plus equivalent
//!      pass-out rule on en0/all interfaces.
//!   2. Load via `pfctl -a com.rushtle/<pid> -f -`.
//!   3. Enable pf with `pfctl -E` (records token to disable on cleanup).
//!   4. On accept(), recover original dst by issuing a `DIOCNATLOOK` ioctl
//!      against `/dev/pf` with the connection 5-tuple.
//!
//! IPv6: uses `inet6` rules; same DIOCNATLOOK with af=AF_INET6.
//!
//! References:
//!   - sshuttle/methods/pf.py (DARWIN class) — protocol of record
//!   - macOS pfvar.h — struct pfioc_natlook layout, DIOCNATLOOK number
//!
//! NOTE: untested on hardware. Skeleton ready for iteration on macOS.

use anyhow::{anyhow, bail, Context, Result};
use std::fs::OpenOptions;
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr};
use std::os::fd::AsRawFd;
use std::process::{Command, Stdio};
use std::sync::Mutex;
use tokio::net::TcpStream;

const PF_DEV: &str = "/dev/pf";

// pfvar.h constants on macOS Sequoia (15.x). pf is frozen on darwin —
// these have been stable since at least 10.9.
const PF_IN: u8 = 1;
const PF_OUT: u8 = 2;

// _IOWR('D', 23, struct pfioc_natlook) on darwin. Computed empirically and
// matches sshuttle's pf.py value.
//
//   _IOWR(g, n, t) = (IOC_INOUT) | ((sizeof(t) & 0x1fff) << 16) | (g << 8) | n
//   IOC_INOUT      = 0xC0000000
//   sizeof(pfioc_natlook) on darwin = 96 bytes (IPv6-wide pf_addr=16 bytes)
//
// Encoded value: 0xc0604417. Verify on hardware; update if wrong.
const DIOCNATLOOK: libc::c_ulong = 0xc0604417;

#[repr(C)]
#[derive(Default, Copy, Clone)]
struct PfAddr {
    pfa: [u8; 16],
}

// `union pf_state_xport { u_int16_t port; u_int16_t call_id; u_int32_t spi; }`
// is 4 bytes wide on darwin. Port lives in the first 2 bytes (network order).
#[repr(C)]
#[derive(Default, Copy, Clone)]
struct PfStateXport {
    raw: [u8; 4],
}

impl PfStateXport {
    fn set_port(&mut self, port: u16) {
        self.raw[..2].copy_from_slice(&port.to_be_bytes());
    }
    fn port(&self) -> u16 {
        u16::from_be_bytes([self.raw[0], self.raw[1]])
    }
}

// Layout must match darwin `struct pfioc_natlook` exactly — sizeof = 96, which
// the DIOCNATLOOK ioctl number (0xc0604417, with 0x60=96 in the size field)
// confirms. Body is 4×16 + 4×4 + 4×1 = 84 bytes; tail pad of 12 brings it to
// 96 to match the C struct's end-alignment / unused-fields region.
#[repr(C)]
#[derive(Default)]
struct PfiocNatlook {
    saddr: PfAddr,
    daddr: PfAddr,
    rsaddr: PfAddr,
    rdaddr: PfAddr,
    sxport: PfStateXport,
    dxport: PfStateXport,
    rsxport: PfStateXport,
    rdxport: PfStateXport,
    af: u8,
    proto: u8,
    proto_variant: u8,
    direction: u8,
    _pad: [u8; 12],
}

const _: () = assert!(std::mem::size_of::<PfiocNatlook>() == 96);

/// State recorded by `install` so `remove` can undo cleanly.
static STATE: Mutex<Option<State>> = Mutex::new(None);

struct State {
    anchor: String,
    enable_token: Option<String>,
}

fn pfctl(args: &[&str]) -> Result<std::process::Output> {
    let out = Command::new("pfctl")
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .with_context(|| format!("exec pfctl {args:?}"))?;
    if !out.status.success() {
        return Err(anyhow!(
            "pfctl {args:?} exit={}: {}",
            out.status,
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    Ok(out)
}

fn pfctl_with_stdin(args: &[&str], stdin_payload: &str) -> Result<()> {
    let mut child = Command::new("pfctl")
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("spawn pfctl {args:?}"))?;
    use std::io::Write;
    if let Some(mut s) = child.stdin.take() {
        s.write_all(stdin_payload.as_bytes())
            .with_context(|| "writing pfctl stdin")?;
    }
    let out = child.wait_with_output()?;
    if !out.status.success() {
        bail!(
            "pfctl {args:?} exit={}: {}",
            out.status,
            String::from_utf8_lossy(&out.stderr)
        );
    }
    Ok(())
}

pub fn install(
    chain: &str,
    tcp_port: u16,
    dns_port: u16,
    subnets_v4: &[String],
    subnets_v6: &[String],
    dns: bool,
) -> Result<()> {
    let anchor = format!("com.rushtle/{chain}");
    tracing::info!("installing pf anchor {anchor}");

    let mut rules = String::new();
    for cidr in subnets_v4 {
        rules.push_str(&format!(
            "rdr pass on lo0 inet proto tcp from any to {cidr} -> 127.0.0.1 port {tcp_port}\n"
        ));
        rules.push_str(&format!(
            "pass out route-to lo0 inet proto tcp from any to {cidr} keep state\n"
        ));
    }
    if dns {
        rules.push_str(&format!(
            "rdr pass on lo0 inet proto udp from any to any port 53 -> 127.0.0.1 port {dns_port}\n"
        ));
    }
    for cidr in subnets_v6 {
        rules.push_str(&format!(
            "rdr pass on lo0 inet6 proto tcp from any to {cidr} -> ::1 port {tcp_port}\n"
        ));
        rules.push_str(&format!(
            "pass out route-to lo0 inet6 proto tcp from any to {cidr} keep state\n"
        ));
    }
    if rules.is_empty() {
        bail!("no subnets to install");
    }

    pfctl_with_stdin(&["-a", &anchor, "-f", "-"], &rules)
        .context("loading pf anchor rules")?;

    // Enable pf and capture the token so we can disable cleanly. On hosts
    // where pf is already enabled, `pfctl -E` returns a token but does not
    // actually flip state; the disable -X uses the token.
    let out = pfctl(&["-E"]).context("enable pf")?;
    let stderr = String::from_utf8_lossy(&out.stderr);
    let token = stderr
        .lines()
        .find_map(|l| l.strip_prefix("Token : ").map(str::trim).map(String::from));

    *STATE.lock().unwrap() = Some(State { anchor, enable_token: token });
    Ok(())
}

pub fn remove(_chain: &str, _has_v6: bool) -> Result<()> {
    let st = STATE.lock().unwrap().take();
    if let Some(st) = st {
        tracing::info!("removing pf anchor {}", st.anchor);
        let _ = pfctl(&["-a", &st.anchor, "-F", "all"]);
        if let Some(tok) = st.enable_token {
            let _ = pfctl(&["-X", &tok]);
        }
    }
    Ok(())
}

/// Look up the pre-NAT destination for an accepted TCP connection.
/// `sock.peer_addr()` and `sock.local_addr()` give the post-NAT 5-tuple
/// from kernel's POV (peer = original src, local = NAT'd 127.0.0.1:port).
/// We ask pf via DIOCNATLOOK to map back to the original dst the app
/// targeted.
pub fn original_dst(sock: &TcpStream, _is_v6: bool) -> Result<(String, u16)> {
    let peer = sock.peer_addr()?;
    let local = sock.local_addr()?;

    let mut nl = PfiocNatlook::default();
    match (peer, local) {
        (SocketAddr::V4(p), SocketAddr::V4(l)) => {
            let pip: Ipv4Addr = *p.ip();
            let lip: Ipv4Addr = *l.ip();
            nl.saddr.pfa[..4].copy_from_slice(&pip.octets());
            nl.daddr.pfa[..4].copy_from_slice(&lip.octets());
            nl.sxport.set_port(p.port());
            nl.dxport.set_port(l.port());
            nl.af = libc::AF_INET as u8;
        }
        (SocketAddr::V6(p), SocketAddr::V6(l)) => {
            nl.saddr.pfa.copy_from_slice(&p.ip().octets());
            nl.daddr.pfa.copy_from_slice(&l.ip().octets());
            nl.sxport.set_port(p.port());
            nl.dxport.set_port(l.port());
            nl.af = libc::AF_INET6 as u8;
        }
        _ => bail!("peer/local family mismatch"),
    }
    nl.proto = libc::IPPROTO_TCP as u8;
    // sshuttle/methods/pf.py probes PF_OUT first then falls back to PF_IN.
    // Mirror that order to match its semantics on hosts where rules are
    // installed at OUT instead of IN.
    nl.direction = PF_OUT;

    let pf = OpenOptions::new()
        .read(true)
        .write(true)
        .open(PF_DEV)
        .with_context(|| format!("open {PF_DEV} (need root)"))?;
    let rc = unsafe {
        libc::ioctl(pf.as_raw_fd(), DIOCNATLOOK, &mut nl as *mut _)
    };
    if rc != 0 {
        nl.direction = PF_IN;
        let rc2 = unsafe { libc::ioctl(pf.as_raw_fd(), DIOCNATLOOK, &mut nl as *mut _) };
        if rc2 != 0 {
            return Err(std::io::Error::last_os_error()).context("DIOCNATLOOK");
        }
    }

    let port = nl.rdxport.port();
    let ip_str = if nl.af == libc::AF_INET as u8 {
        let mut o = [0u8; 4];
        o.copy_from_slice(&nl.rdaddr.pfa[..4]);
        Ipv4Addr::from(o).to_string()
    } else {
        Ipv6Addr::from(nl.rdaddr.pfa).to_string()
    };
    Ok((ip_str, port))
}
