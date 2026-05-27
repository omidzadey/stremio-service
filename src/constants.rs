// Copyright (C) 2017-2026 Smart Code OOD 203358507

pub const STREMIO_URL: &str = "https://web.stremio.com";
pub const APP_IDENTIFIER: &str = "com.stremio.service";
pub const APP_NAME: &str = "StremioService";
pub const APP_ICON: &[u8] = include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/icons/icon.png"));

pub const DESKTOP_FILE_PATH: &str = "/usr/share/applications";
pub const DESKTOP_FILE_NAME: &str = "com.stremio.service.desktop";
pub const AUTOSTART_CONFIG_PATH: &str = ".config/autostart";
pub const LAUNCH_AGENTS_PATH: &str = "Library/LaunchAgents";

/// Default port the gateway binds to. Matches the historical Stremio
/// streaming-server port so existing clients don't need reconfiguration.
pub const DEFAULT_GATEWAY_PORT: u16 = 11470;

/// Default bind address. Localhost-only by default; flip to 0.0.0.0 via
/// the `--bind` flag or `STREMIO_SERVICE_BIND` env var when hosting publicly.
pub const DEFAULT_GATEWAY_BIND: &str = "127.0.0.1";

/// Torbox API base URL.
pub const TORBOX_API_BASE: &str = "https://api.torbox.app";
