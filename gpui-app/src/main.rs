mod canvas;
mod config;
mod loader;
mod model;
mod root;
#[cfg(target_os = "macos")]
mod selfshot;
mod theme;
mod tree;
mod workspace;

use gpui::{prelude::*, px, size, App, Bounds, WindowBounds, WindowOptions};
use root::Root;
use std::path::PathBuf;

fn main() {
    let mut args = std::env::args().skip(1);
    let mut path: Option<PathBuf> = None;
    let mut overlay_path: Option<PathBuf> = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--overlay" => overlay_path = args.next().map(PathBuf::from),
            "--help" | "-h" => {
                eprintln!("usage: gompassql [<schema.graphql>] [--overlay <overlay.graphql>]");
                std::process::exit(0);
            }
            _ => path = Some(PathBuf::from(arg)),
        }
    }

    // With a path, load up front (fail fast); without one, open the landing
    // screen with the recent-schema list.
    let initial = path.map(|path| {
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
        (loaded, path, overlay_path)
    });

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
            |_, cx| cx.new(|cx| Root::new(initial, cx)),
        )
        .unwrap();
        cx.activate(true);
    });
}
