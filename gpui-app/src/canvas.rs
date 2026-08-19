//! The schema graph canvas: GPU-painted cards + edges with pan/zoom.
//!
//! Everything is repainted from the retained `Model` each frame — no texture
//! caches, no LOD placeholder sprites, no motion gates. Those existed in the
//! web app to survive WebGL texture-upload limits; GPUI draws quads, paths and
//! glyphs directly, so culling + text LOD is all that's needed.

use crate::theme::Theme;
use crate::model::{
    mono_w, EdgeGroup, Model, RowHit, RowKind, TypeColor, BAND_FONT_PX, CARD_PAD_X,
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
// Real glyphs stop paying for themselves once they are only a couple of
// pixels tall; below these zooms the painter falls back to text-shaped bars,
// which keeps the card's texture without shaping thousands of invisible runs.
/// Below this zoom, row text gives way to placeholder bars.
const LOD_ROWS: f32 = 0.28;
/// Below this zoom, header text gives way to a name bar.
const LOD_HEADER: f32 = 0.2;
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
    /// EMA of paint_scene duration, shown in the perf panel.
    frame_ms: Rc<Cell<f32>>,
    /// Rolling FPS samples (last 60) + the sampling clock, mirroring the
    /// web's 260x48 bar chart.
    fps_hist: Rc<std::cell::RefCell<Vec<f32>>>,
    fps_now: Rc<Cell<f32>>,
    last_frame: Rc<Cell<Option<std::time::Instant>>>,
    last_sample: Rc<Cell<Option<std::time::Instant>>>,
    /// Cursor position in canvas-local coords, for tooltip placement.
    hover_pos: Option<(f32, f32)>,
    /// Focus history for back navigation (⌘[) and the Recent list.
    history: Vec<HistoryItem>,
    /// Skip the history push for the next `center_on` (set by `go_back`).
    suppress_push: bool,
    /// Investigate mode: outline types/rows lacking descriptions.
    investigate: bool,
    highlight_overlay: bool,
    /// Edge under the cursor (when no card is hovered).
    hovered_edge: Option<u32>,
    /// Edge pinned by a click. Unlike a hover this survives mouse movement:
    /// the whole picture dims against it and a card naming both ends sits at
    /// the bottom of the canvas until it is cleared.
    focused_edge: Option<u32>,
    /// Edge to re-frame on the next paint (needs the viewport size).
    pending_edge: Option<u32>,
    /// When the focused card last changed — bounds the ring ripple.
    focus_changed_at: Option<std::time::Instant>,
    /// Horizontal window-space offset of the canvas pane (sidebar width),
    /// set by the workspace so fit/center math uses the pane, not the window.
    pane_offset_x: f32,
}

/// One stop in the navigation history: a type card, or an edge between two.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum HistoryItem {
    Card(u32),
    Edge(u32),
}

/// A rendered Recent-list row.
pub struct RecentEntry {
    pub item: HistoryItem,
    pub kind: gompass_core::graph::NodeKind,
    pub kind_label: &'static str,
    pub label: SharedString,
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
        let debug_edge: Option<u32> =
            std::env::var("GOMPASS_EDGE").ok().and_then(|v| v.parse().ok());
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
            fps_hist: Rc::new(std::cell::RefCell::new(Vec::new())),
            fps_now: Rc::new(Cell::new(0.0)),
            last_frame: Rc::new(Cell::new(None)),
            last_sample: Rc::new(Cell::new(None)),
            hover_pos: None,
            history: Vec::new(),
            suppress_push: false,
            investigate: false,
            highlight_overlay: false,
            hovered_edge: None,
            // Debug: GOMPASS_EDGE=<index> opens with that edge pinned, so a
            // selfshot can reproduce the focused-edge state. Combined with
            // GOMPASS_VIEW it pins without re-framing, which is how the dim
            // is checked against a known camera.
            focused_edge: preset.is_some().then_some(debug_edge).flatten(),
            pending_edge: preset.is_none().then_some(debug_edge).flatten(),
            focus_changed_at: None,
            pane_offset_x: 340.0,
        }
    }

    pub fn set_pane_offset(&mut self, offset: f32, cx: &mut Context<Self>) {
        if (self.pane_offset_x - offset).abs() > 0.5 {
            self.pane_offset_x = offset;
            cx.notify();
        }
    }

    /// Dim everything the overlay did not touch (web's Highlight toggle).
    pub fn set_highlight_overlay(&mut self, on: bool, cx: &mut Context<Self>) {
        if self.highlight_overlay != on {
            self.highlight_overlay = on;
            cx.notify();
        }
    }

    pub fn set_investigate(&mut self, on: bool, cx: &mut Context<Self>) {
        if self.investigate != on {
            self.investigate = on;
            cx.notify();
        }
    }

    /// Navigate back to the previous history stop.
    pub fn go_back(&mut self, cx: &mut Context<Self>) {
        match self.history.pop() {
            Some(HistoryItem::Card(prev)) => {
                self.pending_center = Some(prev);
                self.suppress_push = true;
            }
            Some(HistoryItem::Edge(ei)) => self.pending_edge = Some(ei),
            None => return,
        }
        cx.notify();
    }

    pub fn clear_history(&mut self, cx: &mut Context<Self>) {
        self.history.clear();
        cx.notify();
    }

    pub fn remove_history(&mut self, item: HistoryItem, cx: &mut Context<Self>) {
        self.history.retain(|&h| h != item);
        cx.notify();
    }

    /// Re-visit a history stop, leaving the rest of the stack alone.
    pub fn revisit(&mut self, item: HistoryItem, cx: &mut Context<Self>) {
        match item {
            HistoryItem::Card(card) => {
                self.pending_center = Some(card);
                self.suppress_push = true;
            }
            HistoryItem::Edge(ei) => self.pending_edge = Some(ei),
        }
        cx.notify();
    }

    /// The Recent list, newest first. An edge stop reads `Source → Target`,
    /// the same shape the web records when you click an edge.
    pub fn history_entries(&self) -> Vec<RecentEntry> {
        self.history
            .iter()
            .rev()
            .take(50)
            .filter_map(|&item| match item {
                HistoryItem::Card(c) => {
                    let card = self.model.cards.get(c as usize)?;
                    Some(RecentEntry {
                        item,
                        kind: card.kind,
                        kind_label: card.kind_label,
                        label: card.name.clone(),
                    })
                }
                HistoryItem::Edge(ei) => {
                    let e = self.model.edges.get(ei as usize)?;
                    let from = self.model.cards.get(e.from as usize)?;
                    let to = self.model.cards.get(e.to as usize)?;
                    Some(RecentEntry {
                        item,
                        kind: from.kind,
                        kind_label: from.kind_label,
                        label: format!("{} → {}", from.name, to.name).into(),
                    })
                }
            })
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
        self.focused_edge = None;
        self.pending_edge = None;
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
        let k = ((vw - pad) / gw).min((vh - pad) / gh).clamp(ZOOM_MIN, 1.2);
        // On huge graphs a full fit is an unreadable speck field — frame the
        // query root instead, zoomed in enough that text is legible. Framing
        // only: opening a schema is not the same as choosing a type, and
        // starting with one already selected dims the whole rest of the graph
        // against a choice the reader never made.
        if k < 0.15 {
            if let Some(&root) = self.model.roots.first() {
                self.frame_on(root, vw, vh, 0.8);
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

    /// Frame both endpoints of an edge (the web's `focusOnEdge`): fit them
    /// with an 80px pad, capped at k = 1.2, and focus the source.
    fn focus_edge(&mut self, edge: u32, vw: f32, vh: f32) {
        let Some(e) = self.model.edges.get(edge as usize) else { return };
        let (a, b) = (e.from as usize, e.to as usize);
        let (pa, pb) = (self.model.positions[a], self.model.positions[b]);
        let (ca, cb) = (&self.model.cards[a], &self.model.cards[b]);
        let x0 = pa.x.min(pb.x);
        let y0 = pa.y.min(pb.y);
        let x1 = (pa.x + ca.w).max(pb.x + cb.w);
        let y1 = (pa.y + ca.h).max(pb.y + cb.h);
        let pad = 80.0;
        let k = (((vw - pad) / (x1 - x0).max(1.0))
            .min((vh - pad) / (y1 - y0).max(1.0)))
        .clamp(ZOOM_MIN, 1.2);
        // The web records the edge itself as the history stop, not whichever
        // node it happened to focus — going back should land on the edge.
        self.history.retain(|&h| h != HistoryItem::Edge(edge));
        self.history.push(HistoryItem::Edge(edge));
        if self.history.len() > 64 {
            self.history.remove(0);
        }
        self.view = ViewTransform {
            x: vw / 2.0 - (x0 + x1) / 2.0 * k,
            y: vh / 2.0 - (y0 + y1) / 2.0 * k,
            k,
        };
        self.focus = None;
        self.hovered_edge = None;
        self.focused_edge = Some(edge);
    }

    /// Drop the pinned edge (the card's X button, Esc, or empty-canvas click).
    pub fn clear_focused_edge(&mut self, cx: &mut Context<Self>) {
        if self.focused_edge.take().is_some() {
            cx.notify();
        }
    }

    /// Put `card` in the middle of the viewport without selecting it.
    fn frame_on(&mut self, card: u32, vw: f32, vh: f32, k: f32) {
        let c = &self.model.cards[card as usize];
        let p = self.model.positions[card as usize];
        self.view = ViewTransform {
            x: vw / 2.0 - (p.x + c.w / 2.0) * k,
            y: vh / 2.0 - (p.y + c.h / 2.0) * k,
            k,
        };
    }

    fn center_on(&mut self, card: u32, vw: f32, vh: f32) {
        if !self.suppress_push {
            if let Some(f) = self.focus {
                if f != card {
                    self.history.push(HistoryItem::Card(f));
                    if self.history.len() > 64 {
                        self.history.remove(0);
                    }
                }
            }
        }
        self.suppress_push = false;
        let k = self.view.k.max(0.9);
        self.frame_on(card, vw, vh, k);
        if self.focus != Some(card) {
            self.focus_changed_at = Some(std::time::Instant::now());
        }
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
            // Hover: only repaint when the hovered TARGET changes. Following
            // the cursor pixel-by-pixel would repaint the whole canvas on
            // every mouse event — the tooltip re-anchors on the next paint.
            let hover = self.hit_test(ev.position);
            let hovered_edge = if hover.is_none() {
                self.hit_test_edge(ev.position)
            } else {
                None
            };
            let (ox, oy) = self.canvas_origin.get();
            self.hover_pos = Some((f32::from(ev.position.x) - ox, f32::from(ev.position.y) - oy));
            if hover != self.hover || hovered_edge != self.hovered_edge {
                self.hover = hover;
                self.hovered_edge = hovered_edge;
                cx.notify();
            }
        }
    }

    fn on_mouse_up(&mut self, ev: &MouseUpEvent, window: &mut Window, cx: &mut Context<Self>) {
        let was_click = matches!(&self.drag, Some(d) if !d.moved);
        self.drag = None;
        if was_click {
            // Web: clicking empty canvas clears focus + pin.
            if self.hit_test(ev.position).is_none() && self.hit_test_edge(ev.position).is_none() {
                self.focus = None;
                self.pinned = None;
                self.hovered_edge = None;
                self.focused_edge = None;
            }
            let vw = f32::from(window.viewport_size().width) - self.pane_offset_x;
            let vh = f32::from(window.viewport_size().height);
            if self.hit_test(ev.position).is_none() {
                if let Some(ei) = self.hit_test_edge(ev.position) {
                    self.focus_edge(ei, vw, vh);
                }
            }
            if let Some(hit) = self.hit_test(ev.position) {
                self.focused_edge = None;
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
                                if self.focus != Some(hit.card) {
                                    self.focus_changed_at = Some(std::time::Instant::now());
                                }
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

    /// Right-hand half of the web's perf panel: `N nodes · N edges · N%`.
    /// Right-hand half of the web's perf panel: `N nodes · N edges`.
    fn stats_line(&self) -> String {
        let m = &self.model;
        let mut s = format!("{} nodes · {} edges", m.cards.len(), m.edges.len());
        if m.overlay_marks > 0 {
            s.push_str(&format!(" · overlay +{}", m.overlay_marks));
        }
        if self.investigate {
            let (documented, total) = m.desc_coverage;
            let pct = (documented * 100).checked_div(total).unwrap_or(100);
            s.push_str(&format!(" · desc {pct}%"));
        }
        s
    }
}

/// Per-stroke alpha for edges faded behind a focused card or a pinned edge.
///
/// It has to be far harder than it looks. The web fades a whole PixiJS
/// container, so its alpha composites once; these are separate strokes, and
/// twenty overlapping hairlines at 0.1 add back up to an opaque wash — which
/// is why "dimmed" edges still read as bright. Chosen for how it accumulates:
/// a lone edge is a whisper, twenty stacked are a faint haze.
const FOCUS_DIM_ALPHA: f32 = 0.015;

/// Should this edge be drawn in the dim pass?
///
/// A pinned edge outranks the node focus: pinning is an explicit "show me
/// only this", so everything else goes down, including edges that happen to
/// touch whatever card was focused before.
fn edge_is_dimmed(
    index: u32,
    from: u32,
    to: u32,
    hub_faded: bool,
    focused_edge: Option<u32>,
    focus: Option<u32>,
) -> bool {
    match (focused_edge, focus) {
        (Some(pinned), _) => pinned != index,
        (None, Some(f)) => from != f && to != f,
        (None, None) => hub_faded,
    }
}

/// Is this card fully documented — description on the type and on every field
/// or enum value it lists?
fn card_is_documented(card: &crate::model::Card) -> bool {
    card.description.is_some()
        && !card.rows.iter().any(|r| {
            matches!(r.kind, RowKind::Field | RowKind::EnumValue) && r.description.is_none()
        })
}

/// Should this card be drawn dimmed?
///
/// With an edge pinned only its two endpoints stay lit. With a card focused,
/// its immediate neighbours stay lit too: what you want to see after clicking
/// a type is the type *and what it touches*, and dimming the far end of every
/// edge you just lit up leaves those edges running into the dark.
fn card_is_dimmed(
    card: u32,
    pinned_ends: Option<(u32, u32)>,
    focus: Option<u32>,
    neighbour: &dyn Fn(u32) -> bool,
) -> bool {
    match pinned_ends {
        Some((from, to)) => card != from && card != to,
        None => matches!(focus, Some(f) if f != card && !neighbour(card)),
    }
}

/// Focus-ring ripple: one pulse every `RIPPLE_CYCLE`, stopping after
/// `RIPPLE_TOTAL` so a parked focus does not keep the window repainting
/// forever. Matches the web's 1600ms × 3.
const RIPPLE_CYCLE: f32 = 1.6;
const RIPPLE_TOTAL: f32 = 4.8;

/// World-space spacing of the background dot lattice, and the smallest
/// on-screen spacing it is allowed to shrink to before the lattice doubles.
const GRID_STEP: f32 = 24.0;
const GRID_MIN_SCREEN_STEP: f32 = 18.0;
/// Hard ceiling on dots per frame, as a guard against a degenerate viewport.
const GRID_MAX_DOTS: usize = 40_000;

/// Largest deviation, in screen pixels, we aim for between a flattened edge
/// and the true curve. Below half a pixel the difference is not resolvable.
const CURVE_TOL: f32 = 0.35;

/// Line segments an edge layer may emit in one frame before the tolerance is
/// relaxed to fit. Stroke tessellation measures at ~0.113µs per segment, so
/// this is about 4.3ms — the share of a 120fps frame (8.3ms) that edges can
/// have and still leave the ~2.1ms that cards and text cost at their worst
/// plus the ~0.6ms the background lattice takes.
const SEG_BUDGET: usize = 38_000;

/// Segments needed to keep one cubic within `tol` screen pixels of its chords.
///
/// Second differences bound the curve's second derivative, and a cubic split
/// into n equal steps sits within max|B''| / (8n²) of its chords.
fn cubic_steps(
    p0: gompass_core::layout::Point,
    c: &gompass_core::layout::CubicSeg,
    k: f32,
    tol: f32,
) -> usize {
    let d0x = (p0.x - 2.0 * c.c1.x + c.c2.x) * k;
    let d0y = (p0.y - 2.0 * c.c1.y + c.c2.y) * k;
    let d1x = (c.c1.x - 2.0 * c.c2.x + c.end.x) * k;
    let d1y = (c.c1.y - 2.0 * c.c2.y + c.end.y) * k;
    let m = (d0x * d0x + d0y * d0y).max(d1x * d1x + d1y * d1y).sqrt();
    ((0.75 * m / tol).sqrt().ceil() as usize).clamp(1, 24)
}

/// Emit one cubic as line segments, choosing the segment count from how much
/// the curve actually bends *on screen*.
///
/// Handing cubics to `PathBuilder::cubic_bezier_to` costs about five times
/// more per edge than line segments, and the cost grows with on-screen size —
/// it was 17ms of a 17.7ms frame at k=0.6. Flattening here instead spends
/// segments only where there is curvature to resolve: a gentle edge collapses
/// to one segment when zoomed out and still bends smoothly when zoomed in.
fn flatten_cubic(
    builder: &mut PathBuilder,
    p0: gompass_core::layout::Point,
    c: &gompass_core::layout::CubicSeg,
    k: f32,
    tol: f32,
    to_screen: &impl Fn(f32, f32) -> gpui::Point<Pixels>,
) -> usize {
    let n = cubic_steps(p0, c, k, tol);
    for i in 1..=n {
        let t = i as f32 / n as f32;
        let u = 1.0 - t;
        let (a, b, cc, d) = (u * u * u, 3.0 * u * u * t, 3.0 * u * t * t, t * t * t);
        builder.line_to(to_screen(
            a * p0.x + b * c.c1.x + cc * c.c2.x + d * c.end.x,
            a * p0.y + b * c.c1.y + cc * c.c2.y + d * c.end.y,
        ));
    }
    n
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

#[allow(dead_code)]
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
        if let Some(ei) = self.pending_edge.take() {
            self.focus_edge(ei, vw, vh);
        }

        // Ripple phase, or None once the pulse has run its course. Decorative
        // motion, so it yields to the system's reduce-motion setting.
        let ripple = self.focus_changed_at.filter(|_| !cx.reduce_motion()).and_then(|t| {
            let elapsed = t.elapsed().as_secs_f32();
            (elapsed < RIPPLE_TOTAL).then(|| (elapsed % RIPPLE_CYCLE) / RIPPLE_CYCLE)
        });
        if ripple.is_some() {
            window.request_animation_frame();
        }

        let model = self.model.clone();
        let view = self.view;
        let hover = self.hover;
        let focus = self.focus;
        let pinned = self.pinned;
        let canvas_origin = self.canvas_origin.clone();
        let frame_ms = self.frame_ms.clone();
        let fps_hist = self.fps_hist.clone();
        let fps_now = self.fps_now.clone();
        let last_frame = self.last_frame.clone();
        let last_sample = self.last_sample.clone();

        // Floating tooltip: field/header description + deprecation info.
        /// Mirrors the web's three tooltip shapes (field / header / edge).
        struct Tooltip {
            /// Kind badge shown before the name, when the hover has a type.
            badge: Option<(gompass_core::graph::NodeKind, &'static str)>,
            title: String,
            /// Right-hand return type, painted amber like the web.
            type_text: Option<gpui::SharedString>,
            description: Option<String>,
            deprecation: Option<String>,
            expired: bool,
            /// Edge tooltips use the wider `rounded-lg` chrome.
            wide: bool,
        }
        let tooltip: Option<Tooltip> = if self.drag.is_some() {
            None
        } else {
            self.hover.and_then(|h| {
                let card = &self.model.cards[h.card as usize];
                let badge = Some((card.kind, card.kind_label));
                match h.row {
                    Some(RowHit::Row(ri)) => {
                        let row = &card.rows[ri];
                        let deprecation = row
                            .deprecation_reason
                            .clone()
                            .or_else(|| row.deprecated.then(|| "deprecated".to_string()));
                        (row.description.is_some() || deprecation.is_some()).then_some(Tooltip {
                            badge,
                            title: format!("{}.{}", card.name, row.left),
                            type_text: (!row.right.is_empty()).then(|| row.right.clone()),
                            description: row.description.clone(),
                            deprecation,
                            expired: row.until_expired,
                            wide: false,
                        })
                    }
                    Some(RowHit::Implements(b)) => Some(Tooltip {
                        badge,
                        title: format!("implements {}", card.implements[b]),
                        type_text: None,
                        description: None,
                        deprecation: None,
                        expired: false,
                        wide: false,
                    }),
                    Some(RowHit::MemberOfUnion(b)) => Some(Tooltip {
                        badge,
                        title: format!("member of union {}", card.member_of_unions[b]),
                        type_text: None,
                        description: None,
                        deprecation: None,
                        expired: false,
                        wide: false,
                    }),
                    // Header hover: the web only shows this when the sprite is
                    // no longer painting the name, or when it has a description.
                    None => (card.description.is_some() || self.view.k < LOD_ROWS).then(|| {
                        Tooltip {
                            badge,
                            title: card.name.to_string(),
                            type_text: None,
                            description: card.description.clone(),
                            deprecation: None,
                            expired: false,
                            wide: false,
                        }
                    }),
                }
            })
        };
        // Edge tooltip: `Source → Target` with the bundled field labels.
        let tooltip = tooltip.or_else(|| {
            let ei = self.hovered_edge?;
            let e = self.model.edges.get(ei as usize)?;
            let from = &self.model.cards[e.from as usize];
            let to = &self.model.cards[e.to as usize].name;
            let shown: Vec<String> = e.labels.iter().take(10).map(|l| l.to_string()).collect();
            let more = e.labels.len().saturating_sub(10);
            let mut desc = shown.join(", ");
            if more > 0 {
                desc.push_str(&format!(" … +{more}"));
            }
            Some(Tooltip {
                badge: Some((from.kind, from.kind_label)),
                title: format!("{} → {}", from.name, to),
                type_text: (e.bundled > 1)
                    .then(|| gpui::SharedString::from(format!("{} fields", e.bundled))),
                description: (!desc.is_empty()).then_some(desc),
                deprecation: None,
                expired: false,
                wide: true,
            })
        });
        let hover_pos = self.hover_pos;
        let investigate = self.investigate;
        let highlight_overlay = self.highlight_overlay;
        let hovered_edge = self.hovered_edge;
        let focused_edge = self.focused_edge;
        let th = crate::theme::current(cx, window.appearance());
        let bg = th.bg;
        let stats = self.stats_line();

        // Cursor mirrors the web: pointer over anything clickable, an open
        // hand over empty canvas, closed while panning.
        let cursor = if self.drag.as_ref().is_some_and(|d| d.moved) {
            gpui::CursorStyle::ClosedHand
        } else if self.hover.is_some() || self.hovered_edge.is_some() {
            gpui::CursorStyle::PointingHand
        } else {
            gpui::CursorStyle::OpenHand
        };
        div()
            .size_full()
            .relative()
            .bg(bg)
            .cursor(cursor)
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
                                    highlight_overlay, hovered_edge, focused_edge, ripple,
                                    bounds, window, cx,
                                );
                            },
                        );
                        let ms = t0.elapsed().as_secs_f32() * 1000.0;
                        let prev = frame_ms.get();
                        frame_ms.set(if prev == 0.0 { ms } else { prev * 0.8 + ms * 0.2 });
                        // frame-interval FPS, sampled into the chart every 200ms
                        let now = std::time::Instant::now();
                        if let Some(prev_t) = last_frame.get() {
                            let dt = now.duration_since(prev_t).as_secs_f32();
                            if dt > 0.0 {
                                let inst = (1.0 / dt).min(240.0);
                                let cur = fps_now.get();
                                fps_now.set(if cur == 0.0 { inst } else { cur * 0.8 + inst * 0.2 });
                            }
                        }
                        last_frame.set(Some(now));
                        let due = last_sample
                            .get()
                            .map(|t| now.duration_since(t).as_millis() >= 200)
                            .unwrap_or(true);
                        if due {
                            last_sample.set(Some(now));
                            let mut h = fps_hist.borrow_mut();
                            h.push(fps_now.get());
                            if h.len() > 60 {
                                h.remove(0);
                            }
                        }
                    },
                )
                .size_full(),
            )
            .child({
                // Web's performance panel: bottom-right, minWidth 280, a
                // 260x48 FPS bar chart over "N fps" / "N nodes · N edges".
                let hist = self.fps_hist.clone();
                let chart_th = th;
                div()
                    .absolute()
                    .bottom(px(16.0))
                    .right(px(16.0))
                    .min_w(px(280.0))
                    .rounded_lg()
                    .border_1()
                    .border_color(th.panel_border.opacity(0.2))
                    .bg(th.bg.opacity(0.1))
                    .px_3()
                    .py_2()
                    .flex()
                    .flex_col()
                    .font_family("Menlo")
                    .text_xs()
                    .text_color(th.text_muted.opacity(0.6))
                    .child(
                        gpui::canvas(
                            |_, _, _| (),
                            move |bounds, _, window, _| {
                                let samples = hist.borrow();
                                if samples.is_empty() {
                                    return;
                                }
                                let (w, h) = (
                                    f32::from(bounds.size.width),
                                    f32::from(bounds.size.height),
                                );
                                let peak = samples.iter().cloned().fold(65.0f32, f32::max);
                                let max_fps = peak + 5.0;
                                let bar_w = w / samples.len() as f32;
                                for (i, v) in samples.iter().enumerate() {
                                    let bh = ((v / max_fps) * h).max(1.0);
                                    let color = if *v < peak * 0.5 {
                                        chart_th.expired.opacity(0.7)
                                    } else {
                                        chart_th.text_muted.opacity(0.35)
                                    };
                                    window.paint_quad(fill(
                                        Bounds {
                                            origin: point(
                                                bounds.origin.x + px(i as f32 * bar_w),
                                                bounds.origin.y + px(h - bh),
                                            ),
                                            size: size(px((bar_w - 1.0).max(1.0)), px(bh)),
                                        },
                                        color,
                                    ));
                                }
                            },
                        )
                        .w(px(260.0))
                        .h(px(48.0))
                        .mb(px(6.0)),
                    )
                    .child(
                        div()
                            .flex()
                            .items_baseline()
                            .justify_between()
                            .gap_4()
                            .child(SharedString::from(format!(
                                "{} fps",
                                self.fps_now.get().round() as i32
                            )))
                            .child(SharedString::from(stats)),
                    )
                    .child(
                        div()
                            .mt_1()
                            .opacity(0.7)
                            .child(SharedString::from(format!(
                                "paint {:.1}ms · {:.0}%",
                                self.frame_ms.get(),
                                self.view.k * 100.0
                            ))),
                    )
            })
            .when_some(tooltip.zip(hover_pos), |el, (tip, (hx, hy))| {
                // Web placement (tooltip-pos.ts): 12px down-right of the
                // cursor, flipping to a right anchor past the horizontal
                // midpoint and to a bottom anchor within 80px of the bottom.
                let tw = if tip.wide { 420.0 } else { 340.0 };
                let flip_x = hx > vw / 2.0;
                let flip_y = hy > vh - 80.0;
                let left = if flip_x { (hx - tw - 12.0).max(4.0) } else { hx + 12.0 };
                let top = if flip_y { (hy - 132.0).max(4.0) } else { hy + 12.0 };
                let badge = tip.badge;
                el.child(
                    div()
                        .absolute()
                        .left(px(left))
                        .top(px(top))
                        .max_w(px(tw))
                        .when(tip.wide, |el| el.rounded_lg().px_3().py_2())
                        .when(!tip.wide, |el| el.rounded_md().px(px(10.0)).py(px(6.0)))
                        .border_1()
                        .border_color(th.card_border)
                        .bg(th.chrome_bg)
                        .shadow_lg()
                        .flex()
                        .flex_col()
                        .gap(px(2.0))
                        .font_family("Menlo")
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap(px(6.0))
                                .when_some(badge, |el, (kind, label)| {
                                    el.child(
                                        div()
                                            .flex_none()
                                            .rounded_md()
                                            .px(px(4.0))
                                            .text_size(px(9.0))
                                            .bg(th.kind_color(kind))
                                            .text_color(gpui::white())
                                            .child(SharedString::from(label)),
                                    )
                                })
                                .child(
                                    div()
                                        .text_size(px(12.5))
                                        .font_weight(gpui::FontWeight::SEMIBOLD)
                                        .text_color(th.text)
                                        .child(SharedString::from(tip.title)),
                                )
                                .when_some(tip.type_text, |el, t| {
                                    el.child(
                                        div()
                                            .text_size(px(12.5))
                                            .text_color(th.type_amber)
                                            .child(t),
                                    )
                                }),
                        )
                        .when_some(tip.description, |el, d| {
                            el.child(
                                div()
                                    .text_size(px(12.5))
                                    .text_color(th.text_muted)
                                    .line_height(px(20.0))
                                    .max_h(px(120.0))
                                    .overflow_hidden()
                                    .child(SharedString::from(d)),
                            )
                        })
                        .when_some(tip.deprecation, |el, d| {
                            let color: Hsla = if tip.expired { th.expired } else { th.type_amber };
                            el.child(
                                div()
                                    .text_size(px(11.0))
                                    .text_color(color)
                                    .child(SharedString::from(format!("⚠ {d}"))),
                            )
                        }),
                )
            })
            .when_some(
                self.focused_edge.and_then(|ei| self.model.edges.get(ei as usize)),
                |el, e| {
                    // Web's focused-edge widget: a card centred at the bottom
                    // naming both ends. That is what tells you *which* edge
                    // survived the dim — a hover tooltip cannot do the job,
                    // since it vanishes the moment you move the mouse to look.
                    let from = &self.model.cards[e.from as usize];
                    let to = &self.model.cards[e.to as usize];
                    let joiner: Option<SharedString> = match e.group {
                        EdgeGroup::Implements => Some("implements".into()),
                        EdgeGroup::Union => Some("|".into()),
                        _ => None,
                    };
                    let label: Option<SharedString> = if e.bundled > 1 {
                        Some(format!("({} fields)", e.bundled).into())
                    } else {
                        e.labels.first().cloned()
                    };
                    let bundled = e.bundled > 1;
                    let badge = |kind, text: &'static str| {
                        div()
                            .flex_none()
                            .rounded_md()
                            .px(px(4.0))
                            .text_size(px(9.0))
                            .bg(th.kind_color(kind))
                            .text_color(gpui::white())
                            .child(SharedString::from(text))
                    };
                    el.child(
                        div()
                            .absolute()
                            .bottom(px(16.0))
                            .left_0()
                            .right_0()
                            .flex()
                            .justify_center()
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(8.0))
                                    .rounded_lg()
                                    .border_1()
                                    .border_color(th.card_border)
                                    .bg(th.chrome_bg)
                                    .shadow_lg()
                                    .px_3()
                                    .py_2()
                                    .font_family("Menlo")
                                    .text_xs()
                                    .text_color(th.text)
                                    .child(badge(from.kind, from.kind_label))
                                    .child(
                                        div()
                                            .font_weight(gpui::FontWeight::SEMIBOLD)
                                            .child(from.name.clone()),
                                    )
                                    .when_some(label, |el, l| {
                                        el.child(
                                            div()
                                                .text_color(if bundled {
                                                    th.text_muted
                                                } else {
                                                    th.type_amber
                                                })
                                                .child(l),
                                        )
                                    })
                                    .when_some(joiner, |el, j| {
                                        el.child(div().text_color(th.text_muted).child(j))
                                    })
                                    .child(div().text_color(th.text_muted).child("→"))
                                    .child(badge(to.kind, to.kind_label))
                                    .child(
                                        div()
                                            .font_weight(gpui::FontWeight::SEMIBOLD)
                                            .child(to.name.clone()),
                                    )
                                    .child(
                                        div()
                                            .id("clear-edge-focus")
                                            .ml_2()
                                            .rounded_md()
                                            .p_1()
                                            .cursor_pointer()
                                            .hover(|el| el.bg(th.hover_bg))
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                this.clear_focused_edge(cx)
                                            }))
                                            .child(crate::icons::icon(
                                                crate::icons::Icon::X,
                                                px(12.0),
                                                th.text_muted,
                                            )),
                                    ),
                            ),
                    )
                },
            )
    }
}

#[allow(clippy::too_many_arguments)]
fn paint_scene(
    model: &Model,
    view: ViewTransform,
    hover: Option<Hover>,
    focus: Option<u32>,
    pinned: Option<(u32, usize)>,
    investigate: bool,
    highlight_overlay: bool,
    hovered_edge: Option<u32>,
    focused_edge: Option<u32>,
    ripple: Option<f32>,
    bounds: Bounds<Pixels>,
    window: &mut Window,
    cx: &mut App,
) {
    let th = crate::theme::current(cx, window.appearance());
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

    // Cards one edge away from the focused one — lit alongside it.
    let neighbours: std::collections::HashSet<u32> = match focus.filter(|_| focused_edge.is_none())
    {
        Some(f) => model
            .edges
            .iter()
            .filter_map(|e| {
                if e.from == f {
                    Some(e.to)
                } else if e.to == f {
                    Some(e.from)
                } else {
                    None
                }
            })
            .collect(),
        None => std::collections::HashSet::new(),
    };

    let t_probe = std::time::Instant::now();
    // ---- dot grid, under everything ----
    // A 24px world lattice of dots, like the web canvas. It is what makes
    // panning and zooming legible over an empty stretch of graph — without it
    // a drag across blank space looks like nothing is happening. Only the
    // dots inside the viewport are emitted, and the lattice doubles its step
    // as you zoom out so the on-screen spacing stays in a readable band
    // instead of collapsing into a grey wash.
    {
        let mut step = GRID_STEP;
        while step * k < GRID_MIN_SCREEN_STEP {
            step *= 2.0;
        }
        let dot = k.clamp(1.0, 2.0);
        let color = th.text_muted.opacity(0.18);
        let gx0 = (wx0 / step).floor() * step;
        let gy0 = (wy0 / step).floor() * step;
        let cols = ((wx1 - gx0) / step).ceil().max(0.0) as usize;
        let rows = ((wy1 - gy0) / step).ceil().max(0.0) as usize;
        if cols.saturating_mul(rows) <= GRID_MAX_DOTS {
            for r in 0..=rows {
                let wy = gy0 + r as f32 * step;
                for c in 0..=cols {
                    let wx = gx0 + c as f32 * step;
                    window.paint_quad(fill(
                        Bounds { origin: to_screen(wx, wy), size: size(px(dot), px(dot)) },
                        color,
                    ));
                }
            }
        }
    }

    // Frame budget: price the edges at the ideal tolerance first, and if the
    // bill is too high, relax the tolerance just enough to fit. n scales with
    // 1/sqrt(tol), so the correction is the square of the overrun. This is
    // what keeps the frame inside 8.3ms when the whole schema is on screen;
    // at any zoom where the edges already fit, the tolerance stays ideal and
    // nothing is given up.
    let curve_tol = {
        let mut want = 0usize;
        for e in &model.edges {
            if e.bbox[2] < wx0 || e.bbox[0] > wx1 || e.bbox[3] < wy0 || e.bbox[1] > wy1 {
                continue;
            }
            let mut p0 = e.start;
            for c in &e.curves {
                want += cubic_steps(p0, c, k, CURVE_TOL);
                p0 = c.end;
            }
        }
        if want > SEG_BUDGET {
            CURVE_TOL * (want as f32 / SEG_BUDGET as f32).powi(2)
        } else {
            CURVE_TOL
        }
    };
    let mut n_pts = 0usize;
    
    // ---- edges, one stroked path per color group ----
    let groups = [
        EdgeGroup::FieldNonNull,
        EdgeGroup::FieldNullable,
        EdgeGroup::Union,
        EdgeGroup::Implements,
        EdgeGroup::Arg,
    ];
    let stroke_w = (1.5 * k).clamp(0.6, 2.5);
    let draw_arrows = k >= 0.3;
    // Edges live in their own layer so cards (next layer) always draw above
    // them — within a single layer GPUI batches by primitive type, which
    // does not guarantee paths-under-quads.
    window.paint_layer(bounds, |window| {
    // With a focused card, incident edges stay bright and the rest dim —
    // the web app's focus behavior, minus its re-tessellation dodge.
    // GPUI tessellates a path into a u16-indexed vertex buffer, so a single
    // path tops out around 65k vertices — pouring thousands of edges into one
    // builder silently drops geometry. Flush in batches well under that cap
    // (the web app hit the same wall and batched for the same reason).
    const SEGS_PER_BATCH: usize = 900;
    for (group, dim_pass) in groups.iter().flat_map(|&g| [(g, false), (g, true)]) {
        let color = if dim_pass {
            // With a focus of any kind the dimmed edges have to recede hard;
            // without one this pass is only the hub fading, which should stay
            // legible.
            let a = if focused_edge.is_some() || focus.is_some() || investigate {
                FOCUS_DIM_ALPHA
            } else {
                0.35
            };
            edge_color(&th, group).opacity(a)
        } else {
            edge_color(&th, group)
        };
        let mut builder = PathBuilder::stroke(px(stroke_w));
        let mut arrows = PathBuilder::fill();
        let mut segs = 0usize;
        let mut any = false;
        let mut any_arrow = false;
        let flush = |builder: &mut PathBuilder,
                         arrows: &mut PathBuilder,
                         any: &mut bool,
                         any_arrow: &mut bool,
                         segs: &mut usize,
                         window: &mut Window| {
            if *any {
                if let Ok(path) = std::mem::replace(builder, PathBuilder::stroke(px(stroke_w)))
                    .build()
                {
                    window.paint_path(path, color);
                }
            }
            if *any_arrow {
                if let Ok(path) = std::mem::replace(arrows, PathBuilder::fill()).build() {
                    window.paint_path(path, color);
                }
            }
            *any = false;
            *any_arrow = false;
            *segs = 0;
        };

        for (ei, e) in model.edges.iter().enumerate() {
            if e.group != group {
                continue;
            }
            if Some(ei as u32) == hovered_edge {
                continue; // painted separately, highlighted
            }
            // With a focus, everything non-incident dims; without one, only
            // hub-star edges dim (the web app's hub fading).
            let mut dimmed =
                edge_is_dimmed(ei as u32, e.from, e.to, e.hub_faded, focused_edge, focus);
            // In investigate mode an edge is only interesting if it touches
            // something undocumented; the rest recede with their cards.
            if investigate && !dimmed {
                let touches_gap = [e.from, e.to].iter().any(|&c| {
                    model.cards.get(c as usize).is_some_and(|c| !card_is_documented(c))
                });
                dimmed = !touches_gap;
            }
            if dimmed != dim_pass {
                continue;
            }
            if e.bbox[2] < wx0 || e.bbox[0] > wx1 || e.bbox[3] < wy0 || e.bbox[1] > wy1 {
                continue;
            }
            builder.move_to(to_screen(e.start.x, e.start.y));
            let mut p0 = e.start;
            for c in &e.curves {
                let n = flatten_cubic(&mut builder, p0, c, k, curve_tol, &to_screen);
                segs += n;
                n_pts += n;
                p0 = c.end;
            }
            any = true;

            if draw_arrows {
                // arrowhead oriented by the curve's final tangent
                let last = e.curves.last();
                let (ex, ey) =
                    last.map(|c| (c.end.x, c.end.y)).unwrap_or((e.start.x, e.start.y));
                let (px_, py_) =
                    last.map(|c| (c.c2.x, c.c2.y)).unwrap_or((e.start.x, e.start.y));
                let (dx, dy) = (ex - px_, ey - py_);
                let len = (dx * dx + dy * dy).sqrt().max(1e-3);
                let (ux, uy) = (dx / len, dy / len);
                let sz = 5.0f32.max(4.0 * k.min(1.0));
                let tip = to_screen(ex, ey);
                let bx = f32::from(tip.x) - ux * sz * 2.0;
                let by = f32::from(tip.y) - uy * sz * 2.0;
                arrows.move_to(tip);
                arrows.line_to(point(px(bx - uy * sz), px(by + ux * sz)));
                arrows.line_to(point(px(bx + uy * sz), px(by - ux * sz)));
                arrows.close();
                any_arrow = true;
                segs += 1;
            }
            if segs >= SEGS_PER_BATCH {
                flush(
                    &mut builder,
                    &mut arrows,
                    &mut any,
                    &mut any_arrow,
                    &mut segs,
                    window,
                );
            }
        }
        flush(&mut builder, &mut arrows, &mut any, &mut any_arrow, &mut segs, window);
    }

    // The hovered — or pinned — edge draws on top, brighter and thicker.
    if let Some(ei) = hovered_edge.or(focused_edge) {
        if let Some(e) = model.edges.get(ei as usize) {
            let mut builder = PathBuilder::stroke(px((stroke_w * 2.0).max(2.0)));
            builder.move_to(to_screen(e.start.x, e.start.y));
            let mut p0 = e.start;
            for c in &e.curves {
                flatten_cubic(&mut builder, p0, c, k, curve_tol, &to_screen);
                p0 = c.end;
            }
            if let Ok(path) = builder.build() {
                window.paint_path(path, edge_color(&th, e.group).opacity(1.0));
            }
        }
    }
    });
    let t_edges = t_probe.elapsed().as_secs_f32() * 1000.0;

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
    // GOMPASS_PERF=1 reports what a frame actually costs.
    let probe = std::env::var("GOMPASS_PERF").is_ok();
    let mut n_cards = 0usize;
    let mut n_rows = 0usize;
    SHAPES.with(|c| c.set(0));

    window.paint_layer(bounds, |window| {
    for (i, card) in model.cards.iter().enumerate() {
        let pos = model.positions[i];
        if pos.x + card.w < wx0 || pos.x > wx1 || pos.y + card.h < wy0 || pos.y > wy1 {
            continue;
        }
        n_cards += 1;
        let ci = i as u32;
        let dim = if highlight_overlay
            && !card.is_overlay
            && !card.rows.iter().any(|r| r.is_overlay)
        {
            DIM_ALPHA
        } else if investigate && card_is_documented(card) {
            // Investigate is a search for what is missing; a fully documented
            // type is the answer "not here", and leaving it at full strength
            // makes the reader do the filtering the mode exists to do.
            DIM_ALPHA
        } else {
            let pinned_ends =
                focused_edge.and_then(|fe| model.edges.get(fe as usize)).map(|e| (e.from, e.to));
            if card_is_dimmed(ci, pinned_ends, focus, &|c| neighbours.contains(&c)) {
                DIM_ALPHA
            } else {
                1.0
            }
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
            text_errors += paint_baseline(
                &text_system,
                card.name_fit.clone(),
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
        if k >= LOD_ROWS && card.rows.is_empty() && card.hidden_rows > 0 {
            // Every field is behind a filter. An empty body would read as a
            // type that declares nothing, which is a different fact.
            text_errors += paint_baseline(
                &text_system,
                SharedString::from(format!(
                    "… {} hidden field{}",
                    card.hidden_rows,
                    if card.hidden_rows == 1 { "" } else { "s" }
                )),
                &row_font,
                px(ROW_FONT_PX * kt),
                th.text_muted.opacity(0.7 * dim),
                None,
                to_screen(pos.x + CARD_PAD_X, 0.0).x.into(),
                to_screen(0.0, pos.y + card.row_baseline(0)).y.into(),
                window,
                cx,
            );
        }
        if k >= LOD_ROWS {
            let (r_lo, r_hi) = visible_rows(card, pos.y, wy0, wy1);
            for (ri, row) in card.rows.iter().enumerate().take(r_hi).skip(r_lo) {
                n_rows += 1;
                let fy = pos.y + card.row_baseline(ri);
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
                            let ty = row.right_fit.clone();
                            let ty_w = row.right_w;
                            let (ty_c, ty_a) = match row.type_color {
                                TypeColor::Expired => (th.expired, 1.0),
                                TypeColor::BuiltinScalar => (th.type_builtin, 0.7),
                                TypeColor::Normal => (th.type_amber, 1.0),
                            };
                            let tx = pos.x + card.w - CARD_PAD_X - ty_w;
                            text_errors += paint_baseline(
                                &text_system,
                                ty,
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
                    name.clone(),
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
                    name.clone(),
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
            let (r_lo, r_hi) = visible_rows(card, pos.y, wy0, wy1);
            for (ri, row) in card.rows.iter().enumerate().take(r_hi).skip(r_lo) {
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
        if let (true, Some(t)) = (is_focused, ripple) {
            // An expanding, fading echo of the ring: it is what makes a jump
            // to a distant card readable as "here", instead of leaving you to
            // hunt for which box grew a border.
            let pad = t * 18.0;
            window.paint_quad(quad(
                Bounds {
                    origin: to_screen(pos.x - pad, pos.y - pad),
                    size: size(px((card.w + pad * 2.0) * k), px((card.h + pad * 2.0) * k)),
                },
                Corners::all(px((6.0 + pad) * k)),
                gpui::transparent_black(),
                Edges::all(px((2.0 * k).clamp(1.0, 3.0))),
                kind_c.opacity((1.0 - t) * 0.6),
                BorderStyle::Solid,
            ));
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
    if probe {
        let n_edges = model
            .edges
            .iter()
            .filter(|e| {
                !(e.bbox[2] < wx0 || e.bbox[0] > wx1 || e.bbox[3] < wy0 || e.bbox[1] > wy1)
            })
            .count();
        eprintln!(
            "perf: k={k:.2} {:.2}ms (edges {t_edges:.2}) cards={n_cards} rows={n_rows} shapes={} edges={n_edges} emit={n_pts} tol={curve_tol:.2}",
            t_probe.elapsed().as_secs_f32() * 1000.0,
            SHAPES.with(|c| c.get())
        );
    }
}

thread_local! {
    /// Shaped-line count for the GOMPASS_PERF probe.
    static SHAPES: Cell<usize> = const { Cell::new(0) };
}

/// Row index range whose band intersects the visible world rect — a tall
/// card must not shape rows nobody can see.
fn visible_rows(card: &crate::model::Card, card_y: f32, wy0: f32, wy1: f32) -> (usize, usize) {
    let pitch = card.body_pitch();
    let top = card_y + card.body_top();
    let lo = (((wy0 - top) / pitch).floor() as isize).max(0) as usize;
    let hi = (((wy1 - top) / pitch).ceil() as isize).max(0) as usize + 1;
    (lo.min(card.rows.len()), hi.min(card.rows.len()))
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
    SHAPES.with(|c| c.set(c.get() + 1));
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

#[cfg(test)]
mod tests {
    use super::*;
    use gompass_core::layout::{CubicSeg, Point};


    #[test]
    fn pinning_an_edge_dims_everything_but_that_edge() {
        // edge 7 runs 3 -> 9; pin it
        let pinned = Some(7u32);
        assert!(!edge_is_dimmed(7, 3, 9, false, pinned, None));
        assert!(edge_is_dimmed(8, 3, 4, false, pinned, None));
        // ...even an edge touching the previously focused card
        assert!(edge_is_dimmed(8, 3, 4, false, pinned, Some(3)));
        // only the two endpoints stay lit
        let none = |_: u32| false;
        assert!(!card_is_dimmed(3, Some((3, 9)), None, &none));
        assert!(!card_is_dimmed(9, Some((3, 9)), None, &none));
        assert!(card_is_dimmed(4, Some((3, 9)), Some(4), &none));
    }

    #[test]
    fn without_a_pin_the_node_focus_decides() {
        assert!(!edge_is_dimmed(8, 3, 4, false, None, Some(3)));
        assert!(!edge_is_dimmed(8, 3, 4, false, None, Some(4)));
        assert!(edge_is_dimmed(8, 3, 4, false, None, Some(5)));
        // The focused card, and anything one edge from it, stay lit.
        let nbr = |c: u32| c == 4;
        assert!(!card_is_dimmed(3, None, Some(3), &nbr));
        assert!(!card_is_dimmed(4, None, Some(3), &nbr));
        assert!(card_is_dimmed(5, None, Some(3), &nbr));
    }

    #[test]
    fn with_no_focus_at_all_only_hub_edges_fade() {
        assert!(edge_is_dimmed(8, 3, 4, true, None, None));
        assert!(!edge_is_dimmed(8, 3, 4, false, None, None));
        assert!(!card_is_dimmed(4, None, None, &|_| false));
    }

    #[test]
    fn cubic_steps_scale_with_on_screen_curvature() {
        let p0 = Point { x: 0.0, y: 0.0 };
        // control points on the chord: no curvature to resolve
        let straight = CubicSeg {
            c1: Point { x: 100.0, y: 0.0 },
            c2: Point { x: 200.0, y: 0.0 },
            end: Point { x: 300.0, y: 0.0 },
        };
        assert_eq!(cubic_steps(p0, &straight, 1.0, CURVE_TOL), 1);

        let bent = CubicSeg {
            c1: Point { x: 100.0, y: 400.0 },
            c2: Point { x: 200.0, y: -400.0 },
            end: Point { x: 300.0, y: 0.0 },
        };
        let near = cubic_steps(p0, &bent, 1.0, CURVE_TOL);
        let far = cubic_steps(p0, &bent, 0.05, CURVE_TOL);
        assert!(near > far, "zoomed in must subdivide more: {near} vs {far}");
        // relaxing the tolerance must never ask for more segments
        assert!(cubic_steps(p0, &bent, 1.0, CURVE_TOL * 16.0) < near);
    }
}
