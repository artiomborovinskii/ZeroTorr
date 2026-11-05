//! Встроенная мини-страница управления.

/// UI одним gzip-блоком (5.1 КБ → ~1.6 КБ в бинарнике): отдаём готовый gzip
/// с Content-Encoding — браузеры всегда шлют Accept-Encoding: gzip, сервер не
/// тратит CPU/RAM на компрессию, несжатая копия в бинарник не включается.
/// Регенерация после правки assets/ui.html:
/// `gzip -9n -c assets/ui.html > assets/ui.html.gz`
pub fn index() -> axum::response::Response {
    axum::response::Response::builder()
        .header(http::header::CONTENT_TYPE, "text/html; charset=utf-8")
        .header(http::header::CONTENT_ENCODING, "gzip")
        .header(http::header::VARY, "Accept-Encoding")
        .body(axum::body::Body::from(
            include_bytes!("../assets/ui.html.gz").as_slice(),
        ))
        .unwrap_or_default()
}
