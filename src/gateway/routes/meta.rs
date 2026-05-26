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
        .route("/casting", get(casting))
        .route("/probe", get(probe))
        // stremio-video calls `/hlsv2/probe` — it's the official endpoint
        // used by `withStreamingServer.canPlayStream`. Same logic, same shape.
        .route("/hlsv2/probe", get(probe))
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
    State(_state): State<SharedState>,
    Json(_body): Json<serde_json::Value>,
) -> Json<Value> {
    // We don't actually allow Stremio to mutate any of our settings — most
    // of them don't apply when the upstream is Torbox. Acknowledge the
    // request with the shape `stremio-core` expects: it calls
    // `E::fetch::<SuccessResponse>` on this POST (see
    // `stremio_core::models::streaming_server::set_settings`), where
    // `SuccessResponse = { success: True }`. Returning anything else
    // (e.g. the full settings document) fails deserialization and flips
    // every Loadable<…> on the StreamingServer model to Err — which
    // visibly hides the Cache size / Torrent profile / Transcode profile
    // rows in stremio-web and shows "Error" next to the URL.
    Json(json!({ "success": true }))
}

fn settings_json(state: &Arc<crate::gateway::state::AppState>) -> Value {
    // The shape here must match `stremio_core::types::streaming_server::SettingsResponse`:
    //   { "baseUrl": Url, "values": Settings }
    // with `Settings` using camelCase field names. Anything else makes
    // stremio-core fail to deserialize and mark the streaming server as
    // errored, even though we return 200 OK.
    let base_url = state
        .cfg
        .public_base_url
        .clone()
        .unwrap_or_else(|| format!("http://{}:{}", state.cfg.bind, state.cfg.port));
    json!({
        "baseUrl": base_url,
        "values": {
            "serverVersion": "4.20.17",
            "appPath": "/dev/null",
            "cacheRoot": "/dev/null",
            "cacheSize": null,
            "btMaxConnections": 0,
            "btHandshakeTimeout": 0,
            "btRequestTimeout": 0,
            "btDownloadSpeedSoftLimit": 0,
            "btDownloadSpeedHardLimit": 0,
            "btMinPeersForStable": 0,
            "remoteHttps": "",
            "proxyStreamsEnabled": false,
            "transcodeProfile": null,
            "transcodingEnabled": true,
            "forceTranscoding": state.cfg.force_transcode,
            "torboxBacked": true,
        },
        "options": [
            {
                "id": "transcodeProfile",
                "label": "TRANSCODE_PROFILE",
                "type": "select",
                "selections": [
                    { "name": "Disabled", "val": null },
                    { "name": "Torbox (HLS)", "val": "torbox" }
                ]
            }
        ]
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
    // Shape must match `stremio_core::types::streaming_server::NetworkInfo`
    // (camelCase). Stremio uses this to populate the "Remote HTTPS" picker;
    // we truthfully say there are no LAN interfaces to advertise because
    // Torbox lives on the public internet.
    Json(json!({
        "availableInterfaces": []
    }))
}

async fn device_info() -> Json<Value> {
    // Shape must match `stremio_core::types::streaming_server::DeviceInfo`
    // (camelCase).
    //
    // `availableHardwareAccelerations` is what populates the
    // "Transcode profile" dropdown in stremio-web (see
    // `useStreamingOptions.ts`: each element becomes a dropdown entry).
    // Advertising `"torbox"` lets the user pick a non-Disabled profile so
    // stremio-video knows transcoding is available. The string is opaque to
    // the client — it's stored as the `transcodeProfile` value and is only
    // surfaced back to us in `POST /settings`, which we ignore.
    Json(json!({
        "availableHardwareAccelerations": ["torbox"]
    }))
}

/// stremio-core fetches `/casting` to populate the list of playback devices
/// (e.g. Chromecast endpoints) that the streaming server can push to. The
/// gateway has no LAN devices to advertise, so we return an empty array —
/// the response shape required is `Vec<PlaybackDevice>` in JSON. If this
/// returns 404, stremio-core marks the whole streaming server as errored.
async fn casting() -> Json<Value> {
    Json(json!([]))
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
                    let mut obj = json!({
                        "track": "audio",
                        "codec": a.codec,
                        "channels": a.channels,
                        "language": a.language,
                        "title": a.title,
                    });
                    if let Some(idx) = a.index {
                        obj["index"] = json!(idx);
                    }
                    obj
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
                    let mut obj = json!({
                        "track": "subtitle",
                        "codec": s.codec,
                        "language": s.language,
                        "title": s.title,
                    });
                    if let Some(idx) = s.index {
                        obj["index"] = json!(idx);
                    }
                    obj
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    let container_name = container_from_filename(mapping.file_name.as_deref());

    // Shape required by stremio-video's `canPlayStream`:
    //   { format: { name }, streams: [{ track, codec, channels?, ... }] }
    //
    // The probe response is used by stremio-video to decide between
    // direct-play and `/hlsv2/{id}/master.m3u8`. The fields it actually
    // looks at are: `format.name` (substring-matched against supported
    // container formats), and per-stream `track` + `codec` + `channels`.
    // Anything else is informational. We keep `torbox` metadata as an
    // extra debugging payload.
    let mut streams: Vec<Value> = Vec::new();
    if let Some(v) = video {
        streams.push(json!({
            "track": "video",
            "codec": v.codec,
            "width": v.width,
            "height": v.height,
            "pixelFormat": v.pixel_format,
            "frameRate": v.frame_rate,
            "bitrate": v.bitrate,
            "duration": v.duration,
        }));
    }
    streams.extend(audios);
    streams.extend(subtitles);

    Json(json!({
        "format": { "name": container_name },
        "streams": streams,
        "torbox": {
            "needs_transcoding": data.needs_transcoding,
            "presigned_token": data.presigned_token,
            "open_subtitles_hash": data.open_subtitles_hash,
            "intro_information": data.intro_information,
            "size": data.size,
        },
    }))
    .into_response()
}

/// Map a filename extension to the FFmpeg-style container name string
/// Stremio looks for. Stremio does substring matching against this
/// (`probe.format.name.indexOf(format) !== -1`), so we use the same comma-
/// separated grouping FFmpeg's `format_name` uses for common containers.
fn container_from_filename(name: Option<&str>) -> &'static str {
    let name = name.unwrap_or("").to_ascii_lowercase();
    let ext = name.rsplit('.').next().unwrap_or("");
    match ext {
        "mkv" | "webm" => "matroska,webm",
        "mp4" | "m4v" | "m4a" | "mov" | "3gp" => "mov,mp4,m4a,3gp,3g2,mj2",
        "avi" => "avi",
        "ts" | "m2ts" | "mts" => "mpegts",
        "flv" => "flv",
        "wmv" | "asf" => "asf",
        // When we don't recognize the extension, advertise the widest
        // possible match so direct-play remains an option for browsers
        // that can actually consume the content-type.
        _ => "mov,mp4,m4a,3gp,3g2,mj2,matroska,webm",
    }
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
