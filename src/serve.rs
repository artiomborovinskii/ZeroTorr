//! Разбор Range и заголовки ответа — аналог Go http.ServeContent.
use std::ops::Range;

/// Разбор заголовка Range: bytes=a-b (одиночный диапазон; multipart отдаём целиком).
/// Возвращает Some(диапазон) или None (полный файл).
pub fn parse_range(header: Option<&str>, size: u64) -> Result<Option<Range<u64>>, u64> {
    let h = match header {
        Some(h) => h,
        None => return Ok(None),
    };
    let h = h.trim();
    let spec = match h.strip_prefix("bytes=") {
        Some(s) => s,
        None => return Ok(None), // непонятный range — игнорируем, как Go
    };
    // multipart (запятые) не поддерживаем — отдаём целиком
    if spec.contains(',') {
        return Ok(None);
    }
    let (a, b) = match spec.split_once('-') {
        Some(x) => x,
        None => return Ok(None),
    };
    if size == 0 {
        return Err(0);
    }
    let range = if a.is_empty() {
        // суффикс: последние N байт
        let n: u64 = b.parse().map_err(|_| size)?;
        if n == 0 {
            return Err(size);
        }
        (size.saturating_sub(n))..size
    } else {
        let start: u64 = a.parse().map_err(|_| size)?;
        let end = if b.is_empty() {
            size - 1
        } else {
            b.parse::<u64>().map_err(|_| size)?.min(size - 1)
        };
        if start > end || start >= size {
            return Err(size);
        }
        start..end + 1
    };
    Ok(Some(range))
}

/// Значение ETag.
pub fn etag_for(hash: &str, file_id: usize, size: u64) -> String {
    format!("\"{hash}-{file_id:x}-{size:x}\"")
}

/// Полный набор заголовков контента (как http.ServeContent + DLNA).
pub struct ContentHeaders {
    pub status: u16, // 200 | 206 | 304
    pub range: Option<Range<u64>>,
}

pub fn apply_content_headers(
    headers: &mut http::HeaderMap,
    size: u64,
    range: Option<Range<u64>>,
    etag: &str,
    mime: &str,
    fname: Option<&str>,
    dlna: bool,
) -> ContentHeaders {
    headers.insert(
        http::header::ACCEPT_RANGES,
        http::HeaderValue::from_static("bytes"),
    );
    if let Ok(v) = http::HeaderValue::from_str(mime) {
        headers.insert(http::header::CONTENT_TYPE, v);
    }
    if let Ok(v) = http::HeaderValue::from_str(etag) {
        headers.insert(http::header::ETAG, v);
    }
    headers.insert(
        "X-Content-Type-Options",
        http::HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        http::header::CONNECTION,
        http::HeaderValue::from_static("close"),
    );
    if let Some(f) = fname {
        if let Ok(v) = http::HeaderValue::from_str(&format!(
            "attachment; filename=\"{}\"",
            f.replace(['"', '\\'], "_")
        )) {
            headers.insert(http::header::CONTENT_DISPOSITION, v);
        }
    }
    if dlna {
        headers.insert(
            "transferMode.dlna.org",
            http::HeaderValue::from_static("Streaming"),
        );
        headers.insert(
            "contentFeatures.dlna.org",
            http::HeaderValue::from_static("DLNA.ORG_OP=01;DLNA.ORG_CI=0"),
        );
    }
    let mut st = 200u16;
    match &range {
        Some(r) => {
            st = 206;
            headers.insert(
                http::header::CONTENT_RANGE,
                http::HeaderValue::from_str(&format!(
                    "bytes {}-{}/{}",
                    r.start,
                    r.end - 1,
                    size
                ))
                .unwrap_or(http::HeaderValue::from_static("bytes */*")),
            );
            headers.insert(http::header::CONTENT_LENGTH, (r.end - r.start).into());
        }
        None => {
            headers.insert(http::header::CONTENT_LENGTH, size.into());
        }
    }
    ContentHeaders { status: st, range }
}
