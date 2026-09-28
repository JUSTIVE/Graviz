//! Root view: a custom title strip (the system titlebar is transparent),
//! then the landing screen (recent schemas / open) until a schema is loaded,
//! then the workspace.

use crate::config::{self, RecentEntry};
use crate::editor::TextArea;
use crate::landing;
use crate::loader;
use crate::workspace::{OpenSchema, OpenSettings, Workspace};
use gpui::{
    div, prelude::*, px, App, Context, Entity, ExternalPaths, FocusHandle, Focusable,
    PathPromptOptions, Window,
};
use std::path::PathBuf;

pub struct Root {
    workspace: Option<Entity<Workspace>>,
    recents: Vec<RecentEntry>,
    error: Option<String>,
    /// "New" tab: show the landing even though a schema is loaded.
    show_landing: bool,
    show_about: bool,
    show_settings: bool,
    editor: Entity<TextArea>,
    recents_open: bool,
    warnings: Vec<String>,
    dragging: bool,
    schema_name: Option<String>,
    /// The landing screen has no other focusable element, so without this its
    /// shortcuts (⌘O) would have nowhere to dispatch.
    focus: FocusHandle,
    focused_once: bool,
    /// Set once the background GitHub-releases check finds a newer tag.
    update_available: Option<crate::update_check::UpdateInfo>,
}

impl Root {
    pub fn new(
        initial: Option<(loader::LoadedSchema, PathBuf, Option<String>)>,
        cx: &mut Context<Self>,
    ) -> Self {
        let workspace = initial
            .map(|(loaded, path, overlay)| cx.new(|cx| Workspace::new(loaded, path, overlay, cx)));
        let mode = config::load_settings().theme_mode;
        crate::theme::set_mode(cx, mode);
        let editor = cx.new(|cx| {
            let mut e = TextArea::new(cx);
            e.placeholder = "# Paste your GraphQL SDL here…";
            e
        });
        cx.spawn(async move |this, cx| {
            let info = cx
                .background_executor()
                .spawn(async { crate::update_check::check_for_update() })
                .await;
            if let Some(info) = info {
                this.update(cx, |root, cx| {
                    root.update_available = Some(info);
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
        Self {
            workspace,
            recents: config::recents(),
            error: None,
            show_landing: false,
            // Debug: GRAVIZ_ABOUT=1 opens on the About page, for selfshots.
            show_about: std::env::var("GRAVIZ_ABOUT").is_ok(),
            // Debug: GRAVIZ_SETTINGS=1 opens on Settings, for selfshots.
            show_settings: std::env::var("GRAVIZ_SETTINGS").is_ok(),
            editor,
            // Debug: GRAVIZ_RECENTS=1 opens the list, for selfshots.
            recents_open: std::env::var("GRAVIZ_RECENTS").is_ok(),
            warnings: Vec::new(),
            dragging: false,
            schema_name: None,
            focus: cx.focus_handle(),
            focused_once: false,
            update_available: None,
        }
    }

    fn open_path(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        let hide_relay = config::load_settings().hide_relay;
        match loader::load(&path, None, hide_relay) {
            Ok(loaded) => {
                self.error = None;
                self.show_landing = false;
                self.workspace = Some(cx.new(|cx| Workspace::new(loaded, path, None, cx)));
            }
            Err(e) => {
                self.error = Some(format!("{e:#}"));
                self.recents = config::recents();
            }
        }
        cx.notify();
    }

    /// Parse whatever is in the editor and open the workspace on it.
    fn visualize(&mut self, cx: &mut Context<Self>) {
        let sdl = self.editor.read(cx).text().to_string();
        if sdl.trim().is_empty() {
            self.error = Some("Paste an SDL or drop a .graphql file.".into());
            cx.notify();
            return;
        }
        let hide_relay = config::load_settings().hide_relay;
        let name = self
            .schema_name
            .clone()
            .unwrap_or_else(|| "Pasted schema".to_string());
        match loader::load_sdl(&sdl, name, None, hide_relay) {
            Ok(loaded) if loaded.graph.nodes.is_empty() => {
                self.error = Some("No types found in this SDL.".into());
            }
            Ok(loaded) => {
                self.warnings = loaded.graph.warnings.clone();
                self.error = None;
                self.show_landing = false;
                let path = PathBuf::from("(pasted)");
                self.workspace = Some(cx.new(|cx| Workspace::new(loaded, path, None, cx)));
            }
            Err(e) => self.error = Some(format!("{e:#}")),
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

impl Focusable for Root {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for Root {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let th = crate::theme::current(cx, window.appearance());

        let has_schema = self.workspace.is_some();
        let route = if self.show_about {
            crate::shell::Route::About
        } else if self.show_settings {
            crate::shell::Route::Settings
        } else if self.show_landing || !has_schema {
            crate::shell::Route::New
        } else {
            crate::shell::Route::View
        };
        let update_badge = self.update_available.as_ref().map(|info| {
            let url = info.url.clone();
            div()
                .id("update-available")
                .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .h(px(20.0))
                .flex()
                .items_center()
                .gap_1()
                .rounded_md()
                .border_1()
                .border_color(th.investigate)
                .px(px(6.0))
                .text_xs()
                .text_color(th.investigate)
                .cursor_pointer()
                .hover(|el| el.bg(th.investigate.opacity(0.1)))
                .on_click(move |_, _, cx| cx.open_url(&url))
                .child(crate::icons::icon(crate::icons::Icon::Sparkles, px(12.0), th.investigate))
                .child(gpui::SharedString::from(format!("v{} available", info.version)))
                .into_any_element()
        });
        let home = std::env::var("HOME").ok();
        let file = self
            .workspace
            .as_ref()
            .map(|w| crate::shell::file_label(w.read(cx).schema_path(), home.as_deref()));
        let header = crate::shell::header(th, file, update_badge);
        let rail = crate::shell::rail(
            th,
            route,
            has_schema,
            |this: &mut Self, route, _window, cx| {
                this.show_about = route == crate::shell::Route::About;
                this.show_settings = route == crate::shell::Route::Settings;
                this.show_landing = route == crate::shell::Route::New;
                cx.notify();
            },
            cx,
        );

        let body: gpui::AnyElement = if self.show_settings {
            crate::settings::view(
                th,
                crate::theme::mode(cx),
                config::settings_path()
                    .map(|p| gpui::SharedString::from(p.to_string_lossy().into_owned())),
                |_this: &mut Self, mode, _w, cx| {
                    crate::theme::set_mode(cx, mode);
                    let mut s = config::load_settings();
                    s.theme_mode = mode;
                    config::save_settings(&s);
                    cx.notify();
                },
                cx,
            )
            .into_any_element()
        } else if self.show_about {
            crate::about::view(
                th,
                |this: &mut Self, _w, cx| {
                    this.show_about = false;
                    this.show_landing = this.workspace.is_none();
                    cx.notify();
                },
                cx,
            )
            .into_any_element()
        } else if let (Some(ws), false) = (&self.workspace, self.show_landing) {
            div().flex_1().min_h_0().child(ws.clone()).into_any_element()
        } else {
            landing::view(
                landing::LandingProps {
                    th,
                    editor: &self.editor,
                    recents: &self.recents,
                    recents_open: self.recents_open,
                    schema_name: self.schema_name.as_deref(),
                    error: self.error.as_deref(),
                    warnings: &self.warnings,
                    dragging: self.dragging,
                },
                |this: &mut Self, _w, cx| this.open_dialog(cx),
                |this: &mut Self, _w, cx| {
                    this.editor
                        .update(cx, |e, cx| e.set_text(landing::SAMPLE_SDL.to_string(), cx));
                    this.schema_name = Some("Sample blog schema".into());
                    this.error = None;
                    cx.notify();
                },
                |this: &mut Self, _w, cx| this.visualize(cx),
                |this: &mut Self, _w, cx| {
                    this.recents_open = !this.recents_open;
                    cx.notify();
                },
                |this: &mut Self, _w, cx| {
                    config::write_recents(&[]);
                    this.recents = config::recents();
                    cx.notify();
                },
                |this: &mut Self, path, _w, cx| this.open_path(path, cx),
                |this: &mut Self, path, _w, cx| {
                    landing::remove_recent(&path);
                    this.recents = config::recents();
                    cx.notify();
                },
                cx,
            )
            .into_any_element()
        };

        if !self.focused_once {
            self.focused_once = true;
            window.focus(&self.focus, cx);
        }
        div()
            .size_full()
            .relative()
            .flex()
            .flex_col()
            .track_focus(&self.focus)
            .key_context("Root")
            .bg(th.bg)
            .on_action(cx.listener(|this, _: &OpenSchema, _, cx| {
                if this.workspace.is_none() {
                    this.open_dialog(cx)
                }
            }))
            // ⌘, toggles: pressing it again puts you back where you were,
            // rather than stranding you on a page with no way out but the nav.
            .on_action(cx.listener(|this, _: &OpenSettings, _, cx| {
                this.show_settings = !this.show_settings;
                if this.show_settings {
                    this.show_about = false;
                }
                cx.notify();
            }))
            .on_drop(cx.listener(|this, paths: &ExternalPaths, _, cx| {
                if let Some(path) = paths.paths().first() {
                    this.open_path(path.clone(), cx);
                }
            }))
            .child(header)
            // The rail is the outermost pane: the schema sidebar and the
            // canvas both start to its right.
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .child(rail)
                    .child(div().flex_1().min_w_0().flex().flex_col().child(body)),
            )
            .when_some(crate::shell::commit_badge(th), |el, b| el.child(b))
    }
}
