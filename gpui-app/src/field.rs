//! The single-line text field, shared by the sidebar's search box and the
//! ⌘K palette.
//!
//! [`crate::textedit`] holds the editing rules as pure types, tested without
//! a window. What is left over is the part that needs GPUI: turning a key
//! into one of those edits (with the clipboard chords), and drawing the
//! caret and selection over monospaced text. Both surfaces need exactly that
//! and nothing else, so it lives here rather than twice.

use crate::textedit::TextEdit;
use crate::theme::Theme;
use gpui::{div, prelude::*, px, ClipboardItem, KeyDownEvent, MouseButton, SharedString, Window};
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
    // ⌥ is the word modifier on this platform, and fn turns the arrows into
    // home/end/page keys. macOS usually rewrites fn+← into "home" before it
    // gets here, but not on every keyboard, so the modifier is honoured too.
    let word = ks.modifiers.alt;
    let whole_line = ks.modifiers.function;
    match ks.key.as_str() {
        "backspace" if word => {
            if !edit.delete_selection() {
                let to = edit.prev_word(edit.cursor);
                edit.move_cursor(to, true);
                edit.delete_selection();
            }
            FieldKey::Edited
        }
        "delete" if word => {
            if !edit.delete_selection() {
                let to = edit.next_word(edit.cursor);
                edit.move_cursor(to, true);
                edit.delete_selection();
            }
            FieldKey::Edited
        }
        "backspace" => {
            edit.backspace();
            FieldKey::Edited
        }
        "delete" => {
            edit.delete_forward();
            FieldKey::Edited
        }
        "left" => {
            let to = if whole_line {
                0
            } else if word {
                edit.prev_word(edit.cursor)
            } else {
                edit.prev_boundary(edit.cursor)
            };
            edit.move_cursor(to, shift);
            FieldKey::Moved
        }
        "right" => {
            let to = if whole_line {
                edit.text.len()
            } else if word {
                edit.next_word(edit.cursor)
            } else {
                edit.next_boundary(edit.cursor)
            };
            edit.move_cursor(to, shift);
            FieldKey::Moved
        }
        // One line means there is nowhere vertical to go: ⌥↑ and the page
        // keys land on the ends, the way a single-line field does anywhere
        // else on this platform.
        "home" | "pageup" => {
            edit.move_cursor(0, shift);
            FieldKey::Moved
        }
        "end" | "pagedown" => {
            edit.move_cursor(edit.text.len(), shift);
            FieldKey::Moved
        }
        "up" if word => {
            edit.move_cursor(0, shift);
            FieldKey::Moved
        }
        "down" if word => {
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
    window: &Window,
    cx: &mut gpui::Context<T>,
    on_click: impl Fn(&mut T, usize, &mut gpui::Context<T>) + 'static,
) -> impl IntoElement {
    let InputProps { th, edit, placeholder, focused, font_px, line_h, origin } = props;
    let query = edit.text.clone();
    let empty = query.is_empty();
    let text: SharedString =
        if empty { placeholder.into() } else { query.clone().into() };
    // The caret and the selection band are placed by asking the text system
    // where each byte landed. Measuring monospace cells was close enough
    // until the text stopped being monospace: Menlo has no Hangul, those
    // glyphs come from a fallback face with its own advance, and the caret
    // drifted further off with every syllable.
    let shaped = (!empty).then(|| {
        let run = gpui::TextRun {
            len: query.len(),
            font: gpui::font(MONO),
            color: th.text,
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        window.text_system().shape_line(
            SharedString::from(query.clone()),
            px(font_px),
            &[run],
            None,
        )
    });
    let x_at = |i: usize| -> f32 {
        shaped.as_ref().map(|s| f32::from(s.x_for_index(i.min(query.len())))).unwrap_or(0.0)
    };
    let caret_x = x_at(edit.cursor);
    let selection = edit.selection().map(|(s, e)| (x_at(s), x_at(e) - x_at(s)));
    let measure = origin.clone();
    let hit = origin;
    let hit_shaped = shaped.clone();
    let hit_len = query.len();
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
                // The same shaping the caret is drawn from, so a click and
                // the caret agree about which glyph was meant.
                let offset = hit_shaped
                    .as_ref()
                    .map(|s| s.index_for_x(px(x.max(0.0))).unwrap_or(hit_len))
                    .unwrap_or(0);
                on_click(this, offset, cx);
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
