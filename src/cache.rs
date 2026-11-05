//! Кольцевой кэш кусков: LRU + pin-окна читателей + вытеснение по байтам.
//!
//! Данные кусков лежат в памяти как bytes::Bytes (нулевое копирование при
//! отдаче: срезы шарят буфер через счётчик ссылок). Суммарный размер ограничен
//! CacheSize. Куски, попадающие в окно любого активного читателя (текущая
//! позиция + readahead), защищены от вытеснения; остальные вытесняются по
//! давности использования. Чтение: сначала кэш, при промахе кусок забирается
//! через FileStream движка и запоминается.
use anyhow::{anyhow, Result};
use bytes::Bytes;
use std::collections::HashMap;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::Mutex;

struct PieceEntry {
    data: Bytes,
    seq: u64, // для LRU
}

struct Reader {
    offset: u64,   // текущая позиция в файле
    file_len: u64, // длина файла (для окна)
}

pub struct RingCache {
    inner: Mutex<Inner>,
    pub capacity: AtomicU64, // байт (CacheSize)
    pub readahead_percent: AtomicI64,
    pub filled: AtomicU64, // сумма байтов в кэше
    seq: AtomicU64,
    pub reads_cache: AtomicU64,
    pub reads_engine: AtomicU64,
}

struct Inner {
    piece_len: u32, // 0 = ещё не известна
    pieces: HashMap<u32, PieceEntry>,
    readers: HashMap<u64, Reader>,
    next_reader: u64,
}

impl RingCache {
    pub fn new(capacity: u64, readahead_percent: i64) -> Self {
        Self {
            inner: Mutex::new(Inner {
                piece_len: 0,
                pieces: HashMap::new(),
                readers: HashMap::new(),
                next_reader: 1,
            }),
            capacity: AtomicU64::new(capacity),
            readahead_percent: AtomicI64::new(readahead_percent.clamp(5, 100)),
            filled: AtomicU64::new(0),
            seq: AtomicU64::new(1),
            reads_cache: AtomicU64::new(0),
            reads_engine: AtomicU64::new(0),
        }
    }

    pub fn set_capacity(&self, capacity: u64) {
        self.capacity.store(capacity, Ordering::Relaxed);
    }

    pub fn set_readahead_percent(&self, pct: i64) {
        self.readahead_percent
            .store(pct.clamp(5, 100), Ordering::Relaxed);
    }

    pub fn set_piece_len(&self, piece_len: u32) {
        let mut g = self.inner.lock().unwrap();
        if g.piece_len == 0 && piece_len > 0 {
            g.piece_len = piece_len;
        }
    }

    pub fn piece_len(&self) -> u32 {
        self.inner.lock().unwrap().piece_len
    }

    pub fn clear(&self) {
        let mut g = self.inner.lock().unwrap();
        g.pieces.clear();
        self.filled.store(0, Ordering::Relaxed);
    }

    pub fn len_pieces(&self) -> usize {
        self.inner.lock().unwrap().pieces.len()
    }

    /// Зарегистрировать читателя (HTTP-стрим). Возвращает id.
    pub fn add_reader(&self, offset: u64, file_len: u64) -> u64 {
        let mut g = self.inner.lock().unwrap();
        let id = g.next_reader;
        g.next_reader += 1;
        g.readers.insert(id, Reader { offset, file_len });
        id
    }

    pub fn update_reader(&self, id: u64, offset: u64) {
        let mut g = self.inner.lock().unwrap();
        if let Some(r) = g.readers.get_mut(&id) {
            r.offset = offset;
        }
    }

    pub fn remove_reader(&self, id: u64) {
        let mut g = self.inner.lock().unwrap();
        g.readers.remove(&id);
    }

    /// Окно readahead в байтах (доля кэша на одного читателя).
    fn window_bytes(&self) -> u64 {
        let pct = self.readahead_percent.load(Ordering::Relaxed) as u64;
        self.capacity.load(Ordering::Relaxed) * pct.max(5) / 100
    }

    /// Снимок читателей для CacheState: (start_piece, end_piece, reader_id).
    pub fn readers_snapshot(&self) -> Vec<(i64, i64, i64)> {
        let g = self.inner.lock().unwrap();
        let pl = g.piece_len.max(1) as u64;
        let wb = self.window_bytes();
        g.readers
            .iter()
            .map(|(id, r)| {
                let start = (r.offset / pl) as i64;
                let end = ((r.offset + wb).min(r.file_len.max(1)) / pl) as i64;
                (start, end.max(start), *id as i64)
            })
            .collect()
    }

    /// Достать кусок из кэша (обновляет LRU). Клонирование — только счётчик ссылок.
    pub fn get_piece(&self, idx: u32) -> Option<Bytes> {
        let mut g = self.inner.lock().unwrap();
        if let Some(e) = g.pieces.get_mut(&idx) {
            e.seq = self.seq.fetch_add(1, Ordering::Relaxed);
            self.reads_cache.fetch_add(1, Ordering::Relaxed);
            return Some(e.data.clone());
        }
        None
    }

    /// Положить кусок в кэш с вытеснением непинненных LRU-кусков.
    /// Вытеснение однопроходное O(n log n): кандидаты сортируются по seq один раз
    /// (прежний вариант с повторными min_by_key сканами деградировал до O(n²)
    /// при вытеснении сотен кусков за раз).
    pub fn put_piece(&self, idx: u32, data: Bytes) {
        let len = data.len() as u64;
        let mut g = self.inner.lock().unwrap();
        if g.piece_len == 0 {
            return;
        }
        let cap = self.capacity.load(Ordering::Relaxed);
        if len > cap {
            return; // кусок больше всего кэша — не кэшируем
        }
        let seq = self.seq.fetch_add(1, Ordering::Relaxed);
        if let Some(old) = g.pieces.insert(idx, PieceEntry { data, seq }) {
            self.filled
                .fetch_sub(old.data.len() as u64, Ordering::Relaxed);
        }
        self.filled.fetch_add(len, Ordering::Relaxed);
        if self.filled.load(Ordering::Relaxed) <= cap {
            return;
        }
        let pinned = pinned_ranges(&g, self.window_bytes());
        let mut evict = |allow_pinned: bool| {
            let mut victims: Vec<(u64, u32, u64)> = g
                .pieces
                .iter()
                .filter(|(i, _e)| allow_pinned || !is_pinned(&pinned, **i))
                .map(|(i, e)| (e.seq, *i, e.data.len() as u64))
                .collect();
            victims.sort_unstable_by_key(|v| v.0);
            for (_, id, l) in victims {
                if self.filled.load(Ordering::Relaxed) <= cap {
                    break;
                }
                if g.pieces.remove(&id).is_some() {
                    self.filled.fetch_sub(l, Ordering::Relaxed);
                }
            }
        };
        // сперва непинненные, затем (если всё ещё переполнено) самые старые вообще
        evict(false);
        if self.filled.load(Ordering::Relaxed) > cap {
            evict(true);
        }
    }

    /// Список кусков кэша для CacheState: (id, size).
    pub fn pieces_snapshot(&self) -> Vec<(u32, i64)> {
        let g = self.inner.lock().unwrap();
        let mut v: Vec<(u32, i64)> = g
            .pieces
            .iter()
            .map(|(i, e)| (*i, e.data.len() as i64))
            .collect();
        v.sort_unstable();
        v
    }

    /// Диапазон кусков [start, end) для байтов [offset, offset+len) файла
    /// и смещение внутри первого куска.
    pub fn piece_range(&self, offset: u64, len: u64) -> Result<(u32, u32, u64)> {
        let g = self.inner.lock().unwrap();
        let pl = g.piece_len;
        if pl == 0 {
            return Err(anyhow!("piece length unknown"));
        }
        let pl = pl as u64;
        let start_piece = (offset / pl) as u32;
        let end_off = offset + len.saturating_sub(1);
        let end_piece = (end_off / pl) as u32;
        Ok((start_piece, end_piece + 1, offset % pl))
    }

    /// Кусок частично: read [off, off+n) внутри куска idx из кэша.
    /// Срез Bytes — без копирования данных.
    pub fn read_piece_part(&self, idx: u32, off: usize, n: usize) -> Option<Bytes> {
        let data = self.get_piece(idx)?;
        if off >= data.len() {
            return None;
        }
        let end = (off + n).min(data.len());
        Some(data.slice(off..end))
    }
}

fn pinned_ranges(g: &Inner, window_bytes: u64) -> Vec<(u32, u32)> {
    let pl = g.piece_len.max(1) as u64;
    g.readers
        .values()
        .map(|r| {
            let start = (r.offset / pl) as u32;
            let end = (r.offset + window_bytes).min(r.file_len) / pl;
            (start, end as u32)
        })
        .collect()
}

fn is_pinned(ranges: &[(u32, u32)], idx: u32) -> bool {
    ranges.iter().any(|(a, b)| idx >= *a && idx <= *b)
}
