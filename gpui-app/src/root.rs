//! Root view: the landing screen (recent schemas / open) until a schema is
//! loaded, then the workspace.

use crate::config::{self, RecentEntry};
use crate::loader;
use crate::workspace::{OpenSchema, Workspace};
use gpui::{
    div, prelude::*, px, App, Context, Entity, PathPromptOptions, SharedString, Window,
};
use std::path::PathBuf;

pub struct Root {
    workspace: Option<Entity<Workspace>>,
    recents: Vec<RecentEntry>,
    error: Option<String>,
}

impl Root {
    pub fn new(
        initial: Option<(loader::LoadedSchema, PathBuf, Option<PathBuf>)>,
        cx: &mut Context<Self>,
    ) -> Self {
        let workspace =
            initial.map(|(loaded, path, overlay)| cx.new(|cx| Workspace::new(loaded, path, overlay, cx)));
        Self {
            workspace,
            recents: config::recents(),
            error: None,
        }
    }

    fn open_path(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        match loader::load(&path, None) {
            Ok(loaded) => {
                self.error = None;
                self.workspace = Some(cx.new(|cx| Workspace::new(loaded, path, None, cx)));
            }
            Err(e) => {
                self.error = Some(format!("{e:#}"));
                self.recents = config::recents();
            }
        }
        cx.notify();
    }

    fn open_dialog(&mut self, cx: &mut Context<Self>) {
        let rx = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Open schema".into()),
        });
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(mut paths))) = rx.await {
                if let Some(path) = paths.pop() {
                    let _ = this.update(cx, |this: &mut Self, cx| this.open_path(path, cx));
                }
            }
        })
        .detach();
    }
}

impl Render for Root {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some(ws) = &self.workspace {
            return div().size_full().child(ws.clone());
        }

        let th = crate::theme::theme(window.appearance());
        let recents = self.recents.clone();
        let error = self.error.clone();

        div()
            .size_full()
            .bg(th.bg)
            .flex()
            .items_center()
            .justify_center()
            .on_action(cx.listener(|this, _: &OpenSchema, _, cx| this.open_dialog(cx)))
            .child(
                div()
                    .w(px(460.0))
                    .flex()
                    .flex_col()
                    .gap_3()
                    .child(
                        div()
                            .text_2xl()
                            .font_family("Menlo")
                            .text_color(th.text)
                            .child("GompassQL"),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(th.text_muted)
                            .child("GraphQL schema visualizer"),
                    )
                    .child(
                        div()
                            .id("open")
                            .mt_2()
                            .px_4()
                            .py_2()
                            .rounded_md()
                            .bg(th.active_bg)
                            .text_color(th.text)
                            .text_sm()
                            .cursor_pointer()
                            .hover(|el| el.bg(th.hover_bg))
                            .on_click(cx.listener(|this, _, _, cx| this.open_dialog(cx)))
                            .child("Open schema…   ⌘O"),
                    )
                    .when_some(error, |el, e| {
                        el.child(div().text_sm().text_color(th.red).child(SharedString::from(e)))
                    })
                    .when(!recents.is_empty(), |el| {
                        el.child(
                            div()
                                .mt_4()
                                .text_xs()
                                .text_color(th.text_faint)
                                .child("Recent"),
                        )
                        .children(recents.into_iter().enumerate().map(|(i, r)| {
                            let path = PathBuf::from(r.path.clone());
                            div()
                                .id(i)
                                .px_3()
                                .py_1()
                                .rounded_md()
                                .cursor_pointer()
                                .hover(|el| el.bg(th.hover_bg))
                                .flex()
                                .gap_2()
                                .items_center()
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.open_path(path.clone(), cx)
                                }))
                                .child(
                                    div()
                                        .text_sm()
                                        .font_family("Menlo")
                                        .text_color(th.text)
                                        .child(SharedString::from(r.name)),
                                )
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(th.text_faint)
                                        .whitespace_nowrap()
                                        .overflow_hidden()
                                        .child(SharedString::from(r.path)),
                                )
                        }))
                    }),
            )
    }
}
