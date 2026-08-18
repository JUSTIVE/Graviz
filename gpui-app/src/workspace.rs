//! Two-pane workspace: tree sidebar + graph canvas, with mode tabs
//! (Reachable / Orphaned / Deprecated), view toggles, file watching, and
//! global shortcuts (⌘K search, ⌘B sidebar, ⌘D descriptions, ⌘E bundling,
//! ⌘I investigate, ⌘O open, ⌘[ back).

use crate::canvas::GraphCanvas;
use crate::config;
use crate::loader;
use crate::model::{build_model, slice_graph, Mode, Model, ModelOptions};
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
        KeyBinding::new("cmd-u", ToggleOverlayDock, None),
        KeyBinding::new("cmd-o", OpenSchema, None),
        KeyBinding::new("cmd-shift-o", OpenOverlay, None),
        KeyBinding::new("cmd-[", Back, None),
    ]);
}

pub struct Workspace {
    schema_path: PathBuf,
    overlay_path: Option<PathBuf>,
    full_graph: ParsedGraph,
    schema_name: String,
    mode: Mode,
    options: ModelOptions,
    investigate: bool,
    tree: Entity<TreePanel>,
    canvas: Entity<GraphCanvas>,
    sidebar_open: bool,
    /// Latest built model, for name→card navigation from the overlay dock.
    model: Rc<Model>,
    overlay_diff: Option<OverlayDiff>,
    dock_open: bool,
}

impl Workspace {
    pub fn new(
        loaded: loader::LoadedSchema,
        schema_path: PathBuf,
        overlay_path: Option<PathBuf>,
        cx: &mut Context<Self>,
    ) -> Self {
        let mode = Mode::Reachable;
        let settings = config::load_settings();
        let mut options = ModelOptions {
            show_descriptions: settings.show_descriptions,
            bundle_edges: settings.bundle_edges,
            ..Default::default()
        };
        let sidebar_open = settings.sidebar_open;
        // Debug presets so automated selfshots can exercise toggle states.
        if std::env::var("GOMPASS_DESC").is_ok() {
            options.show_descriptions = true;
        }
        let investigate = std::env::var("GOMPASS_INVESTIGATE").is_ok();
        let model = Rc::new(build_model(
            slice_graph(&loaded.graph, mode),
            loaded.name.clone(),
            &options,
        ));
        let tree = cx.new(|cx| TreePanel::new(model.clone(), cx));
        let canvas = cx.new(|cx| {
            let mut c = GraphCanvas::new(model.clone());
            c.set_investigate(investigate, cx);
            c
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

        // Watch the schema (and overlay) file and hot-reload on change — the
        // native replacement for the web app's File System Access "linked
        // file" flow. Paths are re-read from the entity each tick so ⌘O /
        // overlay changes are picked up.
        cx.spawn(async move |this, cx| {
            let mut last = 0u128;
            loop {
                cx.background_executor().timer(Duration::from_secs(1)).await;
                let Ok((schema, overlay)) = this.update(cx, |this: &mut Self, _| {
                    (this.schema_path.clone(), this.overlay_path.clone())
                }) else {
                    break;
                };
                let cur = loader::fingerprint(&schema, overlay.as_deref());
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
            overlay_path,
            full_graph: loaded.graph,
            schema_name: loaded.name,
            mode,
            options,
            investigate,
            tree,
            canvas,
            sidebar_open,
            model,
            overlay_diff: loaded.diff,
            dock_open: std::env::var("GOMPASS_DOCK").is_ok(),
        }
    }

    fn save_settings(&self) {
        config::save_settings(&config::Settings {
            show_descriptions: self.options.show_descriptions,
            bundle_edges: self.options.bundle_edges,
            sidebar_open: self.sidebar_open,
        });
    }

    fn rebuild(&mut self, cx: &mut Context<Self>) {
        let model = Rc::new(build_model(
            slice_graph(&self.full_graph, self.mode),
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
        match loader::load(&self.schema_path, self.overlay_path.as_deref()) {
            Ok(loaded) => {
                eprintln!("reloaded {}", self.schema_path.display());
                self.full_graph = loaded.graph;
                self.schema_name = loaded.name;
                self.overlay_diff = loaded.diff;
                self.rebuild(cx);
            }
            Err(e) => eprintln!("reload failed: {e:#}"),
        }
    }

    fn set_overlay(&mut self, path: Option<PathBuf>, cx: &mut Context<Self>) {
        self.overlay_path = path;
        if self.overlay_path.is_none() {
            self.overlay_diff = None;
        }
        self.dock_open = true;
        self.reload_from_disk(cx);
    }

    fn open_overlay(&mut self, cx: &mut Context<Self>) {
        let rx = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Choose overlay SDL".into()),
        });
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(mut paths))) = rx.await {
                if let Some(path) = paths.pop() {
                    let _ = this.update(cx, |this: &mut Self, cx| {
                        this.set_overlay(Some(path), cx);
                    });
                }
            }
        })
        .detach();
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
                        this.overlay_path = None;
                        this.reload_from_disk(cx);
                    });
                }
            }
        })
        .detach();
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
        let toolbar = div()
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
            .child(self.mode_tab(th, Mode::Reachable, cx))
            .child(self.mode_tab(th, Mode::Orphaned, cx))
            .child(self.mode_tab(th, Mode::Deprecated, cx))
            .child(div().w(px(1.0)).h_4().bg(th.card_border))
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

        let dock = self.dock_open.then(|| {
            let overlay_label: SharedString = self
                .overlay_path
                .as_ref()
                .map(|p| SharedString::from(p.display().to_string()))
                .unwrap_or_else(|| "no overlay file".into());
            let diff = self.overlay_diff.clone();
            let section = |title: &'static str,
                           color: gpui::Hsla,
                           items: Vec<String>,
                           cx: &mut Context<Self>| {
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .text_xs()
                            .text_color(color)
                            .child(SharedString::from(format!("{title} · {}", items.len()))),
                    )
                    .children(items.into_iter().take(8).enumerate().map(|(i, name)| {
                        let type_name: String =
                            name.split('.').next().unwrap_or(&name).to_string();
                        div()
                            .id((title, i))
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
                .h(px(190.0))
                .bg(th.chrome_bg)
                .border_t_1()
                .border_color(th.panel_border)
                .shadow_lg()
                .flex()
                .flex_col()
                .gap_2()
                .p_3()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(div().text_sm().text_color(th.text).child("Overlay"))
                        .child(
                            div()
                                .text_xs()
                                .font_family("Menlo")
                                .text_color(th.text_faint)
                                .whitespace_nowrap()
                                .overflow_hidden()
                                .child(overlay_label),
                        )
                        .child(div().flex_1())
                        .child(self.toggle_button(
                            th,
                            "Choose… ⌘⇧O",
                            false,
                            |this, _, cx| this.open_overlay(cx),
                            cx,
                        ))
                        .when(self.overlay_path.is_some(), |el| {
                            el.child(self.toggle_button(
                                th,
                                "Clear",
                                false,
                                |this, _, cx| this.set_overlay(None, cx),
                                cx,
                            ))
                        }),
                )
                .child(match diff {
                    Some(d) => div()
                        .flex()
                        .gap_4()
                        .min_h_0()
                        .child(section("added types", th.overlay_green, d.added_types, cx))
                        .child(section("added fields", th.overlay_green, d.added_fields, cx))
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
                        "Choose an overlay SDL to augment / override / remove types on top \
                         of the loaded schema. Lines like `- Type.field` remove members. \
                         The file is watched — edits apply live.",
                    ),
                })
        });

        let breadcrumb = {
            let names = self.canvas.read(cx).history_names();
            (!names.is_empty()).then(|| {
                let trail: Vec<String> = names.iter().map(|n| n.to_string()).collect();
                div()
                    .absolute()
                    .top_2()
                    .right_2()
                    .px_3()
                    .py_1()
                    .rounded_md()
                    .bg(th.chrome_bg)
                    .border_1()
                    .border_color(th.panel_border)
                    .shadow_md()
                    .text_xs()
                    .font_family("Menlo")
                    .text_color(th.text_muted)
                    .child(SharedString::from(format!("⌘[ ← {}", trail.join(" ‹ "))))
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
            .on_action(cx.listener(|this, _: &ToggleInvestigate, _, cx| {
                this.investigate = !this.investigate;
                let investigate = this.investigate;
                this.canvas
                    .update(cx, |canvas, cx| canvas.set_investigate(investigate, cx));
            }))
            .on_action(cx.listener(|this, _: &OpenSchema, _, cx| this.open_schema(cx)))
            .on_action(cx.listener(|this, _: &OpenOverlay, _, cx| this.open_overlay(cx)))
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
                    .relative()
                    .child(self.canvas.clone())
                    .child(toolbar)
                    .when_some(breadcrumb, |el, b| el.child(b))
                    .when_some(dock, |el, d| el.child(d)),
            )
    }
}
