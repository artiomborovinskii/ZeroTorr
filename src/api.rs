//! HTTP API MatriX.111: роуты, авторизация, стрим-насос, m3u, /download.
use crate::manager::{urldecode, Manager};
use crate::models::*;
use crate::serve;
use crate::torrent::{cache_state, Torrent};
use axum::body::Body;
use axum::extract::{Path as AxPath, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::Router;
use std::collections::HashMap;
use std::sync::atomic::Ordering;
use std::sync::Arc;

pub const VERSION: &str = "MatriX.111.zerotorr.1.4.6";

#[derive(Clone)]
pub struct App {
    pub mgr: Arc<Manager>,
    pub httpauth: bool,
    pub dontkill: bool,
    /// профиль --low-memory: скип preload, малые чанки отдачи,
    /// принудительный санитайз настроек в /settings
    pub low_memory: bool,
}

type Q = HashMap<String, String>;

pub fn router(app: App) -> Router {
    // CORS — самый внешний слой (как в оригинале, где gin-cors стоит до auth):
    // браузерная Lampa с чужого origin и preflight-запросы без пароля.
    Router::new()
        .route("/", get(ui_index))
        .route("/echo", get(echo))
        .route("/shutdown", get(shutdown))
        .route("/settings", post(settings))
        .route("/torrents", post(torrents))
        .route("/cache", post(cache))
        .route("/viewed", post(viewed))
        .route("/stream", get(stream_query).head(stream_query))
        .route("/stream/", get(stream).head(stream))
        .route("/stream/{*fname}", get(stream).head(stream))
        .route("/playlist", get(playlist))
        .route("/playlistall/all.m3u", get(playlist_all))
        .route("/playlistall", get(playlist_all))
        .route("/play/{hash}/{id}", get(play).head(play))
        .route("/download/{size}", get(download))
        .fallback(fallback_404)
        .with_state(app)
        .layer(axum::middleware::from_fn(cors_middleware))
}

fn not_found() -> Response {
    (StatusCode::NOT_FOUND, "").into_response()
}

/// CORS как в TorrServer (gin-contrib/cors, AllowAllOrigins), плюс заголовок
/// Private Network Access: Chrome/Edge с публичной страницы Lampa (lampa.one)
/// требует ответ `Access-Control-Allow-Private-Network: true` на preflight
/// к приватному IP вроде 192.168.x.x — иначе запрос блокируется.
async fn cors_middleware(req: axum::extract::Request, next: axum::middleware::Next) -> Response {
    const METHODS: &str = "GET, POST, PUT, PATCH, HEAD, OPTIONS, DELETE";
    use axum::http::HeaderValue;

    let is_preflight =
        req.method() == axum::http::Method::OPTIONS && req.headers().contains_key("access-control-request-method");

    let mut resp = if is_preflight {
        let mut r = StatusCode::NO_CONTENT.into_response();
        let h = r.headers_mut();
        h.insert("Access-Control-Allow-Origin", HeaderValue::from_static("*"));
        h.insert("Access-Control-Allow-Methods", HeaderValue::from_static(METHODS));
        // эхо запрошенных заголовков: покрывает Content-Type, Authorization и т.д.
        if let Some(v) = req.headers().get("access-control-request-headers") {
            if let Ok(v) = HeaderValue::from_bytes(v.as_bytes()) {
                h.insert("Access-Control-Allow-Headers", v);
            }
        }
        // PNA: отвечаем только если запрос его запросил (как gin-contrib)
        if req
            .headers()
            .get("access-control-request-private-network")
            .and_then(|v| v.to_str().ok())
            .map(|v| v.eq_ignore_ascii_case("true"))
            .unwrap_or(false)
        {
            h.insert(
                "Access-Control-Allow-Private-Network",
                HeaderValue::from_static("true"),
            );
        }
        h.insert("Access-Control-Max-Age", HeaderValue::from_static("43200"));
        return r;
    } else {
        next.run(req).await
    };

    let h = resp.headers_mut();
    h.insert("Access-Control-Allow-Origin", HeaderValue::from_static("*"));
    h.insert(
        "Access-Control-Expose-Headers",
        HeaderValue::from_static(
            "Accept-Ranges, Content-Range, Content-Length, Content-Type, ETag, Last-Modified, Connection",
        ),
    );
    resp
}

async fn fallback_404() -> Response {
    not_found()
}

// ---------------- авторизация ----------------

fn check_auth(app: &App, headers: &HeaderMap) -> bool {
    if !app.httpauth {
        return true;
    }
    let Some(v) = headers.get(http::header::AUTHORIZATION) else {
        return false;
    };
    let Ok(s) = v.to_str() else { return false };
    let Some(b64) = s.strip_prefix("Basic ") else { return false };
    use base64::Engine as _;
    let Ok(decoded) = base64::engine::general_purpose::STANDARD.decode(b64.trim()) else {
        return false;
    };
    let Ok(pair) = String::from_utf8(decoded) else { return false };
    let Some((user, pass)) = pair.split_once(':') else { return false };
    app.mgr.bd.accounts().get(user).map(|p| p == pass).unwrap_or(false)
}

fn unauthorized() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        [("WWW-Authenticate", "Basic realm=\"torr\"")],
        "Not authorized",
    )
        .into_response()
}

// ---------------- базовые ----------------

async fn ui_index(State(app): State<App>, headers: HeaderMap) -> Response {
    if !check_auth(&app, &headers) {
        return unauthorized();
    }
    crate::ui::index()
}

async fn echo() -> Response {
    VERSION.into_response()
}

async fn shutdown(State(app): State<App>) -> Response {
    let resp = "Shutting down".into_response();
    let app = app.clone();
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        if !app.dontkill {
            crate::request_shutdown().await;
        }
    });
    resp
}

async fn settings(State(app): State<App>, body: axum::body::Bytes) -> Response {
    let req: SettingsRequest = match parse_json(&body) {
        Some(r) => r,
        None => return bad_request("bad json"),
    };
    match req.action.as_str() {
        "get" => Json(app.mgr.sets()),
        "set" => {
            let s = app.mgr.set_sets(req.sets);
            Json(s)
        }
        "def" => {
            let s = app.mgr.set_sets(BTSets::default());
            Json(s)
        }
        _ => bad_request("unknown action"),
    }
}

async fn torrents(State(app): State<App>, body: axum::body::Bytes) -> Response {
    let req: TorrentsRequest = match parse_json(&body) {
        Some(r) => r,
        None => return bad_request("bad json"),
    };
    match req.action.as_str() {
        "add" => {
            if req.link.trim().is_empty() && req.data.trim().is_empty() {
                return bad_request("link is empty");
            }
            match app
                .mgr
                .add(&req.link, &req.title, &req.poster, &req.data, req.save_to_db)
                .await
            {
                Ok(t) => Json(t.status()),
                Err(e) => internal_error(format!("{e:#}")),
            }
        }
        "get" => match app.mgr.get(&req.hash) {
            Some(t) => Json(t.status()),
            None => match db_status(&app, &req.hash) {
                Some(st) => Json(st),
                None => bad_request("torrent not found"),
            },
        },
        "set" => {
            let Some(t) = app.mgr.get(&req.hash) else {
                return bad_request("torrent not found");
            };
            if !req.title.is_empty() {
                *t.title.write().unwrap() = req.title.clone();
            }
            if !req.poster.is_empty() {
                *t.poster.write().unwrap() = req.poster.clone();
            }
            if !req.data.is_empty() {
                // SetTorrent в MatriX.111 обновляет и Data (метаданные Lampa)
                *t.data_b64.write().unwrap() = req.data.clone();
            }
            if t.save_to_db.load(Ordering::Relaxed) == 1 {
                app.mgr.sync_db();
            }
            Json(t.status())
        }
        "rem" => match app.mgr.rem(&req.hash).await {
            Ok(_) => "removed".into_response(),
            Err(e) => internal_error(format!("{e:#}")),
        },
        "list" => Json(app.mgr.list()), // bare-массив статусов, как в MatriX.111 (Lampa читает result.length)
        "drop" => match app.mgr.drop_torrent(&req.hash).await {
            Ok(_) => "dropped".into_response(),
            Err(e) => internal_error(format!("{e:#}")),
        },
        _ => bad_request("unknown action"),
    }
}

fn db_status(app: &App, hash: &str) -> Option<TorrentStatus> {
    let h = hash.trim().to_lowercase();
    app.mgr
        .bd
        .torrents()
        .into_iter()
        .find(|i| i.hash == h)
        .map(|item| TorrentStatus {
            title: item.title,
            poster: item.poster,
            timestamp: item.timestamp,
            name: item.name,
            hash: item.hash,
            stat: STAT_INDB,
            stat_string: stat_string(STAT_INDB).to_string(),
            torrent_size: item.size,
            ..Default::default()
        })
}

async fn cache(State(app): State<App>, body: axum::body::Bytes) -> Response {
    let req: CacheRequest = match parse_json(&body) {
        Some(r) => r,
        None => return bad_request("bad json"),
    };
    if req.action != "get" {
        return bad_request("unknown action");
    }
    match app.mgr.get(&req.hash) {
        Some(t) => Json(cache_state(&t)),
        None => bad_request("torrent not found"),
    }
}

async fn viewed(State(app): State<App>, body: axum::body::Bytes) -> Response {
    let req: ViewedRequest = match parse_json(&body) {
        Some(r) => r,
        None => return bad_request("bad json"),
    };
    let hash = req.hash.trim().to_lowercase();
    match req.action.as_str() {
        "list" => {
            // ВАЖНО: Lampa фильтрует ответ ТОЛЬКО по file_index, доверяя серверу
            // фильтрацию по хэшу (как list в MatriX.111). Отдаём bare-массив.
            let list: Vec<ViewedItem> = app
                .mgr
                .bd
                .viewed_list(&hash)
                .into_iter()
                .map(|(h, idx, tc)| ViewedItem { hash: h, file_index: idx, timecode: tc })
                .collect();
            Json(list)
        }
        "set" => {
            if hash.is_empty() {
                return bad_request("hash is required");
            }
            app.mgr.mark_viewed(&hash, req.file_index, req.timecode);
            "ok".into_response()
        }
        "rem" => {
            if hash.is_empty() {
                return bad_request("hash is required");
            }
            app.mgr.bd.viewed_rem(&hash, req.file_index).ok();
            "ok".into_response()
        }
        _ => bad_request("unknown action"),
    }
}

// ---------------- /download/{size} — speedtest (Lampa) ----------------

async fn download(
    State(app): State<App>,
    AxPath(size): AxPath<String>,
    headers: HeaderMap,
) -> Response {
    if !check_auth(&app, &headers) {
        return unauthorized();
    }
    let Ok(mb) = size.parse::<u64>() else {
        return bad_request("bad size");
    };
    if mb == 0 || mb > 100_000 {
        return bad_request("bad size");
    }
    let total = mb * 1024 * 1024;
    let range = match serve::parse_range(
        headers.get(http::header::RANGE).and_then(|v| v.to_str().ok()),
        total,
    ) {
        Ok(r) => r,
        Err(_) => {
            return (
                StatusCode::RANGE_NOT_SATISFIABLE,
                [("Content-Range", format!("bytes */{total}"))],
                "",
            )
                .into_response()
        }
    };
    let mut hh = HeaderMap::new();
    let ch = serve::apply_content_headers(
        &mut hh,
        total,
        range.clone(),
        &format!("\"dl-{mb}\""),
        "application/octet-stream",
        Some(&format!("{mb}mb.bin")),
        false,
    );
    let body = zero_body(ch.range.clone());
    build_response(ch.status, hh, body)
}

/// Поток нулей (генератор тестового файла).
fn zero_body(range: Option<std::ops::Range<u64>>) -> Body {
    let (tx, rx) = tokio::sync::mpsc::channel::<Result<bytes::Bytes, std::io::Error>>(8);
    tokio::spawn(async move {
        const CHUNK: usize = 256 * 1024;
        let zeros = bytes::Bytes::from(vec![0u8; CHUNK]);
        let mut pos = range.as_ref().map(|r| r.start).unwrap_or(0);
        let end = range.as_ref().map(|r| r.end).unwrap_or(u64::MAX);
        while pos < end {
            let n = ((end - pos) as usize).min(CHUNK);
            if tx.send(Ok(zeros.slice(..n))).await.is_err() {
                break;
            }
            pos += n as u64;
        }
    });
    Body::from_stream(tokio_stream::wrappers::ReceiverStream::new(rx))
}

// ---------------- стриминг ----------------

#[derive(Debug, Default, serde::Deserialize)]
#[allow(dead_code)]
struct StreamQuery {
    #[serde(default)]
    link: String,
    #[serde(default)]
    index: String,
    #[serde(default)]
    play: String,
    #[serde(default)]
    stat: String,
    #[serde(default)]
    m3u: String,
    #[serde(default)]
    save: String,
    #[serde(default)]
    preload: String,
    #[serde(default)]
    fromlast: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    poster: String,
}

/// /stream?link=... (без имени файла)
async fn stream_query(State(app): State<App>, Query(q): Query<Q>, headers: HeaderMap) -> Response {
    stream_inner(app, String::new(), q, headers).await
}

/// /stream/*fname?link=...
async fn stream(
    State(app): State<App>,
    AxPath(fname): AxPath<String>,
    Query(q): Query<Q>,
    headers: HeaderMap,
) -> Response {
    let fname = fname.trim_start_matches('/');
    stream_inner(app, urldecode(fname), q, headers).await
}

async fn play(State(app): State<App>, AxPath(hash): AxPath<String>, AxPath(id): AxPath<String>, headers: HeaderMap) -> Response {
    let mut q = Q::new();
    q.insert("link".to_string(), hash);
    q.insert("index".to_string(), id);
    q.insert("play".to_string(), "1".to_string());
    stream_inner(app, String::new(), q, headers).await
}

async fn stream_inner(app: App, fname: String, q: Q, headers: HeaderMap) -> Response {
    let hash = q.get("link").cloned().unwrap_or_default();
    let hash = hash.trim().to_lowercase();
    let index_s = q.get("index").cloned().unwrap_or_else(|| "0".into());
    let index: i64 = index_s.parse().unwrap_or(0);
    let is_play = q.contains_key("play");
    let is_stat = q.contains_key("stat");
    let is_m3u = q.contains_key("m3u");
    let is_preload = q.contains_key("preload");
    let save = q.contains_key("save");

    // not_auth: медиароуты без авторизации работают только для уже загруженных
    let known = app.mgr.is_loaded(&hash);
    if !check_auth(&app, &headers) && !known {
        return unauthorized();
    }

    // save: добавить и сохранить в БД
    if save && !hash.is_empty() {
        if let Some(t) = app.mgr.get(&hash) {
            let title = q.get("title").cloned().unwrap_or_default();
            let poster = q.get("poster").cloned().unwrap_or_default();
            let data = t.data_b64.read().unwrap().clone();
            let _ = app.mgr.add(&t.hash_hex, &title, &poster, &data, true).await;
        }
    }

    // torrent: из памяти, иначе загрузить (нужен полный magnet/ссылка)
    let t = match app.mgr.get(&hash) {
        Some(t) => t,
        None => {
            if is_play || hash.len() == 40 {
                // /play/{hash}/{id} или голый хэш: восстановить из БД
                match restore_from_db(&app, &hash).await {
                    Ok(t) => t,
                    Err(e) => return bad_request(format!("torrent not loaded: {e:#}")),
                }
            } else {
                match app.mgr.add(&hash, "", "", "", false).await {
                    Ok(t) => t,
                    Err(e) => return bad_request(format!("cannot add: {e:#}")),
                }
            }
        }
    };
    t.touch();

    // stat: состояние кэша
    if is_stat {
        return Json(cache_state(&t));
    }

    // m3u: плейлист торрента
    if is_m3u {
        return m3u_response(&app, &t, &host_of(&headers));
    }

    // файл
    let files = t.file_list();
    if files.is_empty() {
        return bad_request("no metadata yet");
    }
    let fi = match t.resolve_file(index) {
        Ok(f) => f,
        Err(e) => return bad_request(format!("{e:#}")),
    };

    if is_preload {
        if app.low_memory {
            // в low-memory скипаем явный префетч: голова+хвост вытеснили бы
            // весь маленький кэш; данные придут при самом стриминге
            t.preload.done.store(2, Ordering::Relaxed);
        } else {
            app.mgr.start_preload(t.clone(), fi.id);
            // ждём окончания префетча (до 20с), как оригинал
            for _ in 0..100 {
                if t.preload.done.load(Ordering::Relaxed) != 1 {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            }
        }
    }

    if is_play {
        app.mgr.mark_viewed(&t.hash_hex, fi.id as i64, 0.0);
    }

    serve_file(&app, &t, &fi, fname, &headers).await
}

/// Восстановить торрент из БД (по data) и загрузить.
async fn restore_from_db(app: &App, hash: &str) -> Result<Arc<Torrent>, anyhow::Error> {
    let h = hash.trim().to_lowercase();
    let item = app
        .mgr
        .bd
        .torrents()
        .into_iter()
        .find(|i| i.hash == h)
        .ok_or_else(|| anyhow::anyhow!("not in db"))?;
    app.mgr
        .add(&item.hash, &item.title, &item.poster, &item.data, true)
        .await
}

fn guess_mime(fname: &str, path: &str) -> &'static str {
    if !fname.is_empty() {
        return crate::mime::by_extension(&crate::mime::ext_of(fname));
    }
    crate::mime::by_extension(&crate::mime::ext_of(path))
}

/// Отдача файла: Range/206, ETag, DLNA, кэш-насос.
async fn serve_file(
    app: &App,
    t: &Arc<Torrent>,
    fi: &crate::torrent::FileInfo,
    fname: String,
    headers: &HeaderMap,
) -> Response {
    let file_len = fi.len.max(0) as u64;
    if file_len == 0 {
        return bad_request("empty file");
    }
    let mime = guess_mime(&fname, &fi.path);
    let etag = crate::torrent::file_etag(t, fi.id, file_len);

    // If-None-Match -> 304
    if let Some(inm) = headers.get(http::header::IF_NONE_MATCH).and_then(|v| v.to_str().ok()) {
        if inm.split(',').any(|s| s.trim() == etag || s.trim() == "*") {
            return (StatusCode::NOT_MODIFIED, "").into_response();
        }
    }

    let range = match serve::parse_range(
        headers.get(http::header::RANGE).and_then(|v| v.to_str().ok()),
        file_len,
    ) {
        Ok(r) => r,
        Err(_) => {
            return (
                StatusCode::RANGE_NOT_SATISFIABLE,
                [("Content-Range", format!("bytes */{file_len}"))],
                "",
            )
                .into_response()
        }
    };

    let mut hh = HeaderMap::new();
    let dlna = app.mgr.sets().EnableDLNA
        || headers
            .get("transferMode.dlna.org")
            .map(|v| v == "Streaming")
            .unwrap_or(false);
    let ch = serve::apply_content_headers(
        &mut hh,
        file_len,
        range.clone(),
        &etag,
        mime,
        None,
        dlna,
    );

    let cache = t.cache.clone();
    let t2 = t.clone();
    let fi2 = fi.clone();
    let reader_id = cache.add_reader(ch.range.as_ref().map(|r| r.start).unwrap_or(0), file_len);
    t.readers.fetch_add(1, Ordering::Relaxed);

    // размер чанка отдачи: 128 КиБ (64 КиБ в low-memory) — диапазон из
    // код-ревью «64–128 КБ»: и не микро-чанки с оверхедом, и не мегабайтные
    // буферы в полёте. Срезы Bytes — нулевое копирование поверх resident-куска.
    let serve_chunk = if app.low_memory { 64 * 1024 } else { 128 * 1024 };
    let body = Body::from_stream(async_stream_body(
        t2, cache, fi2, reader_id, ch.range.clone(), serve_chunk,
    ));
    build_response(ch.status, hh, body)
}

/// Насос: читает из кэша (промахи — через FileStream), отдаёт чанки в HTTP.
fn async_stream_body(
    t: Arc<Torrent>,
    cache: Arc<crate::cache::RingCache>,
    fi: crate::torrent::FileInfo,
    reader_id: u64,
    range: Option<std::ops::Range<u64>>,
    serve_chunk: usize,
) -> impl futures::Stream<Item = Result<bytes::Bytes, std::io::Error>> + Send + 'static {
    let (tx, rx) = tokio::sync::mpsc::channel::<Result<bytes::Bytes, std::io::Error>>(4);
    tokio::spawn(async move {
        let res = pump(&t, &cache, &fi, reader_id, range, &tx, serve_chunk).await;
        if let Err(e) = res {
            let _ = tx
                .send(Err(std::io::Error::other(format!("{e:#}"))))
                .await;
        }
    });
    tokio_stream::wrappers::ReceiverStream::new(rx)
}

async fn pump(
    t: &Arc<Torrent>,
    cache: &Arc<crate::cache::RingCache>,
    fi: &crate::torrent::FileInfo,
    reader_id: u64,
    range: Option<std::ops::Range<u64>>,
    tx: &tokio::sync::mpsc::Sender<Result<bytes::Bytes, std::io::Error>>,
    serve_chunk: usize,
) -> anyhow::Result<()> {
    let _guard = ReaderGuard::new(t.clone(), cache.clone(), reader_id);
    let start = range.as_ref().map(|r| r.start).unwrap_or(0);
    let end = range.as_ref().map(|r| r.end).unwrap_or(fi.len as u64);
    if end <= start || end > fi.len as u64 {
        anyhow::bail!("bad range");
    }
    let pl = cache.piece_len().max(1) as u64;
    let mut stream = t.file_stream(fi.orig).await?;
    let mut pos = start;
    // ленивый буфер чтения куска: аллоцируется (piece_len) только при первом
    // промахе кэша и переиспользуется — полностью закэшированный стрим его
    // вообще не аллоцирует
    let mut buf: Option<Vec<u8>> = None;

    // readahead-подготовка: первые куски отдаём как есть, движок тянет параллельно
    while pos < end {
        let piece = (pos / pl) as u32;
        let in_piece = (pos % pl) as usize;
        let total_len = t.torrent_size().max(0) as u64;
        let piece_abs = piece as u64 * pl;
        let piece_len_actual = (total_len.saturating_sub(piece_abs)).min(pl) as usize;
        let take = ((end - pos) as usize).min(piece_len_actual.saturating_sub(in_piece).max(0));
        if take == 0 {
            break;
        }
        let chunk: bytes::Bytes = match cache.read_piece_part(piece, in_piece, take) {
            Some(c) => c, // срез Bytes — без копирования
            None => {
                cache.reads_engine.fetch_add(1, Ordering::Relaxed);
                let abs = piece_abs;
                tokio::io::AsyncSeekExt::seek(&mut *stream, std::io::SeekFrom::Start(abs))
                    .await
                    .map_err(|e| anyhow::anyhow!("seek: {e}"))?;
                let buf = buf.get_or_insert_with(|| vec![0u8; pl as usize]);
                let n = tokio::io::AsyncReadExt::read_exact(&mut *stream, &mut buf[..piece_len_actual])
                    .await
                    .map_err(|e| anyhow::anyhow!("piece read: {e}"))?;
                // RAM-режим: кусок resident — отдаём срез общего буфера
                // RamTorrent, RingCache индексирует его же (Bytes = счётчик
                // ссылок): физическая копия в RAM одна (v1.4.6). Дисковый
                // путь — как раньше: копия куска в кэш.
                match crate::torrent::ram_piece(t, piece) {
                    Some(b) => {
                        cache.put_piece(piece, b.clone());
                        b.slice(in_piece..(in_piece + take).min(b.len()))
                    }
                    None => {
                        let data = bytes::Bytes::copy_from_slice(&buf[..n]);
                        cache.put_piece(piece, data.clone());
                        data.slice(in_piece..(in_piece + take).min(n))
                    }
                }
            }
        };
        let n = chunk.len();
        if n == 0 {
            anyhow::bail!("no data");
        }
        pos += n as u64;
        cache.update_reader(reader_id, pos);
        t.touch();
        // отдаём ограниченными чанками (нулевое копирование — срезы одного Bytes):
        // сглаживает backpressure и держит минимальный след на слабых устройствах
        let mut off = 0usize;
        loop {
            let l = (n - off).min(serve_chunk);
            tokio::task::yield_now().await;
            if tx.send(Ok(chunk.slice(off..off + l))).await.is_err() {
                return Ok(()); // клиент отключился
            }
            off += l;
            if off >= n {
                break;
            }
        }
    }
    Ok(())
}

struct ReaderGuard {
    t: Arc<Torrent>,
    cache: Arc<crate::cache::RingCache>,
    id: u64,
}

impl ReaderGuard {
    fn new(t: Arc<Torrent>, cache: Arc<crate::cache::RingCache>, id: u64) -> Self {
        Self { t, cache, id }
    }
}

impl Drop for ReaderGuard {
    fn drop(&mut self) {
        self.cache.remove_reader(self.id);
        self.t.readers.fetch_sub(1, Ordering::Relaxed);
    }
}

// ---------------- m3u ----------------

fn m3u_response(_app: &App, t: &Arc<Torrent>, host: &str) -> Response {
    let files = t.file_list();
    if files.is_empty() {
        return bad_request("no metadata yet");
    }
    let host = if host.is_empty() { "127.0.0.1:8090" } else { host };
    let mut out = String::from("#EXTM3U\n");
    for f in files {
        if !crate::mime::is_media(&f.path) {
            continue;
        }
        out.push_str(&format!(
            "#EXTINF:-1,{}\n",
            f.path.rsplit('/').next().unwrap_or(&f.path)
        ));
        out.push_str(&format!(
            "http://{}/stream/{}?link={}&index={}&play\n",
            host,
            urlenc_filename(&f.path),
            t.hash_hex,
            f.id
        ));
    }
    axum::response::Response::builder()
        .header(http::header::CONTENT_TYPE, "audio/x-mpegurl")
        .body(axum::body::Body::from(out))
        .unwrap_or_default()
}

async fn playlist(State(app): State<App>, Query(q): Query<Q>, headers: HeaderMap) -> Response {
    let hash = q.get("hash").cloned().unwrap_or_default();
    let hash = hash.trim().to_lowercase();
    let known = app.mgr.is_loaded(&hash);
    if !check_auth(&app, &headers) && !known {
        return unauthorized();
    }
    let t = match app.mgr.get(&hash) {
        Some(t) => t,
        None => match restore_from_db(&app, &hash).await {
            Ok(t) => t,
            Err(e) => return bad_request(format!("torrent not found: {e:#}")),
        },
    };
    t.touch();
    m3u_response(&app, &t, &host_of(&headers))
}

async fn playlist_all(State(app): State<App>, headers: HeaderMap) -> Response {
    if !check_auth(&app, &headers) {
        return unauthorized();
    }
    let host = host_of(&headers);
    let mut out = String::from("#EXTM3U\n");
    for ts in app.mgr.list() {
        if ts.stat == STAT_INDB {
            continue;
        }
        let Some(t) = app.mgr.get(&ts.hash) else { continue };
        for f in t.file_list() {
            if !crate::mime::is_media(&f.path) {
                continue;
            }
            out.push_str(&format!("#EXTINF:-1,{}\n", f.path));
            out.push_str(&format!(
                "http://{}/stream/{}?link={}&index={}&play\n",
                host,
                urlenc_filename(&f.path),
                t.hash_hex,
                f.id
            ));
        }
    }
    axum::response::Response::builder()
        .header(http::header::CONTENT_TYPE, "audio/x-mpegurl")
        .body(axum::body::Body::from(out))
        .unwrap_or_default()
}

fn urlenc_filename(path: &str) -> String {
    let name = path.rsplit('/').next().unwrap_or(path);
    percent_encoding::utf8_percent_encode(name, percent_encoding::NON_ALPHANUMERIC).to_string()
}

// ---------------- утилиты ----------------

fn host_of(headers: &HeaderMap) -> String {
    headers
        .get(http::header::HOST)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string()
}

fn parse_json<T: serde::de::DeserializeOwned>(body: &[u8]) -> Option<T> {
    if body.is_empty() {
        return Some(serde_json::from_str("{}").ok()?);
    }
    serde_json::from_slice(body).ok()
}

#[allow(non_snake_case)]
fn Json<T: serde::Serialize>(v: T) -> Response {
    ([(http::header::CONTENT_TYPE, "application/json")], serde_json::to_string(&v).unwrap_or_default()).into_response()
}

fn bad_request(msg: impl Into<String>) -> Response {
    (StatusCode::BAD_REQUEST, msg.into()).into_response()
}

fn internal_error(msg: impl Into<String>) -> Response {
    (StatusCode::INTERNAL_SERVER_ERROR, msg.into()).into_response()
}

fn build_response(status: u16, headers: HeaderMap, body: Body) -> Response {
    let mut rb = Response::builder().status(StatusCode::from_u16(status).unwrap_or(StatusCode::OK));
    for (k, v) in headers.iter() {
        rb = rb.header(k, v);
    }
    rb.body(body).unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}
