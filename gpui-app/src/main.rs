mod about;
mod canvas;
mod config;
mod editor;
mod field;
mod filepanel;
mod icons;
mod landing;
mod loader;
mod model;
mod palette;
mod panels;
mod root;
mod settings;
mod shell;
#[cfg(target_os = "macos")]
mod selfshot;
mod textedit;
mod theme;
mod tree;
mod update_check;
mod workspace;

use gpui::{
    actions, prelude::*, px, size, App, Bounds, KeyBinding, Menu, MenuItem, WindowBounds,
    WindowOptions,
};
use root::Root;

actions!(graviz, [Quit]);

/// Append panics to `panic.log` beside the settings, with a backtrace.
fn install_panic_log() {
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let at = info
            .location()
            .map(|l| format!("{}:{}", l.file(), l.line()))
            .unwrap_or_else(|| "?".into());
        let trace = std::backtrace::Backtrace::force_capture();
        let line = format!(
            "\n=== {} panic at {at}\n{info}\n{trace}\n",
            chrono_stamp()
        );
        if let Some(path) = config::panic_log_path() {
            use std::io::Write;
            if let Ok(mut f) =
                std::fs::OpenOptions::new().create(true).append(true).open(path)
            {
                let _ = f.write_all(line.as_bytes());
            }
        }
        eprintln!("{line}");
        prev(info);
    }));
}

/// Seconds since the epoch. A real clock would mean a dependency, and what
/// this has to answer is only "which run was this".
fn chrono_stamp() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
use std::path::PathBuf;

fn main() {
    let mut args = std::env::args().skip(1);
    let mut path: Option<PathBuf> = None;
    let mut overlay_path: Option<PathBuf> = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--overlay" => overlay_path = args.next().map(PathBuf::from),
            "--help" | "-h" => {
                eprintln!("usage: graviz [<schema.graphql>] [--overlay <overlay.graphql>]");
                std::process::exit(0);
            }
            _ => path = Some(PathBuf::from(arg)),
        }
    }

    // With a path, load up front (fail fast); without one, open the landing
    // screen with the recent-schema list.
    let overlay_text = overlay_path.and_then(|p| std::fs::read_to_string(p).ok());
    let hide_relay = config::load_settings().hide_relay;
    let initial = path.map(|path| {
        let t0 = std::time::Instant::now();
        let loaded =
            loader::load(&path, overlay_text.as_deref(), hide_relay).unwrap_or_else(|e| {
                eprintln!("{e:#}");
                std::process::exit(1);
            });
        eprintln!(
            "parsed in {}ms — {} types, {} edges",
            t0.elapsed().as_millis(),
            loaded.graph.nodes.len(),
            loaded.graph.edges.len(),
        );
        (loaded, path, overlay_text)
    });

    // A panic inside a GUI process leaves nothing behind: no terminal is
    // watching stderr, and an unwind out of the render loop takes the window
    // with it. Write it next to the settings so the next report comes with
    // the reason attached.
    install_panic_log();

    #[cfg(target_os = "macos")]
    selfshot::arm_if_requested();

    gpui_platform::application().with_assets(icons::Assets).run(move |cx: &mut App| {
        workspace::init(cx);
        // The window has no system titlebar, but the app still needs the
        // standard menu: without it macOS gives ⌘Q to nothing.
        cx.bind_keys([KeyBinding::new("cmd-q", Quit, None)]);
        cx.on_action(|_: &Quit, cx: &mut App| cx.quit());
        cx.set_menus(vec![Menu {
            name: "Graviz".into(),
            items: vec![
                MenuItem::action("Settings…", workspace::OpenSettings),
                MenuItem::separator(),
                MenuItem::action("Quit Graviz", Quit),
            ],
            disabled: false,
        }]);
        let bounds = Bounds::centered(None, size(px(1440.), px(900.)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: Some(gpui::TitlebarOptions {
                    title: Some("Graviz".into()),
                    appears_transparent: true,
                    // Centred in the title strip: (strip - 12pt button) / 2.
                    traffic_light_position: Some(gpui::point(
                        px(10.0),
                        px((shell::TITLEBAR_H - 12.0) / 2.0),
                    )),
                }),
                ..Default::default()
            },
            |_, cx| cx.new(|cx| Root::new(initial, cx)),
        )
        .unwrap();
        cx.activate(true);
    });
}
