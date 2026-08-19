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
                th.kind_color(gompass_core::graph::NodeKind::Interface)
            } else if word.starts_with(|c: char| c.is_ascii_uppercase()) {
                th.kind_color(gompass_core::graph::NodeKind::Object)
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
        }
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
        let x = (f32::from(pos.x) - ox - PAD).max(0.0);
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
        let viewport_h = self.origin.get().2;
        match ks.key.as_str() {
            "enter" if cmd => {
                cx.emit(EditorEvent::Submitted);
                return;
            }
            "enter" => self.insert("\n", cx),
            "tab" => self.insert("  ", cx),
            "backspace" => {
                if !self.delete_selection() && self.cursor > 0 {
                    let p = self.prev_boundary(self.cursor);
                    self.text.replace_range(p..self.cursor, "");
                    self.cursor = p;
                }
                cx.emit(EditorEvent::Changed);
            }
            "delete" => {
                if !self.delete_selection() && self.cursor < self.text.len() {
                    let n = self.next_boundary(self.cursor);
                    self.text.replace_range(self.cursor..n, "");
                }
                cx.emit(EditorEvent::Changed);
            }
            "left" => {
                let to = if cmd {
                    self.cursor_line_col().1
                } else {
                    self.prev_boundary(self.cursor)
                };
                self.move_cursor(to, shift);
            }
            "right" => {
                let to = if cmd {
                    let (line, _, _) = self.cursor_line_col();
                    self.offset_for_line_col(line, usize::MAX / 2)
                } else {
                    self.next_boundary(self.cursor)
                };
                self.move_cursor(to, shift);
            }
            "up" => {
                let to = if cmd {
                    0
                } else {
                    let (line, _, col) = self.cursor_line_col();
                    if line == 0 {
                        0
                    } else {
                        self.offset_for_line_col(line - 1, col)
                    }
                };
                self.move_cursor(to, shift);
            }
            "down" => {
                let to = if cmd {
                    self.text.len()
                } else {
                    let (line, _, col) = self.cursor_line_col();
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
                if cmd || ks.modifiers.control {
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

        div()
            .size_full()
            .rounded_md()
            .border_1()
            .border_color(if focused { th.accent } else { th.card_border })
            .bg(th.input_bg)
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
                            &text,
                            cursor,
                            selection,
                            scroll_y,
                            focused,
                            &placeholder,
                            th,
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

#[allow(clippy::too_many_arguments)]
fn paint_editor(
    text: &str,
    cursor: usize,
    selection: Option<(usize, usize)>,
    scroll_y: f32,
    focused: bool,
    placeholder: &SharedString,
    th: Theme,
    bounds: Bounds<Pixels>,
    window: &mut Window,
    cx: &mut App,
) {
    window.with_content_mask(Some(gpui::ContentMask { bounds }), |window| {
        let ox = f32::from(bounds.origin.x) + PAD;
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
                    if ls < le || (s <= byte && e > byte + line_len) {
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
                // caret
                if focused && cursor >= byte && cursor <= byte + line_len {
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
        assert_eq!(runs[0].1, t.kind_color(gompass_core::graph::NodeKind::Interface));
        assert!(runs.iter().any(|(_, c)| *c
            == t.kind_color(gompass_core::graph::NodeKind::Object)));
        let c = highlight("# note", t);
        assert_eq!(c, vec![(6, t.text_muted)]);
    }
}
