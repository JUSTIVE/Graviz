//! The schema graph canvas: GPU-painted cards + edges with pan/zoom.
//!
//! Everything is repainted from the retained `Model` each frame — no texture
//! caches, no LOD placeholder sprites, no motion gates. Those existed in the
//! web app to survive WebGL texture-upload limits; GPUI draws quads, paths and
//! glyphs directly, so culling + text LOD is all that's needed.

use crate::theme::Theme;
use crate::model::{
    EdgeGroup, Model, RowKind, CARD_PAD_X, HEADER_H, NAME_FONT_PX, ROW_FONT_PX, ROW_H,
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
    pub row: Option<usize>,
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

    pub fn history_names(&self) -> Vec<gpui::SharedString> {
        self.history
            .iter()
            .rev()
            .take(6)
            .map(|&i| self.model.cards[i as usize].name.clone())
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
        self.history.clear();
        self.suppress_push = false;
        cx.notify();
    }

    /// Called from the workspace when the tree selects a type.
    pub fn navigate_to(&mut self, card: u32, row: Option<usize>, cx: &mut Context<Self>) {
        self.pending_center = Some(card);
        self.pinned = row.map(|r| (card, r));
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
                    card.row_at(wx - pos.x, wy - pos.y)
                } else {
                    None
                };
                return Some(Hover { card: i as u32, row });
            }
        }
        None
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
            let (ox, oy) = self.canvas_origin.get();
            let pos = (f32::from(ev.position.x) - ox, f32::from(ev.position.y) - oy);
            let moved = match self.hover_pos {
                Some((px_, py_)) => (px_ - pos.0).abs() + (py_ - pos.1).abs() > 2.0,
                None => true,
            };
            if hover != self.hover || (hover.is_some() && moved) {
                self.hover = hover;
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
                let vw = f32::from(window.viewport_size().width);
                let vh = f32::from(window.viewport_size().height);
                let target = match hit.row {
                    Some(row) => self.model.cards[hit.card as usize].rows[row].target,
                    None => Some(hit.card),
                };
                if let Some(t) = target {
                    self.center_on(t, vw, vh);
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
            Some(Hover { card, row: Some(row) }) => {
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
                    Some(ri) => {
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
                    None => card.description.clone().map(|d| Tooltip {
                        title: format!("{} {}", card.kind_label, card.name),
                        description: Some(d),
                        deprecation: None,
                        expired: false,
                    }),
                }
            })
        };
        let hover_pos = self.hover_pos;
        let investigate = self.investigate;
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
                                    &model, view, hover, focus, pinned, investigate, bounds,
                                    window, cx,
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
    let stride: usize = if k < 0.08 {
        16
    } else if k < 0.2 {
        4
    } else if k < 0.5 {
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
        for e in &model.edges {
            if e.group != group {
                continue;
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
    });

    // ---- nodes (their own layer, above the edges) ----
    let name_font = mono(FontWeight::SEMIBOLD);
    let row_font = mono(FontWeight::NORMAL);
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
        let origin = to_screen(pos.x, pos.y);
        let card_bounds = Bounds {
            origin,
            size: size(px(card.w * k), px(card.h * k)),
        };
        let kc = th.kind_color(card.kind);
        let is_hovered = matches!(hover, Some(h) if h.card == i as u32);
        let is_focused = focus == Some(i as u32);
        let radius = px(6.0 * k);
        if k >= 0.25 {
            window.paint_drop_shadows(
                card_bounds,
                Corners::all(radius),
                &[gpui::BoxShadow {
                    color: th.shadow,
                    offset: point(px(0.0), px(2.0 * k)),
                    blur_radius: px(10.0 * k),
                    spread_radius: px(0.0),
                    inset: false,
                }],
            );
        }
        let border_w = if is_focused || card.is_overlay { 2.0 } else { 1.25 };
        let border_color = if card.is_overlay {
            th.overlay_green
        } else if is_focused || is_hovered {
            kc
        } else {
            th.card_border
        };
        window.paint_quad(quad(
            card_bounds,
            Corners::all(radius),
            th.card_bg,
            Edges::all(px((border_w * k).clamp(0.5, 3.0))),
            border_color,
            if card.is_overlay { BorderStyle::Dashed } else { BorderStyle::Solid },
        ));
        // header band
        let header_bounds = Bounds {
            origin,
            size: size(px(card.w * k), px(HEADER_H * k)),
        };
        window.paint_quad(quad(
            header_bounds,
            Corners {
                top_left: radius,
                top_right: radius,
                bottom_left: px(0.0),
                bottom_right: px(0.0),
            },
            kc.opacity(0.16),
            Edges::default(),
            gpui::transparent_black(),
            BorderStyle::Solid,
        ));

        // Investigate mode: red outline on undocumented types, red gutter
        // ticks on undocumented field/enum rows.
        if investigate {
            if card.description.is_none() {
                window.paint_quad(quad(
                    card_bounds,
                    Corners::all(radius),
                    gpui::transparent_black(),
                    Edges::all(px((1.5 * k).clamp(0.6, 2.5))),
                    th.red,
                    BorderStyle::Solid,
                ));
            }
            if k >= LOD_ROWS {
                for (ri, row) in card.rows.iter().enumerate() {
                    if matches!(row.kind, RowKind::Field | RowKind::EnumValue)
                        && row.description.is_none()
                    {
                        let tick = Bounds {
                            origin: to_screen(pos.x, pos.y + card.row_y(ri) + 4.0),
                            size: size(px(2.0 * k), px((card.row_h - 8.0) * k)),
                        };
                        window.paint_quad(fill(tick, th.red));
                    }
                }
            }
        }

        // overlay row gutter markers
        if !card.is_overlay {
            for (ri, row) in card.rows.iter().enumerate() {
                if row.is_overlay {
                    let gutter = Bounds {
                        origin: to_screen(pos.x + 2.0, pos.y + card.row_y(ri) + 2.0),
                        size: size(px(3.0 * k), px((card.row_h - 4.0) * k)),
                    };
                    window.paint_quad(fill(gutter, th.overlay_green));
                }
            }
        }

        // pinned row (search hit) highlight
        if let Some((pc, row)) = pinned {
            if pc == i as u32 && row < card.rows.len() {
                let row_bounds = Bounds {
                    origin: to_screen(pos.x, pos.y + card.row_y(row)),
                    size: size(px(card.w * k), px(card.row_h * k)),
                };
                window.paint_quad(fill(row_bounds, th.type_amber.opacity(0.18)));
            }
        }

        // hovered row highlight
        if let Some(Hover { card: hc, row: Some(row) }) = hover {
            if hc == i as u32 {
                let ry = card.row_y(row);
                let row_bounds = Bounds {
                    origin: to_screen(pos.x, pos.y + ry),
                    size: size(px(card.w * k), px(card.row_h * k)),
                };
                window.paint_quad(fill(row_bounds, gpui::white().opacity(0.06)));
            }
        }

        // ---- text ----
        if k < LOD_HEADER {
            continue;
        }
        let name_size = px(NAME_FONT_PX * kt);
        let name_run = [run(card.name.len(), &name_font, th.text)];
        let line = text_system.shape_line(card.name.clone(), name_size, &name_run, None);
        text_errors += line
            .paint(
                to_screen(pos.x + CARD_PAD_X, pos.y + 8.0),
                px(NAME_FONT_PX * 1.4 * kt),
                TextAlign::Left,
                None,
                window,
                cx,
            )
            .is_err() as usize;
        // kind label, small, right of header
        let kl_size = px(9.0 * kt);
        let kl_run = [run(card.kind_label.len(), &row_font, kc)];
        let kl_line = text_system.shape_line(
            SharedString::from(card.kind_label),
            kl_size,
            &kl_run,
            None,
        );
        let kl_x = pos.x + card.w - CARD_PAD_X - f32::from(kl_line.width) / k;
        text_errors += kl_line
            .paint(
                to_screen(kl_x, pos.y + 10.0),
                px(9.0 * 1.4 * kt),
                TextAlign::Left,
                None,
                window,
                cx,
            )
            .is_err() as usize;

        if k < LOD_ROWS {
            // Placeholder bars stand in for row text so zoomed-out cards keep
            // their texture (the web app's "bar" sprite LOD).
            if k >= LOD_ROW_BARS {
                let bar_h = (ROW_FONT_PX * k).max(1.0);
                for (ri, row) in card.rows.iter().enumerate() {
                    let by = pos.y + card.row_y(ri) + (card.row_h - ROW_FONT_PX) / 2.0;
                    let left_w = crate::model::mono_w(&row.left, ROW_FONT_PX)
                        .min(card.w * 0.55);
                    window.paint_quad(fill(
                        Bounds {
                            origin: to_screen(pos.x + CARD_PAD_X, by),
                            size: size(px(left_w * k), px(bar_h)),
                        },
                        th.text_muted.opacity(0.3),
                    ));
                    if !row.right.is_empty() {
                        let right_w = crate::model::mono_w(&row.right, ROW_FONT_PX)
                            .min(card.w * 0.4);
                        window.paint_quad(fill(
                            Bounds {
                                origin: to_screen(
                                    pos.x + card.w - CARD_PAD_X - right_w,
                                    by,
                                ),
                                size: size(px(right_w * k), px(bar_h)),
                            },
                            th.type_amber.opacity(0.25),
                        ));
                    }
                }
            }
            continue;
        }
        let row_size = px(ROW_FONT_PX * kt);
        let row_line_h = px(ROW_H * kt);
        for (ri, row) in card.rows.iter().enumerate() {
            let ry = pos.y + card.row_y(ri) + (ROW_H - ROW_FONT_PX * 1.2) / 2.0;
            let left_color = match row.kind {
                RowKind::Field | RowKind::EnumValue => {
                    if row.deprecated {
                        th.text_muted.opacity(0.6)
                    } else {
                        th.text.opacity(0.92)
                    }
                }
                RowKind::Implements => th.kind_color(NodeKind::Interface).opacity(0.9),
                RowKind::UnionMember | RowKind::MemberOfUnion => {
                    th.kind_color(NodeKind::Union).opacity(0.9)
                }
            };
            let mut left_run = run(row.left.len(), &row_font, left_color);
            if row.deprecated {
                left_run.strikethrough = Some(gpui::StrikethroughStyle {
                    thickness: px(1.0),
                    color: Some(th.text_muted),
                });
            }
            let line = text_system.shape_line(row.left.clone(), row_size, &[left_run], None);
            text_errors += line
                .paint(
                    to_screen(pos.x + CARD_PAD_X, ry),
                    row_line_h,
                    TextAlign::Left,
                    None,
                    window,
                    cx,
                )
                .is_err() as usize;
            if !row.right.is_empty() {
                let rt_color: Hsla = if row.until_expired {
                    th.red
                } else {
                    th.type_amber
                };
                let right_run = [run(row.right.len(), &row_font, rt_color)];
                let rline =
                    text_system.shape_line(row.right.clone(), row_size, &right_run, None);
                let rx = pos.x + card.w - CARD_PAD_X - f32::from(rline.width) / k;
                text_errors += rline
                    .paint(to_screen(rx, ry), row_line_h, TextAlign::Left, None, window, cx)
                    .is_err() as usize;
                if row.is_relay {
                    // small teal dot marking a Relay-unwrapped connection field
                    let d = 3.5 * k;
                    window.paint_quad(quad(
                        Bounds {
                            origin: to_screen(rx - 8.0, ry + 3.5),
                            size: size(px(d), px(d)),
                        },
                        Corners::all(px(d / 2.0)),
                        th.kind_color(NodeKind::Input),
                        Edges::default(),
                        gpui::transparent_black(),
                        BorderStyle::Solid,
                    ));
                }
            }
            if let Some(desc) = &row.description_line {
                let drun = [run(desc.len(), &row_font, th.text_muted.opacity(0.8))];
                let dline = text_system.shape_line(desc.clone(), px(8.5 * kt), &drun, None);
                text_errors += dline
                    .paint(
                        to_screen(pos.x + CARD_PAD_X, pos.y + card.row_y(ri) + 13.5),
                        px(8.5 * 1.3 * kt),
                        TextAlign::Left,
                        None,
                        window,
                        cx,
                    )
                    .is_err() as usize;
            }
        }
    }
    });
    if text_errors > 0 {
        eprintln!("canvas: {text_errors} text paint errors this frame");
    }
}
