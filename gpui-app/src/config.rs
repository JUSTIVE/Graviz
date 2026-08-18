//! Persisted user state: view settings and the recent-schema list.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(default)]
pub struct Settings {
    pub show_descriptions: bool,
    pub bundle_edges: bool,
    pub sidebar_open: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            show_descriptions: false,
            bundle_edges: true,
            sidebar_open: true,
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct RecentEntry {
    pub path: String,
    pub name: String,
}

fn dir() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    let base = if cfg!(target_os = "macos") {
        PathBuf::from(home).join("Library/Application Support/GompassQL")
    } else {
        PathBuf::from(home).join(".config/gompassql")
    };
    std::fs::create_dir_all(&base).ok()?;
    Some(base)
}

fn read_json<T: serde::de::DeserializeOwned>(file: &str) -> Option<T> {
    let path = dir()?.join(file);
    serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
}

fn write_json<T: Serialize>(file: &str, value: &T) {
    let Some(base) = dir() else { return };
    if let Ok(json) = serde_json::to_string_pretty(value) {
        let _ = std::fs::write(base.join(file), json);
    }
}

pub fn load_settings() -> Settings {
    read_json("settings.json").unwrap_or_default()
}

pub fn save_settings(s: &Settings) {
    write_json("settings.json", s);
}

pub fn recents() -> Vec<RecentEntry> {
    read_json("recent.json").unwrap_or_default()
}

pub fn push_recent(path: &Path) {
    let Ok(canonical) = path.canonicalize() else { return };
    let entry = RecentEntry {
        path: canonical.display().to_string(),
        name: canonical
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| canonical.display().to_string()),
    };
    let mut list = recents();
    list.retain(|e| e.path != entry.path);
    list.insert(0, entry);
    list.truncate(10);
    write_json("recent.json", &list);
}
