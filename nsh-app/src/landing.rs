//! The landing screen: open a file, load the sample, or paste SDL; the
//! recently opened schemas as tiles; a drop target over all of it.

use std::path::PathBuf;

use nsh_core::prelude::*;
use nsh_metro::{Icon, TextStyle, TileSize, theme::{LIGHT, SEMIBOLD, size as msize}, tile, tile_grid};

use crate::{
    config::{self, RecentEntry},
    store::Store,
    ui::{c, metro, mono, tilde},
};

pub const SAMPLE_SDL: &str = include_str!("../assets/sample.graphql");
pub const EXTENSIONS: &str = ".graphql · .graphqls · .gql · .sdl · .txt";

const ACTION_UNIT: f32 = 72.0;
const RECENT_UNIT: f32 = 52.0;
const GAP: f32 = 8.0;
const GRID_W: f32 = 6.0 * ACTION_UNIT + 5.0 * GAP;
const RECENTS_COLLAPSED: usize = 4;

/// `YYYY-MM-DD HH:MM` (UTC) for a Unix time, as the web's history shows.
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

/// `3 types · 1 enums · 120 lines`.
pub fn summarize(sdl: &str) -> String {
    let count = |kw: &str| sdl.lines().filter(|l| l.trim_start().starts_with(kw)).count();
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

fn short_dir(path: &str) -> String {
    let dir = std::path::Path::new(path).parent().map(|d| d.display().to_string()).unwrap_or_default();
    tilde(&dir)
}

/// `updated 2025-08-19 09:41` and the type/line summary for a history tile.
fn entry_meta(r: &RecentEntry) -> (String, String) {
    let stamp = std::fs::metadata(&r.path)
        .ok()
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| format!("updated {}", format_stamp(d.as_secs() as i64)))
        .unwrap_or_else(|| "missing".into());
    let stats = std::fs::read_to_string(&r.path).map(|s| summarize(&s)).unwrap_or_else(|_| "unreadable".into());
    (stamp, stats)
}

pub fn remove_recent(path: &str) {
    let list: Vec<RecentEntry> = config::recents().into_iter().filter(|e| e.path != path).collect();
    config::write_recents(&list);
}

fn ink() -> Hsla {
    nsh_metro::palette().accent_ink
}

pub fn page(store: Store) -> impl IntoElement {
    let actions = tile_grid().columns(6).metrics(ACTION_UNIT, GAP).source(move || {
        let has_text = store.draft.with(|d| !d.trim().is_empty());
        let open = tile("open-file", TileSize::Medium)
            .title("open")
            .front(|_| {
                view()
                    .size_full()
                    .relative()
                    .items_center()
                    .justify_center()
                    .child(nsh_metro::icon(Icon::Upload, 56.0, move || AdaptiveColor::from(ink())))
                    .child(
                        nsh_metro::label("⌘O")
                            .absolute()
                            .top(px(6.0))
                            .right(px(8.0))
                            .text_size(px(msize::SMALL))
                            .text_color(move || ink().opacity(0.8)),
                    )
            })
            .on_click(move || store.open_dialog());
        let sample = tile("load-sample", TileSize::Medium).title("sample").icon(Icon::Flask).on_click(move || {
            store.draft.set(SAMPLE_SDL.to_string());
            store.draft_name.set(Some("Sample blog schema".into()));
            store.landing_error.set(None);
        });
        let visualize = tile("visualize", TileSize::Medium).title("visualize").icon(Icon::Play);
        let visualize = if has_text {
            visualize.on_click(move || store.visualize())
        } else {
            visualize.color(nsh_metro::palette().subtle)
        };
        vec![open, sample, visualize]
    });

    let recents = dynamic(move || {
        let all = store.recents.get();
        let open = store.recents_open.get();
        let total = all.len();
        if total == 0 {
            return view().into_element();
        }
        let shown: Vec<RecentEntry> =
            if open { all } else { all.into_iter().take(RECENTS_COLLAPSED).collect() };
        let tiles: Vec<_> = shown
            .into_iter()
            .map(|r| {
                let (stamp, stats) = entry_meta(&r);
                let dir = short_dir(&r.path);
                let name = r.name.clone();
                let path = r.path.clone();
                let remove_path = r.path.clone();
                let hovered = signal(false);
                tile(format!("recent-{}", r.path), TileSize::Wide)
                    .front(move |_| {
                        let line = |s: String, a: f32| {
                            nsh_metro::label(s)
                                .text_size(px(12.0))
                                .truncate()
                                .text_color(move || ink().opacity(a))
                        };
                        let remove_path = remove_path.clone();
                        view()
                            .size_full()
                            .relative()
                            .justify_end()
                            .px(px(10.0))
                            .py(px(8.0))
                            .on_mouse_enter(move |_: &MouseEvent| hovered.set(true))
                            .on_mouse_leave(move |_: &MouseEvent| hovered.set(false))
                            .child(
                                nsh_metro::label(name.clone())
                                    .text_size(px(msize::MEDIUM))
                                    .truncate()
                                    .text_color(ink),
                            )
                            .child(line(dir.clone(), 0.85))
                            .child(line(stamp.clone(), 0.7))
                            .child(line(stats.clone(), 0.7))
                            .child(
                                view()
                                    .absolute()
                                    .top(px(4.0))
                                    .right(px(4.0))
                                    .p(px(4.0))
                                    .opacity(move || if hovered.get() { 1.0_f32 } else { 0.0 })
                                    .hover(|s| s.bg(rgba(0xffffff33)))
                                    .cursor(Cursor::Pointer)
                                    .on_press(move || {
                                        remove_recent(&remove_path);
                                        store.recents.set(config::recents());
                                    })
                                    .child(nsh_metro::icon(Icon::Close, 14.0, move || AdaptiveColor::from(ink()))),
                            )
                    })
                    .on_click(move || store.open_path(PathBuf::from(path.clone())))
            })
            .collect();
        view()
            .flex_1()
            .min_h(px(0.))
            .child(
                view()
                    .flex_none()
                    .mt(px(28.0))
                    .mb(px(10.0))
                    .flex_row()
                    .items_end()
                    .gap(px(16.0))
                    .child(nsh_metro::text(TextStyle::Large, "recent"))
                    .child(view().flex_1())
                    .child(when(total > RECENTS_COLLAPSED, move || {
                        let label = if open { "show fewer".to_string() } else { format!("show all ({total})") };
                        crate::ui::small_link(label, move || store.recents_open.update(|o| *o = !*o))
                    }))
                    .child(
                        crate::ui::small_link("clear all", move || {
                            config::write_recents(&[]);
                            store.recents.set(config::recents());
                        }),
                    ),
            )
            .child(
                scroll()
                    .flex_1()
                    .min_h(px(0.))
                    .child(view().flex_none().child(tile_grid().columns(8).metrics(RECENT_UNIT, GAP).tiles(tiles))),
            )
            .into_element()
    });

    let tiles_col = view().flex_none().w(px(GRID_W)).h_full().min_h(px(0.)).child(actions).child(recents);

    let editor_col = view()
        .flex_1()
        .min_w(px(0.))
        .min_h(px(0.))
        .gap(px(12.0))
        .child(
            view()
                .flex_none()
                .flex_row()
                .items_end()
                .gap(px(16.0))
                .child(nsh_metro::text(TextStyle::Large, "paste sdl"))
                .child(dynamic(move || {
                    let name = store.draft_name.get();
                    let has_text = store.draft.with(|d| !d.trim().is_empty());
                    let label = match (name, has_text) {
                        (Some(n), _) => Some((n, true)),
                        (None, false) => Some(("or drop a file anywhere".to_string(), false)),
                        (None, true) => None,
                    };
                    match label {
                        Some((s, accent)) => metro(s, msize::SMALL)
                            .flex_1()
                            .min_w(px(0.))
                            .truncate()
                            .text_color(move || {
                                let p = nsh_metro::palette();
                                if accent { p.accent } else { p.subtle }
                            })
                            .into_element(),
                        None => view().flex_1().into_element(),
                    }
                }))
                .child(
                    metro(move || store.draft.with(|d| if d.trim().is_empty() { String::new() } else { summarize(d) }), msize::SMALL)
                        .flex_none()
                        .text_color(move || nsh_metro::palette().subtle),
                ),
        )
        .child(
            view().flex_1().min_h(px(0.)).border_1().border_color(c(|t| t.card_border)).bg(c(|t| t.input_bg)).child(
                crate::sdl::editor(
                    move || store.draft.get(),
                    move |s: &str| {
                        store.draft.set(s.to_string());
                        store.draft_name.set(None);
                    },
                    "# Paste your GraphQL SDL here…",
                    10.0,
                ),
            ),
        )
        .child(dynamic(move || match store.landing_error.get() {
            Some(e) => scroll()
                .flex_none()
                .max_h(px(160.0))
                .bg(c(|t| t.red))
                .child(
                    view()
                        .px(px(16.0))
                        .py(px(12.0))
                        .gap(px(4.0))
                        .child(metro("couldn't read this schema", msize::NORMAL).font_weight(SEMIBOLD).text_color(rgb(0xffffff)))
                        .child(mono(e, 12.0).text_color(rgb(0xffffff))),
                )
                .into_element(),
            None => view().into_element(),
        }))
        .child(dynamic(move || {
            let ws = store.warnings.get();
            if ws.is_empty() {
                return view().into_element();
            }
            let mut col = view().flex_none().gap(px(2.0)).child(
                metro("duplicate or conflicting type declarations — fix before visualizing", msize::SMALL)
                    .font_weight(SEMIBOLD)
                    .text_color(c(|t| t.type_amber)),
            );
            for w in ws.into_iter().take(8) {
                col = col.child(mono(w, 12.0).text_color(move || nsh_metro::palette().subtle));
            }
            col.into_element()
        }));

    view()
        .relative()
        .flex_1()
        .min_h(px(0.))
        .bg(c(|t| t.bg))
        .overflow_hidden()
        .px(px(48.0))
        .pt(px(16.0))
        .pb(px(32.0))
        .child(
            metro("open a schema", 72.0)
                .flex_none()
                .ml(px(-4.0))
                .font_weight(LIGHT)
                .line_height(px(72.0 * 1.15))
                .text_color(move || nsh_metro::palette().foreground),
        )
        .child(view().flex_1().min_h(px(0.)).mt(px(24.0)).flex_row().gap(px(48.0)).child(tiles_col).child(editor_col))
        .child(when(move || store.dragging.get(), move || {
            view()
                .absolute()
                .inset_0()
                .bg(move || nsh_metro::palette().background.opacity(0.85))
                .p(px(32.0))
                .pointer_events(PointerEvents::None)
                .child(
                    view()
                        .size_full()
                        .bg(move || nsh_metro::palette().accent)
                        .border_2()
                        .border_color(ink)
                        .items_start()
                        .justify_end()
                        .p(px(32.0))
                        .child(nsh_metro::icon(Icon::Upload, 72.0, move || AdaptiveColor::from(ink())))
                        .child(metro("drop to open", 72.0).mt(px(16.0)).font_weight(LIGHT).text_color(ink))
                        .child(metro(EXTENSIONS, msize::NORMAL).text_color(move || ink().opacity(0.8))),
                )
        }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stamp_formats_like_the_web() {
        assert_eq!(format_stamp(1_609_459_200), "2021-01-01 00:00");
    }

    #[test]
    fn summary_counts_declarations_and_lines() {
        let s = "type A { x: Int }\nenum B { C }\nunion U = A\n";
        assert_eq!(summarize(s), "1 types · 1 enums · 1 unions · 3 lines");
    }
}
