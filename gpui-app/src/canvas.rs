//! The schema graph canvas: GPU-painted cards + edges with pan/zoom.
//!
//! Everything is repainted from the retained `Model` each frame — no texture
//! caches, no LOD placeholder sprites, no motion gates. Those existed in the
//! web app to survive WebGL texture-upload limits; GPUI draws quads, paths and
//! glyphs directly, so culling + text LOD is all that's needed.

use crate::theme::Theme;
use crate::model::{
    fit_text, mono_w, EdgeGroup, Model, RowHit, RowKind, TypeColor, BAND_FONT_PX, CARD_PAD_X,
    DESC_FONT_PX, HEADER_PAD_X, KIND_FONT_PX, NAME_FONT_PX, ROW_FONT_PX, TIGHT_ROW_H,
};
use gompass_core::graph::NodeKind;
use gpui::{
    canvas, div, fill, point, prelude::*, px, quad, size, App, BorderStyle, Bounds,
    Context, Corners, Edges, Font, FontWeight, Hsla, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, PathBuilder, Pixels, Point, ScrollDelta, ScrollWheelEvent, SharedString,
    TextAlign, TextRun, Window,
};
use std::cell::Cell;
use std::rc::Rc;

const ZOOM_MIN: f32 = 0.01;
const ZOOM_MAX: f32 = 4.0;
const CLICK_DRAG_THRESHOLD: f32 = 4.0;
/// Below this zoom, row text gives way to placeholder bars.
const LOD_ROWS: f32 = 0.12;
/// Below this zoom, rows are not drawn at all (sub-pixel pitch).
const LOD_ROW_BARS: f32 = 0.075;
/// Below this zoom, header text is not drawn.
const LOD_HEADER: f32 = 0.03;
/// Alpha applied to everything that is not the focused card (web DIM_ALPHA).
const DIM_ALPHA: f32 = 0.1;

#[derive(Clone, Copy, PartialEq)]
pub struct ViewTransform {
    pub x: f32,
    pub y: f32,
    pub k: f32,
}

#[derive(Clone, Copy, PartialEq)]
pub struct Hover {
    pub card: u32,
    /// None = header hover.
    pub row: Option<RowHit>,
}

struct Drag {
    start: Point<Pixels>,
    orig: ViewTransform,
    moved: bool,
}

pub struct GraphCanvas {
    model: Rc<Model>,
    view: ViewTransform,
    hover: Option<Hover>,
    drag: Option<Drag>,
    fitted: bool,
    focus: Option<u32>,
    /// Center request from outside (tree selection); applied on next render,
    /// where the viewport size is known.
    pending_center: Option<u32>,
    /// Row pinned from a search hit: (card, row index).
    pinned: Option<(u32, usize)>,
    /// Window-space origin of the canvas element, recorded at paint time so
    /// event coordinates (window-relative) can be mapped into the canvas.
    canvas_origin: Rc<Cell<(f32, f32)>>,
    /// EMA of paint_scene duration, for the status bar.
    frame_ms: Rc<Cell<f32>>,
    /// Cursor position in canvas-local coords, for tooltip placement.
    hover_pos: Option<(f32, f32)>,
    /// Focus history for back navigation (⌘[).
    history: Vec<u32>,
    /// Skip the history push for the next `center_on` (set by `go_back`).
    suppress_push: bool,
    /// Investigate mode: outline types/rows lacking descriptions.
    investigate: bool,
    /// Edge under the cursor (when no card is hovered).
    hovered_edge: Option<u32>,
    /// Horizontal window-space offset of the canvas pane (sidebar width),
    /// set by the workspace so fit/center math uses the pane, not the window.
    pane_offset_x: f32,
}

impl GraphCanvas {
    pub fn new(model: Rc<Model>) -> Self {
        // Debug: GOMPASS_VIEW="x,y,k" presets the transform (skips auto-fit)
        // so selfshots can reproduce specific view states.
        let preset = std::env::var("GOMPASS_VIEW").ok().and_then(|s| {
            let mut it = s.split(',').map(|v| v.trim().parse::<f32>());
            match (it.next(), it.next(), it.next()) {
                (Some(Ok(x)), Some(Ok(y)), Some(Ok(k))) => {
                    Some(ViewTransform { x, y, k })
                }
                _ => None,
            }
        });
        Self {
            model,
            view: preset.unwrap_or(ViewTransform { x: 40.0, y: 40.0, k: 1.0 }),
            hover: None,
            drag: None,
            fitted: preset.is_some(),
            focus: None,
            pending_center: None,
            pinned: None,
            canvas_origin: Rc::new(Cell::new((0.0, 0.0))),
            frame_ms: Rc::new(Cell::new(0.0)),
            hover_pos: None,
            history: Vec::new(),
            suppress_push: false,
            investigate: false,
            hovered_edge: None,
            pane_offset_x: 300.0,
        }
    }

    pub fn set_pane_offset(&mut self, offset: f32, cx: &mut Context<Self>) {
        if (self.pane_offset_x - offset).abs() > 0.5 {
            self.pane_offset_x = offset;
            cx.notify();
        }
    }

    pub fn set_investigate(&mut self, on: bool, cx: &mut Context<Self>) {
        if self.investigate != on {
            self.investigate = on;
            cx.notify();
        }
    }

    /// Navigate back to the previously focused card.
    pub fn go_back(&mut self, cx: &mut Context<Self>) {
        if let Some(prev) = self.history.pop() {
            self.pending_center = Some(prev);
            self.suppress_push = true;
            cx.notify();
        }
    }

    pub fn history_entries(&self) -> Vec<(u32, gpui::SharedString)> {
        self.history
            .iter()
            .rev()
            .take(8)
            .map(|&i| (i, self.model.cards[i as usize].name.clone()))
            .collect()
    }

    /// Swap in a different slice of the schema (mode change).
    pub fn set_model(&mut self, model: Rc<Model>, cx: &mut Context<Self>) {
        self.model = model;
        self.fitted = false;
        self.hover = None;
        self.drag = None;
        self.focus = None;
        self.pending_center = None;
        self.pinned = None;
        self.hover_pos = None;
        self.hovered_edge = None;
        self.history.clear();
        self.suppress_push = false;
        cx.notify();
    }

    /// Called from the workspace when the tree selects a type. `row` is in
    /// graph space (field index, then enum-value index) and is mapped through
    /// the card's display-row map (primitive fields may be hidden).
    pub fn navigate_to(&mut self, card: u32, row: Option<usize>, cx: &mut Context<Self>) {
        self.pending_center = Some(card);
        self.pinned = row
            .and_then(|r| self.model.cards[card as usize].display_row(r))
            .map(|r| (card, r));
        cx.notify();
    }

    fn fit(&mut self, vw: f32, vh: f32) {
        let (gw, gh) = (self.model.world_w.max(1.0), self.model.world_h.max(1.0));
        let pad = 80.0;
        let k = ((vw - pad) / gw).min((vh - pad) / gh).min(1.2).max(ZOOM_MIN);
        // On huge graphs a full fit is an unreadable speck field — land on the
        // query root instead, zoomed in enough that text is legible.
        if k < 0.15 {
            if let Some(&root) = self.model.roots.first() {
                self.view.k = 0.8;
                self.center_on(root, vw, vh);
                return;
            }
        }
        self.view = ViewTransform {
            x: (vw - gw * k) / 2.0,
            y: (vh - gh * k) / 2.0,
            k,
        };
    }

    fn screen_to_world(&self, p: Point<Pixels>) -> (f32, f32) {
        let (ox, oy) = self.canvas_origin.get();
        (
            (f32::from(p.x) - ox - self.view.x) / self.view.k,
            (f32::from(p.y) - oy - self.view.y) / self.view.k,
        )
    }

    fn hit_test(&self, p: Point<Pixels>) -> Option<Hover> {
        let (wx, wy) = self.screen_to_world(p);
        let m = &self.model;
        for (i, card) in m.cards.iter().enumerate() {
            let pos = m.positions[i];
            if wx >= pos.x && wx <= pos.x + card.w && wy >= pos.y && wy <= pos.y + card.h {
                let row = if self.view.k >= LOD_ROWS {
                    card.hit_row(wx - pos.x, wy - pos.y)
                } else {
                    None
                };
                return Some(Hover { card: i as u32, row });
            }
        }
        None
    }

    /// Nearest edge within ~6 screen px of the cursor.
    fn hit_test_edge(&self, p: Point<Pixels>) -> Option<u32> {
        let (wx, wy) = self.screen_to_world(p);
        let threshold = 6.0 / self.view.k;
        let t2 = threshold * threshold;
        let mut best: Option<(f32, u32)> = None;
        for (i, e) in self.model.edges.iter().enumerate() {
            if wx < e.bbox[0] - threshold
                || wx > e.bbox[2] + threshold
                || wy < e.bbox[1] - threshold
                || wy > e.bbox[3] + threshold
            {
                continue;
            }
            let pts = &e.points;
            let mut j = 0;
            while j + 3 < pts.len() {
                let (x0, y0, x1, y1) = (pts[j], pts[j + 1], pts[j + 2], pts[j + 3]);
                let (dx, dy) = (x1 - x0, y1 - y0);
                let len2 = dx * dx + dy * dy;
                let t = if len2 > 0.0 {
                    (((wx - x0) * dx + (wy - y0) * dy) / len2).clamp(0.0, 1.0)
                } else {
                    0.0
                };
                let (cx_, cy_) = (x0 + dx * t, y0 + dy * t);
                let d2 = (wx - cx_) * (wx - cx_) + (wy - cy_) * (wy - cy_);
                if d2 < t2 && best.map(|(bd, _)| d2 < bd).unwrap_or(true) {
                    best = Some((d2, i as u32));
                }
                j += 4; // every other segment is plenty for hovering
            }
        }
        best.map(|(_, i)| i)
    }

    fn center_on(&mut self, card: u32, vw: f32, vh: f32) {
        if !self.suppress_push {
            if let Some(f) = self.focus {
                if f != card {
                    self.history.push(f);
                    if self.history.len() > 64 {
                        self.history.remove(0);
                    }
                }
            }
        }
        self.suppress_push = false;
        let c = &self.model.cards[card as usize];
        let p = self.model.positions[card as usize];
        let k = self.view.k.max(0.9);
        self.view = ViewTransform {
            x: vw / 2.0 - (p.x + c.w / 2.0) * k,
            y: vh / 2.0 - (p.y + c.h / 2.0) * k,
            k,
        };
        self.focus = Some(card);
    }

    fn on_scroll(&mut self, ev: &ScrollWheelEvent, window: &mut Window, cx: &mut Context<Self>) {
        let _ = window;
        // Like the web app: scroll/wheel zooms (anchored at the cursor);
        // hold shift to pan. Dragging pans as well.
        if ev.modifiers.shift {
            if let ScrollDelta::Pixels(d) = ev.delta {
                self.view.x += f32::from(d.x);
                self.view.y += f32::from(d.y);
                cx.notify();
                return;
            }
        }
        let dy = match ev.delta {
            ScrollDelta::Pixels(d) => f32::from(d.y),
            ScrollDelta::Lines(d) => d.y * 20.0,
        };
        // smooth exponential zoom: ±100px of scroll ≈ ×/÷ 1.4
        let ratio = 2f32.powf(dy / 200.0);
        let new_k = (self.view.k * ratio).clamp(ZOOM_MIN, ZOOM_MAX);
        let ratio = new_k / self.view.k;
        let (ox, oy) = self.canvas_origin.get();
        let (mx, my) = (f32::from(ev.position.x) - ox, f32::from(ev.position.y) - oy);
        self.view.x = mx - (mx - self.view.x) * ratio;
        self.view.y = my - (my - self.view.y) * ratio;
        self.view.k = new_k;
        cx.notify();
    }

    fn on_mouse_down(&mut self, ev: &MouseDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        self.drag = Some(Drag { start: ev.position, orig: self.view, moved: false });
        cx.notify();
    }

    fn on_mouse_move(&mut self, ev: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(drag) = &mut self.drag {
            let dx = f32::from(ev.position.x - drag.start.x);
            let dy = f32::from(ev.position.y - drag.start.y);
            if dx.abs() + dy.abs() > CLICK_DRAG_THRESHOLD {
                drag.moved = true;
            }
            if drag.moved {
                self.view.x = drag.orig.x + dx;
                self.view.y = drag.orig.y + dy;
                cx.notify();
            }
        } else {
            let hover = self.hit_test(ev.position);
            let hovered_edge = if hover.is_none() {
                self.hit_test_edge(ev.position)
            } else {
                None
            };
            let (ox, oy) = self.canvas_origin.get();
            let pos = (f32::from(ev.position.x) - ox, f32::from(ev.position.y) - oy);
            let moved = match self.hover_pos {
                Some((px_, py_)) => (px_ - pos.0).abs() + (py_ - pos.1).abs() > 2.0,
                None => true,
            };
            if hover != self.hover
                || hovered_edge != self.hovered_edge
                || ((hover.is_some() || hovered_edge.is_some()) && moved)
            {
                self.hover = hover;
                self.hovered_edge = hovered_edge;
                self.hover_pos = Some(pos);
                cx.notify();
            }
        }
    }

    fn on_mouse_up(&mut self, ev: &MouseUpEvent, window: &mut Window, cx: &mut Context<Self>) {
        let was_click = matches!(&self.drag, Some(d) if !d.moved);
        self.drag = None;
        if was_click {
            if let Some(hit) = self.hit_test(ev.position) {
                let vw = f32::from(window.viewport_size().width) - self.pane_offset_x;
                let vh = f32::from(window.viewport_size().height);
                match hit.row {
                    Some(RowHit::Row(row)) => {
                        // Clicking the right-aligned type navigates; clicking
                        // the rest of the row pins it (the web split).
                        let card = &self.model.cards[hit.card as usize];
                        let r = &card.rows[row];
                        let (wx, _) = self.screen_to_world(ev.position);
                        let local_x = wx - self.model.positions[hit.card as usize].x;
                        let rt_w = mono_w(&r.right, ROW_FONT_PX);
                        let on_type = !r.right.is_empty()
                            && local_x >= card.w - CARD_PAD_X - rt_w - 16.0;
                        let navigate = r.target.filter(|_| on_type || r.right.is_empty());
                        match navigate {
                            Some(t) => self.center_on(t, vw, vh),
                            None => {
                                self.pinned = Some((hit.card, row));
                                self.focus = Some(hit.card);
                            }
                        }
                    }
                    Some(RowHit::Implements(b)) => {
                        let name = self.model.cards[hit.card as usize].implements[b].clone();
                        if let Some(&t) = self.model.index_of.get(name.as_ref()) {
                            self.center_on(t, vw, vh);
                        }
                    }
                    Some(RowHit::MemberOfUnion(b)) => {
                        let name =
                            self.model.cards[hit.card as usize].member_of_unions[b].clone();
                        if let Some(&t) = self.model.index_of.get(name.as_ref()) {
                            self.center_on(t, vw, vh);
                        }
                    }
                    None => self.center_on(hit.card, vw, vh),
                }
            }
        }
        cx.notify();
    }

    fn status_line(&self) -> String {
        let m = &self.model;
        let mut base = format!(
            "{}  ·  {} types  ·  {} edges  ·  {:.0}%",
            m.schema_name,
            m.cards.len(),
            m.edges.len(),
            self.view.k * 100.0
        );
        if m.overlay_marks > 0 {
            base.push_str(&format!("  ·  overlay +{}", m.overlay_marks));
        }
        let ms = self.frame_ms.get();
        if ms > 0.0 {
            base.push_str(&format!("  ·  {ms:.1}ms"));
        }
        if self.investigate {
            let (documented, total) = m.desc_coverage;
            let pct = if total > 0 { documented * 100 / total } else { 100 };
            base.push_str(&format!("  ·  desc {documented}/{total} ({pct}%)"));
        }
        match self.hover {
            Some(Hover { card, row: Some(RowHit::Row(row)) }) => {
                let c = &m.cards[card as usize];
                let r = &c.rows[row];
                let mut line = if r.right.is_empty() {
                    format!("{base}   —   {}::{}", c.name, r.left)
                } else {
                    format!("{base}   —   {}.{}: {}", c.name, r.left, r.right)
                };
                if let Some(desc) = &r.description {
                    let one_line: String = desc.split_whitespace().collect::<Vec<_>>().join(" ");
                    let excerpt: String = one_line.chars().take(120).collect();
                    let ellipsis = if one_line.chars().count() > 120 { "…" } else { "" };
                    line.push_str(&format!("   “{excerpt}{ellipsis}”"));
                }
                line
            }
            Some(Hover { card, row: Some(RowHit::Implements(b)) }) => {
                let c = &m.cards[card as usize];
                format!("{base}   —   {} implements {}", c.name, c.implements[b])
            }
            Some(Hover { card, row: Some(RowHit::MemberOfUnion(b)) }) => {
                let c = &m.cards[card as usize];
                format!("{base}   —   {} in union {}", c.name, c.member_of_unions[b])
            }
            Some(Hover { card, row: None }) => {
                let c = &m.cards[card as usize];
                format!("{base}   —   {} {}", c.kind_label, c.name)
            }
            None => base,
        }
    }
}

fn edge_color(th: &Theme, group: EdgeGroup) -> Hsla {
    let c: Hsla = match group {
        EdgeGroup::FieldNonNull | EdgeGroup::FieldNullable => th.kind_color(NodeKind::Object),
        EdgeGroup::Union => th.kind_color(NodeKind::Union),
        EdgeGroup::Implements => th.kind_color(NodeKind::Interface),
        EdgeGroup::Arg => th.arg_orange,
    };
    match group {
        EdgeGroup::FieldNullable => c.opacity(0.45),
        EdgeGroup::Implements => c.opacity(0.55),
        EdgeGroup::Arg => c.opacity(0.55),
        _ => c.opacity(0.85),
    }
}

fn mono(weight: FontWeight) -> Font {
    let mut f = gpui::font("Menlo");
    f.weight = weight;
    f
}

fn run(len: usize, font: &Font, color: Hsla) -> TextRun {
    TextRun {
        len,
        font: font.clone(),
        color,
        background_color: None,
        underline: None,
        strikethrough: None,
    }
}

impl Render for GraphCanvas {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let vw = f32::from(window.viewport_size().width) - self.pane_offset_x;
        let vh = f32::from(window.viewport_size().height);
        if !self.fitted && vw > 0.0 {
            self.fit(vw, vh);
            self.fitted = true;
        }
        if let Some(card) = self.pending_center.take() {
            self.center_on(card, vw, vh);
        }

        let model = self.model.clone();
        let view = self.view;
        let hover = self.hover;
        let focus = self.focus;
        let pinned = self.pinned;
        let canvas_origin = self.canvas_origin.clone();
        let frame_ms = self.frame_ms.clone();

        // Floating tooltip: field/header description + deprecation info.
        struct Tooltip {
            title: String,
            description: Option<String>,
            deprecation: Option<String>,
            expired: bool,
        }
        let tooltip: Option<Tooltip> = if self.drag.is_some() {
            None
        } else {
            self.hover.and_then(|h| {
                let card = &self.model.cards[h.card as usize];
                match h.row {
                    Some(RowHit::Row(ri)) => {
                        let row = &card.rows[ri];
                        let title = if row.right.is_empty() {
                            format!("{}::{}", card.name, row.left)
                        } else {
                            format!("{}.{}: {}", card.name, row.left, row.right)
                        };
                        let deprecation = row
                            .deprecation_reason
                            .clone()
                            .or_else(|| row.deprecated.then(|| "deprecated".to_string()));
                        (row.description.is_some() || deprecation.is_some()).then_some(Tooltip {
                            title,
                            description: row.description.clone(),
                            deprecation,
                            expired: row.until_expired,
                        })
                    }
                    Some(RowHit::Implements(b)) => Some(Tooltip {
                        title: format!("implements {}", card.implements[b]),
                        description: None,
                        deprecation: None,
                        expired: false,
                    }),
                    Some(RowHit::MemberOfUnion(b)) => Some(Tooltip {
                        title: format!("member of union {}", card.member_of_unions[b]),
                        description: None,
                        deprecation: None,
                        expired: false,
                    }),
                    None => card.description.clone().map(|d| Tooltip {
                        title: format!("{} {}", card.kind_label, card.name),
                        description: Some(d),
                        deprecation: None,
                        expired: false,
                    }),
                }
            })
        };
        // Edge tooltip: source → target plus the bundled field labels.
        let tooltip = tooltip.or_else(|| {
            let ei = self.hovered_edge?;
            let e = self.model.edges.get(ei as usize)?;
            let from = &self.model.cards[e.from as usize].name;
            let to = &self.model.cards[e.to as usize].name;
            let shown: Vec<String> = e.labels.iter().take(10).map(|l| l.to_string()).collect();
            let more = e.labels.len().saturating_sub(10);
            let mut desc = shown.join(", ");
            if more > 0 {
                desc.push_str(&format!(" … +{more}"));
            }
            let bundle = if e.bundled > 1 {
                format!("  ·  ×{}", e.bundled)
            } else {
                String::new()
            };
            Some(Tooltip {
                title: format!("{from} → {to}{bundle}"),
                description: (!desc.is_empty()).then_some(desc),
                deprecation: None,
                expired: false,
            })
        });
        let hover_pos = self.hover_pos;
        let investigate = self.investigate;
        let hovered_edge = self.hovered_edge;
        let th = crate::theme::theme(window.appearance());
        let bg = th.bg;
        let status = self.status_line();

        div()
            .size_full()
            .relative()
            .bg(bg)
            .on_scroll_wheel(cx.listener(|this, ev, window, cx| this.on_scroll(ev, window, cx)))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, ev, window, cx| this.on_mouse_down(ev, window, cx)),
            )
            .on_mouse_move(cx.listener(|this, ev, window, cx| this.on_mouse_move(ev, window, cx)))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, ev, window, cx| this.on_mouse_up(ev, window, cx)),
            )
            .child(
                canvas(
                    |_, _, _| (),
                    move |bounds, _, window, cx| {
                        canvas_origin
                            .set((f32::from(bounds.origin.x), f32::from(bounds.origin.y)));
                        let t0 = std::time::Instant::now();
                        // Clip all canvas painting to the element bounds —
                        // paint_layer orders, only a content mask clips.
                        window.with_content_mask(
                            Some(gpui::ContentMask { bounds }),
                            |window| {
                                paint_scene(
                                    &model, view, hover, focus, pinned, investigate,
                                    hovered_edge, bounds, window, cx,
                                );
                            },
                        );
                        let ms = t0.elapsed().as_secs_f32() * 1000.0;
                        let prev = frame_ms.get();
                        frame_ms.set(if prev == 0.0 { ms } else { prev * 0.8 + ms * 0.2 });
                    },
                )
                .size_full(),
            )
            .child(
                div()
                    .absolute()
                    .bottom_2()
                    .left_2()
                    .px_3()
                    .py_1()
                    .rounded_md()
                    .bg(th.chrome_bg)
                    .shadow_md()
                    .text_color(th.text_muted)
                    .text_xs()
                    .font_family("Menlo")
                    .child(SharedString::from(status)),
            )
            .when_some(tooltip.zip(hover_pos), |el, (tip, (hx, hy))| {
                let tw = 340.0;
                let pane_w = vw;
                let left = if hx + 16.0 + tw > pane_w {
                    (hx - tw - 12.0).max(4.0)
                } else {
                    hx + 16.0
                };
                let top = if hy + 160.0 > vh {
                    (hy - 140.0).max(4.0)
                } else {
                    hy + 18.0
                };
                el.child(
                    div()
                        .absolute()
                        .left(px(left))
                        .top(px(top))
                        .w(px(tw))
                        .p_2()
                        .rounded_md()
                        .border_1()
                        .border_color(th.card_border)
                        .bg(th.chrome_bg)
                        .shadow_lg()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .font_family("Menlo")
                        .child(
                            div()
                                .text_xs()
                                .text_color(th.text)
                                .child(SharedString::from(tip.title)),
                        )
                        .when_some(tip.description, |el, d| {
                            el.child(
                                div()
                                    .text_xs()
                                    .text_color(th.text_muted)
                                    .max_h(px(120.0))
                                    .overflow_hidden()
                                    .child(SharedString::from(d)),
                            )
                        })
                        .when_some(tip.deprecation, |el, d| {
                            let color: Hsla = if tip.expired {
                                th.red
                            } else {
                                th.type_amber
                            };
                            el.child(
                                div()
                                    .text_xs()
                                    .text_color(color)
                                    .child(SharedString::from(format!("⚠ {d}"))),
                            )
                        }),
                )
            })
    }
}

fn paint_scene(
    model: &Model,
    view: ViewTransform,
    hover: Option<Hover>,
    focus: Option<u32>,
    pinned: Option<(u32, usize)>,
    investigate: bool,
    hovered_edge: Option<u32>,
    bounds: Bounds<Pixels>,
    window: &mut Window,
    cx: &mut App,
) {
    let th = crate::theme::theme(window.appearance());
    let k = view.k;
    let ox = f32::from(bounds.origin.x) + view.x;
    let oy = f32::from(bounds.origin.y) + view.y;
    let vw = f32::from(bounds.size.width);
    let vh = f32::from(bounds.size.height);
    // visible world rect
    let wx0 = (0.0 - view.x) / k;
    let wy0 = (0.0 - view.y) / k;
    let wx1 = (vw - view.x) / k;
    let wy1 = (vh - view.y) / k;

    let to_screen = |x: f32, y: f32| point(px(ox + x * k), px(oy + y * k));

    // ---- edges, one stroked path per color group ----
    let groups = [
        EdgeGroup::FieldNonNull,
        EdgeGroup::FieldNullable,
        EdgeGroup::Union,
        EdgeGroup::Implements,
        EdgeGroup::Arg,
    ];
    let stroke_w = (1.5 * k).clamp(0.6, 2.5);
    // Zoomed out, bezier detail is subpixel: sample the flattened polyline at
    // a stride (straight-ish lines) so tessellation stays cheap at overview
    // zoom, where every one of the ~5k edges is on screen.
    let stride: usize = if k < 0.05 {
        8
    } else if k < 0.15 {
        4
    } else if k < 0.35 {
        2
    } else {
        1
    };
    let draw_arrows = k >= 0.3;
    // Edges live in their own layer so cards (next layer) always draw above
    // them — within a single layer GPUI batches by primitive type, which
    // does not guarantee paths-under-quads.
    window.paint_layer(bounds, |window| {
    // With a focused card, incident edges stay bright and the rest dim —
    // the web app's focus behavior, minus its re-tessellation dodge.
    for (group, dim_pass) in groups.iter().flat_map(|&g| [(g, false), (g, true)]) {
        let mut builder = PathBuilder::stroke(px(stroke_w));
        let mut any = false;
        let mut arrows = PathBuilder::fill();
        let mut any_arrow = false;
        for (ei, e) in model.edges.iter().enumerate() {
            if e.group != group {
                continue;
            }
            if Some(ei as u32) == hovered_edge {
                continue; // painted separately, highlighted
            }
            // With a focus, everything non-incident dims; without one, only
            // hub-star edges dim (the web app's hub fading).
            let dimmed = match focus {
                Some(f) => e.from != f && e.to != f,
                None => e.hub_faded,
            };
            if dimmed != dim_pass {
                continue;
            }
            if e.bbox[2] < wx0 || e.bbox[0] > wx1 || e.bbox[3] < wy0 || e.bbox[1] > wy1 {
                continue;
            }
            let pts = &e.points;
            builder.move_to(to_screen(pts[0], pts[1]));
            let mut i = 2 * stride;
            while i + 1 < pts.len() {
                builder.line_to(to_screen(pts[i], pts[i + 1]));
                i += 2 * stride;
            }
            let n = pts.len();
            builder.line_to(to_screen(pts[n - 2], pts[n - 1]));
            any = true;
            if !draw_arrows {
                continue;
            }
            // arrowhead: screen-space triangle at the end, oriented by the
            // last polyline segment
            let (ex, ey) = (pts[n - 2], pts[n - 1]);
            let (px_, py_) = (pts[n - 4], pts[n - 3]);
            let (dx, dy) = (ex - px_, ey - py_);
            let len = (dx * dx + dy * dy).sqrt().max(1e-3);
            let (ux, uy) = (dx / len, dy / len);
            let s = 5.0f32.max(4.0 * k.min(1.0));
            let tip = to_screen(ex, ey);
            let bx = f32::from(tip.x) - ux * s * 2.0;
            let by = f32::from(tip.y) - uy * s * 2.0;
            arrows.move_to(tip);
            arrows.line_to(point(px(bx - uy * s), px(by + ux * s)));
            arrows.line_to(point(px(bx + uy * s), px(by - ux * s)));
            arrows.close();
            any_arrow = true;
        }
        let color = if dim_pass {
            edge_color(&th, group).opacity(0.22)
        } else {
            edge_color(&th, group)
        };
        if any {
            if let Ok(path) = builder.build() {
                window.paint_path(path, color);
            }
        }
        if any_arrow {
            if let Ok(path) = arrows.build() {
                window.paint_path(path, color);
            }
        }
    }
    // The hovered edge draws on top, brighter and thicker.
    if let Some(ei) = hovered_edge {
        if let Some(e) = model.edges.get(ei as usize) {
            let mut builder = PathBuilder::stroke(px((stroke_w * 2.0).max(2.0)));
            let pts = &e.points;
            builder.move_to(to_screen(pts[0], pts[1]));
            let mut i = 2;
            while i + 1 < pts.len() {
                builder.line_to(to_screen(pts[i], pts[i + 1]));
                i += 2;
            }
            if let Ok(path) = builder.build() {
                window.paint_path(path, edge_color(&th, e.group).opacity(1.0));
            }
        }
    }
    });

    // ---- nodes (their own layer, above the edges) ----
    // Geometry, colors and draw order mirror the web renderer's
    // `drawNodeSprite` (see UI-PARITY.md §1). Layout math stays in world
    // units via `mono_w`; only glyph shaping happens at the screen scale.
    let name_font = mono(FontWeight::SEMIBOLD);
    let row_font = mono(FontWeight::NORMAL);
    let mut italic_font = mono(FontWeight::NORMAL);
    italic_font.style = gpui::FontStyle::Italic;
    let text_system = window.text_system().clone();
    // Text is shaped at a quantized zoom (√2/4 ladder, ~9% steps) so that a
    // continuous zoom gesture hits the text system's layout cache instead of
    // re-shaping every visible glyph run each frame.
    let kt = 2f32.powf((k.log2() * 8.0).round() / 8.0);
    let mut text_errors = 0usize;

    window.paint_layer(bounds, |window| {
    for (i, card) in model.cards.iter().enumerate() {
        let pos = model.positions[i];
        if pos.x + card.w < wx0 || pos.x > wx1 || pos.y + card.h < wy0 || pos.y > wy1 {
            continue;
        }
        let ci = i as u32;
        let dim = match focus {
            Some(f) if f != ci => DIM_ALPHA,
            _ => 1.0,
        };
        let kind_c = th.kind_color(card.kind);
        let is_hovered = matches!(hover, Some(h) if h.card == ci);
        let is_focused = focus == Some(ci);
        let radius = px(6.0 * k);
        let origin = to_screen(pos.x, pos.y);
        let card_bounds = Bounds {
            origin,
            size: size(px(card.w * k), px(card.h * k)),
        };
        let rect = |x: f32, y: f32, w: f32, h: f32| Bounds {
            origin: to_screen(pos.x + x, pos.y + y),
            size: size(px((w * k).max(0.5)), px((h * k).max(0.5))),
        };

        // card body + kind border (overlay types get the emerald dashed ring)
        let (border_color, border_w, border_style) = if card.is_overlay {
            (th.overlay_green.opacity(dim), 2.0, BorderStyle::Dashed)
        } else {
            (kind_c.opacity(0.75 * dim), 1.25, BorderStyle::Solid)
        };
        window.paint_quad(quad(
            card_bounds,
            Corners::all(radius),
            th.card_bg.opacity(dim),
            Edges::all(px((border_w * k).clamp(0.5, 3.0))),
            border_color,
            border_style,
        ));

        // header band — kind color at full opacity, square bottom corners
        window.paint_quad(quad(
            Bounds {
                origin,
                size: size(px(card.w * k), px(card.header_h * k)),
            },
            Corners {
                top_left: radius,
                top_right: radius,
                bottom_left: px(0.0),
                bottom_right: px(0.0),
            },
            kind_c.opacity(dim),
            Edges::default(),
            gpui::transparent_black(),
            BorderStyle::Solid,
        ));
        window.paint_quad(fill(
            rect(0.0, card.header_h, card.w, 0.75),
            kind_c.opacity(0.4 * dim),
        ));

        // trailing wash bands: violet = implements, amber = member of union
        if !card.implements.is_empty() {
            let c = th.kind_color(NodeKind::Interface);
            let (t, b) = (card.band.iface_band_top, card.band.iface_band_bottom);
            window.paint_quad(fill(rect(0.0, t, card.w, b - t), c.opacity(0.1 * dim)));
            window.paint_quad(fill(rect(0.0, t, card.w, 0.5), c.opacity(0.4 * dim)));
        }
        if !card.member_of_unions.is_empty() {
            let c = th.kind_color(NodeKind::Union);
            let t = card.band.union_band_top;
            window.paint_quad(fill(rect(0.0, t, card.w, card.h - t), c.opacity(0.1 * dim)));
            window.paint_quad(fill(rect(0.0, t, card.w, 0.5), c.opacity(0.4 * dim)));
        }

        // ---- header text ----
        if k >= LOD_HEADER {
            text_errors += paint_baseline(
                &text_system,
                card.kind_upper.into(),
                &name_font,
                px(KIND_FONT_PX * kt),
                gpui::white().opacity(0.6 * dim),
                None,
                to_screen(pos.x + HEADER_PAD_X, 0.0).x.into(),
                to_screen(0.0, pos.y + 14.0).y.into(),
                window,
                cx,
            );
            if card.is_overlay {
                let tag_x = pos.x + card.w - HEADER_PAD_X - mono_w("OVERLAY", 8.0);
                text_errors += paint_baseline(
                    &text_system,
                    "OVERLAY".into(),
                    &name_font,
                    px(8.0 * kt),
                    gpui::white().opacity(0.9 * dim),
                    None,
                    to_screen(tag_x, 0.0).x.into(),
                    to_screen(0.0, pos.y + 14.0).y.into(),
                    window,
                    cx,
                );
            }
            let name = fit_text(&card.name, NAME_FONT_PX, card.w - HEADER_PAD_X * 2.0);
            text_errors += paint_baseline(
                &text_system,
                name.into(),
                &name_font,
                px(NAME_FONT_PX * kt),
                gpui::white().opacity(dim),
                None,
                to_screen(pos.x + HEADER_PAD_X, 0.0).x.into(),
                to_screen(0.0, pos.y + 30.0).y.into(),
                window,
                cx,
            );
            if let Some(d) = &card.header_desc {
                text_errors += paint_baseline(
                    &text_system,
                    d.clone(),
                    &row_font,
                    px(DESC_FONT_PX * kt),
                    gpui::white().opacity(0.75 * dim),
                    None,
                    to_screen(pos.x + HEADER_PAD_X, 0.0).x.into(),
                    to_screen(0.0, pos.y + 42.0).y.into(),
                    window,
                    cx,
                );
            }
        } else {
            // Text-shaped presence survives every zoom: a name bar in the
            // header band even when real glyphs would be sub-pixel.
            let name_w = mono_w(&card.name, NAME_FONT_PX).min(card.w * 0.7);
            window.paint_quad(fill(
                rect(HEADER_PAD_X, card.header_h * 0.45, name_w, NAME_FONT_PX * 0.4),
                gpui::white().opacity(0.55 * dim),
            ));
        }

        // ---- body rows ----
        let pitch = card.body_pitch();
        if k >= LOD_ROWS {
            for (ri, row) in card.rows.iter().enumerate() {
                let fy = pos.y + card.row_baseline(ri);
                let name_w = mono_w(&row.left, ROW_FONT_PX);
                match row.kind {
                    RowKind::Field => {
                        let dep_a = if row.deprecated && !row.until_expired { 0.4 } else { 1.0 };
                        let name_c = if row.until_expired { th.expired } else { th.text };
                        let strike = row.deprecated.then_some(name_c.opacity(dep_a * dim));
                        text_errors += paint_baseline(
                            &text_system,
                            row.left.clone(),
                            &row_font,
                            px(ROW_FONT_PX * kt),
                            name_c.opacity(dep_a * dim),
                            strike,
                            to_screen(pos.x + CARD_PAD_X, 0.0).x.into(),
                            to_screen(0.0, fy).y.into(),
                            window,
                            cx,
                        );
                        if !row.right.is_empty() {
                            let relay_pad = if row.is_relay { 20.0 } else { 0.0 };
                            let max_w =
                                (card.w - 20.0 - name_w - relay_pad - 8.0).max(40.0);
                            let ty = fit_text(&row.right, ROW_FONT_PX, max_w);
                            let ty_w = mono_w(&ty, ROW_FONT_PX);
                            let (ty_c, ty_a) = match row.type_color {
                                TypeColor::Expired => (th.expired, 1.0),
                                TypeColor::BuiltinScalar => (th.type_builtin, 0.7),
                                TypeColor::Normal => (th.type_amber, 1.0),
                            };
                            let tx = pos.x + card.w - CARD_PAD_X - ty_w;
                            text_errors += paint_baseline(
                                &text_system,
                                ty.into(),
                                &row_font,
                                px(ROW_FONT_PX * kt),
                                ty_c.opacity(ty_a * dep_a * dim),
                                strike,
                                to_screen(tx, 0.0).x.into(),
                                to_screen(0.0, fy).y.into(),
                                window,
                                cx,
                            );
                            if row.is_relay {
                                // 8px chain glyph, drawn as two linked bars
                                let cx_ = tx - 8.0;
                                let cy = fy - 2.0;
                                let c = th.relay_orange.opacity(0.85 * dim);
                                for dx in [-4.0f32, -0.5] {
                                    window.paint_quad(quad(
                                        rect(cx_ - pos.x + dx, cy - pos.y - 1.5, 4.5, 3.0),
                                        Corners::all(px(1.5 * k)),
                                        gpui::transparent_black(),
                                        Edges::all(px((1.0 * k).max(0.5))),
                                        c,
                                        BorderStyle::Solid,
                                    ));
                                }
                            }
                        }
                    }
                    RowKind::EnumValue => {
                        // muted, struck only when the sunset date passed, never faded
                        let c = if row.until_expired { th.expired } else { th.text_muted };
                        let strike = row.until_expired.then_some(th.expired.opacity(dim));
                        text_errors += paint_baseline(
                            &text_system,
                            row.left.clone(),
                            &row_font,
                            px(ROW_FONT_PX * kt),
                            c.opacity(dim),
                            strike,
                            to_screen(pos.x + CARD_PAD_X, 0.0).x.into(),
                            to_screen(0.0, fy).y.into(),
                            window,
                            cx,
                        );
                    }
                    RowKind::UnionMember => {
                        text_errors += paint_baseline(
                            &text_system,
                            row.left.clone(),
                            &row_font,
                            px(ROW_FONT_PX * kt),
                            th.kind_color(NodeKind::Object).opacity(dim),
                            None,
                            to_screen(pos.x + CARD_PAD_X, 0.0).x.into(),
                            to_screen(0.0, fy).y.into(),
                            window,
                            cx,
                        );
                    }
                }
                if row.is_overlay {
                    window.paint_quad(fill(
                        rect(3.0, card.row_baseline(ri) - 7.0, 2.0, 9.0),
                        th.overlay_green.opacity(dim),
                    ));
                }
                if let Some(d) = &row.description_line {
                    text_errors += paint_baseline(
                        &text_system,
                        d.clone(),
                        &row_font,
                        px(DESC_FONT_PX * kt),
                        th.text_muted.opacity(0.7 * dim),
                        None,
                        to_screen(pos.x + CARD_PAD_X, 0.0).x.into(),
                        to_screen(0.0, fy + 11.0).y.into(),
                        window,
                        cx,
                    );
                }
            }
            if card.kind == NodeKind::Scalar && card.rows.is_empty() {
                text_errors += paint_baseline(
                    &text_system,
                    "custom scalar".into(),
                    &italic_font,
                    px(ROW_FONT_PX * kt),
                    th.text_muted.opacity(dim),
                    None,
                    to_screen(pos.x + CARD_PAD_X, 0.0).x.into(),
                    to_screen(0.0, pos.y + card.body_top() + 10.0).y.into(),
                    window,
                    cx,
                );
            }
            // band text — one name per line, no prefix; the band color carries
            // the meaning (matching the web renderer).
            for (bi, name) in card.implements.iter().enumerate() {
                let by = card.band.iface_rows_top + bi as f32 * TIGHT_ROW_H + 10.0;
                text_errors += paint_baseline(
                    &text_system,
                    fit_text(name, BAND_FONT_PX, card.w - 20.0).into(),
                    &name_font,
                    px(BAND_FONT_PX * kt),
                    th.kind_color(NodeKind::Interface).opacity(dim),
                    None,
                    to_screen(pos.x + CARD_PAD_X, 0.0).x.into(),
                    to_screen(0.0, pos.y + by).y.into(),
                    window,
                    cx,
                );
            }
            for (bi, name) in card.member_of_unions.iter().enumerate() {
                let by = card.band.union_rows_top + bi as f32 * TIGHT_ROW_H + 10.0;
                text_errors += paint_baseline(
                    &text_system,
                    fit_text(name, BAND_FONT_PX, card.w - 20.0).into(),
                    &name_font,
                    px(BAND_FONT_PX * kt),
                    th.kind_color(NodeKind::Union).opacity(dim),
                    None,
                    to_screen(pos.x + CARD_PAD_X, 0.0).x.into(),
                    to_screen(0.0, pos.y + by).y.into(),
                    window,
                    cx,
                );
            }
        } else {
            // Placeholder bars keep every card text-shaped at any zoom.
            let bar_h = (ROW_FONT_PX * k).max(0.6);
            for (ri, row) in card.rows.iter().enumerate() {
                let by = card.row_y(ri) + (pitch - ROW_FONT_PX) / 2.0;
                let left_w = mono_w(&row.left, ROW_FONT_PX).min(card.w * 0.55);
                window.paint_quad(fill(
                    Bounds {
                        origin: to_screen(pos.x + CARD_PAD_X, pos.y + by),
                        size: size(px(left_w * k), px(bar_h)),
                    },
                    th.text_muted.opacity(0.3 * dim),
                ));
                if !row.right.is_empty() {
                    let right_w = mono_w(&row.right, ROW_FONT_PX).min(card.w * 0.4);
                    window.paint_quad(fill(
                        Bounds {
                            origin: to_screen(pos.x + card.w - CARD_PAD_X - right_w, pos.y + by),
                            size: size(px(right_w * k), px(bar_h)),
                        },
                        th.type_amber.opacity(0.25 * dim),
                    ));
                }
            }
        }

        // ---- interaction overlays (painted above the card, like the web) ----
        if investigate {
            if card.description.is_none() {
                window.paint_quad(quad(
                    Bounds {
                        origin: to_screen(pos.x - 4.0, pos.y - 4.0),
                        size: size(px((card.w + 8.0) * k), px((card.h + 8.0) * k)),
                    },
                    Corners::all(px(8.0 * k)),
                    gpui::transparent_black(),
                    Edges::all(px((3.0 * k).clamp(1.0, 4.0))),
                    th.investigate.opacity(0.95),
                    BorderStyle::Solid,
                ));
            }
            if k >= LOD_ROWS {
                for (ri, row) in card.rows.iter().enumerate() {
                    if matches!(row.kind, RowKind::Field | RowKind::EnumValue)
                        && row.description.is_none()
                    {
                        window.paint_quad(fill(
                            rect(4.0, card.row_y(ri), card.w - 8.0, pitch),
                            th.investigate.opacity(0.22),
                        ));
                    }
                }
            }
        }
        if let Some((pc, row)) = pinned {
            if pc == ci && row < card.rows.len() {
                window.paint_quad(quad(
                    rect(2.0, card.row_y(row) - 2.0, card.w - 4.0, pitch + 4.0),
                    Corners::all(px(4.0 * k)),
                    th.pin.opacity(0.18),
                    Edges::all(px((2.0 * k).clamp(1.0, 3.0))),
                    th.pin.opacity(0.95),
                    BorderStyle::Solid,
                ));
            }
        }
        if let Some(Hover { card: hc, row: Some(hit) }) = hover {
            if hc == ci {
                let (hy, hh) = match hit {
                    RowHit::Row(r) => (card.row_y(r), pitch),
                    RowHit::Implements(b) => (
                        card.band.iface_rows_top + b as f32 * TIGHT_ROW_H,
                        TIGHT_ROW_H,
                    ),
                    RowHit::MemberOfUnion(b) => (
                        card.band.union_rows_top + b as f32 * TIGHT_ROW_H,
                        TIGHT_ROW_H,
                    ),
                };
                window.paint_quad(quad(
                    rect(4.0, hy, card.w - 8.0, hh),
                    Corners::all(px(3.0 * k)),
                    th.text.opacity(0.07),
                    Edges::default(),
                    gpui::transparent_black(),
                    BorderStyle::Solid,
                ));
                // return-type chip, when the cursor is over the type label
                if let (RowHit::Row(r), true) = (hit, k >= LOD_ROWS) {
                    let row = &card.rows[r];
                    if !row.right.is_empty() {
                        let ty_w = mono_w(&row.right, ROW_FONT_PX);
                        let relay_w = if row.is_relay { 12.0 } else { 0.0 };
                        let rt_left = card.w - CARD_PAD_X - ty_w - relay_w - 4.0;
                        window.paint_quad(quad(
                            rect(rt_left, card.row_y(r), ty_w + relay_w + 8.0, pitch),
                            Corners::all(px(3.0 * k)),
                            th.type_amber.opacity(0.22),
                            Edges::all(px((1.0 * k).max(0.5))),
                            th.type_amber.opacity(0.9),
                            BorderStyle::Solid,
                        ));
                    }
                }
            }
        }
        if is_focused || (is_hovered && !is_focused) {
            let (w_ring, a_ring) = if is_focused { (2.5, 0.75) } else { (1.5, 0.4) };
            window.paint_quad(quad(
                Bounds {
                    origin: to_screen(pos.x - 3.0, pos.y - 3.0),
                    size: size(px((card.w + 6.0) * k), px((card.h + 6.0) * k)),
                },
                Corners::all(px(9.0 * k)),
                gpui::transparent_black(),
                Edges::all(px((w_ring * k).clamp(1.0, 3.5))),
                kind_c.opacity(a_ring),
                BorderStyle::Solid,
            ));
        }
        if card.is_overlay {
            for (pad, r, a) in [(22.0f32, 26.0f32, 0.12f32), (13.0, 18.0, 0.22), (6.0, 11.0, 0.38)] {
                window.paint_quad(quad(
                    Bounds {
                        origin: to_screen(pos.x - pad, pos.y - pad),
                        size: size(px((card.w + pad * 2.0) * k), px((card.h + pad * 2.0) * k)),
                    },
                    Corners::all(px(r * k)),
                    th.overlay_green.opacity(a * dim),
                    Edges::default(),
                    gpui::transparent_black(),
                    BorderStyle::Solid,
                ));
            }
        }
    }
    });
    if text_errors > 0 {
        eprintln!("canvas: {text_errors} text paint errors this frame");
    }
}

/// Paints one shaped line with its BASELINE at `baseline` (screen px), the
/// way the web's canvas `fillText` positions text.
#[allow(clippy::too_many_arguments)]
fn paint_baseline(
    ts: &std::sync::Arc<gpui::WindowTextSystem>,
    text: SharedString,
    font: &Font,
    size_px: Pixels,
    color: Hsla,
    strike: Option<Hsla>,
    x: f32,
    baseline: f32,
    window: &mut Window,
    cx: &mut App,
) -> usize {
    if text.is_empty() {
        return 0;
    }
    let run = TextRun {
        len: text.len(),
        font: font.clone(),
        color,
        background_color: None,
        underline: None,
        strikethrough: strike.map(|c| gpui::StrikethroughStyle {
            thickness: px(0.75),
            color: Some(c),
        }),
    };
    let line = ts.shape_line(text, size_px, &[run], None);
    let line_height = line.ascent + line.descent;
    line.paint(
        point(px(x), px(baseline) - line.ascent),
        line_height,
        TextAlign::Left,
        None,
        window,
        cx,
    )
    .is_err() as usize
}
