//! JSON-модели с точными полями TorrServer (MatriX.111).
// Имена полей повторяют JSON оригинала (Go-стиль: BTSets, CacheSize, ...).
#![allow(non_snake_case)]
use serde::{Deserialize, Serialize};

/// Статусы торрента (как в оригинале).
pub const STAT_ADDED: i32 = 0;
pub const STAT_GETINFO: i32 = 1;
pub const STAT_PRELOAD: i32 = 2;
pub const STAT_WORKING: i32 = 3;
pub const STAT_CLOSED: i32 = 4;
pub const STAT_INDB: i32 = 5;

pub fn stat_string(stat: i32) -> &'static str {
    match stat {
        STAT_ADDED => "Torrent added",
        STAT_GETINFO => "Torrent getting info",
        STAT_PRELOAD => "Torrent preload",
        STAT_WORKING => "Torrent working",
        STAT_CLOSED => "Torrent closed",
        STAT_INDB => "Torrent in db",
        _ => "Unknown",
    }
}

/// Настройки BTSets — имена полей с заглавной буквы, как в Go-версии.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct BTSets {
    pub CacheSize: i64,
    pub ReaderReadAHead: i64,
    pub PreloadCache: i64,
    pub UseDisk: bool,
    pub TorrentsSavePath: String,
    pub RemoveCacheOnDrop: bool,
    pub ForceEncrypt: bool,
    pub RetrackersMode: i64,
    pub TorrentDisconnectTimeout: i64,
    pub EnableDebug: bool,
    pub EnableDLNA: bool,
    pub EnableIPv6: bool,
    pub DisableTCP: bool,
    pub DisableUTP: bool,
    pub DisableUPNP: bool,
    pub DisableDHT: bool,
    pub DisablePEX: bool,
    pub DisableUpload: bool,
    pub DownloadRateLimit: i64,
    pub UploadRateLimit: i64,
    pub ConnectionsLimit: i64,
    pub PeersListenPort: i64,
}

impl Default for BTSets {
    fn default() -> Self {
        Self {
            CacheSize: 64 * 1024 * 1024,      // 64 MiB
            ReaderReadAHead: 95,              // % кэша на чтение вперёд (5..100)
            PreloadCache: 50,                 // % кэша на префетч (0..100)
            UseDisk: true,
            TorrentsSavePath: String::new(),
            RemoveCacheOnDrop: false,
            ForceEncrypt: false,
            RetrackersMode: 1,
            TorrentDisconnectTimeout: 30,
            EnableDebug: false,
            EnableDLNA: false,
            EnableIPv6: false,
            DisableTCP: false,
            DisableUTP: false,
            DisableUPNP: false,
            DisableDHT: false,
            DisablePEX: false,
            DisableUpload: false,
            DownloadRateLimit: 0,             // KB/s, 0 = без лимита
            UploadRateLimit: 0,
            ConnectionsLimit: 23,
            PeersListenPort: 0,               // 0 = случайный
        }
    }
}

impl BTSets {
    pub fn sanitize(&mut self) {
        if !(5..=100).contains(&self.ReaderReadAHead) {
            self.ReaderReadAHead = 95;
        }
        if !(0..=100).contains(&self.PreloadCache) {
            self.PreloadCache = 50;
        }
        if self.CacheSize < 16 * 1024 * 1024 {
            self.CacheSize = 16 * 1024 * 1024;
        }
        if self.CacheSize > 6 * 1024 * 1024 * 1024 {
            self.CacheSize = 6 * 1024 * 1024 * 1024;
        }
        if self.TorrentDisconnectTimeout < 10 {
            self.TorrentDisconnectTimeout = 10;
        }
        if !(0..=4).contains(&self.RetrackersMode) {
            self.RetrackersMode = 1;
        }
        if self.ConnectionsLimit < 1 {
            self.ConnectionsLimit = 23;
        }
    }

    /// Консервативный профиль --low-memory: ограничить кэш, пиров и префетч.
    /// RSS процесса — это не только CacheSize (движок, пировые буферы, ядро),
    /// поэтому зажимаем и сетевые параметры, а не только кэш.
    pub fn sanitize_low_memory(&mut self) {
        self.sanitize();
        const CACHE_CAP: i64 = 8 * 1024 * 1024;
        self.CacheSize = self.CacheSize.clamp(2 * 1024 * 1024, CACHE_CAP);
        self.ReaderReadAHead = self.ReaderReadAHead.clamp(10, 50);
        self.PreloadCache = self.PreloadCache.min(10);
        self.ConnectionsLimit = self.ConnectionsLimit.min(8).max(1);
        self.TorrentDisconnectTimeout = self.TorrentDisconnectTimeout.clamp(5, 15);
    }
}

/// save_to_db приходит и булевым значением, и строкой "true"/"false"
/// (Lampa шлёт Storage.get('torrserver_savedb', 'false') как есть).
fn deserialize_boolish<'de, D>(deserializer: D) -> Result<bool, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let v = serde_json::Value::deserialize(deserializer)?;
    match v {
        serde_json::Value::Bool(b) => Ok(b),
        serde_json::Value::Null => Ok(false),
        serde_json::Value::Number(n) => Ok(n.as_i64().unwrap_or(0) != 0),
        serde_json::Value::String(s) => Ok(matches!(
            s.trim().to_ascii_lowercase().as_str(),
            "true" | "1" | "yes" | "on"
        )),
        _ => Ok(false),
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct TorrentFileStat {
    pub id: usize,
    pub path: String,
    pub length: i64,
}

/// TorrentStatus — строчные JSON-поля, как в оригинале.
#[derive(Clone, Debug, Default, Serialize)]
pub struct TorrentStatus {
    pub title: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub poster: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub data: String,
    pub timestamp: i64,
    pub name: String,
    pub hash: String,
    pub stat: i32,
    pub stat_string: String,
    pub loaded_size: i64,
    pub torrent_size: i64,
    pub preloaded_bytes: i64,
    pub preload_size: i64,
    pub download_speed: f64,
    pub upload_speed: f64,
    pub total_peers: i64,
    pub pending_peers: i64,
    pub active_peers: i64,
    pub connected_seeders: i64,
    pub half_open_peers: i64,
    pub bytes_written: i64,
    pub bytes_written_data: i64,
    pub bytes_read: i64,
    pub bytes_read_data: i64,
    pub bytes_read_useful_data: i64,
    pub chunks_written: i64,
    pub chunks_read: i64,
    pub chunks_read_useful: i64,
    pub chunks_read_wasted: i64,
    pub pieces_dirtied_good: i64,
    pub pieces_dirtied_bad: i64,
    pub file_stats: Vec<TorrentFileStat>,
}

/// CacheState — имена полей с заглавной буквы (Go).
#[derive(Debug, Serialize)]
pub struct CachePiece {
    pub Id: u32,
    pub Length: i64,
    pub Size: i64,
    pub Completed: i64,
    pub Priority: i64,
}

#[derive(Debug, Serialize)]
pub struct CacheReader {
    pub Start: i64,
    pub End: i64,
    pub Reader: i64,
}

#[derive(Debug, Serialize)]
pub struct CacheState {
    pub Hash: String,
    pub Capacity: i64,
    pub Filled: i64,
    pub PiecesLength: i64,
    pub PiecesCount: i64,
    pub Torrent: i64,
    pub Pieces: Vec<CachePiece>,
    pub Readers: Vec<CacheReader>,
}

#[derive(Debug, Default, Deserialize)]
pub struct TorrentsRequest {
    pub action: String,
    #[serde(default)]
    pub link: String,
    #[serde(default)]
    pub hash: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub poster: String,
    #[serde(default)]
    pub data: String,
    #[serde(default, deserialize_with = "deserialize_boolish")]
    pub save_to_db: bool,
}

#[derive(Debug, Default, Deserialize)]
pub struct SettingsRequest {
    pub action: String,
    #[serde(default)]
    pub sets: BTSets,
}

#[derive(Debug, Default, Deserialize)]
pub struct ViewedRequest {
    #[serde(default)]
    pub action: String,
    #[serde(default)]
    pub hash: String,
    #[serde(default)]
    pub file_index: i64,
    /// таймкод воспроизведения (сек); в MatriX.111 игнорируется,
    /// в master/Lampa — используется для "продолжить просмотр"
    #[serde(default)]
    pub timecode: f64,
}

/// Элемент ответа /viewed (list): как в оригинале + timecode из master.
#[derive(Clone, Debug, Serialize)]
pub struct ViewedItem {
    pub hash: String,
    pub file_index: i64,
    pub timecode: f64,
}

#[derive(Debug, Default, Deserialize)]
pub struct CacheRequest {
    pub action: String,
    #[serde(default)]
    pub hash: String,
}
