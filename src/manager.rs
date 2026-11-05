//! Менеджер: парсинг ссылок, добавление/загрузка/закрытие торрентов,
//! префетч (start+tail), watcher'ы (статусы, таймауты), БД.
use crate::bd::{Bd, TorrentDbItem};
use crate::engine::Engine;
use crate::models::*;
use crate::torrent::{now_sec, ReadSeek, Torrent};
use anyhow::{anyhow, bail, Context, Result};
use librqbit_core::Id20;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use tokio::sync::Mutex as AsyncMutex;

pub enum LinkSpec {
    Magnet(String),
    Url(String),
    File(PathBuf),
}

pub fn parse_link(link: &str) -> Result<LinkSpec> {
    let link = link.trim();
    if link.is_empty() {
        bail!("link is empty");
    }
    let lower = link.to_ascii_lowercase();
    if lower.starts_with("magnet:") {
        return Ok(LinkSpec::Magnet(link.to_string()));
    }
    if lower.starts_with("http://") || lower.starts_with("https://") {
        return Ok(LinkSpec::Url(link.to_string()));
    }
    if lower.starts_with("file://") {
        let p = &link["file://".len()..];
        let p = urldecode(p);
        return Ok(LinkSpec::File(PathBuf::from(p)));
    }
    // голый хэш (40 hex) -> magnet
    if link.len() == 40 && link.chars().all(|c| c.is_ascii_hexdigit()) {
        return Ok(LinkSpec::Magnet(format!(
            "magnet:?xt=urn:btih:{}",
            link.to_lowercase()
        )));
    }
    // локальный путь
    let p = PathBuf::from(link);
    if p.exists() {
        return Ok(LinkSpec::File(p));
    }
    bail!("unknown link format")
}

pub fn urldecode(s: &str) -> String {
    percent_encoding::percent_decode_str(s)
        .decode_utf8_lossy()
        .to_string()
}

pub struct Manager {
    pub engine: Arc<Engine>,
    pub bd: Arc<Bd>,
    pub sets: std::sync::RwLock<BTSets>,
    pub dir: PathBuf,
    /// профиль --low-memory: принудительно применять sanitize_low_memory в set_sets
    pub low_memory: bool,
    /// загруженные торренты: hash_hex -> Torrent
    pub loaded: std::sync::Mutex<HashMap<String, Arc<Torrent>>>,
    /// защита от двойного добавления одного хэша
    adding: AsyncMutex<()>,
}

impl Manager {
    pub async fn new(
        dir: PathBuf,
        sets: BTSets,
        readonly: bool,
        low_memory: bool,
        ram: Option<Arc<crate::ramstore::RamRegistry>>,
    ) -> Result<Arc<Self>> {
        let bd = Arc::new(Bd::new(&dir, readonly));
        let engine = Arc::new(Engine::new(dir.join("cache"), &sets, ram, low_memory).await?);
        Ok(Arc::new(Self {
            engine,
            bd,
            sets: std::sync::RwLock::new(sets),
            dir,
            low_memory,
            loaded: std::sync::Mutex::new(HashMap::new()),
            adding: AsyncMutex::new(()),
        }))
    }

    pub fn sets(&self) -> BTSets {
        self.sets.read().unwrap().clone()
    }

    pub fn set_sets(&self, mut s: BTSets) -> BTSets {
        s.sanitize();
        if self.low_memory {
            s.sanitize_low_memory(); // флаг live: настройки из API не поднимут кэш сверх лимита
        }
        *self.sets.write().unwrap() = s.clone();
        let _ = self.bd.settings_save(&s);
        s
    }

    pub fn get(&self, hash: &str) -> Option<Arc<Torrent>> {
        let h = hash.trim().to_lowercase();
        self.loaded.lock().unwrap().get(&h).cloned()
    }

    /// Загружен ли торрент (для not_auth семантики медиароутов).
    pub fn is_loaded(&self, hash: &str) -> bool {
        self.get(hash).is_some()
    }

    /// Список статусов: загруженные + из БД (stat=5).
    pub fn list(&self) -> Vec<TorrentStatus> {
        let mut out = Vec::new();
        let mut seen: HashMap<String, ()> = HashMap::new();
        {
            let g = self.loaded.lock().unwrap();
            for t in g.values() {
                out.push(t.status());
                seen.insert(t.hash_hex.clone(), ());
            }
        }
        for item in self.bd.torrents() {
            if !seen.contains_key(&item.hash) {
                out.push(TorrentStatus {
                    title: item.title,
                    poster: item.poster,
                    timestamp: item.timestamp,
                    name: item.name,
                    hash: item.hash,
                    stat: STAT_INDB,
                    stat_string: stat_string(STAT_INDB).to_string(),
                    torrent_size: item.size,
                    ..Default::default()
                });
            }
        }
        out.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
        out
    }

    /// Добавить торрент. Возвращает статус.
    pub async fn add(
        self: &Arc<Self>,
        link: &str,
        title: &str,
        poster: &str,
        data_b64: &str,
        save_to_db: bool,
    ) -> Result<Arc<Torrent>> {
        let sets = self.sets();
        // 1. источник .torrent — семантика MatriX.111 (addTorrent):
        //    link главнее data. Lampa в add ВСЕГДА шлёт link=magnet и
        //    data={JSON-метаданные фильма} (poster/overview/tmdb-id) — это
        //    НЕ base64-.torrent, а строка-метаданные: храним как есть и эхом
        //    возвращаем в status()/list() (Lampa использует её для карточки
        //    на других устройствах). Base64-.torrent в data ожидаем только
        //    при пустом link (совместимость с прочими клиентами API);
        //    для .torrent-файлов есть /torrent/upload, как в оригинале.
        let mut torrent_bytes: Option<Vec<u8>> = None;
        let mut magnet: Option<String> = None;
        let link_t = link.trim();
        if !link_t.is_empty() {
            match parse_link(link_t)? {
                LinkSpec::Magnet(m) => magnet = Some(m),
                LinkSpec::Url(u) => {
                    torrent_bytes = Some(fetch_torrent(&u).await?);
                }
                LinkSpec::File(p) => {
                    torrent_bytes = Some(std::fs::read(&p).context("read torrent file")?);
                }
            }
        } else if !data_b64.trim().is_empty() {
            use base64::Engine as _;
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(data_b64.trim())
                .context("bad base64 data")?;
            torrent_bytes = Some(bytes);
        }

        // 2. хэш: из байтов или из magnet
        let (hash_hex, engine_link): (String, Option<String>) = if let Some(b) = &torrent_bytes {
            let meta =
                librqbit_core::torrent_metainfo::torrent_from_bytes(b).context("bad torrent")?;
            (hex_of_id20(&meta.info_hash), magnet)
        } else if let Some(m) = magnet {
            let mp = librqbit_core::magnet::Magnet::parse(&m).context("bad magnet")?;
            let id = mp.as_id20().context("magnet has no btih v1 hash")?;
            (hex_of_id20(&id), Some(m))
        } else {
            bail!("cannot determine hash: link is empty and data is not a base64 .torrent")
        };

        // 3. защита от гонки добавления
        let _guard = self.adding.lock().await;

        // 4. уже загружен? Обновляем метаданные карточки (Lampa пере-добавляет
        //    тот же magnet при каждом открытии фильма — с актуальными title/poster/data).
        if let Some(t) = self.get(&hash_hex) {
            t.touch();
            if !title.is_empty() {
                *t.title.write().unwrap() = title.to_string();
            }
            if !poster.is_empty() {
                *t.poster.write().unwrap() = poster.to_string();
            }
            if !data_b64.is_empty() {
                *t.data_b64.write().unwrap() = data_b64.to_string();
            }
            if save_to_db {
                self.persist(&t, data_b64).ok();
            }
            return Ok(t);
        }

        // 5. добавление в движок
        let handle = self
            .engine
            .add(
                engine_link.as_deref(),
                torrent_bytes,
                &hash_hex,
                &sets,
                &[],
            )
            .await
            .context("engine add")?;

        let t = Torrent::new(hash_hex.clone(), handle, &sets, self.engine.ram.clone());
        *t.title.write().unwrap() = title.to_string();
        *t.poster.write().unwrap() = poster.to_string();
        *t.data_b64.write().unwrap() = data_b64.to_string();
        if save_to_db {
            t.save_to_db.store(1, Ordering::Relaxed);
        }
        t.stat.store(STAT_GETINFO, Ordering::Relaxed);

        {
            let mut g = self.loaded.lock().unwrap();
            g.insert(hash_hex.clone(), t.clone());
        }

        // 6. фоновый watcher
        let weak_t = Arc::downgrade(&t);
        let weak_m = Arc::downgrade(self);
        tokio::spawn(async move {
            watcher(weak_m, weak_t).await;
        });

        if save_to_db {
            self.persist(&t, data_b64).ok();
        }
        Ok(t)
    }

    fn persist(&self, t: &Arc<Torrent>, data_b64: &str) -> Result<()> {
        let mut items: Vec<TorrentDbItem> = self
            .bd
            .torrents()
            .into_iter()
            .filter(|i| i.hash != t.hash_hex)
            .collect();
        items.push(TorrentDbItem {
            title: t.title.read().unwrap().clone(),
            poster: t.poster.read().unwrap().clone(),
            data: if data_b64.is_empty() {
                t.data_b64.read().unwrap().clone()
            } else {
                data_b64.to_string()
            },
            size: t.torrent_size(),
            timestamp: now_sec(),
            hash: t.hash_hex.clone(),
            name: t.name.read().unwrap().clone(),
        });
        self.bd.torrents_save(&items)
    }

    /// rem: удалить из БД и из памяти (+файлы если RemoveCacheOnDrop/!UseDisk).
    pub async fn rem(&self, hash: &str) -> Result<()> {
        let h = hash.trim().to_lowercase();
        let items: Vec<TorrentDbItem> = self
            .bd
            .torrents()
            .into_iter()
            .filter(|i| i.hash != h)
            .collect();
        self.bd.torrents_save(&items)?;
        self.drop_torrent(&h).await
    }

    /// drop: выгрузить из памяти (БД не трогаем).
    pub async fn drop_torrent(&self, hash: &str) -> Result<()> {
        let h = hash.trim().to_lowercase();
        let t = {
            let mut g = self.loaded.lock().unwrap();
            g.remove(&h)
        };
        let sets = self.sets();
        if let Some(t) = t {
            t.stat.store(STAT_CLOSED, Ordering::Relaxed);
            let delete_files = !sets.UseDisk || sets.RemoveCacheOnDrop;
            if let Ok(id) = crate::engine::id20_from_hex(&h) {
                self.engine.remove(id, delete_files).await.ok();
            }
            t.cache.clear();
        }
        Ok(())
    }

    /// Выгрузка всех торрентов (при shutdown).
    pub async fn close_all(&self) {
        self.sync_db();
        let hashes: Vec<String> = {
            let g = self.loaded.lock().unwrap();
            g.keys().cloned().collect()
        };
        for h in hashes {
            self.drop_torrent(&h).await.ok();
        }
    }

    /// Синхронизировать БД с текущим состоянием.
    pub fn sync_db(&self) {
        let g = self.loaded.lock().unwrap();
        for t in g.values() {
            if t.save_to_db.load(Ordering::Relaxed) == 1 {
                let _ = self.persist(t, "");
            }
        }
    }

    /// Отметить просмотренным (с таймкодом для «продолжить просмотр» Lampa).
    pub fn mark_viewed(&self, hash: &str, file_index: i64, timecode: f64) {
        self.bd.viewed_set(hash, file_index, timecode).ok();
    }

    /// Запустить префетч файла (начало + хвост для moov-атома mp4).
    pub fn start_preload(self: &Arc<Self>, t: Arc<Torrent>, file_id: usize) {
        if t.preload.done.load(Ordering::Relaxed) == 1 {
            return; // уже идёт
        }
        if t.preload.done.load(Ordering::Relaxed) == 2 {
            return; // уже готово
        }
        let sets = self.sets();
        let Ok(fi) = t.resolve_file(file_id as i64) else {
            return;
        };
        let file_len = fi.len.max(0) as u64;
        if file_len == 0 {
            return;
        }
        // размер префетча: доля PreloadCache% от CacheSize, не больше файла
        let ps = ((sets.CacheSize.max(0) as u64) * (sets.PreloadCache.max(0) as u64) / 100)
            .min(file_len);
        let mb5 = (t.cache.piece_len().max(8 * 1024 * 1024) as u64).min(16 * 1024 * 1024);
        t.preload.preload_size.store(ps as i64, Ordering::Relaxed);
        t.preload.done.store(1, Ordering::Relaxed);
        t.stat.store(STAT_PRELOAD, Ordering::Relaxed);

        let weak_t = Arc::downgrade(&t);
        tokio::spawn(async move {
            let res = preload_run(&weak_t, fi.orig, file_len, ps, mb5).await;
            if let Some(t) = weak_t.upgrade() {
                t.preload.done.store(2, Ordering::Relaxed);
                if t.stat.load(Ordering::Relaxed) == STAT_PRELOAD {
                    t.stat.store(STAT_WORKING, Ordering::Relaxed);
                }
                if res.is_err() {
                    t.preload.preloaded_bytes.store(0, Ordering::Relaxed);
                }
            }
        });
    }
}

async fn preload_run(
    weak_t: &std::sync::Weak<Torrent>,
    orig: usize,
    file_len: u64,
    ps: u64,
    mb5: u64,
) -> Result<()> {
    let Some(t) = weak_t.upgrade() else {
        return Ok(());
    };
    if t.cache.piece_len() == 0 {
        bail!("no piece length");
    }
    let mut stream = t.file_stream(orig).await?;
    let mut loaded: u64 = 0;
    // голова [0, ps - mb5)
    let head_end = ps.saturating_sub(mb5).min(file_len);
    if head_end > 0 {
        loaded += cache_copy_range(&t, &mut *stream, 0, head_end).await?;
    }
    // хвост [file_len - mb5, file_len), не пересекаясь с головой
    if file_len > mb5 {
        let tail_start = (file_len - mb5).max(head_end);
        if tail_start < file_len {
            let n = cache_copy_range(&t, &mut *stream, tail_start, file_len).await?;
            let _ = n;
        }
    }
    t.preload
        .preloaded_bytes
        .store(loaded as i64, Ordering::Relaxed);
    Ok(())
}

/// Прочитать диапазон файла [from, to) через кэш (промахи — через FileStream).
async fn cache_copy_range<S: ReadSeek + ?Sized + Unpin>(
    t: &Arc<Torrent>,
    stream: &mut S,
    from: u64,
    to: u64,
) -> Result<u64> {
    let pl = t.cache.piece_len() as u64;
    if pl == 0 {
        bail!("no piece length");
    }
    let mut pos = from;
    let mut buf: Option<Vec<u8>> = None; // ленивый буфер куска
    while pos < to {
        let piece = (pos / pl) as u32;
        let in_piece = (pos % pl) as usize;
        let piece_len_actual = {
            let total = t.torrent_size().max(0) as u64;
            let abs = piece as u64 * pl;
            (total.saturating_sub(abs)).min(pl) as usize
        };
        let take = ((to - pos) as usize).min(piece_len_actual.saturating_sub(in_piece));
        if take == 0 {
            break;
        }
        let chunk_hit = t.cache.read_piece_part(piece, in_piece, take).is_some();
        if !chunk_hit {
            // читаем кусок целиком из FileStream
            let abs = piece as u64 * pl;
            stream
                .seek(std::io::SeekFrom::Start(abs))
                .await
                .context("seek")?;
            let buf = buf.get_or_insert_with(|| vec![0u8; pl as usize]);
            let n = stream
                .read_exact(&mut buf[..piece_len_actual])
                .await
                .context("piece read")?;
            t.cache.reads_engine.fetch_add(1, Ordering::Relaxed);
            // RAM-режим: кусок resident — индексируем общий буфер RamTorrent
            // (Bytes::clone = счётчик ссылок, без копии); дисковый путь — как
            // раньше, копия в кэш.
            match crate::torrent::ram_piece(t, piece) {
                Some(b) => t.cache.put_piece(piece, b),
                None => t
                    .cache
                    .put_piece(piece, bytes::Bytes::copy_from_slice(&buf[..n])),
            }
        }
        pos += take as u64;
    }
    Ok(pos - from)
}

/// Фоновый watcher торрента: метаданные, статус, автозакрытие по таймауту.
async fn watcher(weak_m: std::sync::Weak<Manager>, weak_t: std::sync::Weak<Torrent>) {
    loop {
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        let (Some(m), Some(t)) = (weak_m.upgrade(), weak_t.upgrade()) else {
            return;
        };
        // метаданные
        if t.file_list().is_empty() {
            if t.refresh_metadata() && t.stat.load(Ordering::Relaxed) == STAT_GETINFO {
                t.stat.store(STAT_WORKING, Ordering::Relaxed);
            }
        } else if t.stat.load(Ordering::Relaxed) == STAT_GETINFO {
            t.stat.store(STAT_WORKING, Ordering::Relaxed);
        }

        // таймаут бездействия: нет читателей, префетч не идёт, метаданные есть
        let sets = m.sets();
        let timeout = sets.TorrentDisconnectTimeout.max(10);
        if t.file_list().is_empty()
            || t.readers.load(Ordering::Relaxed) > 0
            || t.preload.done.load(Ordering::Relaxed) == 1
        {
            continue;
        }
        if t.idle_secs() > timeout {
            let h = t.hash_hex.clone();
            let m2 = m.clone();
            tokio::spawn(async move {
                m2.drop_torrent(&h).await.ok();
            });
            return;
        }
    }
}

fn hex_of_id20(id: &Id20) -> String {
    id.0.iter().map(|x| format!("{x:02x}")).collect()
}

/// HTTP(S)-клиент с каскадом корней CA (как в vendored librqbit): системные
/// (rustls-platform-verifier) → встроенные webpki-roots. Termux/BusyBox/
/// минимальные rootfs не имеют системных CA — без каскада клиент не строится.
fn http_client() -> &'static reqwest::Client {
    static CLIENT: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
    CLIENT.get_or_init(|| {
        let mk = |webpki: bool| -> Result<reqwest::ClientBuilder> {
            let b = reqwest::Client::builder()
                .user_agent(concat!("zerotorr/", env!("CARGO_PKG_VERSION")))
                .connect_timeout(std::time::Duration::from_secs(10))
                .timeout(std::time::Duration::from_secs(30));
            if !webpki {
                return Ok(b);
            }
            let mut roots = rustls::RootCertStore::empty();
            roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
            let config = rustls::ClientConfig::builder()
                .with_root_certificates(roots)
                .with_no_client_auth();
            Ok(b.tls_backend_preconfigured(config))
        };
        if let Ok(c) = mk(false).and_then(|b| Ok(b.build()?)) {
            return c;
        }
        tracing::warn!("no system CA certificates, using built-in webpki roots");
        mk(true)
            .and_then(|b| Ok(b.build()?))
            .expect("reqwest: webpki-roots client build failed (TLS backend broken)")
    })
}

/// Скачивание .torrent по URL. Один сетевой стек: async reqwest от librqbit
/// (до 1.4.2 был sync-клиент ureq со своей копией TLS и корней — удалён).
pub async fn fetch_torrent(url: &str) -> Result<Vec<u8>> {
    let resp = http_client()
        .get(url)
        .send()
        .await
        .map_err(|e| anyhow!("torrent url fetch: {e}"))?;
    let st = resp.status();
    if !st.is_success() {
        anyhow::bail!("torrent url fetch: http {st}");
    }
    let bytes = resp
        .bytes()
        .await
        .map_err(|e| anyhow!("torrent url read: {e}"))?;
    if bytes.len() > 16 * 1024 * 1024 {
        anyhow::bail!("torrent url: too big ({} bytes)", bytes.len());
    }
    Ok(bytes.to_vec())
}
