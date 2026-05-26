// Copyright (C) 2017-2026 Smart Code OOD 203358507

//! Shared state held by every gateway route handler.
//!
//! The state is intentionally tiny: a Torbox client, the operator config,
//! and a small in-memory cache mapping `info_hash` to the corresponding
//! Torbox identifiers. The cache is the difference between "every play
//! kicks off a 60/hour-rate-limited `createtorrent`" and "Torbox state is
//! looked up once and reused for the rest of the session".

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use tokio::sync::RwLock;

use crate::config::GatewayConfig;
use crate::torbox::{StreamData, TorboxClient};

/// What we remember about a given `info_hash` once we've talked to Torbox.
#[derive(Debug, Clone)]
pub struct TorrentMapping {
    pub torrent_id: i64,
    pub file_id: i64,
    pub file_name: Option<String>,
    pub size: Option<i64>,
    /// Last time `created_at` / `last_used_at` was updated. We currently
    /// only use it for debug logging, but it would let us evict entries.
    pub last_used_at: Instant,
    /// The most recent stream metadata returned by `createstream`. Lets
    /// `/probe` answer without re-hitting Torbox on every poll cycle.
    pub last_stream: Option<StreamData>,
}

/// Aliases for clarity.
pub type SharedState = Arc<AppState>;

pub struct AppState {
    pub torbox: TorboxClient,
    pub cfg: GatewayConfig,
    /// `info_hash` (lowercase hex) -> Torbox mapping.
    pub mappings: RwLock<HashMap<String, TorrentMapping>>,
}

impl AppState {
    pub fn new(torbox: TorboxClient, cfg: GatewayConfig) -> Self {
        Self {
            torbox,
            cfg,
            mappings: RwLock::new(HashMap::new()),
        }
    }

    pub async fn get_mapping(&self, info_hash: &str) -> Option<TorrentMapping> {
        let map = self.mappings.read().await;
        map.get(&info_hash.to_ascii_lowercase()).cloned()
    }

    pub async fn put_mapping(&self, info_hash: &str, mapping: TorrentMapping) {
        let mut map = self.mappings.write().await;
        map.insert(info_hash.to_ascii_lowercase(), mapping);
    }

    pub async fn update_stream(&self, info_hash: &str, stream: StreamData) {
        let mut map = self.mappings.write().await;
        if let Some(entry) = map.get_mut(&info_hash.to_ascii_lowercase()) {
            entry.last_stream = Some(stream);
            entry.last_used_at = Instant::now();
        }
    }
}
