//! Graviz on Native Shell.
mod graph;
mod model;
mod selfshot;
mod theme;

use std::sync::Arc;

use graviz_core::graph::{SdlToGraphOptions, sdl_to_graph};
use nsh_core::prelude::*;
use nsh_platform_macos as platform;

#[component]
pub fn App() -> impl IntoElement {
    let path = std::env::args().nth(1).expect("usage: graviz <schema.graphql>");
    let sdl = std::fs::read_to_string(&path).expect("reading the schema");
    let full = sdl_to_graph(&sdl, &SdlToGraphOptions { hide_relay_boilerplate: true, ..Default::default() });
    let sliced = model::slice_graph(&full, model::Mode::Reachable, None);
    let built = model::build_model(sliced, path, &model::ModelOptions::default());
    let id = graph::publish(Arc::new(built));
    view().flex_1().bg(rgb(0x000000)).child(native("graviz.graph").prop("model", serde_json::json!(id)).flex_1())
}

fn main() -> impl std::process::Termination {
    selfshot::arm_if_requested();
    platform::set_components(graph::register);
    platform::run_app(
        platform::AppSource::Rust(platform::Root::of::<App>()),
        platform::AppOptions { title: "Graviz".to_owned(), size: (1440, 900), ..Default::default() },
    )
}
