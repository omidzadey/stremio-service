// Copyright (C) 2017-2026 Smart Code OOD 203358507

//! "Boring" endpoints: server identification, probe, opensub hash, etc.
//!
//! These are what the Stremio client hits to decide whether a streaming
//! server is healthy and what it can play. We don't need to be a perfect
//! `server.js` impersonator — Stremio's checks are pretty lenient — but
//! we do need to return the right top-level shapes.

use std::sync::Arc;

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use log::info;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::gateway::routes::helpers::{
    create_stream_for, ensure_mapping, err_response, info_hash_from_magnet, parse_local_media_url,
};
use crate::gateway::state::SharedState;

pub fn router() -> Router<SharedState> {
    Router::new()
        .route("/settings", get(settings_get).post(settings_post))
        .route("/status", get(status))
        .route("/heartbeat", get(heartbeat))
        .route("/network-info", get(network_info))
        .route("/device-info", get(device_info))
        .route("/probe", get(probe))
        .route("/opensubHash", get(open_sub_hash))
        .route("/", get(root))
}

async fn root() -> Json<Value> {
    Json(json!({
        "name": "stremio-service (torbox gateway)",
        "version": env!("CARGO_PKG_VERSION"),
        "ok": true,
    }))
}

/// Stremio polls `/settings` to validate the streaming-server URL. We
/// return a static-but-realistic settings document. The `serverVersion`
/// has to live in the `4.x` range or some older client builds spit out a
/// warning; we pretend to be the same version `server.js` would advertise.
async fn settings_get(State(state): State<SharedState>) -> Json<Value> {
    Json(settings_json(&state))
}

async fn settings_post(
    State(state): State<SharedState>,
    Json(_body): Json<serde_json::Value>,
) -> Json<Value> {
    // We don't actually allow Stremio to mutate any of our settings — most
    // of them don't apply when the upstream is Torbox. Acknowledge the
    // request so the UI doesn't error and echo our current values back.
    Json(settings_json(&state))
}

fn settings_json(state: &Arc<crate::gateway::state::AppState>) -> Value {
    json!({
        "values": {
            "server_version": "4.20.17",
            "app_path": "/dev/null",
            "cache_root": "/dev/null",
            "cache_size": 0,
            "bt_max_connections": 0,
            "bt_handshake_timeout": 0,
            "bt_request_timeout": 0,
            "bt_download_speed_soft_limit": 0,
            "bt_download_speed_hard_limit": 0,
            "bt_min_peers_for_stable": 0,
            "remote_https": null,
            "proxy_streams_enabled": false,
            "transcoding_enabled": true,
            "force_transcoding": state.cfg.force_transcode,
            "torbox_backed": true,
        },
        "options": {
            "transcode_profile": {
                "supports_hevc": false,
                "supports_av1": false,
                "max_audio_channels": 8,
                "supports_ac3": true,
                "supports_eac3": false,
                "supports_truehd": false
            }
        },
        "version": "4.20.17",
        "server_version": "4.20.17",
        "appVersion": "4.20.17",
        "transcoding_enabled": true,
    })
}

async fn status() -> Json<Value> {
    Json(json!({
        "ok": true,
        "version": env!("CARGO_PKG_VERSION"),
        "uptime": uptime_seconds(),
        "transcoding": "torbox",
    }))
}

async fn heartbeat() -> impl IntoResponse {
    (StatusCode::OK, "ok")
}

async fn network_info() -> Json<Value> {
    // Stremio looks up a few fields here to render the "Casting" UI. We
    // truthfully say "no LAN-served streams" because Torbox lives on the
    // public internet.
    Json(json!({
        "available_interfaces": [],
        "ip": "0.0.0.0",
        "hostname": hostname(),
        "platform": std::env::consts::OS,
        "arch": std::env::consts::ARCH,
        "transcoding_backend": "torbox",
    }))
}

async fn device_info() -> Json<Value> {
    Json(json!({
        "platform": std::env::consts::OS,
        "arch": std::env::consts::ARCH,
        "gpu_supported": false,
        "hwaccel": [],
        "ffprobe": null,
        "ffmpeg": null,
    }))
}

#[derive(Debug, Deserialize)]
pub struct ProbeQuery {
    /// The URL Stremio wants probed. When it's a local URL of the form
    /// `<server>/<infoHash>/<idx>` we translate it to Torbox metadata
    /// directly; otherwise we surface a hint that this gateway only
    /// supports Torbox-backed sources.
    #[serde(rename = "mediaURL")]
    pub media_url: Option<String>,
}

/// The Stremio client calls `/probe` to decide whether the source file
/// can play directly or needs transcoding. We translate Torbox's
/// `createstream` metadata into an ffprobe-shaped JSON document the client
/// already knows how to consume. As a side effect we kick off the Torbox
/// transcoder so the player isn't waiting for the cold start later.
async fn probe(State(state): State<SharedState>, Query(q): Query<ProbeQuery>) -> Response {
    let Some(media_url) = q.media_url.filter(|s| !s.is_empty()) else {
        return err_response(StatusCode::BAD_REQUEST, "missing mediaURL");
    };

    // The most common shape: `mediaURL=http://localhost:11470/<hash>/<idx>`.
    let (info_hash, file_idx) = match parse_local_media_url(&media_url) {
        Some(p) => p,
        None => match info_hash_from_magnet(&media_url) {
            Some(h) => (h, 0),
            None => {
                return err_response(
                    StatusCode::BAD_REQUEST,
                    format!(
                        "Could not derive a Torbox source from mediaURL={media_url}. \
                         This gateway only transcodes torrents added through itself."
                    ),
                );
            }
        },
    };

    let mapping = match ensure_mapping(&state, &info_hash, None, Some(file_idx)).await {
        Ok(m) => m,
        Err(resp) => return resp,
    };

    let data = match create_stream_for(&state, &info_hash, &mapping, None, None, None).await {
        Ok(d) => d,
        Err(resp) => return resp,
    };

    info!(
        "/probe {} -> torbox_id={} file_id={} needs_transcoding={:?}",
        info_hash, mapping.torrent_id, mapping.file_id, data.needs_transcoding
    );

    let video = data.metadata.as_ref().and_then(|m| m.video.as_ref());
    let audios = data
        .metadata
        .as_ref()
        .map(|m| {
            m.audios
                .iter()
                .map(|a| {
                    json!({
                        "index": a.index,
                        "codec_type": "audio",
                        "codec_name": a.codec,
                        "channels": a.channels,
                        "channel_layout": null,
                        "language": a.language,
                        "title": a.title,
                        "disposition": { "default": a.default.unwrap_or(false) as u8 },
                    })
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let subtitles = data
        .metadata
        .as_ref()
        .map(|m| {
            m.subtitles
                .iter()
                .map(|s| {
                    json!({
                        "index": s.index,
                        "codec_type": "subtitle",
                        "codec_name": s.codec,
                        "language": s.language,
                        "title": s.title,
                        "disposition": { "default": s.default.unwrap_or(false) as u8 },
                    })
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    let duration: Option<f64> =
        video.and_then(|v| v.duration.as_ref().and_then(|d| parse_duration(d)));

    let format = json!({
        "format_name": "mov,mp4,m4a,3gp,3g2,mj2,matroska,webm",
        "duration": duration.map(|d| d.to_string()),
        "size": data.size,
        "bit_rate": video.and_then(|v| v.bitrate.clone()),
    });

    let mut streams: Vec<Value> = Vec::new();
    if let Some(v) = video {
        streams.push(json!({
            "index": 0,
            "codec_type": "video",
            "codec_name": v.codec,
            "width": v.width,
            "height": v.height,
            "pix_fmt": v.pixel_format,
            "r_frame_rate": v.frame_rate,
            "duration": v.duration,
            "bit_rate": v.bitrate,
        }));
    }
    streams.extend(audios);
    streams.extend(subtitles);

    Json(json!({
        "format": format,
        "streams": streams,
        "torbox": {
            "needs_transcoding": data.needs_transcoding,
            "presigned_token": data.presigned_token,
            "open_subtitles_hash": data.open_subtitles_hash,
            "intro_information": data.intro_information,
        },
    }))
    .into_response()
}

#[derive(Debug, Deserialize)]
pub struct OpenSubHashQuery {
    /// The URL Stremio wants the hash for; typically the same shape as
    /// `/probe`'s `mediaURL`.
    #[serde(rename = "videoUrl", alias = "url", alias = "mediaURL")]
    pub video_url: Option<String>,
}

/// Stremio asks for an OpenSubtitles 64-bit hash so it can fetch matching
/// subtitle files. Torbox already computes this during `createstream`
/// (`open_subtitles_hash`), so we reuse that.
async fn open_sub_hash(
    State(state): State<SharedState>,
    Query(q): Query<OpenSubHashQuery>,
) -> Response {
    let Some(url) = q.video_url else {
        return err_response(StatusCode::BAD_REQUEST, "missing videoUrl");
    };
    let Some((info_hash, file_idx)) = parse_local_media_url(&url) else {
        return err_response(
            StatusCode::BAD_REQUEST,
            "videoUrl must point back at this server",
        );
    };
    let mapping = match ensure_mapping(&state, &info_hash, None, Some(file_idx)).await {
        Ok(m) => m,
        Err(resp) => return resp,
    };
    let data = match create_stream_for(&state, &info_hash, &mapping, None, None, None).await {
        Ok(d) => d,
        Err(resp) => return resp,
    };
    let hash = data.open_subtitles_hash.unwrap_or_default();
    Json(json!({ "result": { "hash": hash } })).into_response()
}

fn uptime_seconds() -> u64 {
    static START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
    let start = START.get_or_init(std::time::Instant::now);
    start.elapsed().as_secs()
}

fn hostname() -> String {
    std::env::var("HOSTNAME").unwrap_or_else(|_| "stremio-service".into())
}

/// Torbox returns durations in `HH:MM:SS.ffffff`. Convert to seconds.
fn parse_duration(s: &str) -> Option<f64> {
    let parts: Vec<&str> = s.split(':').collect();
    match parts.as_slice() {
        [h, m, sec] => {
            let h: f64 = h.parse().ok()?;
            let m: f64 = m.parse().ok()?;
            let sec: f64 = sec.parse().ok()?;
            Some(h * 3600.0 + m * 60.0 + sec)
        }
        [m, sec] => {
            let m: f64 = m.parse().ok()?;
            let sec: f64 = sec.parse().ok()?;
            Some(m * 60.0 + sec)
        }
        [sec] => sec.parse().ok(),
        _ => None,
    }
}
