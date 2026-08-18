mod canvas;
mod model;

use canvas::GraphCanvas;
use gpui::{prelude::*, px, size, App, Bounds, WindowBounds, WindowOptions};
use std::rc::Rc;

fn main() {
    let path = std::env::args().nth(1).unwrap_or_else(|| {
        // repo-root fixture when run from gpui-app/
        for candidate in ["../schema.docs.graphql", "schema.docs.graphql"] {
            if std::path::Path::new(candidate).exists() {
                return candidate.to_string();
            }
        }
        eprintln!("usage: gompassql <schema.graphql>");
        std::process::exit(2);
    });

    let sdl = std::fs::read_to_string(&path).unwrap_or_else(|e| {
        eprintln!("failed to read {path}: {e}");
        std::process::exit(2);
    });
    let schema_name = std::path::Path::new(&path)
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.clone());

    let t0 = std::time::Instant::now();
    let graph = gompass_core::graph::sdl_to_graph(
        &sdl,
        &gompass_core::graph::SdlToGraphOptions {
            hide_relay_boilerplate: true,
            ..Default::default()
        },
    );
    if let Some(err) = &graph.error {
        eprintln!("schema parse error: {err}");
        std::process::exit(1);
    }
    let parse_ms = t0.elapsed().as_millis();

    let t1 = std::time::Instant::now();
    let model = Rc::new(model::build_model(graph, schema_name));
    eprintln!(
        "parsed in {parse_ms}ms, layout+model in {}ms — {} types, {} edges, world {:.0}×{:.0}",
        t1.elapsed().as_millis(),
        model.cards.len(),
        model.edges.len(),
        model.world_w,
        model.world_h,
    );

    gpui_platform::application().run(move |cx: &mut App| {
        let bounds = Bounds::centered(None, size(px(1440.), px(900.)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            |_, cx| cx.new(|_| GraphCanvas::new(model)),
        )
        .unwrap();
        cx.activate(true);
    });
}
