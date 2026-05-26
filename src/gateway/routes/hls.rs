// Copyright (C) 2017-2026 Smart Code OOD 203358507

//! HLS gateway routes.
//!
//! Stremio constructs URLs like:
//!
//! ```text
//! /hlsv2/{convertId}/master.m3u8?mediaURL=<src>&video=…&audio=…&subtitle=…
//! /hlsv2/{convertId}/stream-q-0/0.ts
//! ```
//!
//! `convertId` is opaque to us (the original server.js used it to
//! identify a transcoding job). The query string carries everything we
//! actually need to resolve a stream on Torbox:
//!
//! - `mediaURL` — usually `<host>/<infoHash>/<fileIdx>`, occasionally a
//!   magnet URI.
//! - `audio` / `videoCodecs` / `containerCodec` / `subtitle` — selectors
//!   used to pick which Torbox audio/sub/resolution variant to bake into
//!   the HLS output.
//!
//! Strategy:
//! - **Master / media playlist** requests proxy through Torbox: we fetch
//!   the upstream m3u8, rewrite any token-bearing segment URLs to a
//!   token-stripped variant served from this gateway, then return the
//!   resulting playlist as `application/vnd.apple.mpegurl`. The proxy
//!   ensures the user's Torbox API token never reaches the browser.
//! - **Segment requests** look up the mapping by `convertId` and 302 to
//!   the Torbox CDN URL. Segments don't carry the token in their path so
//!   we don't need to proxy bytes.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::get;
use axum::Router;
use log::{debug, warn};
use once_cell::sync::Lazy;
use serde::Deserialize;
use tokio::sync::RwLock;
use url::Url;

use crate::gateway::routes::helpers::{
    create_stream_for, ensure_mapping, err_response, info_hash_from_magnet, parse_local_media_url,
    url_encode,
};
use crate::gateway::state::SharedState;
use crate::torbox::StreamData;

pub fn router() -> Router<SharedState> {
    Router::new()
        .route("/hlsv2/{convert_id}/{filename}", get(playlist_or_segment))
        .route(
            "/hlsv2/{convert_id}/stream-q-{quality}/{seg}",
            get(segment_by_quality),
        )
        .route(
            "/hlsv2/{convert_id}/stream-{stream}/{seg}",
            get(segment_by_stream),
        )
}

#[derive(Debug, Deserialize, Default)]
#[serde(default)]
pub struct HlsQuery {
    #[serde(rename = "mediaURL")]
    pub media_url: Option<String>,
    pub audio: Option<String>,
    pub subtitle: Option<String>,
    pub video: Option<String>,
    #[serde(rename = "videoCodecs")]
    pub video_codecs: Option<String>,
    #[serde(rename = "audioCodecs")]
    pub audio_codecs: Option<String>,
    #[serde(rename = "containerCodec")]
    pub container_codec: Option<String>,
    pub duration: Option<f64>,
    /// Some clients send numeric audio_index/subtitle_index directly.
    #[serde(rename = "audioIndex", alias = "audio_index")]
    pub audio_index: Option<u32>,
    #[serde(rename = "subtitleIndex", alias = "subtitle_index")]
    pub subtitle_index: Option<u32>,
}

/// Memory cache of "this `convertId` resolves to that Torbox stream". We
/// can't recompute it from query string alone because segment requests
/// don't carry the query, so we cache after the playlist call.
#[derive(Debug, Clone)]
struct ConvertEntry {
    info_hash: String,
    stream: StreamData,
    last_used: Instant,
}

static CONVERT_CACHE: Lazy<RwLock<HashMap<String, ConvertEntry>>> =
    Lazy::new(|| RwLock::new(HashMap::new()));

const CONVERT_TTL: Duration = Duration::from_secs(60 * 60);

async fn put_convert(convert_id: &str, info_hash: String, stream: StreamData) {
    let mut cache = CONVERT_CACHE.write().await;
    // GC stale entries.
    let now = Instant::now();
    cache.retain(|_, v| now.duration_since(v.last_used) < CONVERT_TTL);
    cache.insert(
        convert_id.to_owned(),
        ConvertEntry {
            info_hash,
            stream,
            last_used: now,
        },
    );
}

async fn get_convert(convert_id: &str) -> Option<ConvertEntry> {
    let mut cache = CONVERT_CACHE.write().await;
    if let Some(entry) = cache.get_mut(convert_id) {
        entry.last_used = Instant::now();
        return Some(entry.clone());
    }
    None
}

/// Either a master/media playlist, a `<root>/<file>.m3u8` request, or
/// `<root>/<file>.ts`. We dispatch by suffix.
async fn playlist_or_segment(
    State(state): State<SharedState>,
    Path((convert_id, filename)): Path<(String, String)>,
    Query(query): Query<HlsQuery>,
) -> Response {
    if filename.ends_with(".m3u8") {
        playlist(state, convert_id, filename, query).await
    } else {
        segment(state, convert_id, filename).await
    }
}

async fn segment_by_quality(
    State(state): State<SharedState>,
    Path((convert_id, _quality, seg)): Path<(String, String, String)>,
) -> Response {
    segment(state, convert_id, seg).await
}

async fn segment_by_stream(
    State(state): State<SharedState>,
    Path((convert_id, _stream, seg)): Path<(String, String, String)>,
) -> Response {
    segment(state, convert_id, seg).await
}

async fn playlist(
    state: SharedState,
    convert_id: String,
    _filename: String,
    query: HlsQuery,
) -> Response {
    let Some(media_url) = query.media_url.clone().filter(|s| !s.is_empty()) else {
        return err_response(StatusCode::BAD_REQUEST, "missing mediaURL");
    };

    let (info_hash, file_idx) = match parse_local_media_url(&media_url) {
        Some(p) => p,
        None => match info_hash_from_magnet(&media_url) {
            Some(h) => (h, 0),
            None => {
                return err_response(
                    StatusCode::BAD_REQUEST,
                    format!("Could not derive a Torbox source from mediaURL={media_url}"),
                );
            }
        },
    };

    let mapping = match ensure_mapping(&state, &info_hash, None, Some(file_idx)).await {
        Ok(m) => m,
        Err(resp) => return resp,
    };

    let audio_idx = query
        .audio_index
        .or_else(|| query.audio.as_ref().and_then(|s| s.parse().ok()));
    let sub_idx = query
        .subtitle_index
        .or_else(|| query.subtitle.as_ref().and_then(|s| s.parse().ok()));

    let data = match create_stream_for(&state, &info_hash, &mapping, audio_idx, sub_idx, None).await
    {
        Ok(d) => d,
        Err(resp) => return resp,
    };

    let Some(hls_url) = data.hls_url.clone() else {
        return err_response(
            StatusCode::BAD_GATEWAY,
            "Torbox createstream returned no hls_url",
        );
    };

    put_convert(&convert_id, info_hash.clone(), data.clone()).await;

    // Fetch the upstream playlist (with retries to ride out the cold start).
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(70))
        .build()
        .expect("reqwest builds");

    let body = match fetch_playlist_with_warmup(&client, &hls_url).await {
        Ok(body) => body,
        Err(err) => return err_response(StatusCode::BAD_GATEWAY, err),
    };

    let rewritten = rewrite_playlist(
        &body,
        &hls_url,
        &convert_id,
        state.cfg.public_base_url.as_deref(),
    );

    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/vnd.apple.mpegurl")
        .header(header::CACHE_CONTROL, "no-store")
        .body(Body::from(rewritten))
        .unwrap_or_else(|err| {
            err_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("failed to build playlist response: {err}"),
            )
        })
}

async fn segment(state: SharedState, convert_id: String, segment_name: String) -> Response {
    let Some(entry) = get_convert(&convert_id).await else {
        return err_response(
            StatusCode::NOT_FOUND,
            "Unknown convertId — playlist must be requested first",
        );
    };

    let Some(hls_url) = entry.stream.hls_url.as_ref() else {
        return err_response(StatusCode::BAD_GATEWAY, "Torbox returned no hls_url");
    };

    let segment_url = match join_segment_url(hls_url, &segment_name) {
        Ok(u) => u,
        Err(err) => return err_response(StatusCode::BAD_GATEWAY, err),
    };

    debug!(
        "hls segment redirect: convert={} hash={} seg={} -> {}",
        convert_id, entry.info_hash, segment_name, segment_url
    );

    // Strip the token query if it's there — segment URLs are typically
    // token-free in Torbox, but be defensive.
    let stripped = strip_token(&segment_url);
    let _ = state; // appease "unused" lint; we may use it for analytics later.

    Redirect::temporary(&stripped).into_response()
}

async fn fetch_playlist_with_warmup(
    client: &reqwest::Client,
    hls_url: &str,
) -> Result<String, String> {
    let mut attempts = 0;
    loop {
        let resp = client
            .get(hls_url)
            .send()
            .await
            .map_err(|e| format!("playlist fetch failed: {e}"))?;
        let status = resp.status();
        if status.is_success() {
            return resp
                .text()
                .await
                .map_err(|e| format!("playlist body read failed: {e}"));
        }
        if (status == StatusCode::GATEWAY_TIMEOUT || status == StatusCode::SERVICE_UNAVAILABLE)
            && attempts < 12
        {
            attempts += 1;
            warn!(
                "playlist not ready (status={status}, attempt={attempts}); sleeping before retry"
            );
            tokio::time::sleep(Duration::from_secs(5)).await;
            continue;
        }
        return Err(format!(
            "playlist fetch returned non-success status {status} after {attempts} retries"
        ));
    }
}

fn rewrite_playlist(
    body: &str,
    upstream_url: &str,
    convert_id: &str,
    public_base: Option<&str>,
) -> String {
    let mut out = String::with_capacity(body.len());
    let prefix = build_segment_prefix(convert_id, public_base);

    for line in body.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with('#') || trimmed.is_empty() {
            out.push_str(line);
            out.push('\n');
            continue;
        }
        // Either a segment name or a sub-playlist URL. Rewrite so the
        // player asks us, not Torbox directly (avoids leaking the token).
        let upstream_segment = match resolve_relative(upstream_url, trimmed) {
            Ok(u) => u,
            Err(_) => {
                out.push_str(line);
                out.push('\n');
                continue;
            }
        };
        let stripped_upstream = strip_token(&upstream_segment);

        // For a sub-playlist (.m3u8), we can't proxy without re-running
        // createstream — but it carries no token in segment names, so
        // pointing the client directly at Torbox is fine.
        let target = if trimmed.ends_with(".m3u8") {
            stripped_upstream
        } else {
            // .ts / .mp4 / init segments: keep them flowing through us so
            // the relative-URL invariant holds and we can re-direct.
            format!(
                "{prefix}/{}",
                url_encode(&extract_last_path_segment(&stripped_upstream))
            )
        };

        out.push_str(&target);
        out.push('\n');
    }
    out
}

fn build_segment_prefix(convert_id: &str, public_base: Option<&str>) -> String {
    match public_base {
        Some(base) => format!("{}/hlsv2/{}", base.trim_end_matches('/'), convert_id),
        None => format!("/hlsv2/{convert_id}"),
    }
}

fn resolve_relative(base_url: &str, candidate: &str) -> Result<String, ()> {
    if candidate.starts_with("http://") || candidate.starts_with("https://") {
        return Ok(candidate.to_owned());
    }
    let base = Url::parse(base_url).map_err(|_| ())?;
    base.join(candidate).map(|u| u.to_string()).map_err(|_| ())
}

fn strip_token(url: &str) -> String {
    let Ok(mut parsed) = Url::parse(url) else {
        return url.to_owned();
    };
    let pairs: Vec<(String, String)> = parsed
        .query_pairs()
        .filter(|(k, _)| k != "token")
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();
    if pairs.is_empty() {
        parsed.set_query(None);
    } else {
        parsed
            .query_pairs_mut()
            .clear()
            .extend_pairs(pairs.iter().map(|(k, v)| (k.as_str(), v.as_str())));
    }
    parsed.to_string()
}

fn extract_last_path_segment(url: &str) -> String {
    if let Ok(parsed) = Url::parse(url) {
        if let Some(mut segments) = parsed.path_segments() {
            if let Some(last) = segments.next_back() {
                return last.to_owned();
            }
        }
    }
    url.to_owned()
}

fn join_segment_url(playlist_url: &str, segment_name: &str) -> Result<String, String> {
    let base = Url::parse(playlist_url).map_err(|e| format!("bad upstream URL: {e}"))?;
    base.join(segment_name)
        .map(|u| u.to_string())
        .map_err(|e| format!("could not join segment: {e}"))
}
