//! Обёртка над librqbit: сессия, добавление торрентов, применение настроек.
use anyhow::{Context, Result};
use crate::ramstore::RamRegistry;
use librqbit::storage::StorageFactoryExt as _;
use librqbit::{AddTorrent, AddTorrentOptions, Session, SessionOptions};
use librqbit_core::Id20;
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub type Handle = Arc<librqbit::ManagedTorrent>;

pub struct Engine {
    pub session: Arc<Session>,
    pub out_root: PathBuf,
    /// RAM-хранилище (RAM-режим для tmpfs/маломных устройств); None = диск.
    pub ram: Option<Arc<crate::ramstore::RamRegistry>>,
}

/// Дефолтные трекеры (defTrackers из оригинала).
pub const DEFAULT_TRACKERS: &[&str] = &[
    "http://bt.t-ru.org/ann?magnet=",
    "http://bt2.t-ru.org/ann?magnet=",
    "http://bt3.t-ru.org/ann?magnet=",
    "http://bt4.t-ru.org/ann?magnet=",
    "udp://tracker.opentrackr.org:1337/announce",
    "udp://tracker.openbittorrent.com:6969/announce",
    "udp://opentracker.io:6969/announce",
    "udp://tracker.internetwarriors.net:1337/announce",
    "http://tracker.files.fm:6969/announce",
    "https://tracker.tamersunion.org:443/announce",
    "udp://tracker.torrent.eu.org:451/announce",
    "udp://open.demonii.com:1337/announce",
    "udp://tracker.tiny-vps.com:6969/announce",
];

impl Engine {
    /// Создать сессию с опциями из настроек.
    pub async fn new(
        out_root: PathBuf,
        sets: &crate::models::BTSets,
        ram: Option<Arc<crate::ramstore::RamRegistry>>,
        low_memory: bool,
    ) -> Result<Self> {
        std::fs::create_dir_all(&out_root).with_context(|| format!("mkdir {:?}", out_root))?;
        let mut opts = SessionOptions::default();
        opts.disable_trackers = false;
        // --low-memory: пул блокирующих (дисковых) потоков librqbit 8 → 4 —
        // меньше стеков и буферов в RAM; для медленного flash-диска хватает.
        if low_memory {
            opts.runtime_worker_threads = Some(4);
        }
        opts.dht = if sets.DisableDHT {
            None
        } else {
            Some(Default::default())
        };
        // порт BT
        let port = if sets.PeersListenPort > 0 {
            sets.PeersListenPort as u16
        } else {
            0
        };
        let mut lo = librqbit::ListenerOptions::default();
        lo.listen_addr = std::net::SocketAddr::from(([0, 0, 0, 0], port));
        lo.enable_upnp_port_forwarding = !sets.DisableUPNP;
        if sets.DisableUTP {
            lo.mode = librqbit::ListenerMode::TcpOnly;
        }
        opts.listen = Some(lo);
        // лимиты соединений
        if sets.ConnectionsLimit > 0 {
            opts.peer_limit = Some(sets.ConnectionsLimit as usize);
        }
        // скорости (KB/s -> байт/с)
        let mut rl = librqbit::limits::LimitsConfig::default();
        if sets.DownloadRateLimit > 0 {
            rl.download_bps = std::num::NonZeroU32::new((sets.DownloadRateLimit * 1024) as u32);
        }
        if sets.UploadRateLimit > 0 {
            rl.upload_bps = std::num::NonZeroU32::new((sets.UploadRateLimit * 1024) as u32);
        }
        opts.ratelimits = rl;
        // RAM-хранилище: куски живут в памяти с бюджетом, вытесненные
        // перекачиваются (см. ramstore.rs); также задаём окно опережения
        // стримера, чтобы оно не превышало бюджет (иначе холостые перекачки).
        if let Some(reg) = &ram {
            let win = (reg.budget() / 2).clamp(256 * 1024, 32 * 1024 * 1024);
            std::env::set_var("ZT_STREAM_BUF", win.to_string());
            let registry: crate::ramstore::RamRegistry = RamRegistry::clone(reg);
            opts.default_storage_factory = Some(registry.boxed());
        }
        // TCP вкл/выкл
        let mut co = librqbit::ConnectionOptions::default();
        co.enable_tcp = !sets.DisableTCP;
        opts.connect = Some(co);

        let session = Session::new_with_opts(out_root.clone(), opts)
            .await
            .context("session init")?;
        Ok(Self {
            session,
            out_root,
            ram,
        })
    }

    /// Каталог хранения для торрента.
    fn output_folder(&self, hash_hex: &str, sets: &crate::models::BTSets) -> PathBuf {
        if sets.TorrentsSavePath.is_empty() {
            self.out_root.join(hash_hex)
        } else {
            Path::new(&sets.TorrentsSavePath).join(hash_hex)
        }
    }

    /// Добавить торрент из magnet/URL или байтов .torrent.
    pub async fn add(
        &self,
        link: Option<&str>,
        data: Option<Vec<u8>>,
        hash_hex: &str,
        sets: &crate::models::BTSets,
        extra_trackers: &[String],
    ) -> Result<Handle> {
        let add = if let Some(bytes) = data {
            AddTorrent::from_bytes(bytes)
        } else if let Some(url) = link {
            AddTorrent::from_url(url.to_string())
        } else {
            anyhow::bail!("no link or data");
        };
        let mut opts = AddTorrentOptions::default();
        opts.output_folder = Some(
            self.output_folder(hash_hex, sets)
                .to_string_lossy()
                .to_string(),
        );
        opts.overwrite = true;
        // RetrackersMode: 0 не добавлять, 1 добавить дефолтные, 2 убрать (упрощ.), 3 заменить (упрощ.)
        if sets.RetrackersMode == 1 || sets.RetrackersMode == 3 {
            let mut tr: Vec<String> = extra_trackers.to_vec();
            for t in DEFAULT_TRACKERS {
                tr.push(t.to_string());
            }
            if !tr.is_empty() {
                opts.trackers = Some(tr);
            }
        }
        // Тестовый параметр magnet:?...&x-peers=1.2.3.4:51413,5.6.7.8:51413 —
        // стартовые пировые адреса (обходит DHT/трекер в локальных E2E-тестах).
        if let Some(url) = link {
            if let Some(pos) = url.find("x-peers=") {
                let spec = url[pos + 8..].split('&').next().unwrap_or("");
                let peers: Vec<std::net::SocketAddr> =
                    spec.split(',').filter_map(|s| s.trim().parse().ok()).collect();
                if !peers.is_empty() {
                    opts.initial_peers = Some(peers);
                }
            }
        }
        let resp = self.session.add_torrent(add, Some(opts)).await?;
        let h = match resp {
            librqbit::AddTorrentResponse::AlreadyManaged(_, h) | librqbit::AddTorrentResponse::Added(_, h) => {
                h
            }
            librqbit::AddTorrentResponse::ListOnly(_) => {
                anyhow::bail!("unexpected list_only response")
            }
        };
        // регистрируем хендл в RAM-реестре (для forget_pieces при вытеснении)
        if let Some(reg) = &self.ram {
            let id20 = id20_from_hex(hash_hex)
                .with_context(|| format!("bad hash {hash_hex}"))?;
            reg.register(id20, &h);
        }
        Ok(h)
    }

    /// Удалить торрент из сессии (с файлами или без).
    pub async fn remove(&self, hash: Id20, delete_files: bool) -> Result<()> {
        if let Some(reg) = &self.ram {
            reg.unregister(&hash);
        }
        self.session
            .delete(librqbit::api::TorrentIdOrHash::Hash(hash), delete_files)
            .await
            .context("session delete")
    }
}

/// Декод hex -> Id20.
pub fn id20_from_hex(hex: &str) -> Result<Id20> {
    let h = hex.trim().to_lowercase();
    anyhow::ensure!(h.len() == 40, "bad hash length");
    let mut buf = [0u8; 20];
    for i in 0..20 {
        buf[i] = u8::from_str_radix(&h[i * 2..i * 2 + 2], 16).context("bad hex")?;
    }
    Ok(Id20::new(buf))
}

pub fn hex_of(id: &Id20) -> String {
    id.0.iter().map(|x| format!("{x:02x}")).collect()
}
