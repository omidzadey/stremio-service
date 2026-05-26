// Copyright (C) 2017-2026 Smart Code OOD 203358507

//! Thin async client for the Torbox API.
//!
//! Only covers the endpoints the gateway actually calls — the full surface
//! is documented at <https://api-docs.torbox.app/>. Every method follows
//! the same convention:
//!
//! 1. Issue the HTTP request with the configured bearer token.
//! 2. Parse the standard `{success, error, detail, data}` envelope.
//! 3. Return the inner `data` payload as a typed struct, or surface a
//!    [`TorboxError`] with the API's error code attached.
//!
//! The client is `Clone` because `reqwest::Client` already wraps its inner
//! state in an `Arc` — cloning is cheap and keeps connection pooling.

use std::time::Duration;

use reqwest::{multipart, Client};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::constants::TORBOX_API_BASE;

/// Anything that can go wrong talking to Torbox.
#[derive(Debug, Error)]
pub enum TorboxError {
    #[error("Torbox HTTP error: {0}")]
    Http(#[from] reqwest::Error),

    #[error("Torbox returned malformed JSON: {0}")]
    BadJson(String),

    #[error("Torbox API error: {code} — {detail}")]
    Api { code: String, detail: String },

    #[error("Torbox API error: {detail}")]
    NoCode { detail: String },
}

/// Standard response envelope used by every Torbox endpoint.
#[derive(Debug, Deserialize)]
pub struct Envelope<T> {
    pub success: bool,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub detail: Option<String>,
    pub data: Option<T>,
}

#[derive(Debug, Clone)]
pub struct TorboxClient {
    http: Client,
    base: String,
    token: String,
}

impl TorboxClient {
    pub fn new(token: impl Into<String>) -> Self {
        let http = Client::builder()
            .user_agent(concat!("stremio-service/", env!("CARGO_PKG_VERSION")))
            .timeout(Duration::from_secs(30))
            .build()
            .expect("reqwest client builds with default config");

        Self {
            http,
            base: TORBOX_API_BASE.to_owned(),
            token: token.into(),
        }
    }

    /// Token (for use when we need to put it into a Torbox URL, e.g.
    /// `requestdl?token=...`). Avoid logging this value.
    pub fn token(&self) -> &str {
        &self.token
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.base, path)
    }

    async fn parse<T: serde::de::DeserializeOwned>(
        resp: reqwest::Response,
    ) -> Result<T, TorboxError> {
        let status = resp.status();
        let text = resp.text().await?;
        let env: Envelope<T> = serde_json::from_str(&text).map_err(|err| {
            TorboxError::BadJson(format!("status={status} body={text} err={err}"))
        })?;
        if env.success {
            if let Some(d) = env.data {
                Ok(d)
            } else {
                // Some endpoints return `data: null` on success (e.g. clear
                // notifications). Surface a typed sentinel instead.
                Err(TorboxError::NoCode {
                    detail: env.detail.unwrap_or_else(|| "empty success body".into()),
                })
            }
        } else {
            let code = env.error.unwrap_or_else(|| "UNKNOWN_ERROR".into());
            let detail = env.detail.unwrap_or_else(|| "no detail".into());
            Err(TorboxError::Api { code, detail })
        }
    }

    /// Whether a torrent identified by its info-hash is globally cached on
    /// Torbox. Cached hashes can be added to the account in seconds; an
    /// uncached one has to be downloaded by Torbox first.
    pub async fn check_cached(
        &self,
        info_hash: &str,
    ) -> Result<Option<CachedHashInfo>, TorboxError> {
        // The cached endpoint with `format=object` returns either an object
        // keyed by hash or an empty array depending on cache state. To stay
        // tolerant of both shapes, deserialize as `Value` first.
        let resp = self
            .http
            .get(self.url("/v1/api/torrents/checkcached"))
            .bearer_auth(&self.token)
            .query(&[
                ("hash", info_hash),
                ("format", "object"),
                ("list_files", "true"),
            ])
            .send()
            .await?;

        let env: Envelope<serde_json::Value> = resp.json().await?;
        if !env.success {
            return Err(TorboxError::Api {
                code: env.error.unwrap_or_else(|| "UNKNOWN_ERROR".into()),
                detail: env.detail.unwrap_or_default(),
            });
        }
        let Some(data) = env.data else {
            return Ok(None);
        };
        if data.is_array() {
            return Ok(None);
        }
        let key_match = data
            .as_object()
            .and_then(|m| m.get(info_hash).or_else(|| m.values().next()).cloned());
        if let Some(v) = key_match {
            let info: CachedHashInfo =
                serde_json::from_value(v).map_err(|e| TorboxError::BadJson(e.to_string()))?;
            Ok(Some(info))
        } else {
            Ok(None)
        }
    }

    /// Add a magnet to the user's Torbox account. Returns the torrent_id
    /// that subsequent endpoints will use.
    pub async fn create_torrent_from_magnet(
        &self,
        magnet: &str,
    ) -> Result<CreatedTorrent, TorboxError> {
        let form = multipart::Form::new()
            .text("magnet", magnet.to_owned())
            .text("seed", "3")
            .text("allow_zip", "false")
            .text("as_queued", "false");

        let resp = self
            .http
            .post(self.url("/v1/api/torrents/createtorrent"))
            .bearer_auth(&self.token)
            .multipart(form)
            .send()
            .await?;

        Self::parse::<CreatedTorrent>(resp).await
    }

    /// Fetch a single torrent record by `id`. Useful for polling
    /// `download_state` after a fresh `createtorrent` call.
    pub async fn get_torrent(&self, id: i64) -> Result<TorrentRecord, TorboxError> {
        let resp = self
            .http
            .get(self.url("/v1/api/torrents/mylist"))
            .bearer_auth(&self.token)
            .query(&[("id", id.to_string()), ("bypass_cache", "true".to_owned())])
            .send()
            .await?;

        // `mylist` with `id=` returns the single record (object), not an array.
        Self::parse::<TorrentRecord>(resp).await
    }

    /// Find an existing torrent in the user's account by info-hash. Returns
    /// `None` if it isn't there yet.
    pub async fn find_torrent_by_hash(
        &self,
        info_hash: &str,
    ) -> Result<Option<TorrentRecord>, TorboxError> {
        let resp = self
            .http
            .get(self.url("/v1/api/torrents/mylist"))
            .bearer_auth(&self.token)
            .query(&[("bypass_cache", "true"), ("limit", "1000")])
            .send()
            .await?;

        // `mylist` without `id` returns an array.
        let list: Vec<TorrentRecord> = Self::parse(resp).await?;
        let needle = info_hash.to_ascii_lowercase();
        Ok(list.into_iter().find(|t| {
            t.hash
                .as_deref()
                .map(|h| h.eq_ignore_ascii_case(&needle))
                .unwrap_or(false)
        }))
    }

    /// Get a direct download URL for a torrent file. Returns a presigned
    /// CDN URL that supports HTTP Range — ideal as a `<video>` src.
    pub async fn request_download_link(
        &self,
        torrent_id: i64,
        file_id: i64,
    ) -> Result<String, TorboxError> {
        let resp = self
            .http
            .get(self.url("/v1/api/torrents/requestdl"))
            .bearer_auth(&self.token)
            .query(&[
                ("token", self.token.as_str()),
                ("torrent_id", &torrent_id.to_string()),
                ("file_id", &file_id.to_string()),
                ("redirect", "false"),
            ])
            .send()
            .await?;
        Self::parse::<String>(resp).await
    }

    /// Initialise (or update) a transcoding stream. The returned `hls_url`
    /// is the playlist URL the player should hit. The first call also
    /// kicks off the transcoder, so the first GET on `hls_url` can take
    /// ~60s — call this from `/probe` to warm the pipeline.
    pub async fn create_stream(
        &self,
        opts: &CreateStreamOptions,
    ) -> Result<StreamData, TorboxError> {
        let mut query: Vec<(&str, String)> = vec![
            ("id", opts.id.to_string()),
            ("file_id", opts.file_id.to_string()),
            ("type", opts.kind.as_str().to_owned()),
        ];
        if let Some(idx) = opts.audio_index {
            query.push(("chosen_audio_index", idx.to_string()));
        }
        if let Some(idx) = opts.subtitle_index {
            query.push(("chosen_subtitle_index", idx.to_string()));
        }
        if let Some(idx) = opts.resolution_index {
            query.push(("chosen_resolution_index", idx.to_string()));
        }

        let resp = self
            .http
            .get(self.url("/v1/api/stream/createstream"))
            .bearer_auth(&self.token)
            .query(&query)
            .send()
            .await?;
        Self::parse::<StreamData>(resp).await
    }

    /// Re-fetch the state of an already-created stream.
    pub async fn get_stream_data(
        &self,
        presigned_token: &str,
        opts: &GetStreamDataOverrides,
    ) -> Result<StreamData, TorboxError> {
        let mut query: Vec<(&str, String)> = vec![
            ("presigned_token", presigned_token.to_owned()),
            ("token", self.token.clone()),
        ];
        if let Some(idx) = opts.audio_index {
            query.push(("chosen_audio_index", idx.to_string()));
        }
        if let Some(idx) = opts.subtitle_index {
            query.push(("chosen_subtitle_index", idx.to_string()));
        }
        if let Some(idx) = opts.resolution_index {
            query.push(("chosen_resolution_index", idx.to_string()));
        }

        let resp = self
            .http
            .get(self.url("/v1/api/stream/getstreamdata"))
            .bearer_auth(&self.token)
            .query(&query)
            .send()
            .await?;
        Self::parse::<StreamData>(resp).await
    }
}

// -- request/response types -----------------------------------------------

/// What `checkcached` tells us about a globally-cached hash.
#[derive(Debug, Clone, Deserialize)]
pub struct CachedHashInfo {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub size: Option<i64>,
    #[serde(default)]
    pub hash: Option<String>,
    #[serde(default)]
    pub files: Vec<CachedFileInfo>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CachedFileInfo {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub size: Option<i64>,
}

/// Data returned by `POST /torrents/createtorrent`.
///
/// Torbox returns slightly different shapes depending on whether the
/// torrent was already in the account or freshly added; the fields we care
/// about are present in both, so we keep this loose.
#[derive(Debug, Clone, Deserialize)]
pub struct CreatedTorrent {
    #[serde(alias = "id", alias = "torrent_id", default)]
    pub id: Option<i64>,
    #[serde(default)]
    pub hash: Option<String>,
    #[serde(default)]
    pub auth_id: Option<String>,
    #[serde(default)]
    pub queued_id: Option<i64>,
}

/// One element of `GET /torrents/mylist`.
#[derive(Debug, Clone, Deserialize)]
pub struct TorrentRecord {
    pub id: i64,
    #[serde(default)]
    pub hash: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub size: Option<i64>,
    #[serde(default)]
    pub download_state: Option<String>,
    #[serde(default)]
    pub progress: Option<f64>,
    #[serde(default)]
    pub cached: Option<bool>,
    /// Files may be `null` (e.g. for failed/stalled torrents without any
    /// downloaded content), so we accept either `null` or an array.
    #[serde(default, deserialize_with = "deserialize_null_to_empty_vec")]
    pub files: Vec<TorrentFile>,
}

fn deserialize_null_to_empty_vec<'de, D, T>(d: D) -> Result<Vec<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<Vec<T>>::deserialize(d).map(|v| v.unwrap_or_default())
}

impl TorrentRecord {
    /// True when the torrent is fully downloaded and ready to stream.
    pub fn is_ready(&self) -> bool {
        match self.download_state.as_deref() {
            Some(s) => {
                matches!(
                    s,
                    "cached" | "completed" | "uploading" | "seeding" | "stalled (UP)"
                ) || self.progress.unwrap_or(0.0) >= 1.0
            }
            None => self.progress.unwrap_or(0.0) >= 1.0,
        }
    }

    /// Pick the file index Stremio would default to: the largest file
    /// whose mimetype looks like video, falling back to the largest file
    /// overall.
    pub fn pick_video_file(&self) -> Option<&TorrentFile> {
        let video_pick = self
            .files
            .iter()
            .filter(|f| f.is_video_like())
            .max_by_key(|f| f.size.unwrap_or(0));
        video_pick.or_else(|| self.files.iter().max_by_key(|f| f.size.unwrap_or(0)))
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct TorrentFile {
    pub id: i64,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub size: Option<i64>,
    #[serde(default)]
    pub mimetype: Option<String>,
}

impl TorrentFile {
    pub fn is_video_like(&self) -> bool {
        let mime = self.mimetype.as_deref().unwrap_or("");
        if mime.starts_with("video/") {
            return true;
        }
        let name = self.name.as_deref().unwrap_or("").to_ascii_lowercase();
        const VIDEO_EXTS: &[&str] = &[
            ".mkv", ".mp4", ".avi", ".mov", ".m4v", ".webm", ".ts", ".m2ts", ".wmv", ".flv",
        ];
        VIDEO_EXTS.iter().any(|ext| name.ends_with(ext))
    }
}

/// Selector for `GET /stream/createstream`.
#[derive(Debug, Clone, Default)]
pub struct CreateStreamOptions {
    pub id: i64,
    pub file_id: i64,
    pub kind: StreamKind,
    pub audio_index: Option<u32>,
    pub subtitle_index: Option<u32>,
    pub resolution_index: Option<u32>,
}

#[derive(Debug, Clone, Default)]
pub struct GetStreamDataOverrides {
    pub audio_index: Option<u32>,
    pub subtitle_index: Option<u32>,
    pub resolution_index: Option<u32>,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StreamKind {
    #[default]
    Torrent,
    Usenet,
    Webdownload,
}

impl StreamKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            StreamKind::Torrent => "torrent",
            StreamKind::Usenet => "usenet",
            StreamKind::Webdownload => "webdownload",
        }
    }
}

/// Subset of `data` from `/stream/createstream` and `/stream/getstreamdata`.
/// We only deserialize fields the gateway actually uses.
#[derive(Debug, Clone, Deserialize)]
pub struct StreamData {
    #[serde(default)]
    pub hls_url: Option<String>,
    #[serde(default)]
    pub domain: Option<String>,
    #[serde(default)]
    pub presigned_token: Option<String>,
    #[serde(default)]
    pub mimetype: Option<String>,
    #[serde(default)]
    pub size: Option<i64>,
    #[serde(default)]
    pub needs_transcoding: Option<bool>,
    #[serde(default)]
    pub is_transcoding: Option<bool>,
    #[serde(default)]
    pub open_subtitles_hash: Option<String>,
    #[serde(default)]
    pub intro_information: Option<IntroInformation>,
    #[serde(default)]
    pub metadata: Option<StreamMetadata>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct IntroInformation {
    #[serde(default)]
    pub start_time: Option<f64>,
    #[serde(default)]
    pub end_time: Option<f64>,
    #[serde(default)]
    pub title: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct StreamMetadata {
    #[serde(default)]
    pub video: Option<VideoTrack>,
    #[serde(default)]
    pub audios: Vec<AudioTrack>,
    #[serde(default)]
    pub subtitles: Vec<SubtitleTrack>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct VideoTrack {
    #[serde(default)]
    pub codec: Option<String>,
    #[serde(default)]
    pub width: Option<u32>,
    #[serde(default)]
    pub height: Option<u32>,
    #[serde(default)]
    pub frame_rate: Option<String>,
    #[serde(default)]
    pub bitrate: Option<String>,
    #[serde(default)]
    pub duration: Option<String>,
    #[serde(default)]
    pub pixel_format: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AudioTrack {
    #[serde(default)]
    pub index: Option<u32>,
    #[serde(default)]
    pub codec: Option<String>,
    #[serde(default)]
    pub channels: Option<u32>,
    #[serde(default)]
    pub language: Option<String>,
    #[serde(default)]
    pub language_full: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub default: Option<bool>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SubtitleTrack {
    #[serde(default)]
    pub index: Option<u32>,
    #[serde(default)]
    pub codec: Option<String>,
    #[serde(default)]
    pub language: Option<String>,
    #[serde(default)]
    pub language_full: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub default: Option<bool>,
}
