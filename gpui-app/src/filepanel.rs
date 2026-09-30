//! The file pane: the schema's own text, beside the graph drawn from it.
//!
//! The graph is an interpretation. When it says something surprising the
//! next question is always "what does the file actually say", and until now
//! answering it meant leaving for an editor. This pane puts the source next
//! to the picture, with the line numbers to talk about it by.
//!
//! It opens read-only. The buffer is somebody's schema on disk, and a pane
//! you opened to read should not be one keypress away from rewriting it.

use crate::editor::{EditorEvent, TextArea};
use crate::field::{self, FieldKey, MONO};
use crate::icons::{icon, Icon};
use crate::textedit::TextEdit;
use crate::theme::Theme;
use gpui::{
    div, prelude::*, px, App, Context, Entity, EventEmitter, FocusHandle, Focusable, KeyDownEvent,
    MouseButton, SharedString, Window,
};
use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::rc::Rc;

pub enum FileEvent {
    /// Written to disk: the workspace reloads the graph from it.
    Saved,
    Close,
}

pub struct FilePanel {
    editor: Entity<TextArea>,
    path: Option<PathBuf>,
    /// What was last read from or written to disk, to tell edited from not.
    on_disk: String,
    read_only: bool,
    find: TextEdit,
    find_origin: Rc<Cell<f32>>,
    find_focus: FocusHandle,
    matches: Vec<(usize, usize)>,
    active: usize,
    error: Option<String>,
    focus: FocusHandle,
}

impl FilePanel {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let editor = cx.new(|cx| {
            let mut e = TextArea::new(cx);
            e.gutter = true;
            e.read_only = true;
            e.placeholder = "No file open.";
            e
        });
        cx.subscribe(&editor, |this: &mut Self, _, event: &EditorEvent, cx| match event {
            EditorEvent::Save => this.save(cx),
            // Retyping invalidates the byte offsets the hits are made of.
            EditorEvent::Changed => this.refind(cx),
            EditorEvent::Submitted => {}
        })
        .detach();
        FilePanel {
            editor,
            path: None,
            on_disk: String::new(),
            read_only: true,
            find: TextEdit::default(),
            find_origin: Rc::new(Cell::new(0.0)),
            find_focus: cx.focus_handle(),
            matches: Vec::new(),
            active: 0,
            error: None,
            focus: cx.focus_handle(),
        }
    }

    /// Read `path` from disk. Called when the pane opens and after the
    /// workspace reloads, so the text is the file rather than a memory of it.
    pub fn load(&mut self, path: &Path, cx: &mut Context<Self>) {
        if self.path.as_deref() == Some(path) && self.is_dirty_with(cx) {
            // An edit in progress outranks a re-read; losing it silently
            // because something else touched the graph would be worse.
            return;
        }
        match std::fs::read_to_string(path) {
            Ok(text) => {
                self.error = None;
                self.on_disk = text.clone();
                self.editor.update(cx, |e, cx| e.set_text(text, cx));
            }
            Err(e) => self.error = Some(format!("{e}")),
        }
        self.path = Some(path.to_path_buf());
        self.refind(cx);
        cx.notify();
    }

    fn save(&mut self, cx: &mut Context<Self>) {
        let (Some(path), false) = (self.path.clone(), self.read_only) else { return };
        let text = self.editor.read(cx).text().to_string();
        match std::fs::write(&path, &text) {
            Ok(()) => {
                self.on_disk = text;
                self.error = None;
                cx.emit(FileEvent::Saved);
            }
            Err(e) => self.error = Some(format!("{e}")),
        }
        cx.notify();
    }

    fn set_read_only(&mut self, ro: bool, cx: &mut Context<Self>) {
        self.read_only = ro;
        self.editor.update(cx, |e, cx| {
            e.read_only = ro;
            cx.notify();
        });
        cx.notify();
    }

    /// Every case-insensitive hit for the find box, in byte ranges.
    fn refind(&mut self, cx: &mut Context<Self>) {
        let needle = self.find.text.to_lowercase();
        self.matches.clear();
        if !needle.is_empty() {
            let hay = self.editor.read(cx).text().to_lowercase();
            let mut from = 0usize;
            while let Some(i) = hay[from..].find(&needle) {
                let s = from + i;
                self.matches.push((s, s + needle.len()));
                // Overlapping hits would stack bands on the same glyphs.
                from = s + needle.len().max(1);
            }
        }
        self.active = self.active.min(self.matches.len().saturating_sub(1));
        let (m, a) = (self.matches.clone(), self.active);
        self.editor.update(cx, |e, cx| {
            e.set_matches(m, a);
            cx.notify();
        });
        cx.notify();
    }

    fn go(&mut self, delta: isize, cx: &mut Context<Self>) {
        if self.matches.is_empty() {
            return;
        }
        let n = self.matches.len();
        self.active = if delta < 0 {
            (self.active + n - 1) % n
        } else {
            (self.active + 1) % n
        };
        let at = self.matches[self.active].0;
        let (m, a) = (self.matches.clone(), self.active);
        self.editor.update(cx, |e, cx| {
            e.set_matches(m, a);
            e.reveal(at, cx);
        });
        cx.notify();
    }

    fn on_find_key(&mut self, ev: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        let shift = ev.keystroke.modifiers.shift;
        match field::key(&mut self.find, ev, cx) {
            FieldKey::Edited => {
                self.active = 0;
                self.refind(cx);
                // Land on the first hit as you type, the way a find bar does.
                if let Some(&(at, _)) = self.matches.first() {
                    self.editor.update(cx, |e, cx| e.reveal(at, cx));
                }
            }
            FieldKey::Moved => {}
            FieldKey::Enter => self.go(if shift { -1 } else { 1 }, cx),
            FieldKey::Up => self.go(-1, cx),
            FieldKey::Down => self.go(1, cx),
            FieldKey::Escape => {
                self.find.clear();
                self.refind(cx);
            }
            FieldKey::Ignored => return,
        }
        cx.notify();
    }

    fn header(&mut self, th: Theme, window: &mut Window, cx: &mut Context<Self>) -> gpui::Div {
        let dirty = self.is_dirty_with(cx);
        let name: SharedString = self
            .path
            .as_ref()
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().into_owned().into())
            .unwrap_or_else(|| SharedString::from("—"));
        let ro = self.read_only;
        let found: SharedString = if self.find.text.is_empty() {
            "".into()
        } else if self.matches.is_empty() {
            "0".into()
        } else {
            format!("{}/{}", self.active + 1, self.matches.len()).into()
        };
        div()
            .flex_none()
            .flex()
            .flex_col()
            .border_b_1()
            .border_color(th.panel_border)
            .child(
                div()
                    .h(px(32.0))
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_2()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .font_family(MONO)
                            .text_size(px(11.0))
                            .text_color(th.text)
                            .child(name),
                    )
                    .when(dirty, |el| {
                        el.child(
                            div()
                                .flex_none()
                                .text_size(px(10.0))
                                .text_color(th.type_amber)
                                .child("edited · ⌘S"),
                        )
                    })
                    .child(
                        div()
                            .id("file-readonly")
                            .flex_none()
                            .flex()
                            .items_center()
                            .gap_1()
                            .rounded_md()
                            .border_1()
                            .px(px(6.0))
                            .py(px(2.0))
                            .text_size(px(10.0))
                            .cursor_pointer()
                            .when(ro, |el| {
                                el.border_color(th.card_border).text_color(th.text_muted)
                            })
                            .when(!ro, |el| {
                                el.border_color(th.type_amber).text_color(th.type_amber)
                            })
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.set_read_only(!ro, cx)
                            }))
                            .child(icon(
                                if ro { Icon::Lock } else { Icon::LockOpen },
                                px(11.0),
                                if ro { th.text_muted } else { th.type_amber },
                            ))
                            .child(if ro { "Read only" } else { "Editing" }),
                    )
                    .child(
                        div()
                            .id("file-close")
                            .flex_none()
                            .cursor_pointer()
                            .rounded_md()
                            .p_1()
                            .hover(|el| el.bg(th.hover_bg))
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(FileEvent::Close)))
                            .child(icon(Icon::X, px(12.0), th.text_muted)),
                    ),
            )
            .child(
                div()
                    .h(px(28.0))
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_2()
                    .border_t_1()
                    .border_color(th.panel_border)
                    .track_focus(&self.find_focus)
                    .on_key_down(cx.listener(Self::on_find_key))
                    .child(icon(Icon::Search, px(11.0), th.text_muted))
                    .child(field::input(
                        field::InputProps {
                            th,
                            edit: &self.find,
                            placeholder: "Find in file…",
                            focused: self.find_focus.is_focused(window),
                            font_px: 11.0,
                            line_h: 16.0,
                            origin: self.find_origin.clone(),
                        },
                        cx,
                        |this, offset, cx| {
                            this.find.move_cursor(offset, false);
                            cx.notify();
                        },
                    ))
                    .child(
                        div()
                            .flex_none()
                            .font_family(MONO)
                            .text_size(px(10.0))
                            .text_color(th.text_faint)
                            .child(found),
                    )
                    .child(
                        div()
                            .id("find-prev")
                            .flex_none()
                            .cursor_pointer()
                            .rounded(px(3.0))
                            .hover(|el| el.bg(th.hover_bg))
                            .on_click(cx.listener(|this, _, _, cx| this.go(-1, cx)))
                            .child(icon(Icon::ChevronUp, px(12.0), th.text_muted)),
                    )
                    .child(
                        div()
                            .id("find-next")
                            .flex_none()
                            .cursor_pointer()
                            .rounded(px(3.0))
                            .hover(|el| el.bg(th.hover_bg))
                            .on_click(cx.listener(|this, _, _, cx| this.go(1, cx)))
                            .child(icon(Icon::ChevronDown, px(12.0), th.text_muted)),
                    ),
            )
    }

    /// Debug: GRAVIZ_FILE=<query> opens the pane with that search running,
    /// so a selfshot can reproduce it.
    pub fn debug_find(&mut self, q: String, cx: &mut Context<Self>) {
        self.find.set_text(q);
        self.refind(cx);
        if let Some(&(at, _)) = self.matches.first() {
            self.editor.update(cx, |e, cx| e.reveal(at, cx));
        }
    }

    fn is_dirty_with(&self, cx: &App) -> bool {
        self.editor.read(cx).text() != self.on_disk
    }
}

impl Focusable for FilePanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl EventEmitter<FileEvent> for FilePanel {}

impl Render for FilePanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let th = crate::theme::current(cx, window.appearance());
        let header = self.header(th, window, cx);
        div()
            .size_full()
            .flex()
            .flex_col()
            .min_w_0()
            .bg(th.panel)
            .border_l_1()
            .border_color(th.panel_border)
            .track_focus(&self.focus)
            // escape is an app-wide action binding, dispatched before any key
            // handler, so the find box can only hear it this way. Only while
            // it has something to clear.
            .on_action(cx.listener(
                |this, _: &crate::workspace::ClearSelection, window, cx| {
                    if this.find_focus.is_focused(window) && !this.find.text.is_empty() {
                        this.find.clear();
                        this.refind(cx);
                        cx.stop_propagation();
                        cx.notify();
                    }
                },
            ))
            // The canvas is behind this pane and reads raw mouse events; a
            // press meant for the text must not also pan the graph.
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(header)
            .when_some(self.error.clone(), |el, e| {
                el.child(
                    div()
                        .flex_none()
                        .px_2()
                        .py_1()
                        .text_size(px(10.0))
                        .text_color(th.red)
                        .child(SharedString::from(e)),
                )
            })
            .child(div().flex_1().min_h_0().p_1().child(self.editor.clone()))
    }
}
