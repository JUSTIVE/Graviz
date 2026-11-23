//! The About route: what this build is made of, and how to drive it.
//!
//! The web app has the same page, but its content describes a WebGL renderer
//! that no longer exists here — sprite texture caches, SDF glyph atlases,
//! render-on-demand tickers. What follows is the native pipeline that
//! replaced it, so the page keeps telling the truth about the thing you are
//! actually looking at.

use crate::theme::Theme;
use gpui::{div, prelude::*, px, SharedString, Window};
use gpui_metro::{button, text, theme::size, TextStyle};

struct Item {
    title: &'static str,
    body: &'static str,
}

const RENDERING: &[Item] = &[
    Item {
        title: "GPUI immediate-mode canvas",
        body: "The graph is painted straight into a GPUI canvas element every frame — quads for \
               cards, stroked paths for edges, shaped lines for text. There is no sprite cache and \
               no texture atlas: the retained-mode machinery the browser build needed to avoid \
               re-rasterising is simply not the shape of the problem here.",
    },
    Item {
        title: "Screen-error edge flattening",
        body: "Edges are cubic Béziers in world space. Handing those to the path builder costs \
               about five times more per edge than line segments, and the cost grows with \
               on-screen size. Instead each curve's second differences are measured at the live \
               zoom and it is flattened into just enough segments to stay inside a sub-pixel \
               error bound — one segment when zoomed out, a smooth arc when zoomed in.",
    },
    Item {
        title: "A frame budget, not a quality setting",
        body: "Before painting, the edges are priced at the ideal tolerance. If the bill exceeds \
               the frame's share, the tolerance is relaxed by exactly the square of the overrun \
               (segments scale with the inverse square root of tolerance) and no further. At any \
               zoom where the edges already fit, nothing is given up.",
    },
    Item {
        title: "Batched path building",
        body: "Path tessellation writes a 16-bit index buffer, so a single path tops out around \
               65k vertices — pour thousands of edges into one builder and geometry silently \
               disappears. Edges are flushed in batches well under that ceiling, per colour \
               group and per dim state.",
    },
    Item {
        title: "Quantised glyph shaping",
        body: "Text shaping is cached by (string, size). Scaling a font by a continuously varying \
               zoom would miss that cache on every frame, so the zoom is quantised to a ×2^(1/8) \
               ladder before it reaches the text system — imperceptible on screen, and the cache \
               holds through a drag.",
    },
    Item {
        title: "Level of detail that never drops text",
        body: "Zooming out sheds description lines, then field rows, then header detail, and \
               finally leaves a coloured chrome box. What it never does is replace text with \
               placeholder bars: a legible name at any zoom is the whole point of the view.",
    },
    Item {
        title: "Viewport culling on both axes",
        body: "Cards outside the visible world rect are skipped, and inside a visible card only \
               the field rows that intersect the viewport are laid out and shaped. Edges are \
               rejected by a precomputed bounding box before any geometry is built.",
    },
    Item {
        title: "Repaint only on real change",
        body: "Mouse movement notifies the view only when the hovered target actually changes, so \
               dragging the cursor across empty canvas costs nothing. The focus ripple is the one \
               animation that requests frames, and it stops itself after three pulses.",
    },
];

const LAYOUT: &[Item] = &[
    Item {
        title: "Native layered layout",
        body: "Layout is a Sugiyama-style layered pass written in Rust — cycle breaking, ranking, \
               virtual-node routing, crossing reduction, coordinate assignment. It replaces a \
               GraphViz WASM build plus the chunking orchestrator that existed only to keep that \
               build from running out of memory. The whole GitHub schema lays out in one pass.",
    },
    Item {
        title: "Ranking by distance from the roots",
        body: "The textbook longest-path ranking makes the graph as deep as its longest chain — \
               seventy columns on a real schema, with every edge spanning a fifth of the picture. \
               Ranking by BFS depth from Query and Mutation halves the depth, then a median pass \
               pulls each type toward the things that reference it.",
    },
    Item {
        title: "Lanes for every long edge, in both directions",
        body: "An edge spanning more than one rank is threaded through a virtual node in each rank \
               between, so it travels in the gaps instead of across whatever is in the way. Edges \
               that come out pointing backwards get lanes too — left unrouted they were the \
               single largest source of edges cutting through unrelated cards.",
    },
    Item {
        title: "Relaxed routes",
        body: "Lanes are chosen one rank at a time, so a long route arrives as a zigzag. \
               Averaging its interior turns that into the gentle arc it was meant to be: it reads \
               as one line rather than a staircase, and the flatter curve is cheaper to draw.",
    },
    Item {
        title: "Rank height balancing",
        body: "Left alone, BFS depth puts hundreds of types in one rank and the component becomes \
               a 100k-pixel ribbon. Over-tall ranks are split into genuine extra ranks before the \
               crossing-reduction sweeps, so 'one rank is one column' still holds and the ordering \
               that follows actually survives.",
    },
    Item {
        title: "Shelf packing",
        body: "Disconnected components are laid out independently and packed first-fit by \
               decreasing height; lone types go in a grid underneath. Nothing overlaps and the \
               result stays close to square.",
    },
];

const TIPS: &[&str] = &[
    "Pick a root operation in the left panel, then click a field's type to drill in.",
    "Scroll to zoom, drag to pan. ⌘[ walks back up the navigation stack.",
    "⌘K focuses the search box. Use Type.field syntax for two-phase matching.",
    "Click an edge to pin it — everything else dims and a card names both ends. Esc clears it.",
    "⌘P hides scalar fields and ⌘R hides the Relay boilerplate, to cut visual noise.",
    "⌘D renders SDL descriptions inline; ⌘E collapses parallel field edges into one arrow.",
    "⌘I highlights types and fields with no description, with a coverage figure in the corner.",
    "The Orphaned tab lists types no root operation can reach.",
    "The Deprecated tab lists every @deprecated member — expired [until] dates in red, upcoming in amber.",
    "⌘U opens the overlay dock: sketch SDL on top of the schema without touching the file.",
    "Drag the sidebar's right edge, or the dock's top edge, to resize. ⌘B collapses the sidebar.",
    "The FPS chart in the bottom-right corner reports what each frame actually costs.",
];

/// A type-ramp section: a light lowercase header, then each item as a title
/// over subtle body copy. Whitespace separates them; nothing is boxed.
fn section(title: &'static str, items: &'static [Item]) -> impl IntoElement {
    div()
        .mt_12()
        .flex()
        .flex_col()
        .gap_5()
        .child(text(TextStyle::Title2, title).lowercase())
        .children(items.iter().map(|it| {
            div()
                .flex()
                .flex_col()
                .gap_1()
                .child(
                    div()
                        .text_size(px(size::MEDIUM_LARGE))
                        .font_weight(gpui_metro::theme::SEMILIGHT)
                        .child(it.title),
                )
                .child(body(it.body))
        }))
}

fn body(content: impl Into<SharedString>) -> impl IntoElement {
    div()
        .max_w(px(720.0))
        .child(text(TextStyle::Subtle, content))
}

/// The About page. `on_start` fires when the reader clicks through to the
/// landing screen.
pub fn view<T: 'static>(
    th: Theme,
    on_start: impl Fn(&mut T, &mut Window, &mut gpui::Context<T>) + 'static,
    cx: &mut gpui::Context<T>,
) -> impl IntoElement {
    let p = gpui_metro::Theme::palette(cx);
    let entity = cx.entity().downgrade();
    div()
        .id("about-scroll")
        .flex_1()
        .min_h_0()
        .overflow_y_scroll()
        .font_family(gpui_metro::Theme::font(cx))
        .text_color(th.text)
        .child(
            div()
                .max_w(px(880.0))
                .mx_auto()
                .px_6()
                .py_8()
                .flex()
                .flex_col()
                .child(
                    div()
                        .ml(px(-3.))
                        .child(text(TextStyle::Title1, "graviz").lowercase()),
                )
                .child(div().mt_3().child(body(
                    "A GraphQL schema explorer. Load an SDL file and it becomes a graph \
                     you can walk: types as cards, fields as edges, with the parts you \
                     cannot reach and the parts you have deprecated each in their own \
                     tab.",
                )))
                .child(div().mt_3().child(body(SharedString::from(format!(
                    "Stack — Rust and GPUI, drawing to Metal. Schema parsing, layout and \
                     overlay merging are all native, with no browser, WASM or worker pool \
                     in the path.{}",
                    crate::shell::COMMIT
                        .map(|c| format!(" Build {c}."))
                        .unwrap_or_default()
                )))))
                .child(section("Rendering", RENDERING))
                .child(section("Graph and layout", LAYOUT))
                .child(
                    div()
                        .mt_12()
                        .flex()
                        .flex_col()
                        .gap_3()
                        .child(text(TextStyle::Title2, "tips").lowercase())
                        .children(TIPS.iter().map(|t| {
                            div()
                                .flex()
                                .items_start()
                                .gap_3()
                                .child(div().flex_none().mt(px(9.)).size(px(6.)).bg(p.accent))
                                .child(body(*t))
                        })),
                )
                .child(
                    div().mt_12().flex().child(
                        button("about-start", "start visualizing")
                            .accent()
                            .on_click(move |_, window, cx| {
                                entity
                                    .update(cx, |this, cx| on_start(this, window, cx))
                                    .ok();
                            }),
                    ),
                ),
        )
}
