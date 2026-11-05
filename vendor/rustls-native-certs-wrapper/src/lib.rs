//! Drop-in замена `rustls-native-certs` 0.8.5 для ZeroTorr.
//!
//! Проблема: оригинальный крейт ищет системные хранилища CA (/etc/ssl/certs,
//! /etc/pki/tls, ...), которых нет на Android/Termux и в минимальных
//! контейнерах. В итоге rustls-platform-verifier (используется reqwest 0.13
//! внутри librqbit) падает при создании HTTP-клиента:
//! «unexpected error: No CA certificates were loaded from the system»,
//! а у пользователя это выглядит как `Error: session init`.
//!
//! Решение: если не заданы SSL_CERT_FILE/SSL_CERT_DIR — вернуть встроенный
//! в бинарник комплект Mozilla CA (webpki-root-certs), как это делают
//! Go-бинарники TorrServer со встроенными корнями. Бинарник становится
//! полностью самодостаточным: TLS-трекеры и загрузка .torrent по https
//! работают без ca-certificates в системе.
//!
//! Если переменные SSL_CERT_FILE/SSL_CERT_DIR заданы — сначала используется
//! их содержимое (как в оригинале; удобно для корпоративных CA), а при пустом
//! результате происходит откат к встроенному комплекту.

use pki_types::pem::PemObject;
use pki_types::CertificateDer;
use std::error::Error as StdError;
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};

const ENV_CERT_FILE: &str = "SSL_CERT_FILE";
const ENV_CERT_DIR: &str = "SSL_CERT_DIR";

/// Встроенный комплект Mozilla CA (~150 корней, компилируется в бинарник).
fn bundled_certs() -> Vec<CertificateDer<'static>> {
    webpki_root_certs::TLS_SERVER_ROOT_CERTS.to_vec()
}

/// Загрузка «системных» корней: SSL_CERT_FILE/SSL_CERT_DIR, иначе — встроенные.
pub fn load_native_certs() -> CertificateResult {
    let file = std::env::var_os(ENV_CERT_FILE).map(PathBuf::from);
    let dir = std::env::var_os(ENV_CERT_DIR).map(PathBuf::from);
    if file.is_some() || dir.is_some() {
        // Явный override: пробуем его, но при пустом результате
        // откатываемся к встроенным, чтобы TLS не умирал целиком.
        let res = load_certs_from_paths(file.as_deref(), dir.as_deref());
        if !res.certs.is_empty() {
            return res;
        }
    }
    CertificateResult {
        certs: bundled_certs(),
        errors: Vec::new(),
    }
}

/// Упрощённый аналог оригинала: PEM-сертификаты из файла и/или каталога.
pub fn load_certs_from_paths(file: Option<&Path>, dir: Option<&Path>) -> CertificateResult {
    let mut out = CertificateResult {
        certs: Vec::new(),
        errors: Vec::new(),
    };
    if let Some(f) = file {
        // pem_file_iter: ошибка открытия файла — из Result, ошибки разбора — из итератора
        match CertificateDer::pem_file_iter(f) {
            Ok(iter) => {
                for cert in iter {
                    match cert {
                        Ok(c) => out.certs.push(c),
                        Err(e) => out.errors.push(Error {
                            context: "loading PEM certificates",
                            kind: ErrorKind::Pem(e),
                        }),
                    }
                }
            }
            Err(e) => out.errors.push(Error {
                context: "opening certificate file",
                kind: ErrorKind::Io {
                    inner: io::Error::new(io::ErrorKind::InvalidData, e.to_string()),
                    path: f.to_owned(),
                },
            }),
        }
    }
    if let Some(d) = dir {
        let rd = match std::fs::read_dir(d) {
            Ok(rd) => rd,
            Err(e) => {
                out.errors.push(Error {
                    context: "reading certificate directory",
                    kind: ErrorKind::Io {
                        inner: e,
                        path: d.to_owned(),
                    },
                });
                return out;
            }
        };
        let mut paths: Vec<PathBuf> = rd.filter_map(|e| e.ok()).map(|e| e.path()).collect();
        paths.sort();
        for p in paths {
            if !p.is_file() {
                continue;
            }
            if let Ok(iter) = CertificateDer::pem_file_iter(&p) {
                for cert in iter {
                    if let Ok(c) = cert {
                        out.certs.push(c);
                    }
                }
            }
        }
    }
    out
}

/// Результат загрузки (совместим с оригиналом 0.8.x).
#[derive(Debug)]
pub struct CertificateResult {
    /// Найденные сертификаты.
    pub certs: Vec<CertificateDer<'static>>,
    /// Ошибки, возникшие при загрузке.
    pub errors: Vec<Error>,
}

impl CertificateResult {
    /// Вернуть сертификаты или паниковать с сообщением `msg`.
    #[track_caller]
    pub fn expect(self, msg: &str) -> Vec<CertificateDer<'static>> {
        if self.errors.is_empty() {
            self.certs
        } else {
            panic!("{msg}: {:?}", self.errors)
        }
    }

    /// Вернуть сертификаты или паниковать.
    #[track_caller]
    pub fn unwrap(self) -> Vec<CertificateDer<'static>> {
        self.expect("called `CertificateResult::unwrap()` on an error result")
    }
}

/// Ошибка загрузки сертификата (совместима с оригиналом).
#[derive(Debug)]
pub struct Error {
    pub context: &'static str,
    pub kind: ErrorKind,
}

impl StdError for Error {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        match &self.kind {
            ErrorKind::Io { inner, .. } => Some(inner),
            ErrorKind::Os(err) => Some(err.as_ref()),
            ErrorKind::Pem(err) => Some(err),
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.context)?;
        f.write_str(": ")?;
        match &self.kind {
            ErrorKind::Io { inner, path } => write!(f, "{inner} at '{}'", path.display()),
            ErrorKind::Os(err) => err.fmt(f),
            ErrorKind::Pem(err) => err.fmt(f),
        }
    }
}

/// Вид ошибки (совместим с оригиналом).
#[non_exhaustive]
#[derive(Debug)]
pub enum ErrorKind {
    Io { inner: io::Error, path: PathBuf },
    Os(Box<dyn StdError + Send + Sync + 'static>),
    Pem(pki_types::pem::Error),
}
