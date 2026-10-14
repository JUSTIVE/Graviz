//! The sidebar's right-click copy menu, shared by the three tab bodies
//! (`TreePanel`, `OrphanPanel`, `UntilPanel`).
//!
//! A row naming a type offers the type name; a row naming a field offers the
//! bare field and `Type.field`. Each panel keeps an `Option<CopyMenu>` and
//! exposes it through [`HasCopyMenu`]; [`opener`] fills it from a right click
//! and [`render`] draws it at the click, above everything else.

use crate::theme::Theme;
use gpui::{
    anchored, deferred, div, prelude::*, px, AnyElement, App, Context, MouseButton,
    MouseDownEvent, Pixels, Point, SharedString, Window,
};

#[derive(Clone)]
pub struct CopyMenu {
    /// Window position of the click; the menu opens there.
    pub at: Point<Pixels>,
    pub type_name: SharedString,
    pub field: Option<SharedString>,
}

impl CopyMenu {
    /// What each item copies, in menu order. A field row's own name, not
    /// whatever it targets.
    fn copies(&self) -> Vec<String> {
        match &self.field {
            Some(f) => vec![f.to_string(), format!("{}.{}", self.type_name, f)],
            None => vec![self.type_name.to_string()],
        }
    }
}

pub trait HasCopyMenu: 'static + Sized {
    fn copy_menu(&mut self) -> &mut Option<CopyMenu>;
}

/// A right-button listener that opens the menu for `type_name` (and `field`,
/// on a field row) at the click.
pub fn opener<V: HasCopyMenu>(
    type_name: SharedString,
    field: Option<SharedString>,
    cx: &mut Context<V>,
) -> impl Fn(&MouseDownEvent, &mut Window, &mut App) + 'static {
    // `cx.listener` would tie the closure to `cx`'s borrow; a weak handle
    // does the same job without it.
    let this = cx.entity().downgrade();
    move |ev: &MouseDownEvent, _: &mut Window, cx: &mut App| {
        let _ = this.update(cx, |this, cx| {
            *this.copy_menu() = Some(CopyMenu {
                at: ev.position,
                type_name: type_name.clone(),
                field: field.clone(),
            });
            cx.notify();
        });
    }
}

pub fn render<V: HasCopyMenu>(view: &mut V, th: Theme, cx: &mut Context<V>) -> Option<AnyElement> {
    let menu = view.copy_menu().clone()?;
    let items = menu.copies().into_iter().enumerate().map(|(i, text)| {
        let label = SharedString::from(format!("Copy \"{text}\""));
        div()
            .id(("copy-menu-item", i))
            .px_3()
            .py(px(6.0))
            .whitespace_nowrap()
            .cursor_pointer()
            .hover(|el| el.bg(th.hover_bg))
            .on_click(cx.listener(move |this, _, _, cx| {
                cx.write_to_clipboard(gpui::ClipboardItem::new_string(text.clone()));
                *this.copy_menu() = None;
                cx.notify();
            }))
            .child(label)
    });
    Some(
        deferred(
            anchored().position(menu.at).snap_to_window().child(
                div()
                    .id("copy-menu")
                    .occlude()
                    .min_w(px(180.0))
                    .rounded_lg()
                    .border_1()
                    .border_color(th.card_border)
                    .bg(th.chrome_bg)
                    .shadow_lg()
                    .py_1()
                    .font_family("Menlo")
                    .text_size(px(12.0))
                    .text_color(th.text)
                    // See the same trick in canvas.rs's node context menu:
                    // without this a click on an item also lands on the row
                    // underneath and navigates instead of copying.
                    .on_any_mouse_down(|_, _, cx| cx.stop_propagation())
                    .on_mouse_up(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_mouse_up(MouseButton::Right, |_, _, cx| cx.stop_propagation())
                    // Any press elsewhere, in the panel or on the canvas,
                    // closes it.
                    .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                        if this.copy_menu().take().is_some() {
                            cx.notify();
                        }
                    }))
                    .children(items),
            ),
        )
        .with_priority(1)
        .into_any_element(),
    )
}
