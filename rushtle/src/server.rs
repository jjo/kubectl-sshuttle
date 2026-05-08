//! Server side: in-pod, ssnet over stdin/stdout.
//!
//! Implements the subset of sshuttle's server.py that matters for kubectl
//! tunneling: TCP_CONNECT, TCP_DATA, TCP_EOF, TCP_STOP_SENDING, DNS_REQ,
//! UDP_OPEN/UDP_DATA/UDP_CLOSE, HOST_REQ→HOST_LIST, PING/PONG, EXIT,
//! plus the synchronization header and an optional bootstrap-eater for
//! interop with stock sshuttle clients.

use crate::ssnet::{self, ConnectTarget, Frame};
use anyhow::{Context, Result};
use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpStream, UdpSocket};
use tokio::sync::{mpsc, Mutex, OnceCell};

type FrameTx = mpsc::Sender<Frame>;

struct TcpChannel {
    /// Bytes from remote → write into this connection.
    inbound_tx: mpsc::Sender<Vec<u8>>,
}

struct UdpChannel {
    /// (dst_ip, dst_port, body) → sendto from server-side socket.
    inbound_tx: mpsc::Sender<(String, u16, Vec<u8>)>,
}

pub async fn run(compat_bootstrap: bool, assembler_bytes: u64) -> Result<()> {
    tracing::info!(
        "rushtle server starting (stdio mode, compat_bootstrap={compat_bootstrap}, assembler_bytes={assembler_bytes})"
    );

    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();
    let mut reader = tokio::io::BufReader::new(stdin);
    let writer = Arc::new(Mutex::new(stdout));

    if compat_bootstrap {
        if assembler_bytes > 0 {
            let mut sink = vec![0u8; assembler_bytes as usize];
            reader
                .read_exact(&mut sink)
                .await
                .with_context(|| format!("reading {assembler_bytes} bytes of assembler.py source"))?;
            tracing::info!("discarded {assembler_bytes} bytes of assembler.py source");
        }
        consume_sshuttle_bootstrap(&mut reader)
            .await
            .context("consuming sshuttle bootstrap")?;
        tracing::info!("sshuttle bootstrap consumed, entering ssnet mode");
    }

    // Sync header — stock sshuttle client waits for `\0\0SSHUTTLE0001` before
    // reading frames.
    {
        let mut w = writer.lock().await;
        w.write_all(ssnet::SYNC_HEADER).await?;
        w.flush().await?;
    }

    // Single writer task — all outbound frames funnel through this channel.
    let (out_tx, mut out_rx) = mpsc::channel::<Frame>(1024);
    let writer_clone = writer.clone();
    let writer_task = tokio::spawn(async move {
        while let Some(frame) = out_rx.recv().await {
            tracing::debug!(
                "tx ch={} cmd={} len={}",
                frame.channel,
                Frame::cmd_name(frame.cmd),
                frame.data.len()
            );
            let mut w = writer_clone.lock().await;
            if let Err(e) = ssnet::write_frame(&mut *w, &frame).await {
                tracing::error!("stdout write failed: {e}");
                break;
            }
        }
    });

    // Initial PING(chicken) — sshuttle Mux does this on construct.
    let _ = out_tx
        .send(Frame::new(0, ssnet::CMD_PING, b"chicken".to_vec()))
        .await;

    // Empty CMD_ROUTES — sshuttle's client blocks installing iptables until
    // it receives this frame (its `onroutes` callback triggers `fw.start()`).
    // sshuttle/client.py:755-761. Even with no auto-nets we must emit one so
    // the local firewall manager unblocks.
    let _ = out_tx
        .send(Frame::new(0, ssnet::CMD_ROUTES, vec![]))
        .await;

    let tcp_chans: Arc<Mutex<HashMap<u16, TcpChannel>>> = Arc::new(Mutex::new(HashMap::new()));
    let udp_chans: Arc<Mutex<HashMap<u16, UdpChannel>>> = Arc::new(Mutex::new(HashMap::new()));

    loop {
        let frame = match ssnet::read_frame(&mut reader).await {
            Ok(Some(f)) => f,
            Ok(None) => {
                tracing::info!("stdin EOF, server shutting down");
                break;
            }
            Err(e) => {
                tracing::error!("frame read error: {e}");
                break;
            }
        };

        tracing::debug!(
            "rx ch={} cmd={} len={}",
            frame.channel,
            Frame::cmd_name(frame.cmd),
            frame.data.len()
        );

        match frame.cmd {
            ssnet::CMD_PING => {
                let _ = out_tx
                    .send(Frame::new(frame.channel, ssnet::CMD_PONG, frame.data))
                    .await;
            }
            ssnet::CMD_PONG => {
                tracing::debug!("PONG received");
            }
            ssnet::CMD_EXIT => break,
            ssnet::CMD_TCP_CONNECT => {
                let target = match ssnet::parse_connect(&frame.data) {
                    Ok(t) => t,
                    Err(e) => {
                        tracing::warn!("bad CONNECT payload: {e}");
                        let _ = out_tx
                            .send(Frame::new(frame.channel, ssnet::CMD_TCP_STOP_SENDING, vec![]))
                            .await;
                        continue;
                    }
                };
                spawn_tcp_connect(frame.channel, target, tcp_chans.clone(), out_tx.clone()).await;
            }
            ssnet::CMD_TCP_DATA => {
                let chans = tcp_chans.lock().await;
                if let Some(ch) = chans.get(&frame.channel) {
                    let _ = ch.inbound_tx.send(frame.data).await;
                }
            }
            ssnet::CMD_TCP_EOF | ssnet::CMD_TCP_STOP_SENDING => {
                tcp_chans.lock().await.remove(&frame.channel);
            }
            ssnet::CMD_DNS_REQ => {
                spawn_dns(frame.channel, frame.data, out_tx.clone()).await;
            }
            ssnet::CMD_UDP_OPEN => {
                let family = std::str::from_utf8(&frame.data)
                    .ok()
                    .and_then(|s| s.trim().parse::<u16>().ok())
                    .unwrap_or(ssnet::AF_INET);
                spawn_udp_open(frame.channel, family, udp_chans.clone(), out_tx.clone()).await;
            }
            ssnet::CMD_UDP_DATA => {
                let res = ssnet::parse_udp_data(&frame.data);
                let (ip, port, body) = match res {
                    Ok(v) => v,
                    Err(e) => {
                        tracing::warn!("bad UDP_DATA: {e}");
                        continue;
                    }
                };
                let body = body.to_vec();
                let chans = udp_chans.lock().await;
                if let Some(ch) = chans.get(&frame.channel) {
                    let _ = ch.inbound_tx.send((ip, port, body)).await;
                }
            }
            ssnet::CMD_UDP_CLOSE => {
                udp_chans.lock().await.remove(&frame.channel);
            }
            ssnet::CMD_HOST_REQ => {
                let entries = read_etc_hosts("/etc/hosts").unwrap_or_default();
                let payload = ssnet::encode_host_list(entries);
                let _ = out_tx
                    .send(Frame::new(0, ssnet::CMD_HOST_LIST, payload))
                    .await;
            }
            _ => {
                tracing::debug!("ignoring cmd {}", Frame::cmd_name(frame.cmd));
            }
        }
    }

    drop(out_tx);
    let _ = writer_task.await;
    Ok(())
}

// --- bootstrap-eater -------------------------------------------------------

/// Consume sshuttle's assembler-protocol bootstrap from stdin.
///
/// sshuttle/assembler.py protocol (per-module record):
///   line: name (trailing \n)         — empty name terminates
///   line: nbytes (decimal, trailing \n)
///   nbytes: zlib-compressed module bytes (single zlib stream across modules)
///
/// We don't need to decompress — we just discard everything until an empty
/// name line. After that, sshuttle's bootstrap imports server.py and calls
/// main(), which is when ssnet I/O would begin. From rushtle's POV, after
/// the empty-name marker we control stdout — we emit the sync header and
/// start serving frames.
async fn consume_sshuttle_bootstrap<R>(r: &mut tokio::io::BufReader<R>) -> Result<()>
where
    R: AsyncReadExt + Unpin,
{
    let mut total_bytes = 0u64;
    let mut module_count = 0u32;
    loop {
        let mut name_line = String::new();
        let n = r.read_line(&mut name_line).await?;
        if n == 0 {
            anyhow::bail!("EOF on stdin during bootstrap");
        }
        let name = name_line.trim_end_matches(&['\r', '\n'][..]);
        if name.is_empty() {
            tracing::info!(
                "bootstrap: consumed {module_count} modules, {total_bytes} bytes"
            );
            return Ok(());
        }
        let mut nbytes_line = String::new();
        let n = r.read_line(&mut nbytes_line).await?;
        if n == 0 {
            anyhow::bail!("EOF after module name {name:?}");
        }
        let nbytes: usize = nbytes_line
            .trim()
            .parse()
            .with_context(|| format!("bad nbytes for module {name:?}: {nbytes_line:?}"))?;
        let mut buf = vec![0u8; nbytes];
        r.read_exact(&mut buf).await?;
        total_bytes += nbytes as u64;
        module_count += 1;
        tracing::debug!("bootstrap: discarded module {name} ({nbytes} bytes)");
    }
}

// --- TCP -------------------------------------------------------------------

async fn spawn_tcp_connect(
    channel: u16,
    target: ConnectTarget,
    channels: Arc<Mutex<HashMap<u16, TcpChannel>>>,
    out_tx: FrameTx,
) {
    let (inbound_tx, mut inbound_rx) = mpsc::channel::<Vec<u8>>(64);
    {
        let mut map = channels.lock().await;
        map.insert(channel, TcpChannel { inbound_tx });
    }
    let host = target.host.clone();
    let port = target.port;

    tokio::spawn(async move {
        let stream = match TcpStream::connect((host.as_str(), port)).await {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!("connect {host}:{port} failed: {e}");
                let _ = out_tx
                    .send(Frame::new(channel, ssnet::CMD_TCP_STOP_SENDING, vec![]))
                    .await;
                channels.lock().await.remove(&channel);
                return;
            }
        };
        tracing::info!("ch={channel} TCP -> {host}:{port}");

        let (mut read_half, mut write_half) = stream.into_split();

        let writer_join = tokio::spawn(async move {
            while let Some(buf) = inbound_rx.recv().await {
                if buf.is_empty() {
                    break;
                }
                if write_half.write_all(&buf).await.is_err() {
                    break;
                }
            }
            let _ = write_half.shutdown().await;
        });

        let out_tx_clone = out_tx.clone();
        let reader_join = tokio::spawn(async move {
            let mut buf = vec![0u8; ssnet::CHUNK];
            loop {
                match read_half.read(&mut buf).await {
                    Ok(0) => break,
                    Ok(n) => {
                        let frame = Frame::new(channel, ssnet::CMD_TCP_DATA, buf[..n].to_vec());
                        if out_tx_clone.send(frame).await.is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
            let _ = out_tx_clone
                .send(Frame::new(channel, ssnet::CMD_TCP_EOF, vec![]))
                .await;
        });

        let _ = tokio::join!(writer_join, reader_join);
        channels.lock().await.remove(&channel);
        tracing::debug!("ch={channel} TCP closed");
    });
}

// --- DNS -------------------------------------------------------------------

static RESOLVERS: OnceCell<Vec<SocketAddr>> = OnceCell::const_new();
const DNS_TIMEOUT: Duration = Duration::from_secs(5);

async fn resolvers() -> &'static [SocketAddr] {
    RESOLVERS
        .get_or_init(|| async {
            let v = parse_resolv_conf("/etc/resolv.conf").unwrap_or_default();
            if v.is_empty() {
                tracing::warn!("no nameservers in /etc/resolv.conf, falling back to 1.1.1.1");
                vec![SocketAddr::from(([1, 1, 1, 1], 53))]
            } else {
                tracing::info!("DNS resolvers from resolv.conf: {v:?}");
                v
            }
        })
        .await
        .as_slice()
}

fn parse_resolv_conf(path: &str) -> std::io::Result<Vec<SocketAddr>> {
    let s = std::fs::read_to_string(path)?;
    let mut out = Vec::new();
    for line in s.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        if let Some(rest) = line.strip_prefix("nameserver") {
            let ip = rest.trim();
            if let Ok(addr) = ip.parse::<IpAddr>() {
                out.push(SocketAddr::new(addr, 53));
            }
        }
    }
    Ok(out)
}

async fn spawn_dns(channel: u16, query: Vec<u8>, out_tx: FrameTx) {
    tokio::spawn(async move {
        let resolvers = resolvers().await;
        for (i, ns) in resolvers.iter().enumerate() {
            match dns_query_one(*ns, &query).await {
                Ok(reply) => {
                    let _ = out_tx
                        .send(Frame::new(channel, ssnet::CMD_DNS_RESPONSE, reply))
                        .await;
                    return;
                }
                Err(e) => {
                    tracing::debug!("ch={channel} ns={ns} attempt {} failed: {e}", i + 1);
                }
            }
        }
        tracing::warn!("ch={channel} all DNS resolvers failed");
        let _ = out_tx
            .send(Frame::new(channel, ssnet::CMD_DNS_RESPONSE, vec![]))
            .await;
    });
}

async fn dns_query_one(ns: SocketAddr, query: &[u8]) -> Result<Vec<u8>> {
    let bind: SocketAddr = if ns.is_ipv6() {
        "[::]:0".parse().unwrap()
    } else {
        "0.0.0.0:0".parse().unwrap()
    };
    let sock = UdpSocket::bind(bind).await?;
    sock.send_to(query, ns).await?;
    let mut buf = vec![0u8; 4096];
    let n = tokio::time::timeout(DNS_TIMEOUT, sock.recv(&mut buf)).await??;
    buf.truncate(n);
    Ok(buf)
}

// --- general UDP -----------------------------------------------------------

async fn spawn_udp_open(
    channel: u16,
    family: u16,
    channels: Arc<Mutex<HashMap<u16, UdpChannel>>>,
    out_tx: FrameTx,
) {
    let bind: SocketAddr = if family == ssnet::AF_INET {
        "0.0.0.0:0".parse().unwrap()
    } else {
        "[::]:0".parse().unwrap()
    };
    let sock = match UdpSocket::bind(bind).await {
        Ok(s) => Arc::new(s),
        Err(e) => {
            tracing::warn!("ch={channel} UDP bind failed: {e}");
            return;
        }
    };
    tracing::info!("ch={channel} UDP open family={family}");

    let (inbound_tx, mut inbound_rx) = mpsc::channel::<(String, u16, Vec<u8>)>(64);
    {
        let mut map = channels.lock().await;
        map.insert(channel, UdpChannel { inbound_tx });
    }

    // outbound: client → target
    let sock_send = sock.clone();
    tokio::spawn(async move {
        while let Some((ip, port, body)) = inbound_rx.recv().await {
            let addr: SocketAddr = match format!("{ip}:{port}").parse() {
                Ok(a) => a,
                Err(e) => {
                    tracing::warn!("ch={channel} bad UDP target {ip}:{port}: {e}");
                    continue;
                }
            };
            if let Err(e) = sock_send.send_to(&body, addr).await {
                tracing::warn!("ch={channel} UDP sendto {addr}: {e}");
            }
        }
    });

    // inbound: target → client
    tokio::spawn(async move {
        let mut buf = vec![0u8; 4096];
        loop {
            match sock.recv_from(&mut buf).await {
                Ok((n, peer)) => {
                    let payload = ssnet::encode_udp_data(&peer.ip().to_string(), peer.port(), &buf[..n]);
                    if out_tx
                        .send(Frame::new(channel, ssnet::CMD_UDP_DATA, payload))
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
                Err(e) => {
                    tracing::warn!("ch={channel} UDP recv: {e}");
                    break;
                }
            }
            if !channels.lock().await.contains_key(&channel) {
                break;
            }
        }
    });
}

// --- HOST_REQ → HOST_LIST --------------------------------------------------

fn read_etc_hosts(path: &str) -> std::io::Result<Vec<(String, String)>> {
    let s = std::fs::read_to_string(path)?;
    let mut out = Vec::new();
    for line in s.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut it = line.split_whitespace();
        let ip = match it.next() {
            Some(v) => v.to_string(),
            None => continue,
        };
        for name in it {
            // skip aliases that look like comments
            if name.starts_with('#') {
                break;
            }
            out.push((name.to_string(), ip.clone()));
        }
    }
    Ok(out)
}
