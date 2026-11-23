//! Two-pane workspace: tree sidebar + graph canvas, with a mode tab bar,
//! view toggles, the in-app overlay editor dock, file watching, and global
//! shortcuts (⌘K search, ⌘B sidebar, ⌘D descriptions, ⌘E bundling,
//! ⌘I investigate, ⌘P primitives, ⌘R relay, ⌘U overlay dock, ⌘O open,
//! ⌘⇧O load overlay file, ⌘[ back, ⌘↵ apply overlay).

use crate::canvas::GraphCanvas;
use crate::config;
use crate::editor::{EditorEvent, TextArea};
use crate::icons::{icon, Icon};
use crate::loader;
use crate::model::{build_model, slice_graph, Mode, Model, ModelOptions};
use crate::panels::{OrphanPanel, PanelEvent, UntilPanel};
use crate::theme::Theme;
use crate::tree::{TreeEvent, TreePanel};
use crate::tween::TweenExt;
use gpui::{
    actions, div, prelude::*, px, App, Context, Entity, FocusHandle, Focusable, KeyBinding,
    PathPromptOptions, SharedString, StyleRefinement, Window,
};
use gpui_metro::theme::{LIGHT, SEMIBOLD};
use graviz_core::graph::{OverlayDiff, ParsedGraph};
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

actions!(
    graviz,
    [
        FocusSearch,
        ToggleSidebar,
        ToggleDescriptions,
        ToggleBundling,
        ToggleCustomScalars,
        ToggleInvestigate,
        TogglePrimitives,
        ToggleRelay,
        ToggleOverlayDock,
        OpenSchema,
        OpenOverlay,
        OpenSettings,
        Back,
        ClearSelection
    ]
);

pub fn init(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("cmd-k", FocusSearch, None),
        KeyBinding::new("cmd-b", ToggleSidebar, None),
        KeyBinding::new("cmd-d", ToggleDescriptions, None),
        KeyBinding::new("cmd-e", ToggleBundling, None),
        KeyBinding::new("cmd-shift-s", ToggleCustomScalars, None),
        KeyBinding::new("cmd-i", ToggleInvestigate, None),
        KeyBinding::new("cmd-p", TogglePrimitives, None),
        KeyBinding::new("cmd-r", ToggleRelay, None),
        KeyBinding::new("cmd-u", ToggleOverlayDock, None),
        KeyBinding::new("cmd-o", OpenSchema, None),
        KeyBinding::new("cmd-shift-o", OpenOverlay, None),
        // The macOS convention for Preferences. Nothing under the workspace
        // claims it, so it bubbles to the root, which owns the route.
        KeyBinding::new("cmd-,", OpenSettings, None),
        KeyBinding::new("cmd-[", Back, None),
        KeyBinding::new("escape", ClearSelection, None),
    ]);
}

/// The two draggable pane edges.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Splitter {
    Sidebar,
    Dock,
}

/// A 5px grab strip on a pane's edge. It only records which splitter went
/// down; the move and release are handled on the workspace root, so the drag
/// keeps working after the cursor outruns the strip.
fn splitter(
    id: &'static str,
    which: Splitter,
    th: crate::theme::Theme,
    active: bool,
    cx: &mut Context<Workspace>,
) -> impl IntoElement {
    let vertical = matches!(which, Splitter::Sidebar);
    div()
        .id(id)
        .absolute()
        .when(vertical, |el| {
            el.top_0().bottom_0().right(px(-2.0)).w(px(5.0))
        })
        .when(!vertical, |el| {
            el.left_0().right_0().top(px(-2.0)).h(px(5.0))
        })
        .cursor(if vertical {
            gpui::CursorStyle::ResizeLeftRight
        } else {
            gpui::CursorStyle::ResizeUpDown
        })
        .when(active, |el| el.bg(th.accent.opacity(0.5)))
        .hover(|el| el.bg(th.accent.opacity(0.35)))
        .on_mouse_down(
            gpui::MouseButton::Left,
            cx.listener(move |this, _, _, cx| {
                this.resizing = Some(which);
                cx.notify();
            }),
        )
}

pub struct Workspace {
    schema_path: PathBuf,
    full_graph: ParsedGraph,
    schema_name: String,
    mode: Mode,
    options: ModelOptions,
    hide_relay: bool,
    investigate: bool,
    root_override: Option<String>,
    tree: Entity<TreePanel>,
    /// The ⌘K palette, and whether it is up. It lives here rather than in the
    /// root because choosing a hit has to reach the canvas.
    palette: Entity<crate::palette::Palette>,
    palette_open: bool,
    /// Set when the palette closes: the keyboard has to come back here, or
    /// the focused node is one that no longer exists and every shortcut on
    /// this workspace stops answering.
    reclaim_focus: bool,
    orphan_panel: Entity<OrphanPanel>,
    until_panel: Entity<UntilPanel>,
    canvas: Entity<GraphCanvas>,
    overlay_editor: Entity<TextArea>,
    /// The workspace's own focus. Actions dispatch along the focus path, so
    /// without a handle here nothing but the tree and the editor could receive
    /// a shortcut — and neither of those holds focus when a schema first
    /// opens, which left every hotkey dead until something was clicked.
    focus: FocusHandle,
    focused_once: bool,
    sidebar_open: bool,
    sidebar_width: f32,
    /// When `sidebar_open` last flipped — the expand button and the view
    /// controls' +44px shift wait out the 300ms collapse before appearing,
    /// so they don't pop in on top of the panel while it is still closing.
    sidebar_toggled_at: Option<std::time::Instant>,
    /// Which splitter is being dragged, if any.
    resizing: Option<Splitter>,
    /// Latest built model, for name→card navigation from the overlay dock.
    model: Rc<Model>,
    /// The last APPLIED overlay text (reloads re-apply it).
    overlay_text: Option<String>,
    overlay_diff: Option<OverlayDiff>,
    overlay_error: Option<String>,
    dock_open: bool,
    /// Tab badge counts, recomputed only when the schema changes.
    history_open: bool,
    dock_height: f32,
    highlight_overlay: bool,
    orphan_count: usize,
    deprecated_count: usize,
    /// Hover state of the three floating canvas-overlay cards, each faded to
    /// 0.4 opacity at rest and animated to 1.0 on hover.
    controls_hovered: bool,
    investigate_hovered: bool,
    recent_hovered: bool,
}

/// Web's "0.4 opacity at rest, 1.0 on hover" fade, shared by the three
/// floating canvas-overlay cards (view controls / investigate / recent).
fn hover_fade<E: IntoElement + gpui::Styled + 'static>(
    el: E,
    id: &'static str,
    hovered: bool,
    window: &mut Window,
    cx: &mut App,
) -> gpui::AnyElement {
    let tw = crate::tween::track(id, if hovered { 1.0 } else { 0.0 }, window, cx);
    el.tween(tw, move |el, t| el.opacity(0.4 + 0.6 * t.clamp(0.0, 1.0)))
}

/// Types no root operation can reach, and deprecated members — the two tab
/// badges the web shows next to Orphaned / Deprecated.
fn tab_counts(graph: &ParsedGraph) -> (usize, usize) {
    let orphans = slice_graph(graph, Mode::Orphaned, None).nodes.len();
    let deprecated = graph
        .nodes
        .iter()
        .map(|n| {
            n.fields
                .as_deref()
                .unwrap_or(&[])
                .iter()
                .filter(|f| f.is_deprecated)
                .count()
                + n.values
                    .as_deref()
                    .unwrap_or(&[])
                    .iter()
                    .filter(|v| v.is_deprecated)
                    .count()
        })
        .sum();
    (orphans, deprecated)
}

impl Workspace {
    pub fn new(
        loaded: loader::LoadedSchema,
        schema_path: PathBuf,
        initial_overlay: Option<String>,
        cx: &mut Context<Self>,
    ) -> Self {
        // Debug: GRAVIZ_MODE=orphaned|deprecated opens on that tab so
        // selfshots can verify each sidebar body.
        let mode = match std::env::var("GRAVIZ_MODE").as_deref() {
            Ok("orphaned") => Mode::Orphaned,
            Ok("deprecated") => Mode::Deprecated,
            _ => Mode::Reachable,
        };
        let settings = config::load_settings();
        let mut options = ModelOptions {
            show_descriptions: settings.show_descriptions,
            bundle_edges: settings.bundle_edges,
            hide_custom_scalars: settings.hide_custom_scalars,
            hide_primitive_fields: settings.hide_primitive_fields,
            ..Default::default()
        };
        let sidebar_open = settings.sidebar_open;
        let sidebar_width = settings
            .sidebar_width
            .clamp(config::SIDEBAR_MIN_W, config::SIDEBAR_MAX_W);
        let dock_height = settings
            .dock_height
            .clamp(config::DOCK_MIN_H, config::DOCK_MAX_H);
        // Debug presets so automated selfshots can exercise toggle states.
        if std::env::var("GRAVIZ_DESC").is_ok() {
            options.show_descriptions = true;
        }
        let investigate = std::env::var("GRAVIZ_INVESTIGATE").is_ok();
        let t_layout = std::time::Instant::now();
        let mut sliced = slice_graph(&loaded.graph, mode, None);
        if options.hide_custom_scalars {
            crate::model::drop_custom_scalars(&mut sliced);
        }
        let model = Rc::new(build_model(sliced, loaded.name.clone(), &options));
        if std::env::var("GRAVIZ_PERF").is_ok() {
            eprintln!(
                "perf: layout+model {}ms — {} cards, {} edges",
                t_layout.elapsed().as_millis(),
                model.cards.len(),
                model.edges.len()
            );
        }
        let tree = cx.new(|cx| TreePanel::new(model.clone(), cx));
        let palette = cx.new(|cx| crate::palette::Palette::new(model.clone(), cx));
        // The Orphaned / Deprecated tab bodies work off the FULL graph, since
        // their whole point is what the reachable slice leaves out.
        let list_opts = ModelOptions {
            skip_layout: true,
            ..options.clone()
        };
        let full_model = Rc::new(build_model(
            loaded.graph.clone(),
            loaded.name.clone(),
            &list_opts,
        ));
        let orphan_panel = cx.new(|cx| OrphanPanel::new(full_model.clone(), cx));
        let until_panel = cx.new(|cx| UntilPanel::new(full_model, cx));
        let canvas = cx.new(|cx| {
            let mut c = GraphCanvas::new(model.clone());
            c.set_investigate(investigate, cx);
            c
        });
        let overlay_editor = cx.new(|cx| {
            let mut e = TextArea::new(cx);
            e.placeholder =
                "type User { newField: String }   ·   - Type.field removes   ·   ⌘↵ apply";
            if let Some(text) = &initial_overlay {
                e.set_text(text.clone(), cx);
            }
            e
        });
        cx.subscribe(
            &palette,
            |this: &mut Self, _, event: &crate::palette::PaletteEvent, cx| match event {
                crate::palette::PaletteEvent::Select { node_index, row } => {
                    let (node_index, row) = (*node_index, *row);
                    this.palette_open = false;
                    this.reclaim_focus = true;
                    this.canvas.update(cx, |canvas, cx| {
                        canvas.navigate_to(node_index as u32, row, cx);
                    });
                    cx.notify();
                }
                crate::palette::PaletteEvent::Dismiss => {
                    this.palette_open = false;
                    this.reclaim_focus = true;
                    cx.notify();
                }
            },
        )
        .detach();
        cx.subscribe(
            &tree,
            |this: &mut Self, _, event: &TreeEvent, cx| match event {
                TreeEvent::RootPicked(name) => {
                    this.root_override = Some(name.clone());
                    this.rebuild(cx);
                }
                TreeEvent::Select { node_index, row } => {
                    let (node_index, row) = (*node_index, *row);
                    this.canvas.update(cx, |canvas, cx| {
                        canvas.navigate_to(node_index as u32, row, cx);
                    });
                }
            },
        )
        .detach();
        for sub in [
            cx.subscribe(&orphan_panel, |this: &mut Self, _, e: &PanelEvent, cx| {
                this.on_panel_select(e, cx)
            }),
            cx.subscribe(&until_panel, |this: &mut Self, _, e: &PanelEvent, cx| {
                this.on_panel_select(e, cx)
            }),
        ] {
            sub.detach();
        }
        cx.subscribe(
            &overlay_editor,
            |this: &mut Self, _, event: &EditorEvent, cx| {
                if matches!(event, EditorEvent::Submitted) {
                    this.apply_overlay(cx);
                }
            },
        )
        .detach();

        // Watch the schema file and hot-reload on change (the overlay lives
        // in the editor, so only the schema needs watching).
        cx.spawn(async move |this, cx| {
            let mut last = 0u128;
            loop {
                cx.background_executor().timer(Duration::from_secs(1)).await;
                let Ok(schema) = this.update(cx, |this: &mut Self, _| this.schema_path.clone())
                else {
                    break;
                };
                let cur = loader::fingerprint(&schema);
                if last != 0
                    && cur != last
                    && this
                        .update(cx, |this: &mut Self, cx| this.reload_from_disk(cx))
                        .is_err()
                {
                    break;
                }
                last = cur;
            }
        })
        .detach();

        let counts = tab_counts(&loaded.graph);
        Self {
            schema_path,
            full_graph: loaded.graph,
            schema_name: loaded.name,
            mode,
            options,
            hide_relay: settings.hide_relay,
            investigate,
            root_override: None,
            tree,
            palette,
            palette_open: std::env::var("GRAVIZ_PALETTE").is_ok(),
            reclaim_focus: false,
            orphan_panel,
            until_panel,
            canvas,
            overlay_editor,
            focus: cx.focus_handle(),
            focused_once: false,
            sidebar_open,
            sidebar_width,
            sidebar_toggled_at: None,
            resizing: None,
            model,
            overlay_text: initial_overlay,
            overlay_diff: loaded.diff,
            overlay_error: None,
            dock_open: std::env::var("GRAVIZ_DOCK").is_ok(),
            history_open: false,
            dock_height,
            highlight_overlay: false,
            orphan_count: counts.0,
            deprecated_count: counts.1,
            controls_hovered: false,
            investigate_hovered: false,
            recent_hovered: false,
        }
    }

    /// A row click in the Orphaned / Deprecated tab focuses that type.
    fn on_panel_select(&mut self, event: &PanelEvent, cx: &mut Context<Self>) {
        let PanelEvent::Select { node_index, row } = event;
        let (node_index, row) = (*node_index, *row);
        // Panels index the full graph; map the id into the current slice.
        let name = self.model.graph.nodes.get(node_index).map(|n| n.id.clone());
        let card = name.and_then(|n| self.model.index_of.get(&n).copied());
        if let Some(card) = card {
            self.canvas.update(cx, |c, cx| c.navigate_to(card, row, cx));
        }
    }

    fn save_settings(&self, cx: &gpui::App) {
        config::save_settings(&config::Settings {
            show_descriptions: self.options.show_descriptions,
            bundle_edges: self.options.bundle_edges,
            hide_custom_scalars: self.options.hide_custom_scalars,
            hide_primitive_fields: self.options.hide_primitive_fields,
            hide_relay: self.hide_relay,
            sidebar_open: self.sidebar_open,
            sidebar_width: self.sidebar_width,
            dock_height: self.dock_height,
            // Both of these live in the settings page, not on this toolbar.
            // They are read back from their globals because this rewrites the
            // whole file: anything not named here would be reset.
            theme_mode: crate::theme::mode(cx),
            scroll_mode: config::scroll_mode(cx),
            accent: crate::theme::accent(cx).name().to_string(),
        });
    }

    fn rebuild(&mut self, cx: &mut Context<Self>) {
        // Only the canvas slice loses its scalars; the Orphaned and Deprecated
        // tabs work off the full graph and should still list them.
        let mut sliced = slice_graph(&self.full_graph, self.mode, self.root_override.as_deref());
        if self.options.hide_custom_scalars {
            crate::model::drop_custom_scalars(&mut sliced);
        }
        let model = Rc::new(build_model(sliced, self.schema_name.clone(), &self.options));
        self.model = model.clone();
        self.tree
            .update(cx, |tree, cx| tree.set_model(model.clone(), cx));
        self.palette.update(cx, |p, _| p.set_model(model.clone()));
        let list_opts = ModelOptions {
            skip_layout: true,
            ..self.options.clone()
        };
        let full_model = Rc::new(build_model(
            self.full_graph.clone(),
            self.schema_name.clone(),
            &list_opts,
        ));
        self.orphan_panel
            .update(cx, |p, cx| p.set_model(full_model.clone(), cx));
        self.until_panel
            .update(cx, |p, cx| p.set_model(full_model, cx));
        self.canvas.update(cx, |canvas, cx| {
            canvas.set_model(model, cx);
            canvas.set_investigate(self.investigate, cx);
        });
        cx.notify();
    }

    /// Path of the schema this workspace has open, for the titlebar.
    pub fn schema_path(&self) -> &std::path::Path {
        &self.schema_path
    }

    fn reload_from_disk(&mut self, cx: &mut Context<Self>) {
        match loader::load(
            &self.schema_path,
            self.overlay_text.as_deref(),
            self.hide_relay,
        ) {
            Ok(loaded) => {
                eprintln!("reloaded {}", self.schema_path.display());
                self.full_graph = loaded.graph;
                let counts = tab_counts(&self.full_graph);
                self.orphan_count = counts.0;
                self.deprecated_count = counts.1;
                self.schema_name = loaded.name;
                self.overlay_diff = loaded.diff;
                self.overlay_error = None;
                self.rebuild(cx);
            }
            Err(e) => {
                self.overlay_error = Some(format!("{e:#}"));
                cx.notify();
            }
        }
    }

    fn apply_overlay(&mut self, cx: &mut Context<Self>) {
        let text = self.overlay_editor.read(cx).text().to_string();
        self.overlay_text = (!text.trim().is_empty()).then_some(text);
        self.reload_from_disk(cx);
    }

    fn open_schema(&mut self, cx: &mut Context<Self>) {
        let rx = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Open schema".into()),
        });
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(mut paths))) = rx.await {
                if let Some(path) = paths.pop() {
                    let _ = this.update(cx, |this: &mut Self, cx| {
                        this.schema_path = path;
                        this.root_override = None;
                        this.reload_from_disk(cx);
                    });
                }
            }
        })
        .detach();
    }

    /// Load an overlay SDL file into the editor and apply it.
    fn open_overlay_file(&mut self, cx: &mut Context<Self>) {
        let rx = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Load overlay SDL into the editor".into()),
        });
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(mut paths))) = rx.await {
                if let Some(path) = paths.pop() {
                    if let Ok(text) = std::fs::read_to_string(&path) {
                        let _ = this.update(cx, |this: &mut Self, cx| {
                            this.dock_open = true;
                            this.overlay_editor.update(cx, |e, cx| e.set_text(text, cx));
                            this.apply_overlay(cx);
                        });
                    }
                }
            }
        })
        .detach();
    }

    fn clear_overlay(&mut self, cx: &mut Context<Self>) {
        self.overlay_editor
            .update(cx, |e, cx| e.set_text(String::new(), cx));
        self.overlay_text = None;
        self.overlay_diff = None;
        self.overlay_error = None;
        self.reload_from_disk(cx);
    }

    fn navigate_to_type(&mut self, name: &str, cx: &mut Context<Self>) {
        if let Some(&card) = self.model.index_of.get(name) {
            self.canvas
                .update(cx, |canvas, cx| canvas.navigate_to(card, None, cx));
        }
    }

    fn set_mode(&mut self, mode: Mode, cx: &mut Context<Self>) {
        if mode == self.mode {
            return;
        }
        self.mode = mode;
        self.rebuild(cx);
    }

    /// One sidebar mode as a Metro pivot header: a lowercase light word,
    /// foreground when active and subtle otherwise, its badge count a small
    /// superscript (accent on the active header).
    fn mode_tab(
        &self,
        th: Theme,
        mode: Mode,
        count: Option<usize>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let active = self.mode == mode;
        let disabled = matches!(count, Some(0));
        let fg = if disabled {
            th.text_faint
        } else if active {
            th.text
        } else {
            th.text_muted
        };
        let word = match mode {
            Mode::Reachable => "reachable",
            Mode::Orphaned => "orphaned",
            Mode::Deprecated => "deprecated",
        };
        div()
            .id(mode.label())
            .flex_none()
            .flex()
            .items_start()
            .whitespace_nowrap()
            .text_size(px(PIVOT_PX))
            .line_height(px(PIVOT_PX * 1.25))
            .font_weight(LIGHT)
            .text_color(fg)
            .when(!disabled && !active, |el| {
                el.cursor_pointer().hover(|el| el.text_color(th.text))
            })
            .child(word)
            .when_some(count.filter(|c| *c > 0), |el, c| {
                el.child(
                    div()
                        .ml(px(2.0))
                        .mt(px(3.0))
                        .text_size(px(11.0))
                        .line_height(px(12.0))
                        .font_weight(SEMIBOLD)
                        .text_color(if active { th.accent } else { th.text_muted })
                        .child(SharedString::from(c.to_string())),
                )
            })
            .when(!disabled, |el| {
                el.on_click(cx.listener(move |this, _, _, cx| this.set_mode(mode, cx)))
            })
    }

    /// A view switch on the canvas's floating controls: gpui-metro's check
    /// box, rendered through `scaled` so the phone-sized control (32 px box,
    /// 20 px label) sits at toolbar size with its proportions intact.
    fn chip<F>(
        &self,
        _th: Theme,
        id: &'static str,
        label: &'static str,
        active: bool,
        on_click: F,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<F>
    where
        F: Fn(&mut Self, &mut Window, &mut Context<Self>) + 'static,
    {
        let this = cx.entity().downgrade();
        gpui_metro::scaled(CONTROL_SCALE).child(
            gpui_metro::check_box(id, active)
                .label(label)
                .on_change(move |_, window, cx| {
                    this.update(cx, |this, cx| on_click(this, window, cx)).ok();
                }),
        )
    }
}

/// Size of the metro controls floating over the canvas, against their phone
/// size: 0.6 makes the label 12 px, the toolbar's old chip text.
const CONTROL_SCALE: f32 = 0.6;

/// Sidebar pivot header size: Metro's pivot type, scaled to a 340px column.
const PIVOT_PX: f32 = 24.0;

/// An app-bar button, desktop-sized: gpui-metro's round button (2 px ring,
/// accent flood and tilt while held, foreground disc when `filled`) over its
/// lowercase label. The whole column takes the click, label included.
fn bar_button(
    id: &'static str,
    glyph: gpui_metro::Icon,
    label: &'static str,
    th: Theme,
    enabled: bool,
    filled: bool,
) -> gpui::Stateful<gpui::Div> {
    let color = if enabled { th.text } else { th.text_faint };
    div()
        .id(id)
        .flex_none()
        .flex()
        .flex_col()
        .items_center()
        .gap(px(2.0))
        .w(px(60.0))
        .child(
            gpui_metro::round_button((id, 0usize), glyph)
                .diameter(30.0)
                .disabled(!enabled)
                .filled(filled),
        )
        .child(
            div()
                .text_size(px(10.0))
                .line_height(px(12.0))
                .text_color(color)
                .whitespace_nowrap()
                .child(label),
        )
        .when(enabled, |el| el.cursor_pointer())
}

/// Solid kind badge used in list rows (web `KIND_STYLES[kind].badge`).
pub fn kind_badge(th: Theme, kind: graviz_core::graph::NodeKind, label: &'static str) -> gpui::Div {
    div()
        .flex_none()
        .px(px(6.0))
        .text_size(px(9.0))
        .bg(th.kind_color(kind))
        .text_color(gpui::white())
        .child(SharedString::from(label))
}

impl Focusable for Workspace {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let th = crate::theme::current(cx, window.appearance());
        let vh = f32::from(window.viewport_size().height);
        let sidebar_tw = crate::tween::track(
            "sidebar-width",
            if self.sidebar_open {
                self.sidebar_width
            } else {
                0.0
            },
            window,
            cx,
        )
        .animate(self.resizing != Some(Splitter::Sidebar));

        // ---- sidebar pivot (web: the tab strip lives INSIDE the sidebar) ----
        // Metro pivot order: the selected header leads and the rest follow it,
        // wrapping; whatever runs past the column's edge is clipped, the way
        // a phone pivot trails off the screen.
        let mut modes: Vec<(Mode, Option<usize>)> = vec![
            (Mode::Reachable, None),
            (Mode::Orphaned, Some(self.orphan_count)),
        ];
        if self.deprecated_count > 0 {
            modes.push((Mode::Deprecated, Some(self.deprecated_count)));
        }
        if let Some(at) = modes.iter().position(|(m, _)| *m == self.mode) {
            modes.rotate_left(at);
        }
        let headers: Vec<_> = modes
            .into_iter()
            .map(|(m, c)| self.mode_tab(th, m, c, cx))
            .collect();
        let tab_row = div()
            .flex_none()
            .flex()
            .items_center()
            .pl(px(12.0))
            .pt(px(6.0))
            .pb(px(4.0))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .flex()
                    .items_end()
                    .gap(px(16.0))
                    .children(headers),
            )
            .child(
                div()
                    .id("collapse-sidebar")
                    .flex()
                    .items_center()
                    .justify_center()
                    .flex_none()
                    .size(px(36.0))
                    .cursor_pointer()
                    .hover(|el| el.bg(th.hover_bg))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.sidebar_open = false;
                        this.sidebar_toggled_at = Some(std::time::Instant::now());
                        this.canvas
                            .update(cx, |canvas, cx| canvas.set_pane_offset(0.0, cx));
                        this.save_settings(cx);
                        cx.notify();
                    }))
                    .child(icon(Icon::PanelLeftClose, px(16.0), th.text_muted)),
            );

        // ---- canvas overlay: floating "View controls" card (left 16 / top 16)
        // The sidebar itself takes ~350ms to visually collapse (see the
        // width spring below); anything that assumes it is already gone —
        // the expand button, the controls shift — waits for that instead of
        // popping in on top of the still-closing panel.
        let sidebar_settled = self
            .sidebar_toggled_at
            .is_none_or(|t| t.elapsed() >= Duration::from_millis(350));
        let inset = if !self.sidebar_open && sidebar_settled {
            44.0
        } else {
            0.0
        };
        let controls_hovered = self.controls_hovered;
        let controls = hover_fade(
            div()
                .id("view-controls")
                .absolute()
                .top(px(16.0 + inset))
                .left(px(16.0))
                .flex()
                .items_center()
                .gap(px(16.0))
                .bg(th.chrome_bg)
                .p(px(6.0))
                .on_hover(cx.listener(|this, hovered, _, cx| {
                    this.controls_hovered = *hovered;
                    cx.notify();
                }))
                .child(self.chip(
                    th,
                    "hide-primitives",
                    "hide primitives",
                    self.options.hide_primitive_fields,
                    |this, _, cx| {
                        this.options.hide_primitive_fields = !this.options.hide_primitive_fields;
                        this.rebuild(cx);
                        this.save_settings(cx);
                    },
                    cx,
                ))
                .child(self.chip(
                    th,
                    "hide-relay",
                    "hide relay",
                    self.hide_relay,
                    |this, _, cx| {
                        this.hide_relay = !this.hide_relay;
                        this.reload_from_disk(cx);
                        this.save_settings(cx);
                    },
                    cx,
                ))
                .child(self.chip(
                    th,
                    "show-descriptions",
                    "show descriptions",
                    self.options.show_descriptions,
                    |this, _, cx| {
                        this.options.show_descriptions = !this.options.show_descriptions;
                        this.rebuild(cx);
                        this.save_settings(cx);
                    },
                    cx,
                ))
                .child(self.chip(
                    th,
                    "bundle-edges",
                    "bundle edges",
                    self.options.bundle_edges,
                    |this, _, cx| {
                        this.options.bundle_edges = !this.options.bundle_edges;
                        this.rebuild(cx);
                        this.save_settings(cx);
                    },
                    cx,
                ))
                .child(self.chip(
                    th,
                    "hide-custom-scalars",
                    "hide custom scalars",
                    self.options.hide_custom_scalars,
                    |this, _, cx| {
                        this.options.hide_custom_scalars = !this.options.hide_custom_scalars;
                        this.rebuild(cx);
                        this.save_settings(cx);
                    },
                    cx,
                )),
            "controls",
            controls_hovered,
            window,
            cx,
        );

        // ---- canvas overlay: Investigate card (left 16 / top 56) ----
        let (documented, total) = self.model.desc_coverage;
        let coverage = if total > 0 {
            documented as f32 / total as f32
        } else {
            1.0
        };
        let cov_color = if coverage >= 0.9 {
            th.kind_color(graviz_core::graph::NodeKind::Enum)
        } else if coverage >= 0.5 {
            th.type_amber
        } else {
            th.kind_color(graviz_core::graph::NodeKind::Scalar)
        };
        let investigate_hovered = self.investigate_hovered;
        let investigate_card = hover_fade(
            div()
                .id("investigate-card")
                .absolute()
                .top(px(56.0 + inset))
                .left(px(16.0))
                .flex()
                .flex_col()
                .gap(px(4.0))
                .bg(th.chrome_bg)
                .p(px(6.0))
                .on_hover(cx.listener(|this, hovered, _, cx| {
                    this.investigate_hovered = *hovered;
                    cx.notify();
                }))
                // A Metro group title: lowercase, light, in the accent.
                .child(
                    div()
                        .text_size(px(16.0))
                        .line_height(px(18.0))
                        .font_weight(LIGHT)
                        .text_color(th.accent)
                        .child("investigate"),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(10.0))
                        .child({
                            let this = cx.entity().downgrade();
                            gpui_metro::scaled(CONTROL_SCALE).child(
                                gpui_metro::check_box("investigate-mode", self.investigate)
                                    .label("missing descriptions")
                                    .on_change(move |on, _, cx| {
                                        this.update(cx, |this, cx| {
                                            this.investigate = on;
                                            this.canvas.update(cx, |canvas, cx| {
                                                canvas.set_investigate(on, cx)
                                            });
                                            cx.notify();
                                        })
                                        .ok();
                                    }),
                            )
                        })
                        .child(
                            // Coverage as a bare Metro number: the type carries it.
                            div()
                                .text_size(px(20.0))
                                .line_height(px(24.0))
                                .font_weight(LIGHT)
                                .text_color(cov_color)
                                .child(SharedString::from(format!(
                                    "{}%",
                                    (coverage * 100.0).round() as i32
                                ))),
                        ),
                ),
            "investigate",
            investigate_hovered,
            window,
            cx,
        );

        // ---- overlay dock (web: collapsed strip by default) ----
        let counts = self.overlay_diff.as_ref().map(|d| {
            (
                d.added_types.len() + d.added_fields.len(),
                d.changed_fields.len(),
                d.removed_types.len() + d.removed_fields.len(),
            )
        });
        let applied = self.overlay_text.is_some();
        let dirty = {
            let draft = self.overlay_editor.read(cx).text().trim().to_string();
            draft != self.overlay_text.clone().unwrap_or_default().trim()
        };
        let can_highlight = counts.map(|(a, c, _)| a + c > 0).unwrap_or(false);

        // Flat solid count tiles: +added / ~overridden / −removed.
        let pill = |label: String, color: gpui::Hsla| {
            div()
                .px(px(5.0))
                .text_size(px(11.0))
                .line_height(px(16.0))
                .font_weight(SEMIBOLD)
                .bg(color)
                .text_color(gpui::white())
                .child(SharedString::from(label))
        };
        let count_pills = move |th: Theme| {
            let mut row = div().flex().items_center().gap(px(3.0));
            match counts {
                Some((a, c, r)) if a + c + r > 0 => {
                    if a > 0 {
                        row = row.child(pill(format!("+{a}"), th.overlay_green));
                    }
                    if c > 0 {
                        row = row.child(pill(format!("~{c}"), th.accent));
                    }
                    if r > 0 {
                        // U+2212 MINUS SIGN, like the web
                        row = row.child(pill(format!("\u{2212}{r}"), th.red));
                    }
                }
                Some(_) => {
                    row = row.child(
                        div()
                            .text_size(px(11.0))
                            .text_color(th.text_muted)
                            .child("no change"),
                    );
                }
                None => {}
            }
            row
        };
        // The dock's title, set like a Metro page title in miniature.
        let dock_title = |th: Theme, size: f32| {
            div()
                .flex_none()
                .text_size(px(size))
                .font_weight(LIGHT)
                .text_color(th.text)
                .child("overlay")
        };
        // The app bar's "···": here it opens and closes the dock.
        let more = |th: Theme, id: &'static str| {
            div()
                .id(id)
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .w(px(40.0))
                .h(px(28.0))
                .cursor_pointer()
                .child(gpui_metro::Icon::More.render(px(22.0), th.text))
        };

        // Web: the dock's collapsed strip vs. expanded editor is a height
        // spring, not a hard swap — a shared "dock-height" id lets it carry
        // the same in-flight height (and velocity) across the toggle even
        // though the two sides are otherwise unrelated element trees.
        let dock_open = self.dock_open;
        let dock_height = self.dock_height;
        let dock_collapsed_h = 34.0_f32;
        let dock_target_h = if dock_open {
            dock_height
        } else {
            dock_collapsed_h
        };
        let dock_tw = crate::tween::track("dock-height", dock_target_h, window, cx)
            .animate(self.resizing != Some(Splitter::Dock));
        let dock: gpui::AnyElement = if !self.dock_open {
            // Collapsed: the minimized app bar — one chrome line and "···".
            let status: gpui::AnyElement = match (&self.overlay_error, applied) {
                (Some(e), _) => div()
                    .flex_1()
                    .min_w_0()
                    .text_size(px(11.0))
                    .text_color(th.red)
                    .whitespace_nowrap()
                    .overflow_hidden()
                    .child(SharedString::from(format!("not applied — {e}")))
                    .into_any_element(),
                (None, true) => count_pills(th).flex_1().into_any_element(),
                (None, false) => div()
                    .flex_1()
                    .min_w_0()
                    .text_size(px(11.0))
                    .text_color(th.text_muted)
                    .child("sketch SDL on top of this schema")
                    .into_any_element(),
            };
            div()
                .id("dock-strip")
                .flex_none()
                .flex()
                .items_center()
                .gap(px(10.0))
                .bg(th.chrome_bg)
                .pl(px(12.0))
                .cursor_pointer()
                .on_click(cx.listener(|this, _, _, cx| {
                    this.dock_open = true;
                    cx.notify();
                }))
                .child(icon(Icon::Layers, px(14.0), th.overlay_green))
                .child(dock_title(th, 16.0))
                .child(status)
                .child(more(th, "dock-more"))
                .overflow_hidden()
                .tween(dock_tw, move |el, h| el.h(px(h)))
                .into_any_element()
        } else {
            let apply_enabled = dirty || !applied;
            let highlight_on = can_highlight && self.highlight_overlay;
            div()
                .flex_none()
                .overflow_hidden()
                .relative()
                .flex()
                .flex_col()
                .bg(th.bg)
                .child(splitter(
                    "dock-splitter",
                    Splitter::Dock,
                    th,
                    self.resizing == Some(Splitter::Dock),
                    cx,
                ))
                // The app bar: title + diff counts on the left, glyph
                // buttons with lowercase labels on the right, "···" folds it.
                .child(
                    div()
                        .flex_none()
                        .flex()
                        .items_center()
                        .gap(px(10.0))
                        .bg(th.chrome_bg)
                        .pl(px(12.0))
                        .py(px(6.0))
                        .child(icon(Icon::Layers, px(16.0), th.overlay_green))
                        .child(dock_title(th, 22.0))
                        .child(count_pills(th))
                        .child(div().flex_1())
                        .child(
                            div()
                                .flex_none()
                                .text_size(px(11.0))
                                .text_color(th.text_muted)
                                .child(SharedString::from(if dirty && applied {
                                    "unapplied edits · ⌘↵"
                                } else {
                                    "⌘↵"
                                })),
                        )
                        .child(
                            bar_button(
                                "overlay-highlight",
                                gpui_metro::Icon::Edit,
                                "highlight",
                                th,
                                can_highlight,
                                highlight_on,
                            )
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.highlight_overlay = !this.highlight_overlay;
                                let on = this.highlight_overlay;
                                this.canvas
                                    .update(cx, |c, cx| c.set_highlight_overlay(on, cx));
                                cx.notify();
                            })),
                        )
                        .child(
                            bar_button(
                                "overlay-apply",
                                gpui_metro::Icon::Check,
                                if applied && dirty {
                                    "re-apply"
                                } else {
                                    "apply"
                                },
                                th,
                                apply_enabled,
                                apply_enabled,
                            )
                            .on_click(cx.listener(|this, _, _, cx| this.apply_overlay(cx))),
                        )
                        .child(
                            bar_button(
                                "overlay-clear",
                                gpui_metro::Icon::Delete,
                                "clear",
                                th,
                                applied,
                                false,
                            )
                            .on_click(cx.listener(|this, _, _, cx| this.clear_overlay(cx))),
                        )
                        .child(
                            bar_button(
                                "overlay-load",
                                gpui_metro::Icon::Download,
                                "load file",
                                th,
                                true,
                                false,
                            )
                            .on_click(cx.listener(|this, _, _, cx| this.open_overlay_file(cx))),
                        )
                        .child(more(th, "overlay-collapse").on_click(cx.listener(
                            |this, _, _, cx| {
                                this.dock_open = false;
                                cx.notify();
                            },
                        ))),
                )
                // editor
                .child(
                    div().flex_1().min_h_0().p_2().child(
                        self.overlay_editor
                            .clone()
                            .cached(StyleRefinement::default().size_full()),
                    ),
                )
                // status strip
                .when_some(self.overlay_error.clone(), |el, e| {
                    el.child(
                        div()
                            .flex_none()
                            .bg(th.red.opacity(0.1))
                            .px_3()
                            .py_2()
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .text_size(px(10.0))
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .text_color(th.red)
                                    .child("OVERLAY NOT APPLIED"),
                            )
                            .child(
                                div()
                                    .text_size(px(10.0))
                                    .font_family("Menlo")
                                    .text_color(th.red.opacity(0.9))
                                    .child(SharedString::from(e)),
                            )
                            .child(div().text_size(px(10.0)).text_color(th.text_muted).child(
                                "The graph is still showing the schema without the overlay.",
                            )),
                    )
                })
                .when(self.overlay_error.is_none() && applied, |el| {
                    let d = self.overlay_diff.clone();
                    el.child(
                        div()
                            .flex_none()
                            .id("dock-diff")
                            .max_h(px(self.dock_height * 0.45))
                            .overflow_y_scroll()
                            .bg(th.overlay_green.opacity(0.1))
                            .px_3()
                            .py_2()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(
                                div()
                                    .text_size(px(10.0))
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .text_color(th.overlay_green)
                                    .child(SharedString::from(format!(
                                        "OVERLAY APPLIED ({})",
                                        change_label(counts)
                                    ))),
                            )
                            .when_some(d, |el, d| {
                                el.child(div().flex().flex_wrap().gap_1().children(
                                    d.added_types.into_iter().take(12).enumerate().map(
                                        |(i, name)| {
                                            let t = name.clone();
                                            div()
                                                .id(("added-type", i))
                                                .bg(th.overlay_green.opacity(0.2))
                                                .px(px(6.0))
                                                .py(px(1.0))
                                                .text_size(px(10.0))
                                                .font_family("Menlo")
                                                .text_color(th.overlay_green)
                                                .cursor_pointer()
                                                .on_click(cx.listener(move |this, _, _, cx| {
                                                    this.navigate_to_type(&t, cx)
                                                }))
                                                .child(SharedString::from(name))
                                        },
                                    ),
                                ))
                                .child(
                                    div()
                                        .flex()
                                        .flex_wrap()
                                        .gap_2()
                                        .text_size(px(10.0))
                                        .font_family("Menlo")
                                        .text_color(th.text_muted)
                                        .children(
                                            d.added_fields
                                                .into_iter()
                                                .take(12)
                                                .map(|f| SharedString::from(format!("+{f}"))),
                                        ),
                                )
                                .child(
                                    div()
                                        .flex()
                                        .flex_wrap()
                                        .gap_2()
                                        .text_size(px(10.0))
                                        .font_family("Menlo")
                                        .text_color(th.accent.opacity(0.9))
                                        .children(
                                            d.changed_fields
                                                .into_iter()
                                                .take(12)
                                                .map(|f| SharedString::from(format!("~{f}"))),
                                        ),
                                )
                                .child(
                                    div()
                                        .flex()
                                        .flex_wrap()
                                        .gap_2()
                                        .text_size(px(10.0))
                                        .font_family("Menlo")
                                        .text_color(th.red.opacity(0.8))
                                        .children(
                                            d.removed_types
                                                .into_iter()
                                                .chain(d.removed_fields)
                                                .take(12)
                                                .map(|f| {
                                                    SharedString::from(format!("\u{2212}{f}"))
                                                }),
                                        ),
                                )
                            }),
                    )
                })
                .tween(dock_tw, move |el, h| el.h(px(h)))
                .into_any_element()
        };

        // ---- canvas overlay: click-history "Recent" panel (right 16 / top 16)
        let recent_hovered = self.recent_hovered;
        let recent = {
            let entries = self.canvas.read(cx).history_entries();
            let open = self.history_open;
            (!entries.is_empty()).then(|| {
                let count = entries.len();
                hover_fade(
                    div()
                        .id("recent-panel")
                        .absolute()
                        .top(px(16.0))
                        .right(px(16.0))
                        .w(px(256.0))
                        .bg(th.chrome_bg)
                        .flex()
                        .flex_col()
                        .on_hover(cx.listener(|this, hovered, _, cx| {
                            this.recent_hovered = *hovered;
                            cx.notify();
                        }))
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_2()
                                .px_3()
                                .py(px(6.0))
                                .child(
                                    div()
                                        .flex_1()
                                        .flex()
                                        .items_start()
                                        .gap(px(2.0))
                                        .child(
                                            div()
                                                .text_size(px(18.0))
                                                .font_weight(LIGHT)
                                                .text_color(th.text)
                                                .child("recent"),
                                        )
                                        .child(
                                            div()
                                                .mt(px(2.0))
                                                .text_size(px(10.0))
                                                .font_weight(SEMIBOLD)
                                                .text_color(th.text_muted)
                                                .child(SharedString::from(count.to_string())),
                                        ),
                                )
                                .child(
                                    div()
                                        .id("clear-history")
                                        .p_1()
                                        .cursor_pointer()
                                        .hover(|el| el.bg(th.hover_bg))
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.canvas
                                                .update(cx, |canvas, cx| canvas.clear_history(cx));
                                            cx.notify();
                                        }))
                                        .child(gpui_metro::icon(
                                            gpui_metro::Icon::Delete,
                                            px(12.0),
                                            th.text_muted,
                                        )),
                                )
                                .child(
                                    div()
                                        .id("toggle-history")
                                        .p_1()
                                        .cursor_pointer()
                                        .hover(|el| el.bg(th.hover_bg))
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.history_open = !this.history_open;
                                            cx.notify();
                                        }))
                                        .child(icon(
                                            if open {
                                                Icon::ChevronUp
                                            } else {
                                                Icon::ChevronDown
                                            },
                                            px(12.0),
                                            th.text_muted,
                                        )),
                                ),
                        )
                        .when(open, |el| {
                            el.child(
                                div()
                                    .id("recent-list")
                                    // 60vh, matching the web — a fixed height
                                    // wastes a tall window and overflows a short
                                    // one.
                                    .max_h(px(vh * 0.6))
                                    .overflow_y_scroll()
                                    .py_1()
                                    .flex()
                                    .flex_col()
                                    .children(entries.into_iter().enumerate().map(|(i, entry)| {
                                        let item = entry.item;
                                        div()
                                            .id(("recent", i))
                                            .group("recent-row")
                                            .flex()
                                            .items_center()
                                            .gap_2()
                                            .px_2()
                                            .py(px(2.0))
                                            .child(
                                                div()
                                                    .id(("recent-go", i))
                                                    .flex()
                                                    .flex_1()
                                                    .min_w_0()
                                                    .items_center()
                                                    .gap_2()
                                                    .px_2()
                                                    .py_1()
                                                    .cursor_pointer()
                                                    .bg(th.kind_color(entry.kind).opacity(0.1))
                                                    .hover(|el| el.bg(th.hover_bg))
                                                    .on_click(cx.listener(move |this, _, _, cx| {
                                                        this.canvas.update(cx, |canvas, cx| {
                                                            canvas.revisit(item, cx)
                                                        });
                                                    }))
                                                    .child(kind_badge(
                                                        th,
                                                        entry.kind,
                                                        entry.kind_label,
                                                    ))
                                                    .child(
                                                        div()
                                                            .flex_1()
                                                            .min_w_0()
                                                            .text_xs()
                                                            .font_family("Menlo")
                                                            .text_color(th.kind_color(entry.kind))
                                                            .whitespace_nowrap()
                                                            .overflow_hidden()
                                                            .child(entry.label),
                                                    ),
                                            )
                                            .child(
                                                div()
                                                    .id(("recent-x", i))
                                                    .p_1()
                                                    .cursor_pointer()
                                                    .hover(|el| el.bg(th.hover_bg))
                                                    .on_click(cx.listener(move |this, _, _, cx| {
                                                        this.canvas.update(cx, |canvas, cx| {
                                                            canvas.remove_history(item, cx)
                                                        });
                                                        cx.notify();
                                                    }))
                                                    .child(gpui_metro::icon(
                                                        gpui_metro::Icon::Close,
                                                        px(12.0),
                                                        th.text_muted,
                                                    )),
                                            )
                                    })),
                            )
                        }),
                    "recent",
                    recent_hovered,
                    window,
                    cx,
                )
            })
        };

        if self.reclaim_focus {
            self.reclaim_focus = false;
            window.focus(&self.focus, cx);
        }
        if !self.focused_once {
            self.focused_once = true;
            if self.palette_open {
                self.palette.update(cx, |p, cx| {
                    p.debug_query();
                    p.reopen_keeping_query(window, cx);
                });
            } else {
                window.focus(&self.focus, cx);
            }
            // Debug: GRAVIZ_FOCUS=<TypeName> opens with that card focused, so
            // a selfshot can reproduce the focused state.
            if let Ok(name) = std::env::var("GRAVIZ_FOCUS") {
                cx.defer_in(window, move |this: &mut Self, _, cx| {
                    this.navigate_to_type(&name, cx)
                });
            }
        }
        div()
            .flex()
            .relative()
            .size_full()
            .font_family(gpui_metro::Theme::font(cx))
            .track_focus(&self.focus)
            .key_context("Workspace")
            // A drag started on a splitter is tracked here, not on the strip
            // itself: once the cursor outruns the 5px grab area the strip
            // stops seeing moves, and the pane would stick mid-drag.
            .on_mouse_move(cx.listener(|this, ev: &gpui::MouseMoveEvent, window, cx| {
                let Some(which) = this.resizing else { return };
                match which {
                    Splitter::Sidebar => {
                        this.sidebar_width = f32::from(ev.position.x)
                            .clamp(config::SIDEBAR_MIN_W, config::SIDEBAR_MAX_W);
                        let w = this.sidebar_width;
                        this.canvas.update(cx, |c, cx| c.set_pane_offset(w, cx));
                    }
                    Splitter::Dock => {
                        let vh = f32::from(window.viewport_size().height);
                        this.dock_height = (vh - f32::from(ev.position.y))
                            .clamp(config::DOCK_MIN_H, config::DOCK_MAX_H.min(vh * 0.7));
                    }
                }
                cx.notify();
            }))
            .on_mouse_up(
                gpui::MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    if this.resizing.take().is_some() {
                        this.save_settings(cx);
                        cx.notify();
                    }
                }),
            )
            // ⌘K used to open the sidebar and put the caret in its search
            // box. It opens the palette now: a shortcut for "find me a type"
            // should not depend on a pane being there, or move the layout.
            .on_action(cx.listener(|this, _: &FocusSearch, window, cx| {
                // A second press closes it, the way ⌘, closes Settings.
                this.palette_open = !this.palette_open;
                if this.palette_open {
                    this.palette.update(cx, |p, cx| p.reopen(window, cx));
                } else {
                    window.focus(&this.focus, cx);
                }
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &ToggleSidebar, _, cx| {
                this.sidebar_open = !this.sidebar_open;
                this.sidebar_toggled_at = Some(std::time::Instant::now());
                let offset = if this.sidebar_open {
                    this.sidebar_width
                } else {
                    0.0
                };
                this.canvas
                    .update(cx, |canvas, cx| canvas.set_pane_offset(offset, cx));
                this.save_settings(cx);
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &ToggleDescriptions, _, cx| {
                this.options.show_descriptions = !this.options.show_descriptions;
                this.rebuild(cx);
                this.save_settings(cx);
            }))
            .on_action(cx.listener(|this, _: &ToggleCustomScalars, _, cx| {
                this.options.hide_custom_scalars = !this.options.hide_custom_scalars;
                this.rebuild(cx);
                this.save_settings(cx);
            }))
            .on_action(cx.listener(|this, _: &ToggleBundling, _, cx| {
                this.options.bundle_edges = !this.options.bundle_edges;
                this.rebuild(cx);
                this.save_settings(cx);
            }))
            .on_action(cx.listener(|this, _: &TogglePrimitives, _, cx| {
                this.options.hide_primitive_fields = !this.options.hide_primitive_fields;
                this.rebuild(cx);
                this.save_settings(cx);
            }))
            .on_action(cx.listener(|this, _: &ToggleRelay, _, cx| {
                this.hide_relay = !this.hide_relay;
                this.reload_from_disk(cx);
                this.save_settings(cx);
            }))
            .on_action(cx.listener(|this, _: &ToggleInvestigate, _, cx| {
                this.investigate = !this.investigate;
                let investigate = this.investigate;
                this.canvas
                    .update(cx, |canvas, cx| canvas.set_investigate(investigate, cx));
            }))
            .on_action(cx.listener(|this, _: &OpenSchema, _, cx| this.open_schema(cx)))
            .on_action(cx.listener(|this, _: &OpenOverlay, _, cx| this.open_overlay_file(cx)))
            .on_action(cx.listener(|this, _: &ToggleOverlayDock, _, cx| {
                this.dock_open = !this.dock_open;
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &Back, _, cx| {
                this.canvas.update(cx, |canvas, cx| canvas.go_back(cx));
            }))
            .on_action(cx.listener(|this, _: &ClearSelection, _, cx| {
                this.canvas
                    .update(cx, |canvas, cx| canvas.clear_focused_edge(cx));
            }))
            .child({
                // Web: sidebar collapse is a 300ms width animation, not a
                // hard show/hide — the panel always mounts, clipping its
                // fixed-width (unreflowed) content as the outer box narrows.
                let width = self.sidebar_width;
                div()
                    .h_full()
                    .flex_none()
                    .overflow_hidden()
                    .bg(th.panel)
                    .child(
                        div()
                            .w(px(width))
                            .h_full()
                            .flex_none()
                            .flex()
                            .flex_col()
                            .child(tab_row)
                            .child(
                                div().flex_1().min_h_0().child(match self.mode {
                                    Mode::Reachable => self
                                        .tree
                                        .clone()
                                        .cached(StyleRefinement::default().size_full())
                                        .into_any_element(),
                                    Mode::Orphaned => self
                                        .orphan_panel
                                        .clone()
                                        .cached(StyleRefinement::default().size_full())
                                        .into_any_element(),
                                    Mode::Deprecated => self
                                        .until_panel
                                        .clone()
                                        .cached(StyleRefinement::default().size_full())
                                        .into_any_element(),
                                }),
                            ),
                    )
                    .tween(sidebar_tw, move |el, w| el.w(px(w)))
            })
            .when(self.sidebar_open, |el| {
                el.child(splitter(
                    "sidebar-splitter",
                    Splitter::Sidebar,
                    th,
                    self.resizing == Some(Splitter::Sidebar),
                    cx,
                ))
            })
            .child(
                div()
                    .flex_1()
                    .h_full()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .flex_1()
                            .min_h_0()
                            .relative()
                            // Cached: sidebar/dock/tab/hover animations here
                            // are otherwise enough to re-render the whole
                            // Workspace every frame, which would force a full
                            // canvas repaint even though nothing in it
                            // changed. This reuses last frame's paint unless
                            // the canvas itself was notified or its bounds
                            // (pane width/height) actually moved.
                            .child(
                                self.canvas
                                    .clone()
                                    .cached(StyleRefinement::default().size_full()),
                            )
                            .child(controls)
                            .child(investigate_card)
                            .when_some(recent, |el, r| el.child(r))
                            .when(!self.sidebar_open && sidebar_settled, |el| {
                                el.child(
                                    div()
                                        .id("expand-sidebar")
                                        .absolute()
                                        .top(px(16.0))
                                        .left(px(16.0))
                                        .bg(th.chrome_bg)
                                        .p(px(6.0))
                                        .cursor_pointer()
                                        .hover(|el| el.bg(th.hover_bg))
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.sidebar_open = true;
                                            this.sidebar_toggled_at =
                                                Some(std::time::Instant::now());
                                            this.canvas.update(cx, |canvas, cx| {
                                                canvas.set_pane_offset(this.sidebar_width, cx)
                                            });
                                            this.save_settings(cx);
                                            cx.notify();
                                        }))
                                        .child(icon(Icon::PanelLeftOpen, px(16.0), th.text_muted)),
                                )
                            }),
                    )
                    .child(dock),
            )
            // Above both panes and the canvas, so it covers whatever the
            // reader was on rather than being wedged into one of them.
            // The overlay box belongs to the parent: a view's own root cannot
            // position itself against an ancestor, and an absolute root would
            // resolve its insets against a zero-sized wrapper.
            .when(self.palette_open, |el| {
                el.child(div().absolute().inset_0().child(self.palette.clone()))
            })
    }
}

/// `2 additions, 1 override` — the web's applied-summary wording.
fn change_label(counts: Option<(usize, usize, usize)>) -> String {
    let Some((a, c, r)) = counts else {
        return "no change".into();
    };
    let mut parts = Vec::new();
    let plural = |n: usize, w: &str| {
        if n == 1 {
            format!("{n} {w}")
        } else {
            format!("{n} {w}s")
        }
    };
    if a > 0 {
        parts.push(plural(a, "addition"));
    }
    if c > 0 {
        parts.push(plural(c, "override"));
    }
    if r > 0 {
        parts.push(plural(r, "removal"));
    }
    if parts.is_empty() {
        "no change".into()
    } else {
        parts.join(", ")
    }
}
