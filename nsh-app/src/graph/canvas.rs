//! What the canvas does with input, apart from any windowing system: the
//! camera, hover and focus, the pinned row and edge, the history, and the
//! gestures that change them. The rules are the gpui canvas's, which were the
//! web app's; `view.rs` only feeds events in and draws what comes out.

use std::{sync::Arc, time::Instant};

use super::{
    gpu::Camera,
    paint::{Hover, LOD_ROWS, ViewState},
};
use crate::model::{CARD_PAD_X, Model, ROW_FONT_PX, RowHit, mono_w};

pub const ZOOM_MIN: f32 = 0.01;
pub const ZOOM_MAX: f32 = 4.0;
/// Pixels a press may travel and still be a click.
pub const CLICK_DRAG_THRESHOLD: f32 = 4.0;
/// How close to a side the cursor has to be before a drag starts sliding the
/// view, and how fast it slides when right up against it.
const EDGE_BAND: f32 = 40.0;
const EDGE_MAX_SPEED: f32 = 1100.0;
/// Focus-ring ripple: one pulse every cycle, stopping after the total so a
/// parked focus does not keep the canvas repainting. The web's 1600ms × 3.
const RIPPLE_CYCLE: f32 = 1.6;
const RIPPLE_TOTAL: f32 = 4.8;
/// History stops kept.
const HISTORY_MAX: usize = 64;

/// One stop in the navigation history: a type card, or an edge between two.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum HistoryItem {
    Card(u32),
    Edge(u32),
}

impl HistoryItem {
    pub fn to_json(self) -> serde_json::Value {
        match self {
            HistoryItem::Card(c) => serde_json::json!(["card", c]),
            HistoryItem::Edge(e) => serde_json::json!(["edge", e]),
        }
    }

    pub fn from_json(v: &serde_json::Value) -> Option<Self> {
        let kind = v.get(0)?.as_str()?;
        let i = v.get(1)?.as_u64()? as u32;
        match kind {
            "card" => Some(HistoryItem::Card(i)),
            "edge" => Some(HistoryItem::Edge(i)),
            _ => None,
        }
    }
}

/// What a scroll gesture asks for.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ScrollAction {
    /// Zoom at the cursor by this much vertical scroll.
    Zoom(f32),
    /// Move the view by this much, in points.
    Pan(f32, f32),
}

/// Settings → scroll: what a trackpad swipe or a wheel turn does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ScrollMode {
    /// Scroll zooms at the cursor and shift pans: the web app's.
    #[default]
    Zoom,
    /// Scroll pans and the platform modifier zooms: most canvases on macOS.
    Pan,
}

/// Modifier keys held during a gesture.
#[derive(Debug, Clone, Copy, Default)]
pub struct Mods {
    pub shift: bool,
    pub command: bool,
    pub control: bool,
}

/// Two ways round, chosen in Settings. A wheel has one axis and shift asks
/// for the other, but macOS has already made that swap by the time the event
/// arrives (a shifted wheel turn is an x delta), so the axis an amount
/// arrives on is the axis it belongs to.
pub fn scroll_action(mode: ScrollMode, dx: f32, dy: f32, mods: Mods) -> ScrollAction {
    let zooming = match mode {
        ScrollMode::Zoom => !mods.shift,
        ScrollMode::Pan => mods.command || mods.control,
    };
    if zooming { ScrollAction::Zoom(dy) } else { ScrollAction::Pan(dx, dy) }
}

/// How fast the view slides for a cursor at (`x`, `y`) in a canvas of
/// (`w`, `h`): zero away from the sides, full speed at the very edge. The
/// slide carries on the way the hand was going: a cursor pinned against the
/// left edge is a hand still pushing left.
pub fn edge_velocity(x: f32, y: f32, w: f32, h: f32) -> (f32, f32) {
    let axis = |p: f32, len: f32| -> f32 {
        if len <= EDGE_BAND * 2.0 {
            0.0
        } else if p < EDGE_BAND {
            -((EDGE_BAND - p) / EDGE_BAND).clamp(0.0, 1.0) * EDGE_MAX_SPEED
        } else if p > len - EDGE_BAND {
            ((p - (len - EDGE_BAND)) / EDGE_BAND).clamp(0.0, 1.0) * EDGE_MAX_SPEED
        } else {
            0.0
        }
    };
    (axis(x, w), axis(y, h))
}

#[derive(Clone, Copy, Debug)]
struct Drag {
    start: (f32, f32),
    orig: Camera,
    moved: bool,
}

/// A right-click menu, anchored in canvas points.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Menu {
    pub card: u32,
    /// Display row, when the click was on one.
    pub row: Option<usize>,
    pub x: f32,
    pub y: f32,
}

/// What changed in a call, so the view knows what to tell the app.
#[derive(Default, Debug, Clone, Copy)]
pub struct Changes {
    /// Focus, pinned edge, history or offscreen.
    pub state: bool,
    /// The hover target.
    pub hover: bool,
    pub menu: bool,
    pub redraw: bool,
}

impl std::ops::BitOrAssign for Changes {
    fn bitor_assign(&mut self, o: Self) {
        self.state |= o.state;
        self.hover |= o.hover;
        self.menu |= o.menu;
        self.redraw |= o.redraw;
    }
}

pub struct Canvas {
    pub model: Option<Arc<Model>>,
    pub cam: Camera,
    pub fitted: bool,
    /// Canvas size in points.
    pub vw: f32,
    pub vh: f32,
    pub hover: Option<Hover>,
    pub hover_pos: Option<(f32, f32)>,
    drag: Option<Drag>,
    pub focus: Option<u32>,
    pub pinned: Option<(u32, usize)>,
    pub history: Vec<HistoryItem>,
    suppress_push: bool,
    pub investigate: bool,
    pub highlight_overlay: bool,
    pub hovered_edge: Option<u32>,
    pub focused_edge: Option<u32>,
    pending_center: Option<u32>,
    pending_edge: Option<u32>,
    focus_changed_at: Option<Instant>,
    /// While a drag holds the cursor against a side: velocity, last step.
    pub edge_pan: Option<((f32, f32), Instant)>,
    pub scroll_mode: ScrollMode,
    pub reduce_motion: bool,
    pub menu: Option<Menu>,
    /// The offscreen flag last reported.
    pub was_offscreen: bool,
}

impl Default for Canvas {
    fn default() -> Self {
        // Debug: GRAVIZ_VIEW="x,y,k" presets the camera (skips the fit);
        // GRAVIZ_EDGE=<index> opens with that edge pinned.
        let preset = std::env::var("GRAVIZ_VIEW").ok().and_then(|s| {
            let v: Vec<f32> = s.split(',').filter_map(|v| v.trim().parse().ok()).collect();
            (v.len() == 3).then(|| Camera { x: v[0], y: v[1], k: v[2] })
        });
        let debug_edge: Option<u32> = std::env::var("GRAVIZ_EDGE").ok().and_then(|v| v.parse().ok());
        Canvas {
            model: None,
            cam: preset.unwrap_or(Camera { k: 1.0, x: 40.0, y: 40.0 }),
            fitted: preset.is_some(),
            vw: 0.0,
            vh: 0.0,
            hover: None,
            hover_pos: None,
            drag: None,
            focus: None,
            pinned: None,
            history: Vec::new(),
            suppress_push: false,
            investigate: false,
            highlight_overlay: false,
            hovered_edge: None,
            focused_edge: preset.is_some().then_some(debug_edge).flatten(),
            pending_center: None,
            pending_edge: preset.is_none().then_some(debug_edge).flatten(),
            focus_changed_at: None,
            edge_pan: None,
            scroll_mode: ScrollMode::Zoom,
            reduce_motion: false,
            menu: None,
            was_offscreen: false,
        }
    }
}

const ALL: Changes = Changes { state: true, hover: true, menu: true, redraw: true };
const REDRAW: Changes = Changes { state: false, hover: false, menu: false, redraw: true };

impl Canvas {
    fn m(&self) -> Option<&Model> {
        self.model.as_deref()
    }

    /// What the painter needs.
    pub fn view_state(&self) -> ViewState {
        ViewState {
            hover: if self.dragging() { None } else { self.hover },
            focus: self.focus,
            pinned: self.pinned,
            investigate: self.investigate,
            highlight_overlay: self.highlight_overlay,
            hovered_edge: self.hovered_edge,
            focused_edge: self.focused_edge,
            ripple: self.ripple(),
        }
    }

    pub fn dragging(&self) -> bool {
        self.drag.is_some_and(|d| d.moved)
    }

    /// Ripple phase, or `None` once the pulse has run its course. Decorative
    /// motion, so it yields to the system's reduce-motion setting.
    pub fn ripple(&self) -> Option<f32> {
        if self.reduce_motion {
            return None;
        }
        let elapsed = self.focus_changed_at?.elapsed().as_secs_f32();
        (elapsed < RIPPLE_TOTAL).then(|| (elapsed % RIPPLE_CYCLE) / RIPPLE_CYCLE)
    }

    /// Swap in a different model: everything that pointed into the old one
    /// goes.
    pub fn set_model(&mut self, model: Arc<Model>) -> Changes {
        self.model = Some(model);
        if std::env::var("GRAVIZ_VIEW").is_err() {
            self.fitted = false;
        }
        self.hover = None;
        self.drag = None;
        self.focus = None;
        self.pending_center = None;
        self.pinned = None;
        self.hover_pos = None;
        self.hovered_edge = None;
        self.focused_edge = None;
        self.history.clear();
        self.suppress_push = false;
        self.menu = None;
        ALL
    }

    /// A rebuilt model of the same slice (a display toggle): positions move,
    /// but what the reader was looking at is still there by name.
    pub fn replace_model(&mut self, model: Arc<Model>) -> Changes {
        let old = self.model.take();
        let rename = |i: u32| -> Option<u32> {
            let name = &old.as_ref()?.cards.get(i as usize)?.name;
            model.index_of.get(name.as_ref()).copied()
        };
        let focus = self.focus.and_then(rename);
        let history: Vec<HistoryItem> = self
            .history
            .iter()
            .filter_map(|h| match *h {
                HistoryItem::Card(c) => rename(c).map(HistoryItem::Card),
                HistoryItem::Edge(_) => None,
            })
            .collect();
        let changes = self.set_model(model);
        self.focus = focus;
        self.history = history;
        self.fitted = true;
        if let Some(f) = focus {
            self.pending_center = Some(f);
            self.suppress_push = true;
        }
        changes
    }

    pub fn resize(&mut self, vw: f32, vh: f32) -> Changes {
        if (vw, vh) == (self.vw, self.vh) {
            return Changes::default();
        }
        self.vw = vw;
        self.vh = vh;
        REDRAW
    }

    /// Fit the whole graph, or on a huge one frame the query root instead,
    /// zoomed in enough to read: a full fit there is a field of specks.
    /// Framing only: opening a schema is not choosing a type.
    pub fn fit(&mut self) -> Changes {
        let Some(m) = self.m() else { return Changes::default() };
        let (gw, gh) = (m.world_w.max(1.0), m.world_h.max(1.0));
        let root = m.roots.first().copied();
        let pad = 80.0;
        let k = ((self.vw - pad) / gw).min((self.vh - pad) / gh).clamp(ZOOM_MIN, 1.2);
        if k < 0.15 {
            if let Some(root) = root {
                self.frame_on(root, 0.8);
                return ALL;
            }
        }
        self.cam = Camera { x: (self.vw - gw * k) / 2.0, y: (self.vh - gh * k) / 2.0, k };
        ALL
    }

    /// Run whatever waited for the canvas to have a size.
    pub fn settle(&mut self) -> Changes {
        let mut c = Changes::default();
        if self.vw <= 0.0 || self.model.is_none() {
            return c;
        }
        if !self.fitted {
            c |= self.fit();
            self.fitted = true;
        }
        if let Some(card) = self.pending_center.take() {
            self.center_on(card);
            c |= ALL;
        }
        if let Some(ei) = self.pending_edge.take() {
            self.focus_edge(ei);
            c |= ALL;
        }
        let off = self.offscreen();
        if off != self.was_offscreen {
            self.was_offscreen = off;
            c.state = true;
        }
        c
    }

    /// The whole graph has left the viewport.
    pub fn offscreen(&self) -> bool {
        let Some(m) = self.m() else { return false };
        let (gw, gh) = (m.world_w, m.world_h);
        let c = self.cam;
        c.x + gw * c.k < 0.0 || c.y + gh * c.k < 0.0 || c.x > self.vw || c.y > self.vh
    }

    fn to_world(&self, (x, y): (f32, f32)) -> (f32, f32) {
        ((x - self.cam.x) / self.cam.k, (y - self.cam.y) / self.cam.k)
    }

    pub fn hit_test(&self, p: (f32, f32)) -> Option<Hover> {
        let m = self.m()?;
        let (wx, wy) = self.to_world(p);
        for (i, card) in m.cards.iter().enumerate() {
            let pos = m.positions[i];
            if wx >= pos.x && wx <= pos.x + card.w && wy >= pos.y && wy <= pos.y + card.h {
                let row = (self.cam.k >= LOD_ROWS).then(|| card.hit_row(wx - pos.x, wy - pos.y)).flatten();
                return Some(Hover { card: i as u32, row });
            }
        }
        None
    }

    /// The nearest edge the cursor can act on. Investigate dims every edge
    /// and lights none, so there an edge is scenery.
    pub fn hit_test_edge_interactive(&self, p: (f32, f32)) -> Option<u32> {
        (!self.investigate).then(|| self.hit_test_edge(p)).flatten()
    }

    /// The nearest edge within about 6 points of the cursor.
    pub fn hit_test_edge(&self, p: (f32, f32)) -> Option<u32> {
        let m = self.m()?;
        let (wx, wy) = self.to_world(p);
        let threshold = 6.0 / self.cam.k;
        let t2 = threshold * threshold;
        let mut best: Option<(f32, u32)> = None;
        for (i, e) in m.edges.iter().enumerate() {
            if wx < e.bbox[0] - threshold || wx > e.bbox[2] + threshold || wy < e.bbox[1] - threshold || wy > e.bbox[3] + threshold {
                continue;
            }
            let pts = &e.points;
            let mut j = 0;
            while j + 3 < pts.len() {
                let (x0, y0, x1, y1) = (pts[j], pts[j + 1], pts[j + 2], pts[j + 3]);
                let (dx, dy) = (x1 - x0, y1 - y0);
                let len2 = dx * dx + dy * dy;
                let t = if len2 > 0.0 { (((wx - x0) * dx + (wy - y0) * dy) / len2).clamp(0.0, 1.0) } else { 0.0 };
                let (cx, cy) = (x0 + dx * t, y0 + dy * t);
                let d2 = (wx - cx).powi(2) + (wy - cy).powi(2);
                if d2 < t2 && best.is_none_or(|(bd, _)| d2 < bd) {
                    best = Some((d2, i as u32));
                }
                j += 4; // every other segment is plenty for hovering
            }
        }
        best.map(|(_, i)| i)
    }

    fn push_history(&mut self, item: HistoryItem) {
        self.history.push(item);
        if self.history.len() > HISTORY_MAX {
            self.history.remove(0);
        }
    }

    /// Frame both ends of an edge (the web's `focusOnEdge`) and pin it. The
    /// edge itself is the history stop, so going back lands on it.
    fn focus_edge(&mut self, edge: u32) {
        let Some(m) = self.m() else { return };
        let Some(e) = m.edges.get(edge as usize) else { return };
        let (a, b) = (e.from as usize, e.to as usize);
        let (pa, pb) = (m.positions[a], m.positions[b]);
        let (ca, cb) = (&m.cards[a], &m.cards[b]);
        let (x0, y0) = (pa.x.min(pb.x), pa.y.min(pb.y));
        let (x1, y1) = ((pa.x + ca.w).max(pb.x + cb.w), (pa.y + ca.h).max(pb.y + cb.h));
        let pad = 80.0;
        let k = ((self.vw - pad) / (x1 - x0).max(1.0)).min((self.vh - pad) / (y1 - y0).max(1.0)).clamp(ZOOM_MIN, 1.2);
        self.history.retain(|&h| h != HistoryItem::Edge(edge));
        self.push_history(HistoryItem::Edge(edge));
        self.cam = Camera { x: self.vw / 2.0 - (x0 + x1) / 2.0 * k, y: self.vh / 2.0 - (y0 + y1) / 2.0 * k, k };
        self.focus = None;
        self.hovered_edge = None;
        self.focused_edge = Some(edge);
    }

    /// Put `card` in the middle of the canvas without selecting it.
    fn frame_on(&mut self, card: u32, k: f32) {
        let Some(m) = self.m() else { return };
        let (c, p) = (&m.cards[card as usize], m.positions[card as usize]);
        self.cam = Camera { x: self.vw / 2.0 - (p.x + c.w / 2.0) * k, y: self.vh / 2.0 - (p.y + c.h / 2.0) * k, k };
    }

    fn center_on(&mut self, card: u32) {
        if !self.suppress_push {
            if let Some(f) = self.focus.filter(|&f| f != card) {
                self.push_history(HistoryItem::Card(f));
            }
        }
        self.suppress_push = false;
        let k = self.cam.k.max(0.9);
        self.frame_on(card, k);
        if self.focus != Some(card) {
            self.focus_changed_at = Some(Instant::now());
        }
        self.focus = Some(card);
    }

    /* ---- commands from the app ---- */

    /// The tree or search selected a type. `row` is in graph space (field,
    /// then enum value) and goes through the card's display-row map.
    pub fn navigate_to(&mut self, card: u32, row: Option<usize>) -> Changes {
        let Some(m) = self.m() else { return Changes::default() };
        if card as usize >= m.cards.len() {
            return Changes::default();
        }
        self.pinned = row.and_then(|r| m.cards[card as usize].display_row(r)).map(|r| (card, r));
        self.pending_center = Some(card);
        ALL
    }

    pub fn go_back(&mut self) -> Changes {
        match self.history.pop() {
            Some(HistoryItem::Card(prev)) => {
                self.pending_center = Some(prev);
                self.suppress_push = true;
            }
            Some(HistoryItem::Edge(ei)) => self.pending_edge = Some(ei),
            None => return Changes::default(),
        }
        ALL
    }

    /// Visit a history stop, leaving the rest of the stack alone.
    pub fn revisit(&mut self, item: HistoryItem) -> Changes {
        match item {
            HistoryItem::Card(card) => {
                self.pending_center = Some(card);
                self.suppress_push = true;
            }
            HistoryItem::Edge(ei) => self.pending_edge = Some(ei),
        }
        ALL
    }

    pub fn remove_history(&mut self, item: HistoryItem) -> Changes {
        self.history.retain(|&h| h != item);
        ALL
    }

    pub fn clear_history(&mut self) -> Changes {
        self.history.clear();
        ALL
    }

    pub fn clear_focused_edge(&mut self) -> Changes {
        if self.focused_edge.take().is_some() { ALL } else { Changes::default() }
    }

    pub fn close_menu(&mut self) -> Changes {
        if self.menu.take().is_some() { ALL } else { Changes::default() }
    }

    pub fn set_investigate(&mut self, on: bool) -> Changes {
        if self.investigate == on {
            return Changes::default();
        }
        self.investigate = on;
        // A hovered or pinned edge would otherwise stay lit against a mode
        // that dims every edge.
        if on {
            self.hovered_edge = None;
            self.focused_edge = None;
        }
        ALL
    }

    pub fn set_highlight_overlay(&mut self, on: bool) -> Changes {
        if self.highlight_overlay == on {
            return Changes::default();
        }
        self.highlight_overlay = on;
        REDRAW
    }

    /* ---- input ---- */

    pub fn scroll(&mut self, at: (f32, f32), dx: f32, dy: f32, mods: Mods) -> Changes {
        self.menu = None;
        match scroll_action(self.scroll_mode, dx, dy, mods) {
            ScrollAction::Pan(dx, dy) => {
                self.cam.x += dx;
                self.cam.y += dy;
            }
            // Smooth exponential zoom: ±100pt of scroll ≈ ×/÷ 1.4.
            ScrollAction::Zoom(dy) => self.zoom_at(at, 2f32.powf(dy / 200.0)),
        }
        ALL
    }

    /// Pinch.
    pub fn magnify(&mut self, at: (f32, f32), by: f32) -> Changes {
        self.zoom_at(at, 1.0 + by);
        ALL
    }

    fn zoom_at(&mut self, (mx, my): (f32, f32), ratio: f32) {
        let k = (self.cam.k * ratio).clamp(ZOOM_MIN, ZOOM_MAX);
        let ratio = k / self.cam.k;
        self.cam.x = mx - (mx - self.cam.x) * ratio;
        self.cam.y = my - (my - self.cam.y) * ratio;
        self.cam.k = k;
    }

    pub fn mouse_down(&mut self, at: (f32, f32)) -> Changes {
        self.drag = Some(Drag { start: at, orig: self.cam, moved: false });
        self.menu = None;
        ALL
    }

    pub fn right_click(&mut self, at: (f32, f32)) -> Changes {
        self.menu = self.hit_test(at).map(|hit| Menu {
            card: hit.card,
            row: match hit.row {
                Some(RowHit::Row(r)) => Some(r),
                _ => None,
            },
            x: at.0,
            y: at.1,
        });
        ALL
    }

    /// A move with or without the button held. Returns whether the drag is
    /// now pressing against a side, for the cursor to be held in.
    pub fn mouse_move(&mut self, at: (f32, f32)) -> Changes {
        if let Some(drag) = &mut self.drag {
            let (dx, dy) = (at.0 - drag.start.0, at.1 - drag.start.1);
            if dx.abs() + dy.abs() > CLICK_DRAG_THRESHOLD {
                drag.moved = true;
            }
            if drag.moved {
                self.cam.x = drag.orig.x + dx;
                self.cam.y = drag.orig.y + dy;
                let v = edge_velocity(at.0, at.1, self.vw, self.vh);
                self.edge_pan = (v != (0.0, 0.0)).then(|| (v, Instant::now()));
                return ALL;
            }
            return Changes::default();
        }
        // Only the hovered target matters; following the cursor pixel by
        // pixel would repaint for nothing.
        let hover = self.hit_test(at);
        let hovered_edge = if hover.is_none() { self.hit_test_edge_interactive(at) } else { None };
        self.hover_pos = Some(at);
        if hover != self.hover || hovered_edge != self.hovered_edge {
            self.hover = hover;
            self.hovered_edge = hovered_edge;
            return Changes { hover: true, redraw: true, ..Default::default() };
        }
        Changes::default()
    }

    pub fn mouse_exit(&mut self) -> Changes {
        if self.drag.is_some() {
            return Changes::default();
        }
        let had = self.hover.is_some() || self.hovered_edge.is_some();
        self.hover = None;
        self.hovered_edge = None;
        self.hover_pos = None;
        if had { Changes { hover: true, redraw: true, ..Default::default() } } else { Changes::default() }
    }

    pub fn mouse_up(&mut self, at: (f32, f32)) -> Changes {
        let was_click = self.drag.is_some_and(|d| !d.moved);
        self.drag = None;
        self.edge_pan = None;
        if !was_click {
            return ALL;
        }
        let hit = self.hit_test(at);
        let edge = if hit.is_none() { self.hit_test_edge_interactive(at) } else { None };
        // Web: clicking empty canvas clears focus and pin.
        if hit.is_none() && edge.is_none() {
            self.focus = None;
            self.pinned = None;
            self.hovered_edge = None;
            self.focused_edge = None;
        }
        if let Some(ei) = edge {
            self.focus_edge(ei);
        }
        let Some(hit) = hit else { return ALL };
        let Some(m) = self.model.clone() else { return ALL };
        self.focused_edge = None;
        let card = &m.cards[hit.card as usize];
        match hit.row {
            Some(RowHit::Row(row)) => {
                // The right-aligned type navigates; the rest of the row pins
                // it (the web's split).
                let r = &card.rows[row];
                let local_x = self.to_world(at).0 - m.positions[hit.card as usize].x;
                let rt_w = mono_w(&r.right, ROW_FONT_PX);
                let on_type = !r.right.is_empty() && local_x >= card.w - CARD_PAD_X - rt_w - 16.0;
                match r.target.filter(|_| on_type || r.right.is_empty()) {
                    Some(t) => self.center_on(t),
                    None => {
                        self.pinned = Some((hit.card, row));
                        if self.focus != Some(hit.card) {
                            self.focus_changed_at = Some(Instant::now());
                        }
                        self.focus = Some(hit.card);
                    }
                }
            }
            Some(RowHit::Implements(b)) => {
                if let Some(&t) = m.index_of.get(card.implements[b].as_ref()) {
                    self.center_on(t);
                }
            }
            Some(RowHit::MemberOfUnion(b)) => {
                if let Some(&t) = m.index_of.get(card.member_of_unions[b].as_ref()) {
                    self.center_on(t);
                }
            }
            None => self.center_on(hit.card),
        }
        ALL
    }

    /// One display frame: the drag's auto-pan. A long frame must not launch
    /// the view across the graph in one step.
    pub fn step(&mut self, window_active: bool) -> Changes {
        if self.drag.is_none() || !window_active {
            self.edge_pan = None;
        }
        let Some(((vx, vy), last)) = self.edge_pan else { return Changes::default() };
        let now = Instant::now();
        let dt = (now - last).as_secs_f32().min(0.05);
        self.cam.x += vx * dt;
        self.cam.y += vy * dt;
        // The drag's anchor travels too, or the next move would undo it.
        if let Some(d) = &mut self.drag {
            d.orig.x += vx * dt;
            d.orig.y += vy * dt;
        }
        self.edge_pan = Some(((vx, vy), now));
        REDRAW
    }

    /// The state the app shows around the canvas.
    pub fn state_json(&self) -> serde_json::Value {
        serde_json::json!({
            "focus": self.focus,
            "edge": self.focused_edge,
            "pinned": self.pinned.map(|(c, r)| [c as usize, r]),
            "history": self.history.iter().map(|h| h.to_json()).collect::<Vec<_>>(),
            "offscreen": self.was_offscreen,
        })
    }

    pub fn hover_json(&self) -> serde_json::Value {
        let row = self.hover.and_then(|h| h.row).map(|r| match r {
            RowHit::Row(i) => serde_json::json!(["row", i]),
            RowHit::Implements(i) => serde_json::json!(["implements", i]),
            RowHit::MemberOfUnion(i) => serde_json::json!(["union", i]),
        });
        let (x, y) = self.hover_pos.unwrap_or((0.0, 0.0));
        serde_json::json!({
            "card": self.hover.map(|h| h.card),
            "row": row,
            "edge": self.hovered_edge,
            "x": x, "y": y,
            "k": self.cam.k,
            "dragging": self.drag.is_some(),
        })
    }

    pub fn menu_json(&self) -> serde_json::Value {
        match self.menu {
            Some(m) => serde_json::json!({ "card": m.card, "row": m.row, "x": m.x, "y": m.y }),
            None => serde_json::Value::Null,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_view_only_slides_near_a_side() {
        assert_eq!(edge_velocity(400.0, 300.0, 800.0, 600.0), (0.0, 0.0));
        let (vx, _) = edge_velocity(0.0, 300.0, 800.0, 600.0);
        assert_eq!(vx, -EDGE_MAX_SPEED);
        let (vx, _) = edge_velocity(780.0, 300.0, 800.0, 600.0);
        assert!(vx > 0.0 && vx < EDGE_MAX_SPEED);
    }

    #[test]
    fn each_mode_zooms_on_its_own_gesture() {
        let none = Mods::default();
        let cmd = Mods { command: true, ..none };
        let shift = Mods { shift: true, ..none };
        assert_eq!(scroll_action(ScrollMode::Zoom, 0.0, 5.0, none), ScrollAction::Zoom(5.0));
        assert_eq!(scroll_action(ScrollMode::Zoom, 5.0, 0.0, shift), ScrollAction::Pan(5.0, 0.0));
        assert_eq!(scroll_action(ScrollMode::Pan, 1.0, 5.0, none), ScrollAction::Pan(1.0, 5.0));
        assert_eq!(scroll_action(ScrollMode::Pan, 0.0, 5.0, cmd), ScrollAction::Zoom(5.0));
    }

    #[test]
    fn history_items_round_trip() {
        for h in [HistoryItem::Card(3), HistoryItem::Edge(9)] {
            assert_eq!(HistoryItem::from_json(&h.to_json()), Some(h));
        }
    }
}
