// Copyright (C) 2017-2026 Smart Code OOD 203358507

//! The in-process HTTP gateway that replaces the historical `server.js`
//! blob. We expose the subset of the Stremio streaming-server protocol
//! that the desktop / web client actually calls, and translate each call
//! into one or more Torbox API requests.
//!
//! Architecture overview:
//!
//! ```text
//!   Stremio client              gateway                   Torbox API
//!  --------------------------------------------------------------
//!  GET /settings           -->  static JSON
//!  POST {hash}/create      -->  magnet -> torbox createtorrent
//!  GET {hash}/{idx}        -->  302 to torbox requestdl
//!  GET /probe?mediaURL=    -->  createstream metadata (also warms HLS)
//!  GET /hlsv2/{id}/...m3u8 -->  proxy(torbox createstream.hls_url)
//! ```
//!
//! State lives in [`state::SharedState`]: a small in-memory map keyed by
//! `info_hash` that remembers which Torbox `torrent_id` / `file_id` /
//! `presigned_token` we used for a given hash. This avoids hitting the
//! 60/hour `createtorrent` rate limit on every play.

pub mod routes;
pub mod state;

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;

use anyhow::{Context, Error};
use axum::http::{header, HeaderName, Method};
use axum::Router;
use log::info;
use tower_http::cors::{AllowOrigin, CorsLayer};
use tower_http::trace::TraceLayer;

use crate::config::GatewayConfig;
use crate::torbox::TorboxClient;

use self::state::AppState;

/// Bring the HTTP gateway up on the configured bind+port. Runs until the
/// underlying tokio task is dropped — typically the lifetime of the
/// `Application`.
pub async fn serve(cfg: GatewayConfig) -> Result<(), Error> {
    let bind: IpAddr = cfg.bind;
    let port = cfg.port;
    let addr = SocketAddr::new(bind, port);

    let torbox = TorboxClient::new(cfg.torbox_api_key.clone());
    let state = Arc::new(AppState::new(torbox, cfg.clone()));

    let cors = build_cors(&cfg);

    let app = Router::new()
        .merge(routes::meta::router())
        .merge(routes::stream::router())
        .merge(routes::hls::router())
        .with_state(state)
        .layer(cors)
        .layer(TraceLayer::new_for_http());

    info!("Gateway listening on http://{addr}");

    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .with_context(|| format!("failed to bind {addr}"))?;
    axum::serve(listener, app)
        .await
        .context("gateway HTTP server exited unexpectedly")?;

    Ok(())
}

fn build_cors(cfg: &GatewayConfig) -> CorsLayer {
    // Same allowlist as the original server.js plus any operator extras.
    let mut origins: Vec<String> = vec![
        "https://web.stremio.com".into(),
        "https://app.strem.io".into(),
        "https://www.strem.io".into(),
        "https://strem.io".into(),
        "https://www.stremio.com".into(),
        "https://stremio.com".into(),
        "https://www.stremio.net".into(),
        "https://stremio.net".into(),
        "https://stremio.github.io".into(),
        "https://stremio-development.netlify.app".into(),
    ];
    origins.extend(cfg.extra_allowed_origins.iter().cloned());

    let allow = AllowOrigin::predicate(move |origin, _req| {
        let Ok(origin_str) = origin.to_str() else {
            return false;
        };
        if origins.iter().any(|o| o == origin_str) {
            return true;
        }
        // Localhost (any port) is always allowed — Stremio desktop opens
        // file:// or arbitrary localhost ports.
        origin_str.starts_with("http://127.0.0.1:")
            || origin_str.starts_with("http://localhost:")
            || origin_str.starts_with("https://127.0.0.1:")
            || origin_str.starts_with("https://localhost:")
            || origin_str == "null"
    });

    CorsLayer::new()
        .allow_origin(allow)
        .allow_methods([
            Method::GET,
            Method::POST,
            Method::PUT,
            Method::DELETE,
            Method::OPTIONS,
            Method::HEAD,
        ])
        .allow_headers([
            header::CONTENT_TYPE,
            header::AUTHORIZATION,
            header::RANGE,
            header::ACCEPT,
            header::CACHE_CONTROL,
            HeaderName::from_static("x-requested-with"),
        ])
        .expose_headers([
            header::CONTENT_LENGTH,
            header::CONTENT_RANGE,
            header::CONTENT_TYPE,
        ])
        .allow_credentials(false)
}
