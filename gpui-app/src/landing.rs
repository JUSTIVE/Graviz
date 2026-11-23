//! Landing screen — a Metro start screen: big light title, accent tiles for
//! open / sample / visualize and one wide tile per recent schema, the SDL
//! paste editor, and error / warning blocks.

use crate::config::{self, RecentEntry};
use crate::editor::TextArea;
use crate::theme::Theme;
use gpui::{div, prelude::*, px, Entity, SharedString, Window};
use gpui_metro::{
    theme::size as msize, tile, tile_grid, Icon as MIcon, TextStyle, Theme as MetroTheme, TileSize,
};
use std::path::PathBuf;

pub const SAMPLE_SDL: &str = include_str!("../assets/sample.graphql");
pub const EXTENSIONS: &str = ".graphql · .graphqls · .gql · .sdl · .txt";

/// gpui-metro's hyperlink is phone-sized (20 px); 0.7 makes it 14 px.
const LINK_SCALE: f32 = 0.7;

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

/// Start-screen grid metrics. The action grid (6 columns of 72) and the
/// recents grid (8 columns of 52) are both 472px wide, so their edges line up.
const ACTION_UNIT: f32 = 72.0;
const RECENT_UNIT: f32 = 52.0;
const GAP: f32 = 8.0;
const GRID_W: f32 = 6.0 * ACTION_UNIT + 5.0 * GAP;
/// Recents shown while the list is collapsed: two rows of wide tiles.
const RECENTS_COLLAPSED: usize = 4;

/// Builds the landing body — a Metro start screen. Callbacks are wired by the
/// caller; element ids: `open-file`, `load-sample`, `visualize`,
/// `toggle-recents`, `clear-recents`, `recent-{i}`, `recent-x-{i}`.
#[allow(clippy::too_many_arguments)]
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
    let mp = MetroTheme::palette(cx);
    let font = MetroTheme::font(cx);
    let ink = mp.accent_ink;
    let recents_open = p.recents_open;
    let total = p.recents.len();
    let shown: Vec<RecentEntry> = if recents_open {
        p.recents.to_vec()
    } else {
        p.recents.iter().take(RECENTS_COLLAPSED).cloned().collect()
    };
    let sdl = p.editor.read(cx).text().to_string();
    let has_text = !sdl.trim().is_empty();
    let summary = has_text.then(|| summarize(&sdl));

    // ── action tiles ──────────────────────────────────────────────────────
    let open_tile = tile("open-file", TileSize::Medium)
        .title("open")
        .front(move |_, _| {
            div()
                .size_full()
                .relative()
                .flex()
                .items_center()
                .justify_center()
                .child(MIcon::Upload.render(px(56.0), ink))
                .child(
                    div()
                        .absolute()
                        .top(px(6.0))
                        .right(px(8.0))
                        .text_size(px(msize::SMALL))
                        .text_color(ink.opacity(0.8))
                        .child("⌘O"),
                )
        })
        .on_click(cx.listener(move |this, _, window, cx| on_open(this, window, cx)));
    let sample_tile = tile("load-sample", TileSize::Medium)
        .title("sample")
        .icon(MIcon::Flask)
        .on_click(cx.listener(move |this, _, window, cx| on_sample(this, window, cx)));
    let visualize_tile = {
        let t = tile("visualize", TileSize::Medium)
            .title("visualize")
            .icon(MIcon::Play);
        if has_text {
            t.on_click(cx.listener(move |this, _, window, cx| on_visualize(this, window, cx)))
        } else {
            // Nothing to parse yet: a grey, inert tile.
            t.color(mp.subtle)
        }
    };

    // ── recent tiles ──────────────────────────────────────────────────────
    let recent_tiles: Vec<_> = shown
        .into_iter()
        .enumerate()
        .map(|(i, r)| {
            let pick = on_pick_recent.clone();
            let remove = on_remove_recent.clone();
            let path = PathBuf::from(r.path.clone());
            let rp = r.path.clone();
            let (stamp, stats) = entry_meta(&r);
            let dir = short_dir(&r.path);
            let name = r.name.clone();
            let group: SharedString = format!("recent-tile-{i}").into();
            let remove_click = cx.listener(move |this, _: &gpui::ClickEvent, window, cx| {
                cx.stop_propagation();
                remove(this, rp.clone(), window, cx)
            });
            tile(("recent", i), TileSize::Wide)
                .front(move |_, _| {
                    let line = |s: String, a: f32| {
                        div()
                            .text_size(px(12.0))
                            .text_color(ink.opacity(a))
                            .whitespace_nowrap()
                            .overflow_hidden()
                            .text_ellipsis()
                            .child(SharedString::from(s))
                    };
                    div()
                        .group(group.clone())
                        .size_full()
                        .relative()
                        .flex()
                        .flex_col()
                        .justify_end()
                        .px(px(10.0))
                        .py(px(8.0))
                        .child(
                            div()
                                .text_size(px(msize::MEDIUM))
                                .text_color(ink)
                                .whitespace_nowrap()
                                .overflow_hidden()
                                .text_ellipsis()
                                .child(SharedString::from(name)),
                        )
                        .child(line(dir, 0.85))
                        .child(line(stamp, 0.7))
                        .child(line(stats, 0.7))
                        .child(
                            div()
                                .id(("recent-x", i))
                                .absolute()
                                .top(px(4.0))
                                .right(px(4.0))
                                .p(px(4.0))
                                .opacity(0.0)
                                .group_hover(group, |s| s.opacity(1.0))
                                .hover(|s| s.bg(ink.opacity(0.2)))
                                .cursor_pointer()
                                .on_click(remove_click)
                                .child(MIcon::Close.render(px(14.0), ink)),
                        )
                })
                .on_click(
                    cx.listener(move |this, _, window, cx| pick(this, path.clone(), window, cx)),
                )
        })
        .collect();

    // gpui-metro's hyperlink (underlined, accent while pressed), scaled from
    // its 20 px phone size down to the 14 px this header row uses.
    let link = |id: &'static str, label: String| gpui_metro::hyperlink(id, label);
    let small = |l: gpui_metro::HyperlinkButton| gpui_metro::scaled(LINK_SCALE).child(l);

    // ── left column: tiles ────────────────────────────────────────────────
    let tiles_col = div()
        .flex_none()
        .w(px(GRID_W))
        .min_h_0()
        .flex()
        .flex_col()
        .child(
            tile_grid()
                .columns(6)
                .metrics(ACTION_UNIT, GAP)
                .tile(open_tile)
                .tile(sample_tile)
                .tile(visualize_tile),
        )
        .when(total > 0, |el| {
            el.child(
                div()
                    .flex_none()
                    .mt(px(28.0))
                    .mb(px(10.0))
                    .flex()
                    .items_baseline()
                    .gap(px(16.0))
                    .child(TextStyle::Large.styled(cx).child("recent"))
                    .child(div().flex_1())
                    .when(total > RECENTS_COLLAPSED, |el| {
                        el.child(small(
                            link(
                                "toggle-recents",
                                if recents_open {
                                    "show fewer".into()
                                } else {
                                    format!("show all ({total})")
                                },
                            )
                            .on_click(cx.listener(
                                move |this, _, window, cx| on_toggle_recents(this, window, cx),
                            )),
                        ))
                    })
                    .child(small(link("clear-recents", "clear all".into()).on_click(
                        cx.listener(move |this, _, window, cx| on_clear_recents(this, window, cx)),
                    ))),
            )
            .child(
                div()
                    // Scrolls rather than clips: the list grows with every
                    // schema opened.
                    .id("landing-recents")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .child(
                        tile_grid()
                            .columns(8)
                            .metrics(RECENT_UNIT, GAP)
                            .tiles(recent_tiles),
                    ),
            )
        });

    // ── right column: SDL editor + diagnostics ────────────────────────────
    let editor_col = div()
        .flex_1()
        .min_w_0()
        .min_h_0()
        .flex()
        .flex_col()
        .gap(px(12.0))
        .child(
            div()
                .flex_none()
                .flex()
                .items_baseline()
                .gap(px(16.0))
                .child(TextStyle::Large.styled(cx).child("paste sdl"))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .whitespace_nowrap()
                        .overflow_hidden()
                        .text_ellipsis()
                        .text_size(px(msize::SMALL))
                        .text_color(mp.subtle)
                        .when_some(p.schema_name.map(str::to_string), |el, n| {
                            el.child(div().text_color(mp.accent).child(SharedString::from(n)))
                        })
                        .when(p.schema_name.is_none() && summary.is_none(), |el| {
                            el.child("or drop a file anywhere")
                        }),
                )
                .when_some(summary, |el, s| {
                    el.child(
                        div()
                            .flex_none()
                            .text_size(px(msize::SMALL))
                            .text_color(mp.subtle)
                            .child(SharedString::from(s)),
                    )
                }),
        )
        .child(div().flex_1().min_h_0().child(p.editor.clone()))
        .when_some(p.error.map(str::to_string), |el, e| {
            // Metro message block: solid fill, semibold heading, body text.
            el.child(
                div()
                    .id("landing-error")
                    .flex_none()
                    .max_h(px(160.0))
                    .overflow_y_scroll()
                    .bg(th.red)
                    .px(px(16.0))
                    .py(px(12.0))
                    .flex()
                    .flex_col()
                    .gap(px(4.0))
                    .text_color(gpui::white())
                    .child(
                        div()
                            .text_size(px(msize::NORMAL))
                            .font_weight(gpui_metro::theme::SEMIBOLD)
                            .child("couldn't read this schema"),
                    )
                    .child(
                        div()
                            .text_size(px(12.0))
                            .font_family("Menlo")
                            .child(SharedString::from(e)),
                    ),
            )
        })
        .when(!p.warnings.is_empty(), |el| {
            let ws = p.warnings.to_vec();
            el.child(
                div()
                    .flex_none()
                    .flex()
                    .flex_col()
                    .gap(px(2.0))
                    .child(
                        div()
                            .text_size(px(msize::SMALL))
                            .font_weight(gpui_metro::theme::SEMIBOLD)
                            .text_color(th.type_amber)
                            .child(
                                "duplicate or conflicting type declarations — fix before \
                                 visualizing",
                            ),
                    )
                    .children(ws.into_iter().take(8).map(|w| {
                        div()
                            .text_size(px(12.0))
                            .font_family("Menlo")
                            .text_color(mp.subtle)
                            .child(SharedString::from(w))
                    })),
            )
        });

    div()
        .relative()
        .flex_1()
        .min_h_0()
        .bg(th.bg)
        .font_family(font)
        .flex()
        .flex_col()
        .overflow_hidden()
        .px(px(48.0))
        .pt(px(16.0))
        .pb(px(32.0))
        // title: big light lowercase heading (the shell already shows
        // the GRAVIZ app title in the titlebar)
        .child(
            div()
                .flex_none()
                .ml(px(-4.0))
                .text_size(px(72.0))
                .font_weight(gpui_metro::theme::LIGHT)
                .line_height(gpui::relative(1.15))
                .text_color(mp.foreground)
                .child("open a schema"),
        )
        .child(
            div()
                .flex_1()
                .min_h_0()
                .mt(px(24.0))
                .flex()
                .gap(px(48.0))
                .child(tiles_col)
                .child(editor_col),
        )
        // drag-over: the whole screen becomes an accent drop target
        .when(p.dragging, |el| {
            el.child(
                div()
                    .absolute()
                    .inset_0()
                    .bg(mp.background.opacity(0.85))
                    .p(px(32.0))
                    .child(
                        div()
                            .size_full()
                            .bg(mp.accent)
                            .border_2()
                            .border_color(ink)
                            .flex()
                            .flex_col()
                            .items_start()
                            .justify_end()
                            .p(px(32.0))
                            .child(MIcon::Upload.render(px(72.0), ink))
                            .child(
                                div()
                                    .mt(px(16.0))
                                    .text_size(px(72.0))
                                    .font_weight(gpui_metro::theme::LIGHT)
                                    .text_color(ink)
                                    .child("drop to open"),
                            )
                            .child(
                                div()
                                    .text_size(px(msize::NORMAL))
                                    .text_color(ink.opacity(0.8))
                                    .child(EXTENSIONS),
                            ),
                    ),
            )
        })
}

/// The entry's folder, with `$HOME` shortened to `~`.
fn short_dir(path: &str) -> String {
    let dir = std::path::Path::new(path)
        .parent()
        .map(|d| d.display().to_string())
        .unwrap_or_default();
    match std::env::var("HOME") {
        Ok(home) if !home.is_empty() && dir.starts_with(&home) => {
            format!("~{}", &dir[home.len()..])
        }
        _ => dir,
    }
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
