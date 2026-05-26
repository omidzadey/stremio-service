// Copyright (C) 2017-2026 Smart Code OOD 203358507

//! Torrent lifecycle routes.
//!
//! Stremio's client model is "one torrent per info-hash, one file per
//! play". It pokes a few endpoints in sequence:
//!
//! 1. `POST /{infoHash}/create` with a JSON body containing either a
//!    `torrent` (magnet) or `infoHash`. We persist the mapping to Torbox.
//! 2. `<video src="http://{server}/{infoHash}/{fileIdx}">` — we 302 to the
//!    Torbox direct-download URL, which supports `Range` so seeking works.
//! 3. `GET /{infoHash}/stats.json` (and the per-file variant) is polled by
//!    the "downloading…" UI; we return Torbox's progress instead.
//! 4. `GET /{infoHash}/remove` is best-effort cleanup; for Torbox we leave
//!    the user's account untouched and just drop our local mapping.

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use log::{debug, info};
use serde::Deserialize;
use serde_json::json;

use crate::gateway::routes::helpers::{
    build_magnet, ensure_mapping, err_response, info_hash_from_magnet, normalize_info_hash,
    torbox_to_response,
};
use crate::gateway::state::SharedState;

pub fn router() -> Router<SharedState> {
    Router::new()
        .route("/create", post(create_torrent_top))
        .route("/{info_hash}/create", post(create_torrent))
        .route("/{info_hash}/stats.json", get(torrent_stats))
        .route("/{info_hash}/remove", get(remove_torrent))
        .route("/{info_hash}/{file_idx}", get(stream_file))
        .route("/{info_hash}/{file_idx}/stats.json", get(file_stats))
}

#[derive(Debug, Deserialize, Default)]
#[serde(default)]
pub struct CreatePayload {
    /// Magnet URI or torrent: link.
    pub torrent: Option<String>,
    /// Alternate field name some Stremio versions use.
    pub magnet: Option<String>,
    /// Some clients send `infoHash` directly instead of a full magnet.
    #[serde(alias = "info_hash")]
    pub info_hash: Option<String>,
}

/// Variant that accepts the info-hash via URL path.
async fn create_torrent(
    State(state): State<SharedState>,
    Path(info_hash): Path<String>,
    raw_body: Bytes,
) -> Response {
    let payload = parse_create_payload(&raw_body);
    handle_create(state, Some(info_hash), payload).await
}

/// Variant where the info-hash isn't in the path. Stremio's older
/// "torrent server" flow used this shape.
async fn create_torrent_top(State(state): State<SharedState>, raw_body: Bytes) -> Response {
    let payload = parse_create_payload(&raw_body);
    handle_create(state, None, payload).await
}

fn parse_create_payload(raw: &Bytes) -> CreatePayload {
    if raw.is_empty() {
        return CreatePayload::default();
    }
    if let Ok(payload) = serde_json::from_slice::<CreatePayload>(raw) {
        return payload;
    }
    // Some Stremio variants POST a bare magnet string.
    if let Ok(s) = std::str::from_utf8(raw) {
        let trimmed = s.trim();
        if trimmed.starts_with("magnet:") {
            return CreatePayload {
                torrent: Some(trimmed.to_owned()),
                ..Default::default()
            };
        }
    }
    CreatePayload::default()
}

async fn handle_create(
    state: SharedState,
    path_info_hash: Option<String>,
    payload: CreatePayload,
) -> Response {
    let magnet = payload
        .torrent
        .clone()
        .or(payload.magnet.clone())
        .filter(|m| !m.is_empty());

    let info_hash = path_info_hash
        .filter(|h| !h.is_empty())
        .or_else(|| magnet.as_deref().and_then(info_hash_from_magnet))
        .or_else(|| payload.info_hash.clone())
        .map(|h| normalize_info_hash(&h));

    let Some(info_hash) = info_hash else {
        return err_response(
            StatusCode::BAD_REQUEST,
            "create: missing info_hash (provide a magnet URI in `torrent`/`magnet` or pass it via URL)",
        );
    };

    let magnet = magnet.unwrap_or_else(|| build_magnet(&info_hash));
    let mapping = match ensure_mapping(&state, &info_hash, Some(&magnet), None).await {
        Ok(m) => m,
        Err(resp) => return resp,
    };

    info!(
        "create: registered {} -> torbox_id={} file_id={}",
        info_hash, mapping.torrent_id, mapping.file_id
    );

    Json(json!({
        "ok": true,
        "infoHash": info_hash,
        "fileIdx": mapping.file_id,
        "fileName": mapping.file_name,
        "size": mapping.size,
        "torbox_id": mapping.torrent_id,
    }))
    .into_response()
}

/// Top-level torrent stats. We answer "ready and seeded" once we have a
/// Torbox mapping, which lines up with the Torbox cached/completed state.
async fn torrent_stats(
    State(state): State<SharedState>,
    Path(info_hash): Path<String>,
) -> Response {
    let normalized = normalize_info_hash(&info_hash);
    match state.get_mapping(&normalized).await {
        Some(mapping) => Json(json!({
            "infoHash": normalized,
            "name": mapping.file_name,
            "downloaded": mapping.size,
            "uploaded": 0,
            "downloadSpeed": 0,
            "uploadSpeed": 0,
            "peers": 0,
            "seeds": 0,
            "progress": 1.0,
            "ready": true,
        }))
        .into_response(),
        None => err_response(StatusCode::NOT_FOUND, "torrent not registered"),
    }
}

async fn file_stats(
    State(state): State<SharedState>,
    Path((info_hash, file_idx)): Path<(String, i64)>,
) -> Response {
    let normalized = normalize_info_hash(&info_hash);
    match state.get_mapping(&normalized).await {
        Some(mapping) => Json(json!({
            "infoHash": normalized,
            "fileIdx": file_idx,
            "name": mapping.file_name,
            "size": mapping.size,
            "downloaded": mapping.size,
            "progress": 1.0,
            "ready": true,
        }))
        .into_response(),
        None => err_response(StatusCode::NOT_FOUND, "torrent not registered"),
    }
}

/// "Remove from cache" for the local engine. Torbox handles its own
/// retention policy, so we just drop the local mapping.
async fn remove_torrent(
    State(state): State<SharedState>,
    Path(info_hash): Path<String>,
) -> Response {
    let normalized = normalize_info_hash(&info_hash);
    let mut map = state.mappings.write().await;
    map.remove(&normalized);
    drop(map);
    Json(json!({ "ok": true })).into_response()
}

/// The direct-play endpoint. We resolve the Torbox direct-download URL on
/// demand and 302-redirect the player to it. The Torbox CDN supports HTTP
/// Range, so seeking works without us touching the bytes.
async fn stream_file(
    State(state): State<SharedState>,
    Path((info_hash, file_idx)): Path<(String, i64)>,
) -> Response {
    let normalized = normalize_info_hash(&info_hash);
    let mapping = match ensure_mapping(&state, &normalized, None, Some(file_idx)).await {
        Ok(m) => m,
        Err(resp) => return resp,
    };

    debug!(
        "stream_file: requesting Torbox download URL for torrent_id={} file_id={}",
        mapping.torrent_id, mapping.file_id
    );

    let url = match state
        .torbox
        .request_download_link(mapping.torrent_id, mapping.file_id)
        .await
    {
        Ok(u) => u,
        Err(err) => return torbox_to_response(err),
    };

    let mut response = Redirect::temporary(&url).into_response();
    // Hint to the player that this URL is a video; some players (notably
    // mpv) re-detect via content-type after the redirect.
    if let Some(name) = mapping.file_name.as_deref() {
        if let Ok(hv) = header::HeaderValue::from_str(name) {
            response
                .headers_mut()
                .insert(header::HeaderName::from_static("x-original-filename"), hv);
        }
    }
    response
}
