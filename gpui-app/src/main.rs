mod canvas;
mod loader;
mod model;
#[cfg(target_os = "macos")]
mod selfshot;
mod tree;
mod workspace;

use gpui::{prelude::*, px, size, App, Bounds, WindowBounds, WindowOptions};
use std::path::PathBuf;
use workspace::Workspace;

fn main() {
    let mut args = std::env::args().skip(1);
    let mut path: Option<String> = None;
    let mut overlay_path: Option<PathBuf> = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--overlay" => overlay_path = args.next().map(PathBuf::from),
            _ => path = Some(arg),
        }
    }
    let path = PathBuf::from(path.unwrap_or_else(|| {
        // repo-root fixture when run from gpui-app/
        for candidate in ["../schema.docs.graphql", "schema.docs.graphql"] {
            if std::path::Path::new(candidate).exists() {
                return candidate.to_string();
            }
        }
        eprintln!("usage: gompassql <schema.graphql> [--overlay <overlay.graphql>]");
        std::process::exit(2);
    }));

    let t0 = std::time::Instant::now();
    let loaded = loader::load(&path, overlay_path.as_deref()).unwrap_or_else(|e| {
        eprintln!("{e:#}");
        std::process::exit(1);
    });
    eprintln!(
        "parsed in {}ms — {} types, {} edges",
        t0.elapsed().as_millis(),
        loaded.graph.nodes.len(),
        loaded.graph.edges.len(),
    );

    #[cfg(target_os = "macos")]
    selfshot::arm_if_requested();

    gpui_platform::application().run(move |cx: &mut App| {
        workspace::init(cx);
        let bounds = Bounds::centered(None, size(px(1440.), px(900.)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            |_, cx| cx.new(|cx| Workspace::new(loaded, path, overlay_path, cx)),
        )
        .unwrap();
        cx.activate(true);
    });
}
