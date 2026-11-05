//! Встроенные корневые сертификаты (Mozilla CA bundle).
//!
//! Проблема: Android/Termux и минимальные окружения (alpine без ca-certificates,
//! контейнеры без /etc/ssl) не имеют системных корневых сертификатов. Внутри
//! librqbit клиент reqwest собирается с rustls-platform-verifier, который при
//! старте сессии (`Session::new_with_opts` -> сборка HTTP-клиента) грузит
//! системные CA через rustls-native-certs и падает:
//!   "error building HTTP(S) client / No CA certificates were loaded
//!    from the system"
//!
//! Решение: бандл Mozilla CA вшит в бинарник и при отсутствии системных
//! сертификатов выгружается в файл, путь к которому передаётся через
//! SSL_CERT_FILE — rustls-native-certs читает эту переменную первой
//! (openssl-probe). Вызывать ДО создания сессии, в главном потоке.

/// Mozilla CA bundle (curl.se/ca/cacert.pem), вшит в бинарник (~200 КиБ).
pub const CA_PEM: &str = include_str!("../assets/cacert.pem");

/// Системные бандлы обычного Linux (десктоп/сервер). Если есть хоть один —
/// не вмешиваемся: работаем как раньше, на системных сертификатах.
const SYSTEM_BUNDLES: &[&str] = &[
    "/etc/ssl/certs/ca-certificates.crt",
    "/etc/pki/tls/certs/ca-bundle.crt",
    "/etc/ssl/ca-bundle.pem",
    "/etc/pki/tls/cacert.pem",
    "/etc/ssl/cert.pem",
];

/// Гарантировать читаемый бандл CA до инициализации сессии.
///
/// Порядок:
/// 1. SSL_CERT_FILE уже задан пользователем — уважаем, ничего не делаем.
/// 2. Есть системный бандл — не вмешиваемся.
/// 3. Termux: $PREFIX/etc/tls/cert.pem (pkg install ca-certificates).
/// 4. Вшитый бандл -> `<data_dir>/ca-cert.pem` (или TMPDIR), SSL_CERT_FILE на него.
///
/// ВАЖНО: вызывать до спавна потоков (setenv не потокобезопасен) —
/// то есть из main() до построения tokio-runtime.
pub fn ensure_ca_file(data_dir: &std::path::Path) {
    use std::path::Path;

    // 1. явный оверрайд пользователя
    if std::env::var_os("SSL_CERT_FILE").is_some() {
        return;
    }

    // 2. обычный Linux со системными сертификатами
    if SYSTEM_BUNDLES.iter().any(|p| Path::new(p).is_file()) {
        return;
    }

    // 3. Termux
    if let Some(prefix) = std::env::var_os("PREFIX") {
        let p = std::path::PathBuf::from(&prefix).join("etc/tls/cert.pem");
        if p.is_file() {
            std::env::set_var("SSL_CERT_FILE", &p);
            println!("CA bundle: {}", p.display());
            return;
        }
    }

    // 4. вшитый бандл
    let candidates = [
        data_dir.join("ca-cert.pem"),
        std::env::temp_dir().join("zerotorr-ca-cert.pem"),
    ];
    // уже выгружен ранее — просто используем
    for p in &candidates {
        if p.is_file() {
            std::env::set_var("SSL_CERT_FILE", p);
            println!("CA bundle: {}", p.display());
            return;
        }
    }
    for p in &candidates {
        match std::fs::write(p, CA_PEM) {
            Ok(_) => {
                std::env::set_var("SSL_CERT_FILE", p);
                println!("CA bundle (вшитый): {}", p.display());
                return;
            }
            Err(e) => eprintln!("CA bundle: не удалось записать {}: {e}", p.display()),
        }
    }
    // записать не удалось нигде — оставляем как есть: платформенный верификатор
    // либо найдёт сертификаты сам, либо вернёт прежнюю понятную ошибку
}
