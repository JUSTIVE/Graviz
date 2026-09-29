//! The single-line text field, shared by the sidebar's search box and the
//! ⌘K palette.
//!
//! [`crate::textedit`] holds the editing rules as pure types, tested without
//! a window. What is left over is the part that needs GPUI: turning a key
//! into one of those edits (with the clipboard chords), and drawing the
//! caret and selection over monospaced text. Both surfaces need exactly that
//! and nothing else, so it lives here rather than twice.

use crate::model::mono_w;
use crate::textedit::TextEdit;
use crate::theme::Theme;
use gpui::{div, prelude::*, px, ClipboardItem, KeyDownEvent, MouseButton, SharedString};
use std::cell::Cell;
use std::rc::Rc;

pub const MONO: &str = "Menlo";

/// What a key meant, once the field has taken the part that belongs to it.
///
/// A single-line field has no use for up, down or enter, and escape means
/// something different on every surface, so those come back to the caller.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldKey {
    /// The text changed, so whatever it drives has to be recomputed.
    Edited,
    /// Only the caret or the selection moved.
    Moved,
    Up,
    Down,
    Enter,
    Escape,
    /// Nothing the field reacts to.
    Ignored,
}

/// Apply `ev` to `edit`, returning what the caller still has to do about it.
///
/// ⌘-chords come first: on this platform they are the field's, not the
/// list's. ⌘←/⌘→ are line start and end, the way they are in every other
/// single-line field here.
pub fn key<T: 'static>(
    edit: &mut TextEdit,
    ev: &KeyDownEvent,
    cx: &mut gpui::Context<T>,
) -> FieldKey {
    let ks = &ev.keystroke;
    let shift = ks.modifiers.shift;
    if ks.modifiers.platform {
        return match ks.key.as_str() {
            "a" => {
                edit.select_all();
                FieldKey::Moved
            }
            "c" => {
                if let Some(sel) = edit.selected_text() {
                    cx.write_to_clipboard(ClipboardItem::new_string(sel.to_string()));
                }
                FieldKey::Ignored
            }
            "x" => {
                let Some(sel) = edit.selected_text().map(str::to_string) else {
                    return FieldKey::Ignored;
                };
                cx.write_to_clipboard(ClipboardItem::new_string(sel));
                edit.delete_selection();
                FieldKey::Edited
            }
            "v" => {
                let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) else {
                    return FieldKey::Ignored;
                };
                edit.insert(&text);
                FieldKey::Edited
            }
            "left" => {
                edit.move_cursor(0, shift);
                FieldKey::Moved
            }
            "right" => {
                edit.move_cursor(edit.text.len(), shift);
                FieldKey::Moved
            }
            _ => FieldKey::Ignored,
        };
    }
    if ks.modifiers.control {
        return FieldKey::Ignored;
    }
    match ks.key.as_str() {
        "backspace" => {
            edit.backspace();
            FieldKey::Edited
        }
        "delete" => {
            edit.delete_forward();
            FieldKey::Edited
        }
        "left" => {
            let to = edit.prev_boundary(edit.cursor);
            edit.move_cursor(to, shift);
            FieldKey::Moved
        }
        "right" => {
            let to = edit.next_boundary(edit.cursor);
            edit.move_cursor(to, shift);
            FieldKey::Moved
        }
        "home" => {
            edit.move_cursor(0, shift);
            FieldKey::Moved
        }
        "end" => {
            edit.move_cursor(edit.text.len(), shift);
            FieldKey::Moved
        }
        "escape" => FieldKey::Escape,
        "up" => FieldKey::Up,
        "down" => FieldKey::Down,
        "enter" => FieldKey::Enter,
        _ => match ks.key_char.as_deref() {
            Some(ch) if !ch.chars().any(|c| c.is_control()) => {
                edit.insert(ch);
                FieldKey::Edited
            }
            _ => FieldKey::Ignored,
        },
    }
}

/// Byte offset whose caret position sits closest to `x`, measured in pixels
/// from the start of the text. Only char boundaries are candidates, so the
/// caret can never land inside a multi-byte glyph.
pub fn offset_for_x(text: &str, x: f32, font_px: f32) -> usize {
    text.char_indices()
        .map(|(i, _)| i)
        .chain(std::iter::once(text.len()))
        .min_by(|&a, &b| {
            let da = (mono_w(&text[..a], font_px) - x).abs();
            let db = (mono_w(&text[..b], font_px) - x).abs();
            da.total_cmp(&db)
        })
        .unwrap_or(0)
}

pub struct InputProps<'a> {
    pub th: Theme,
    pub edit: &'a TextEdit,
    pub placeholder: &'static str,
    pub focused: bool,
    pub font_px: f32,
    pub line_h: f32,
    /// Where the text starts on screen, recorded while painting so a click
    /// can be turned back into a caret offset.
    pub origin: Rc<Cell<f32>>,
}

/// The text itself, with its caret and selection band.
///
/// `on_click` receives the byte offset the press landed nearest, and is
/// expected to move the owner's caret there.
pub fn input<T: 'static>(
    props: InputProps,
    cx: &mut gpui::Context<T>,
    on_click: impl Fn(&mut T, usize, &mut gpui::Context<T>) + 'static,
) -> impl IntoElement {
    let InputProps { th, edit, placeholder, focused, font_px, line_h, origin } = props;
    let query = edit.text.clone();
    let empty = query.is_empty();
    let text: SharedString =
        if empty { placeholder.into() } else { query.clone().into() };
    // The caret and the selection band are placed by measuring the text to
    // their left, which is exact because the box is monospaced.
    let caret_x = mono_w(&query[..edit.cursor], font_px);
    let selection = edit
        .selection()
        .map(|(s, e)| (mono_w(&query[..s], font_px), mono_w(&query[s..e], font_px)));
    let measure = origin.clone();
    let hit = origin;
    let hit_text = query.clone();
    div()
        .id("field-text")
        .flex_1()
        .min_w_0()
        .relative()
        .h(px(line_h))
        .flex()
        .items_center()
        .font_family(MONO)
        .text_size(px(font_px))
        .whitespace_nowrap()
        .overflow_hidden()
        .text_color(if empty { th.text_muted } else { th.text })
        .child(
            gpui::canvas(
                |_, _, _| (),
                move |bounds, _, _, _| measure.set(f32::from(bounds.origin.x)),
            )
            .absolute()
            .size_full(),
        )
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |this, ev: &gpui::MouseDownEvent, _, cx| {
                let x = f32::from(ev.position.x) - hit.get();
                on_click(this, offset_for_x(&hit_text, x, font_px), cx);
            }),
        )
        // Painted before the glyphs so it sits behind them.
        .when_some(selection, |el, (x, w)| {
            el.child(
                div()
                    .absolute()
                    .left(px(x))
                    .top_0()
                    .bottom_0()
                    .w(px(w))
                    .rounded(px(2.0))
                    .bg(th.accent.opacity(0.35)),
            )
        })
        .child(text)
        // No caret while a selection is up, matching the platform's own
        // fields.
        .when(focused && selection.is_none(), |el| {
            el.child(
                div()
                    .absolute()
                    .left(px(caret_x))
                    .top(px(2.0))
                    .bottom(px(2.0))
                    .w(px(1.5))
                    .bg(th.accent),
            )
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The size the sidebar's box uses; the palette's is larger, and the
    /// rule is the same either way.
    const FONT: f32 = 12.0;

    fn offset_for_x2(text: &str, x: f32) -> usize {
        offset_for_x(text, x, FONT)
    }

    /// Clicking maps an x offset back to a caret position, snapping to the
    /// nearer boundary so the caret lands where the pointer looks.
    #[test]
    fn click_maps_x_to_the_nearest_caret_offset() {
        let w = |s: &str| mono_w(s, FONT);
        assert_eq!(offset_for_x2("user", 0.0), 0);
        assert_eq!(offset_for_x2("user", w("user")), 4, "past the end clamps to the end");
        assert_eq!(offset_for_x2("user", w("user") + 999.0), 4);
        assert_eq!(offset_for_x2("user", w("us")), 2);
        // Just past a glyph's midpoint rounds on to the next boundary.
        assert_eq!(offset_for_x2("user", w("us") + w("e") * 0.6), 3);
        assert_eq!(offset_for_x2("", 42.0), 0, "empty text has only offset 0");
    }

    /// Byte offsets again: a click inside a multi-byte glyph has to resolve
    /// to one of its edges, never into the middle.
    #[test]
    fn click_never_lands_inside_a_multibyte_glyph() {
        let text = "한글";
        for step in 0..40 {
            let off = offset_for_x2(text, step as f32 * 2.0);
            assert!(text.is_char_boundary(off), "offset {off} splits a glyph");
        }
    }

}
