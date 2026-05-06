//! Embed the short git SHA into the binary at build time.
//!
//! At runtime the value is reachable via `env!("RUSHTLE_GIT_REV")` and is
//! folded into clap's `--version` output. Falls back to `unknown` when the
//! crate is being built outside a git checkout (e.g. from a published
//! source tarball or `cargo install rushtle@<version>`).
//!
//! `cargo:rerun-if-changed=.git/HEAD` would force a rebuild when the
//! current commit changes, but the docker build context omits `.git`, so
//! we leave it off — the prebuilt-image flow stamps `unknown` anyway.

use std::process::Command;

fn main() {
    let rev = Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".to_string());

    println!("cargo:rustc-env=RUSHTLE_GIT_REV={rev}");
}
