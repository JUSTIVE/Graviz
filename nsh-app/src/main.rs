//! Graviz on Native Shell: a GraphQL schema explorer. The chrome is
//! nsh-metro on AppKit; the graph draws itself on the GPU (`graph`).

mod about;
mod chrome;
mod config;
mod graph;
mod landing;
mod loader;
mod model;
mod palette;
mod sdl;
mod panels;
mod selfshot;
mod settings;
mod shell;
mod store;
mod theme;
mod tree;
mod ui;
mod update_check;
mod workspace;

use std::path::PathBuf;

use nsh_core::prelude::*;
use nsh_core::desktop::MenuRole;
use nsh_core::element::desktop::{set_application_menu, MenuItem};
use nsh_metro::{FlyoutWidth, Mode as MetroMode, Theme as MetroTheme, settings_flyout};
use nsh_platform_macos as platform;

use crate::{
    config::ThemeMode,
    store::{Route, Store},
    ui::c,
};

/// Command-line arguments, read before the app starts.
#[derive(Default, Clone)]
struct Args {
    path: Option<PathBuf>,
    overlay: Option<String>,
}

fn args() -> Args {
    let mut it = std::env::args().skip(1);
    let mut a = Args::default();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--overlay" => a.overlay = it.next().and_then(|p| std::fs::read_to_string(p).ok()),
            "--help" | "-h" => {
                eprintln!("usage: graviz [<schema.graphql>] [--overlay <overlay.graphql>]");
                std::process::exit(0);
            }
            _ => a.path = Some(PathBuf::from(arg)),
        }
    }
    a
}

#[component]
pub fn App() -> impl IntoElement {
    let store = Store::new();
    let s0 = store.settings.get_untracked();
    MetroTheme::set_accent(theme::accent_from_name(&s0.accent));
    // The ground follows the setting, and for "system" the window.
    effect(move || {
        let mode = store.theme_mode();
        let dark = match mode {
            ThemeMode::Light => false,
            ThemeMode::Dark => true,
            ThemeMode::System => {
                if std::env::var("GRAVIZ_LIGHT").is_ok() {
                    false
                } else if std::env::var("GRAVIZ_DARK").is_ok() {
                    true
                } else {
                    dark()
                }
            }
        };
        MetroTheme::set_mode(if dark { MetroMode::Dark } else { MetroMode::Light });
    });
    effect(move || {
        let reduce = reduce_motion();
        MetroTheme::set_reduced_motion(reduce);
    });

    let a = args();
    if let Some(path) = a.path.clone() {
        store.open_initial(path, a.overlay.clone());
        if std::env::var("GRAVIZ_ABOUT").is_ok() {
            store.route.set(Route::About);
        }
    }

    // One blocking GET to GitHub's releases, off the engine thread.
    spawn(async move {
        let info = blocking(update_check::check_for_update).await;
        if let Ok(Some(info)) = info {
            store.update.set(Some((info.version, info.url)));
        }
    });

    shortcuts(store);
    workspace::watch_schema(store);

    let settings = settings_flyout(store.settings_open)
        .title("settings")
        .width(FlyoutWidth::Wide)
        .child(move || settings::page(store))
        .on_dismiss(move || store.settings_open.set(false));

    view()
        .flex_1()
        .relative()
        .bg(c(|t| t.bg))
        .on_file_drag_enter(move |_: &FileDragEvent| store.dragging.set(true))
        .on_file_drag_leave(move || store.dragging.set(false))
        .on_file_drop(move |e: &FileDropEvent| {
            store.dragging.set(false);
            if let Some(path) = e.uris.first().and_then(|u| file_path(u)) {
                store.open_path(path);
            }
        })
        .child(shell::header(store))
        .child(shell::nav(store))
        .child(
            view()
                .flex_1()
                .min_h(px(0.))
                .relative()
                // The workspace stays mounted under the other screens, as
                // the gpui root kept its entity: the graph keeps its camera,
                // focus and history across a visit to About or New.
                .child(when(move || store.has_schema(), move || workspace::page(store)))
                .child(dynamic(move || match store.route.get() {
                    Route::View if store.has_schema() => view().into_element(),
                    Route::About => screen(about::page(store)),
                    _ => screen(landing::page(store)),
                })),
        )
        .child(shell::commit_badge())
        .child(settings)
}

/// A full screen over the workspace, turnstiling in as Metro pages do.
fn screen(el: impl IntoElement) -> AnyElement {
    view()
        .absolute()
        .inset_0()
        .bg(c(|t| t.bg))
        .child(nsh_metro::motion::turnstile_in(nsh_metro::motion::Direction::Forward, 0., 0., view().flex_1().min_h(px(0.)).child(el)).flex_1().min_h(px(0.)))
        .into_element()
}

/// `file:///a/b.graphql` → `/a/b.graphql`.
fn file_path(uri: &str) -> Option<PathBuf> {
    let rest = uri.strip_prefix("file://")?;
    let mut out = Vec::with_capacity(rest.len());
    let b = rest.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Ok(v) = u8::from_str_radix(&rest[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    Some(PathBuf::from(String::from_utf8_lossy(&out).into_owned()))
}

/// The keyboard: the gpui app's bindings, on the same keys.
fn shortcuts(store: Store) {
    let ws = move || store.has_schema() && store.route.get_untracked() == Route::View;
    on_key("mod+k", move |_| {
        if ws() {
            let open = !store.palette_open.get_untracked();
            store.palette_open.set(open);
            if open {
                store.palette_gen.update(|g| *g += 1);
            }
        }
    });
    on_key("mod+b", move |_| {
        if ws() {
            store.set_settings(|s| s.sidebar_open = !s.sidebar_open);
        }
    });
    on_key("mod+d", move |_| {
        if ws() {
            store.toggle_option(|s| s.show_descriptions = !s.show_descriptions);
        }
    });
    on_key("mod+e", move |_| {
        if ws() {
            store.toggle_option(|s| s.bundle_edges = !s.bundle_edges);
        }
    });
    on_key("mod+shift+s", move |_| {
        if ws() {
            store.toggle_option(|s| s.hide_custom_scalars = !s.hide_custom_scalars);
        }
    });
    on_key("mod+i", move |_| {
        if ws() {
            store.investigate.update(|v| *v = !*v);
        }
    });
    on_key("mod+p", move |_| {
        if ws() {
            store.toggle_option(|s| s.hide_primitive_fields = !s.hide_primitive_fields);
        }
    });
    on_key("mod+r", move |_| {
        if ws() {
            store.toggle_relay();
        }
    });
    on_key("mod+u", move |_| {
        if ws() {
            store.dock_open.update(|v| *v = !*v);
        }
    });
    on_key("mod+o", move |_| store.open_dialog());
    on_key("mod+shift+o", move |_| {
        if ws() {
            store.open_overlay_file();
        }
    });
    // gpui's menu bar: the app menu with Settings… and Quit. The Edit and
    // Window menus are the platform's own, which the native text fields
    // need for ⌘C / ⌘V / ⌘Z.
    set_application_menu(vec![
        MenuItem::submenu(
            "Graviz",
            [
                MenuItem::role(MenuRole::About),
                MenuItem::separator(),
                MenuItem::new("Settings…").shortcut("mod+,").on_select(move || store.settings_open.update(|v| *v = !*v)),
                MenuItem::separator(),
                MenuItem::role(MenuRole::Hide),
                MenuItem::role(MenuRole::HideOthers),
                MenuItem::role(MenuRole::ShowAll),
                MenuItem::separator(),
                MenuItem::role(MenuRole::Quit).label("Quit Graviz"),
            ],
        ),
        MenuItem::submenu(
            "Edit",
            [
                MenuItem::role(MenuRole::Undo),
                MenuItem::role(MenuRole::Redo),
                MenuItem::separator(),
                MenuItem::role(MenuRole::Cut),
                MenuItem::role(MenuRole::Copy),
                MenuItem::role(MenuRole::Paste),
                MenuItem::role(MenuRole::SelectAll),
            ],
        ),
        MenuItem::submenu(
            "Window",
            [MenuItem::role(MenuRole::Minimize), MenuItem::role(MenuRole::Zoom), MenuItem::role(MenuRole::ToggleFullScreen)],
        ),
    ]);
    on_key("mod+[", move |_| {
        if ws() {
            store.command("back", serde_json::Value::Null);
        }
    });
    on_key("escape", move |_| {
        if store.palette_open.get_untracked() {
            store.palette_open.set(false);
        } else if store.settings_open.get_untracked() {
            store.settings_open.set(false);
        } else if store.menu.with_untracked(Option::is_some) {
            store.command("closeMenu", serde_json::Value::Null);
        } else {
            store.command("clearEdge", serde_json::Value::Null);
        }
    });
}

fn main() -> impl std::process::Termination {
    selfshot::arm_if_requested();
    platform::set_components(|components| {
        graph::register(components);
        chrome::register(components);
    });
    platform::run_app(
        platform::AppSource::Rust(platform::Root::of::<App>()),
        platform::AppOptions {
            title: "Graviz".to_owned(),
            app_id: "dev.justive.graviz".to_owned(),
            size: (1440, 900),
            ..Default::default()
        },
    )
}
