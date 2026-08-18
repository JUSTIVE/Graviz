//! Two-pane workspace: tree sidebar + graph canvas, with mode tabs
//! (Reachable / Orphaned / Deprecated), view toggles, file watching, and
//! global shortcuts (⌘K search, ⌘B sidebar, ⌘D descriptions, ⌘E bundling,
//! ⌘I investigate, ⌘O open, ⌘[ back).

use crate::canvas::GraphCanvas;
use crate::loader;
use crate::model::{build_model, slice_graph, Mode, ModelOptions};
use crate::tree::{TreeEvent, TreePanel};
use gompass_core::graph::ParsedGraph;
use gpui::{
    actions, div, prelude::*, px, rgb, rgba, App, Context, Entity, KeyBinding, PathPromptOptions,
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
        OpenSchema,
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
        KeyBinding::new("cmd-o", OpenSchema, None),
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
}

impl Workspace {
    pub fn new(
        loaded: loader::LoadedSchema,
        schema_path: PathBuf,
        overlay_path: Option<PathBuf>,
        cx: &mut Context<Self>,
    ) -> Self {
        let mode = Mode::Reachable;
        let options = ModelOptions::default();
        let model = Rc::new(build_model(
            slice_graph(&loaded.graph, mode),
            loaded.name.clone(),
            &options,
        ));
        let tree = cx.new(|cx| TreePanel::new(model.clone(), cx));
        let canvas = cx.new(|_| GraphCanvas::new(model));
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
        // file" flow.
        {
            let schema_path = schema_path.clone();
            let overlay_path = overlay_path.clone();
            cx.spawn(async move |this, cx| {
                let mut last = loader::fingerprint(&schema_path, overlay_path.as_deref());
                loop {
                    cx.background_executor().timer(Duration::from_secs(1)).await;
                    let cur = loader::fingerprint(&schema_path, overlay_path.as_deref());
                    if cur != last {
                        last = cur;
                        if this
                            .update(cx, |this: &mut Self, cx| this.reload_from_disk(cx))
                            .is_err()
                        {
                            break;
                        }
                    }
                }
            })
            .detach();
        }

        Self {
            schema_path,
            overlay_path,
            full_graph: loaded.graph,
            schema_name: loaded.name,
            mode,
            options,
            investigate: false,
            tree,
            canvas,
            sidebar_open: true,
        }
    }

    fn rebuild(&mut self, cx: &mut Context<Self>) {
        let model = Rc::new(build_model(
            slice_graph(&self.full_graph, self.mode),
            self.schema_name.clone(),
            &self.options,
        ));
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
                self.rebuild(cx);
            }
            Err(e) => eprintln!("reload failed: {e:#}"),
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

    fn mode_tab(&self, mode: Mode, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let active = self.mode == mode;
        toolbar_button(mode.label(), active)
            .on_click(cx.listener(move |this, _, _, cx| this.set_mode(mode, cx)))
    }

    fn toggle_button<F>(
        &self,
        label: &'static str,
        active: bool,
        on_click: F,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<F>
    where
        F: Fn(&mut Self, &mut Window, &mut Context<Self>) + 'static,
    {
        toolbar_button(label, active)
            .on_click(cx.listener(move |this, _, window, cx| on_click(this, window, cx)))
    }
}

fn toolbar_button(label: &'static str, active: bool) -> gpui::Stateful<gpui::Div> {
    div()
        .id(label)
        .px_3()
        .py_1()
        .rounded_md()
        .cursor_pointer()
        .text_xs()
        .when(active, |el| el.bg(rgb(0x2a3140)).text_color(rgb(0xe6e9ef)))
        .when(!active, |el| el.text_color(rgb(0x8b93a3)))
        .hover(|el| el.bg(rgb(0x232936)))
        .child(SharedString::from(label))
}

impl Render for Workspace {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let toolbar = div()
            .absolute()
            .top_2()
            .left_2()
            .flex()
            .items_center()
            .gap_1()
            .p_1()
            .rounded_lg()
            .bg(rgba(0x14171df0))
            .border_1()
            .border_color(rgb(0x242a35))
            .shadow_md()
            .child(self.mode_tab(Mode::Reachable, cx))
            .child(self.mode_tab(Mode::Orphaned, cx))
            .child(self.mode_tab(Mode::Deprecated, cx))
            .child(div().w(px(1.0)).h_4().bg(rgb(0x2c3340)))
            .child(self.toggle_button(
                "Desc",
                self.options.show_descriptions,
                |this, _, cx| {
                    this.options.show_descriptions = !this.options.show_descriptions;
                    this.rebuild(cx);
                },
                cx,
            ))
            .child(self.toggle_button(
                "Bundle",
                self.options.bundle_edges,
                |this, _, cx| {
                    this.options.bundle_edges = !this.options.bundle_edges;
                    this.rebuild(cx);
                },
                cx,
            ))
            .child(self.toggle_button(
                "Investigate",
                self.investigate,
                |this, _, cx| {
                    this.investigate = !this.investigate;
                    let investigate = this.investigate;
                    this.canvas
                        .update(cx, |canvas, cx| canvas.set_investigate(investigate, cx));
                },
                cx,
            ));

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
                    .bg(rgba(0x14171df0))
                    .border_1()
                    .border_color(rgb(0x242a35))
                    .shadow_md()
                    .text_xs()
                    .font_family("Menlo")
                    .text_color(rgb(0x8b93a3))
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
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &ToggleDescriptions, _, cx| {
                this.options.show_descriptions = !this.options.show_descriptions;
                this.rebuild(cx);
            }))
            .on_action(cx.listener(|this, _: &ToggleBundling, _, cx| {
                this.options.bundle_edges = !this.options.bundle_edges;
                this.rebuild(cx);
            }))
            .on_action(cx.listener(|this, _: &ToggleInvestigate, _, cx| {
                this.investigate = !this.investigate;
                let investigate = this.investigate;
                this.canvas
                    .update(cx, |canvas, cx| canvas.set_investigate(investigate, cx));
            }))
            .on_action(cx.listener(|this, _: &OpenSchema, _, cx| this.open_schema(cx)))
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
                    .when_some(breadcrumb, |el, b| el.child(b)),
            )
    }
}
