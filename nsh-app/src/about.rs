//! The About page: what the app is, how it draws and lays out, and tips.

use nsh_core::prelude::*;
use nsh_metro::{TextStyle, button, theme::{SEMILIGHT, size}};

use crate::{
    shell::COMMIT,
    store::{Route, Store},
    ui::metro,
};

struct Item {
    title: &'static str,
    body: &'static str,
}

const RENDERING: &[Item] = &[
    Item {
        title: "Native chrome, a GPU canvas",
        body: "Everything around the graph is Native Shell drawing AppKit's own views, in the \
               Metro design language of nsh-metro. The graph itself is one native view with a \
               renderer of its own on Metal (wgpu): AppKit's 2D drawing replays thirty thousand \
               curves one path at a time, which measured 212ms a frame with a whole schema in view.",
    },
    Item {
        title: "Edges uploaded once",
        body: "Edges are cubic Béziers in world space. They are tessellated into triangle meshes when \
               the schema is laid out, four times over for four zoom ceilings, and never again: a \
               frame is a camera uniform and a draw call. The stroke width is a uniform too, so one \
               mesh serves every zoom at the right thickness.",
    },
    Item {
        title: "Styles without re-uploading",
        body: "Which edges are lit and which recede lives in a small per-edge table the vertex \
               shader reads. Focusing a card rewrites that table; the geometry stays where it is. \
               Receded edges are drawn first and lit ones after, in opaque colours mixed into the \
               ground, so a bundle of dimmed lanes is no brighter than one.",
    },
    Item {
        title: "Text as distance fields",
        body: "Glyphs are rasterised once into an atlas as signed distance fields and thresholded \
               in the fragment shader at whatever size they land, so text costs the same whether \
               the zoom is moving or not, and every line is exactly as wide as its card. Small text \
               is rasterised at its exact pixel size instead, where a field would lose the counters.",
    },
    Item {
        title: "Level of detail",
        body: "Zooming out sheds description lines, then field rows, then header detail. Rows \
               become placeholder bars below a quarter scale and the name a bar below a fifth, so \
               every card stays text-shaped at any zoom.",
    },
    Item {
        title: "Viewport culling",
        body: "Cards outside the visible world rect are skipped, and inside a visible card only the \
               rows that meet the viewport are laid out. A frame's cards and text are a short list \
               of rectangles rebuilt from what is on screen.",
    },
    Item {
        title: "Repaint only on real change",
        body: "The view draws on the display link only when something changed. Mouse movement \
               redraws only when the hovered target changes, so moving the cursor across empty \
               canvas costs nothing. The focus ripple is the one animation that asks for frames, and \
               it stops itself after three pulses.",
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
fn body(content: impl Into<String>) -> impl IntoElement {
    view().max_w(px(720.0)).child(nsh_metro::text(TextStyle::Subtle, content.into()))
}

fn section(title: &'static str, items: &'static [Item]) -> impl IntoElement {
    let mut col = view().mt(px(48.0)).gap(px(20.0)).child(nsh_metro::text(TextStyle::Title2, title).lowercase());
    for it in items {
        col = col.child(
            view()
                .gap(px(4.0))
                .child(metro(it.title, size::MEDIUM_LARGE).font_weight(SEMILIGHT).text_color(move || nsh_metro::palette().foreground))
                .child(body(it.body)),
        );
    }
    col
}

pub fn page(store: Store) -> impl IntoElement {
    let mut tips = view().mt(px(48.0)).gap(px(12.0)).child(nsh_metro::text(TextStyle::Title2, "tips").lowercase());
    for t in TIPS {
        tips = tips.child(
            view()
                .flex_row()
                .items_start()
                .gap(px(12.0))
                .child(view().flex_none().mt(px(9.)).size(px(6.)).bg(move || nsh_metro::palette().accent))
                .child(body(*t)),
        );
    }
    scroll().flex_1().min_h(px(0.)).child(
        view()
            .w_full()
            .max_w(px(880.0))
            .self_center()
            .px(px(24.0))
            .py(px(32.0))
            .child(view().ml(px(-3.)).child(nsh_metro::text(TextStyle::Title1, "graviz").lowercase()))
            .child(view().mt(px(12.0)).child(body(
                "A GraphQL schema explorer. Load an SDL file and it becomes a graph you can walk: \
                 types as cards, fields as edges, with the parts you cannot reach and the parts you \
                 have deprecated each in their own tab.",
            )))
            .child(view().mt(px(12.0)).child(body(format!(
                "Stack — Rust on Native Shell, with the graph drawn by its own Metal renderer. Schema \
                 parsing, layout and overlay merging are all native, with no browser, WASM or worker \
                 pool in the path.{}",
                COMMIT.map(|c| format!(" Build {c}.")).unwrap_or_default()
            ))))
            .child(section("Rendering", RENDERING))
            .child(section("Graph and layout", LAYOUT))
            .child(tips)
            .child(view().mt(px(48.0)).flex_row().child(button("start visualizing").accent().on_click(move || {
                store.route.set(if store.has_schema() { Route::View } else { Route::New });
            }))),
    )
}
