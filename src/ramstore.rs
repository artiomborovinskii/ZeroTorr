//! RAM-хранилище librqbit: куски торрента живут в памяти с жёстким бюджетом.
//!
//! Зачем: на устройствах, где «диск» — это tmpfs (Yandex Station и т.п.),
//! любой файл = невытесняемая RAM, и неограниченная запись скачанных кусков
//! убивает систему (OOM). В RAM-режиме куски принимает этот бэкенд: LRU
//! вытесняет давно не трогаемые куски за бюджет (= CacheSize), а через
//! ManagedTorrent::forget_pieces (вендорный патч librqbit) сбрасывает бит
//! «have» — вытесненный кусок перекачивается при первом обращении стримера.
//!
//! Рабочее окно защищается естественно: чтение стримера обновляет touch,
//! поэтому куски у текущей позиции переживают куски, скачанные далеко вперёд.
//! Окно опережения скачивания ограничивается ZT_STREAM_BUF (см. engine.rs),
//! чтобы оно не превышало бюджет и не вызывало холостые перекачки.
//!
//! v1.4.6: куски хранятся как bytes::Bytes (заморозка BytesMut по завершении
//! куска), а реестр выдаёт буфер resident-куска по ссылке (piece_bytes) —
//! HTTP-насос и preload отдают и индексируют РАЗДЕЛЯЕМЫЙ буфер, физическая
//! копия данных в RAM одна (раньше RingCache дублировал RamTorrent целиком
//! в пределах своего бюджета). Clone/take хранилища разделяют то же
//! Arc<Mutex<..>> — пауза больше не делает глубокую копию карты кусков.
use anyhow::{anyhow, Result};
use bytes::{Bytes, BytesMut};
use librqbit::storage::{BoxStorageFactory, StorageFactory, StorageFactoryExt, TorrentStorage};
use librqbit::{ManagedTorrent, ManagedTorrentShared, TorrentMetadata};
use librqbit_core::hash_id::Id20;
use librqbit_core::lengths::ValidPieceIndex;
use std::collections::HashMap;
use std::io::IoSlice;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};

/// Бюджет не меньше трёх кусков, иначе LRU вытесняет только что скачанное.
const MIN_BUDGET_PIECES: u64 = 3;

/// Захват мьютекса, устойчивый к отравлению (poisoned lock). При panic="abort"
/// паника на .unwrap() отравленного лока — мгновенная смерть процесса (именно
/// так падал v1.4.0 на станции: ramstore.rs:187 Option::unwrap on None).
/// Данные под отравленным локом остаются консистентными (все операции с
/// кусками атомарны под локом), поэтому спокойно продолжаем работу.
fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

/// Данные одного куска: пока кусок качается блоками — Partial (заполняемый
/// BytesMut, как раньше Vec<u8>), после проверки хэша — Full (иммутабельный
/// Bytes, который можно отдавать по ссылке без копирования).
enum PieceBuf {
    Partial(BytesMut),
    Full(Bytes),
}

impl PieceBuf {
    fn len(&self) -> usize {
        match self {
            PieceBuf::Partial(b) => b.len(),
            PieceBuf::Full(b) => b.len(),
        }
    }

    fn as_slice(&self) -> &[u8] {
        match self {
            PieceBuf::Partial(b) => b,
            PieceBuf::Full(b) => b,
        }
    }

    /// Изменяемый доступ. Если кусок уже был заморожен, а librqbit снова
    /// пишет (нештатный путь до-записи), размораживаем с сохранением данных.
    fn as_mut(&mut self) -> &mut BytesMut {
        if let PieceBuf::Full(b) = self {
            *self = PieceBuf::Partial(BytesMut::from(&b[..]));
        }
        match self {
            PieceBuf::Partial(b) => b,
            PieceBuf::Full(_) => unreachable!("as_mut: just unfrozen"),
        }
    }

    /// Заморозка: BytesMut -> Bytes без копирования (вызывается по
    /// on_piece_completed, когда хэш куска проверен).
    fn freeze(&mut self) {
        if let PieceBuf::Partial(b) = std::mem::replace(self, PieceBuf::Partial(BytesMut::new())) {
            *self = PieceBuf::Full(b.freeze());
        }
    }
}

#[derive(Clone)]
pub struct RamRegistry {
    /// Бюджет одного торрента, байт (обычно = CacheSize из настроек).
    budget: Arc<AtomicU64>,
    /// info_hash -> слабая ссылка на ManagedTorrent: по ней при вытеснении
    /// вызывается forget_pieces (после регистрации в manager).
    handles: Arc<Mutex<HashMap<Id20, Weak<ManagedTorrent>>>>,
    /// info_hash -> слабая ссылка на общее состояние RamTorrent: через неё
    /// piece_bytes отдаёт resident-куски без копирования.
    stores: Arc<Mutex<HashMap<Id20, Weak<Mutex<RamInner>>>>>,
    /// Суммарно байт во всех RAM-хранилищах (статистика).
    used_total: Arc<AtomicU64>,
    /// Weak на сам Arc (фабрика создаёт хранилища, которые должны уметь
    /// достать реестр без удержания Arc).
    self_weak: Arc<OnceLock<Weak<RamRegistry>>>,
}

impl RamRegistry {
    pub fn new(budget: u64) -> Arc<Self> {
        let arc = Arc::new(Self {
            budget: Arc::new(AtomicU64::new(budget)),
            handles: Arc::new(Mutex::new(HashMap::new())),
            stores: Arc::new(Mutex::new(HashMap::new())),
            used_total: Arc::new(AtomicU64::new(0)),
            self_weak: Arc::new(OnceLock::new()),
        });
        let _ = arc.self_weak.set(Arc::downgrade(&arc));
        arc
    }

    pub fn set_budget(&self, budget: u64) {
        self.budget.store(budget, Ordering::Relaxed);
    }

    pub fn budget(&self) -> u64 {
        self.budget.load(Ordering::Relaxed)
    }

    /// Суммарный объём данных в RAM-хранилищах (байт).
    pub fn used_bytes(&self) -> u64 {
        self.used_total.load(Ordering::Relaxed)
    }

    /// Зарегистрировать торрент после добавления в сессию (до регистрации у
    /// торрента нет хендла и пиров, значит и вытеснений не будет).
    pub fn register(&self, hash: Id20, handle: &Arc<ManagedTorrent>) {
        lock(&self.handles).insert(hash, Arc::downgrade(handle));
    }

    pub fn unregister(&self, hash: &Id20) {
        lock(&self.handles).remove(hash);
        lock(&self.stores).remove(hash);
    }

    /// Буфер завершённого resident-куска (только Full): Bytes::clone — это
    /// счётчик ссылок, данные не копируются. Вызов обновляет LRU.
    pub fn piece_bytes(&self, hash: &Id20, idx: u32) -> Option<Bytes> {
        let weak = lock(&self.stores).get(hash).cloned()?;
        let inner = weak.upgrade()?;
        let mut g = lock(&inner);
        let out = match g.pieces.get_mut(&idx) {
            Some(PieceBuf::Full(b)) => Some(b.clone()),
            _ => None,
        };
        // touch только при попадании: защищаем играемый кусок от вытеснения
        if out.is_some() {
            g.touch(idx);
        }
        out
    }

    fn forget(&self, hash: Id20, pieces: &[u32]) {
        let weak = lock(&self.handles).get(&hash).cloned();
        if let Some(w) = weak {
            if let Some(h) = w.upgrade() {
                h.forget_pieces(pieces);
            }
        }
    }

    fn weak_self(&self) -> Weak<RamRegistry> {
        self.self_weak.get().cloned().unwrap_or_else(Weak::new)
    }
}

impl StorageFactory for RamRegistry {
    type Storage = RamTorrent;

    fn create(
        &self,
        shared: &ManagedTorrentShared,
        metadata: &TorrentMetadata,
    ) -> Result<Self::Storage> {
        // Карта кусков: индекс -> (смещение в торренте, длина). Все куски,
        // кроме последнего, имеют стандартную длину (default_piece_length).
        let pl = metadata.lengths().default_piece_length() as u64;
        let piece_map: Vec<(u32, u64, u64)> = metadata
            .lengths()
            .iter_piece_infos()
            .map(|pi| {
                let idx = pi.piece_index.get();
                (idx, idx as u64 * pl, pi.len as u64)
            })
            .collect();
        let budget = self
            .budget()
            .max(MIN_BUDGET_PIECES * pl)
            .max(1024 * 1024);
        let inner = Arc::new(Mutex::new(RamInner {
            pieces: HashMap::new(),
            touch: HashMap::new(),
            seq: 0,
            used: 0,
            budget,
            piece_map,
            file_offsets: Vec::new(),
        }));
        // регистрируем общее состояние для piece_bytes (zero-copy отдача)
        lock(&self.stores).insert(shared.info_hash, Arc::downgrade(&inner));
        Ok(RamTorrent {
            hash: shared.info_hash,
            registry: self.weak_self(),
            inner,
        })
    }

    fn clone_box(&self) -> BoxStorageFactory {
        self.clone().boxed()
    }
}

/// Хранилище одного торрента. Clone (в т.ч. через take() при паузе)
/// разделяет то же Arc<Mutex<RamInner>> — копии карты кусков нет.
#[derive(Clone)]
pub struct RamTorrent {
    hash: Id20,
    registry: Weak<RamRegistry>,
    /// Общее состояние; Clone/take разделяют его (без глубокой копии).
    inner: Arc<Mutex<RamInner>>,
}

struct RamInner {
    /// индекс куска -> данные (Partial — качается, Full — проверен и
    /// отдаваем по ссылке)
    pieces: HashMap<u32, PieceBuf>,
    /// индекс куска -> последний touch (LRU)
    touch: HashMap<u32, u64>,
    seq: u64,
    used: u64,
    budget: u64,
    /// (индекс, смещение в торренте, длина куска), по возрастанию индекса
    piece_map: Vec<(u32, u64, u64)>,
    /// смещение файла в торренте по file_id (заполняется в init)
    file_offsets: Vec<u64>,
}

impl RamInner {
    fn piece_len(&self, idx: u32) -> u64 {
        self.piece_map
            .binary_search_by_key(&idx, |e| e.0)
            .ok()
            .map(|i| self.piece_map[i].2)
            .unwrap_or(0)
    }

    fn piece_offset(&self, idx: u32) -> u64 {
        self.piece_map
            .binary_search_by_key(&idx, |e| e.0)
            .ok()
            .map(|i| self.piece_map[i].1)
            .unwrap_or(0)
    }

    fn default_pl(&self) -> u64 {
        self.piece_map.first().map(|e| e.2).unwrap_or(0)
    }

    fn max_idx(&self) -> u32 {
        self.piece_map.last().map(|e| e.0).unwrap_or(0)
    }

    fn touch(&mut self, idx: u32) {
        self.seq += 1;
        self.touch.insert(idx, self.seq);
    }

    /// Диапазон кусков [start, end], покрывающих [abs, abs+len).
    fn range(&self, abs: u64, len: u64) -> Result<std::ops::RangeInclusive<u32>> {
        let pl = self.default_pl();
        if pl == 0 {
            return Err(anyhow!("ramstore: empty piece map"));
        }
        if len == 0 {
            return Err(anyhow!("ramstore: zero-length range"));
        }
        let start = (abs / pl) as u32;
        let end = ((abs + len - 1) / pl) as u32;
        if end > self.max_idx() {
            return Err(anyhow!(
                "ramstore: range {abs}+{len} out of torrent (max piece {})",
                self.max_idx()
            ));
        }
        Ok(start..=end)
    }

    /// Вытеснить за бюджет давно не трогаемые куски. Возвращает (список
    /// вытесненных, освобождённые байты) — сброс в librqbit (forget_pieces)
    /// выполняется ПОСЛЕ отпускания мьютекса (иначе deadlock с блокировками
    /// стримера).
    fn evict(&mut self) -> (Vec<u32>, u64) {
        if self.used <= self.budget {
            return (Vec::new(), 0);
        }
        let mut victims: Vec<(u64, u32)> = self.touch.iter().map(|(k, v)| (*v, *k)).collect();
        victims.sort_unstable_by_key(|v| v.0);
        let mut forgotten: Vec<u32> = Vec::new();
        let mut freed: u64 = 0;
        for (_, id) in victims {
            if self.used <= self.budget {
                break;
            }
            if let Some(data) = self.pieces.remove(&id) {
                let len = data.len() as u64;
                self.used -= len;
                freed += len;
                self.touch.remove(&id);
                forgotten.push(id);
            }
        }
        (forgotten, freed)
    }
}

impl RamTorrent {
    fn on_pieces_added(&self, delta: u64) {
        if let Some(reg) = self.registry.upgrade() {
            reg.used_total.fetch_add(delta, Ordering::Relaxed);
        }
    }

    fn on_pieces_removed(&self, delta: u64) {
        if let Some(reg) = self.registry.upgrade() {
            reg.used_total.fetch_sub(delta, Ordering::Relaxed);
        }
    }
}

impl TorrentStorage for RamTorrent {
    fn init(
        &mut self,
        _shared: &ManagedTorrentShared,
        metadata: &TorrentMetadata,
    ) -> Result<()> {
        let mut inner = lock(&self.inner);
        inner.file_offsets = metadata
            .file_infos
            .iter()
            .map(|fi| fi.offset_in_torrent)
            .collect();
        Ok(())
    }

    fn pread_exact(&self, file_id: usize, offset: u64, buf: &mut [u8]) -> Result<()> {
        if buf.is_empty() {
            return Ok(());
        }
        let mut inner = lock(&self.inner);
        let base = *inner
            .file_offsets
            .get(file_id)
            .ok_or_else(|| anyhow!("ramstore: bad file_id {file_id}"))?;
        let abs = base + offset;
        let end = abs + buf.len() as u64;
        for idx in inner.range(abs, buf.len() as u64)? {
            let poff = inner.piece_offset(idx);
            {
                // Один поиск куска, без повторного индексирования: отсутствие
                // куска (вытеснен) = ошибка, а не паника на unwrap/индексе.
                let data = inner
                    .pieces
                    .get(&idx)
                    .ok_or_else(|| anyhow!("ramstore: piece {idx} not resident (evicted?)"))?;
                let src_buf = data.as_slice();
                let from = abs.max(poff);
                let to = end.min(poff + src_buf.len() as u64);
                let src = (from - poff) as usize;
                let dst = (from - abs) as usize;
                let n = (to - from) as usize;
                buf[dst..dst + n].copy_from_slice(&src_buf[src..src + n]);
            }
            inner.touch(idx);
        }
        Ok(())
    }

    fn pwrite_all(&self, file_id: usize, offset: u64, buf: &[u8]) -> Result<()> {
        if buf.is_empty() {
            return Ok(());
        }
        let mut added: u64 = 0;
        let forgotten: Vec<u32>;
        let freed: u64;
        {
            let mut inner = lock(&self.inner);
            let base = *inner
                .file_offsets
                .get(file_id)
                .ok_or_else(|| anyhow!("ramstore: bad file_id {file_id}"))?;
            let abs = base + offset;
            let end = abs + buf.len() as u64;
            for idx in inner.range(abs, buf.len() as u64)? {
                let poff = inner.piece_offset(idx);
                let plen = inner.piece_len(idx) as usize;
                if !inner.pieces.contains_key(&idx) {
                    inner
                        .pieces
                        .insert(idx, PieceBuf::Partial(BytesMut::zeroed(plen)));
                    inner.used += plen as u64;
                    added += plen as u64;
                }
                let Some(data) = inner.pieces.get_mut(&idx) else {
                    continue;
                };
                let from = abs.max(poff);
                let to = end.min(poff + plen as u64);
                let n = (to - from) as usize;
                let src = (from - abs) as usize;
                let dst = (from - poff) as usize;
                data.as_mut()[dst..dst + n].copy_from_slice(&buf[src..src + n]);
                inner.touch(idx);
            }
            let (f, fr) = inner.evict();
            forgotten = f;
            freed = fr;
        }
        // блокировка отпущена — можно звать librqbit (forget_pieces берёт
        // свои блокировки; держать наш мьютекс = риск deadlock со стримером)
        if added > 0 {
            self.on_pieces_added(added);
        }
        if freed > 0 {
            self.on_pieces_removed(freed);
        }
        if !forgotten.is_empty() {
            if let Some(reg) = self.registry.upgrade() {
                reg.forget(self.hash, &forgotten);
            }
        }
        Ok(())
    }

    fn pwrite_all_vectored(
        &self,
        file_id: usize,
        offset: u64,
        bufs: [IoSlice<'_>; 2],
    ) -> Result<usize> {
        // последовательная запись двух срезов (объём тот же)
        let mut offset = offset;
        let mut size = 0;
        for ioslice in bufs {
            self.pwrite_all(file_id, offset, &ioslice)?;
            offset += ioslice.len() as u64;
            size += ioslice.len();
        }
        Ok(size)
    }

    fn remove_file(&self, _file_id: usize, _filename: &Path) -> Result<()> {
        // удаление файлов торрента — данные умирают вместе с RamTorrent
        // (remove_torrent удаляет storage целиком)
        Ok(())
    }

    fn remove_directory_if_empty(&self, _path: &Path) -> Result<()> {
        Ok(())
    }

    fn ensure_file_length(&self, _file_id: usize, _length: u64) -> Result<()> {
        Ok(())
    }

    fn take(&self) -> Result<Box<dyn TorrentStorage>> {
        // пауза торрента: хранилище сохраняем — данные остаются в RAM,
        // Clone разделяет то же Arc<Mutex<RamInner>> (без глубокой копии),
        // resume продолжит работать с теми же кусками.
        Ok(Box::new(self.clone()))
    }

    fn on_piece_completed(&self, piece_index: ValidPieceIndex) -> Result<()> {
        let mut inner = lock(&self.inner);
        let idx = piece_index.get();
        if let Some(pb) = inner.pieces.get_mut(&idx) {
            // хэш проверен — замораживаем для zero-copy отдачи (piece_bytes)
            pb.freeze();
        }
        inner.touch(idx);
        Ok(())
    }
}
