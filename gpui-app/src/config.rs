//! Persisted user state: view settings and the recent-schema list.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(default)]
pub struct Settings {
    pub show_descriptions: bool,
    pub bundle_edges: bool,
    pub hide_primitive_fields: bool,
    pub hide_relay: bool,
    pub sidebar_open: bool,
    pub sidebar_width: f32,
    pub dock_height: f32,
    pub theme_mode: crate::theme::ThemeMode,
}

/// Drag limits for the two resizable panes, matching the web's clamps.
pub const SIDEBAR_MIN_W: f32 = 260.0;
pub const SIDEBAR_MAX_W: f32 = 720.0;
pub const SIDEBAR_DEFAULT_W: f32 = 340.0;
pub const DOCK_MIN_H: f32 = 160.0;
pub const DOCK_MAX_H: f32 = 720.0;
pub const DOCK_DEFAULT_H: f32 = 280.0;

impl Default for Settings {
    fn default() -> Self {
        Settings {
            show_descriptions: false,
            bundle_edges: true,
            hide_primitive_fields: false,
            hide_relay: true,
            sidebar_open: true,
            sidebar_width: SIDEBAR_DEFAULT_W,
            dock_height: DOCK_DEFAULT_H,
            theme_mode: crate::theme::ThemeMode::System,
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

pub fn search_history() -> Vec<String> {
    read_json("searches.json").unwrap_or_default()
}

pub fn push_search(query: &str) {
    let q = query.trim();
    if q.is_empty() {
        return;
    }
    let mut list = search_history();
    list.retain(|s| s != q);
    list.insert(0, q.to_string());
    list.truncate(12);
    write_json("searches.json", &list);
}

pub fn write_recents(list: &[RecentEntry]) {
    write_json("recent.json", &list);
}

/// Replace the persisted list — the sidebar's per-row delete / "Clear all".
pub fn set_search_history(list: &[String]) {
    write_json("searches.json", &list.to_vec());
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
