//! Two-pane workspace: tree sidebar + graph canvas, with mode tabs
//! (Reachable / Orphaned / Deprecated) and global shortcuts (⌘K search,
//! ⌘B sidebar).

use crate::canvas::GraphCanvas;
use crate::model::{build_model, slice_graph, Mode};
use crate::tree::{TreeEvent, TreePanel};
use gompass_core::graph::ParsedGraph;
use gpui::{
    actions, div, prelude::*, px, rgb, rgba, App, Context, Entity, KeyBinding, SharedString,
    Window,
};
use std::rc::Rc;

actions!(gompass, [FocusSearch, ToggleSidebar]);

pub fn init(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("cmd-k", FocusSearch, None),
        KeyBinding::new("cmd-b", ToggleSidebar, None),
    ]);
}

pub struct Workspace {
    full_graph: ParsedGraph,
    schema_name: String,
    mode: Mode,
    tree: Entity<TreePanel>,
    canvas: Entity<GraphCanvas>,
    sidebar_open: bool,
}

impl Workspace {
    pub fn new(graph: ParsedGraph, schema_name: String, cx: &mut Context<Self>) -> Self {
        let mode = Mode::Reachable;
        let model = Rc::new(build_model(slice_graph(&graph, mode), schema_name.clone()));
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
        Self {
            full_graph: graph,
            schema_name,
            mode,
            tree,
            canvas,
            sidebar_open: true,
        }
    }

    fn set_mode(&mut self, mode: Mode, cx: &mut Context<Self>) {
        if mode == self.mode {
            return;
        }
        self.mode = mode;
        let model = Rc::new(build_model(
            slice_graph(&self.full_graph, mode),
            self.schema_name.clone(),
        ));
        self.tree.update(cx, |tree, cx| tree.set_model(model.clone(), cx));
        self.canvas.update(cx, |canvas, cx| canvas.set_model(model, cx));
        cx.notify();
    }

    fn mode_tab(&self, mode: Mode, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let active = self.mode == mode;
        div()
            .id(mode.label())
            .px_3()
            .py_1()
            .rounded_md()
            .cursor_pointer()
            .text_xs()
            .when(active, |el| el.bg(rgb(0x2a3140)).text_color(rgb(0xe6e9ef)))
            .when(!active, |el| el.text_color(rgb(0x8b93a3)))
            .hover(|el| el.bg(rgb(0x232936)))
            .on_click(cx.listener(move |this, _, _, cx| this.set_mode(mode, cx)))
            .child(SharedString::from(mode.label()))
    }
}

impl Render for Workspace {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let tabs = div()
            .absolute()
            .top_2()
            .left_2()
            .flex()
            .gap_1()
            .p_1()
            .rounded_lg()
            .bg(rgba(0x14171de0))
            .border_1()
            .border_color(rgb(0x242a35))
            .child(self.mode_tab(Mode::Reachable, cx))
            .child(self.mode_tab(Mode::Orphaned, cx))
            .child(self.mode_tab(Mode::Deprecated, cx));

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
                    .child(tabs),
            )
    }
}
