//! The SDL editors (the landing's paste box and the overlay dock) with the
//! gpui app's syntax colours.
//!
//! A native text field draws its text in one colour, so the field's own
//! text is made transparent and the coloured runs are laid out beneath it
//! in the same face, size, padding and line height. Once the text could
//! scroll, the layer beneath would not follow it: the field then shows its
//! own text in the plain colour instead.

use graviz_core::graph::NodeKind;
use nsh_core::prelude::*;

use crate::theme::{Color, Theme};
use crate::ui::{c, th, MONO};

/// GraphQL keywords the highlighter paints in the interface colour.
const KEYWORDS: [&str; 14] = [
    "type", "interface", "enum", "union", "input", "scalar", "schema", "extend", "implements", "directive", "query",
    "mutation", "subscription", "fragment",
];

const FONT_PX: f32 = 12.0;
const LINE_H: f32 = 18.0;

/// Splits one SDL line into `(byte_len, colour)` runs: comments, strings,
/// keywords, directives, type names and punctuation, like cm6-graphql
/// (ported from the gpui editor).
pub fn highlight(line: &str, th: &Theme) -> Vec<(usize, Color)> {
    let mut runs: Vec<(usize, Color)> = Vec::new();
    let push = |len: usize, c: Color, runs: &mut Vec<(usize, Color)>| {
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
        let ch = rest.chars().next().unwrap_or(' ');
        if ch == '#' {
            push(line.len() - i, th.text_muted, &mut runs);
            break;
        }
        if ch == '"' {
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
            let j = j.min(line.len());
            let j = (j..=line.len()).find(|&k| line.is_char_boundary(k)).unwrap_or(line.len());
            push(j - i, th.overlay_green, &mut runs);
            i = j;
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
                th.kind_color(NodeKind::Interface)
            } else if word.starts_with(|c: char| c.is_ascii_uppercase()) {
                th.kind_color(NodeKind::Object)
            } else {
                th.text
            };
            push(j - i, color, &mut runs);
            i = j;
            continue;
        }
        if ch == '-' && rest.len() > 1 && !bytes[i + 1].is_ascii_digit() {
            // the overlay's `- Type.field` removal marker
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

/// The coloured copy of `src` that sits under the field.
fn painted(src: &str) -> Text {
    let t = th();
    let mut out = text("");
    for (li, line) in src.split('\n').enumerate() {
        if li > 0 {
            out = out.child(text("\n"));
        }
        let mut at = 0;
        for (len, color) in highlight(line, &t) {
            let run = line[at..at + len].to_string();
            out = out.child(text(run).text_color(color.hsla()));
            at += len;
        }
    }
    out
}

/// A multi-line SDL field: `value` in, `on_change` out, `pad` inside the
/// box. `rows` is how many lines fit before the text could scroll.
pub fn editor(
    value: impl Fn() -> String + Copy + 'static,
    on_change: impl Fn(&str) + 'static,
    placeholder: &'static str,
    pad: f32,
) -> View {
    let height = signal(0.0f32);
    // The layer only holds while every line is on screen.
    let fits = move || {
        let lines = value().split('\n').count() as f32;
        let h = height.get();
        h <= 0.0 || lines * LINE_H + 2.0 * pad <= h
    };
    view()
        .flex_1()
        .min_h(px(0.))
        .on_layout(move |b: &Bounds<Points>| height.set(b.size.height.0))
        .child(
            view()
                .absolute()
                .top(px(pad))
                .left(px(pad + LEFT_INSET))
                .right(px(pad))
                .pointer_events(PointerEvents::None)
                .opacity(move || if fits() { 1.0_f32 } else { 0.0 })
                .child(dynamic(move || {
                    painted(&value())
                        .font_family(MONO)
                        .text_size(px(FONT_PX))
                        .line_height(px(LINE_H))
                        .into_element()
                })),
        )
        .child(
            nsh_core::element::input()
                .multiline(true)
                .flex_1()
                .p(px(pad))
                .font_family(MONO)
                .text_size(px(FONT_PX))
                .line_height(px(LINE_H))
                .text_color(move || if fits() { th().text.opacity(0.0).hsla() } else { th().text.hsla() })
                .caret_color(c(|t| t.text))
                .placeholder(placeholder)
                .placeholder_color(c(|t| t.text_muted))
                .value(value)
                .on_change(on_change),
        )
}

/// How far the field sets its text in from its padding (the text view's
/// line fragment padding).
const LEFT_INSET: f32 = 0.0;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runs_cover_the_line() {
        let t = Theme::dark(Color::hex(0xe8702a));
        for line in ["type User {", "  name: String! @deprecated(reason: \"x\")", "# c", "- User.email", "\"\"\"doc\"\"\""] {
            assert_eq!(highlight(line, &t).iter().map(|(l, _)| l).sum::<usize>(), line.len(), "{line:?}");
        }
    }

    #[test]
    fn keywords_types_and_comments_get_distinct_colours() {
        let t = Theme::dark(Color::hex(0xe8702a));
        let runs = highlight("type User {", &t);
        assert_eq!(runs[0].1, t.kind_color(NodeKind::Interface));
        assert!(runs.iter().any(|(_, c)| *c == t.kind_color(NodeKind::Object)));
        assert_eq!(highlight("# note", &t), vec![(6, t.text_muted)]);
    }
}
