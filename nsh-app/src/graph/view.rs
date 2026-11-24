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
    DefinedClass, MainThreadOnly, define_class, msg_send,
    rc::Retained,
    runtime::NSObjectProtocol,
    sel,
};
use objc2_app_kit::{NSEvent, NSEventModifierFlags, NSResponder, NSView};
use objc2_foundation::{MainThreadMarker, NSObject, NSPoint, NSRunLoop, NSRunLoopCommonModes, NSSize};
use objc2_quartz_core::CADisplayLink;

use super::{
    gpu::{Camera, Frame, Gpu},
    paint::{self, ViewState},
    text::Fonts,
};
use crate::{model::Model, theme::{Color, Theme}};

const ZOOM_MIN: f32 = 0.01;
const ZOOM_MAX: f32 = 4.0;

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
    model: RefCell<Option<Arc<Model>>>,
    theme: Cell<Theme>,
    view: Cell<ViewState>,
    /// The state the GPU's edge styles were last made for.
    styled: Cell<Option<(ViewState, Theme)>>,
    fonts: RefCell<Fonts>,
    scratch: RefCell<Frame>,
    cam: Cell<Camera>,
    fitted: Cell<bool>,
    dirty: Cell<bool>,
    link: RefCell<Option<Retained<CADisplayLink>>>,
    drag: Cell<Option<NSPoint>>,
    bench: RefCell<Option<Bench>>,
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

        #[unsafe(method(acceptsFirstResponder))]
        fn accepts_first_responder(&self) -> bool {
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

        #[unsafe(method(tick:))]
        fn tick(&self, _link: &CADisplayLink) {
            self.frame();
        }

        #[unsafe(method(scrollWheel:))]
        fn scroll_wheel(&self, event: &NSEvent) {
            let (dx, dy) = (event.scrollingDeltaX() as f32, event.scrollingDeltaY() as f32);
            let flags = event.modifierFlags();
            if flags.contains(NSEventModifierFlags::Command) || flags.contains(NSEventModifierFlags::Control) {
                self.zoom_at(self.location(event), (dy * 0.01).exp());
            } else {
                let mut cam = self.ivars().cam.get();
                cam.x += dx;
                cam.y += dy;
                self.set_camera(cam);
            }
        }

        #[unsafe(method(magnifyWithEvent:))]
        fn magnify(&self, event: &NSEvent) {
            self.zoom_at(self.location(event), 1.0 + event.magnification() as f32);
        }

        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, event: &NSEvent) {
            self.ivars().drag.set(Some(self.location(event)));
        }

        #[unsafe(method(mouseDragged:))]
        fn mouse_dragged(&self, event: &NSEvent) {
            let at = self.location(event);
            if let Some(from) = self.ivars().drag.replace(Some(at)) {
                let mut cam = self.ivars().cam.get();
                cam.x += (at.x - from.x) as f32;
                cam.y += (at.y - from.y) as f32;
                self.set_camera(cam);
            }
        }

        #[unsafe(method(mouseUp:))]
        fn mouse_up(&self, _event: &NSEvent) {
            self.ivars().drag.set(None);
        }
    }
);

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
            model: RefCell::new(None),
            theme: Cell::new(Theme::dark(Color::hex(0x0050ef))),
            view: Cell::new(ViewState::default()),
            styled: Cell::new(None),
            fonts: RefCell::new(Fonts::new()),
            scratch: RefCell::new(Frame::default()),
            cam: Cell::new(Camera { k: 1.0, x: 0.0, y: 0.0 }),
            fitted: Cell::new(false),
            dirty: Cell::new(true),
            link: RefCell::new(None),
            drag: Cell::new(None),
            bench: RefCell::new(bench),
        });
        // SAFETY: NSView's designated initializer with no frame.
        unsafe { msg_send![super(this), init] }
    }

    fn location(&self, event: &NSEvent) -> NSPoint {
        self.convertPoint_fromView(event.locationInWindow(), None)
    }

    fn set_camera(&self, cam: Camera) {
        self.ivars().cam.set(cam);
        self.ivars().dirty.set(true);
    }

    fn zoom_at(&self, at: NSPoint, factor: f32) {
        let cam = self.ivars().cam.get();
        let k = (cam.k * factor).clamp(ZOOM_MIN, ZOOM_MAX);
        let (ax, ay) = (at.x as f32, at.y as f32);
        // Keep the world point under the pointer where it is.
        self.set_camera(Camera { k, x: ax - (ax - cam.x) * k / cam.k, y: ay - (ay - cam.y) * k / cam.k });
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
                    if let Some(model) = state.model.borrow().as_ref() {
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

    fn set_model(&self, model: Arc<Model>) {
        let state = self.ivars();
        if let Some(gpu) = state.gpu.borrow_mut().as_mut() {
            gpu.set_model(&model);
        }
        *state.model.borrow_mut() = Some(model);
        state.styled.set(None);
        state.fitted.set(false);
        state.dirty.set(true);
    }

    fn bench_camera(&self, i: usize, vw: f32, vh: f32) -> Camera {
        let model = self.ivars().model.borrow();
        let (gw, gh) = model.as_ref().map_or((1.0, 1.0), |m| (m.world_w.max(1.0), m.world_h.max(1.0)));
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
        let Some(model) = state.model.borrow().clone() else { return };
        if vw <= 0.0 {
            return;
        }
        if !state.fitted.get() {
            let (w, h) = (model.world_w.max(1.0), model.world_h.max(1.0));
            let k = ((vw - 80.0) / w).min((vh - 80.0) / h).clamp(ZOOM_MIN, 1.2);
            state.cam.set(Camera { k, x: (vw - w * k) / 2.0, y: (vh - h * k) / 2.0 });
            state.fitted.set(true);
            state.dirty.set(true);
        }
        let benching = {
            let mut bench = state.bench.borrow_mut();
            if let Some(bench) = bench.as_mut() {
                let now = Instant::now();
                if let Some(last) = bench.last.replace(now) {
                    bench.interval.push((now - last).as_secs_f32() * 1000.0);
                    bench.zoom.push(state.cam.get().k);
                }
                if bench.frame >= BENCH_FRAMES {
                    report(bench);
                    std::process::exit(0);
                }
                let cam = self.bench_camera(bench.frame, vw, vh);
                bench.frame += 1;
                state.cam.set(cam);
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
        let (cam, theme, view) = (state.cam.get(), state.theme.get(), state.view.get());
        if state.styled.get() != Some((view, theme)) {
            gpu.set_edge_styles(&paint::edge_styles(&model, &theme, &view));
            state.styled.set(Some((view, theme)));
        }
        let mut frame = state.scratch.borrow_mut();
        let mut fonts = state.fonts.borrow_mut();
        fonts.scale = self.scale();
        paint::paint(&mut frame, &mut fonts, &model, &theme, cam, vw, vh, &view);
        let painted = t.elapsed().as_secs_f32() * 1000.0;
        gpu.render(cam, self.scale(), &frame, &mut fonts);
        if let Some(bench) = state.bench.borrow_mut().as_mut() {
            bench.paint.push((cam.k, painted, frame.rects.len(), frame.glyphs.len()));
        }
        if let Some(bench) = state.bench.borrow_mut().as_mut() {
            bench.cpu.push(t.elapsed().as_secs_f32() * 1000.0);
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
    #[allow(dead_code)]
    context: ComponentContext,
}

impl NativeComponent for Graph {
    fn view(&self) -> Retained<NSView> {
        Retained::into_super(self.view.clone())
    }

    fn set_prop(&mut self, name: &str, value: Option<&JsonValue>) {
        if name == "model" {
            if let Some(model) = value.and_then(JsonValue::as_u64).and_then(super::take) {
                self.view.set_model(model);
            }
        }
    }
}

/// Registers the graph view; given to `set_components` before the app runs.
pub fn register(components: &Components) {
    components.register(
        "graviz.graph",
        Rc::new(|context: ComponentContext| -> Box<dyn NativeComponent> {
            let mtm = MainThreadMarker::new().expect("components are made on the main thread");
            Box::new(Graph { view: GraphView::new(mtm), context })
        }),
    );
}
