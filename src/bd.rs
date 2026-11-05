//! Шард-файлы БД с атомарной записью: torrents.json, viewed.json, settings.json, accs.db.
use anyhow::{Context, Result};
use serde::{de::DeserializeOwned, Serialize};
use std::io::Write;
use std::path::{Path, PathBuf};

/// Запись торрента в БД (формат torrents.json совместим с оригиналом по полям).
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct TorrentDbItem {
    pub title: String,
    pub poster: String,
    pub data: String,
    pub size: i64,
    pub timestamp: i64,
    pub hash: String,
    #[serde(default)]
    pub name: String,
}

pub struct Bd {
    dir: PathBuf,
    readonly: bool,
}

impl Bd {
    pub fn new(dir: &Path, readonly: bool) -> Self {
        if !readonly {
            let _ = std::fs::create_dir_all(dir);
        }
        Self {
            dir: dir.to_path_buf(),
            readonly,
        }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.dir.join(name)
    }

    fn load<T: DeserializeOwned>(&self, name: &str) -> Option<T> {
        let data = std::fs::read(self.path(name)).ok()?;
        serde_json::from_slice(&data).ok()
    }

    fn store<T: Serialize>(&self, name: &str, val: &T) -> Result<()> {
        if self.readonly {
            anyhow::bail!("readonly mode");
        }
        let tmp = self.path(&format!("{name}.tmp"));
        let dst = self.path(name);
        let data = serde_json::to_vec(val).context("serialize")?;
        {
            let mut f = std::fs::File::create(&tmp).with_context(|| format!("create {tmp:?}"))?;
            f.write_all(&data)?;
            f.sync_all().ok();
        }
        std::fs::rename(&tmp, &dst).with_context(|| format!("rename {dst:?}"))?;
        Ok(())
    }

    // ---- torrents.json ----
    pub fn torrents(&self) -> Vec<TorrentDbItem> {
        self.load::<Vec<TorrentDbItem>>("torrents.json").unwrap_or_default()
    }

    pub fn torrents_save(&self, items: &[TorrentDbItem]) -> Result<()> {
        self.store("torrents.json", &items.to_vec())
    }

    // ---- viewed.json: {hash: {file_index: timecode}} ----
    // Как в оригинале: на каждый хэш — множество просмотренных файлов.
    // timecode добавлен как в master (Lampa «продолжить просмотр»).
    // Старый формат v1 {hash: file_index} читается и мигрируется на лету.
    fn viewed_map(&self) -> std::collections::HashMap<String, std::collections::HashMap<i64, f64>> {
        if let Some(v) = self
            .load::<std::collections::HashMap<String, std::collections::HashMap<i64, f64>>>(
                "viewed.json",
            )
        {
            return v;
        }
        // миграция v1 → v2
        let old: std::collections::HashMap<String, i64> =
            self.load("viewed.json").unwrap_or_default();
        old.into_iter()
            .map(|(h, i)| (h, std::iter::once((i, 0.0)).collect()))
            .collect()
    }

    /// Список просмотренных: (hash, file_index, timecode).
    /// hash != "" — только этот торрент (Lampa доверяет фильтру сервера!).
    pub fn viewed_list(&self, hash: &str) -> Vec<(String, i64, f64)> {
        let v = self.viewed_map();
        let mut out = Vec::new();
        if hash.is_empty() {
            for (h, files) in v {
                for (idx, tc) in files {
                    out.push((h.clone(), idx, tc));
                }
            }
        } else if let Some(files) = v.get(hash) {
            for (idx, tc) in files {
                out.push((hash.to_string(), *idx, *tc));
            }
        }
        out.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));
        out
    }

    pub fn viewed_set(&self, hash: &str, file_index: i64, timecode: f64) -> Result<()> {
        let mut v = self.viewed_map();
        v.entry(hash.to_string())
            .or_default()
            .insert(file_index, timecode);
        self.store("viewed.json", &v)
    }

    pub fn viewed_rem(&self, hash: &str, file_index: i64) -> Result<()> {
        let mut v = self.viewed_map();
        if file_index < 0 {
            v.remove(hash);
        } else if let Some(files) = v.get_mut(hash) {
            files.remove(&file_index);
            if files.is_empty() {
                v.remove(hash);
            }
        }
        self.store("viewed.json", &v)
    }

    // ---- settings.json ----
    pub fn settings(&self) -> Option<crate::models::BTSets> {
        self.load::<crate::models::BTSets>("settings.json")
    }

    pub fn settings_save(&self, s: &crate::models::BTSets) -> Result<()> {
        self.store("settings.json", s)
    }

    // ---- accs.db: {"login":"password"} ----
    pub fn accounts(&self) -> std::collections::HashMap<String, String> {
        self.load::<std::collections::HashMap<String, String>>("accs.db")
            .unwrap_or_default()
    }
}
