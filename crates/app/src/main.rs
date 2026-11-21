// No console window behind the app on Windows release builds.
#![cfg_attr(all(target_os = "windows", not(debug_assertions)), windows_subsystem = "windows")]
#![recursion_limit = "512"]

mod app;
mod dialogs;
mod discovery;
mod export_ui;
mod launch;
mod map_view;
mod otherwise;
mod panel;
mod services_ui;
mod settings;
mod share_ui;
mod star;
mod terminal;
mod theme;
mod ui;
mod util;

use gpui::*;
use gpui_component::Root;
use gpui_component_assets::Assets;
use std::borrow::Cow;
use std::path::PathBuf;

actions!(nodo, [Quit, OpenWebsite]);

pub(crate) const APP_NAME: &str = "Nodo"; // i18n-ok: product name
pub(crate) const COMPANY: &str = "Otherwise Technologies LLC"; // i18n-ok: company name
pub(crate) const COMPANY_URL: &str = "https://otherwisetech.com";
pub(crate) const REPO_URL: &str = "https://github.com/otherwisetech/nodo-rs";
pub(crate) const VERSION: &str = env!("CARGO_PKG_VERSION");

/// The native menu bar holds plain strings, so it is rebuilt on every language switch and
/// whenever the recent maps change.
pub(crate) fn set_app_menus(cx: &mut App) {
    use app::*;
    let app_menu = Menu {
        name: APP_NAME.into(),
        items: vec![
            MenuItem::action(i18n::t!("app.about.menu", app = APP_NAME), ShowAbout),
            MenuItem::action(i18n::t!("app.credit.menu", company = COMPANY), OpenWebsite),
            MenuItem::separator(),
            MenuItem::action(i18n::t!("app.menu.settings"), OpenSettings),
            MenuItem::separator(),
            MenuItem::action(i18n::t!("app.menu.quit", app = APP_NAME), Quit),
        ],
        disabled: false,
    };
    let recent = nodo_core::paths::recent();
    let mut recent_items: Vec<MenuItem> = recent
        .iter()
        .take(RECENT_ACTIONS)
        .enumerate()
        .map(|(i, p)| {
            let name = p.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
            recent_action(i, name)
        })
        .collect();
    if recent_items.is_empty() {
        recent_items.push(MenuItem::Action { name: i18n::t!("app.menu.no_recent").into(), action: Box::new(NewMap), os_action: None, checked: false, disabled: true });
    } else {
        recent_items.push(MenuItem::separator());
        recent_items.push(MenuItem::action(i18n::t!("app.menu.clear_recent"), ClearRecent));
    }
    let file = Menu {
        name: i18n::t!("app.menu.file").into(),
        items: vec![
            MenuItem::action(i18n::t!("app.file.new"), NewMap),
            MenuItem::action(i18n::t!("app.file.open"), OpenMap),
            MenuItem::submenu(Menu { name: i18n::t!("app.menu.open_recent").into(), items: recent_items, disabled: false }),
            MenuItem::separator(),
            MenuItem::action(i18n::t!("app.menu.import_csv"), ImportCsv),
            MenuItem::action(i18n::t!("app.menu.import_wg"), ImportWgDump),
            MenuItem::submenu(Menu {
                name: i18n::t!("app.menu.export").into(),
                items: vec![
                    MenuItem::action(i18n::t!("app.export.png"), ExportPng),
                    MenuItem::action(i18n::t!("app.export.pdf"), ExportPdf),
                    MenuItem::action(i18n::t!("app.export.svg"), ExportSvg),
                    MenuItem::action(i18n::t!("app.export.mermaid"), ExportMermaid),
                    MenuItem::separator(),
                    MenuItem::action(i18n::t!("app.export.csv"), ExportCsv),
                    MenuItem::action(i18n::t!("app.export.json"), ExportJson),
                ],
                disabled: false,
            }),
            MenuItem::separator(),
            MenuItem::action(i18n::t!("app.menu.close"), CloseMap),
        ],
        disabled: false,
    };
    let view = Menu {
        name: i18n::t!("app.menu.view").into(),
        items: vec![
            MenuItem::action(i18n::t!("app.view.toggle_list"), ToggleList),
            MenuItem::separator(),
            MenuItem::action(i18n::t!("app.layout.radial"), LayoutRadial),
            MenuItem::action(i18n::t!("app.layout.subnet"), LayoutSubnet),
            MenuItem::action(i18n::t!("app.layout.network"), LayoutNetwork),
            MenuItem::separator(),
            MenuItem::action(i18n::t!("app.map.zoom_in"), ZoomIn),
            MenuItem::action(i18n::t!("app.map.zoom_out"), ZoomOut),
            MenuItem::action(i18n::t!("app.map.zoom_reset"), ZoomReset),
            MenuItem::separator(),
            MenuItem::action(i18n::t!("app.search.menu"), FocusSearch),
        ],
        disabled: false,
    };
    let discover = Menu {
        name: i18n::t!("app.menu.discover").into(),
        items: vec![
            MenuItem::action(i18n::t!("app.discover.refresh"), Refresh),
            MenuItem::action(i18n::t!("app.discover.scan"), ScanSubnet),
            MenuItem::separator(),
            MenuItem::action(i18n::t!("app.discover.tailscale"), DiscoverTailscale),
            MenuItem::action(i18n::t!("app.discover.aws"), DiscoverAws),
            MenuItem::action(i18n::t!("app.discover.wireguard"), DiscoverWireGuard),
            MenuItem::separator(),
            MenuItem::action(i18n::t!("app.discover.add"), AddDevice),
            MenuItem::action(i18n::t!("app.monitor.toggle"), ToggleMonitor),
        ],
        disabled: false,
    };
    cx.set_menus(vec![app_menu, file, view, discover]);
}

/// gpui-component's icon set plus our own `icons/…`.
struct AppAssets;

impl AssetSource for AppAssets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        if path == LOGO_SVG {
            return Ok(Some(Cow::Borrowed(include_bytes!("../assets/icons/nodo-logo.svg"))));
        }
        if let Some(name) = path.strip_prefix("icons/kind/") {
            return Ok(kind_icon(name).map(Cow::Borrowed));
        }
        Assets.load(path)
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        Assets.list(path)
    }
}

/// Device and network icons drawn for Nodo.
fn kind_icon(file: &str) -> Option<&'static [u8]> {
    macro_rules! icons {
        ($($n:literal),*) => {
            match file {
                $(concat!($n, ".svg") => Some(include_bytes!(concat!("../assets/icons/kind/", $n, ".svg")) as &[u8]),)*
                _ => None,
            }
        };
    }
    icons!("router", "switch", "ap", "server", "nas", "camera", "printer", "tv", "speaker", "phone", "tablet", "laptop", "desktop", "iot", "vm", "vpn", "unknown", "root", "lan", "tailscale", "aws", "wireguard", "internet", "pin")
}

pub(crate) const LOGO_SVG: &str = "icons/nodo-logo.svg";
pub(crate) static ICON_PNG: &[u8] = include_bytes!("../assets/logo/nodo-icon-512.png");
pub(crate) static FONTS: [&[u8]; 3] = [
    include_bytes!("../assets/fonts/Inter-Regular.ttf"),
    include_bytes!("../assets/fonts/Inter-Medium.ttf"),
    include_bytes!("../assets/fonts/Inter-SemiBold.ttf"),
];

/// Sets the Dock icon at runtime: covers `cargo run` (no bundle) and stale Dock icon caches.
#[cfg(target_os = "macos")]
#[allow(unexpected_cfgs)] // objc 0.2 macros check a `cargo-clippy` feature
fn set_dock_icon() {
    use objc::runtime::Object;
    use objc::{class, msg_send, sel, sel_impl};
    static PNG: &[u8] = include_bytes!("../assets/logo/nodo-icon-1024.png");
    // SAFETY: plain AppKit calls on the main thread with a valid byte buffer that NSData copies.
    unsafe {
        let data: *mut Object = msg_send![class!(NSData), dataWithBytes: PNG.as_ptr() length: PNG.len()];
        let image: *mut Object = msg_send![class!(NSImage), alloc];
        let image: *mut Object = msg_send![image, initWithData: data];
        if image.is_null() {
            return;
        }
        let app: *mut Object = msg_send![class!(NSApplication), sharedApplication];
        let _: () = msg_send![app, setApplicationIconImage: image];
        let _: () = msg_send![image, release];
    }
}

fn main() {
    i18n::set_locale(settings::initial_language(&settings::Settings::load()));
    let arg = std::env::args().nth(1);
    if matches!(arg.as_deref(), Some("-h" | "--help")) {
        println!("{}", i18n::t!("app.cli.usage"));
        return;
    }
    // Linux / Windows hand a deep link over as the argument; `%u` may also turn a map into a
    // file:// URL
    let (initial, link) = match arg.as_deref() {
        Some(a) if a.starts_with("file://") => (file_url_to_path(a), None),
        Some(a) if a.contains("://") => (None, Some(a.to_string())),
        Some(a) => (Some(PathBuf::from(a)), None),
        None => (None, None),
    };
    otherwise::INBOUND.lock().unwrap_or_else(|e| e.into_inner()).extend(link.clone());
    std::thread::spawn(otherwise::register_scheme);
    // tailscale / aws / wg live on the login shell's PATH; resolve it while the window opens
    nodo_core::shell_env::init();
    let application = gpui_platform::application().with_assets(AppAssets);
    application.on_open_urls(|urls| {
        let (files, links): (Vec<String>, Vec<String>) = urls.into_iter().partition(|u| u.starts_with("file://"));
        otherwise::INBOUND.lock().unwrap_or_else(|e| e.into_inner()).extend(links);
        OPEN_FILES.lock().unwrap_or_else(|e| e.into_inner()).extend(files.iter().filter_map(|f| file_url_to_path(f)));
    });
    application.run(move |cx| {
        if let Err(e) = cx.text_system().add_fonts(FONTS.iter().map(|f| Cow::Borrowed(*f)).collect()) {
            eprintln!("fonts: {e}");
        }
        #[cfg(target_os = "macos")]
        set_dock_icon();
        gpui_component::init(cx);
        app::sync_theme(None, cx);
        terminal::panel::sync_style(cx);
        terminal::panel::bind_keys(cx);
        app::bind_keys(cx);
        cx.bind_keys([KeyBinding::new("secondary-q", Quit, None)]);
        cx.on_action(|_: &Quit, cx| cx.quit());
        cx.on_action(|_: &OpenWebsite, cx| cx.open_url(COMPANY_URL));
        // with no window focused (all closed on Linux/Windows never happens: we quit)
        cx.on_action(|_: &app::NewMap, cx| open_window(None, true, cx));
        gpui_component::set_locale(i18n::locale());
        set_app_menus(cx);
        cx.on_window_closed(|cx, _| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();
        // a link alone opens its own window (the pump decides which map)
        if link.is_none() || initial.is_some() {
            open_window(initial.clone(), false, cx);
        }
        cx.spawn(async move |cx| {
            let mut retry = Vec::new();
            loop {
                cx.background_executor().timer(std::time::Duration::from_millis(250)).await;
                cx.update(|cx| {
                    otherwise::pump(cx, &mut retry);
                    for p in std::mem::take(&mut *OPEN_FILES.lock().unwrap_or_else(|e| e.into_inner())) {
                        open_window(Some(p), false, cx);
                    }
                });
            }
        })
        .detach();
    });
}

/// Map files Finder asked us to open while running.
static OPEN_FILES: std::sync::Mutex<Vec<PathBuf>> = std::sync::Mutex::new(Vec::new());

/// `file:///Users/a/My%20map.nodo` → `/Users/a/My map.nodo`.
fn file_url_to_path(url: &str) -> Option<PathBuf> {
    let rest = url.strip_prefix("file://")?;
    let rest = rest.strip_prefix("localhost").unwrap_or(rest);
    let decoded = otherwise_link::link::decode(rest, false).ok()?;
    // file:///C:/x on Windows
    let decoded = if cfg!(windows) { decoded.trim_start_matches('/').to_string() } else { decoded };
    Some(PathBuf::from(decoded))
}

/// A new window: on `path`'s map if given, else the welcome screen (`new_map`: ask for a new
/// map's file right away).
pub(crate) fn open_window(path: Option<PathBuf>, new_map: bool, cx: &mut App) {
    let window_options = WindowOptions {
        window_bounds: Some(WindowBounds::centered(size(px(1440.), px(900.)), cx)),
        titlebar: Some(TitlebarOptions { title: Some(APP_NAME.into()), ..Default::default() }),
        ..Default::default()
    };
    cx.spawn(async move |cx| {
        cx.open_window(window_options, |window, cx| {
            let view = cx.new(|cx| app::NodoApp::new(path, new_map, window, cx));
            cx.new(|cx| Root::new(view, window, cx))
        })
        .expect("open window");
    })
    .detach();
}
