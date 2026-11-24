//! The graph canvas: an `NSView` that draws the schema on the GPU.
//!
//! nsh's views and its canvas are drawn by AppKit and CoreGraphics on the
//! main thread, one path at a time. That is the right tool for chrome and the
//! wrong one for thirty thousand curves: a spike that replayed the GitHub
//! schema through nsh's canvas took 212ms a frame with all of it in view.
//! So the graph is a native component with a renderer of its own. The edges
//! are static meshes in world space, uploaded once; the cards and text on
//! screen are a short list rebuilt each frame; and a frame costs the same
//! whether the camera is moving or not.
//!
//! The engine thread builds a [`Model`] and publishes it under an id; the
//! view, on the main thread, takes it by that id when its `model` prop
//! changes. Nothing else crosses between the two but small JSON.

pub mod canvas;
mod gpu;
mod paint;
mod text;
mod view;

use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
};

pub use paint::LOD_ROWS;
pub use view::register;

use crate::model::Model;

static MODELS: Mutex<Option<HashMap<u64, Arc<Model>>>> = Mutex::new(None);
static NEXT: AtomicU64 = AtomicU64::new(1);

/// Hands a model to the view; the id goes in its `model` prop.
pub fn publish(model: Arc<Model>) -> u64 {
    let id = NEXT.fetch_add(1, Ordering::Relaxed);
    let mut models = MODELS.lock().unwrap();
    let models = models.get_or_insert_with(HashMap::new);
    // The view holds the one it shows; older ones are only garbage here.
    models.clear();
    models.insert(id, model);
    id
}

fn take(id: u64) -> Option<Arc<Model>> {
    MODELS.lock().unwrap().as_ref()?.get(&id).cloned()
}
