// Copyright (C) 2017-2026 Smart Code OOD 203358507

use std::net::IpAddr;
use std::path::PathBuf;

use clap::Parser;

/// Command-line arguments for the Torbox-backed Stremio streaming server.
///
/// The original stremio-service shipped a `server.js` blob, ffmpeg and an
/// auto-updater. This fork replaces all of that with an in-process Rust
/// gateway that translates Stremio streaming-server protocol calls into
/// Torbox API calls, so the CLI is intentionally much smaller.
#[derive(Parser, Debug, Clone)]
#[command(author, version, about, long_about = None)]
pub struct Args {
    /// Open an URL with a custom `stremio://` scheme and exit.
    ///
    /// If empty URL or no url is provided, the service will skip this argument.
    #[clap(short, long)]
    pub open: Option<String>,

    /// Path to a TOML config file. Defaults to
    /// `$XDG_CONFIG_HOME/stremio-service/config.toml` (or the platform
    /// equivalent). Settings from the file are overridden by env vars and
    /// by the flags below.
    #[clap(long)]
    pub config: Option<PathBuf>,

    /// Torbox API key. Also read from the `TORBOX_API_KEY` env var or the
    /// `torbox_api_key` field of the config file.
    #[clap(long, env = "TORBOX_API_KEY")]
    pub torbox_api_key: Option<String>,

    /// IP address to bind the gateway to. Defaults to `127.0.0.1`.
    /// Set to `0.0.0.0` for public hosting (behind TLS).
    #[clap(long, env = "STREMIO_SERVICE_BIND")]
    pub bind: Option<IpAddr>,

    /// Port to bind the gateway to. Defaults to `11470` — the well-known
    /// Stremio streaming-server port.
    #[clap(long, env = "STREMIO_SERVICE_PORT")]
    pub port: Option<u16>,

    /// Run without a system-tray icon. Auto-enabled when no display is
    /// available (e.g. on a headless host). Always pair with this flag
    /// when running as a systemd service.
    #[clap(long)]
    pub headless: bool,
}
