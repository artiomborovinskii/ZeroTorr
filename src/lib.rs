//! ZeroTorr — TorrServer (MatriX.111 API) на чистом Rust + librqbit.
pub mod api;
pub mod bd;
pub mod cache;
pub mod engine;
pub mod manager;
pub mod mime;
pub mod models;
pub mod ramstore;
pub mod serve;
pub mod torrent;
pub mod ui;

use anyhow::{Context as _, Result};
use axum::serve::ListenerExt as _;
use std::sync::Arc;

pub struct ServerCtx {
    pub app: api::App,
    pub shutdown: Arc<tokio::sync::Notify>,
}

static SHUTDOWN: std::sync::OnceLock<Arc<tokio::sync::Notify>> = std::sync::OnceLock::new();

pub async fn request_shutdown() {
    if let Some(n) = SHUTDOWN.get() {
        n.notify_waiters();
    }
}

pub async fn shutdown_signal() -> &'static Arc<tokio::sync::Notify> {
    let n = SHUTDOWN.get_or_init(|| Arc::new(tokio::sync::Notify::new()));
    n
}

/// Полный запуск: менеджер + HTTP-сервер. Возвращает после /shutdown или сигнала.
pub async fn run(opts: crate::RunOptions) -> Result<()> {
    let sets = load_sets(&opts);
    print_mem_banner();
    // RAM-режим: куски торрентов живут в памяти с бюджетом CacheSize,
    // вытесненные перекачиваются (для tmpfs/маломных устройств).
    let ram = if opts.ram_storage {
        let budget = sets.CacheSize.max(2 * 1024 * 1024) as u64;
        println!(
            "storage: RAM mode (tmpfs/ramfs or --ram-storage), budget {} KiB/torrent",
            budget / 1024
        );
        Some(ramstore::RamRegistry::new(budget))
    } else {
        println!("storage: disk mode");
        None
    };
    let mgr = manager::Manager::new(opts.path.clone(), sets, opts.rdb, opts.low_memory, ram).await?;

    // восстановление торрентов из БД (по data)
    if !opts.rdb {
        for item in mgr.bd.torrents() {
            if item.data.is_empty() {
                continue;
            }
            if let Err(e) = mgr.add(&item.hash, &item.title, &item.poster, &item.data, true).await {
                tracing::warn!("restore {}: {e:#}", &item.hash[..16.min(item.hash.len())]);
            }
        }
    }

    // автозагрузка .torrent из каталога
    if let Some(tdir) = &opts.torrentsdir {
        autoload(&mgr, tdir).await;
    }

    let app = api::App {
        mgr: mgr.clone(),
        httpauth: opts.httpauth,
        dontkill: opts.dontkill,
        low_memory: opts.low_memory,
    };

    let router = api::router(app);
    let addr = std::net::SocketAddr::from(([0, 0, 0, 0], opts.port));
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .with_context(|| format!("bind {addr}"))?
        // TCP_NODELAY на принятых соединениях — как net в Go (гипер сам не ставит;
        // без него мелкие JSON-ответы страдают от Нагла до 40 мс)
        .tap_io(|io| {
            let _ = io.set_nodelay(true);
        });
    println!("Start http server at {addr}");

    // обработка сигналов
    tokio::spawn(async {
        let _ = tokio::signal::ctrl_c().await;
        request_shutdown().await;
    });

    let notify = shutdown_signal().await.clone();
    let shutdown_fut = async move {
        notify.notified().await;
    };

    let mgr2 = mgr.clone();
    axum::serve(listener, router)
        .with_graceful_shutdown(async move {
            shutdown_fut.await;
            println!("Shutting down...");
            mgr2.close_all().await;
        })
        .await
        .context("server")?;
    Ok(())
}

pub struct RunOptions {
    pub port: u16,
    pub path: std::path::PathBuf,
    pub httpauth: bool,
    pub rdb: bool,
    pub dontkill: bool,
    pub torrentsdir: Option<std::path::PathBuf>,
    pub dbg: bool,
    /// профиль экономии памяти (см. models::BTSets::sanitize_low_memory)
    pub low_memory: bool,
    /// RAM-хранилище: куски в памяти с бюджетом (tmpfs/маломные устройства)
    pub ram_storage: bool,
}

/// Строка «mem: total .. MiB, available .. MiB» (только Linux).
fn print_mem_banner() {
    #[cfg(target_os = "linux")]
    {
        let parse = |key: &str| -> Option<u64> {
            std::fs::read_to_string("/proc/meminfo").ok()?.lines().find_map(|l| {
                l.strip_prefix(key)
                    .and_then(|r| r.trim_start_matches(':').trim().split_whitespace().next())
                    .and_then(|v| v.parse::<u64>().ok())
            })
        };
        if let (Some(total), Some(avail)) = (parse("MemTotal"), parse("MemAvailable")) {
            println!("mem: total {} MiB, available {} MiB", total / 1024, avail / 1024);
        }
        // собственный RSS процесса — видно, во что обходится сервер на месте
        if let Ok(st) = std::fs::read_to_string("/proc/self/status") {
            if let Some(l) = st.lines().find(|l| l.starts_with("VmRSS:")) {
                println!("mem: self RSS {}", l["VmRSS:".len()..].trim());
            }
        }
    }
}

fn load_sets(opts: &RunOptions) -> models::BTSets {
    let mut sets = {
        let bd = bd::Bd::new(&opts.path, opts.rdb);
        bd.settings().unwrap_or_default()
    };
    if opts.dbg {
        sets.EnableDebug = true;
    }
    sets.sanitize();
    if opts.low_memory {
        sets.sanitize_low_memory();
        println!(
            "low-memory: cache {} KiB, readahead {}%, peers {}",
            sets.CacheSize / 1024,
            sets.ReaderReadAHead,
            sets.ConnectionsLimit
        );
    }
    sets
}

async fn autoload(mgr: &Arc<manager::Manager>, dir: &std::path::Path) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for entry in rd.flatten() {
        let p = entry.path();
        if p.extension().and_then(|e| e.to_str()) == Some("torrent") {
            match std::fs::read(&p) {
                Ok(bytes) => {
                    use base64::Engine as _;
                    let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
                    let name = p
                        .file_stem()
                        .map(|s| s.to_string_lossy().to_string())
                        .unwrap_or_default();
                    match mgr.add(&name, &name, "", &b64, true).await {
                        Ok(_) => {
                            std::fs::remove_file(&p).ok(); // как в оригинале
                        }
                        Err(e) => tracing::warn!("autoload {}: {e:#}", name),
                    }
                }
                Err(e) => tracing::warn!("autoload read: {e}"),
            }
        }
    }
}
