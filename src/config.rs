// Copyright (C) 2017-2026 Smart Code OOD 203358507

use std::fs;
use std::net::{IpAddr, Ipv4Addr};
use std::path::PathBuf;

use anyhow::{bail, Context, Error};
use serde::{Deserialize, Serialize};

use crate::args::Args;
use crate::constants::{DEFAULT_GATEWAY_BIND, DEFAULT_GATEWAY_PORT};

/// Top-level application configuration: gateway + everything we need to
/// reach Torbox.
#[derive(Debug, Clone)]
pub struct Config {
    /// The user's home directory. Used on `*nix` for autostart and lockfile paths.
    #[cfg_attr(any(not(feature = "bundled"), target_os = "windows"), allow(dead_code))]
    pub home_dir: PathBuf,

    /// Tray icon scratch directory.
    pub tray_icon: PathBuf,

    /// Lockfile guarding against double-spawn.
    pub lockfile: PathBuf,

    /// Gateway + Torbox config.
    pub gateway: GatewayConfig,

    /// Whether the operator asked for headless mode (no tray).
    pub headless: bool,
}

/// Knobs that affect the in-process gateway and its Torbox upstream.
#[derive(Clone)]
pub struct GatewayConfig {
    pub torbox_api_key: String,
    pub bind: IpAddr,
    pub port: u16,
    /// Extra `Origin` values that should be added to the CORS allowlist on
    /// top of the built-in Stremio origins. Useful when hosting publicly
    /// behind a custom web client.
    pub extra_allowed_origins: Vec<String>,
    /// Public base URL (e.g. `https://stremio.example.com`) that the
    /// gateway is reachable at. Used when generating absolute URLs in
    /// proxied HLS playlists. If `None`, segments are kept as relative
    /// URLs (which is fine for direct redirect mode).
    pub public_base_url: Option<String>,
    /// Whether to ALWAYS route playback through Torbox transcoding even if
    /// the source codec would otherwise be directly playable.
    pub force_transcode: bool,
}

impl std::fmt::Debug for GatewayConfig {
    /// Manual `Debug` impl that redacts the Torbox API key. The full
    /// config is logged at startup, so leaking the key here would expose
    /// it in journal logs / stdout pipes.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let key = &self.torbox_api_key;
        let masked = if key.len() > 8 {
            format!("{}…{}", &key[..4], &key[key.len() - 4..])
        } else if key.is_empty() {
            "<unset>".to_owned()
        } else {
            "<redacted>".to_owned()
        };
        f.debug_struct("GatewayConfig")
            .field("torbox_api_key", &masked)
            .field("bind", &self.bind)
            .field("port", &self.port)
            .field("extra_allowed_origins", &self.extra_allowed_origins)
            .field("public_base_url", &self.public_base_url)
            .field("force_transcode", &self.force_transcode)
            .finish()
    }
}

/// Disk shape of the config file (TOML). Everything optional; we layer it
/// with env vars and CLI args.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields, rename_all = "snake_case")]
pub struct FileConfig {
    pub torbox_api_key: Option<String>,
    pub bind: Option<IpAddr>,
    pub port: Option<u16>,
    pub extra_allowed_origins: Vec<String>,
    pub public_base_url: Option<String>,
    pub force_transcode: Option<bool>,
}

impl Config {
    pub fn new(args: Args) -> Result<Self, Error> {
        let home_dir = dirs::home_dir().context("Failed to get home dir")?;
        let cache_dir = dirs::cache_dir().context("Failed to get cache dir")?;

        let tray_icon = if cfg!(target_os = "linux") {
            // Fall back to the cache dir when XDG_RUNTIME_DIR isn't set,
            // which is the common case on headless hosts.
            dirs::runtime_dir()
                .unwrap_or_else(|| cache_dir.clone())
                .join("stremio-service")
        } else {
            PathBuf::new()
        };

        let lockfile = cache_dir.join("stremio-service.lock");

        let config_path = args.config.clone().or_else(default_config_path);
        let file = load_file_config(config_path.as_deref())?;

        let torbox_api_key = args
            .torbox_api_key
            .clone()
            .or(file.torbox_api_key)
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "Torbox API key not set. Pass --torbox-api-key, set TORBOX_API_KEY, \
                     or add `torbox_api_key = \"…\"` to ~/.config/stremio-service/config.toml."
                )
            })?;

        let bind = args.bind.or(file.bind).unwrap_or_else(|| {
            DEFAULT_GATEWAY_BIND
                .parse()
                .unwrap_or(IpAddr::V4(Ipv4Addr::LOCALHOST))
        });
        let port = args.port.or(file.port).unwrap_or(DEFAULT_GATEWAY_PORT);

        let gateway = GatewayConfig {
            torbox_api_key,
            bind,
            port,
            extra_allowed_origins: file.extra_allowed_origins,
            public_base_url: file.public_base_url,
            force_transcode: file.force_transcode.unwrap_or(false),
        };

        Ok(Self {
            home_dir,
            tray_icon,
            lockfile,
            gateway,
            headless: args.headless || is_headless_environment(),
        })
    }
}

fn default_config_path() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("stremio-service").join("config.toml"))
}

fn load_file_config(path: Option<&std::path::Path>) -> Result<FileConfig, Error> {
    let Some(path) = path else {
        return Ok(FileConfig::default());
    };
    if !path.exists() {
        return Ok(FileConfig::default());
    }
    let raw = fs::read_to_string(path)
        .with_context(|| format!("Failed to read config file at {}", path.display()))?;
    match toml::from_str::<FileConfig>(&raw) {
        Ok(file) => Ok(file),
        Err(err) => bail!("Invalid config file at {}: {err}", path.display()),
    }
}

fn is_headless_environment() -> bool {
    // No DISPLAY/WAYLAND_DISPLAY on Linux generally means no GUI session.
    if cfg!(target_os = "linux") {
        std::env::var_os("DISPLAY").is_none() && std::env::var_os("WAYLAND_DISPLAY").is_none()
    } else {
        false
    }
}
