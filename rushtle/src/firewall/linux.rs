//! Linux NAT redirect via iptables/ip6tables.

use anyhow::{anyhow, Context, Result};
use std::net::{IpAddr, Ipv6Addr};
use std::os::fd::AsRawFd;
use tokio::net::TcpStream;

/// Mirror sshuttle/helpers.py:resolvconf_nameservers: enumerate `nameserver`
/// entries from BOTH /etc/resolv.conf AND /run/systemd/resolve/resolv.conf
/// (when present). On systemd-resolved systems /etc/resolv.conf typically
/// only lists 127.0.0.53; the real upstreams live in the systemd file.
/// Capturing both means apps going through the libc resolver AND traffic
/// originating from systemd-resolved itself both hit our DNS redirect.
/// Returns IPv4 nameservers only (no ip6tables redirects for v6 yet).
fn read_local_nameservers_v4() -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut seen = std::collections::HashSet::<String>::new();
    for f in ["/etc/resolv.conf", "/run/systemd/resolve/resolv.conf"] {
        let Ok(s) = std::fs::read_to_string(f) else { continue };
        let mut found_in_this_file: Vec<String> = Vec::new();
        for line in s.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
                continue;
            }
            if let Some(rest) = line.strip_prefix("nameserver") {
                let ip = rest.trim();
                if let Ok(IpAddr::V4(v4)) = ip.parse::<IpAddr>() {
                    let s = v4.to_string();
                    if seen.insert(s.clone()) {
                        out.push(s.clone());
                    }
                    found_in_this_file.push(s);
                }
            }
        }
        if !found_in_this_file.is_empty() {
            tracing::debug!("nameservers from {f}: {found_in_this_file:?}");
        }
    }
    out
}

// IP6T_SO_ORIGINAL_DST is 80 in <linux/netfilter_ipv6/ip6_tables.h>; libc
// crate doesn't expose it on all targets so we hardcode.
const IP6T_SO_ORIGINAL_DST: libc::c_int = 80;

pub fn original_dst(sock: &TcpStream, is_v6: bool) -> Result<(String, u16)> {
    let fd = sock.as_raw_fd();
    if is_v6 {
        let mut addr: libc::sockaddr_in6 = unsafe { std::mem::zeroed() };
        let mut len = std::mem::size_of::<libc::sockaddr_in6>() as libc::socklen_t;
        let rc = unsafe {
            libc::getsockopt(
                fd,
                libc::IPPROTO_IPV6,
                IP6T_SO_ORIGINAL_DST,
                &mut addr as *mut _ as *mut libc::c_void,
                &mut len,
            )
        };
        if rc != 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        let port = u16::from_be(addr.sin6_port);
        let ip = Ipv6Addr::from(addr.sin6_addr.s6_addr);
        Ok((ip.to_string(), port))
    } else {
        let mut addr: libc::sockaddr_in = unsafe { std::mem::zeroed() };
        let mut len = std::mem::size_of::<libc::sockaddr_in>() as libc::socklen_t;
        let rc = unsafe {
            libc::getsockopt(
                fd,
                libc::SOL_IP,
                libc::SO_ORIGINAL_DST,
                &mut addr as *mut _ as *mut libc::c_void,
                &mut len,
            )
        };
        if rc != 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        let port = u16::from_be(addr.sin_port);
        let ip = u32::from_be(addr.sin_addr.s_addr);
        let ip_str = format!(
            "{}.{}.{}.{}",
            (ip >> 24) & 0xff,
            (ip >> 16) & 0xff,
            (ip >> 8) & 0xff,
            ip & 0xff
        );
        Ok((ip_str, port))
    }
}

fn iptables_cmd(bin: &str, args: &[&str]) -> Result<()> {
    // Mirror sshuttle's UX: when not root, prefix `sudo -n` so the user
    // doesn't have to wrap the whole command. Falls back to interactive
    // sudo (`-p '[local sudo] Password: '`) if NOPASSWD isn't configured.
    let needs_sudo = unsafe { libc::geteuid() } != 0;
    let mut cmd = if needs_sudo {
        let mut c = std::process::Command::new("sudo");
        c.arg("-p").arg("[local sudo] Password: ").arg(bin);
        for a in args {
            c.arg(a);
        }
        c
    } else {
        let mut c = std::process::Command::new(bin);
        for a in args {
            c.arg(a);
        }
        c
    };
    let status = cmd
        .status()
        .with_context(|| format!("exec {bin} {args:?}"))?;
    if !status.success() {
        return Err(anyhow!("{bin} {args:?} exited with {status}"));
    }
    Ok(())
}

/// `iptables -t nat -N <chain>` but treats "Chain already exists" as a
/// non-fatal condition. A prior crash / kill -9 can leave a stale chain;
/// sshuttle handles this by proceeding straight to `-F` to flush it.
/// Returns Ok(()) whether the chain was newly created or already existed.
fn iptables_create_chain(bin: &str, chain: &str) -> Result<()> {
    let res = iptables_cmd(bin, &["-t", "nat", "-N", chain]);
    if res.is_ok() {
        return Ok(());
    }
    // Probe for existence with `-L` (silent on success). If it exists, the
    // earlier -N error was the harmless "Chain already exists"; otherwise
    // surface the original failure.
    if iptables_cmd(bin, &["-t", "nat", "-L", chain, "-n"]).is_ok() {
        tracing::info!("{bin} chain {chain} already exists, reusing");
        return Ok(());
    }
    res
}

pub fn install(
    chain: &str,
    tcp_port: u16,
    dns_port: u16,
    subnets_v4: &[String],
    subnets_v6: &[String],
    dns: bool,
) -> Result<()> {
    tracing::info!("installing iptables NAT chain {chain}");
    let tcp_s = tcp_port.to_string();
    let dns_s = dns_port.to_string();

    iptables_create_chain("iptables", chain)?;
    iptables_cmd("iptables", &["-t", "nat", "-F", chain])?;

    // Mirror sshuttle/methods/nat.py rule order:
    //   1. per-nameserver UDP/53 REDIRECT
    //   2. per-subnet TCP REDIRECT
    //   3. addrtype LOCAL -j RETURN (escape for everything else local-bound)
    if dns {
        let nameservers = read_local_nameservers_v4();
        if nameservers.is_empty() {
            tracing::warn!("no nameservers in resolv.conf; falling back to all-UDP/53 redirect");
            iptables_cmd("iptables", &[
                "-t", "nat", "-A", chain, "-p", "udp", "--dport", "53",
                "-j", "REDIRECT", "--to-ports", &dns_s,
            ])?;
        } else {
            for ns in &nameservers {
                tracing::info!("DNS redirect: {ns}:53 -> 127.0.0.1:{dns_port}");
                iptables_cmd("iptables", &[
                    "-t", "nat", "-A", chain, "-p", "udp", "-d", ns,
                    "--dport", "53", "-j", "REDIRECT", "--to-ports", &dns_s,
                ])?;
            }
        }
    }

    for cidr in subnets_v4 {
        iptables_cmd("iptables", &[
            "-t", "nat", "-A", chain, "-p", "tcp", "-d", cidr, "-j", "REDIRECT",
            "--to-ports", &tcp_s,
        ])?;
    }

    // Skip remaining LOCAL traffic. Same rule sshuttle installs at the tail
    // of its chain. addrtype matches anything that would otherwise hit
    // local services (loopback, host-bound IPs, etc.).
    iptables_cmd("iptables", &[
        "-t", "nat", "-A", chain, "-m", "addrtype", "--dst-type", "LOCAL", "-j", "RETURN",
    ])?;

    iptables_cmd("iptables", &["-t", "nat", "-A", "OUTPUT", "-j", chain])?;

    if !subnets_v6.is_empty() {
        iptables_create_chain("ip6tables", chain)?;
        iptables_cmd("ip6tables", &["-t", "nat", "-F", chain])?;
        iptables_cmd("ip6tables", &["-t", "nat", "-A", chain, "-d", "::1/128", "-j", "RETURN"])?;
        for cidr in subnets_v6 {
            iptables_cmd("ip6tables", &[
                "-t", "nat", "-A", chain, "-p", "tcp", "-d", cidr, "-j", "REDIRECT",
                "--to-ports", &tcp_s,
            ])?;
        }
        iptables_cmd("ip6tables", &["-t", "nat", "-A", "OUTPUT", "-j", chain])?;
    }
    Ok(())
}

pub fn remove(chain: &str, has_v6: bool) -> Result<()> {
    tracing::info!("removing iptables chain {chain}");
    let _ = iptables_cmd("iptables", &["-t", "nat", "-D", "OUTPUT", "-j", chain]);
    let _ = iptables_cmd("iptables", &["-t", "nat", "-F", chain]);
    let _ = iptables_cmd("iptables", &["-t", "nat", "-X", chain]);
    if has_v6 {
        let _ = iptables_cmd("ip6tables", &["-t", "nat", "-D", "OUTPUT", "-j", chain]);
        let _ = iptables_cmd("ip6tables", &["-t", "nat", "-F", chain]);
        let _ = iptables_cmd("ip6tables", &["-t", "nat", "-X", chain]);
    }
    Ok(())
}
