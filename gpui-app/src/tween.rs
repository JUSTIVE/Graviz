//! Value transitions on the pinned gpui (which predates `with_spring`).
//!
//! [`track`] remembers the last target per key (via `gpui_metro::motion`)
//! and bumps a generation whenever it changes; [`TweenExt::tween`] then
//! animates from the old target to the new one on Metro's exponential
//! ease-out. A retarget mid-flight restarts from the previous *target*, not
//! the in-flight value — the one thing a spring did that this does not.

use gpui::{Animation, AnimationExt, AnyElement, App, IntoElement, Window};
use gpui_metro::motion;
use std::time::Duration;

/// Panel width/height and hover fades: Metro moves fast and lands hard.
pub const UI: Duration = Duration::from_millis(300);

#[derive(Clone, Copy)]
pub struct Tween {
    key: &'static str,
    from: f32,
    to: f32,
    generation: usize,
    animate: bool,
    duration: Duration,
}

pub fn track(key: &'static str, to: f32, window: &mut Window, cx: &mut App) -> Tween {
    let (from, generation) = motion::track(key, to, window, cx);
    Tween {
        key,
        from,
        to,
        generation,
        animate: !cx.reduce_motion(),
        duration: UI,
    }
}

impl Tween {
    /// Jump straight to the target — e.g. while a splitter is being dragged,
    /// where the pane must follow the cursor rather than chase it.
    pub fn animate(mut self, on: bool) -> Self {
        self.animate &= on;
        self
    }
}

pub trait TweenExt: IntoElement + Sized + 'static {
    fn tween(self, tw: Tween, apply: impl Fn(Self, f32) -> Self + 'static) -> AnyElement {
        if tw.generation == 0 || !tw.animate || tw.from == tw.to {
            return apply(self, tw.to).into_any_element();
        }
        let (from, to) = (tw.from, tw.to);
        self.with_animation(
            motion::generation_id(tw.key, tw.generation),
            Animation::new(tw.duration).with_easing(motion::ease_out_expo),
            move |el, t| apply(el, motion::lerp(from, to, t)),
        )
        .into_any_element()
    }
}

impl<E: IntoElement + Sized + 'static> TweenExt for E {}
