//! What one frame of the graph looks like: the cards on screen, their text,
//! and how each edge is styled. Geometry, colours and draw order follow the
//! gpui canvas this replaced, which followed the web renderer's
//! `drawNodeSprite`.

use graviz_core::{
    graph::NodeKind,
    layout::{CubicSeg, Point},
};

use super::{
    gpu::{Camera, EdgeStyle, Frame, RectInstance, TriVertex},
    text::{Face, Fonts},
};
use crate::{
    model::{
        BAND_FONT_PX, CARD_PAD_X, Card, DESC_FONT_PX, EdgeGroup, HEADER_PAD_X, KIND_FONT_PX, Model, NAME_FONT_PX,
        ROW_FONT_PX, RowHit, RowKind, TIGHT_ROW_H, TypeColor, mono_w,
    },
    theme::{Color, Theme},
};

/// Below this zoom, row text gives way to placeholder bars.
pub const LOD_ROWS: f32 = 0.28;
/// Below this zoom, header text gives way to a name bar.
pub const LOD_HEADER: f32 = 0.2;
/// Zoom range over which description lines fade out, rather than cut.
/// Descriptions are the 9px text: below about 0.42 they are under 3.8px on
/// screen, a grey smear that reads as texture, not words.
const LOD_DESC: f32 = 0.42;
const LOD_DESC_FADE: f32 = 0.14;
/// Alpha applied to every card that is not the one in focus.
const DIM_ALPHA: f32 = 0.1;
/// How much of a receded edge's colour survives while something else has
/// the focus. It is mixed into the background and drawn opaque, so a bundle
/// of thirty receded lanes is no brighter than one.
const EDGE_DIM: f32 = 0.10;
/// Arrowheads appear from this zoom up.
const LOD_ARROWS: f32 = 0.3;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Hover {
    pub card: u32,
    /// `None`: the header.
    pub row: Option<RowHit>,
}

/// Everything about a frame that is not the model or the camera.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ViewState {
    pub hover: Option<Hover>,
    pub focus: Option<u32>,
    /// A pinned row: (card, row).
    pub pinned: Option<(u32, usize)>,
    pub investigate: bool,
    pub highlight_overlay: bool,
    pub hovered_edge: Option<u32>,
    pub focused_edge: Option<u32>,
    /// 0..1 through the focus ring's ripple, while it plays.
    pub ripple: Option<f32>,
}

fn edge_color(th: &Theme, group: EdgeGroup) -> Color {
    let c = match group {
        EdgeGroup::FieldNonNull | EdgeGroup::FieldNullable => th.kind_color(NodeKind::Object),
        EdgeGroup::Union => th.kind_color(NodeKind::Union),
        EdgeGroup::Implements => th.kind_color(NodeKind::Interface),
        EdgeGroup::Arg => th.arg_orange,
    };
    match group {
        EdgeGroup::FieldNullable => c.opacity(0.45),
        EdgeGroup::Implements | EdgeGroup::Arg => c.opacity(0.55),
        _ => c.opacity(0.85),
    }
}

fn edge_is_dimmed(index: u32, from: u32, to: u32, hub_faded: bool, st: &ViewState) -> bool {
    // Investigate highlights types and rows, never edges: an edge into an
    // undocumented type says nothing about the edge.
    if st.investigate {
        return true;
    }
    match (st.focused_edge, st.focus) {
        (Some(pinned), _) => pinned != index,
        (None, Some(f)) => from != f && to != f,
        (None, None) => hub_faded,
    }
}

/// The edge drawn on top, thick and cased, instead of in the mesh.
fn lifted(st: &ViewState) -> Option<u32> {
    st.hovered_edge.or(st.focused_edge)
}

/// How every edge is drawn in this state. Depends on the state only, never
/// the camera, so it is sent to the GPU when the state changes and not
/// otherwise.
pub fn edge_styles(model: &Model, th: &Theme, st: &ViewState) -> Vec<EdgeStyle> {
    let receding = st.focused_edge.is_some() || st.focus.is_some() || st.investigate;
    let dim = if receding { EDGE_DIM } else { 0.35 };
    model
        .edges
        .iter()
        .enumerate()
        .map(|(i, e)| {
            let c = edge_color(th, e.group);
            let (color, layer) = if Some(i as u32) == lifted(st) {
                (c, 9)
            } else if edge_is_dimmed(i as u32, e.from, e.to, e.hub_faded, st) {
                (c.opacity(dim).over(th.bg), 0)
            } else {
                (c, 1)
            };
            EdgeStyle { color: color.array(), layer, _pad: [0; 3] }
        })
        .collect()
}

fn card_is_documented(card: &Card) -> bool {
    card.description.is_some()
        && !card.rows.iter().any(|r| matches!(r.kind, RowKind::Field | RowKind::EnumValue) && !r.is_documented())
}

fn desc_alpha(k: f32) -> f32 {
    ((k - LOD_DESC) / LOD_DESC_FADE).clamp(0.0, 1.0)
}

/// Row index range whose band meets the visible world rect: a tall card
/// must not lay out rows nobody can see.
fn visible_rows(card: &Card, card_y: f32, wy0: f32, wy1: f32) -> (usize, usize) {
    let pitch = card.body_pitch();
    let top = card_y + card.body_top();
    let lo = (((wy0 - top) / pitch).floor() as isize).max(0) as usize;
    let hi = (((wy1 - top) / pitch).ceil() as isize).max(0) as usize + 1;
    (lo.min(card.rows.len()), hi.min(card.rows.len()))
}

fn kind_caption(kind: NodeKind) -> &'static str {
    match kind {
        NodeKind::Object => "object",
        NodeKind::Interface => "interface",
        NodeKind::Union => "union",
        NodeKind::Enum => "enum",
        NodeKind::Scalar => "scalar",
        NodeKind::Input => "input",
    }
}

fn fill(out: &mut Vec<RectInstance>, x: f32, y: f32, w: f32, h: f32, c: Color) {
    if c.a > 0.0 && w > 0.0 && h > 0.0 {
        out.push(RectInstance { rect: [x, y, w, h], color: c.array() });
    }
}

/// A rectangle's outline, `t` wide, inside its bounds.
/// A dashed ring like gpui's `BorderStyle::Dashed`: dashes twice the
/// stroke long, with gaps as long.
fn dashed_outline(out: &mut Vec<RectInstance>, x: f32, y: f32, w: f32, h: f32, t: f32, c: Color) {
    let dash = (2.0 * t).max(1.0);
    let run = |out: &mut Vec<RectInstance>, len: f32, mut put: Box<dyn FnMut(&mut Vec<RectInstance>, f32, f32)>| {
        let mut a = 0.0;
        while a < len {
            put(out, a, dash.min(len - a));
            a += 2.0 * dash;
        }
    };
    run(out, w, Box::new(move |o, a, l| fill(o, x + a, y, l, t, c)));
    run(out, w, Box::new(move |o, a, l| fill(o, x + a, y + h - t, l, t, c)));
    run(out, h, Box::new(move |o, a, l| fill(o, x, y + a, t, l, c)));
    run(out, h, Box::new(move |o, a, l| fill(o, x + w - t, y + a, t, l, c)));
}

fn outline(out: &mut Vec<RectInstance>, x: f32, y: f32, w: f32, h: f32, t: f32, c: Color) {
    fill(out, x, y, w, t, c);
    fill(out, x, y + h - t, w, t, c);
    fill(out, x, y + t, t, h - 2.0 * t, c);
    fill(out, x + w - t, y + t, t, h - 2.0 * t, c);
}

/// Segments that keep one cubic within `tol` screen points of its chords.
fn cubic_steps(p0: Point, c: &CubicSeg, k: f32, tol: f32) -> usize {
    let w = p0.x.max(c.c1.x).max(c.c2.x).max(c.end.x) - p0.x.min(c.c1.x).min(c.c2.x).min(c.end.x);
    let h = p0.y.max(c.c1.y).max(c.c2.y).max(c.end.y) - p0.y.min(c.c1.y).min(c.c2.y).min(c.end.y);
    if w.min(h) * k <= tol {
        return 1;
    }
    let d0x = (p0.x - 2.0 * c.c1.x + c.c2.x) * k;
    let d0y = (p0.y - 2.0 * c.c1.y + c.c2.y) * k;
    let d1x = (c.c1.x - 2.0 * c.c2.x + c.end.x) * k;
    let d1y = (c.c1.y - 2.0 * c.c2.y + c.end.y) * k;
    let m = (d0x * d0x + d0y * d0y).max(d1x * d1x + d1y * d1y).sqrt();
    ((0.75 * m / tol).sqrt().ceil() as usize).clamp(1, 48)
}

/// A polyline as a `width`-wide ribbon of triangles, mitred at its joints.
fn ribbon(out: &mut Vec<TriVertex>, pts: &[(f32, f32)], width: f32, color: Color) {
    if pts.len() < 2 {
        return;
    }
    let half = width / 2.0;
    let color = color.array();
    let dir = |a: (f32, f32), b: (f32, f32)| {
        let (dx, dy) = (b.0 - a.0, b.1 - a.1);
        let len = (dx * dx + dy * dy).sqrt().max(1e-4);
        (dx / len, dy / len)
    };
    let mut sides: Vec<[(f32, f32); 2]> = Vec::with_capacity(pts.len());
    for i in 0..pts.len() {
        let before = if i > 0 { dir(pts[i - 1], pts[i]) } else { dir(pts[0], pts[1]) };
        let after = if i + 1 < pts.len() { dir(pts[i], pts[i + 1]) } else { before };
        // The joint's normal bisects the two segments'; lengthen it so the
        // ribbon keeps its width through the turn, within reason.
        let (nx, ny) = (-(before.1 + after.1), before.0 + after.0);
        let len = (nx * nx + ny * ny).sqrt().max(1e-4);
        let (nx, ny) = (nx / len, ny / len);
        let cos = (nx * -before.1 + ny * before.0).max(0.35);
        let reach = half / cos;
        sides.push([(pts[i].0 + nx * reach, pts[i].1 + ny * reach), (pts[i].0 - nx * reach, pts[i].1 - ny * reach)]);
    }
    for w in sides.windows(2) {
        let (a, b) = (w[0], w[1]);
        for p in [a[0], a[1], b[0], a[1], b[1], b[0]] {
            out.push(TriVertex { pos: [p.0, p.1], color });
        }
    }
}

/// Fills `frame` with everything but the edge meshes.
#[allow(clippy::too_many_arguments)]
pub fn paint(
    frame: &mut Frame,
    fonts: &mut Fonts,
    model: &Model,
    th: &Theme,
    cam: Camera,
    vw: f32,
    vh: f32,
    st: &ViewState,
) {
    let k = cam.k;
    frame.reset();
    frame.clear = Some(th.bg);
    frame.edge_half_width = (1.5 * k).clamp(0.6, 2.5) / 2.0;
    let sx = |x: f32| x * k + cam.x;
    let sy = |y: f32| y * k + cam.y;
    let (wx0, wy0, wx1, wy1) = (-cam.x / k, -cam.y / k, (vw - cam.x) / k, (vh - cam.y) / k);

    // ---- arrowheads, receded edges' first so lit ones sit on top ----
    if k >= LOD_ARROWS {
        let styles = edge_styles(model, th, st);
        for layer in [0u32, 1] {
            for (e, style) in model.edges.iter().zip(&styles) {
                if style.layer != layer {
                    continue;
                }
                let Some(last) = e.curves.last() else { continue };
                let (ex, ey) = (last.end.x, last.end.y);
                if ex < wx0 - 20.0 || ex > wx1 + 20.0 || ey < wy0 - 20.0 || ey > wy1 + 20.0 {
                    continue;
                }
                arrow(&mut frame.tris, (sx(ex), sy(ey)), (ex - last.c2.x, ey - last.c2.y), k, style.color);
            }
        }
    }

    // ---- the hovered or pinned edge: cased, thick, on top of the rest ----
    if let Some(e) = lifted(st).and_then(|i| model.edges.get(i as usize)) {
        let mut pts = vec![(sx(e.start.x), sy(e.start.y))];
        let mut p0 = e.start;
        for c in &e.curves {
            let n = cubic_steps(p0, c, k, 0.25);
            for i in 1..=n {
                let t = i as f32 / n as f32;
                let u = 1.0 - t;
                let (a, b, cc, d) = (u * u * u, 3.0 * u * u * t, 3.0 * u * t * t, t * t * t);
                pts.push((
                    sx(a * p0.x + b * c.c1.x + cc * c.c2.x + d * c.end.x),
                    sy(a * p0.y + b * c.c1.y + cc * c.c2.y + d * c.end.y),
                ));
            }
            p0 = c.end;
        }
        // A wide stroke in the ground's colour, then the edge itself: a
        // brighter line among thirty lanes of its own colour is still hard
        // to follow, and the casing cuts it clear of them.
        let w = (frame.edge_half_width * 4.0).max(2.5);
        let color = edge_color(th, e.group).opacity(1.0);
        let color = Color { a: 1.0, ..color }.lighten(0.3);
        ribbon(&mut frame.tris, &pts, w + 5.0, th.bg);
        ribbon(&mut frame.tris, &pts, w, color);
        if let Some(last) = e.curves.last() {
            let tip = (sx(last.end.x), sy(last.end.y));
            arrow(&mut frame.tris, tip, (last.end.x - last.c2.x, last.end.y - last.c2.y), k, color.array());
        }
    }

    // Cards one edge away from the focused one are lit alongside it.
    let neighbours: std::collections::HashSet<u32> = match st.focus.filter(|_| st.focused_edge.is_none()) {
        Some(f) => model
            .edges
            .iter()
            .filter_map(|e| if e.from == f { Some(e.to) } else if e.to == f { Some(e.from) } else { None })
            .collect(),
        None => Default::default(),
    };
    let pinned_ends = st.focused_edge.and_then(|fe| model.edges.get(fe as usize)).map(|e| (e.from, e.to));
    let desc_a = desc_alpha(k);
    let white = Color::WHITE;

    for (i, card) in model.cards.iter().enumerate() {
        let pos = model.positions[i];
        if pos.x + card.w < wx0 || pos.x > wx1 || pos.y + card.h < wy0 || pos.y > wy1 {
            continue;
        }
        let ci = i as u32;
        let receded = if st.highlight_overlay {
            !card.is_overlay && !card.rows.iter().any(|r| r.is_overlay)
        } else if st.investigate {
            // Investigate is a search for what is missing; a fully
            // documented type is the answer "not here".
            card_is_documented(card)
        } else {
            match pinned_ends {
                Some((from, to)) => ci != from && ci != to,
                None => matches!(st.focus, Some(f) if f != ci && !neighbours.contains(&ci)),
            }
        };
        let dim = if receded { DIM_ALPHA } else { 1.0 };
        let kind_c = th.kind_color(card.kind);
        let is_hovered = matches!(st.hover, Some(h) if h.card == ci);
        let is_focused = st.focus == Some(ci);
        let rects = &mut frame.rects;
        // A rectangle given in the card's own units; never thinner than
        // half a point, so a hairline survives zooming out.
        let local =
            |rects: &mut Vec<RectInstance>, x: f32, y: f32, w: f32, h: f32, c: Color| {
                fill(rects, sx(pos.x + x), sy(pos.y + y), (w * k).max(0.5), (h * k).max(0.5), c);
            };

        // Card body: a flat tile, no outline; the kind-colour header and the
        // ground around it carry the edge. Only overlay types keep a ring,
        // since that is a fact about the card rather than decoration.
        local(rects, 0.0, 0.0, card.w, card.h, th.card_bg.opacity(dim));
        if card.is_overlay {
            let t = (2.0 * k).clamp(0.5, 3.0);
            dashed_outline(rects, sx(pos.x), sy(pos.y), card.w * k, card.h * k, t, th.overlay_green.opacity(dim));
        }
        local(rects, 0.0, 0.0, card.w, card.header_h, kind_c.opacity(dim));

        // Trailing wash bands: violet = implements, amber = member of union.
        if !card.implements.is_empty() {
            let c = th.kind_color(NodeKind::Interface);
            let (t, b) = (card.band.iface_band_top, card.band.iface_band_bottom);
            local(rects, 0.0, t, card.w, b - t, c.opacity(0.1 * dim));
            local(rects, 0.0, t, card.w, 0.5, c.opacity(0.4 * dim));
        }
        if !card.member_of_unions.is_empty() {
            let c = th.kind_color(NodeKind::Union);
            let t = card.band.union_band_top;
            local(rects, 0.0, t, card.w, card.h - t, c.opacity(0.1 * dim));
            local(rects, 0.0, t, card.w, 0.5, c.opacity(0.4 * dim));
        }

        let glyphs = &mut frame.glyphs;
        let mut text = |glyphs: &mut Vec<_>,
                        rects: &mut Vec<RectInstance>,
                        face: Face,
                        s: &str,
                        px: f32,
                        c: Color,
                        strike: Option<Color>,
                        x: f32,
                        baseline: f32| {
            let (x, y) = (sx(x), sy(baseline));
            let w = fonts.draw(glyphs, face, s, px * k, c, x, y);
            if let Some(strike) = strike {
                fill(rects, x, y - px * k * 0.3, w, (0.75 * k).max(0.5), strike);
            }
        };

        // ---- header ----
        if k >= LOD_HEADER {
            text(
                glyphs,
                rects,
                Face::Sans,
                kind_caption(card.kind),
                KIND_FONT_PX,
                white.opacity(0.6 * dim),
                None,
                pos.x + HEADER_PAD_X,
                pos.y + 14.0,
            );
            if card.is_overlay {
                let tag_x = pos.x + card.w - HEADER_PAD_X - mono_w("OVERLAY", 8.0);
                text(glyphs, rects, Face::Sans, "overlay", 8.0, white.opacity(0.9 * dim), None, tag_x, pos.y + 14.0);
            }
            text(
                glyphs,
                rects,
                Face::Sans,
                &card.name_fit,
                NAME_FONT_PX,
                white.opacity(dim),
                None,
                pos.x + HEADER_PAD_X,
                pos.y + 30.0,
            );
            if let Some(d) = card.header_desc.as_ref().filter(|_| desc_a > 0.0) {
                text(
                    glyphs,
                    rects,
                    Face::Mono,
                    d,
                    DESC_FONT_PX,
                    white.opacity(0.75 * dim * desc_a),
                    None,
                    pos.x + HEADER_PAD_X,
                    pos.y + 42.0,
                );
            }
        } else {
            // Text-shaped presence survives every zoom: a name bar in the
            // header band even when real glyphs would be sub-pixel.
            let name_w = mono_w(&card.name, NAME_FONT_PX).min(card.w * 0.7);
            local(rects, HEADER_PAD_X, card.header_h * 0.45, name_w, NAME_FONT_PX * 0.4, white.opacity(0.55 * dim));
        }

        // ---- body ----
        let pitch = card.body_pitch();
        let (r_lo, r_hi) = visible_rows(card, pos.y, wy0, wy1);
        if k >= LOD_ROWS {
            if card.rows.is_empty() && card.hidden_rows > 0 {
                // Every field is behind a filter. An empty body would read
                // as a type that declares nothing, which is a different fact.
                let s = format!("… {} hidden field{}", card.hidden_rows, if card.hidden_rows == 1 { "" } else { "s" });
                text(
                    glyphs,
                    rects,
                    Face::Mono,
                    &s,
                    ROW_FONT_PX,
                    th.text_muted.opacity(0.7 * dim),
                    None,
                    pos.x + CARD_PAD_X,
                    pos.y + card.row_baseline(0),
                );
            }
            for (ri, row) in card.rows.iter().enumerate().take(r_hi).skip(r_lo) {
                let fy = pos.y + card.row_baseline(ri);
                let left = pos.x + CARD_PAD_X;
                match row.kind {
                    RowKind::Field => {
                        let dep_a = if row.deprecated && !row.until_expired { 0.4 } else { 1.0 };
                        let name_c = if row.until_expired { th.expired } else { th.text };
                        let strike = row.deprecated.then_some(name_c.opacity(dep_a * dim));
                        text(glyphs, rects, Face::Mono, &row.left, ROW_FONT_PX, name_c.opacity(dep_a * dim), strike, left, fy);
                        if !row.right.is_empty() {
                            let (ty_c, ty_a) = match row.type_color {
                                TypeColor::Expired => (th.expired, 1.0),
                                TypeColor::BuiltinScalar => (th.type_builtin, 0.7),
                                TypeColor::Normal => (th.type_amber, 1.0),
                            };
                            let tx = pos.x + card.w - CARD_PAD_X - row.right_w;
                            text(
                                glyphs,
                                rects,
                                Face::Mono,
                                &row.right_fit,
                                ROW_FONT_PX,
                                ty_c.opacity(ty_a * dep_a * dim),
                                strike,
                                tx,
                                fy,
                            );
                            if row.is_relay {
                                // The chain glyph: two linked bars.
                                let c = th.relay_orange.opacity(0.85 * dim);
                                let t = k.max(0.5);
                                for dx in [-4.0f32, -0.5] {
                                    outline(rects, sx(tx - 8.0 + dx), sy(fy - 2.0 - 1.5), 4.5 * k, 3.0 * k, t, c);
                                }
                            }
                        }
                    }
                    RowKind::EnumValue => {
                        // Muted; struck only when the sunset date has passed.
                        let c = if row.until_expired { th.expired } else { th.text_muted };
                        let strike = row.until_expired.then_some(th.expired.opacity(dim));
                        text(glyphs, rects, Face::Mono, &row.left, ROW_FONT_PX, c.opacity(dim), strike, left, fy);
                    }
                    RowKind::UnionMember => {
                        let c = th.kind_color(NodeKind::Object).opacity(dim);
                        text(glyphs, rects, Face::Mono, &row.left, ROW_FONT_PX, c, None, left, fy);
                    }
                }
                if row.is_overlay {
                    local(rects, 3.0, card.row_baseline(ri) - 7.0, 2.0, 9.0, th.overlay_green.opacity(dim));
                }
                if let Some(d) = row.description_line.as_ref().filter(|_| desc_a > 0.0) {
                    let c = th.text_muted.opacity(0.7 * dim * desc_a);
                    text(glyphs, rects, Face::Mono, d, DESC_FONT_PX, c, None, left, fy + 11.0);
                }
            }
            if card.kind == NodeKind::Scalar && card.rows.is_empty() {
                let c = th.text_muted.opacity(dim);
                let y = pos.y + card.body_top() + 10.0;
                text(glyphs, rects, Face::MonoItalic, "custom scalar", ROW_FONT_PX, c, None, pos.x + CARD_PAD_X, y);
            }
            // Band text: one name per line, no prefix; the band's colour
            // carries the meaning.
            for (bi, name) in card.implements.iter().enumerate() {
                let by = card.band.iface_rows_top + bi as f32 * TIGHT_ROW_H + 10.0;
                let c = th.kind_color(NodeKind::Interface).opacity(dim);
                text(glyphs, rects, Face::MonoBold, name, BAND_FONT_PX, c, None, pos.x + CARD_PAD_X, pos.y + by);
            }
            for (bi, name) in card.member_of_unions.iter().enumerate() {
                let by = card.band.union_rows_top + bi as f32 * TIGHT_ROW_H + 10.0;
                let c = th.kind_color(NodeKind::Union).opacity(dim);
                text(glyphs, rects, Face::MonoBold, name, BAND_FONT_PX, c, None, pos.x + CARD_PAD_X, pos.y + by);
            }
        } else {
            // Placeholder bars keep every card text-shaped at any zoom.
            let bar_h = (ROW_FONT_PX * k).max(0.6);
            for (ri, row) in card.rows.iter().enumerate().take(r_hi).skip(r_lo) {
                let by = sy(pos.y + card.row_y(ri) + (pitch - ROW_FONT_PX) / 2.0);
                let left_w = mono_w(&row.left, ROW_FONT_PX).min(card.w * 0.55);
                fill(rects, sx(pos.x + CARD_PAD_X), by, left_w * k, bar_h, th.text_muted.opacity(0.3 * dim));
                if !row.right.is_empty() {
                    let right_w = mono_w(&row.right, ROW_FONT_PX).min(card.w * 0.4);
                    let x = sx(pos.x + card.w - CARD_PAD_X - right_w);
                    fill(rects, x, by, right_w * k, bar_h, th.type_amber.opacity(0.25 * dim));
                }
            }
        }

        // ---- interaction overlays, above the card ----
        if st.investigate {
            if card.description.is_none() {
                let t = (3.0 * k).clamp(1.0, 4.0);
                let c = th.investigate.opacity(0.95);
                outline(rects, sx(pos.x - 4.0), sy(pos.y - 4.0), (card.w + 8.0) * k, (card.h + 8.0) * k, t, c);
            }
            // No LOD gate on the stripes: the mode is most useful zoomed
            // out, where a card's outline says nothing about which of its
            // fields are bare.
            for (ri, row) in card.rows.iter().enumerate().take(r_hi).skip(r_lo) {
                if matches!(row.kind, RowKind::Field | RowKind::EnumValue) && !row.is_documented() {
                    local(rects, 4.0, card.row_y(ri), card.w - 8.0, pitch, th.investigate.opacity(0.22));
                }
            }
        }
        if let Some((pc, row)) = st.pinned {
            if pc == ci && row < card.rows.len() {
                let (x, y) = (sx(pos.x + 2.0), sy(pos.y + card.row_y(row) - 2.0));
                let (w, h) = ((card.w - 4.0) * k, (pitch + 4.0) * k);
                fill(rects, x, y, w, h, th.pin.opacity(0.18));
                outline(rects, x, y, w, h, (2.0 * k).clamp(1.0, 3.0), th.pin.opacity(0.95));
            }
        }
        if let Some(Hover { card: hc, row: Some(hit) }) = st.hover {
            if hc == ci {
                let (hy, hh) = match hit {
                    RowHit::Row(r) => (card.row_y(r), pitch),
                    RowHit::Implements(b) => (card.band.iface_rows_top + b as f32 * TIGHT_ROW_H, TIGHT_ROW_H),
                    RowHit::MemberOfUnion(b) => (card.band.union_rows_top + b as f32 * TIGHT_ROW_H, TIGHT_ROW_H),
                };
                local(rects, 4.0, hy, card.w - 8.0, hh, th.text.opacity(0.07));
                // Return-type chip, when the pointer is over a row with one.
                if let (RowHit::Row(r), true) = (hit, k >= LOD_ROWS) {
                    let row = &card.rows[r];
                    if !row.right.is_empty() {
                        let ty_w = mono_w(&row.right, ROW_FONT_PX);
                        let relay_w = if row.is_relay { 12.0 } else { 0.0 };
                        let left = card.w - CARD_PAD_X - ty_w - relay_w - 4.0;
                        let (x, y) = (sx(pos.x + left), sy(pos.y + card.row_y(r)));
                        let (w, h) = ((ty_w + relay_w + 8.0) * k, pitch * k);
                        fill(rects, x, y, w, h, th.type_amber.opacity(0.22));
                        outline(rects, x, y, w, h, k.max(0.5), th.type_amber.opacity(0.9));
                    }
                }
            }
        }
        if let (true, Some(t)) = (is_focused, st.ripple) {
            // An expanding, fading echo of the ring: what makes a jump to a
            // distant card readable as "here".
            let pad = t * 18.0;
            let c = kind_c.opacity((1.0 - t) * 0.6);
            let w = (2.0 * k).clamp(1.0, 3.0);
            outline(rects, sx(pos.x - pad), sy(pos.y - pad), (card.w + pad * 2.0) * k, (card.h + pad * 2.0) * k, w, c);
        }
        if is_focused || is_hovered {
            let (w_ring, a_ring) = if is_focused { (2.5, 0.75) } else { (1.5, 0.4) };
            let t = (w_ring * k).clamp(1.0, 3.5);
            let c = kind_c.opacity(a_ring);
            outline(rects, sx(pos.x - 3.0), sy(pos.y - 3.0), (card.w + 6.0) * k, (card.h + 6.0) * k, t, c);
        }
        if card.is_overlay {
            // A soft green halo: nested translucent grounds.
            for (pad, a) in [(22.0f32, 0.12f32), (13.0, 0.22), (6.0, 0.38)] {
                let c = th.overlay_green.opacity(a * dim);
                fill(rects, sx(pos.x - pad), sy(pos.y - pad), (card.w + pad * 2.0) * k, (card.h + pad * 2.0) * k, c);
            }
        }
    }
}

/// An arrowhead at `tip`, pointing along `tangent`.
fn arrow(out: &mut Vec<TriVertex>, tip: (f32, f32), tangent: (f32, f32), k: f32, color: [f32; 4]) {
    let len = (tangent.0 * tangent.0 + tangent.1 * tangent.1).sqrt().max(1e-3);
    let (ux, uy) = (tangent.0 / len, tangent.1 / len);
    let sz = 5.0f32.max(4.0 * k.min(1.0));
    let (bx, by) = (tip.0 - ux * sz * 2.0, tip.1 - uy * sz * 2.0);
    for pos in [[tip.0, tip.1], [bx - uy * sz, by + ux * sz], [bx + uy * sz, by - ux * sz]] {
        out.push(TriVertex { pos, color });
    }
}
