//! Root view: a custom title strip (the system titlebar is transparent),
//! then the landing screen (recent schemas / open) until a schema is loaded,
//! then the workspace.

use crate::config::{self, RecentEntry};
use crate::loader;
use crate::workspace::{OpenSchema, Workspace};
use gpui::{
    div, prelude::*, px, App, Context, Entity, ExternalPaths, MouseButton, PathPromptOptions,
    SharedString, Window,
};
use std::path::PathBuf;

pub struct Root {
    workspace: Option<Entity<Workspace>>,
    recents: Vec<RecentEntry>,
    error: Option<String>,
}

impl Root {
    pub fn new(
        initial: Option<(loader::LoadedSchema, PathBuf, Option<String>)>,
        cx: &mut Context<Self>,
    ) -> Self {
        let workspace = initial
            .map(|(loaded, path, overlay)| cx.new(|cx| Workspace::new(loaded, path, overlay, cx)));
        Self {
            workspace,
            recents: config::recents(),
            error: None,
        }
    }

    fn open_path(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        let hide_relay = config::load_settings().hide_relay;
        match loader::load(&path, None, hide_relay) {
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
        let th = crate::theme::theme(window.appearance());

        // Custom title strip: houses the traffic lights (transparent system
        // titlebar) and acts as the window drag area.
        let title_strip = div()
            .flex_none()
            .h(px(34.0))
            .w_full()
            .bg(th.panel)
            .border_b_1()
            .border_color(th.panel_border)
            .flex()
            .items_center()
            .justify_center()
            .on_mouse_down(MouseButton::Left, |_, window, _| {
                window.start_window_move();
            })
            .child(
                div()
                    .text_xs()
                    .font_family("Menlo")
                    .text_color(th.text_faint)
                    .child("GompassQL"),
            );

        let body: gpui::AnyElement = if let Some(ws) = &self.workspace {
            div().flex_1().min_h_0().child(ws.clone()).into_any_element()
        } else {
            let recents = self.recents.clone();
            let error = self.error.clone();
            div()
                .flex_1()
                .min_h_0()
                .bg(th.bg)
                .flex()
                .items_center()
                .justify_center()
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
                                .child("Open schema…   ⌘O   (or drop a .graphql file)"),
                        )
                        .when_some(error, |el, e| {
                            el.child(
                                div().text_sm().text_color(th.red).child(SharedString::from(e)),
                            )
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
                                    .w_full()
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
                                            .flex_none()
                                            .text_sm()
                                            .font_family("Menlo")
                                            .text_color(th.text)
                                            .child(SharedString::from(r.name)),
                                    )
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w_0()
                                            .text_xs()
                                            .text_color(th.text_faint)
                                            .whitespace_nowrap()
                                            .overflow_hidden()
                                            .child(SharedString::from(r.path)),
                                    )
                            }))
                        }),
                )
                .into_any_element()
        };

        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(th.bg)
            .on_action(cx.listener(|this, _: &OpenSchema, _, cx| {
                if this.workspace.is_none() {
                    this.open_dialog(cx)
                }
            }))
            .on_drop(cx.listener(|this, paths: &ExternalPaths, _, cx| {
                if let Some(path) = paths.paths().first() {
                    this.open_path(path.clone(), cx);
                }
            }))
            .child(title_strip)
            .child(body)
    }
}
