// Copyright (C) 2017-2026 Smart Code OOD 203358507

//! Gateway HTTP routes, broken up by concern.
//!
//! - [`meta`] — `/settings`, `/status`, `/heartbeat`, `/network-info`,
//!   `/device-info`, `/probe`, plus a few small helpers.
//! - [`stream`] — torrent lifecycle: `POST /:hash/create`,
//!   `GET /:hash/:idx`, `stats.json`, `remove`.
//! - [`hls`] — `/hlsv2/...` family: master & media playlists and
//!   segment redirects.

pub mod helpers;
pub mod hls;
pub mod meta;
pub mod stream;
