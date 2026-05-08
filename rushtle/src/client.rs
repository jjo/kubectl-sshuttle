//! Client side: redirects outbound TCP (IPv4+IPv6) and optionally UDP/53 for
//! given CIDRs to local listeners via iptables/ip6tables NAT, spawns the
//! remote rushtle server over a user-supplied shell command, and forwards
//! bytes through ssnet frames.

use crate::firewall;
use crate::ssnet::{self, Frame};
use anyhow::{anyhow, bail, Context, Result};
use std::collections::HashMap;
use std::net::{Ipv6Addr, SocketAddr};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream, UdpSocket};
use tokio::process::Command;
use tokio::sync::{mpsc, Mutex};

const REDIRECT_PORT_TCP: u16 = 12300;
const REDIRECT_PORT_DNS: u16 = 12353;

pub struct ClientArgs {
    pub remote_cmd: String,
    pub subnets: Vec<String>,
    pub listen_port: u16,
    pub manage_iptables: bool,
    pub dns: bool,
    pub dns_listen_port: u16,

    /// Microsecond per-frame delay to switch on if the startup link probe
    /// times out. Default 2000 (2 ms). Set to 0 to skip the probe entirely
    /// (useful when the user has already pinned `RUSHTLE_FRAME_DELAY_US`
    /// via env or knows their cluster is healthy and wants minimum
    /// latency).
    pub probe_fallback_us: u64,
}

type ChanMap = Arc<Mutex<HashMap<u16, mpsc::Sender<Vec<u8>>>>>;
type DnsMap = Arc<Mutex<HashMap<u16, SocketAddr>>>;

pub async fn run(args: ClientArgs) -> Result<()> {
    let port = if args.listen_port == 0 { REDIRECT_PORT_TCP } else { args.listen_port };
    let dns_port = if args.dns_listen_port == 0 { REDIRECT_PORT_DNS } else { args.dns_listen_port };

    let (subnets_v4, subnets_v6) = split_subnets_by_family(&args.subnets);
    tracing::info!(
        "rushtle client: tcp_port={port}, dns={} (port={dns_port}), v4={:?}, v6={:?}",
        args.dns,
        subnets_v4,
        subnets_v6
    );

    let listener_v4 = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, port))
        .await
        .with_context(|| format!("bind 127.0.0.1:{port}"))?;
    let listener_v6 = if !subnets_v6.is_empty() {
        Some(
            TcpListener::bind((Ipv6Addr::LOCALHOST, port))
                .await
                .with_context(|| format!("bind [::1]:{port}"))?,
        )
    } else {
        None
    };

    let dns_sock = if args.dns {
        let s = UdpSocket::bind(("127.0.0.1", dns_port))
            .await
            .with_context(|| format!("bind UDP 127.0.0.1:{dns_port}"))?;
        Some(Arc::new(s))
    } else {
        None
    };

    // Spawn kubectl-exec child + probe link health, with a respawn fallback
    // for clusters where the unchunked probe burst kills the entire
    // websocket session (some tailscale-fronted apiservers do this — the
    // pipe goes dead, not just truncates). On dead-pipe failure
    // `link_setup` kills the old child, sets `RUSHTLE_FRAME_DELAY_US`, and
    // respawns a fresh kubectl-exec session before probing again.
    let (_child, mut remote_stdin, mut remote_stdout) =
        link_setup(&args.remote_cmd, args.probe_fallback_us).await?;

    let (out_tx, mut out_rx) = mpsc::channel::<Frame>(1024);
    let _writer_task = tokio::spawn(async move {
        while let Some(frame) = out_rx.recv().await {
            tracing::debug!(
                "tx ch={} cmd={} len={}",
                frame.channel,
                Frame::cmd_name(frame.cmd),
                frame.data.len()
            );
            if let Err(e) = ssnet::write_frame(&mut remote_stdin, &frame).await {
                tracing::error!("remote stdin write: {e}");
                break;
            }
        }
    });

    let channels: ChanMap = Arc::new(Mutex::new(HashMap::new()));
    let dns_inflight: DnsMap = Arc::new(Mutex::new(HashMap::new()));

    let channels_in = channels.clone();
    let dns_in = dns_inflight.clone();
    let dns_sock_in = dns_sock.clone();

    let out_tx_pong = out_tx.clone();
    let _demux_task = tokio::spawn(async move {
        loop {
            let frame = match ssnet::read_frame(&mut remote_stdout).await {
                Ok(Some(f)) => f,
                Ok(None) => {
                    tracing::info!("remote stdout closed");
                    break;
                }
                Err(e) => {
                    tracing::error!("remote stdout read: {e}");
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
                    let _ = out_tx_pong
                        .send(Frame::new(frame.channel, ssnet::CMD_PONG, frame.data))
                        .await;
                }
                ssnet::CMD_PONG => {}
                ssnet::CMD_TCP_DATA => {
                    let map = channels_in.lock().await;
                    if let Some(tx) = map.get(&frame.channel) {
                        let _ = tx.send(frame.data).await;
                    }
                }
                ssnet::CMD_TCP_EOF | ssnet::CMD_TCP_STOP_SENDING => {
                    let mut map = channels_in.lock().await;
                    map.remove(&frame.channel);
                }
                ssnet::CMD_DNS_RESPONSE => {
                    let peer = dns_in.lock().await.remove(&frame.channel);
                    if let (Some(peer), Some(sock)) = (peer, dns_sock_in.as_ref()) {
                        if frame.data.is_empty() {
                            tracing::debug!("ch={} empty DNS response", frame.channel);
                        } else if let Err(e) = sock.send_to(&frame.data, peer).await {
                            tracing::warn!("dns reply -> {peer} failed: {e}");
                        }
                    }
                }
                _ => {}
            }
        }
    });

    let chain = format!("RUSHTLE_{}", std::process::id());
    if args.manage_iptables {
        if let Err(e) = firewall::install(&chain, port, dns_port, &subnets_v4, &subnets_v6, args.dns) {
            let _ = firewall::remove(&chain, !subnets_v6.is_empty());
            return Err(e);
        }
    }
    let chain_for_cleanup = chain.clone();
    let manage = args.manage_iptables;
    let has_v6 = !subnets_v6.is_empty();
    let cleanup = move || {
        if manage {
            let _ = firewall::remove(&chain_for_cleanup, has_v6);
        }
    };

    let next_id = Arc::new(Mutex::new(1u16));

    let cleanup_sig = cleanup.clone();
    tokio::spawn(async move {
        // Catch BOTH SIGINT and SIGTERM. ctrl_c() only handles SIGINT;
        // a `kill <pid>` (default SIGTERM) or systemd-stop would otherwise
        // skip cleanup and leave the iptables chain on the host.
        use tokio::signal::unix::{signal, SignalKind};
        let mut term = match signal(SignalKind::terminate()) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!("install SIGTERM handler: {e}");
                let _ = tokio::signal::ctrl_c().await;
                tracing::info!("ctrl-c, cleaning up");
                cleanup_sig();
                std::process::exit(0);
            }
        };
        tokio::select! {
            _ = tokio::signal::ctrl_c() => tracing::info!("SIGINT, cleaning up"),
            _ = term.recv() => tracing::info!("SIGTERM, cleaning up"),
        }
        cleanup_sig();
        std::process::exit(0);
    });

    if let Some(sock) = dns_sock.clone() {
        let out_tx_d = out_tx.clone();
        let next_id_d = next_id.clone();
        let dns_inflight_d = dns_inflight.clone();
        tokio::spawn(async move {
            let mut buf = vec![0u8; 4096];
            loop {
                let (n, peer) = match sock.recv_from(&mut buf).await {
                    Ok(v) => v,
                    Err(e) => {
                        tracing::warn!("dns recv: {e}");
                        continue;
                    }
                };
                let ch = {
                    let mut id = next_id_d.lock().await;
                    let v = *id;
                    *id = id.checked_add(1).unwrap_or(1);
                    v
                };
                dns_inflight_d.lock().await.insert(ch, peer);
                let frame = Frame::new(ch, ssnet::CMD_DNS_REQ, buf[..n].to_vec());
                if out_tx_d.send(frame).await.is_err() {
                    break;
                }
                tracing::debug!("dns ch={ch} {} bytes from {peer}", n);
            }
        });
    }

    // v6 accept loop
    if let Some(l6) = listener_v6 {
        let out_tx_6 = out_tx.clone();
        let chans_6 = channels.clone();
        let next_id_6 = next_id.clone();
        tokio::spawn(async move {
            accept_loop(l6, true, out_tx_6, chans_6, next_id_6).await;
        });
    }

    accept_loop(listener_v4, false, out_tx.clone(), channels.clone(), next_id.clone()).await;
    cleanup();
    Ok(())
}

async fn accept_loop(
    listener: TcpListener,
    is_v6: bool,
    out_tx: mpsc::Sender<Frame>,
    channels: ChanMap,
    next_id: Arc<Mutex<u16>>,
) {
    loop {
        let (sock, _peer) = match listener.accept().await {
            Ok(v) => v,
            Err(e) => {
                tracing::error!("accept ({}): {e}", if is_v6 { "v6" } else { "v4" });
                continue;
            }
        };
        let dst = match firewall::original_dst(&sock, is_v6) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!("SO_ORIGINAL_DST ({}): {e}", if is_v6 { "v6" } else { "v4" });
                continue;
            }
        };

        let ch = {
            let mut id = next_id.lock().await;
            let v = *id;
            *id = id.checked_add(1).unwrap_or(1);
            v
        };

        let (in_tx, in_rx) = mpsc::channel::<Vec<u8>>(64);
        channels.lock().await.insert(ch, in_tx);

        let out_tx_c = out_tx.clone();
        let chans_c = channels.clone();
        tokio::spawn(async move {
            handle_conn(ch, sock, dst, in_rx, out_tx_c, chans_c).await;
        });
    }
}

async fn handle_conn(
    ch: u16,
    sock: TcpStream,
    dst: (String, u16),
    mut in_rx: mpsc::Receiver<Vec<u8>>,
    out_tx: mpsc::Sender<Frame>,
    channels: ChanMap,
) {
    tracing::info!("ch={ch} new conn -> {}:{}", dst.0, dst.1);

    let family = if dst.0.contains(':') { ssnet::AF_INET6 } else { ssnet::AF_INET };
    if out_tx
        .send(Frame::new(
            ch,
            ssnet::CMD_TCP_CONNECT,
            ssnet::encode_connect(family, &dst.0, dst.1),
        ))
        .await
        .is_err()
    {
        return;
    }

    let (mut rh, mut wh) = sock.into_split();

    let out_tx_a = out_tx.clone();
    let a = tokio::spawn(async move {
        let mut buf = vec![0u8; ssnet::CHUNK];
        loop {
            match rh.read(&mut buf).await {
                Ok(0) => break,
                Ok(n) => {
                    if out_tx_a
                        .send(Frame::new(ch, ssnet::CMD_TCP_DATA, buf[..n].to_vec()))
                        .await
                        .is_err()
                    {
                        return;
                    }
                }
                Err(_) => break,
            }
        }
        let _ = out_tx_a.send(Frame::new(ch, ssnet::CMD_TCP_EOF, vec![])).await;
    });

    let b = tokio::spawn(async move {
        while let Some(buf) = in_rx.recv().await {
            if buf.is_empty() {
                break;
            }
            if wh.write_all(&buf).await.is_err() {
                break;
            }
        }
        let _ = wh.shutdown().await;
    });

    let _ = tokio::join!(a, b);
    channels.lock().await.remove(&ch);
    tracing::debug!("ch={ch} closed");
}

// --- child spawn + link setup ----------------------------------------------

/// Tokio child handle + the two pipe ends we hand to the writer/demux tasks.
type RemoteIo = (
    tokio::process::Child,
    tokio::process::ChildStdin,
    tokio::io::BufReader<tokio::process::ChildStdout>,
);

/// Spawn `sh -c <remote_cmd>` and grab its stdio pipes. Stderr inherits so
/// kubectl-exec error messages reach the user.
async fn spawn_child(remote_cmd: &str) -> Result<RemoteIo> {
    let mut child = Command::new("sh")
        .arg("-c")
        .arg(remote_cmd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .with_context(|| format!("spawn remote: {remote_cmd}"))?;
    let stdin = child.stdin.take().ok_or_else(|| anyhow!("no remote stdin"))?;
    let stdout = child.stdout.take().ok_or_else(|| anyhow!("no remote stdout"))?;
    Ok((child, stdin, tokio::io::BufReader::new(stdout)))
}

/// Heuristic: did this error indicate the remote pipe is dead? Inner-retry
/// inside `ensure_link` cannot recover from a closed pipe, so we surface
/// dead-pipe failures specifically and respawn the kubectl-exec session.
fn is_dead_pipe_err(e: &anyhow::Error) -> bool {
    let s = format!("{e:#}");
    s.contains("Broken pipe")
        || s.contains("os error 32")
        || s.contains("EOF on remote stdout")
        || s.contains("UnexpectedEof")
        || s.contains("unexpected end of file")
}

/// Establish the rushtle <-> kubectl-exec link with auto-recovery for
/// burst-kill apiserver middleware.
///
/// Strategy:
///   1. Spawn kubectl-exec, read sync header, run probe with the current
///      `FRAME_DELAY_US` (zero by default; honors any pre-set env).
///   2. On success: return the live io handles.
///   3. On dead-pipe failure (websocket killed during probe burst): kill
///      the old child, set `FRAME_DELAY_US = fallback_us`, respawn a fresh
///      kubectl-exec, and probe again. The second probe runs with chunking
///      from frame 1 — survives clusters where back-to-back unchunked
///      writes tear down the entire session.
///   4. On any other failure (frames lost but pipe alive, payload mismatch,
///      etc.): the inner retry inside `ensure_link` already engaged
///      chunking on the same pipe; we propagate its result.
async fn link_setup(remote_cmd: &str, fallback_us: u64) -> Result<RemoteIo> {
    let (mut child, mut stdin, mut stdout) = spawn_child(remote_cmd).await?;

    if let Err(e) = ssnet::read_sync_header(&mut stdout).await {
        return Err(anyhow!("waiting for server sync header: {e}"));
    }
    tracing::info!("got server sync header, entering ssnet mode");

    match ensure_link(&mut stdin, &mut stdout, fallback_us).await {
        Ok(()) => Ok((child, stdin, stdout)),
        Err(e) if is_dead_pipe_err(&e) && fallback_us > 0 => {
            tracing::warn!(
                "kubectl-exec died during probe ({:#}); respawning with FRAME_DELAY={fallback_us}us pre-set",
                e
            );
            // Kill old child and drop pipes so the host-side fd is released.
            let _ = child.kill().await;
            drop(stdin);
            drop(stdout);

            // Pre-set the delay BEFORE we start writing into the new pipe —
            // this is the whole point of the respawn path.
            ssnet::set_frame_delay_us(fallback_us);

            let (child2, mut stdin2, mut stdout2) = spawn_child(remote_cmd).await?;
            if let Err(e) = ssnet::read_sync_header(&mut stdout2).await {
                return Err(anyhow!("waiting for server sync header (respawn): {e}"));
            }
            tracing::info!("got server sync header on respawn, re-running probe");

            // On the respawn we don't want ensure_link's inner retry
            // doubling up, but the worst case is one extra probe round —
            // if that also fails the error message is clear enough.
            ensure_link(&mut stdin2, &mut stdout2, fallback_us)
                .await
                .with_context(|| {
                    format!(
                        "probe still failing after respawn with FRAME_DELAY={fallback_us}us — \
                         apiserver layer drops frames even with chunking. \
                         Try a larger --probe-fallback-us, or fall back to --rushtle-server."
                    )
                })?;
            Ok((child2, stdin2, stdout2))
        }
        Err(e) => Err(e),
    }
}

// --- link health probe -----------------------------------------------------

/// Base channel for probe PING/PONG. We allocate `PROBE_BURST_FRAMES`
/// consecutive channels above the `next_id` allocator's wrapping range
/// (which keeps to 1..=u16::MAX-1) so probe channels cannot collide with
/// real connections.
const PROBE_CHANNEL_BASE: u16 = 0xFF00;

/// Number of frames sent back-to-back during the probe.
///
/// The `--rushtle` truncation seen on tailscale-fronted apiservers in
/// practice is NOT triggered by single-write size (a lone 2 KB PING goes
/// through fine) — it's triggered by rapid back-to-back writes being
/// coalesced by the kernel pipe + apiserver buffer into a single 1+ KB
/// websocket frame which middleware then drops. So the probe must imitate
/// real traffic: send many small frames in quick succession and verify
/// all replies come back.
const PROBE_BURST_FRAMES: usize = 16;

/// Per-frame payload size. 256 bytes keeps each individual frame small
/// (264 bytes total with the 8-byte header) so any failure points to
/// rate-driven coalescing rather than per-frame oversize.
const PROBE_FRAME_PAYLOAD: usize = 256;

/// How long to wait for all PONGs before declaring the probe failed and
/// turning on chunking.
const PROBE_TIMEOUT: Duration = Duration::from_secs(3);

/// Verify the kubectl-exec stdio path can carry a burst of small frames
/// without losing any. On failure, switch on per-frame delay and retry once.
///
/// This catches the dominant failure mode in `--rushtle` mode: middleware
/// coalesces back-to-back ssnet frames into 1+ KB websocket messages and
/// then truncates them, leaving the tunnel idle until the apiserver gives
/// up and closes it. With this probe we detect it in the first ~3 s and
/// surface a clear log line + auto-fallback.
async fn ensure_link<W, R>(stdin: &mut W, stdout: &mut R, fallback_us: u64) -> Result<()>
where
    W: AsyncWriteExt + Unpin,
    R: AsyncReadExt + Unpin,
{
    if fallback_us == 0 {
        tracing::info!("link probe disabled (--probe-fallback-us=0)");
        return Ok(());
    }

    let payloads = make_probe_payloads();
    tracing::info!(
        "starting link probe ({} frames × {} byte PINGs, timeout {:?}, fallback {fallback_us}us)",
        PROBE_BURST_FRAMES,
        PROBE_FRAME_PAYLOAD,
        PROBE_TIMEOUT
    );

    match probe_once(stdin, stdout, &payloads).await {
        Ok(()) => {
            tracing::info!(
                "link probe OK ({} burst PINGs round-tripped, no chunking needed)",
                PROBE_BURST_FRAMES
            );
            Ok(())
        }
        Err(e) => {
            tracing::warn!(
                "link probe failed ({e}); enabling FRAME_DELAY={fallback_us}us auto-fallback and retrying"
            );
            ssnet::set_frame_delay_us(fallback_us);
            probe_once(stdin, stdout, &payloads).await.with_context(|| {
                format!(
                    "link probe still failing with FRAME_DELAY={fallback_us}us — \
                     apiserver layer drops frames even with chunking. \
                     Try a larger --probe-fallback-us, or fall back to --rushtle-server."
                )
            })?;
            tracing::info!("link probe OK with FRAME_DELAY={fallback_us}us auto-fallback");
            Ok(())
        }
    }
}

fn make_probe_payloads() -> Vec<Vec<u8>> {
    // Distinct deterministic content per frame — lets us detect the case
    // where one frame's payload is silently delivered to the channel of
    // another (a coalescing-and-misalign symptom).
    (0..PROBE_BURST_FRAMES)
        .map(|i| {
            (0..PROBE_FRAME_PAYLOAD)
                .map(|j| (((i * 31) + j) & 0xff) as u8)
                .collect()
        })
        .collect()
}

/// Fire the burst, then collect PONGs until either all `PROBE_BURST_FRAMES`
/// are accounted for or the timeout fires.
///
/// Server's housekeeping frames (initial `PING(chicken)` on channel 0,
/// empty `CMD_ROUTES`, etc.) are skipped; only PONGs in the reserved
/// `PROBE_CHANNEL_BASE` range count.
async fn probe_once<W, R>(stdin: &mut W, stdout: &mut R, payloads: &[Vec<u8>]) -> Result<()>
where
    W: AsyncWriteExt + Unpin,
    R: AsyncReadExt + Unpin,
{
    for (i, payload) in payloads.iter().enumerate() {
        let ch = PROBE_CHANNEL_BASE + i as u16;
        let frame = Frame::new(ch, ssnet::CMD_PING, payload.clone());
        ssnet::write_frame(stdin, &frame)
            .await
            .with_context(|| format!("sending probe PING #{i} (ch={ch})"))?;
    }

    let mut seen = vec![false; payloads.len()];
    let result = tokio::time::timeout(PROBE_TIMEOUT, async {
        let mut got = 0usize;
        while got < payloads.len() {
            match ssnet::read_frame(stdout).await {
                Ok(Some(f)) if f.cmd == ssnet::CMD_PONG => {
                    let idx = f.channel.wrapping_sub(PROBE_CHANNEL_BASE) as usize;
                    if idx >= payloads.len() {
                        tracing::debug!(
                            "probe: skipping unexpected PONG on ch={}",
                            f.channel
                        );
                        continue;
                    }
                    if f.data != payloads[idx] {
                        bail!(
                            "probe PONG #{idx} payload mismatch ({} bytes received, {} expected)",
                            f.data.len(),
                            payloads[idx].len()
                        );
                    }
                    if !seen[idx] {
                        seen[idx] = true;
                        got += 1;
                    }
                }
                Ok(Some(f)) => {
                    tracing::debug!(
                        "probe: skipping {} on ch={}",
                        Frame::cmd_name(f.cmd),
                        f.channel
                    );
                }
                Ok(None) => bail!("EOF on remote stdout during probe"),
                Err(e) => return Err(e),
            }
        }
        Ok(())
    })
    .await;

    match result {
        Ok(Ok(())) => Ok(()),
        Ok(Err(e)) => Err(e),
        Err(_elapsed) => {
            let lost: Vec<usize> = seen
                .iter()
                .enumerate()
                .filter_map(|(i, &v)| (!v).then_some(i))
                .collect();
            Err(anyhow!(
                "probe timeout after {PROBE_TIMEOUT:?}, lost {}/{} PONGs (idx {:?})",
                lost.len(),
                payloads.len(),
                lost
            ))
        }
    }
}

fn split_subnets_by_family(subnets: &[String]) -> (Vec<String>, Vec<String>) {
    let mut v4 = Vec::new();
    let mut v6 = Vec::new();
    for s in subnets {
        if s.contains(':') {
            v6.push(s.clone());
        } else {
            v4.push(s.clone());
        }
    }
    (v4, v6)
}
