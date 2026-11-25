//! The app's state: signals, made once by the root and copied into every
//! handler. gpui kept this in three entities (Root, Workspace, the canvas)
//! that re-rendered on change; here a change reruns only what read it.

use std::{path::PathBuf, sync::Arc};

use graviz_core::graph::{OverlayDiff, ParsedGraph};
use nsh_core::prelude::*;

use crate::{
    config::{self, RecentEntry, ScrollMode, Settings, ThemeMode},
    graph::{self, canvas::HistoryItem},
    loader,
    model::{self, Mode, Model, ModelOptions},
};

/// Shared by pointer, so a signal of it compares in O(1).
pub struct Shared<T>(pub Arc<T>);

impl<T> Clone for Shared<T> {
    fn clone(&self) -> Self {
        Shared(self.0.clone())
    }
}

impl<T> PartialEq for Shared<T> {
    fn eq(&self, o: &Self) -> bool {
        Arc::ptr_eq(&self.0, &o.0)
    }
}

impl<T> std::ops::Deref for Shared<T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.0
    }
}

/// The top-level screen.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Route {
    New,
    View,
    About,
}

/// The open schema and everything derived from the file.
#[derive(Clone, PartialEq)]
pub struct Doc {
    pub path: PathBuf,
    pub name: String,
    pub full: Shared<ParsedGraph>,
    pub orphan_count: usize,
    pub deprecated_count: usize,
}

/// The overlay: what was applied, its effect, and why it was not.
#[derive(Clone, PartialEq, Default)]
pub struct Overlay {
    pub applied: Option<String>,
    pub diff: Option<Shared<OverlayDiff>>,
    pub error: Option<String>,
}

/// What the canvas last reported about itself.
#[derive(Clone, PartialEq, Default, Debug)]
pub struct CanvasState {
    pub focus: Option<u32>,
    pub edge: Option<u32>,
    pub pinned: Option<(u32, usize)>,
    pub history: Vec<HistoryItem>,
    pub offscreen: bool,
}

/// What is under the pointer on the canvas.
#[derive(Clone, PartialEq, Default, Debug)]
pub struct Hover {
    pub card: Option<u32>,
    /// ("row" | "implements" | "union", index)
    pub row: Option<(String, usize)>,
    pub edge: Option<u32>,
    pub x: f32,
    pub y: f32,
    pub k: f32,
    pub dragging: bool,
}

/// The canvas's right-click menu.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Menu {
    pub card: u32,
    pub row: Option<usize>,
    pub x: f32,
    pub y: f32,
}

/// The perf panel's numbers.
#[derive(Clone, PartialEq, Default, Debug)]
pub struct Perf {
    pub fps: f32,
    pub paint: f32,
    pub k: f32,
    pub hist: Vec<f32>,
}

#[derive(Clone, Copy)]
pub struct Store {
    pub route: Signal<Route>,
    pub settings_open: Signal<bool>,
    pub settings: Signal<Settings>,
    pub recents: Signal<Vec<RecentEntry>>,
    pub recents_open: Signal<bool>,
    pub landing_error: Signal<Option<String>>,
    /// A short notice across the top of the canvas (a hot reload), and
    /// which one it is, so an older one's timer does not clear a newer.
    pub toast: Signal<Option<(u64, String, String)>>,
    pub warnings: Signal<Vec<String>>,
    pub draft: Signal<String>,
    pub draft_name: Signal<Option<String>>,
    pub dragging: Signal<bool>,
    pub update: Signal<Option<(String, String)>>,

    pub doc: Signal<Option<Doc>>,
    pub overlay: Signal<Overlay>,
    pub overlay_draft: Signal<String>,
    pub mode: Signal<Mode>,
    pub root_override: Signal<Option<String>>,
    pub model: Signal<Option<Shared<Model>>>,
    /// The full graph built without layout, for the list panels.
    pub full_model: Signal<Option<Shared<Model>>>,
    /// The id the graph view takes the model by.
    pub model_id: Signal<u64>,
    pub investigate: Signal<bool>,
    pub highlight_overlay: Signal<bool>,
    pub dock_open: Signal<bool>,
    pub history_open: Signal<bool>,
    pub palette_open: Signal<bool>,
    /// Bumps each time the palette opens, so it starts empty.
    pub palette_gen: Signal<u64>,

    pub canvas: Signal<CanvasState>,
    pub hover: Signal<Hover>,
    pub menu: Signal<Option<Menu>>,
    pub perf: Signal<Perf>,
    pub graph_ref: ElementRef,
}

fn tab_counts(graph: &ParsedGraph) -> (usize, usize) {
    let orphans = model::slice_graph(graph, Mode::Orphaned, None).nodes.len();
    let deprecated = graph
        .nodes
        .iter()
        .map(|n| {
            n.fields.as_deref().unwrap_or(&[]).iter().filter(|f| f.is_deprecated).count()
                + n.values.as_deref().unwrap_or(&[]).iter().filter(|v| v.is_deprecated).count()
        })
        .sum();
    (orphans, deprecated)
}

impl Store {
    pub fn new() -> Self {
        let settings = config::load_settings();
        let mode = match std::env::var("GRAVIZ_MODE").as_deref() {
            Ok("orphaned") => Mode::Orphaned,
            Ok("deprecated") => Mode::Deprecated,
            _ => Mode::Reachable,
        };
        Store {
            route: signal(if std::env::var("GRAVIZ_ABOUT").is_ok() { Route::About } else { Route::New }),
            settings_open: signal(std::env::var("GRAVIZ_SETTINGS").is_ok()),
            settings: signal(settings),
            recents: signal(config::recents()),
            recents_open: signal(std::env::var("GRAVIZ_RECENTS").is_ok()),
            landing_error: signal(None),
            toast: signal(None),
            warnings: signal(Vec::new()),
            draft: signal(String::new()),
            draft_name: signal(None),
            dragging: signal(false),
            update: signal(None),
            doc: signal(None),
            overlay: signal(Overlay::default()),
            overlay_draft: signal(String::new()),
            mode: signal(mode),
            root_override: signal(None),
            model: signal(None),
            full_model: signal(None),
            model_id: signal(0),
            investigate: signal(std::env::var("GRAVIZ_INVESTIGATE").is_ok()),
            highlight_overlay: signal(false),
            dock_open: signal(std::env::var("GRAVIZ_DOCK").is_ok()),
            history_open: signal(false),
            palette_open: signal(std::env::var("GRAVIZ_PALETTE").is_ok()),
            palette_gen: signal(0),
            canvas: signal(CanvasState::default()),
            hover: signal(Hover::default()),
            menu: signal(None),
            perf: signal(Perf::default()),
            graph_ref: element_ref(),
        }
    }

    pub fn has_schema(&self) -> bool {
        self.doc.with(Option::is_some)
    }

    /* ---- settings ---- */

    /// Changes the settings and writes them to disk.
    pub fn set_settings(&self, f: impl FnOnce(&mut Settings)) {
        self.settings.update(f);
        config::save_settings(&self.settings.get_untracked());
    }

    pub fn options(&self) -> ModelOptions {
        let s = self.settings.get_untracked();
        let mut o = ModelOptions {
            show_descriptions: s.show_descriptions,
            bundle_edges: s.bundle_edges,
            hide_custom_scalars: s.hide_custom_scalars,
            hide_primitive_fields: s.hide_primitive_fields,
            ..Default::default()
        };
        if std::env::var("GRAVIZ_DESC").is_ok() {
            o.show_descriptions = true;
        }
        o
    }

    pub fn theme_mode(&self) -> ThemeMode {
        self.settings.with(|s| s.theme_mode)
    }

    pub fn scroll_mode(&self) -> ScrollMode {
        self.settings.with(|s| s.scroll_mode)
    }

    /* ---- opening a schema ---- */

    /// Opens `path`; on failure the landing shows why.
    pub fn open_path(&self, path: PathBuf) {
        let hide_relay = self.settings.get_untracked().hide_relay;
        match loader::load(&path, None, hide_relay) {
            Ok(loaded) => {
                self.landing_error.set(None);
                self.open_loaded(loaded, path);
            }
            Err(e) => {
                self.landing_error.set(Some(format!("{e:#}")));
            }
        }
        self.recents.set(config::recents());
    }

    /// The landing's "visualize": the pasted SDL.
    pub fn visualize(&self) {
        let sdl = self.draft.get_untracked();
        if sdl.trim().is_empty() {
            self.landing_error.set(Some("Paste an SDL or drop a .graphql file.".into()));
            return;
        }
        let hide_relay = self.settings.get_untracked().hide_relay;
        let name = self.draft_name.get_untracked().unwrap_or_else(|| "Pasted schema".to_string());
        match loader::load_sdl(&sdl, name, None, hide_relay) {
            Ok(loaded) if loaded.graph.nodes.is_empty() => {
                self.landing_error.set(Some("No types found in this SDL.".into()));
            }
            Ok(loaded) => {
                self.warnings.set(loaded.graph.warnings.clone());
                self.landing_error.set(None);
                self.open_loaded(loaded, PathBuf::from("(pasted)"));
            }
            Err(e) => self.landing_error.set(Some(format!("{e:#}"))),
        }
    }

    /// The open dialog: a schema file, then open it.
    pub fn open_dialog(&self) {
        let store = *self;
        spawn(async move {
            let picked = pick_paths(nsh_core::file_dialog::OpenDialog::files().title("Open schema")).await;
            if let Some(path) = picked.ok().flatten().into_iter().flatten().next() {
                store.open_path(path);
            }
        });
    }

    fn open_loaded(&self, loaded: loader::LoadedSchema, path: PathBuf) {
        let (orphans, deprecated) = tab_counts(&loaded.graph);
        self.overlay.set(Overlay { applied: None, diff: loaded.diff.map(|d| Shared(Arc::new(d))), error: None });
        self.overlay_draft.set(String::new());
        self.root_override.set(None);
        self.doc.set(Some(Doc {
            path,
            name: loaded.name,
            full: Shared(Arc::new(loaded.graph)),
            orphan_count: orphans,
            deprecated_count: deprecated,
        }));
        self.route.set(Route::View);
        self.rebuild();
    }

    /// Re-reads the schema file (and re-applies the overlay). Hot reload,
    /// the relay toggle, and the overlay's apply and clear all land here.
    pub fn reload_from_disk(&self) -> bool {
        let Some(doc) = self.doc.get_untracked() else { return false };
        if doc.path.as_os_str() == "(pasted)" {
            // A pasted schema has no file to re-read; re-run what was pasted.
            let sdl = self.draft.get_untracked();
            let hide_relay = self.settings.get_untracked().hide_relay;
            let overlay = self.overlay.get_untracked().applied;
            return match loader::load_sdl(&sdl, doc.name.clone(), overlay.as_deref(), hide_relay) {
                Ok(loaded) => {
                    self.reloaded(doc, loaded);
                    true
                }
                Err(e) => {
                    self.overlay.update(|o| o.error = Some(format!("{e:#}")));
                    false
                }
            };
        }
        let hide_relay = self.settings.get_untracked().hide_relay;
        let overlay = self.overlay.get_untracked().applied;
        match loader::load(&doc.path, overlay.as_deref(), hide_relay) {
            Ok(loaded) => {
                eprintln!("reloaded {}", doc.path.display());
                self.reloaded(doc, loaded);
                true
            }
            Err(e) => {
                self.overlay.update(|o| o.error = Some(format!("{e:#}")));
                false
            }
        }
    }

    /// Shows `title` and `message` across the top of the canvas for a few
    /// seconds.
    pub fn notify(&self, title: impl Into<String>, message: impl Into<String>) {
        let id = self.toast.get_untracked().map_or(1, |(id, _, _)| id + 1);
        self.toast.set(Some((id, title.into(), message.into())));
        let toast = self.toast;
        let _ = set_timeout(std::time::Duration::from_millis(2500), move || {
            if toast.get_untracked().is_some_and(|(at, _, _)| at == id) {
                toast.set(None);
            }
        });
    }

    fn reloaded(&self, doc: Doc, loaded: loader::LoadedSchema) {
        let (orphans, deprecated) = tab_counts(&loaded.graph);
        self.overlay.update(|o| {
            o.diff = loaded.diff.map(|d| Shared(Arc::new(d)));
            o.error = None;
        });
        self.doc.set(Some(Doc {
            name: loaded.name,
            full: Shared(Arc::new(loaded.graph)),
            orphan_count: orphans,
            deprecated_count: deprecated,
            ..doc
        }));
        self.rebuild();
    }

    /// Re-slices and re-lays-out the open schema with the current options.
    pub fn rebuild(&self) {
        let Some(doc) = self.doc.get_untracked() else { return };
        let options = self.options();
        let t = std::time::Instant::now();
        let mut sliced =
            model::slice_graph(&doc.full, self.mode.get_untracked(), self.root_override.get_untracked().as_deref());
        // Only the canvas slice loses its scalars; the list panels work off
        // the full graph and should still list them.
        if options.hide_custom_scalars {
            model::drop_custom_scalars(&mut sliced);
        }
        let built = Arc::new(model::build_model(sliced, doc.name.clone(), &options));
        if std::env::var("GRAVIZ_PERF").is_ok() {
            eprintln!(
                "perf: layout+model {}ms — {} cards, {} edges",
                t.elapsed().as_millis(),
                built.cards.len(),
                built.edges.len()
            );
        }
        let full = model::build_model(
            (*doc.full).clone(),
            doc.name.clone(),
            &ModelOptions { skip_layout: true, ..options },
        );
        self.full_model.set(Some(Shared(Arc::new(full))));
        let id = graph::publish(built.clone());
        self.canvas.set(CanvasState::default());
        self.hover.set(Hover::default());
        self.menu.set(None);
        self.model.set(Some(Shared(built)));
        self.model_id.set(id);
    }

    pub fn set_mode(&self, mode: Mode) {
        if self.mode.get_untracked() != mode {
            self.mode.set(mode);
            self.rebuild();
        }
    }

    /// A view switch that rebuilds the model.
    pub fn toggle_option(&self, f: impl FnOnce(&mut Settings)) {
        self.set_settings(f);
        self.rebuild();
    }

    pub fn toggle_relay(&self) {
        self.set_settings(|s| s.hide_relay = !s.hide_relay);
        self.reload_from_disk();
    }

    /* ---- overlay ---- */

    pub fn apply_overlay(&self) {
        let text = self.overlay_draft.get_untracked();
        self.overlay.update(|o| o.applied = (!text.trim().is_empty()).then_some(text));
        self.reload_from_disk();
    }

    pub fn clear_overlay(&self) {
        self.overlay_draft.set(String::new());
        self.overlay.set(Overlay::default());
        self.reload_from_disk();
    }

    /// Loads an overlay SDL file into the editor and applies it.
    pub fn open_overlay_file(&self) {
        let store = *self;
        spawn(async move {
            let picked =
                pick_paths(nsh_core::file_dialog::OpenDialog::files().title("Load overlay SDL into the editor")).await;
            if let Some(path) = picked.ok().flatten().into_iter().flatten().next() {
                if let Ok(text) = std::fs::read_to_string(&path) {
                    store.dock_open.set(true);
                    store.overlay_draft.set(text);
                    store.apply_overlay();
                }
            }
        });
    }

    /// A schema opened from the command line or a debug preset.
    pub fn open_initial(&self, path: PathBuf, overlay: Option<String>) {
        let hide_relay = self.settings.get_untracked().hide_relay;
        let t0 = std::time::Instant::now();
        match loader::load(&path, overlay.as_deref(), hide_relay) {
            Ok(loaded) => {
                // The same span as the gpui app's: reading and parsing only.
                eprintln!(
                    "parsed in {}ms — {} types, {} edges",
                    t0.elapsed().as_millis(),
                    loaded.graph.nodes.len(),
                    loaded.graph.edges.len()
                );
                if let Some(text) = overlay {
                    self.open_loaded(loaded, path);
                    self.overlay_draft.set(text.clone());
                    self.overlay.update(|o| o.applied = Some(text));
                    self.reload_from_disk();
                } else {
                    self.open_loaded(loaded, path);
                }
            }
            Err(e) => {
                eprintln!("{e:#}");
                std::process::exit(1);
            }
        }
    }

    /* ---- the canvas ---- */

    pub fn command(&self, name: &str, args: serde_json::Value) {
        self.graph_ref.command(name, args);
    }

    pub fn navigate_to(&self, card: u32, row: Option<usize>) {
        self.command("navigate", serde_json::json!({ "card": card, "row": row }));
    }

    pub fn navigate_to_type(&self, name: &str) {
        let card = self.model.with_untracked(|m| m.as_ref().and_then(|m| m.index_of.get(name).copied()));
        if let Some(card) = card {
            self.navigate_to(card, None);
        }
    }

    /// The graph view reported something.
    pub fn on_canvas_event(&self, name: &str, data: &serde_json::Value) {
        use serde_json::Value;
        let u32_of = |v: &Value| v.as_u64().map(|n| n as u32);
        match name {
            "state" => {
                let pinned = data["pinned"].as_array().and_then(|a| {
                    Some((a.first()?.as_u64()? as u32, a.get(1)?.as_u64()? as usize))
                });
                self.canvas.set(CanvasState {
                    focus: u32_of(&data["focus"]),
                    edge: u32_of(&data["edge"]),
                    pinned,
                    history: data["history"]
                        .as_array()
                        .map(|a| a.iter().filter_map(HistoryItem::from_json).collect())
                        .unwrap_or_default(),
                    offscreen: data["offscreen"].as_bool().unwrap_or(false),
                });
            }
            "hover" => {
                let row = data["row"].as_array().and_then(|a| {
                    Some((a.first()?.as_str()?.to_string(), a.get(1)?.as_u64()? as usize))
                });
                self.hover.set(Hover {
                    card: u32_of(&data["card"]),
                    row,
                    edge: u32_of(&data["edge"]),
                    x: data["x"].as_f64().unwrap_or(0.0) as f32,
                    y: data["y"].as_f64().unwrap_or(0.0) as f32,
                    k: data["k"].as_f64().unwrap_or(1.0) as f32,
                    dragging: data["dragging"].as_bool().unwrap_or(false),
                });
            }
            "menu" => {
                self.menu.set(u32_of(&data["card"]).map(|card| Menu {
                    card,
                    row: data["row"].as_u64().map(|r| r as usize),
                    x: data["x"].as_f64().unwrap_or(0.0) as f32,
                    y: data["y"].as_f64().unwrap_or(0.0) as f32,
                }));
            }
            "perf" => {
                self.perf.set(Perf {
                    fps: data["fps"].as_f64().unwrap_or(0.0) as f32,
                    paint: data["paint"].as_f64().unwrap_or(0.0) as f32,
                    k: data["k"].as_f64().unwrap_or(1.0) as f32,
                    hist: data["hist"]
                        .as_array()
                        .map(|a| a.iter().filter_map(|v| v.as_f64().map(|f| f as f32)).collect())
                        .unwrap_or_default(),
                });
            }
            _ => {}
        }
    }
}
