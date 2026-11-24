//! The window's title strip. The app draws its own (the wordmark, the open
//! file), as the gpui app did, so the system's title bar is made
//! transparent and the content runs up under the traffic lights. Behind the
//! strip sits this view, which is the part of the window a drag moves and a
//! double click zooms.

use std::rc::Rc;

use nsh_core::JsonValue;
use nsh_platform_macos::{ComponentContext, Components, NativeComponent};
use objc2::{MainThreadOnly, define_class, msg_send, rc::Retained, runtime::NSObjectProtocol};
use objc2_app_kit::{NSEvent, NSResponder, NSView, NSWindowStyleMask, NSWindowTitleVisibility};
use objc2_foundation::{MainThreadMarker, NSObject};

define_class!(
    #[unsafe(super(NSView, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "GravizTitleStrip"]
    #[ivars = ()]
    pub struct TitleStrip;

    unsafe impl NSObjectProtocol for TitleStrip {}

    impl TitleStrip {
        #[unsafe(method(mouseDownCanMoveWindow))]
        fn can_move(&self) -> bool {
            true
        }

        #[unsafe(method(viewDidMoveToWindow))]
        fn did_move_to_window(&self) {
            let Some(window) = self.window() else { return };
            window.setTitlebarAppearsTransparent(true);
            window.setTitleVisibility(NSWindowTitleVisibility::Hidden);
            window.setStyleMask(window.styleMask() | NSWindowStyleMask::FullSizeContentView);
            // Debug: where the window is, in Quartz's top-left global
            // points, for scripts that drive it with synthesized events.
            if std::env::var("GRAVIZ_FRAME").is_ok() {
                let mtm = MainThreadMarker::new().unwrap();
                #[allow(deprecated)]
                objc2_app_kit::NSApplication::sharedApplication(mtm).activateIgnoringOtherApps(true);
                window.makeKeyAndOrderFront(None);
                let f = window.frame();
                let primary = objc2_app_kit::NSScreen::screens(MainThreadMarker::new().unwrap())
                    .firstObject()
                    .map_or(f.size.height, |s| s.frame().size.height);
                eprintln!(
                    "frame: {:.0} {:.0} {:.0} {:.0}",
                    f.origin.x,
                    primary - (f.origin.y + f.size.height),
                    f.size.width,
                    f.size.height
                );
            }
        }

        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, event: &NSEvent) {
            let Some(window) = self.window() else { return };
            if event.clickCount() == 2 {
                // The system's title bar double-click: zoom, or whatever
                // the user set it to in System Settings (minimize is the
                // other choice); `performZoom` is the default.
                window.performZoom(None);
            } else {
                window.performWindowDragWithEvent(event);
            }
        }
    }
);

impl TitleStrip {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(());
        // SAFETY: NSView's designated initializer with no frame.
        unsafe { msg_send![super(this), init] }
    }
}

struct Strip {
    view: Retained<TitleStrip>,
}

impl NativeComponent for Strip {
    fn view(&self) -> Retained<NSView> {
        Retained::into_super(self.view.clone())
    }

    fn set_prop(&mut self, _name: &str, _value: Option<&JsonValue>) {}
}

pub fn register(components: &Components) {
    components.register(
        "graviz.titlestrip",
        Rc::new(|_cx: ComponentContext| -> Box<dyn NativeComponent> {
            let mtm = MainThreadMarker::new().expect("components are made on the main thread");
            Box::new(Strip { view: TitleStrip::new(mtm) })
        }),
    );
}
