//! The file pane: the schema's own text, beside the graph drawn from it.
//!
//! The graph is an interpretation. When it says something surprising the
//! next question is always "what does the file actually say", and until now
//! answering it meant leaving for an editor. This pane puts the source next
//! to the picture, with the line numbers to talk about it by.
//!
//! It opens read-only, and unlocking it never touches the file. Edits apply
//! to the graph the way the overlay does: the drawing follows the buffer, the
//! file on disk stays as it was, and closing the pane or reloading throws the
//! edit away. A pane you opened to read a schema should not be able to
//! rewrite somebody's source, and a sketch is worth more when it costs
//! nothing to abandon.

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

/// A right-click menu over the text, placed in the pane's own coordinates.
struct Menu {
    x: f32,
    y: f32,
    type_name: SharedString,
}

pub enum FileEvent {
    /// Show this type on the canvas.
    GoToType(String),
    /// Draw the graph from this text instead of the file. Nothing is written.
    Apply(String),
    /// Forget the edit and go back to the file as it is on disk.
    Revert,
    Close,
}

pub struct FilePanel {
    editor: Entity<TextArea>,
    path: Option<PathBuf>,
    /// The file as it is on disk, to tell edited from not and to revert to.
    on_disk: String,
    /// The text last applied to the graph, when it is not the file's.
    applied: Option<String>,
    read_only: bool,
    find: TextEdit,
    find_origin: Rc<Cell<f32>>,
    find_focus: FocusHandle,
    matches: Vec<(usize, usize)>,
    active: usize,
    error: Option<String>,
    focus: FocusHandle,
    /// The graph as currently drawn, to tell a type name from any other word.
    model: Option<Rc<crate::model::Model>>,
    menu: Option<Menu>,
    /// The pane's own origin, so a window-space click can be placed inside it.
    origin: Rc<Cell<(f32, f32)>>,
}

impl FilePanel {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let editor = cx.new(|cx| {
            let mut e = TextArea::new(cx);
            e.gutter = true;
            e.read_only = true;
            e.surface = true;
            e.placeholder = "No file open.";
            e
        });
        cx.subscribe(&editor, |this: &mut Self, _, event: &EditorEvent, cx| match event {
            EditorEvent::Save => this.apply(cx),
            // Retyping invalidates the byte offsets the hits are made of.
            EditorEvent::Changed => this.refind(cx),
            EditorEvent::Submitted => {}
            EditorEvent::RightClick { offset, x, y } => this.open_menu(*offset, *x, *y, cx),
        })
        .detach();
        FilePanel {
            editor,
            path: None,
            on_disk: String::new(),
            applied: None,
            read_only: true,
            find: TextEdit::default(),
            find_origin: Rc::new(Cell::new(0.0)),
            find_focus: cx.focus_handle(),
            matches: Vec::new(),
            active: 0,
            error: None,
            focus: cx.focus_handle(),
            model: None,
            menu: None,
            origin: Rc::new(Cell::new((0.0, 0.0))),
        }
    }

    /// The workspace's current graph. Only names in it are worth offering to
    /// jump to; everything else in the file is prose or punctuation.
    pub fn set_model(&mut self, model: Rc<crate::model::Model>) {
        self.model = Some(model);
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
                self.applied = None;
                self.editor.update(cx, |e, cx| e.set_text(text, cx));
            }
            Err(e) => self.error = Some(format!("{e}")),
        }
        self.path = Some(path.to_path_buf());
        self.refind(cx);
        cx.notify();
    }

    /// ⌘S applies the buffer to the graph. It does not write the file: this
    /// pane is a sketchpad over the schema, not an editor of it.
    fn apply(&mut self, cx: &mut Context<Self>) {
        if self.read_only {
            return;
        }
        let text = self.editor.read(cx).text().to_string();
        self.applied = Some(text.clone());
        self.error = None;
        cx.emit(FileEvent::Apply(text));
        cx.notify();
    }

    /// Offer to jump when the click landed on a name the graph knows.
    fn open_menu(&mut self, offset: usize, x: f32, y: f32, cx: &mut Context<Self>) {
        let word = {
            let text = self.editor.read(cx).text();
            word_at(text, offset)
        };
        let known = word.filter(|w| {
            self.model.as_ref().is_some_and(|m| m.index_of.contains_key(w.as_str()))
        });
        let (ox, oy) = self.origin.get();
        self.menu = known.map(|w| Menu { x: x - ox, y: y - oy, type_name: w.into() });
        cx.notify();
    }

    fn close_menu(&mut self, cx: &mut Context<Self>) {
        if self.menu.take().is_some() {
            cx.notify();
        }
    }

    /// Put the caret on the start of 1-based `line` and scroll it into view.
    pub fn reveal_line(&mut self, line: u32, cx: &mut Context<Self>) {
        let at = {
            let text = self.editor.read(cx).text();
            line_start(text, line)
        };
        self.editor.update(cx, |e, cx| e.reveal(at, cx));
        cx.notify();
    }

    /// The workspace went back to drawing the file, so what was applied no
    /// longer is. The buffer is left alone: the text is the reader's, and
    /// the header's "⌘S to draw this" already says it is not on screen.
    pub fn sketch_dropped(&mut self, cx: &mut Context<Self>) {
        if self.applied.take().is_some() {
            cx.notify();
        }
    }

    /// Put the file back, dropping whatever was typed over it.
    fn revert(&mut self, cx: &mut Context<Self>) {
        let text = self.on_disk.clone();
        self.applied = None;
        self.editor.update(cx, |e, cx| e.set_text(text, cx));
        cx.emit(FileEvent::Revert);
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
        self.matches.clear();
        if !self.find.text.is_empty() {
            self.matches = find_all(self.editor.read(cx).text(), &self.find.text);
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
        let applied = self.applied.is_some();
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
                                // The file is never written, so the words have
                                // to say what the keystroke actually does.
                                .child("⌘S to draw this"),
                        )
                    })
                    .when(applied, |el| {
                        el.child(
                            div()
                                .id("file-revert")
                                .flex_none()
                                .rounded_md()
                                .border_1()
                                .border_color(th.card_border)
                                .px(px(6.0))
                                .py(px(2.0))
                                .text_size(px(10.0))
                                .text_color(th.text_muted)
                                .cursor_pointer()
                                .hover(|el| el.bg(th.hover_bg))
                                .on_click(cx.listener(|this, _, _, cx| this.revert(cx)))
                                .child("Revert to file"),
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
                            .child(if ro { "Read only" } else { "Sketching" }),
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

/// The identifier `offset` falls in, if any.
///
/// GraphQL names are letters, digits and underscores, so the word runs out
/// to whatever is neither. A click on a space or a brace is not on a name.
fn word_at(text: &str, offset: usize) -> Option<String> {
    let at = offset.min(text.len());
    if !text.is_char_boundary(at) {
        return None;
    }
    let is_name = |c: char| c.is_alphanumeric() || c == '_';
    let start = text[..at]
        .char_indices()
        .rev()
        .take_while(|(_, c)| is_name(*c))
        .last()
        .map(|(i, _)| i)
        .unwrap_or(at);
    let end = at + text[at..].chars().take_while(|c| is_name(*c)).map(char::len_utf8).sum::<usize>();
    (start < end).then(|| text[start..end].to_string())
}

/// Byte offset where 1-based `line` starts, clamped to the end of the text.
///
/// Line 0 does not exist: the parser counts from one, and anything without a
/// source is marked 0 rather than pointing at the top of the file.
fn line_start(text: &str, line: u32) -> usize {
    if line <= 1 {
        return 0;
    }
    let mut seen = 1u32;
    for (i, c) in text.char_indices() {
        if c == '\n' {
            seen += 1;
            if seen == line {
                return i + 1;
            }
        }
    }
    text.len()
}

/// Case-insensitive hits, as byte ranges into `hay` itself.
///
/// Lowercasing the whole buffer first and searching that is the obvious way
/// and a trap: `to_lowercase` can change a string's length (`İ` is two bytes,
/// its lowercase is three), and every offset past such a character then
/// points somewhere else in the original. Painting one slices a character in
/// half, which is a panic, which is the whole window gone. Comparing in
/// place keeps the offsets honest.
fn find_all(hay: &str, needle: &str) -> Vec<(usize, usize)> {
    let n = needle.len();
    let mut out = Vec::new();
    if n == 0 || n > hay.len() {
        return out;
    }
    let mut i = 0usize;
    while i + n <= hay.len() {
        if hay.is_char_boundary(i)
            && hay.is_char_boundary(i + n)
            && hay[i..i + n].eq_ignore_ascii_case(needle)
        {
            out.push((i, i + n));
            // Overlapping hits would stack bands on the same glyphs.
            i += n;
        } else {
            i += 1;
        }
    }
    out
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
        let origin = self.origin.clone();
        div()
            .size_full()
            .relative()
            .flex()
            .flex_col()
            .min_w_0()
            .bg(th.panel)
            .border_l_1()
            .border_color(th.panel_border)
            .track_focus(&self.focus)
            .child(
                gpui::canvas(
                    |_, _, _| (),
                    move |bounds, _, _, _| {
                        origin.set((
                            f32::from(bounds.origin.x),
                            f32::from(bounds.origin.y),
                        ))
                    },
                )
                .absolute()
                .size_full(),
            )
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
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    this.close_menu(cx);
                    cx.stop_propagation();
                }),
            )
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
            .child(div().flex_1().min_h_0().child(self.editor.clone()))
            .when_some(self.menu.as_ref().map(|m| (m.x, m.y, m.type_name.clone())), |el, (x, y, name)| {
                el.child(
                    div()
                        .absolute()
                        .left(px(x))
                        .top(px(y))
                        .min_w(px(160.0))
                        .rounded_lg()
                        .border_1()
                        .border_color(th.card_border)
                        .bg(th.chrome_bg)
                        .py_1()
                        .font_family(MONO)
                        .text_size(px(12.0))
                        .text_color(th.text)
                        // The press that opens a menu must not also reach the
                        // text underneath, and the one that picks an item
                        // must not be read as "dismiss".
                        .on_any_mouse_down(|_, _, cx| cx.stop_propagation())
                        .child(
                            div()
                                .id("file-goto-type")
                                .px_3()
                                .py(px(6.0))
                                .cursor_pointer()
                                .hover(|el| el.bg(th.hover_bg))
                                .on_click(cx.listener({
                                    let name = name.clone();
                                    move |this, _, _, cx| {
                                        this.menu = None;
                                        cx.emit(FileEvent::GoToType(name.to_string()));
                                        cx.notify();
                                    }
                                }))
                                .child(SharedString::from(format!("Go to {name}"))),
                        ),
                )
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn find_all_reports_ranges_into_the_original_text() {
        let hay = "type Post { post: Post }";
        assert_eq!(find_all(hay, "post"), vec![(5, 9), (12, 16), (18, 22)]);
        for (s, e) in find_all(hay, "post") {
            assert_eq!(hay[s..e].to_lowercase(), "post");
        }
        assert!(find_all(hay, "").is_empty());
        assert!(find_all("", "post").is_empty());
    }

    #[test]
    fn find_all_never_cuts_a_character_in_half() {
        // The lowercase of `İ` is longer than `İ` itself, so a search over a
        // lowercased copy would hand back offsets that land mid-character in
        // this string. Every range here has to be sliceable as it stands.
        let hay = "İstanbul 한글 type User { İd: ID }";
        for needle in ["d", "한", "type", "İ", "user"] {
            for (s, e) in find_all(hay, needle) {
                assert!(hay.is_char_boundary(s) && hay.is_char_boundary(e), "{needle}");
                let _ = &hay[s..e];
            }
        }
    }
}

#[cfg(test)]
mod line_tests {
    use super::*;

    #[test]
    fn line_start_counts_from_one() {
        let t = "a\nbb\n\nccc";
        assert_eq!(line_start(t, 1), 0);
        assert_eq!(line_start(t, 2), 2);
        assert_eq!(line_start(t, 3), 5, "the empty line still occupies one");
        assert_eq!(line_start(t, 4), 6);
        // Off the end clamps rather than panicking, and line 0 is not a line.
        assert_eq!(line_start(t, 99), t.len());
        assert_eq!(line_start(t, 0), 0);
        assert_eq!(line_start("", 3), 0);
    }

    #[test]
    fn line_start_lands_on_a_char_boundary() {
        let t = "한글\n타입 User\n{}";
        for line in 1..=4 {
            let at = line_start(t, line);
            assert!(t.is_char_boundary(at), "line {line} -> {at}");
        }
        assert_eq!(line_start(t, 2), "한글\n".len());
    }
}

#[cfg(test)]
mod word_tests {
    use super::*;

    #[test]
    fn word_at_reads_the_identifier_under_the_offset() {
        let t = "  author: User!";
        assert_eq!(word_at(t, 2).as_deref(), Some("author"));
        assert_eq!(word_at(t, 5).as_deref(), Some("author"), "from inside it");
        assert_eq!(word_at(t, 8).as_deref(), Some("author"), "the trailing edge");
        assert_eq!(word_at(t, 10).as_deref(), Some("User"));
        assert_eq!(word_at(t, 14).as_deref(), Some("User"), "the ! is not a name");
        assert_eq!(word_at(t, 0), None, "a space is not a name");
        assert_eq!(word_at(t, 9), None, "the colon is not a name");
    }

    #[test]
    fn word_at_holds_on_multibyte_text() {
        let t = "\"\"\"한글 설명\"\"\" type_이름: Post";
        for i in 0..=t.len() {
            // Every offset answers something or nothing, and never panics.
            let _ = word_at(t, i);
        }
        let at = t.find("Post").unwrap();
        assert_eq!(word_at(t, at + 1).as_deref(), Some("Post"));
    }
}
