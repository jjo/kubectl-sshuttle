use anyhow::Result;
use clap::{Parser, Subcommand};

mod client;
mod firewall;
mod server;
mod ssnet;

/// Version string baked into clap's `--version` output: crate semver +
/// short git sha (filled in by build.rs). Examples:
///
///   rushtle 0.2.0+ace3751
///   rushtle 0.2.0+unknown   (built outside a git checkout)
const VERSION: &str = concat!(env!("CARGO_PKG_VERSION"), "+", env!("RUSHTLE_GIT_REV"));

#[derive(Parser, Debug)]
#[command(version = VERSION, about = "sshuttle-compatible TCP tunnel (rushtle)", long_about = None)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,

    /// Verbose logs (info). Use -vv for debug.
    #[arg(short, long, action = clap::ArgAction::Count, global = true)]
    verbose: u8,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// Run the in-pod side: read ssnet frames from stdin, open targets, write
    /// responses to stdout.
    Server {
        /// Consume sshuttle's assembler-protocol bootstrap from stdin before
        /// entering ssnet mode. Use this when paired with a stock sshuttle
        /// client (which always sends a python bootstrap to the remote).
        #[arg(long)]
        compat_bootstrap: bool,

        /// Number of leading stdin bytes that contain the raw assembler.py
        /// source (sshuttle writes those before the zlib-compressed module
        /// records). When invoked via the `-c PYSCRIPT` shim this is parsed
        /// out of the pyscript automatically; otherwise pass it explicitly.
        #[arg(long, default_value_t = 0)]
        assembler_bytes: u64,
    },

    /// Run the local side: redirect outbound TCP for SUBNETS through the
    /// remote command and forward bytes via ssnet frames.
    Client {
        /// Remote command launching `rushtle server`. Run via `sh -c`.
        #[arg(long)]
        cmd: String,

        /// Local port for iptables REDIRECT target (TCP).
        #[arg(long, default_value_t = 12300)]
        listen_port: u16,

        /// Skip iptables setup. Useful for tests where rules are pre-installed
        /// or another process manages them.
        #[arg(long)]
        no_iptables: bool,

        /// Tunnel DNS queries (UDP/53) through the remote.
        #[arg(long)]
        dns: bool,

        /// Local port for iptables REDIRECT target (UDP/53). Only used with
        /// --dns.
        #[arg(long, default_value_t = 12353)]
        dns_listen_port: u16,

        /// sshuttle compat flag — accepted for parity. rushtle has no
        /// flow-window today, so behavior is already "no latency control".
        #[arg(long = "no-latency-control")]
        no_latency_control: bool,

        /// sshuttle compat flag — accepted for parity, no-op.
        #[arg(long = "latency-control")]
        latency_control: bool,

        /// Microsecond per-frame delay to enable when the startup link
        /// probe times out. The probe sends a 2 KB PING right after the
        /// sync header — if the kubectl-exec layer drops it, we set the
        /// inter-frame delay to this value and retry. 0 disables the
        /// probe entirely. Default 2000 (2 ms).
        #[arg(long, default_value_t = 2000)]
        probe_fallback_us: u64,

        /// CIDRs to route through the tunnel.
        #[arg(required = true)]
        subnets: Vec<String>,
    },
}

/// Parse the `stdin.read(N)` count out of sshuttle's pyscript. Returns 0 if
/// not found.
fn parse_assembler_bytes(pyscript: &str) -> u64 {
    // Look for the literal `stdin.read(<digits>)`.
    if let Some(start) = pyscript.find("stdin.read(") {
        let tail = &pyscript[start + "stdin.read(".len()..];
        if let Some(end) = tail.find(')') {
            if let Ok(n) = tail[..end].trim().parse::<u64>() {
                return n;
            }
        }
    }
    0
}

fn init_tracing(verbose: u8) {
    let level = match verbose {
        0 => "info",
        1 => "debug",
        _ => "trace",
    };
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(format!("rushtle={level}")));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .try_init();
}

fn main() -> Result<()> {
    // Special case: when rushtle is invoked as `rushtle -c <PYSCRIPT>` (the
    // shape sshuttle expects from a python interpreter), shortcut into
    // server compat-bootstrap mode and parse out the assembler.py byte count
    // from PYSCRIPT.
    let argv: Vec<String> = std::env::args().collect();
    if argv.len() >= 3 && argv[1] == "-c" {
        let pyscript = &argv[2];
        let n = parse_assembler_bytes(pyscript);
        // Verbose level — sshuttle puts `verbosity=N;` in the pyscript.
        let verbose = if pyscript.contains("verbosity=2") {
            2
        } else if pyscript.contains("verbosity=1") {
            1
        } else {
            0
        };
        init_tracing(verbose);
        ssnet::init_frame_delay_from_env();
        tracing::info!("rushtle invoked as python shim (-c), assembler_bytes={n}");
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()?;
        return rt.block_on(server::run(true, n));
    }

    let cli = Cli::parse();
    init_tracing(cli.verbose);
    ssnet::init_frame_delay_from_env();

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;

    rt.block_on(async move {
        match cli.cmd {
            Cmd::Server {
                compat_bootstrap,
                assembler_bytes,
            } => server::run(compat_bootstrap, assembler_bytes).await,
            Cmd::Client {
                cmd,
                listen_port,
                no_iptables,
                dns,
                dns_listen_port,
                no_latency_control,
                latency_control,
                probe_fallback_us,
                subnets,
            } => {
                if no_latency_control {
                    tracing::info!("--no-latency-control: accepted (rushtle has no flow-window)");
                }
                if latency_control {
                    tracing::warn!("--latency-control: accepted but unimplemented");
                }
                client::run(client::ClientArgs {
                    remote_cmd: cmd,
                    subnets,
                    listen_port,
                    manage_iptables: !no_iptables,
                    dns,
                    dns_listen_port,
                    probe_fallback_us,
                })
                .await
            }
        }
    })
}
