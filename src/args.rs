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

    /// Skip the HLS playlist proxy and have the browser stream the
    /// transcoded HLS playlist + segments directly from Torbox.
    ///
    /// Off by default. When set, after Torbox confirms the transcoder is
    /// hot, the gateway 302-redirects `/hlsv2/<id>/master.m3u8` to the
    /// upstream Torbox HLS URL (which has the API key in its query
    /// string). Browser then talks straight to `*.tb-cdn.io` for the
    /// playlist and every segment.
    ///
    /// Tradeoff:
    ///   - Eliminates the gateway from the per-segment bandwidth path
    ///     (significant on multi-Mbps video streams).
    ///   - **Leaks the Torbox API token to the browser** — it ends up in
    ///     network panels, dev tools history, browser history, and any
    ///     screen-share. Only enable on single-user self-hosted setups.
    ///
    /// Direct-play (non-transcoded) requests already 307 to Torbox CDN
    /// presigned URLs regardless of this flag.
    ///
    /// Accepts a value (`--direct-hls true`/`false`) or no value (just
    /// `--direct-hls` enables it). Env var values `1`, `true`, and `yes`
    /// (case-insensitive) enable; anything else disables.
    #[clap(
        long,
        env = "STREMIO_SERVICE_DIRECT_HLS",
        num_args = 0..=1,
        default_missing_value = "true",
        default_value_t = false,
        value_parser = parse_bool_lenient,
    )]
    pub direct_hls: bool,
}

/// Lenient boolean parser: accepts `1`/`true`/`yes`/`on` (and the negations
/// `0`/`false`/`no`/`off`) case-insensitively, so env-var values like
/// `STREMIO_SERVICE_DIRECT_HLS=1` work alongside `--direct-hls true`.
fn parse_bool_lenient(s: &str) -> Result<bool, String> {
    match s.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Ok(true),
        "0" | "false" | "no" | "off" | "" => Ok(false),
        other => Err(format!(
            "expected a boolean (1/0, true/false, yes/no, on/off), got `{other}`"
        )),
    }
}
