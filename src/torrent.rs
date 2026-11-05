//! Состояние торрента: статус, watcher (скорости, таймауты), файлы, ETag.
use crate::cache::RingCache;
use crate::engine::id20_from_hex;
use crate::models::*;
use crate::ramstore::RamRegistry;
use crate::serve;
use anyhow::Result;
use librqbit::ManagedTorrent;
use librqbit_core::hash_id::Id20;
use std::sync::atomic::{AtomicI32, AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::time::{SystemTime, UNIX_EPOCH};

pub type DynStream = Box<dyn ReadSeek + Send + Unpin>;

/// Один principal-трейт, чтобы можно было сделать trait object
/// (FileStream в librqbit 9.0.1 не экспортируется под именем).
pub trait ReadSeek: tokio::io::AsyncRead + tokio::io::AsyncSeek {}
impl<T: tokio::io::AsyncRead + tokio::io::AsyncSeek + ?Sized> ReadSeek for T {}

pub struct PreloadState {
    pub preloaded_bytes: AtomicI64,
    pub preload_size: AtomicI64,
    pub done: AtomicI64, // 0 = нет, 1 = идёт, 2 = готово
}

/// Файл торрента: id (1-based, сортировка по пути), orig (индекс в метаданных
/// librqbit, 0-based), путь, длина.
#[derive(Clone, Debug)]
pub struct FileInfo {
    pub id: usize,
    pub orig: usize,
    pub path: String,
    pub len: i64,
}

pub struct Torrent {
    pub hash_hex: String,
    pub title: RwLock<String>,
    pub poster: RwLock<String>,
    pub data_b64: RwLock<String>,
    pub handle: Arc<ManagedTorrent>,
    pub cache: Arc<RingCache>,
    pub added_at: i64,
    pub last_access: AtomicI64, // unix sec
    pub readers: AtomicU64,
    pub preload: PreloadState,
    pub save_to_db: AtomicI64,
    pub stat: AtomicI32,
    /// имя торрента (после получения метаданных)
    pub name: RwLock<String>,
    /// файлы, отсортированные по пути
    pub files: RwLock<Vec<FileInfo>>,
    /// RAM-режим: реестр RamTorrent + info_hash. Через ram_piece() HTTP-насос
    /// и preload забирают resident-кусок по ссылке (Bytes, счётчик ссылок) —
    /// физическая копия данных в RAM одна (см. ramstore.rs, v1.4.6).
    pub ram: Option<(Arc<RamRegistry>, Id20)>,
}

pub fn now_sec() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Zero-copy доступ к завершённому resident-куску RAM-хранилища.
/// Возвращает Bytes, разделяющий буфер RamTorrent (без копирования);
/// в дисковом режиме всегда None (там кэш хранит свою копию). Также
/// обновляет LRU RamTorrent — отдача стримера защищает кусок от вытеснения.
pub fn ram_piece(t: &Torrent, idx: u32) -> Option<bytes::Bytes> {
    let (reg, id) = t.ram.as_ref()?;
    reg.piece_bytes(id, idx)
}

impl Torrent {
    pub fn new(
        hash_hex: String,
        handle: Arc<ManagedTorrent>,
        sets: &BTSets,
        ram: Option<Arc<RamRegistry>>,
    ) -> Arc<Self> {
        let piece_len = handle
            .with_metadata(|m| m.info.info().piece_length)
            .unwrap_or(0);
        let cache = Arc::new(RingCache::new(
            sets.CacheSize.max(0) as u64,
            sets.ReaderReadAHead,
        ));
        cache.set_piece_len(piece_len);
        let ram = ram.and_then(|r| id20_from_hex(&hash_hex).ok().map(|id| (r, id)));
        Arc::new(Self {
            hash_hex,
            title: RwLock::new(String::new()),
            poster: RwLock::new(String::new()),
            data_b64: RwLock::new(String::new()),
            handle,
            cache,
            added_at: now_sec(),
            last_access: AtomicI64::new(now_sec()),
            readers: AtomicU64::new(0),
            preload: PreloadState {
                preloaded_bytes: AtomicI64::new(0),
                preload_size: AtomicI64::new(0),
                done: AtomicI64::new(0),
            },
            save_to_db: AtomicI64::new(0),
            stat: AtomicI32::new(STAT_ADDED),
            name: RwLock::new(String::new()),
            files: RwLock::new(Vec::new()),
            ram,
        })
    }

    pub fn touch(&self) {
        self.last_access.store(now_sec(), Ordering::Relaxed);
    }

    pub fn idle_secs(&self) -> i64 {
        now_sec() - self.last_access.load(Ordering::Relaxed)
    }

    /// Обновить name/files из метаданных. false = метаданных ещё нет.
    pub fn refresh_metadata(&self) -> bool {
        if !self.files.read().unwrap().is_empty() {
            return true;
        }
        let meta = match self.handle.with_metadata(|m| m.clone()) {
            Ok(m) => m,
            Err(_) => return false,
        };
        let info = meta.info.info(); // &TorrentMetaV1Info<ByteBufOwned>
        let name = info
            .name
            .as_ref()
            .map(|n| String::from_utf8_lossy(n.as_ref()).to_string())
            .unwrap_or_else(|| self.hash_hex.clone());
        let mut files: Vec<FileInfo> = Vec::new();
        match &info.files {
            Some(fs) => {
                for (i, f) in fs.iter().enumerate() {
                    let path = f
                        .path
                        .iter()
                        .map(|p| String::from_utf8_lossy(p.as_ref()).to_string())
                        .collect::<Vec<_>>()
                        .join("/");
                    files.push(FileInfo {
                        id: 0,
                        orig: i,
                        path,
                        len: f.length as i64,
                    });
                }
            }
            None => {
                let len = info.length.unwrap_or(0) as i64;
                files.push(FileInfo {
                    id: 0,
                    orig: 0,
                    path: name.clone(),
                    len,
                });
            }
        }
        files.sort_by(|a, b| a.path.cmp(&b.path));
        for (i, f) in files.iter_mut().enumerate() {
            f.id = i + 1;
        }
        *self.name.write().unwrap() = name;
        *self.files.write().unwrap() = files;
        true
    }

    pub fn file_list(&self) -> Vec<FileInfo> {
        self.files.read().unwrap().clone()
    }

    /// Разрешить индекс из запроса (index/id) в orig-индекс librqbit.
    /// Принимаем: 0 -> первый файл; 1..=n -> id (1-based, отсортирован по пути);
    /// n+1.. — как 0-based позиция (совместимость).
    pub fn resolve_file(&self, idx: i64) -> Result<FileInfo> {
        let files = self.file_list();
        anyhow::ensure!(!files.is_empty(), "no files/metadata yet");
        if idx == 0 {
            return Ok(files[0].clone());
        }
        if idx >= 1 as i64 && (idx as usize) <= files.len() {
            return Ok(files[(idx - 1) as usize].clone());
        }
        if idx >= 0 && (idx as usize) < files.len() {
            return Ok(files[idx as usize].clone());
        }
        Err(anyhow::anyhow!("file index {} out of range", idx))
    }

    /// FileStream по orig-индексу.
    pub async fn file_stream(&self, orig: usize) -> Result<DynStream> {
        let s = self.handle.clone().stream(orig).await?;
        Ok(Box::new(s))
    }

    pub fn torrent_size(&self) -> i64 {
        self.handle.stats().total_bytes as i64
    }

    /// Полный TorrentStatus для API.
    pub fn status(&self) -> TorrentStatus {
        let st = self.stat.load(Ordering::Relaxed);
        let stats = self.handle.stats();
        let live = stats.live.as_ref();
        let snap = live.map(|l| &l.snapshot);
        let ps = snap.map(|s| &s.peer_stats);
        TorrentStatus {
            title: self.title.read().unwrap().clone(),
            poster: self.poster.read().unwrap().clone(),
            data: self.data_b64.read().unwrap().clone(),
            timestamp: self.added_at,
            name: self.name.read().unwrap().clone(),
            hash: self.hash_hex.clone(),
            stat: st,
            stat_string: stat_string(st).to_string(),
            loaded_size: stats.progress_bytes as i64,
            torrent_size: stats.total_bytes as i64,
            preloaded_bytes: self.preload.preloaded_bytes.load(Ordering::Relaxed),
            preload_size: self.preload.preload_size.load(Ordering::Relaxed),
            download_speed: live.map(|l| l.download_speed.as_bytes()).unwrap_or(0) as f64,
            upload_speed: live.map(|l| l.upload_speed.as_bytes()).unwrap_or(0) as f64,
            total_peers: ps.map(|p| p.seen as i64).unwrap_or(0),
            pending_peers: ps.map(|p| p.queued as i64).unwrap_or(0),
            active_peers: ps.map(|p| (p.connecting + p.live) as i64).unwrap_or(0),
            connected_seeders: ps.map(|p| p.live as i64).unwrap_or(0), // аппроксимация
            half_open_peers: ps.map(|p| p.connecting as i64).unwrap_or(0),
            bytes_written: 0,
            bytes_written_data: snap.map(|s| s.uploaded_bytes as i64).unwrap_or(0),
            bytes_read: self.cache.reads_cache.load(Ordering::Relaxed) as i64
                + self.cache.reads_engine.load(Ordering::Relaxed) as i64,
            bytes_read_data: stats.progress_bytes as i64,
            bytes_read_useful_data: stats.progress_bytes as i64,
            chunks_read: self.cache.reads_engine.load(Ordering::Relaxed) as i64,
            chunks_read_useful: self.cache.reads_engine.load(Ordering::Relaxed) as i64,
            file_stats: self
                .file_list()
                .into_iter()
                .map(|f| TorrentFileStat {
                    id: f.id,
                    path: f.path,
                    length: f.len,
                })
                .collect(),
            ..Default::default()
        }
    }
}

/// CacheState для /cache.
pub fn cache_state(t: &Torrent) -> serde_json::Value {
    use serde_json::json;
    let pl = t.cache.piece_len() as i64;
    let pieces = t.cache.pieces_snapshot();
    let readers = t
        .cache
        .readers_snapshot()
        .into_iter()
        .map(|(s, e, r)| json!({ "Start": s, "End": e, "Reader": r }))
        .collect::<Vec<_>>();
    json!({
        "Hash": t.hash_hex,
        "Capacity": t.cache.capacity.load(Ordering::Relaxed),
        "Filled": t.cache.filled.load(Ordering::Relaxed),
        "PiecesLength": pl,
        "PiecesCount": pieces.len() as i64,
        "Torrent": t.readers.load(Ordering::Relaxed),
        "Pieces": pieces
            .into_iter()
            .map(|(id, size)| json!({
                "Id": id, "Length": pl, "Size": size,
                "Completed": 1, "Priority": 0
            }))
            .collect::<Vec<_>>(),
        "Readers": readers,
    })
}

pub fn file_etag(t: &Torrent, file_id: usize, size: u64) -> String {
    serve::etag_for(&t.hash_hex, file_id, size)
}
