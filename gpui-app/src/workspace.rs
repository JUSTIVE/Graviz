//! Two-pane workspace: tree sidebar + graph canvas, with a mode tab bar,
//! view toggles, the in-app overlay editor dock, file watching, and global
//! shortcuts (⌘K search, ⌘B sidebar, ⌘D descriptions, ⌘E bundling,
//! ⌘I investigate, ⌘P primitives, ⌘R relay, ⌘U overlay dock, ⌘O open,
//! ⌘⇧O load overlay file, ⌘[ back, ⌘↵ apply overlay).

use crate::canvas::GraphCanvas;
use crate::config;
use crate::editor::{EditorEvent, TextArea};
use crate::loader;
use crate::model::{build_model, slice_graph, Mode, Model, ModelOptions};
use crate::panels::{OrphanPanel, PanelEvent, UntilPanel};
use crate::icons::{icon, Icon};
use crate::theme::Theme;
use crate::tree::{TreeEvent, TreePanel};
use gompass_core::graph::{OverlayDiff, ParsedGraph};
use gpui::{
    actions, div, prelude::*, px, App, Context, Entity, KeyBinding, PathPromptOptions,
    SharedString, Window,
};
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

actions!(
    gompass,
    [
        FocusSearch,
        ToggleSidebar,
        ToggleDescriptions,
        ToggleBundling,
        ToggleInvestigate,
        TogglePrimitives,
        ToggleRelay,
        ToggleOverlayDock,
        OpenSchema,
        OpenOverlay,
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
        KeyBinding::new("cmd-i", ToggleInvestigate, None),
        KeyBinding::new("cmd-p", TogglePrimitives, None),
        KeyBinding::new("cmd-r", ToggleRelay, None),
        KeyBinding::new("cmd-u", ToggleOverlayDock, None),
        KeyBinding::new("cmd-o", OpenSchema, None),
        KeyBinding::new("cmd-shift-o", OpenOverlay, None),
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
        .when(vertical, |el| el.top_0().bottom_0().right(px(-2.0)).w(px(5.0)))
        .when(!vertical, |el| el.left_0().right_0().top(px(-2.0)).h(px(5.0)))
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
    orphan_panel: Entity<OrphanPanel>,
    until_panel: Entity<UntilPanel>,
    canvas: Entity<GraphCanvas>,
    overlay_editor: Entity<TextArea>,
    sidebar_open: bool,
    sidebar_width: f32,
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
}

/// Types no root operation can reach, and deprecated members — the two tab
/// badges the web shows next to Orphaned / Deprecated.
fn tab_counts(graph: &ParsedGraph) -> (usize, usize) {
    let orphans = slice_graph(graph, Mode::Orphaned, None).nodes.len();
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

impl Workspace {
    pub fn new(
        loaded: loader::LoadedSchema,
        schema_path: PathBuf,
        initial_overlay: Option<String>,
        cx: &mut Context<Self>,
    ) -> Self {
        // Debug: GOMPASS_MODE=orphaned|deprecated opens on that tab so
        // selfshots can verify each sidebar body.
        let mode = match std::env::var("GOMPASS_MODE").as_deref() {
            Ok("orphaned") => Mode::Orphaned,
            Ok("deprecated") => Mode::Deprecated,
            _ => Mode::Reachable,
        };
        let settings = config::load_settings();
        let mut options = ModelOptions {
            show_descriptions: settings.show_descriptions,
            bundle_edges: settings.bundle_edges,
            hide_primitive_fields: settings.hide_primitive_fields,
            ..Default::default()
        };
        let sidebar_open = settings.sidebar_open;
        let sidebar_width =
            settings.sidebar_width.clamp(config::SIDEBAR_MIN_W, config::SIDEBAR_MAX_W);
        let dock_height = settings.dock_height.clamp(config::DOCK_MIN_H, config::DOCK_MAX_H);
        // Debug presets so automated selfshots can exercise toggle states.
        if std::env::var("GOMPASS_DESC").is_ok() {
            options.show_descriptions = true;
        }
        let investigate = std::env::var("GOMPASS_INVESTIGATE").is_ok();
        let t_layout = std::time::Instant::now();
        let model = Rc::new(build_model(
            slice_graph(&loaded.graph, mode, None),
            loaded.name.clone(),
            &options,
        ));
        if std::env::var("GOMPASS_PERF").is_ok() {
            eprintln!(
                "perf: layout+model {}ms — {} cards, {} edges, world {:.0}×{:.0}",
                t_layout.elapsed().as_millis(),
                model.cards.len(),
                model.edges.len(),
                model.world_w,
                model.world_h
            );
        }
        let tree = cx.new(|cx| TreePanel::new(model.clone(), cx));
        // The Orphaned / Deprecated tab bodies work off the FULL graph, since
        // their whole point is what the reachable slice leaves out.
        let full_model = Rc::new(build_model(
            loaded.graph.clone(),
            loaded.name.clone(),
            &options,
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
        cx.subscribe(&tree, |this: &mut Self, _, event: &TreeEvent, cx| match event {
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
        })
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
        cx.subscribe(&overlay_editor, |this: &mut Self, _, event: &EditorEvent, cx| {
            if matches!(event, EditorEvent::Submitted) {
                this.apply_overlay(cx);
            }
        })
        .detach();

        // Watch the schema file and hot-reload on change (the overlay lives
        // in the editor, so only the schema needs watching).
        cx.spawn(async move |this, cx| {
            let mut last = 0u128;
            loop {
                cx.background_executor().timer(Duration::from_secs(1)).await;
                let Ok(schema) =
                    this.update(cx, |this: &mut Self, _| this.schema_path.clone())
                else {
                    break;
                };
                let cur = loader::fingerprint(&schema);
                if last != 0 && cur != last
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
            orphan_panel,
            until_panel,
            canvas,
            overlay_editor,
            sidebar_open,
            sidebar_width,
            resizing: None,
            model,
            overlay_text: initial_overlay,
            overlay_diff: loaded.diff,
            overlay_error: None,
            dock_open: std::env::var("GOMPASS_DOCK").is_ok(),
            history_open: false,
            dock_height,
            highlight_overlay: false,
            orphan_count: counts.0,
            deprecated_count: counts.1,
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
            self.canvas
                .update(cx, |c, cx| c.navigate_to(card, row, cx));
        }
    }

    fn save_settings(&self, cx: &gpui::App) {
        config::save_settings(&config::Settings {
            show_descriptions: self.options.show_descriptions,
            bundle_edges: self.options.bundle_edges,
            hide_primitive_fields: self.options.hide_primitive_fields,
            hide_relay: self.hide_relay,
            sidebar_open: self.sidebar_open,
            sidebar_width: self.sidebar_width,
            dock_height: self.dock_height,
            theme_mode: crate::theme::mode(cx),
        });
    }

    fn rebuild(&mut self, cx: &mut Context<Self>) {
        let model = Rc::new(build_model(
            slice_graph(&self.full_graph, self.mode, self.root_override.as_deref()),
            self.schema_name.clone(),
            &self.options,
        ));
        self.model = model.clone();
        self.tree.update(cx, |tree, cx| tree.set_model(model.clone(), cx));
        let full_model = Rc::new(build_model(
            self.full_graph.clone(),
            self.schema_name.clone(),
            &self.options,
        ));
        self.orphan_panel
            .update(cx, |p, cx| p.set_model(full_model.clone(), cx));
        self.until_panel.update(cx, |p, cx| p.set_model(full_model, cx));
        self.canvas.update(cx, |canvas, cx| {
            canvas.set_model(model, cx);
            canvas.set_investigate(self.investigate, cx);
        });
        cx.notify();
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
                            this.overlay_editor
                                .update(cx, |e, cx| e.set_text(text, cx));
                            this.apply_overlay(cx);
                        });
                    }
                }
            }
        })
        .detach();
    }

    fn clear_overlay(&mut self, cx: &mut Context<Self>) {
        self.overlay_editor.update(cx, |e, cx| e.set_text(String::new(), cx));
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


    /// One mode tab, styled like the web's `ModeTab`: icon always keeps its
    /// tone, the label collapses away when inactive, active tab grows.
    fn mode_tab(
        &self,
        th: Theme,
        mode: Mode,
        icon_name: Icon,
        tone: gpui::Hsla,
        count: Option<usize>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let active = self.mode == mode;
        let disabled = matches!(count, Some(0));
        div()
            .id(mode.label())
            .flex()
            .items_center()
            .justify_center()
            .px_3()
            .py_2()
            .border_b_2()
            .when(active, |el| el.border_color(tone))
            .when(!active, |el| el.border_color(gpui::transparent_black()))
            .when(active, |el| el.flex_grow(1.0))
            .text_size(px(12.0))
            .when(disabled, |el| el.opacity(0.4))
            .when(!disabled, |el| {
                el.cursor_pointer().hover(|el| el.text_color(th.text))
            })
            .when(!active, |el| el.text_color(th.text_muted))
            .when(active, |el| el.text_color(tone))
            .child(icon(icon_name, px(16.0), tone.opacity(if active { 1.0 } else { 0.7 })))
            .when(active, |el| {
                el.child(
                    div()
                        .ml(px(6.0))
                        .flex()
                        .items_center()
                        .whitespace_nowrap()
                        .child(SharedString::from(mode.label()))
                        .when_some(count.filter(|c| *c > 0), |el, c| {
                            el.child(
                                div()
                                    .ml(px(6.0))
                                    .rounded_full()
                                    .px(px(6.0))
                                    .py(px(1.0))
                                    .text_size(px(10.0))
                                    .bg(tone.opacity(0.15))
                                    .text_color(tone)
                                    .child(SharedString::from(c.to_string())),
                            )
                        }),
                )
            })
            .when(!disabled, |el| {
                el.on_click(cx.listener(move |this, _, _, cx| this.set_mode(mode, cx)))
            })
    }

    /// A pill filter chip from the canvas's floating "View controls" card.
    fn chip<F>(
        &self,
        th: Theme,
        id: &'static str,
        label: &'static str,
        active: bool,
        on_click: F,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<F>
    where
        F: Fn(&mut Self, &mut Window, &mut Context<Self>) + 'static,
    {
        let fg = if active { th.primary } else { th.text_muted };
        div()
            .id(id)
            .flex()
            .items_center()
            .gap_1()
            .rounded_full()
            .border_1()
            .px_2()
            .py(px(2.0))
            .text_size(px(10.0))
            .cursor_pointer()
            .when(active, |el| el.border_color(th.primary).bg(th.primary.opacity(0.1)))
            .when(!active, |el| {
                el.border_color(th.card_border).hover(|el| el.text_color(th.text))
            })
            .text_color(fg)
            .child(icon(Icon::Filter, px(10.0), fg))
            .child(SharedString::from(label))
            .on_click(cx.listener(move |this, _, window, cx| on_click(this, window, cx)))
    }
}

/// Solid kind badge used in list rows (web `KIND_STYLES[kind].badge`).
pub fn kind_badge(th: Theme, kind: gompass_core::graph::NodeKind, label: &'static str) -> gpui::Div {
    div()
        .flex_none()
        .rounded_md()
        .px(px(6.0))
        .text_size(px(9.0))
        .bg(th.kind_color(kind))
        .text_color(gpui::white())
        .child(SharedString::from(label))
}


impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let th = crate::theme::current(cx, window.appearance());
        let vh = f32::from(window.viewport_size().height);

        // ---- sidebar tab strip (web: it lives INSIDE the sidebar) ----
        let deprecated_tone = th.red;
        let tab_row = div()
            .flex_none()
            .flex()
            .items_stretch()
            .border_b_1()
            .border_color(th.panel_border)
            .child(self.mode_tab(
                th,
                Mode::Reachable,
                Icon::Waypoints,
                th.kind_color(gompass_core::graph::NodeKind::Object),
                None,
                cx,
            ))
            .child(self.mode_tab(
                th,
                Mode::Orphaned,
                Icon::Unlink,
                th.type_amber,
                Some(self.orphan_count),
                cx,
            ))
            .when(self.deprecated_count > 0, |el| {
                el.child(self.mode_tab(
                    th,
                    Mode::Deprecated,
                    Icon::Clock,
                    deprecated_tone,
                    Some(self.deprecated_count),
                    cx,
                ))
            })
            .child(
                div()
                    .id("collapse-sidebar")
                    .flex()
                    .items_center()
                    .justify_center()
                    .flex_none()
                    .border_l_1()
                    .border_color(th.panel_border)
                    .px(px(10.0))
                    .text_color(th.text_muted)
                    .cursor_pointer()
                    .hover(|el| el.bg(th.hover_bg).text_color(th.text))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.sidebar_open = false;
                        this.canvas
                            .update(cx, |canvas, cx| canvas.set_pane_offset(0.0, cx));
                        this.save_settings(cx);
                        cx.notify();
                    }))
                    .child(icon(Icon::PanelLeftClose, px(16.0), th.text_muted)),
            );

        // ---- canvas overlay: floating "View controls" card (left 16 / top 16)
        let inset = if self.sidebar_open { 0.0 } else { 44.0 };
        let controls = div()
            .absolute()
            .top(px(16.0 + inset))
            .left(px(16.0))
            .flex()
            .items_center()
            .gap(px(6.0))
            .rounded_lg()
            .border_1()
            .border_color(th.panel_border)
            .bg(th.chrome_bg)
            .px_2()
            .py(px(6.0))
            .shadow_lg()
            .opacity(0.4)
            .hover(|el| el.opacity(1.0))
            .child(self.chip(
                th,
                "hide-primitives",
                "Hide primitives",
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
                "Hide Relay",
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
                "Show descriptions",
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
                "Bundle edges",
                self.options.bundle_edges,
                |this, _, cx| {
                    this.options.bundle_edges = !this.options.bundle_edges;
                    this.rebuild(cx);
                    this.save_settings(cx);
                },
                cx,
            ));

        // ---- canvas overlay: Investigate card (left 16 / top 56) ----
        let (documented, total) = self.model.desc_coverage;
        let coverage = if total > 0 { documented as f32 / total as f32 } else { 1.0 };
        let cov_color = if coverage >= 0.9 {
            th.kind_color(gompass_core::graph::NodeKind::Enum)
        } else if coverage >= 0.5 {
            th.type_amber
        } else {
            th.kind_color(gompass_core::graph::NodeKind::Scalar)
        };
        let investigate_card = div()
            .absolute()
            .top(px(56.0 + inset))
            .left(px(16.0))
            .flex()
            .flex_col()
            .gap(px(6.0))
            .rounded_lg()
            .border_1()
            .border_color(th.panel_border)
            .bg(th.chrome_bg)
            .px_2()
            .py(px(6.0))
            .shadow_lg()
            .opacity(0.4)
            .hover(|el| el.opacity(1.0))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .child(icon(Icon::Microscope, px(12.0), th.text_muted))
                    .child(
                        div()
                            .text_size(px(10.0))
                            .text_color(th.text_muted)
                            .child("INVESTIGATE"),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .child(
                        div()
                            .id("investigate-mode")
                            .rounded_full()
                            .border_1()
                            .px_2()
                            .py(px(2.0))
                            .text_size(px(10.0))
                            .cursor_pointer()
                            .when(self.investigate, |el| {
                                el.border_color(th.investigate)
                                    .bg(th.investigate.opacity(0.1))
                                    .text_color(th.investigate)
                            })
                            .when(!self.investigate, |el| {
                                el.border_color(th.card_border).text_color(th.text_muted)
                            })
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.investigate = !this.investigate;
                                let on = this.investigate;
                                this.canvas
                                    .update(cx, |canvas, cx| canvas.set_investigate(on, cx));
                            }))
                            .child("Missing descriptions"),
                    )
                    .child(
                        div()
                            .rounded_full()
                            .px(px(6.0))
                            .py(px(2.0))
                            .text_size(px(10.0))
                            .bg(cov_color.opacity(0.1))
                            .text_color(cov_color)
                            .child(SharedString::from(format!(
                                "{}%",
                                (coverage * 100.0).round() as i32
                            ))),
                    ),
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

        let pill = |label: String, color: gpui::Hsla| {
            div()
                .rounded_full()
                .px(px(6.0))
                .py(px(1.0))
                .text_size(px(10.0))
                .bg(color.opacity(0.15))
                .text_color(color)
                .child(SharedString::from(label))
        };
        let count_pills = move |th: Theme| {
            let mut row = div().flex().items_center().gap_1();
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
                            .text_size(px(10.0))
                            .text_color(th.text_muted)
                            .child("no change"),
                    );
                }
                None => {}
            }
            row
        };

        let dock: gpui::AnyElement = if !self.dock_open {
            // Collapsed: a single one-line strip.
            let status: gpui::AnyElement = match (&self.overlay_error, applied) {
                (Some(e), _) => div()
                    .flex_1()
                    .min_w_0()
                    .text_size(px(10.0))
                    .text_color(th.red)
                    .whitespace_nowrap()
                    .overflow_hidden()
                    .child(SharedString::from(format!("not applied — {e}")))
                    .into_any_element(),
                (None, true) => count_pills(th).flex_1().into_any_element(),
                (None, false) => div()
                    .flex_1()
                    .min_w_0()
                    .text_size(px(10.0))
                    .text_color(th.text_muted)
                    .child("Sketch SDL on top of this schema")
                    .into_any_element(),
            };
            div()
                .id("dock-strip")
                .flex_none()
                .flex()
                .items_center()
                .gap_2()
                .border_t_1()
                .border_color(th.panel_border)
                .bg(th.card_bg.opacity(0.4))
                .px_3()
                .py(px(6.0))
                .cursor_pointer()
                .hover(|el| el.bg(th.hover_bg))
                .on_click(cx.listener(|this, _, _, cx| {
                    this.dock_open = true;
                    cx.notify();
                }))
                .child(icon(Icon::Layers, px(14.0), th.overlay_green))
                .child(
                    div()
                        .flex_none()
                        .text_xs()
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .text_color(th.text)
                        .child("Overlay"),
                )
                .child(status)
                .child(icon(Icon::ChevronUp, px(14.0), th.text_muted))
                .into_any_element()
        } else {
            let apply_enabled = dirty || !applied;
            div()
                .flex_none()
                .h(px(self.dock_height))
                .relative()
                .flex()
                .flex_col()
                .border_t_1()
                .border_color(th.panel_border)
                .bg(th.card_bg.opacity(0.4))
                .child(splitter(
                    "dock-splitter",
                    Splitter::Dock,
                    th,
                    self.resizing == Some(Splitter::Dock),
                    cx,
                ))
                // header row
                .child(
                    div()
                        .flex_none()
                        .flex()
                        .items_center()
                        .gap_2()
                        .border_b_1()
                        .border_color(th.panel_border)
                        .px_3()
                        .py(px(6.0))
                        .child(icon(Icon::Layers, px(14.0), th.overlay_green))
                        .child(
                            div()
                                .flex_none()
                                .text_xs()
                                .font_weight(gpui::FontWeight::MEDIUM)
                                .text_color(th.text)
                                .child("Overlay"),
                        )
                        .child(count_pills(th))
                        .child(div().flex_1())
                        .child(
                            div()
                                .id("overlay-highlight")
                                .flex()
                                .items_center()
                                .gap_1()
                                .rounded_md()
                                .border_1()
                                .px_2()
                                .py_1()
                                .text_xs()
                                .when(!can_highlight, |el| {
                                    el.opacity(0.4).border_color(th.card_border)
                                })
                                .when(can_highlight && self.highlight_overlay, |el| {
                                    el.border_color(th.overlay_green)
                                        .bg(th.overlay_green.opacity(0.15))
                                        .text_color(th.overlay_green)
                                })
                                .when(can_highlight && !self.highlight_overlay, |el| {
                                    el.border_color(th.card_border)
                                        .text_color(th.text_muted)
                                        .cursor_pointer()
                                        .hover(|el| el.bg(th.hover_bg))
                                })
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.highlight_overlay = !this.highlight_overlay;
                                    let on = this.highlight_overlay;
                                    this.canvas
                                        .update(cx, |c, cx| c.set_highlight_overlay(on, cx));
                                    cx.notify();
                                }))
                                .child(icon(Icon::Highlighter, px(14.0), th.text_muted))
                                .child("Highlight"),
                        )
                        .child(
                            div()
                                .text_size(px(10.0))
                                .text_color(th.text_muted)
                                .child(SharedString::from(if dirty && applied {
                                    "Unapplied edits · ⌘↵"
                                } else {
                                    "⌘↵"
                                })),
                        )
                        .child(
                            div()
                                .id("overlay-apply")
                                .rounded_md()
                                .px(px(10.0))
                                .py_1()
                                .text_xs()
                                .font_weight(gpui::FontWeight::MEDIUM)
                                .when(apply_enabled, |el| {
                                    el.bg(th.overlay_green)
                                        .text_color(gpui::white())
                                        .cursor_pointer()
                                })
                                .when(!apply_enabled, |el| {
                                    el.bg(th.active_bg).text_color(th.text_muted)
                                })
                                .on_click(cx.listener(|this, _, _, cx| this.apply_overlay(cx)))
                                .child(SharedString::from(if applied && dirty {
                                    "Re-apply"
                                } else {
                                    "Apply"
                                })),
                        )
                        .child(
                            div()
                                .id("overlay-clear")
                                .rounded_md()
                                .border_1()
                                .border_color(th.card_border)
                                .px(px(10.0))
                                .py_1()
                                .text_xs()
                                .when(applied, |el| {
                                    el.text_color(th.text_muted)
                                        .cursor_pointer()
                                        .hover(|el| el.bg(th.hover_bg))
                                })
                                .when(!applied, |el| el.opacity(0.4).text_color(th.text_muted))
                                .on_click(cx.listener(|this, _, _, cx| this.clear_overlay(cx)))
                                .child("Clear"),
                        )
                        .child(
                            div()
                                .id("overlay-collapse")
                                .rounded_md()
                                .p_1()
                                .cursor_pointer()
                                .hover(|el| el.bg(th.hover_bg))
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.dock_open = false;
                                    cx.notify();
                                }))
                                .child(icon(Icon::ChevronDown, px(16.0), th.text_muted)),
                        ),
                )
                // editor
                .child(div().flex_1().min_h_0().p_2().child(self.overlay_editor.clone()))
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
                            .child(
                                div()
                                    .text_size(px(10.0))
                                    .text_color(th.text_muted)
                                    .child(
                                        "The graph is still showing the schema without the overlay.",
                                    ),
                            ),
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
                                el.child(
                                    div()
                                        .flex()
                                        .flex_wrap()
                                        .gap_1()
                                        .children(d.added_types.into_iter().take(12).enumerate().map(
                                            |(i, name)| {
                                                let t = name.clone();
                                                div()
                                                    .id(("added-type", i))
                                                    .rounded_md()
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
                                        )),
                                )
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
                                                .map(|f| SharedString::from(format!("\u{2212}{f}"))),
                                        ),
                                )
                            }),
                    )
                })
                .into_any_element()
        };

        // ---- canvas overlay: click-history "Recent" panel (right 16 / top 16)
        let recent = {
            let entries = self.canvas.read(cx).history_entries();
            let open = self.history_open;
            (!entries.is_empty()).then(|| {
                let count = entries.len();
                div()
                    .absolute()
                    .top(px(16.0))
                    .right(px(16.0))
                    .w(px(256.0))
                    .rounded_lg()
                    .border_1()
                    .border_color(th.panel_border)
                    .bg(th.chrome_bg)
                    .shadow_lg()
                    .flex()
                    .flex_col()
                    .opacity(0.4)
                    .hover(|el| el.opacity(1.0))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .border_b_1()
                            .border_color(th.panel_border)
                            .px_3()
                            .py_2()
                            .child(icon(Icon::History, px(12.0), th.text_muted))
                            .child(
                                div()
                                    .flex_1()
                                    .text_size(px(10.0))
                                    .text_color(th.text_muted)
                                    .child(SharedString::from(format!("RECENT ({count})"))),
                            )
                            .child(
                                div()
                                    .id("clear-history")
                                    .rounded_md()
                                    .p_1()
                                    .cursor_pointer()
                                    .hover(|el| el.bg(th.hover_bg))
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.canvas
                                            .update(cx, |canvas, cx| canvas.clear_history(cx));
                                        cx.notify();
                                    }))
                                    .child(icon(Icon::Trash2, px(12.0), th.text_muted)),
                            )
                            .child(
                                div()
                                    .id("toggle-history")
                                    .rounded_md()
                                    .p_1()
                                    .cursor_pointer()
                                    .hover(|el| el.bg(th.hover_bg))
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.history_open = !this.history_open;
                                        cx.notify();
                                    }))
                                    .child(icon(
                                        if open { Icon::ChevronUp } else { Icon::ChevronDown },
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
                                .children(entries.into_iter().enumerate().map(
                                    |(i, entry)| {
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
                                                    .rounded_md()
                                                    .px_2()
                                                    .py_1()
                                                    .cursor_pointer()
                                                    .bg(th.kind_color(entry.kind).opacity(0.1))
                                                    .hover(|el| el.bg(th.hover_bg))
                                                    .on_click(cx.listener(
                                                        move |this, _, _, cx| {
                                                            this.canvas.update(cx, |canvas, cx| {
                                                                canvas.revisit(item, cx)
                                                            });
                                                        },
                                                    ))
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
                                                    .rounded_md()
                                                    .p_1()
                                                    .cursor_pointer()
                                                    .hover(|el| el.bg(th.hover_bg))
                                                    .on_click(cx.listener(
                                                        move |this, _, _, cx| {
                                                            this.canvas.update(cx, |canvas, cx| {
                                                                canvas.remove_history(item, cx)
                                                            });
                                                            cx.notify();
                                                        },
                                                    ))
                                                    .child(icon(Icon::X, px(12.0), th.text_muted)),
                                            )
                                    },
                                )),
                        )
                    })
            })
        };

        div()
            .flex()
            .size_full()
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
            .on_action(cx.listener(|this, _: &FocusSearch, window, cx| {
                this.sidebar_open = true;
                let handle = this.tree.read(cx).focus_handle();
                window.focus(&handle, cx);
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &ToggleSidebar, _, cx| {
                this.sidebar_open = !this.sidebar_open;
                let offset = if this.sidebar_open { this.sidebar_width } else { 0.0 };
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
                this.canvas.update(cx, |canvas, cx| canvas.clear_focused_edge(cx));
            }))
            .when(self.sidebar_open, |el| {
                el.child(
                    div()
                        .w(px(self.sidebar_width))
                        .h_full()
                        .flex_none()
                        .flex()
                        .flex_col()
                        .bg(th.panel)
                        .border_r_1()
                        .border_color(th.panel_border)
                        .child(tab_row)
                        .child(div().flex_1().min_h_0().child(match self.mode {
                            Mode::Reachable => self.tree.clone().into_any_element(),
                            Mode::Orphaned => self.orphan_panel.clone().into_any_element(),
                            Mode::Deprecated => self.until_panel.clone().into_any_element(),
                        })),
                )
                .child(splitter(
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
                            .child(self.canvas.clone())
                            .child(controls)
                            .child(investigate_card)
                            .when_some(recent, |el, r| el.child(r))
                            .when(!self.sidebar_open, |el| {
                                el.child(
                                    div()
                                        .id("expand-sidebar")
                                        .absolute()
                                        .top(px(16.0))
                                        .left(px(16.0))
                                        .rounded_lg()
                                        .border_1()
                                        .border_color(th.panel_border)
                                        .bg(th.chrome_bg)
                                        .p(px(6.0))
                                        .shadow_lg()
                                        .cursor_pointer()
                                        .hover(|el| el.bg(th.hover_bg))
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.sidebar_open = true;
                                            this.canvas.update(cx, |canvas, cx| {
                                                canvas.set_pane_offset(this.sidebar_width, cx)
                                            });
                                            this.save_settings(cx);
                                            cx.notify();
                                        }))
                                        .child(icon(
                                            Icon::PanelLeftOpen,
                                            px(16.0),
                                            th.text_muted,
                                        )),
                                )
                            }),
                    )
                    .child(dock),
            )
    }
}

/// `2 additions, 1 override` — the web's applied-summary wording.
fn change_label(counts: Option<(usize, usize, usize)>) -> String {
    let Some((a, c, r)) = counts else { return "no change".into() };
    let mut parts = Vec::new();
    let plural = |n: usize, w: &str| {
        if n == 1 { format!("{n} {w}") } else { format!("{n} {w}s") }
    };
    if a > 0 { parts.push(plural(a, "addition")); }
    if c > 0 { parts.push(plural(c, "override")); }
    if r > 0 { parts.push(plural(r, "removal")); }
    if parts.is_empty() { "no change".into() } else { parts.join(", ") }
}
