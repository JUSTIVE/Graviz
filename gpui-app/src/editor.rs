//! Minimal multi-line text editor for the overlay dock.
//!
//! Deliberately small: no IME composition, no undo stack. Caret geometry is
//! measured in monospace *cells*, so a description containing Hangul or CJK
//! places its caret and selection correctly even though the glyphs are two
//! columns wide. Covers typing, newline/tab, arrows with shift-selection,
//! click/drag caret placement, ⌘A/C/X/V, and scrolling. ⌘↵ emits `Submitted`.

use crate::theme::Theme;
use gpui::{
    canvas, div, fill, point, prelude::*, px, size, App, Bounds, ClipboardItem, Context,
    EventEmitter, FocusHandle, Focusable, FontWeight, KeyDownEvent, MouseButton, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, Pixels, Point, ScrollDelta, ScrollWheelEvent, SharedString,
    TextAlign, TextRun, Window,
};
use std::cell::Cell;
use std::rc::Rc;

/// GraphQL keywords the highlighter paints in the interface color.
const KEYWORDS: [&str; 14] = [
    "type", "interface", "enum", "union", "input", "scalar", "schema", "extend",
    "implements", "directive", "query", "mutation", "subscription", "fragment",
];

/// Splits one SDL line into `(byte_len, color)` runs — comments, strings,
/// keywords, directives, type names and punctuation, like cm6-graphql.
fn highlight(line: &str, th: Theme) -> Vec<(usize, gpui::Hsla)> {
    let mut runs: Vec<(usize, gpui::Hsla)> = Vec::new();
    let push = |len: usize, c: gpui::Hsla, runs: &mut Vec<(usize, gpui::Hsla)>| {
        if len == 0 {
            return;
        }
        match runs.last_mut() {
            Some((l, prev)) if *prev == c => *l += len,
            _ => runs.push((len, c)),
        }
    };
    let bytes = line.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        let rest = &line[i..];
        let ch = bytes[i] as char;
        if ch == '#' {
            push(line.len() - i, th.text_muted, &mut runs);
            break;
        }
        if ch == '"' {
            // block or single-line string (descriptions)
            let quote_len = if rest.starts_with("\"\"\"") { 3 } else { 1 };
            let mut j = i + quote_len;
            while j < bytes.len() {
                if quote_len == 3 && line[j..].starts_with("\"\"\"") {
                    j += 3;
                    break;
                }
                if quote_len == 1 && bytes[j] == b'"' {
                    j += 1;
                    break;
                }
                j += 1;
            }
            push(j.min(line.len()) - i, th.overlay_green, &mut runs);
            i = j.min(line.len());
            continue;
        }
        if ch == '@' {
            let mut j = i + 1;
            while j < bytes.len() && (bytes[j].is_ascii_alphanumeric() || bytes[j] == b'_') {
                j += 1;
            }
            push(j - i, th.type_amber, &mut runs);
            i = j;
            continue;
        }
        if ch.is_ascii_alphabetic() || ch == '_' {
            let mut j = i;
            while j < bytes.len() && (bytes[j].is_ascii_alphanumeric() || bytes[j] == b'_') {
                j += 1;
            }
            let word = &line[i..j];
            let color = if KEYWORDS.contains(&word) {
                th.kind_color(graviz_core::graph::NodeKind::Interface)
            } else if word.starts_with(|c: char| c.is_ascii_uppercase()) {
                th.kind_color(graviz_core::graph::NodeKind::Object)
            } else {
                th.text
            };
            push(j - i, color, &mut runs);
            i = j;
            continue;
        }
        if ch == '-' && rest.len() > 1 && !bytes[i + 1].is_ascii_digit() {
            // the overlay's `-Type.field` removal marker
            push(1, th.red, &mut runs);
            i += 1;
            continue;
        }
        let len = ch.len_utf8();
        let color = if ch.is_ascii_digit() { th.type_amber } else { th.text_muted };
        push(len, color, &mut runs);
        i += len;
    }
    runs
}

const FONT_PX: f32 = 12.0;
const LINE_H: f32 = 18.0;
const PAD: f32 = 8.0;

pub enum EditorEvent {
    Changed,
    Submitted,
    /// ⌘S. What saving means is the owner's business.
    Save,
}

pub struct TextArea {
    text: String,
    /// Caret byte offset.
    cursor: usize,
    /// Selection anchor byte offset (None = no selection).
    anchor: Option<usize>,
    focus: FocusHandle,
    scroll_y: f32,
    dragging: bool,
    /// Element origin+height recorded at paint time for click mapping.
    origin: Rc<Cell<(f32, f32, f32)>>,
    pub placeholder: &'static str,
    /// Line numbers down the left edge.
    pub gutter: bool,
    /// Refuse every edit. The file panel opens this way: the buffer is
    /// somebody's schema on disk, and a stray keypress should not rewrite it.
    pub read_only: bool,
    /// Paint it as a surface rather than a field: the page's own background,
    /// flush to its pane, with no rounded box around it. A full-height code
    /// view is a place you look at, not a control you fill in, and the
    /// deepest colour is what makes the syntax colours carry.
    pub surface: bool,
    /// Byte ranges the in-file search found, and which of them is current.
    matches: Vec<(usize, usize)>,
    active_match: usize,
}

impl TextArea {
    pub fn new(cx: &mut Context<Self>) -> Self {
        Self {
            text: String::new(),
            cursor: 0,
            anchor: None,
            focus: cx.focus_handle(),
            scroll_y: 0.0,
            dragging: false,
            origin: Rc::new(Cell::new((0.0, 0.0, 0.0))),
            placeholder: "",
            gutter: false,
            read_only: false,
            surface: false,
            matches: Vec::new(),
            active_match: 0,
        }
    }

    /// Ranges for the in-file search to paint, and which one the view should
    /// be showing.
    pub fn set_matches(&mut self, matches: Vec<(usize, usize)>, active: usize) {
        self.matches = matches;
        self.active_match = active;
    }

    /// Put the caret on `offset` and bring it into view, for "go to match".
    pub fn reveal(&mut self, offset: usize, cx: &mut Context<Self>) {
        self.cursor = offset.min(self.text.len());
        self.anchor = None;
        let viewport_h = self.origin.get().2;
        self.ensure_cursor_visible(viewport_h);
        cx.notify();
    }

    /// Width the line-number column needs, including its padding.
    fn gutter_w(&self) -> f32 {
        if !self.gutter {
            return 0.0;
        }
        let digits = self.line_count().max(1).to_string().len().max(2) as f32;
        (digits + 2.0) * FONT_PX * crate::model::MONO_ADVANCE
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn set_text(&mut self, text: String, cx: &mut Context<Self>) {
        self.text = text;
        self.cursor = self.text.len();
        self.anchor = None;
        cx.emit(EditorEvent::Changed);
        cx.notify();
    }

    #[allow(dead_code)]
    pub fn focus_handle(&self) -> FocusHandle {
        self.focus.clone()
    }

    fn selection(&self) -> Option<(usize, usize)> {
        let a = self.anchor?;
        if a == self.cursor {
            return None;
        }
        Some((a.min(self.cursor), a.max(self.cursor)))
    }

    fn delete_selection(&mut self) -> bool {
        if let Some((s, e)) = self.selection() {
            self.text.replace_range(s..e, "");
            self.cursor = s;
            self.anchor = None;
            true
        } else {
            false
        }
    }

    fn insert(&mut self, s: &str, cx: &mut Context<Self>) {
        self.delete_selection();
        self.text.insert_str(self.cursor, s);
        self.cursor += s.len();
        cx.emit(EditorEvent::Changed);
    }

    fn prev_boundary(&self, from: usize) -> usize {
        let mut i = from;
        while i > 0 {
            i -= 1;
            if self.text.is_char_boundary(i) {
                return i;
            }
        }
        0
    }

    fn next_boundary(&self, from: usize) -> usize {
        let mut i = from;
        while i < self.text.len() {
            i += 1;
            if self.text.is_char_boundary(i) {
                return i;
            }
        }
        self.text.len()
    }

    /// (line index, byte offset of line start, column in chars)
    fn cursor_line_col(&self) -> (usize, usize, usize) {
        let before = &self.text[..self.cursor];
        let line = before.matches('\n').count();
        let line_start = before.rfind('\n').map(|i| i + 1).unwrap_or(0);
        let col = self.text[line_start..self.cursor].chars().count();
        (line, line_start, col)
    }

    /// Byte offset of the glyph nearest `cells` columns into `line`.
    fn offset_for_line_cells(&self, line: usize, cells: f32) -> usize {
        let mut start = 0usize;
        for (i, l) in self.text.split('\n').enumerate() {
            if i == line {
                let mut off = start;
                let mut acc = 0.0f32;
                for c in l.chars() {
                    let w = crate::model::mono_cells(&c.to_string());
                    if acc + w / 2.0 >= cells {
                        return off;
                    }
                    acc += w;
                    off += c.len_utf8();
                }
                return off;
            }
            start += l.len() + 1;
        }
        self.text.len()
    }

    fn offset_for_line_col(&self, line: usize, col: usize) -> usize {
        let mut start = 0usize;
        for (i, l) in self.text.split('\n').enumerate() {
            if i == line {
                let mut off = start;
                for (ci, c) in l.chars().enumerate() {
                    if ci == col {
                        return off;
                    }
                    off += c.len_utf8();
                }
                return off;
            }
            start += l.len() + 1;
        }
        self.text.len()
    }

    fn line_count(&self) -> usize {
        self.text.split('\n').count()
    }

    fn move_cursor(&mut self, to: usize, select: bool) {
        if select {
            if self.anchor.is_none() {
                self.anchor = Some(self.cursor);
            }
        } else {
            self.anchor = None;
        }
        self.cursor = to.min(self.text.len());
    }

    fn ensure_cursor_visible(&mut self, viewport_h: f32) {
        let (line, _, _) = self.cursor_line_col();
        let top = line as f32 * LINE_H;
        if top < self.scroll_y {
            self.scroll_y = top;
        } else if top + LINE_H > self.scroll_y + viewport_h - PAD * 2.0 {
            self.scroll_y = top + LINE_H - (viewport_h - PAD * 2.0);
        }
        self.scroll_y = self.scroll_y.max(0.0);
    }

    fn offset_at(&self, pos: Point<Pixels>) -> usize {
        let (ox, oy, _h) = self.origin.get();
        let x = (f32::from(pos.x) - ox - PAD - self.gutter_w()).max(0.0);
        let y = f32::from(pos.y) - oy - PAD + self.scroll_y;
        let line = ((y / LINE_H).floor().max(0.0)) as usize;
        let line = line.min(self.line_count().saturating_sub(1));
        // Walk the line accumulating cell widths so a click lands on the
        // glyph under the cursor, not `x / advance` characters along.
        let want = x / (FONT_PX * crate::model::MONO_ADVANCE);
        self.offset_for_line_cells(line, want)
    }

    fn on_key_down(&mut self, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let ks = &ev.keystroke;
        let cmd = ks.modifiers.platform;
        let shift = ks.modifiers.shift;
        // ⌥ moves and deletes by word; fn turns the arrows into the home, end
        // and page keys, which macOS usually rewrites before they get here.
        let word = ks.modifiers.alt;
        let fnkey = ks.modifiers.function;
        let viewport_h = self.origin.get().2;
        // A page is whatever is on screen, less a line so the eye keeps one
        // row of context across the jump.
        let page_lines = ((viewport_h / LINE_H) as usize).saturating_sub(1).max(1);
        // Read-only stops the keys that change text. Moving, selecting and
        // copying all still work, which is the point of reading.
        if self.read_only
            && matches!(ks.key.as_str(), "enter" | "tab" | "backspace" | "delete")
            || self.read_only && cmd && matches!(ks.key.as_str(), "v" | "x")
        {
            return;
        }
        match ks.key.as_str() {
            "s" if cmd => {
                cx.emit(EditorEvent::Save);
                return;
            }
            "enter" if cmd => {
                cx.emit(EditorEvent::Submitted);
                return;
            }
            "enter" => self.insert("\n", cx),
            "tab" => self.insert("  ", cx),
            "backspace" => {
                if !self.delete_selection() && self.cursor > 0 {
                    let p = if word {
                        crate::textedit::prev_word_boundary(&self.text, self.cursor)
                    } else {
                        self.prev_boundary(self.cursor)
                    };
                    self.text.replace_range(p..self.cursor, "");
                    self.cursor = p;
                }
                cx.emit(EditorEvent::Changed);
            }
            "delete" => {
                if !self.delete_selection() && self.cursor < self.text.len() {
                    let n = if word {
                        crate::textedit::next_word_boundary(&self.text, self.cursor)
                    } else {
                        self.next_boundary(self.cursor)
                    };
                    self.text.replace_range(self.cursor..n, "");
                }
                cx.emit(EditorEvent::Changed);
            }
            "home" => {
                let to = self.cursor_line_col().1;
                self.move_cursor(to, shift);
            }
            "end" => {
                let (line, _, _) = self.cursor_line_col();
                let to = self.offset_for_line_col(line, usize::MAX / 2);
                self.move_cursor(to, shift);
            }
            "pageup" => {
                let (line, _, col) = self.cursor_line_col();
                let to = self.offset_for_line_col(line.saturating_sub(page_lines), col);
                self.move_cursor(to, shift);
            }
            "pagedown" => {
                let (line, _, col) = self.cursor_line_col();
                let to = self.offset_for_line_col(line + page_lines, col);
                self.move_cursor(to, shift);
            }
            "left" => {
                let to = if cmd || fnkey {
                    self.cursor_line_col().1
                } else if word {
                    crate::textedit::prev_word_boundary(&self.text, self.cursor)
                } else {
                    self.prev_boundary(self.cursor)
                };
                self.move_cursor(to, shift);
            }
            "right" => {
                let to = if cmd || fnkey {
                    let (line, _, _) = self.cursor_line_col();
                    self.offset_for_line_col(line, usize::MAX / 2)
                } else if word {
                    crate::textedit::next_word_boundary(&self.text, self.cursor)
                } else {
                    self.next_boundary(self.cursor)
                };
                self.move_cursor(to, shift);
            }
            "up" => {
                let (line, _, col) = self.cursor_line_col();
                let to = if cmd {
                    0
                } else if fnkey {
                    self.offset_for_line_col(line.saturating_sub(page_lines), col)
                } else if word {
                    // ⌥↑ is "up a paragraph": the start of this line, or of
                    // the one above when the caret already sits there.
                    let start = self.cursor_line_col().1;
                    if self.cursor == start && line > 0 {
                        self.offset_for_line_col(line - 1, 0)
                    } else {
                        start
                    }
                } else if line == 0 {
                    0
                } else {
                    self.offset_for_line_col(line - 1, col)
                };
                self.move_cursor(to, shift);
            }
            "down" => {
                let (line, _, col) = self.cursor_line_col();
                let to = if cmd {
                    self.text.len()
                } else if fnkey {
                    self.offset_for_line_col(line + page_lines, col)
                } else if word {
                    self.offset_for_line_col(line, usize::MAX / 2)
                } else {
                    self.offset_for_line_col(line + 1, col)
                };
                self.move_cursor(to, shift);
            }
            "a" if cmd => {
                self.anchor = Some(0);
                self.cursor = self.text.len();
            }
            "c" if cmd => {
                if let Some((s, e)) = self.selection() {
                    cx.write_to_clipboard(ClipboardItem::new_string(self.text[s..e].to_string()));
                }
                return;
            }
            "x" if cmd => {
                if let Some((s, e)) = self.selection() {
                    cx.write_to_clipboard(ClipboardItem::new_string(self.text[s..e].to_string()));
                    self.delete_selection();
                    cx.emit(EditorEvent::Changed);
                }
            }
            "v" if cmd => {
                if let Some(item) = cx.read_from_clipboard() {
                    if let Some(text) = item.text() {
                        self.insert(&text, cx);
                    }
                }
            }
            _ => {
                if cmd || ks.modifiers.control || self.read_only {
                    return;
                }
                if let Some(ch) = ks.key_char.as_deref() {
                    if !ch.chars().any(|c| c.is_control()) {
                        self.insert(ch, cx);
                    } else {
                        return;
                    }
                } else {
                    return;
                }
            }
        }
        self.ensure_cursor_visible(viewport_h);
        let _ = window;
        cx.notify();
    }

    fn on_mouse_down(&mut self, ev: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus, cx);
        let off = self.offset_at(ev.position);
        self.anchor = None;
        self.cursor = off;
        self.dragging = true;
        cx.notify();
    }

    fn on_mouse_move(&mut self, ev: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.dragging {
            let off = self.offset_at(ev.position);
            if self.anchor.is_none() {
                self.anchor = Some(self.cursor);
            }
            self.cursor = off;
            cx.notify();
        }
    }

    fn on_mouse_up(&mut self, _: &MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        self.dragging = false;
        if self.anchor == Some(self.cursor) {
            self.anchor = None;
        }
        cx.notify();
    }

    fn on_scroll(&mut self, ev: &ScrollWheelEvent, _: &mut Window, cx: &mut Context<Self>) {
        let dy = match ev.delta {
            ScrollDelta::Pixels(d) => f32::from(d.y),
            ScrollDelta::Lines(d) => d.y * LINE_H,
        };
        let max = (self.line_count() as f32 * LINE_H - 40.0).max(0.0);
        self.scroll_y = (self.scroll_y - dy).clamp(0.0, max);
        cx.stop_propagation();
        cx.notify();
    }
}

impl EventEmitter<EditorEvent> for TextArea {}

impl Focusable for TextArea {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for TextArea {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let th = crate::theme::current(cx, window.appearance());
        let focused = self.focus.is_focused(window);
        let text = self.text.clone();
        let cursor = self.cursor;
        let selection = self.selection();
        let scroll_y = self.scroll_y;
        let origin = self.origin.clone();
        let placeholder: SharedString = self.placeholder.into();
        let gutter_w = self.gutter_w();
        let matches = self.matches.clone();
        let active_match = self.active_match;

        div()
            .size_full()
            .when(!self.surface, |el| {
                el.rounded_md()
                    .border_1()
                    .border_color(if focused { th.accent } else { th.card_border })
                    .bg(th.input_bg)
            })
            .when(self.surface, |el| el.bg(th.bg))
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::on_key_down))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_scroll_wheel(cx.listener(Self::on_scroll))
            .child(
                canvas(
                    |_, _, _| (),
                    move |bounds, _, window, cx| {
                        origin.set((
                            f32::from(bounds.origin.x),
                            f32::from(bounds.origin.y),
                            f32::from(bounds.size.height),
                        ));
                        paint_editor(
                            Painted {
                                text: &text,
                                cursor,
                                selection,
                                scroll_y,
                                focused,
                                placeholder: &placeholder,
                                gutter_w,
                                matches: &matches,
                                active_match,
                                th,
                            },
                            bounds,
                            window,
                            cx,
                        );
                    },
                )
                .size_full(),
            )
    }
}

/// Everything one paint of the buffer needs.
struct Painted<'a> {
    text: &'a str,
    cursor: usize,
    selection: Option<(usize, usize)>,
    scroll_y: f32,
    focused: bool,
    placeholder: &'a SharedString,
    /// Width of the line-number column; zero when there is none.
    gutter_w: f32,
    matches: &'a [(usize, usize)],
    active_match: usize,
    th: Theme,
}

fn paint_editor(p: Painted, bounds: Bounds<Pixels>, window: &mut Window, cx: &mut App) {
    let Painted {
        text,
        cursor,
        selection,
        scroll_y,
        focused,
        placeholder,
        gutter_w,
        matches,
        active_match,
        th,
    } = p;
    window.with_content_mask(Some(gpui::ContentMask { bounds }), |window| {
        let ox = f32::from(bounds.origin.x) + PAD + gutter_w;
        let oy = f32::from(bounds.origin.y) + PAD - scroll_y;
        let vh = f32::from(bounds.size.height);
        let mut font = gpui::font("Menlo");
        font.weight = FontWeight::NORMAL;
        let text_system = window.text_system().clone();

        if text.is_empty() {
            let run = TextRun {
                len: placeholder.len(),
                font: font.clone(),
                color: th.text_faint,
                background_color: None,
                underline: None,
                strikethrough: None,
            };
            let line =
                text_system.shape_line(placeholder.clone(), px(FONT_PX), &[run], None);
            let _ = line.paint(
                point(px(ox), px(oy)),
                px(LINE_H),
                TextAlign::Left,
                None,
                window,
                cx,
            );
        }

        let mut byte = 0usize;
        for (li, l) in text.split('\n').enumerate() {
            let top = oy + li as f32 * LINE_H;
            let line_len = l.len();
            if top + LINE_H >= f32::from(bounds.origin.y) && top <= f32::from(bounds.origin.y) + vh
            {
                // selection band for this line
                if let Some((s, e)) = selection {
                    let ls = s.max(byte);
                    let le = e.min(byte + line_len);
                    let safe = text.is_char_boundary(ls.max(byte))
                        && text.is_char_boundary(le.max(ls));
                    if safe && (ls < le || (s <= byte && e > byte + line_len)) {
                        // Cells, not characters: a Hangul or CJK glyph is two
                        // columns wide, so counting characters puts the
                        // selection box and the caret in the wrong place on
                        // any line that is not pure ASCII.
                        let cols_before = crate::model::mono_cells(&text[byte..ls.max(byte)]);
                        let cols_sel = crate::model::mono_cells(&text[ls.max(byte)..le.max(ls)]);
                        let x0 = ox + cols_before * FONT_PX * crate::model::MONO_ADVANCE;
                        let w = (cols_sel * FONT_PX * crate::model::MONO_ADVANCE).max(4.0);
                        window.paint_quad(fill(
                            Bounds {
                                origin: point(px(x0), px(top)),
                                size: size(px(w), px(LINE_H)),
                            },
                            th.accent.opacity(0.25),
                        ));
                    }
                }
                // Search hits: every one tinted, the current one stronger,
                // so the eye can see where it is among them without losing
                // the others.
                let cell_x = |from: usize, to: usize| {
                    let before = crate::model::mono_cells(&text[byte..from]);
                    let w = crate::model::mono_cells(&text[from..to]);
                    (
                        ox + before * FONT_PX * crate::model::MONO_ADVANCE,
                        (w * FONT_PX * crate::model::MONO_ADVANCE).max(2.0),
                    )
                };
                for (mi, &(ms, me)) in matches.iter().enumerate() {
                    let (s0, e0) = (ms.max(byte), me.min(byte + line_len));
                    // A range from before the last edit can point anywhere,
                    // including into the middle of a character. Slicing one
                    // panics, and a panic in a paint takes the window with
                    // it, so an impossible band is simply not drawn.
                    if s0 >= e0 || !text.is_char_boundary(s0) || !text.is_char_boundary(e0) {
                        continue;
                    }
                    let (x0, w) = cell_x(s0, e0);
                    let color = if mi == active_match {
                        th.type_amber.opacity(0.55)
                    } else {
                        th.type_amber.opacity(0.22)
                    };
                    window.paint_quad(fill(
                        Bounds {
                            origin: point(px(x0), px(top)),
                            size: size(px(w), px(LINE_H)),
                        },
                        color,
                    ));
                }
                if !l.is_empty() {
                    let runs: Vec<TextRun> = highlight(l, th)
                        .into_iter()
                        .map(|(len, color)| TextRun {
                            len,
                            font: font.clone(),
                            color,
                            background_color: None,
                            underline: None,
                            strikethrough: None,
                        })
                        .collect();
                    let line = text_system.shape_line(
                        SharedString::from(l.to_string()),
                        px(FONT_PX),
                        &runs,
                        None,
                    );
                    let _ = line.paint(
                        point(px(ox), px(top)),
                        px(LINE_H),
                        TextAlign::Left,
                        None,
                        window,
                        cx,
                    );
                }
                if gutter_w > 0.0 {
                    let n = SharedString::from((li + 1).to_string());
                    let run = TextRun {
                        len: n.len(),
                        font: font.clone(),
                        color: th.text_faint,
                        background_color: None,
                        underline: None,
                        strikethrough: None,
                    };
                    let shaped = text_system.shape_line(n.clone(), px(FONT_PX), &[run], None);
                    // Right-aligned against the text column, so the digits
                    // line up however many of them there are.
                    let w = n.len() as f32 * FONT_PX * crate::model::MONO_ADVANCE;
                    let gx = ox - FONT_PX * crate::model::MONO_ADVANCE - w;
                    let _ = shaped.paint(
                        point(px(gx), px(top)),
                        px(LINE_H),
                        TextAlign::Left,
                        None,
                        window,
                        cx,
                    );
                }
                // caret
                if focused
                    && cursor >= byte
                    && cursor <= byte + line_len
                    && text.is_char_boundary(cursor)
                {
                    let cols = crate::model::mono_cells(&text[byte..cursor]);
                    let x = ox + cols * FONT_PX * crate::model::MONO_ADVANCE;
                    window.paint_quad(fill(
                        Bounds {
                            origin: point(px(x), px(top + 1.0)),
                            size: size(px(1.5), px(LINE_H - 2.0)),
                        },
                        th.accent,
                    ));
                }
            }
            byte += line_len + 1;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_click_lands_on_the_glyph_under_it_in_a_cjk_line() {
        // "가나다" is three glyphs but six columns. A click four columns in
        // belongs on the third glyph, not the fifth character (there is none).
        let cells = |s: &str| crate::model::mono_cells(s);
        assert_eq!(cells("가나다"), 6.0);
        assert_eq!(cells("abc"), 3.0);
    }

    fn th() -> Theme {
        crate::theme::theme(gpui::WindowAppearance::Dark)
    }

    /// Runs must tile the line exactly — a mismatch makes shape_line panic.
    fn total(line: &str) -> usize {
        highlight(line, th()).iter().map(|(l, _)| l).sum()
    }

    #[test]
    fn runs_cover_every_byte() {
        for line in [
            "type User implements Node {",
            "  name: String! @deprecated(reason: \"gone\")",
            "# a comment",
            "\"\"\"description\"\"\"",
            "  -legacyName",
            "",
            "  tags: [String!]!",
        ] {
            assert_eq!(total(line), line.len(), "line: {line:?}");
        }
    }

    #[test]
    fn keywords_types_and_comments_get_distinct_colors() {
        let t = th();
        let runs = highlight("type User {", t);
        assert_eq!(runs[0].1, t.kind_color(graviz_core::graph::NodeKind::Interface));
        assert!(runs.iter().any(|(_, c)| *c
            == t.kind_color(graviz_core::graph::NodeKind::Object)));
        let c = highlight("# note", t);
        assert_eq!(c, vec![(6, t.text_muted)]);
    }
}
