# Stremio Service — Torbox Gateway

[![GitHub Workflow Status (with event)](https://img.shields.io/github/actions/workflow/status/stremio/stremio-service/build.yml?label=build%20(master))](https://github.com/Stremio/stremio-service/actions/workflows/build.yml?query=branch%3Amaster)

A fork of the official Stremio streaming companion service that **replaces
the bundled `server.js` + ffmpeg pipeline with a Torbox-backed Rust
gateway**. Instead of torrenting and transcoding on the local machine, this
service:

1. Accepts the same streaming-server protocol Stremio expects on
   `http://localhost:11470`.
2. Resolves magnet links coming from addons (Torrentio, Comet, …) on the
   user's [Torbox](https://torbox.app) account.
3. Streams the original file directly when the codec is supported, or
   asks Torbox to transcode it on the fly (Pro plan) when it isn't.

The Stremio client treats this service exactly like the official one —
**no addon installation, no Stremio code changes**. You just paste this
service's URL into Stremio's "Streaming server URL" setting.

## How it integrates with Stremio

Stremio expects a "streaming server" that exposes a small set of HTTP
endpoints (`/settings`, `/probe`, `/<infoHash>/<idx>`, `/hlsv2/<id>/…`,
etc.). The classic service bundled a precompiled `server.js` blob and
spawned it locally to provide them — that blob is what this fork
replaces.

```
                          ┌─────────────────────────┐
   Stremio Web / Desktop  │  http://localhost:11470 │     Torbox API
   ┌──────────────────┐   │  (this service)         │   ┌──────────────┐
   │  Settings ───────┼───►  /settings, /probe …    ├───►  /torrents,  │
   │  Player <───m3u8─┼───  /hlsv2/<id>/playlist…   ◄───┤  /stream/…   │
   └──────────────────┘   └─────────────────────────┘   └──────────────┘
```

## Configuration

The gateway needs a Torbox API key. It can be provided via, in order of
precedence:

1. `--torbox-api-key <KEY>` CLI flag.
2. `TORBOX_API_KEY` environment variable.
3. `torbox_api_key` field in a TOML config file (`--config <PATH>` or
   defaults to `~/.config/stremio-service/config.toml` on Linux, the
   equivalent on macOS/Windows).

```toml
# ~/.config/stremio-service/config.toml
torbox_api_key = "your-torbox-pro-key"
bind = "0.0.0.0"          # default: 127.0.0.1
port = 11470              # default: 11470
public_base_url = "https://stremio.example.com"   # only when behind TLS
extra_allowed_origins = ["https://my-fork.example.com"]
force_transcode = false   # default; set to true to always go via HLS
direct_hls      = false   # default; see "HLS proxy vs direct redirect" below
```

CLI flags:

| Flag | Description |
| --- | --- |
| `--torbox-api-key` | Torbox API key (env: `TORBOX_API_KEY`) |
| `--bind` | IP to bind on (default `127.0.0.1`) |
| `--port` | Port to bind on (default `11470`) |
| `--config` | Path to a TOML config file |
| `--headless` | Skip the system-tray icon (recommended on servers) |
| `--direct-hls` | Redirect HLS instead of proxying (env: `STREMIO_SERVICE_DIRECT_HLS`) |
| `--open <url>` | Handle a `stremio://` URL and exit |

## Hosting modes

### Local (single user)

```sh
TORBOX_API_KEY=… cargo run --release
```

Then in Stremio: **Settings → Streaming server URL →
`http://localhost:11470`**. That's it.

### Public (multi user)

Bind to `0.0.0.0` behind a reverse proxy that terminates TLS
(`stremio.example.com`):

```sh
TORBOX_API_KEY=… \
  ./stremio-service --headless --bind 0.0.0.0 --port 11470
```

The gateway sets a CORS allowlist for `https://web.stremio.com`,
`*.strem.io`, `*.stremio.com` and any extra origins you list in the
config. Because the HLS playlist contains the Torbox API key, the
gateway proxies the `.m3u8` (stripping the token) — segments are
served directly by Torbox's CDN (no token, no PII).

### HLS proxy vs direct redirect

For transcoded (HEVC, AV1, etc.) playback the gateway has two modes:

- **Proxy mode (default)** — the gateway fetches Torbox's HLS playlist,
  rewrites segment URLs back to itself so the Torbox API token never
  reaches the browser, and proxies every segment. Safer; uses your
  gateway's bandwidth for video bytes.
- **Direct mode** — enable with `--direct-hls` /
  `STREMIO_SERVICE_DIRECT_HLS=1`. The gateway pre-warms the Torbox
  transcoder, waits for it to come online, then 302-redirects to the raw
  Torbox HLS URL. Browser fetches the playlist and every segment
  directly from `*.tb-cdn.io`. **Your Torbox API token is visible in the
  browser's network panel and history.** Only enable for single-user
  self-hosted setups where you trust everyone with access to that
  browser — never on a public multi-user instance.

## What's different from the upstream service

| | Upstream `stremio-service` | This fork |
| --- | --- | --- |
| Torrent client | Local (bundled `server.js`) | Torbox cloud |
| Transcoder | Local `ffmpeg`/`ffprobe` | Torbox `/stream/createstream` |
| Bundled binaries | `server.js`, `ffmpeg`, `ffprobe`, `stremio-runtime` (~240 MB) | None |
| Auto-updater | Yes (downloaded `server.js` from `dl.strem.io`) | Removed |
| Storage | Local cache | Torbox account |
| Resumes | Local | Stremio handles via Range to Torbox CDN |

## Development

```sh
git clone https://github.com/Stremio/stremio-service
cd stremio-service
TORBOX_API_KEY=… RUST_LOG=info cargo run -- --headless --port 11470
```

### Build requirements

#### Linux (Ubuntu)

```sh
apt install build-essential pkg-config libgtk-3-dev libssl-dev libayatana-appindicator3-dev
cargo install cargo-deb        # for .deb packaging
cargo install cargo-generate-rpm  # for .rpm packaging
```

#### Linux (Fedora)

```sh
dnf install gtk3-devel
```

#### macOS

```sh
npm install -g create-dmg && brew install graphicsmagick imagemagick
```

#### Windows

Install [Inno Setup](https://jrsoftware.org/isdl.php).

### Build

```sh
cargo build --release
```

### Package

#### Linux .deb

```sh
cargo deb
```

#### Linux .rpm

```sh
cargo build --release --features=bundled
strip -s target/release/stremio-service
cargo generate-rpm
```

#### Windows

```sh
cargo build --release --features=bundled
& "C:\Program Files (x86)\Inno Setup 6\ISCC.exe" "setup\StremioService.iss"
```

#### macOS

```sh
cargo run --bin bundle-macos
create-dmg --overwrite target/macos/*.app target/macos
```

## Limitations / known caveats

- **Transcoder cold start.** The first time Torbox transcodes a file, it
  takes ~60 s for the m3u8 to start serving. The gateway pre-warms the
  transcoder during Stremio's `/probe` call so the lag happens *before*
  the user clicks Play, not after.
- **Magnets only.** Stremio addons (Torrentio, Comet, etc.) emit magnet
  links — that's what the gateway resolves. Direct HTTP streams from
  HTTP addons bypass this service entirely.
- **PGS/bitmap subtitles** are dropped by Torbox's transcoding pipeline.
  Text subtitles (SRT/ASS) survive.
- **`/torrents/createtorrent` is rate-limited** to 60 / hr by Torbox.
  In practice this is hit only on long binge sessions with many fresh
  uncached magnets.
- **No local caching.** Everything goes through Torbox's CDN. Bandwidth
  bills accordingly.

## License

GPL-2.0, same as upstream.
