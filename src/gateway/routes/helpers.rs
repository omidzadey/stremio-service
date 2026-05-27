// Copyright (C) 2017-2026 Smart Code OOD 203358507

//! Cross-cutting helpers used by multiple route modules: error mapping,
//! magnet/info-hash parsing, and the "ensure a Torbox mapping exists for
//! this hash" routine that backs both `/create` and `/probe`.

use std::time::{Duration, Instant};

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use log::{debug, info, warn};
use percent_encoding::{utf8_percent_encode, NON_ALPHANUMERIC};
use url::Url;

use crate::gateway::state::{AppState, TorrentMapping};
use crate::torbox::{CreateStreamOptions, StreamData, StreamKind, TorboxError};

/// Maximum time we'll wait for a freshly-added (uncached) torrent to
/// reach `cached`/`completed` before giving up.
pub const TORRENT_READY_TIMEOUT: Duration = Duration::from_secs(120);

/// Poll interval while waiting for the torrent to be ready.
pub const TORRENT_POLL_INTERVAL: Duration = Duration::from_millis(1500);

/// Convert any error into a 502 Bad Gateway with a JSON body. The
/// Stremio client doesn't care about the body shape but we make it a
/// `{ error: string }` for human inspection.
pub fn err_response(status: StatusCode, message: impl Into<String>) -> Response {
    let msg = message.into();
    warn!("gateway -> {status}: {msg}");
    (
        status,
        [(axum::http::header::CONTENT_TYPE, "application/json")],
        serde_json::json!({ "error": msg }).to_string(),
    )
        .into_response()
}

/// Map a [`TorboxError`] to an HTTP status + body. We surface Torbox's
/// own error code so debugging is straightforward.
pub fn torbox_to_response(err: TorboxError) -> Response {
    let status = match &err {
        TorboxError::Http(_) | TorboxError::BadJson(_) => StatusCode::BAD_GATEWAY,
        TorboxError::Api { code, .. } if code == "NO_AUTH" || code == "BAD_TOKEN" => {
            StatusCode::UNAUTHORIZED
        }
        TorboxError::Api { code, .. } if code == "PLAN_RESTRICTED_FEATURE" => {
            StatusCode::PAYMENT_REQUIRED
        }
        TorboxError::Api { code, .. } if code == "ITEM_NOT_FOUND" => StatusCode::NOT_FOUND,
        _ => StatusCode::BAD_GATEWAY,
    };
    err_response(status, err.to_string())
}

/// Try to pull an info-hash out of a magnet URI. Returns the lowercase
/// 40-char hex form on success.
pub fn info_hash_from_magnet(magnet: &str) -> Option<String> {
    let url = Url::parse(magnet).ok()?;
    if url.scheme() != "magnet" {
        return None;
    }
    for (key, value) in url.query_pairs() {
        if key != "xt" {
            continue;
        }
        if let Some(hash) = value.strip_prefix("urn:btih:") {
            return Some(normalize_info_hash(hash));
        }
    }
    None
}

/// Normalize an info-hash to the 40-char lowercase hex form. Accepts both
/// hex and base32 inputs; non-base32 base16 inputs are returned as-is.
pub fn normalize_info_hash(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.len() == 40 && trimmed.chars().all(|c| c.is_ascii_hexdigit()) {
        return trimmed.to_ascii_lowercase();
    }
    if trimmed.len() == 32 {
        // Best-effort base32 decode.
        if let Some(bytes) = base32_decode(trimmed) {
            return hex::encode(bytes);
        }
    }
    trimmed.to_ascii_lowercase()
}

fn base32_decode(input: &str) -> Option<Vec<u8>> {
    const ALPHABET: &[u8; 32] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";
    let upper = input.to_ascii_uppercase();
    let bytes = upper.as_bytes();
    let mut out = Vec::with_capacity(bytes.len() * 5 / 8);
    let mut buf: u32 = 0;
    let mut bits = 0u8;
    for &b in bytes {
        let idx = ALPHABET.iter().position(|&c| c == b)? as u32;
        buf = (buf << 5) | idx;
        bits += 5;
        if bits >= 8 {
            bits -= 8;
            out.push(((buf >> bits) & 0xff) as u8);
        }
    }
    Some(out)
}

/// Build a minimal magnet URI from an info-hash so we can hand it off to
/// `createtorrent` even when Stremio's request body is empty.
pub fn build_magnet(info_hash: &str) -> String {
    format!("magnet:?xt=urn:btih:{}", info_hash.to_ascii_lowercase())
}

/// Try to pull an info-hash + file-idx out of a `mediaURL` Stremio passed
/// us. Stremio constructs the `mediaURL` as `<server>/<infoHash>/<idx>`
/// when it asks for HLS transcoding of a torrent we already registered.
pub fn parse_local_media_url(url: &str) -> Option<(String, i64)> {
    let parsed = Url::parse(url).ok()?;
    let segments: Vec<&str> = parsed.path_segments()?.collect();
    let info_hash = segments.first()?;
    let idx = segments.get(1)?.parse::<i64>().ok()?;
    if info_hash.len() < 32 {
        return None;
    }
    Some((normalize_info_hash(info_hash), idx))
}

/// Percent-encode a value for use as a query parameter.
pub fn url_encode(value: &str) -> String {
    utf8_percent_encode(value, NON_ALPHANUMERIC).to_string()
}

/// Make sure we have a [`TorrentMapping`] for the given info-hash. If we
/// haven't seen it before, look it up on Torbox; if Torbox doesn't have
/// it yet, add it (using the supplied magnet) and wait until it's ready.
///
/// `magnet_hint` is used as the magnet URI passed to `createtorrent`; if
/// `None`, a minimal magnet is constructed from `info_hash`.
pub async fn ensure_mapping(
    state: &AppState,
    info_hash: &str,
    magnet_hint: Option<&str>,
    desired_file_idx: Option<i64>,
) -> Result<TorrentMapping, Response> {
    let normalized = normalize_info_hash(info_hash);
    if let Some(existing) = state.get_mapping(&normalized).await {
        if desired_file_idx
            .map(|idx| idx == existing.file_id)
            .unwrap_or(true)
        {
            return Ok(existing);
        }
    }

    debug!("ensure_mapping: looking up {normalized} on Torbox");
    let mut record = match state.torbox.find_torrent_by_hash(&normalized).await {
        Ok(Some(rec)) => rec,
        Ok(None) => {
            let magnet = magnet_hint
                .map(|m| m.to_owned())
                .unwrap_or_else(|| build_magnet(&normalized));
            info!("Adding magnet {normalized} to Torbox");
            match state.torbox.create_torrent_from_magnet(&magnet).await {
                Ok(created) => {
                    let id = created.id.ok_or_else(|| {
                        err_response(
                            StatusCode::BAD_GATEWAY,
                            "Torbox createtorrent returned no id",
                        )
                    })?;
                    // Initial record may have empty files; refetch.
                    state
                        .torbox
                        .get_torrent(id)
                        .await
                        .map_err(torbox_to_response)?
                }
                Err(err) => return Err(torbox_to_response(err)),
            }
        }
        Err(err) => return Err(torbox_to_response(err)),
    };

    // Wait for ready / non-empty files list.
    let deadline = Instant::now() + TORRENT_READY_TIMEOUT;
    while !record.is_ready() || record.files.is_empty() {
        if Instant::now() >= deadline {
            return Err(err_response(
                StatusCode::GATEWAY_TIMEOUT,
                format!(
                    "Torbox did not finish caching torrent {} within {}s",
                    normalized,
                    TORRENT_READY_TIMEOUT.as_secs()
                ),
            ));
        }
        tokio::time::sleep(TORRENT_POLL_INTERVAL).await;
        record = state
            .torbox
            .get_torrent(record.id)
            .await
            .map_err(torbox_to_response)?;
    }

    let file = if let Some(idx) = desired_file_idx {
        record
            .files
            .iter()
            .find(|f| f.id == idx)
            .or_else(|| record.pick_video_file())
    } else {
        record.pick_video_file()
    }
    .ok_or_else(|| {
        err_response(
            StatusCode::NOT_FOUND,
            "Torbox returned no files for this torrent",
        )
    })?;

    let mapping = TorrentMapping {
        torrent_id: record.id,
        file_id: file.id,
        file_name: file.name.clone(),
        size: file.size,
        last_used_at: Instant::now(),
        last_stream: None,
    };
    state.put_mapping(&normalized, mapping.clone()).await;
    Ok(mapping)
}

/// Convenience to call `createstream` and cache the result on the mapping.
pub async fn create_stream_for(
    state: &AppState,
    info_hash: &str,
    mapping: &TorrentMapping,
    audio_index: Option<u32>,
    subtitle_index: Option<u32>,
    resolution_index: Option<u32>,
) -> Result<StreamData, Response> {
    let opts = CreateStreamOptions {
        id: mapping.torrent_id,
        file_id: mapping.file_id,
        kind: StreamKind::Torrent,
        audio_index,
        subtitle_index,
        resolution_index,
    };
    let data = state
        .torbox
        .create_stream(&opts)
        .await
        .map_err(torbox_to_response)?;
    state.update_stream(info_hash, data.clone()).await;
    Ok(data)
}
