//! The schema graph canvas: GPU-painted cards + edges with pan/zoom.
//!
//! Everything is repainted from the retained `Model` each frame — no texture
//! caches, no LOD placeholder sprites, no motion gates. Those existed in the
//! web app to survive WebGL texture-upload limits; GPUI draws quads, paths and
//! glyphs directly, so culling + text LOD is all that's needed.

use crate::model::{
    EdgeGroup, Model, RowKind, CARD_PAD_X, HEADER_H, NAME_FONT_PX, ROW_FONT_PX, ROW_H,
};
use gompass_core::graph::NodeKind;
use gpui::{
    canvas, div, fill, point, prelude::*, px, quad, rgb, rgba, size, App, BorderStyle, Bounds,
    Context, Corners, Edges, Font, FontWeight, Hsla, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, PathBuilder, Pixels, Point, ScrollDelta, ScrollWheelEvent, SharedString,
    TextAlign, TextRun, Window,
};
use std::rc::Rc;

const ZOOM_MIN: f32 = 0.01;
const ZOOM_MAX: f32 = 4.0;
const ZOOM_STEP: f32 = 1.12;
const CLICK_DRAG_THRESHOLD: f32 = 4.0;
/// Below this zoom, rows are not drawn (and not clickable).
const LOD_ROWS: f32 = 0.35;
/// Below this zoom, header text is not drawn.
const LOD_HEADER: f32 = 0.12;

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
}

impl GraphCanvas {
    pub fn new(model: Rc<Model>) -> Self {
        Self {
            model,
            view: ViewTransform { x: 40.0, y: 40.0, k: 1.0 },
            hover: None,
            drag: None,
            fitted: false,
            focus: None,
            pending_center: None,
            pinned: None,
        }
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
        self.view = ViewTransform {
            x: (vw - gw * k) / 2.0,
            y: (vh - gh * k) / 2.0,
            k,
        };
    }

    fn screen_to_world(&self, p: Point<Pixels>) -> (f32, f32) {
        (
            (f32::from(p.x) - self.view.x) / self.view.k,
            (f32::from(p.y) - self.view.y) / self.view.k,
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
        match ev.delta {
            ScrollDelta::Pixels(d) if !ev.modifiers.platform && !ev.modifiers.control => {
                // trackpad two-finger scroll pans
                self.view.x += f32::from(d.x);
                self.view.y += f32::from(d.y);
            }
            _ => {
                // mouse wheel / cmd+scroll zooms, anchored at the cursor
                let dy = match ev.delta {
                    ScrollDelta::Pixels(d) => f32::from(d.y),
                    ScrollDelta::Lines(d) => d.y * 20.0,
                };
                let ratio = if dy > 0.0 { ZOOM_STEP } else { 1.0 / ZOOM_STEP };
                let new_k = (self.view.k * ratio).clamp(ZOOM_MIN, ZOOM_MAX);
                let ratio = new_k / self.view.k;
                let (mx, my) = (f32::from(ev.position.x), f32::from(ev.position.y));
                self.view.x = mx - (mx - self.view.x) * ratio;
                self.view.y = my - (my - self.view.y) * ratio;
                self.view.k = new_k;
            }
        }
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
            if hover != self.hover {
                self.hover = hover;
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
        let base = format!(
            "{}  ·  {} types  ·  {} edges  ·  {:.0}%",
            m.schema_name,
            m.cards.len(),
            m.edges.len(),
            self.view.k * 100.0
        );
        match self.hover {
            Some(Hover { card, row: Some(row) }) => {
                let c = &m.cards[card as usize];
                let r = &c.rows[row];
                if r.right.is_empty() {
                    format!("{base}   —   {}::{}", c.name, r.left)
                } else {
                    format!("{base}   —   {}.{}: {}", c.name, r.left, r.right)
                }
            }
            Some(Hover { card, row: None }) => {
                let c = &m.cards[card as usize];
                format!("{base}   —   {} {}", c.kind_label, c.name)
            }
            None => base,
        }
    }
}

pub struct Palette {
    pub bg: Hsla,
    pub card_bg: Hsla,
    pub card_border: Hsla,
    pub text: Hsla,
    pub text_muted: Hsla,
    pub type_amber: Hsla,
}

pub fn palette() -> Palette {
    Palette {
        bg: rgb(0x101216).into(),
        card_bg: rgb(0x1a1e26).into(),
        card_border: rgb(0x2c3340).into(),
        text: rgb(0xe6e9ef).into(),
        text_muted: rgb(0x8b93a3).into(),
        type_amber: rgb(0xd9a441).into(),
    }
}

pub fn kind_color(kind: NodeKind) -> Hsla {
    match kind {
        NodeKind::Object => rgb(0x4c8dff).into(),
        NodeKind::Interface => rgb(0x9d7bff).into(),
        NodeKind::Union => rgb(0xd9a441).into(),
        NodeKind::Enum => rgb(0x4dbd82).into(),
        NodeKind::Input => rgb(0x3fb6c9).into(),
        NodeKind::Scalar => rgb(0x8b93a3).into(),
    }
}

fn edge_color(group: EdgeGroup) -> Hsla {
    let c: Hsla = match group {
        EdgeGroup::FieldNonNull => rgb(0x4c8dff).into(),
        EdgeGroup::FieldNullable => rgb(0x4c8dff).into(),
        EdgeGroup::Union => rgb(0xd9a441).into(),
        EdgeGroup::Implements => rgb(0x9d7bff).into(),
        EdgeGroup::Arg => rgb(0xe08a4a).into(),
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
        let vw = f32::from(window.viewport_size().width);
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
        let pal = palette();
        let bg = pal.bg;
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
                        paint_scene(&model, view, hover, focus, pinned, bounds, window, cx);
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
                    .bg(rgba(0x1a1e26e0))
                    .text_color(pal.text_muted)
                    .text_xs()
                    .font_family("Menlo")
                    .child(SharedString::from(status)),
            )
    }
}

fn paint_scene(
    model: &Model,
    view: ViewTransform,
    hover: Option<Hover>,
    focus: Option<u32>,
    pinned: Option<(u32, usize)>,
    bounds: Bounds<Pixels>,
    window: &mut Window,
    cx: &mut App,
) {
    let pal = palette();
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
    for group in groups {
        let mut builder = PathBuilder::stroke(px(stroke_w));
        let mut any = false;
        let mut arrows = PathBuilder::fill();
        let mut any_arrow = false;
        for e in &model.edges {
            if e.group != group {
                continue;
            }
            if e.bbox[2] < wx0 || e.bbox[0] > wx1 || e.bbox[3] < wy0 || e.bbox[1] > wy1 {
                continue;
            }
            let pts = &e.points;
            builder.move_to(to_screen(pts[0], pts[1]));
            for i in (2..pts.len()).step_by(2) {
                builder.line_to(to_screen(pts[i], pts[i + 1]));
            }
            any = true;
            // arrowhead: screen-space triangle at the end, oriented by the
            // last polyline segment
            let n = pts.len();
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
        if any {
            if let Ok(path) = builder.build() {
                window.paint_path(path, edge_color(group));
            }
        }
        if any_arrow {
            if let Ok(path) = arrows.build() {
                window.paint_path(path, edge_color(group));
            }
        }
    }

    // ---- nodes ----
    let name_font = mono(FontWeight::SEMIBOLD);
    let row_font = mono(FontWeight::NORMAL);
    let text_system = window.text_system().clone();

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
        let kc = kind_color(card.kind);
        let is_hovered = matches!(hover, Some(h) if h.card == i as u32);
        let is_focused = focus == Some(i as u32);
        let radius = px(6.0 * k);
        let border_w = if is_focused { 2.0 } else { 1.25 };
        window.paint_quad(quad(
            card_bounds,
            Corners::all(radius),
            pal.card_bg,
            Edges::all(px((border_w * k).clamp(0.5, 3.0))),
            if is_focused || is_hovered { kc } else { pal.card_border },
            BorderStyle::Solid,
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

        // pinned row (search hit) highlight
        if let Some((pc, row)) = pinned {
            if pc == i as u32 && row < card.rows.len() {
                let row_bounds = Bounds {
                    origin: to_screen(pos.x, pos.y + card.row_y(row)),
                    size: size(px(card.w * k), px(ROW_H * k)),
                };
                window.paint_quad(fill(row_bounds, pal.type_amber.opacity(0.18)));
            }
        }

        // hovered row highlight
        if let Some(Hover { card: hc, row: Some(row) }) = hover {
            if hc == i as u32 {
                let ry = card.row_y(row);
                let row_bounds = Bounds {
                    origin: to_screen(pos.x, pos.y + ry),
                    size: size(px(card.w * k), px(ROW_H * k)),
                };
                window.paint_quad(fill(row_bounds, gpui::white().opacity(0.06)));
            }
        }

        // ---- text ----
        if k < LOD_HEADER {
            continue;
        }
        let name_size = px(NAME_FONT_PX * k);
        let name_run = [run(card.name.len(), &name_font, pal.text)];
        let line = text_system.shape_line(
            SharedString::from(card.name.clone()),
            name_size,
            &name_run,
            None,
        );
        let _ = line.paint(
            to_screen(pos.x + CARD_PAD_X, pos.y + 8.0),
            px(NAME_FONT_PX * 1.4 * k),
            TextAlign::Left,
            None,
            window,
            cx,
        );
        // kind label, small, right of header
        let kl_size = px(9.0 * k);
        let kl_run = [run(card.kind_label.len(), &row_font, kc)];
        let kl_line = text_system.shape_line(
            SharedString::from(card.kind_label),
            kl_size,
            &kl_run,
            None,
        );
        let kl_x = pos.x + card.w - CARD_PAD_X - f32::from(kl_line.width) / k;
        let _ = kl_line.paint(
            to_screen(kl_x, pos.y + 10.0),
            px(9.0 * 1.4 * k),
            TextAlign::Left,
            None,
            window,
            cx,
        );

        if k < LOD_ROWS {
            continue;
        }
        let row_size = px(ROW_FONT_PX * k);
        let row_line_h = px(ROW_H * k);
        for (ri, row) in card.rows.iter().enumerate() {
            let ry = pos.y + card.row_y(ri) + (ROW_H - ROW_FONT_PX * 1.2) / 2.0;
            let left_color = match row.kind {
                RowKind::Field | RowKind::EnumValue => {
                    if row.deprecated {
                        pal.text_muted.opacity(0.6)
                    } else {
                        pal.text.opacity(0.92)
                    }
                }
                RowKind::Implements => kind_color(NodeKind::Interface).opacity(0.9),
                RowKind::UnionMember | RowKind::MemberOfUnion => {
                    kind_color(NodeKind::Union).opacity(0.9)
                }
            };
            let mut left_run = run(row.left.len(), &row_font, left_color);
            if row.deprecated {
                left_run.strikethrough = Some(gpui::StrikethroughStyle {
                    thickness: px(1.0),
                    color: Some(pal.text_muted),
                });
            }
            let line = text_system.shape_line(
                SharedString::from(row.left.clone()),
                row_size,
                &[left_run],
                None,
            );
            let _ = line.paint(
                to_screen(pos.x + CARD_PAD_X, ry),
                row_line_h,
                TextAlign::Left,
                None,
                window,
                cx,
            );
            if !row.right.is_empty() {
                let right_run = [run(row.right.len(), &row_font, pal.type_amber)];
                let rline = text_system.shape_line(
                    SharedString::from(row.right.clone()),
                    row_size,
                    &right_run,
                    None,
                );
                let rx = pos.x + card.w - CARD_PAD_X - f32::from(rline.width) / k;
                let _ = rline.paint(to_screen(rx, ry), row_line_h, TextAlign::Left, None, window, cx);
            }
        }
    }
}
