//! Client side: redirects outbound TCP (IPv4+IPv6) and optionally UDP/53 for
//! given CIDRs to local listeners via iptables/ip6tables NAT, spawns the
//! remote rushtle server over a user-supplied shell command, and forwards
//! bytes through ssnet frames.

use crate::firewall;
use crate::ssnet::{self, Frame};
use anyhow::{anyhow, Context, Result};
use std::collections::HashMap;
use std::net::{Ipv6Addr, SocketAddr};
use std::process::Stdio;
use std::sync::Arc;
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

    let mut child = Command::new("sh")
        .arg("-c")
        .arg(&args.remote_cmd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .with_context(|| format!("spawn remote: {}", args.remote_cmd))?;

    let remote_stdin = child.stdin.take().ok_or_else(|| anyhow!("no remote stdin"))?;
    let remote_stdout = child.stdout.take().ok_or_else(|| anyhow!("no remote stdout"))?;

    let (out_tx, mut out_rx) = mpsc::channel::<Frame>(1024);
    let mut remote_stdin = remote_stdin;
    let _writer_task = tokio::spawn(async move {
        while let Some(frame) = out_rx.recv().await {
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
    let mut remote_stdout = tokio::io::BufReader::new(remote_stdout);

    if let Err(e) = ssnet::read_sync_header(&mut remote_stdout).await {
        return Err(anyhow!("waiting for server sync header: {e}"));
    }
    tracing::info!("got server sync header, entering ssnet mode");

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
        let _ = tokio::signal::ctrl_c().await;
        tracing::info!("ctrl-c, cleaning up");
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
