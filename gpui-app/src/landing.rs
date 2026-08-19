//! Landing screen — the web app's `/` route: title, action bar, recent
//! schemas card, SDL editor, error/warning banners and the Visualize button.

use crate::config::{self, RecentEntry};
use crate::editor::TextArea;
use crate::icons::{icon, Icon};
use crate::theme::Theme;
use gpui::{div, prelude::*, px, Entity, SharedString, Stateful, Window};
use std::path::PathBuf;

pub const SAMPLE_SDL: &str = include_str!("../assets/sample.graphql");
pub const EXTENSIONS: &str = ".graphql · .graphqls · .gql · .sdl · .txt";

/// A `size=sm, variant=outline` shadcn button: 32px tall, 8px radius.
pub fn outline_button(
    th: Theme,
    id: &'static str,
    glyph: Icon,
    label: &'static str,
) -> Stateful<gpui::Div> {
    div()
        .id(id)
        .h(px(32.0))
        .flex()
        .flex_none()
        .items_center()
        .gap_1()
        .rounded_md()
        .border_1()
        .border_color(th.card_border)
        .px(px(10.0))
        .text_sm()
        .text_color(th.text)
        .cursor_pointer()
        .hover(|el| el.bg(th.hover_bg))
        .child(icon(glyph, px(14.0), th.text))
        .child(SharedString::from(label))
}

/// `2025-08-19 09:41` — the web's history timestamp format.
pub fn format_stamp(secs: i64) -> String {
    let days = secs.div_euclid(86_400);
    let tod = secs.rem_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02} {:02}:{:02}", tod / 3600, (tod % 3600) / 60)
}

/// `12 types · 3 enums · 1 union · 240 lines` — the web's per-entry summary.
pub fn summarize(sdl: &str) -> String {
    let count = |kw: &str| {
        sdl.lines()
            .filter(|l| l.trim_start().starts_with(kw))
            .count()
    };
    let mut parts = Vec::new();
    for (kw, word) in [("type ", "types"), ("enum ", "enums"), ("union ", "unions")] {
        let n = count(kw);
        if n > 0 {
            parts.push(format!("{n} {word}"));
        }
    }
    parts.push(format!("{} lines", sdl.lines().count()));
    parts.join(" · ")
}

pub struct LandingProps<'a> {
    pub th: Theme,
    pub editor: &'a Entity<TextArea>,
    pub recents: &'a [RecentEntry],
    pub recents_open: bool,
    pub schema_name: Option<&'a str>,
    pub error: Option<&'a str>,
    pub warnings: &'a [String],
    pub dragging: bool,
}

/// Builds the landing body. Callbacks are wired by the caller through the
/// returned element ids: `open-file`, `load-sample`, `visualize`,
/// `toggle-recents`, `clear-recents`, `recent-{i}`, `recent-x-{i}`.
pub fn view<T: 'static>(
    p: LandingProps<'_>,
    on_open: impl Fn(&mut T, &mut Window, &mut gpui::Context<T>) + 'static,
    on_sample: impl Fn(&mut T, &mut Window, &mut gpui::Context<T>) + 'static,
    on_visualize: impl Fn(&mut T, &mut Window, &mut gpui::Context<T>) + 'static,
    on_toggle_recents: impl Fn(&mut T, &mut Window, &mut gpui::Context<T>) + 'static,
    on_clear_recents: impl Fn(&mut T, &mut Window, &mut gpui::Context<T>) + 'static,
    on_pick_recent: impl Fn(&mut T, PathBuf, &mut Window, &mut gpui::Context<T>) + 'static + Clone,
    on_remove_recent: impl Fn(&mut T, String, &mut Window, &mut gpui::Context<T>) + 'static + Clone,
    cx: &mut gpui::Context<T>,
) -> impl IntoElement {
    let th = p.th;
    let recents = p.recents.to_vec();
    let recents_open = p.recents_open;

    div()
        .relative()
        .flex_1()
        .min_h_0()
        .bg(th.bg)
        .flex()
        .justify_center()
        .child(
            div()
                .w_full()
                .max_w(px(768.0))
                .flex()
                .flex_col()
                .gap_4()
                .overflow_hidden()
                .p_6()
                // 1. title + tagline
                .child(
                    div()
                        .flex_none()
                        .child(
                            div()
                                .text_2xl()
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .text_color(th.text)
                                .child("Visualize your GraphQL schema"),
                        )
                        .child(
                            div()
                                .mt_1()
                                .text_sm()
                                .text_color(th.text_muted)
                                .child(
                                    "Drop a .graphql file, pick one, or paste SDL below. \
                                     Parsed once, then opens an interactive explorer.",
                                ),
                        ),
                )
                // 2. action bar
                .child(
                    div()
                        .flex_none()
                        .flex()
                        .flex_wrap()
                        .items_center()
                        .gap_2()
                        .child(
                            outline_button(th, "open-file", Icon::Upload, "Open file").on_click(
                                cx.listener(move |this, _, window, cx| on_open(this, window, cx)),
                            ),
                        )
                        .child(
                            outline_button(th, "load-sample", Icon::Sparkles, "Load sample")
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    on_sample(this, window, cx)
                                })),
                        )
                        .when_some(p.schema_name.map(str::to_string), |el, n| {
                            el.child(
                                div()
                                    .ml_1()
                                    .flex()
                                    .items_center()
                                    .gap_1()
                                    .text_xs()
                                    .text_color(th.text_muted)
                                    .child(icon(Icon::Link2, px(12.0), th.accent))
                                    .child(SharedString::from(n)),
                            )
                        }),
                )
                // 3. recent schemas card
                .when(!recents.is_empty(), |el| {
                    el.child(
                        div()
                            .flex_none()
                            .rounded_md()
                            .border_1()
                            .border_color(th.card_border)
                            .bg(th.card_bg)
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .justify_between()
                                    .px_3()
                                    .py_2()
                                    .child(
                                        div()
                                            .id("toggle-recents")
                                            .flex()
                                            .items_center()
                                            .gap(px(6.0))
                                            .text_xs()
                                            .text_color(th.text_muted)
                                            .cursor_pointer()
                                            .hover(|el| el.text_color(th.text))
                                            .on_click(cx.listener(move |this, _, window, cx| {
                                                on_toggle_recents(this, window, cx)
                                            }))
                                            .child(icon(
                                                if recents_open {
                                                    Icon::ChevronDown
                                                } else {
                                                    Icon::ChevronRight
                                                },
                                                px(14.0),
                                                th.text_muted,
                                            ))
                                            .child(icon(Icon::History, px(14.0), th.text_muted))
                                            .child(SharedString::from(format!(
                                                "Recent schemas ({})",
                                                recents.len()
                                            ))),
                                    )
                                    .when(recents_open, |el| {
                                        el.child(
                                            div()
                                                .id("clear-recents")
                                                .h(px(28.0))
                                                .flex()
                                                .items_center()
                                                .gap_1()
                                                .rounded_md()
                                                .px_2()
                                                .text_xs()
                                                .text_color(th.text_muted)
                                                .cursor_pointer()
                                                .hover(|el| el.text_color(th.red))
                                                .on_click(cx.listener(
                                                    move |this, _, window, cx| {
                                                        on_clear_recents(this, window, cx)
                                                    },
                                                ))
                                                .child(icon(Icon::Trash2, px(12.0), th.text_muted))
                                                .child("Clear"),
                                        )
                                    }),
                            )
                            .when(recents_open, |el| {
                                el.child(
                                    div()
                                        .max_h(px(192.0))
                                        .overflow_hidden()
                                        .border_t_1()
                                        .border_color(th.panel_border)
                                        .flex()
                                        .flex_col()
                                        .children(recents.into_iter().enumerate().map(
                                            |(i, r)| {
                                                let pick = on_pick_recent.clone();
                                                let remove = on_remove_recent.clone();
                                                let path = PathBuf::from(r.path.clone());
                                                let rp = r.path.clone();
                                                let (stamp, stats) = entry_meta(&r);
                                                div()
                                                    .flex()
                                                    .items_center()
                                                    .gap_2()
                                                    .px_3()
                                                    .py_2()
                                                    .border_b_1()
                                                    .border_color(th.panel_border.opacity(0.5))
                                                    .child(
                                                        div()
                                                            .id(("recent", i))
                                                            .flex()
                                                            .flex_1()
                                                            .min_w_0()
                                                            .flex_col()
                                                            .gap(px(2.0))
                                                            .cursor_pointer()
                                                            .on_click(cx.listener(
                                                                move |this, _, window, cx| {
                                                                    pick(
                                                                        this,
                                                                        path.clone(),
                                                                        window,
                                                                        cx,
                                                                    )
                                                                },
                                                            ))
                                                            .child(
                                                                div()
                                                                    .text_sm()
                                                                    .font_weight(
                                                                        gpui::FontWeight::MEDIUM,
                                                                    )
                                                                    .text_color(th.text)
                                                                    .whitespace_nowrap()
                                                                    .overflow_hidden()
                                                                    .child(SharedString::from(
                                                                        r.name.clone(),
                                                                    )),
                                                            )
                                                            .child(
                                                                div()
                                                                    .text_size(px(11.0))
                                                                    .text_color(th.text_muted)
                                                                    .child(SharedString::from(
                                                                        stamp,
                                                                    )),
                                                            )
                                                            .child(
                                                                div()
                                                                    .text_size(px(10.0))
                                                                    .font_family("Menlo")
                                                                    .text_color(
                                                                        th.text_muted.opacity(0.7),
                                                                    )
                                                                    .whitespace_nowrap()
                                                                    .overflow_hidden()
                                                                    .child(SharedString::from(
                                                                        stats,
                                                                    )),
                                                            ),
                                                    )
                                                    .child(
                                                        div()
                                                            .id(("recent-x", i))
                                                            .flex_none()
                                                            .rounded_md()
                                                            .p_1()
                                                            .cursor_pointer()
                                                            .hover(|el| el.bg(th.hover_bg))
                                                            .on_click(cx.listener(
                                                                move |this, _, window, cx| {
                                                                    remove(
                                                                        this,
                                                                        rp.clone(),
                                                                        window,
                                                                        cx,
                                                                    )
                                                                },
                                                            ))
                                                            .child(icon(
                                                                Icon::X,
                                                                px(14.0),
                                                                th.text_muted,
                                                            )),
                                                    )
                                            },
                                        )),
                                )
                            }),
                    )
                })
                // 4. editor
                .child(
                    div()
                        .flex_1()
                        .min_h_0()
                        .rounded_md()
                        .border_1()
                        .border_color(th.card_border)
                        .bg(th.card_bg)
                        .child(p.editor.clone()),
                )
                // 5. error banner
                .when_some(p.error.map(str::to_string), |el, e| {
                    el.child(
                        div()
                            .flex_none()
                            .flex()
                            .items_start()
                            .gap_2()
                            .max_h(px(128.0))
                            .overflow_hidden()
                            .rounded_md()
                            .border_1()
                            .border_color(th.red.opacity(0.4))
                            .bg(th.red.opacity(0.1))
                            .p_3()
                            .text_xs()
                            .text_color(th.red)
                            .child(icon(Icon::CircleAlert, px(16.0), th.red))
                            .child(
                                div()
                                    .font_family("Menlo")
                                    .child(SharedString::from(e)),
                            ),
                    )
                })
                // 6. warnings banner
                .when(!p.warnings.is_empty(), |el| {
                    let ws = p.warnings.to_vec();
                    el.child(
                        div()
                            .flex_none()
                            .rounded_md()
                            .border_1()
                            .border_color(th.type_amber.opacity(0.4))
                            .bg(th.type_amber.opacity(0.1))
                            .p_3()
                            .text_xs()
                            .text_color(th.type_amber)
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .mb(px(6.0))
                                    .flex()
                                    .items_center()
                                    .gap(px(6.0))
                                    .font_weight(gpui::FontWeight::MEDIUM)
                                    .child(icon(Icon::TriangleAlert, px(14.0), th.type_amber))
                                    .child(
                                        "Schema has duplicate or conflicting type declarations \
                                         — fix before visualizing",
                                    ),
                            )
                            .children(ws.into_iter().take(8).map(|w| {
                                div()
                                    .font_family("Menlo")
                                    .child(SharedString::from(w))
                            })),
                    )
                })
                // 7. action row
                .child(
                    div()
                        .flex_none()
                        .flex()
                        .justify_end()
                        .child(
                            div()
                                .id("visualize")
                                .h(px(40.0))
                                .flex()
                                .items_center()
                                .gap_2()
                                .rounded_md()
                                .px_4()
                                .bg(th.primary)
                                .text_color(th.primary_fg)
                                .text_sm()
                                .font_weight(gpui::FontWeight::MEDIUM)
                                .cursor_pointer()
                                .hover(|el| el.opacity(0.9))
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    on_visualize(this, window, cx)
                                }))
                                .child(icon(Icon::Wand2, px(16.0), th.primary_fg))
                                .child("Visualize"),
                        ),
                ),
        )
        // 8. drag overlay
        .when(p.dragging, |el| {
            el.child(
                div()
                    .absolute()
                    .inset_0()
                    .bg(th.bg.opacity(0.8))
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(
                        div()
                            .rounded_xl()
                            .border_2()
                            .border_dashed()
                            .border_color(th.primary)
                            .bg(th.card_bg)
                            .px(px(48.0))
                            .py(px(40.0))
                            .flex()
                            .flex_col()
                            .items_center()
                            .child(icon(Icon::Upload, px(40.0), th.primary))
                            .child(
                                div()
                                    .mt_3()
                                    .text_lg()
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .text_color(th.text)
                                    .child("Drop your SDL file"),
                            )
                            .child(
                                div()
                                    .mt_1()
                                    .text_xs()
                                    .text_color(th.text_muted)
                                    .child(EXTENSIONS),
                            ),
                    ),
            )
        })
}

/// `updated 2025-08-19 09:41` + the type/line summary for a history row.
fn entry_meta(r: &RecentEntry) -> (String, String) {
    let meta = std::fs::metadata(&r.path).ok();
    let stamp = meta
        .as_ref()
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| format!("updated {}", format_stamp(d.as_secs() as i64)))
        .unwrap_or_else(|| "missing".into());
    let stats = std::fs::read_to_string(&r.path)
        .map(|s| summarize(&s))
        .unwrap_or_else(|_| "unreadable".into());
    (stamp, stats)
}

/// Recent entries are keyed by path; used by the landing's remove button.
pub fn remove_recent(path: &str) {
    let list: Vec<RecentEntry> = config::recents()
        .into_iter()
        .filter(|e| e.path != path)
        .collect();
    config::write_recents(&list);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stamp_formats_like_the_web() {
        // 2021-01-01 00:00 UTC
        assert_eq!(format_stamp(1_609_459_200), "2021-01-01 00:00");
    }

    #[test]
    fn summary_counts_declarations_and_lines() {
        let s = "type A { x: Int }\nenum B { C }\nunion U = A\n";
        assert_eq!(summarize(s), "1 types · 1 enums · 1 unions · 3 lines");
    }
}
