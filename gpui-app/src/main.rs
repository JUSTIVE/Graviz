mod canvas;
mod model;
mod tree;
mod workspace;

use workspace::Workspace;
use gpui::{prelude::*, px, size, App, Bounds, WindowBounds, WindowOptions};

fn main() {
    let mut args = std::env::args().skip(1);
    let mut path: Option<String> = None;
    let mut overlay_path: Option<String> = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--overlay" => overlay_path = args.next(),
            _ => path = Some(arg),
        }
    }
    let path = path.unwrap_or_else(|| {
        // repo-root fixture when run from gpui-app/
        for candidate in ["../schema.docs.graphql", "schema.docs.graphql"] {
            if std::path::Path::new(candidate).exists() {
                return candidate.to_string();
            }
        }
        eprintln!("usage: gompassql <schema.graphql> [--overlay <overlay.graphql>]");
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
    let base_options = gompass_core::graph::SdlToGraphOptions {
        hide_relay_boilerplate: true,
        ..Default::default()
    };
    let mut graph = gompass_core::graph::sdl_to_graph(&sdl, &base_options);
    if let Some(err) = &graph.error {
        eprintln!("schema parse error: {err}");
        std::process::exit(1);
    }

    // Lay a scratch overlay SDL over the schema (augment / override / remove).
    if let Some(overlay_path) = overlay_path {
        let overlay_sdl = std::fs::read_to_string(&overlay_path).unwrap_or_else(|e| {
            eprintln!("failed to read {overlay_path}: {e}");
            std::process::exit(2);
        });
        let prepared = gompass_core::graph::prepare_overlay(&sdl, &overlay_sdl);
        for w in &prepared.warnings {
            eprintln!("overlay warning: {w}");
        }
        let merged = gompass_core::graph::sdl_to_graph(
            &prepared.sdl,
            &gompass_core::graph::SdlToGraphOptions {
                hide_relay_boilerplate: true,
                remove: prepared.removals,
                override_duplicates: true,
            },
        );
        if let Some(err) = &merged.error {
            eprintln!("overlay parse error: {err}");
            std::process::exit(1);
        }
        let (marked, diff) = gompass_core::graph::mark_overlay(&graph, merged);
        eprintln!(
            "overlay: +{} types, +{} fields, ~{} fields, -{} types, -{} fields",
            diff.added_types.len(),
            diff.added_fields.len(),
            diff.changed_fields.len(),
            diff.removed_types.len(),
            diff.removed_fields.len(),
        );
        graph = marked;
    }
    let parse_ms = t0.elapsed().as_millis();

    eprintln!(
        "parsed in {parse_ms}ms — {} types, {} edges",
        graph.nodes.len(),
        graph.edges.len(),
    );

    gpui_platform::application().run(move |cx: &mut App| {
        workspace::init(cx);
        let bounds = Bounds::centered(None, size(px(1440.), px(900.)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            |_, cx| cx.new(|cx| Workspace::new(graph, schema_name, cx)),
        )
        .unwrap();
        cx.activate(true);
    });
}
