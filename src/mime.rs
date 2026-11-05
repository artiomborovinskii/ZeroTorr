//! MIME-таблицы: контент-тайп по расширению + фильтр медиафайлов для плейлистов.

pub fn by_extension(ext: &str) -> &'static str {
    match ext.to_ascii_lowercase().as_str() {
        "mp4" | "m4v" => "video/mp4",
        "mkv" => "video/x-matroska",
        "avi" => "video/x-msvideo",
        "mov" => "video/quicktime",
        "wmv" => "video/x-ms-wmv",
        "flv" => "video/x-flv",
        "mpg" | "mpeg" => "video/mpeg",
        "ts" | "m2ts" => "video/mp2t",
        "webm" => "video/webm",
        "3gp" => "video/3gpp",
        "ogv" => "video/ogg",
        "mp3" => "audio/mpeg",
        "aac" => "audio/aac",
        "m4a" => "audio/mp4",
        "flac" => "audio/flac",
        "ogg" | "oga" | "opus" => "audio/ogg",
        "wav" => "audio/wav",
        "wma" => "audio/x-ms-wma",
        "ac3" | "dts" => "audio/ac3",
        "srt" | "ass" | "ssa" | "sub" | "vtt" => "text/plain",
        "html" | "htm" => "text/html",
        "css" => "text/css",
        "js" => "application/javascript",
        "json" => "application/json",
        "txt" | "log" => "text/plain",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "svg" => "image/svg+xml",
        "ico" => "image/x-icon",
        "torrent" => "application/x-bittorrent",
        "m3u" => "audio/x-mpegurl",
        "pdf" => "application/pdf",
        "zip" => "application/zip",
        "iso" => "application/octet-stream",
        _ => "application/octet-stream",
    }
}

/// Есть ли расширение в файле вообще (для путей без точки берём хвост).
pub fn ext_of(path: &str) -> String {
    let name = path.rsplit(['/', '\\']).next().unwrap_or(path);
    match name.rsplit_once('.') {
        Some((_, e)) if e.len() <= 8 => e.to_string(),
        _ => String::new(),
    }
}

/// Медиа-файл (видео/аудио) — такие попадают в m3u-плейлисты.
pub fn is_media(path: &str) -> bool {
    matches!(
        ext_of(path).as_str(),
        "mp4" | "m4v" | "mkv" | "avi" | "mov" | "wmv" | "flv" | "mpg" | "mpeg" | "ts" | "m2ts"
            | "webm" | "3gp" | "ogv" | "mp3" | "aac" | "m4a" | "flac" | "ogg" | "oga" | "opus"
            | "wav" | "wma" | "ac3" | "dts"
    )
}
