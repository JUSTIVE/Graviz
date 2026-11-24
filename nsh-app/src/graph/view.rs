//! The `NSView` behind `native("graviz.graph")`: owns the renderer, the
//! camera and the display link, all on the main thread.

use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    sync::Arc,
    time::Instant,
};

use nsh_core::JsonValue;
use nsh_platform_macos::{ComponentContext, Components, NativeComponent};
use objc2::{
    AllocAnyThread, DefinedClass, MainThreadOnly, define_class, msg_send,
    rc::Retained,
    runtime::NSObjectProtocol,
    sel,
};
use objc2_app_kit::{NSCursor, NSEvent, NSEventModifierFlags, NSResponder, NSTrackingArea, NSTrackingAreaOptions, NSView};
use objc2_foundation::{MainThreadMarker, NSObject, NSPoint, NSRect, NSRunLoop, NSRunLoopCommonModes, NSSize};
use objc2_quartz_core::CADisplayLink;

use super::{
    canvas::{Canvas, Changes, HistoryItem, Mods, ScrollMode},
    gpu::{Camera, Frame, Gpu},
    paint::{self, ViewState},
    text::Fonts,
};
use crate::{
    model::Model,
    theme::{Color, Theme},
};

use super::canvas::ZOOM_MIN;

/// The scripted camera sweep (`GRAVIZ_BENCH=1`): the gpui canvas's, so the
/// two can be compared frame for frame.
struct Bench {
    frame: usize,
    /// Milliseconds spent in `render`, and between display-link ticks.
    cpu: Vec<f32>,
    interval: Vec<f32>,
    /// Zoom each interval was drawn at.
    zoom: Vec<f32>,
    /// (zoom, ms in `paint`, rects, glyphs) per frame.
    paint: Vec<(f32, f32, usize, usize)>,
    last: Option<Instant>,
}

const BENCH_FRAMES: usize = 480;
const BENCH_ZOOMS: [f32; 5] = [0.0, 0.25, 0.5, 1.0, 2.0];
const BENCH_STOPS: usize = 48;

pub struct State {
    gpu: RefCell<Option<Gpu>>,
    canvas: RefCell<Canvas>,
    theme: Cell<Theme>,
    /// The state the GPU's edge styles were last made for.
    styled: Cell<Option<(ViewState, Theme)>>,
    fonts: RefCell<Fonts>,
    scratch: RefCell<Frame>,
    dirty: Cell<bool>,
    link: RefCell<Option<Retained<CADisplayLink>>>,
    context: RefCell<Option<ComponentContext>>,
    bench: RefCell<Option<Bench>>,
    perf: RefCell<Perf>,
}

/// The perf panel's numbers: an EMA of the frame's cost and an FPS history
/// sampled every 200ms, as the web's panel keeps them.
#[derive(Default)]
struct Perf {
    paint_ms: f32,
    fps: f32,
    hist: Vec<f32>,
    last_frame: Option<Instant>,
    last_sample: Option<Instant>,
}

define_class!(
    #[unsafe(super(NSView, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "GravizGraphView"]
    #[ivars = State]
    pub struct GraphView;

    unsafe impl NSObjectProtocol for GraphView {}

    impl GraphView {
        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            true
        }

        // Keys belong to nsh's window, where the app's shortcuts are; a
        // canvas that took first responder would swallow them.
        #[unsafe(method(acceptsFirstResponder))]
        fn accepts_first_responder(&self) -> bool {
            false
        }

        #[unsafe(method(acceptsFirstMouse:))]
        fn accepts_first_mouse(&self, _event: Option<&NSEvent>) -> bool {
            true
        }

        #[unsafe(method(viewDidMoveToWindow))]
        fn did_move_to_window(&self) {
            self.attach();
        }

        #[unsafe(method(setFrameSize:))]
        fn set_frame_size(&self, size: NSSize) {
            // SAFETY: NSView's own setter, with the argument it was given.
            let _: () = unsafe { msg_send![super(self), setFrameSize: size] };
            self.ivars().dirty.set(true);
        }

        #[unsafe(method(resetCursorRects))]
        fn reset_cursor_rects(&self) {
            self.update_cursor();
        }

        #[unsafe(method(tick:))]
        fn tick(&self, _link: &CADisplayLink) {
            self.frame();
        }

        #[unsafe(method(scrollWheel:))]
        fn scroll_wheel(&self, event: &NSEvent) {
            let (mut dx, mut dy) = (event.scrollingDeltaX() as f32, event.scrollingDeltaY() as f32);
            if !event.hasPreciseScrollingDeltas() {
                // A wheel's notches are lines.
                dx *= 20.0;
                dy *= 20.0;
            }
            let at = self.location(event);
            let mods = mods(event);
            self.apply(|c| c.scroll(at, dx, dy, mods));
        }

        #[unsafe(method(magnifyWithEvent:))]
        fn magnify(&self, event: &NSEvent) {
            let at = self.location(event);
            let by = event.magnification() as f32;
            self.apply(|c| c.magnify(at, by));
        }

        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, event: &NSEvent) {
            let at = self.location(event);
            self.apply(|c| c.mouse_down(at));
            self.update_cursor();
        }

        #[unsafe(method(mouseDragged:))]
        fn mouse_dragged(&self, event: &NSEvent) {
            let at = self.location(event);
            self.apply(|c| c.mouse_move(at));
            if self.ivars().canvas.borrow().dragging() {
                self.confine_cursor(at);
            }
            self.update_cursor();
        }

        #[unsafe(method(mouseUp:))]
        fn mouse_up(&self, event: &NSEvent) {
            let at = self.location(event);
            self.apply(|c| c.mouse_up(at));
            self.update_cursor();
        }

        #[unsafe(method(mouseMoved:))]
        fn mouse_moved(&self, event: &NSEvent) {
            let at = self.location(event);
            self.apply(|c| c.mouse_move(at));
            self.update_cursor();
        }

        #[unsafe(method(mouseExited:))]
        fn mouse_exited(&self, _event: &NSEvent) {
            self.apply(Canvas::mouse_exit);
        }

        #[unsafe(method(rightMouseDown:))]
        fn right_mouse_down(&self, event: &NSEvent) {
            let at = self.location(event);
            self.apply(|c| c.right_click(at));
        }
    }
);

fn mods(event: &NSEvent) -> Mods {
    let f = event.modifierFlags();
    Mods {
        shift: f.contains(NSEventModifierFlags::Shift),
        command: f.contains(NSEventModifierFlags::Command),
        control: f.contains(NSEventModifierFlags::Control),
    }
}

impl GraphView {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let bench = std::env::var("GRAVIZ_BENCH").is_ok().then(|| Bench {
            frame: 0,
            cpu: Vec::new(),
            interval: Vec::new(),
            zoom: Vec::new(),
            paint: Vec::new(),
            last: None,
        });
        let this = Self::alloc(mtm).set_ivars(State {
            gpu: RefCell::new(None),
            canvas: RefCell::new(Canvas::default()),
            theme: Cell::new(Theme::dark(Color::hex(0x0050ef))),
            styled: Cell::new(None),
            fonts: RefCell::new(Fonts::new()),
            scratch: RefCell::new(Frame::default()),
            dirty: Cell::new(true),
            link: RefCell::new(None),
            context: RefCell::new(None),
            bench: RefCell::new(bench),
            perf: RefCell::new(Perf::default()),
        });
        // SAFETY: NSView's designated initializer with no frame.
        let this: Retained<Self> = unsafe { msg_send![super(this), init] };
        // Moves arrive without a button held, and exits clear the hover.
        let options = NSTrackingAreaOptions::MouseMoved
            | NSTrackingAreaOptions::MouseEnteredAndExited
            | NSTrackingAreaOptions::ActiveInKeyWindow
            | NSTrackingAreaOptions::InVisibleRect;
        // SAFETY: a tracking area owned by the view it is added to.
        let area = unsafe {
            NSTrackingArea::initWithRect_options_owner_userInfo(
                NSTrackingArea::alloc(),
                NSRect::ZERO,
                options,
                Some(&this),
                None,
            )
        };
        this.addTrackingArea(&area);
        this
    }

    fn location(&self, event: &NSEvent) -> (f32, f32) {
        let p = self.convertPoint_fromView(event.locationInWindow(), None);
        (p.x as f32, p.y as f32)
    }

    fn emit(&self, name: &str, data: serde_json::Value) {
        if let Some(cx) = self.ivars().context.borrow().as_ref() {
            cx.emit(name, data);
        }
    }

    /// Runs `f` on the canvas, then redraws and tells the app what changed.
    fn apply(&self, f: impl FnOnce(&mut Canvas) -> Changes) {
        let c = {
            let mut canvas = self.ivars().canvas.borrow_mut();
            let mut c = f(&mut canvas);
            c |= canvas.settle();
            c
        };
        self.publish(c);
    }

    fn publish(&self, c: Changes) {
        if c.redraw || c.state || c.hover || c.menu {
            self.ivars().dirty.set(true);
        }
        let canvas = self.ivars().canvas.borrow();
        let (state, hover, menu) = (
            c.state.then(|| canvas.state_json()),
            c.hover.then(|| canvas.hover_json()),
            c.menu.then(|| canvas.menu_json()),
        );
        drop(canvas);
        if let Some(s) = state {
            self.emit("state", s);
        }
        if let Some(h) = hover {
            self.emit("hover", h);
        }
        if let Some(m) = menu {
            self.emit("menu", m);
        }
    }

    /// The pointer mirrors the web's: a hand over empty canvas, closed while
    /// panning, a pointer over anything that can be clicked.
    fn update_cursor(&self) {
        let canvas = self.ivars().canvas.borrow();
        let cursor = if canvas.dragging() {
            NSCursor::closedHandCursor()
        } else if canvas.hover.is_some() || canvas.hovered_edge.is_some() {
            NSCursor::pointingHandCursor()
        } else {
            NSCursor::openHandCursor()
        };
        cursor.set();
    }

    /// Hold the cursor inside the canvas while a drag runs, one point in from
    /// the edge. Without this a pan that reaches a side carries the pointer
    /// out over the desktop; pinned, it keeps pointing at the canvas and the
    /// auto-pan does the travelling.
    fn confine_cursor(&self, (x, y): (f32, f32)) {
        let b = self.bounds();
        let (w, h) = (b.size.width as f32, b.size.height as f32);
        if w <= 2.0 || h <= 2.0 {
            return;
        }
        let (cx, cy) = (x.clamp(1.0, w - 1.0), y.clamp(1.0, h - 1.0));
        if (cx - x).abs() < 0.5 && (cy - y).abs() < 0.5 {
            return;
        }
        let Some(window) = self.window() else { return };
        let in_window = self.convertPoint_toView(NSPoint::new(cx as f64, cy as f64), None);
        let on_screen = window.convertPointToScreen(in_window);
        // Quartz's global space is top-left origin on the primary display.
        let primary_h = core_graphics::display::CGDisplay::main().bounds().size.height;
        let global = core_graphics::geometry::CGPoint::new(on_screen.x, primary_h - on_screen.y);
        let _ = core_graphics::display::CGDisplay::warp_mouse_cursor_position(global);
        // A warp otherwise swallows the next quarter second of movement.
        let _ = core_graphics::display::CGDisplay::associate_mouse_and_mouse_cursor_position(true);
    }

    fn scale(&self) -> f32 {
        self.window().map_or(2.0, |w| w.backingScaleFactor() as f32)
    }

    /// Device-pixel size of the view.
    fn pixels(&self) -> (u32, u32) {
        let (b, s) = (self.bounds(), self.scale());
        ((b.size.width as f32 * s) as u32, (b.size.height as f32 * s) as u32)
    }
    /// In a window: make the renderer and start ticking. Out of one: stop.
    fn attach(&self) {
        let state = self.ivars();
        if let Some(link) = state.link.borrow_mut().take() {
            link.invalidate();
        }
        if self.window().is_none() {
            return;
        }
        if state.gpu.borrow().is_none() {
            let (w, h) = self.pixels();
            let handle = raw_window_handle::AppKitWindowHandle::new(
                std::ptr::NonNull::from(self).cast::<std::ffi::c_void>(),
            );
            let target = wgpu::SurfaceTargetUnsafe::RawHandle {
                raw_display_handle: Some(raw_window_handle::RawDisplayHandle::AppKit(
                    raw_window_handle::AppKitDisplayHandle::new(),
                )),
                raw_window_handle: raw_window_handle::RawWindowHandle::AppKit(handle),
            };
            // SAFETY: the view outlives its renderer, which it owns.
            match unsafe { Gpu::new(target, w, h) } {
                Ok(mut gpu) => {
                    if let Some(model) = state.canvas.borrow().model.as_ref() {
                        gpu.set_model(model);
                    }
                    *state.gpu.borrow_mut() = Some(gpu);
                }
                Err(e) => eprintln!("graph: no GPU renderer: {e:#}"),
            }
        }
        // SAFETY: `tick:` takes the link, as the selector requires. The link
        // retains the view; `attach` invalidates it when the view leaves its
        // window, which breaks the cycle.
        let link = unsafe { self.displayLinkWithTarget_selector(self, sel!(tick:)) };
        // SAFETY: the main run loop exists while the app runs.
        unsafe { link.addToRunLoop_forMode(&NSRunLoop::mainRunLoop(), NSRunLoopCommonModes) };
        *state.link.borrow_mut() = Some(link);
        state.dirty.set(true);
    }

    fn set_model(&self, model: Arc<Model>, keep_place: bool) {
        let state = self.ivars();
        if let Some(gpu) = state.gpu.borrow_mut().as_mut() {
            gpu.set_model(&model);
        }
        state.styled.set(None);
        self.apply(|c| if keep_place { c.replace_model(model) } else { c.set_model(model) });
    }

    fn set_theme(&self, theme: Theme) {
        self.ivars().theme.set(theme);
        self.ivars().dirty.set(true);
    }

    fn bench_camera(&self, i: usize, vw: f32, vh: f32) -> Camera {
        let canvas = self.ivars().canvas.borrow();
        let (gw, gh) = canvas.model.as_ref().map_or((1.0, 1.0), |m| (m.world_w.max(1.0), m.world_h.max(1.0)));
        if i >= BENCH_FRAMES / 2 {
            let t = (i - BENCH_FRAMES / 2) as f32 / (BENCH_FRAMES / 2) as f32;
            let k = 0.2 + 1.2 * t;
            return Camera { k, x: vw / 2.0 - gw * 0.4 * k, y: vh / 2.0 - gh * 0.4 * k };
        }
        let z = BENCH_ZOOMS[(i / BENCH_STOPS) % BENCH_ZOOMS.len()];
        let k = if z == 0.0 { ((vw - 80.0) / gw).min((vh - 80.0) / gh).clamp(ZOOM_MIN, 1.2) } else { z };
        let stop = i % BENCH_STOPS;
        let (col, row) = (stop % 4, (stop / 4) % 4);
        let (cx, cy) = (gw * (col as f32 + 0.5) / 4.0, gh * (row as f32 + 0.5) / 4.0);
        Camera { k, x: vw / 2.0 - cx * k, y: vh / 2.0 - cy * k }
    }

    fn frame(&self) {
        let state = self.ivars();
        let b = self.bounds();
        let (vw, vh) = (b.size.width as f32, b.size.height as f32);
        if vw <= 0.0 {
            return;
        }
        let active = self.window().is_some_and(|w| w.isKeyWindow());
        self.apply(|c| {
            let mut ch = c.resize(vw, vh);
            ch |= c.step(active);
            if c.ripple().is_some() {
                ch.redraw = true;
            }
            ch
        });
        let Some(model) = state.canvas.borrow().model.clone() else { return };
        let benching = {
            let mut bench = state.bench.borrow_mut();
            if let Some(bench) = bench.as_mut() {
                let now = Instant::now();
                if let Some(last) = bench.last.replace(now) {
                    bench.interval.push((now - last).as_secs_f32() * 1000.0);
                    bench.zoom.push(state.canvas.borrow().cam.k);
                }
                if bench.frame >= BENCH_FRAMES {
                    report(bench);
                    std::process::exit(0);
                }
                let cam = self.bench_camera(bench.frame, vw, vh);
                bench.frame += 1;
                state.canvas.borrow_mut().cam = cam;
                true
            } else {
                false
            }
        };
        if !benching && !state.dirty.replace(false) {
            return;
        }
        let mut gpu = state.gpu.borrow_mut();
        let Some(gpu) = gpu.as_mut() else { return };
        let (pw, ph) = self.pixels();
        gpu.resize(pw, ph);
        let t = Instant::now();
        let (cam, view) = {
            let c = state.canvas.borrow();
            (c.cam, c.view_state())
        };
        let theme = state.theme.get();
        // Edge styles depend on the state, not the camera or the ripple.
        let styled = ViewState { ripple: None, hover: None, ..view };
        if state.styled.get() != Some((styled, theme)) {
            gpu.set_edge_styles(&paint::edge_styles(&model, &theme, &styled));
            state.styled.set(Some((styled, theme)));
        }
        let mut frame = state.scratch.borrow_mut();
        let mut fonts = state.fonts.borrow_mut();
        fonts.scale = self.scale();
        paint::paint(&mut frame, &mut fonts, &model, &theme, cam, vw, vh, &view);
        let painted = t.elapsed().as_secs_f32() * 1000.0;
        gpu.render(cam, self.scale(), &frame, &mut fonts);
        let ms = t.elapsed().as_secs_f32() * 1000.0;
        if let Some(bench) = state.bench.borrow_mut().as_mut() {
            bench.paint.push((cam.k, painted, frame.rects.len(), frame.glyphs.len()));
            bench.cpu.push(ms);
        }
        drop(frame);
        drop(fonts);
        self.sample_perf(ms, cam.k);
    }

    /// Feeds the perf panel: the cost EMA, the frame-interval FPS, and a
    /// sample into the chart every 200ms.
    fn sample_perf(&self, ms: f32, k: f32) {
        let mut p = self.ivars().perf.borrow_mut();
        p.paint_ms = if p.paint_ms == 0.0 { ms } else { p.paint_ms * 0.8 + ms * 0.2 };
        let now = Instant::now();
        if let Some(prev) = p.last_frame {
            let dt = now.duration_since(prev).as_secs_f32();
            if dt > 0.0 {
                let inst = (1.0 / dt).min(240.0);
                p.fps = if p.fps == 0.0 { inst } else { p.fps * 0.8 + inst * 0.2 };
            }
        }
        p.last_frame = Some(now);
        if p.last_sample.is_none_or(|t| now.duration_since(t).as_millis() >= 200) {
            p.last_sample = Some(now);
            let fps = p.fps;
            p.hist.push(fps);
            if p.hist.len() > 60 {
                p.hist.remove(0);
            }
            let data = serde_json::json!({ "fps": p.fps, "paint": p.paint_ms, "k": k, "hist": p.hist });
            drop(p);
            self.emit("perf", data);
        }
    }
}

/// An interval this long means a refresh went by without a frame. The link
/// ticks every 8.33ms give or take scheduling, so the line sits between one
/// refresh and two rather than on the budget itself.
const MISSED_MS: f32 = 10.5;

fn report(bench: &mut Bench) {
    let stats = |v: &mut Vec<f32>| {
        v.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let at = |q: f32| v[((v.len() - 1) as f32 * q) as usize];
        (at(0.5), at(0.9), at(0.99), *v.last().unwrap(), v.iter().filter(|&&x| x > MISSED_MS).count())
    };
    for (label, lo, hi) in [("k<0.1", 0.0, 0.1), ("0.1-0.4", 0.1, 0.4), ("0.4-0.8", 0.4, 0.8), ("0.8+", 0.8, 9.0)] {
        let mut v: Vec<f32> =
            bench.interval.iter().zip(&bench.zoom).filter(|&(_, &k)| k >= lo && k < hi).map(|(&i, _)| i).collect();
        if !v.is_empty() {
            let (p50, p90, _, max, over) = stats(&mut v);
            let at: Vec<&(f32, f32, usize, usize)> = bench.paint.iter().filter(|p| p.0 >= lo && p.0 < hi).collect();
            let mut ms: Vec<f32> = at.iter().map(|p| p.1).collect();
            let (m50, _, _, mmax, _) = stats(&mut ms);
            let rects = at.iter().map(|p| p.2).max().unwrap_or(0);
            let glyphs = at.iter().map(|p| p.3).max().unwrap_or(0);
            eprintln!(
                "  {label}: n={} interval p50 {p50:.2} p90 {p90:.2} max {max:.2}  missed {over} | paint p50 {m50:.2} max {mmax:.2} ms, up to {rects} rects {glyphs} glyphs",
                v.len()
            );
        }
    }
    let (p50, p90, p99, max, _) = stats(&mut bench.cpu);
    eprintln!("bench: {} frames  render cpu p50 {p50:.2}  p90 {p90:.2}  p99 {p99:.2}  max {max:.2} ms", bench.cpu.len());
    let (p50, p90, p99, max, over) = stats(&mut bench.interval);
    eprintln!(
        "bench: frame interval p50 {p50:.2}  p90 {p90:.2}  p99 {p99:.2}  max {max:.2} ms  missed refreshes {over} ({:.1}%)",
        100.0 * over as f32 / bench.interval.len() as f32
    );
}

struct Graph {
    view: Retained<GraphView>,
}

impl NativeComponent for Graph {
    fn view(&self) -> Retained<NSView> {
        Retained::into_super(self.view.clone())
    }

    fn set_prop(&mut self, name: &str, value: Option<&JsonValue>) {
        let v = &self.view;
        let flag = || value.and_then(JsonValue::as_bool).unwrap_or(false);
        match name {
            // [id, keep_place]: a new slice starts fresh; a display toggle
            // keeps the reader where they were.
            "model" => {
                let id = value.and_then(|v| v.get(0)).and_then(JsonValue::as_u64);
                let keep = value.and_then(|v| v.get(1)).and_then(JsonValue::as_bool).unwrap_or(false);
                if let Some(model) = id.and_then(super::take) {
                    v.set_model(model, keep);
                }
            }
            "theme" => {
                let dark = value.and_then(|v| v.get("dark")).and_then(JsonValue::as_bool).unwrap_or(true);
                let accent = value.and_then(|v| v.get("accent")).and_then(JsonValue::as_u64).unwrap_or(0x0050ef);
                let accent = Color::hex(accent as u32);
                v.set_theme(if dark { Theme::dark(accent) } else { Theme::light(accent) });
            }
            "investigate" => {
                let on = flag();
                v.apply(|c| c.set_investigate(on));
            }
            "highlightOverlay" => {
                let on = flag();
                v.apply(|c| c.set_highlight_overlay(on));
            }
            "scrollMode" => {
                let pan = value.and_then(JsonValue::as_str) == Some("pan");
                v.ivars().canvas.borrow_mut().scroll_mode = if pan { ScrollMode::Pan } else { ScrollMode::Zoom };
            }
            "reduceMotion" => {
                v.ivars().canvas.borrow_mut().reduce_motion = flag();
            }
            _ => {}
        }
    }

    fn command(&mut self, name: &str, args: &JsonValue) {
        let v = &self.view;
        match name {
            "navigate" => {
                let card = args.get("card").and_then(JsonValue::as_u64).map(|c| c as u32);
                let row = args.get("row").and_then(JsonValue::as_u64).map(|r| r as usize);
                if let Some(card) = card {
                    v.apply(|c| c.navigate_to(card, row));
                }
            }
            "back" => v.apply(Canvas::go_back),
            "revisit" => {
                if let Some(item) = HistoryItem::from_json(args) {
                    v.apply(|c| c.revisit(item));
                }
            }
            "removeHistory" => {
                if let Some(item) = HistoryItem::from_json(args) {
                    v.apply(|c| c.remove_history(item));
                }
            }
            "clearHistory" => v.apply(Canvas::clear_history),
            "clearEdge" => v.apply(Canvas::clear_focused_edge),
            "closeMenu" => v.apply(Canvas::close_menu),
            "fit" => v.apply(Canvas::fit),
            _ => {}
        }
    }
}

/// Registers the graph view; given to `set_components` before the app runs.
pub fn register(components: &Components) {
    components.register(
        "graviz.graph",
        Rc::new(|context: ComponentContext| -> Box<dyn NativeComponent> {
            let mtm = MainThreadMarker::new().expect("components are made on the main thread");
            let view = GraphView::new(mtm);
            *view.ivars().context.borrow_mut() = Some(context);
            Box::new(Graph { view })
        }),
    );
}
