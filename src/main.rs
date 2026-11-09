//! CLI, запуск сервера.
use anyhow::Result;
use zerotorr::{RunOptions};

// --- Аллокатор (фича jemalloc) ------------------------------------------------
// По умолчанию — системный musl mallocng. С --features jemalloc все аллокации
// идут в jemalloc 5.3 — тот же выбор, что у стоковой прошивки станции
// (maind: «Initialized MemoryProfilingManager ... jemalloc»).
//
// malloc_conf читается jemalloc'ом при первом malloc, поэтому задаётся
// статически на этапе компиляции:
//   narenas:2          — по числу tokio-воркеров в --low-memory (меньше пер-арен-
//                        ных структур: каждая арена ~десятки КиБ метаданных);
//   dirty_decay_ms:1000 — грязные (освобождённые) страницы возвращаются ОС через
//                        ~1 с (madvise DONTNEED), а не копятся в кэше аллокатора;
//   muzzy_decay_ms:1000 — то же для lazy-purged страниц.
// Итог: RSS после всплеска (перемотка, метаданные 4К-торрента) спадает обратно,
// вместо того чтобы оставаться на пике.
#[cfg(feature = "jemalloc")]
#[global_allocator]
static GLOBAL: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

// malloc_conf задаётся на этапе СБОРКИ (JEMALLOC_SYS_WITH_MALLOC_CONF,
// см. .cargo/config.toml): narenas:2 (= 2 tokio-воркера --low-memory),
// dirty/muzzy decay 1 с — освобождённые страницы почти сразу возвращаются ОС,
// RSS после всплеска (перемотка, метаданные) спадает, а не остаётся на пике.
fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();

    let mut port: u16 = 8090;
    let mut path = std::env::current_dir().unwrap_or_else(|_| ".".into());
    let mut logpath: Option<String> = None;
    let mut weblogpath: Option<String> = None;
    let mut rdb = false;
    let mut httpauth = false;
    let mut dontkill = false;
    let mut ui = false;
    let mut torrentsdir: Option<std::path::PathBuf> = None;
    let mut dbg = false;
    let mut low_memory = false;
    let mut ram_storage: Option<bool> = None;

    let mut i = 1;
    let flag = |a: &[String], i: usize, short: &str, long: &str| -> bool {
        a[i] == short || a[i] == long || a[i] == format!("{long}=")
    };
    while i < args.len() {
        let a = &args[i];
        let take_val = |i: &mut usize, args: &[String]| -> Option<String> {
            if let Some(eq) = args[*i].find('=') {
                let v = args[*i][eq + 1..].to_string();
                Some(v)
            } else {
                *i += 1;
                args.get(*i).cloned()
            }
        };
        if a == "-h" || a == "--help" {
            print_help();
            return Ok(());
        } else if flag(&args, i, "-p", "--port") {
            if let Some(v) = take_val(&mut i, &args) {
                port = v.parse().unwrap_or(8090);
            }
        } else if flag(&args, i, "-d", "--path") {
            if let Some(v) = take_val(&mut i, &args) {
                path = v.into();
            }
        } else if flag(&args, i, "-l", "--logpath") {
            if let Some(v) = take_val(&mut i, &args) {
                logpath = Some(v);
            }
        } else if flag(&args, i, "-w", "--weblogpath") {
            if let Some(v) = take_val(&mut i, &args) {
                weblogpath = Some(v);
            }
        } else if flag(&args, i, "-r", "--rdb") {
            rdb = true;
        } else if flag(&args, i, "-a", "--httpauth") {
            httpauth = true;
        } else if flag(&args, i, "-k", "--dontkill") {
            dontkill = true;
        } else if flag(&args, i, "-u", "--ui") {
            ui = true;
        } else if flag(&args, i, "-t", "--torrentsdir") {
            if let Some(v) = take_val(&mut i, &args) {
                torrentsdir = Some(v.into());
            }
        } else if a == "--dbg" {
            dbg = true;
        } else if a == "--low-memory" {
            low_memory = true;
        } else if a == "--ram-storage" {
            ram_storage = Some(true);
        } else if a == "--no-ram-storage" {
            ram_storage = Some(false);
        }
        i += 1;
    }
    let _ = (logpath, weblogpath, ui); // совместимость: принимаем, не используем

    // RAM-режим: явно флагом или автоопределением tmpfs/ramfs под -d.
    let ram_storage = ram_storage.unwrap_or_else(|| is_ramfs(&path));

    println!("=========== START ===========");
    println!("ZeroTorr {} (Rust, librqbit)", zerotorr::api::VERSION);
    #[cfg(feature = "jemalloc")]
    println!("alloc: jemalloc 5.3 (narenas:2, dirty/muzzy decay 1s)");
    #[cfg(not(feature = "jemalloc"))]
    println!("alloc: system (musl mallocng)");
    println!("path: {}", path.display());
    // «storage: …» печатает zerotorr::run (с бюджетом RAM-режима) — здесь дубликат не нужен.

    // runtime: экономия RAM на ТВ-боксах/роутерах. В --low-memory 2 воркера
    // (стриминг I/O-bound, этого достаточно); на десктопе дефолт по числу ядер,
    // чтобы не резать скорость раздачи. Стек потоков 1 МиБ вместо 2, пул
    // блокирующих потоков ограничен (дефолт tokio — 512).
    let mut rt_b = tokio::runtime::Builder::new_multi_thread();
    rt_b.enable_all();
    rt_b.thread_stack_size(1024 * 1024);
    // код-ревью: 4 блокирующих потока в low-memory (дефолт tokio — 512)
    rt_b.max_blocking_threads(if low_memory { 4 } else { 32 });
    if low_memory {
        rt_b.worker_threads(2);
    }
    let rt = rt_b.build()?;
    // --dbg (фича dbg): инициализируем подписчик tracing. С
    // release_max_level_warn в бинарнике остаются только warn!/error!.
    #[cfg(feature = "dbg")]
    {
        use tracing_subscriber::EnvFilter;
        tracing_subscriber::fmt()
            .with_env_filter(
                EnvFilter::try_from_default_env()
                    .unwrap_or_else(|_| EnvFilter::new(std::env::var("RUST_LOG").unwrap_or_else(|_| "warn".into()))),
            )
            .init();
    }
    let opts = RunOptions {
        port,
        path,
        httpauth,
        rdb,
        dontkill,
        torrentsdir,
        dbg,
        low_memory,
        ram_storage,
    };
    rt.block_on(zerotorr::run(opts))
}

/// tmpfs/ramfs под каталогом данных? (авто-RAM-режим).
/// Каталога может ещё не существовать — поднимаемся до ближайшего живого
/// предка (созданный позже каталог унаследует ту же ФС).
#[cfg(target_os = "linux")]
fn is_ramfs(p: &std::path::Path) -> bool {
    use std::os::unix::ffi::OsStrExt as _;
    const TMPFS_MAGIC: u32 = 0x0102_1994;
    const RAMFS_MAGIC: u32 = 0x8584_58F6;
    let mut cur: Option<&std::path::Path> = Some(p);
    while let Some(dir) = cur {
        if let Ok(c) = std::ffi::CString::new(dir.as_os_str().as_bytes()) {
            let mut st: libc::statfs = unsafe { std::mem::zeroed() };
            if unsafe { libc::statfs(c.as_ptr(), &mut st) } == 0 {
                let t = st.f_type as u32;
                return t == TMPFS_MAGIC || t == RAMFS_MAGIC;
            }
        }
        cur = dir.parent();
    }
    false
}

#[cfg(not(target_os = "linux"))]
fn is_ramfs(_p: &std::path::Path) -> bool {
    false
}

fn print_help() {
    println!(
        "ZeroTorr — TorrServer (MatriX.111 API) in Rust

Usage: zerotorr [flags]
  -p, --port string        web server port (default \"8090\")
  -d, --path string        database dir path (default \".\")
  -l, --logpath string     log file path
  -w, --weblogpath string  web access log path (ignored, kept for compat)
  -t, --torrentsdir string autoload torrents from dir
  -r, --rdb                start in read-only DB mode
  -a, --httpauth           enable http basic auth (accs.db)
  -k, --dontkill           don't exit on shutdown endpoint (keep for compat)
  -u, --ui                 (kept for compat)
  --dbg                    verbose log
  --low-memory             low-memory profile: cache ≤ 8 MiB, ≤ 8 peers,
                           no preload, 64 KiB HTTP chunks, 2 tokio workers
  --ram-storage            keep downloaded pieces in RAM with a budget
                           (auto-enabled when -d is on tmpfs/ramfs)
  --no-ram-storage         force disk storage even on tmpfs"
    );
}
