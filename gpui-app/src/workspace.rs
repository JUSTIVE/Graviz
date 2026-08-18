//! Two-pane workspace: tree sidebar + graph canvas, with a mode tab bar,
//! view toggles, the in-app overlay editor dock, file watching, and global
//! shortcuts (⌘K search, ⌘B sidebar, ⌘D descriptions, ⌘E bundling,
//! ⌘I investigate, ⌘P primitives, ⌘R relay, ⌘U overlay dock, ⌘O open,
//! ⌘⇧O load overlay file, ⌘[ back, ⌘↵ apply overlay).

use crate::canvas::GraphCanvas;
use crate::config;
use crate::editor::{EditorEvent, TextArea};
use crate::loader;
use crate::model::{build_model, root_candidates, slice_graph, Mode, Model, ModelOptions};
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
        Back
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
    ]);
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
    canvas: Entity<GraphCanvas>,
    overlay_editor: Entity<TextArea>,
    sidebar_open: bool,
    /// Latest built model, for name→card navigation from the overlay dock.
    model: Rc<Model>,
    /// The last APPLIED overlay text (reloads re-apply it).
    overlay_text: Option<String>,
    overlay_diff: Option<OverlayDiff>,
    overlay_error: Option<String>,
    dock_open: bool,
}

impl Workspace {
    pub fn new(
        loaded: loader::LoadedSchema,
        schema_path: PathBuf,
        initial_overlay: Option<String>,
        cx: &mut Context<Self>,
    ) -> Self {
        let mode = Mode::Reachable;
        let settings = config::load_settings();
        let mut options = ModelOptions {
            show_descriptions: settings.show_descriptions,
            bundle_edges: settings.bundle_edges,
            hide_primitive_fields: settings.hide_primitive_fields,
            ..Default::default()
        };
        let sidebar_open = settings.sidebar_open;
        // Debug presets so automated selfshots can exercise toggle states.
        if std::env::var("GOMPASS_DESC").is_ok() {
            options.show_descriptions = true;
        }
        let investigate = std::env::var("GOMPASS_INVESTIGATE").is_ok();
        let model = Rc::new(build_model(
            slice_graph(&loaded.graph, mode, None),
            loaded.name.clone(),
            &options,
        ));
        let tree = cx.new(|cx| TreePanel::new(model.clone(), cx));
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
            TreeEvent::Select { node_index, row } => {
                let (node_index, row) = (*node_index, *row);
                this.canvas.update(cx, |canvas, cx| {
                    canvas.navigate_to(node_index as u32, row, cx);
                });
            }
        })
        .detach();
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
                if last != 0 && cur != last {
                    if this
                        .update(cx, |this: &mut Self, cx| this.reload_from_disk(cx))
                        .is_err()
                    {
                        break;
                    }
                }
                last = cur;
            }
        })
        .detach();

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
            canvas,
            overlay_editor,
            sidebar_open,
            model,
            overlay_text: initial_overlay,
            overlay_diff: loaded.diff,
            overlay_error: None,
            dock_open: std::env::var("GOMPASS_DOCK").is_ok(),
        }
    }

    fn save_settings(&self) {
        config::save_settings(&config::Settings {
            show_descriptions: self.options.show_descriptions,
            bundle_edges: self.options.bundle_edges,
            hide_primitive_fields: self.options.hide_primitive_fields,
            hide_relay: self.hide_relay,
            sidebar_open: self.sidebar_open,
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

    fn cycle_root(&mut self, cx: &mut Context<Self>) {
        let candidates = root_candidates(&self.full_graph);
        if candidates.len() < 2 {
            return;
        }
        let current = self
            .root_override
            .clone()
            .or_else(|| candidates.first().cloned());
        let idx = candidates
            .iter()
            .position(|c| Some(c) == current.as_ref())
            .unwrap_or(0);
        self.root_override = Some(candidates[(idx + 1) % candidates.len()].clone());
        self.rebuild(cx);
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

    fn mode_tab(&self, th: Theme, mode: Mode, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let active = self.mode == mode;
        toolbar_button(th, mode.label(), active)
            .on_click(cx.listener(move |this, _, _, cx| this.set_mode(mode, cx)))
    }

    fn toggle_button<F>(
        &self,
        th: Theme,
        label: &'static str,
        active: bool,
        on_click: F,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<F>
    where
        F: Fn(&mut Self, &mut Window, &mut Context<Self>) + 'static,
    {
        toolbar_button(th, label, active)
            .on_click(cx.listener(move |this, _, window, cx| on_click(this, window, cx)))
    }
}

fn toolbar_button(th: Theme, label: &'static str, active: bool) -> gpui::Stateful<gpui::Div> {
    div()
        .id(label)
        .px_3()
        .py_1()
        .rounded_md()
        .cursor_pointer()
        .text_xs()
        .when(active, |el| el.bg(th.active_bg).text_color(th.text))
        .when(!active, |el| el.text_color(th.text_muted))
        .hover(|el| el.bg(th.hover_bg))
        .child(SharedString::from(label))
}

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let th = crate::theme::theme(window.appearance());

        // Web-app placement: a real tab bar above the canvas for the modes,
        // with the root selector on its right edge.
        let root_label: SharedString = {
            let candidates = root_candidates(&self.full_graph);
            let current = self
                .root_override
                .clone()
                .or_else(|| candidates.first().cloned())
                .unwrap_or_else(|| "—".into());
            format!("Root: {current}").into()
        };
        let tab_bar = div()
            .flex_none()
            .h(px(40.0))
            .flex()
            .items_center()
            .gap_1()
            .px_2()
            .bg(th.panel)
            .border_b_1()
            .border_color(th.panel_border)
            .child(self.mode_tab(th, Mode::Reachable, cx))
            .child(self.mode_tab(th, Mode::Orphaned, cx))
            .child(self.mode_tab(th, Mode::Deprecated, cx))
            .child(div().flex_1())
            .child(
                div()
                    .id("root-cycle")
                    .px_3()
                    .py_1()
                    .rounded_md()
                    .cursor_pointer()
                    .text_xs()
                    .text_color(th.text_muted)
                    .hover(|el| el.bg(th.hover_bg))
                    .on_click(cx.listener(|this, _, _, cx| this.cycle_root(cx)))
                    .child(root_label),
            );

        // Floating view-controls cluster at the canvas's top-left.
        let controls = div()
            .absolute()
            .top_2()
            .left_2()
            .flex()
            .items_center()
            .gap_1()
            .p_1()
            .rounded_lg()
            .bg(th.chrome_bg)
            .border_1()
            .border_color(th.panel_border)
            .shadow_md()
            .child(self.toggle_button(
                th,
                "Desc",
                self.options.show_descriptions,
                |this, _, cx| {
                    this.options.show_descriptions = !this.options.show_descriptions;
                    this.rebuild(cx);
                    this.save_settings();
                },
                cx,
            ))
            .child(self.toggle_button(
                th,
                "Bundle",
                self.options.bundle_edges,
                |this, _, cx| {
                    this.options.bundle_edges = !this.options.bundle_edges;
                    this.rebuild(cx);
                    this.save_settings();
                },
                cx,
            ))
            .child(self.toggle_button(
                th,
                "Prims",
                self.options.hide_primitive_fields,
                |this, _, cx| {
                    this.options.hide_primitive_fields = !this.options.hide_primitive_fields;
                    this.rebuild(cx);
                    this.save_settings();
                },
                cx,
            ))
            .child(self.toggle_button(
                th,
                "Relay",
                self.hide_relay,
                |this, _, cx| {
                    this.hide_relay = !this.hide_relay;
                    this.reload_from_disk(cx);
                    this.save_settings();
                },
                cx,
            ))
            .child(self.toggle_button(
                th,
                "Investigate",
                self.investigate,
                |this, _, cx| {
                    this.investigate = !this.investigate;
                    let investigate = this.investigate;
                    this.canvas
                        .update(cx, |canvas, cx| canvas.set_investigate(investigate, cx));
                },
                cx,
            ))
            .child(self.toggle_button(
                th,
                "Overlay",
                self.dock_open,
                |this, _, cx| {
                    this.dock_open = !this.dock_open;
                    cx.notify();
                },
                cx,
            ));

        // Overlay dock: in-app SDL editor + live diff panel.
        let dock = self.dock_open.then(|| {
            let diff = self.overlay_diff.clone();
            let error = self.overlay_error.clone();
            let section = |title: &'static str,
                           color: gpui::Hsla,
                           items: Vec<String>,
                           cx: &mut Context<Self>| {
                div()
                    .flex()
                    .flex_col()
                    .gap_0p5()
                    .child(
                        div()
                            .text_xs()
                            .text_color(color)
                            .child(SharedString::from(format!("{title} · {}", items.len()))),
                    )
                    .children(items.into_iter().take(5).enumerate().map(|(i, name)| {
                        let type_name: String =
                            name.split('.').next().unwrap_or(&name).to_string();
                        div()
                            .id((title, i))
                            .w_full()
                            .text_xs()
                            .font_family("Menlo")
                            .text_color(th.text_muted)
                            .whitespace_nowrap()
                            .overflow_hidden()
                            .cursor_pointer()
                            .hover(|el| el.bg(th.hover_bg))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.navigate_to_type(&type_name, cx)
                            }))
                            .child(SharedString::from(name))
                    }))
            };
            div()
                .absolute()
                .bottom_0()
                .left_0()
                .right_0()
                .h(px(240.0))
                .bg(th.chrome_bg)
                .border_t_1()
                .border_color(th.panel_border)
                .shadow_lg()
                .flex()
                .gap_3()
                .p_3()
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_2()
                                .child(div().text_sm().text_color(th.text).child("Overlay SDL"))
                                .child(div().flex_1())
                                .child(self.toggle_button(
                                    th,
                                    "Apply ⌘↵",
                                    self.overlay_text.is_some(),
                                    |this, _, cx| this.apply_overlay(cx),
                                    cx,
                                ))
                                .child(self.toggle_button(
                                    th,
                                    "Load file…",
                                    false,
                                    |this, _, cx| this.open_overlay_file(cx),
                                    cx,
                                ))
                                .child(self.toggle_button(
                                    th,
                                    "Clear",
                                    false,
                                    |this, _, cx| this.clear_overlay(cx),
                                    cx,
                                )),
                        )
                        .child(div().flex_1().min_h_0().child(self.overlay_editor.clone())),
                )
                .child(
                    div()
                        .w(px(300.0))
                        .flex_none()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .overflow_hidden()
                        .when_some(error, |el, e| {
                            el.child(
                                div()
                                    .text_xs()
                                    .font_family("Menlo")
                                    .text_color(th.red)
                                    .child(SharedString::from(e)),
                            )
                        })
                        .child(match diff {
                            Some(d) => div()
                                .flex()
                                .flex_col()
                                .gap_2()
                                .child(section(
                                    "added types",
                                    th.overlay_green,
                                    d.added_types,
                                    cx,
                                ))
                                .child(section(
                                    "added fields",
                                    th.overlay_green,
                                    d.added_fields,
                                    cx,
                                ))
                                .child(section("changed", th.accent, d.changed_fields, cx))
                                .child(section(
                                    "removed",
                                    th.red,
                                    d.removed_types
                                        .into_iter()
                                        .chain(d.removed_fields)
                                        .collect(),
                                    cx,
                                )),
                            None => div().text_xs().text_color(th.text_faint).child(
                                "Sketch SDL here to augment / override / remove types on top \
                                 of the loaded schema. `- Type.field` removes a member. \
                                 ⌘↵ applies.",
                            ),
                        }),
                )
        });

        // Web-app placement: the click-history "Recent" panel at top-right.
        let recent = {
            let entries = self.canvas.read(cx).history_entries();
            (!entries.is_empty()).then(|| {
                div()
                    .absolute()
                    .top_2()
                    .right_2()
                    .w(px(200.0))
                    .p_2()
                    .rounded_lg()
                    .bg(th.chrome_bg)
                    .border_1()
                    .border_color(th.panel_border)
                    .shadow_md()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .text_xs()
                            .text_color(th.text_faint)
                            .child("Recent  ·  ⌘[ back"),
                    )
                    .children(entries.into_iter().enumerate().map(|(i, (card, name))| {
                        div()
                            .id(("recent", i))
                            .w_full()
                            .px_2()
                            .py(px(2.0))
                            .rounded_md()
                            .cursor_pointer()
                            .hover(|el| el.bg(th.hover_bg))
                            .text_xs()
                            .font_family("Menlo")
                            .text_color(th.text_muted)
                            .whitespace_nowrap()
                            .overflow_hidden()
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.canvas.update(cx, |canvas, cx| {
                                    canvas.navigate_to(card, None, cx)
                                });
                            }))
                            .child(name)
                    }))
            })
        };

        div()
            .flex()
            .size_full()
            .key_context("Workspace")
            .on_action(cx.listener(|this, _: &FocusSearch, window, cx| {
                this.sidebar_open = true;
                let handle = this.tree.read(cx).focus_handle();
                window.focus(&handle, cx);
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &ToggleSidebar, _, cx| {
                this.sidebar_open = !this.sidebar_open;
                let offset = if this.sidebar_open { 300.0 } else { 0.0 };
                this.canvas
                    .update(cx, |canvas, cx| canvas.set_pane_offset(offset, cx));
                this.save_settings();
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &ToggleDescriptions, _, cx| {
                this.options.show_descriptions = !this.options.show_descriptions;
                this.rebuild(cx);
                this.save_settings();
            }))
            .on_action(cx.listener(|this, _: &ToggleBundling, _, cx| {
                this.options.bundle_edges = !this.options.bundle_edges;
                this.rebuild(cx);
                this.save_settings();
            }))
            .on_action(cx.listener(|this, _: &TogglePrimitives, _, cx| {
                this.options.hide_primitive_fields = !this.options.hide_primitive_fields;
                this.rebuild(cx);
                this.save_settings();
            }))
            .on_action(cx.listener(|this, _: &ToggleRelay, _, cx| {
                this.hide_relay = !this.hide_relay;
                this.reload_from_disk(cx);
                this.save_settings();
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
            .when(self.sidebar_open, |el| {
                el.child(div().w(px(300.0)).h_full().flex_none().child(self.tree.clone()))
            })
            .child(
                div()
                    .flex_1()
                    .h_full()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(tab_bar)
                    .child(
                        div()
                            .flex_1()
                            .min_h_0()
                            .relative()
                            .child(self.canvas.clone())
                            .child(controls)
                            .when_some(recent, |el, r| el.child(r))
                            .when_some(dock, |el, d| el.child(d)),
                    ),
            )
    }
}
