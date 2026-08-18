//! Two-pane workspace: tree sidebar + graph canvas, with global shortcuts
//! (⌘K focuses search, ⌘B toggles the sidebar).

use crate::canvas::GraphCanvas;
use crate::model::Model;
use crate::tree::{TreeEvent, TreePanel};
use gpui::{
    actions, div, prelude::*, px, App, Context, Entity, KeyBinding, Window,
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
    tree: Entity<TreePanel>,
    canvas: Entity<GraphCanvas>,
    sidebar_open: bool,
}

impl Workspace {
    pub fn new(model: Rc<Model>, cx: &mut Context<Self>) -> Self {
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
        Self { tree, canvas, sidebar_open: true }
    }
}

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
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
            .child(div().flex_1().h_full().min_w_0().child(self.canvas.clone()))
    }
}
