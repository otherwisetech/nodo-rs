//! File → Export (PNG, PDF, SVG, Mermaid, CSV, JSON) and the imports (CSV inventory,
//! WireGuard dump). The pictures come from the core's scene; PNG rasterises its SVG with resvg
//! and our own Inter fonts.
use crate::app::NodoApp;
use gpui::*;
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Png,
    Pdf,
    Svg,
    Mermaid,
    Csv,
    Json,
}

impl Format {
    fn ext(self) -> &'static str {
        match self {
            Format::Png => "png",
            Format::Pdf => "pdf",
            Format::Svg => "svg",
            Format::Mermaid => "mmd",
            Format::Csv => "csv",
            Format::Json => "json",
        }
    }
}

/// SVG → PNG at 2×, white-label fonts from the app bundle.
pub fn svg_to_png(svg: &str) -> Result<Vec<u8>, String> {
    use resvg::{tiny_skia, usvg};
    let mut opt = usvg::Options::default();
    for f in crate::FONTS {
        opt.fontdb_mut().load_font_data(f.to_vec());
    }
    opt.font_family = "Inter".into(); // i18n-ok: font name
    let tree = usvg::Tree::from_str(svg, &opt).map_err(|e| e.to_string())?;
    let size = tree.size();
    let scale = (8000. / size.width().max(size.height())).min(2.);
    let (w, h) = ((size.width() * scale).ceil() as u32, (size.height() * scale).ceil() as u32);
    let mut pixmap = tiny_skia::Pixmap::new(w.max(1), h.max(1)).ok_or_else(|| "pixmap".to_string())?;
    resvg::render(&tree, tiny_skia::Transform::from_scale(scale, scale), &mut pixmap.as_mut());
    pixmap.encode_png().map_err(|e| e.to_string())
}

impl NodoApp {
    fn export_bytes(&mut self, f: Format, cx: &App) -> Option<Vec<u8>> {
        let c = self.cached()?;
        let doc = self.doc.as_ref()?;
        let title = doc.title();
        let states = self.mon.states.clone();
        let now = nodo_core::time::now();
        let online = |id: nodo_core::model::DeviceId| doc.data.device(id).map(|d| crate::discovery::status_of(d, &states, now).online());
        let scene = || nodo_core::render::scene(&doc.data, &c.graph, &c.placed, &title, &online);
        let dark = crate::theme::tokens(cx).dark;
        Some(match f {
            Format::Png => match svg_to_png(&nodo_core::render::to_svg(&scene(), dark)) {
                Ok(b) => b,
                Err(_) => return None,
            },
            Format::Pdf => nodo_core::render::to_pdf(&scene()),
            Format::Svg => nodo_core::render::to_svg(&scene(), false).into_bytes(),
            Format::Mermaid => nodo_core::export::to_mermaid(&doc.data, &c.graph).into_bytes(),
            Format::Csv => nodo_core::export::to_csv(&doc.data).into_bytes(),
            Format::Json => nodo_core::export::to_json(&doc.data).into_bytes(),
        })
    }

    pub fn export(&mut self, f: Format, window: &mut Window, cx: &mut Context<Self>) {
        let Some(doc) = &self.doc else { return };
        let name = format!("{}.{}", doc.title(), f.ext());
        let dir = doc.store.path().parent().map(|p| p.to_path_buf()).unwrap_or_else(crate::app::documents_dir);
        let Some(bytes) = self.export_bytes(f, cx) else {
            self.toast(i18n::t!("app.export.failed", detail = f.ext()), cx);
            return;
        };
        let rx = cx.prompt_for_new_path(&dir, Some(&name));
        cx.spawn_in(window, async move |this, cx| {
            if let Ok(Ok(Some(path))) = rx.await {
                let r = std::fs::write(&path, &bytes);
                this.update(cx, |this, cx| match r {
                    Ok(()) => this.toast(i18n::t!("app.export.done", path = path.display()), cx),
                    Err(e) => this.toast(i18n::t!("app.export.failed", detail = e), cx),
                })
                .ok();
            }
        })
        .detach();
    }

    fn pick_file(&mut self, window: &mut Window, cx: &mut Context<Self>, then: fn(&mut NodoApp, PathBuf, &mut Context<NodoApp>)) {
        if self.doc.is_none() {
            return;
        }
        let rx = cx.prompt_for_paths(PathPromptOptions { files: true, directories: false, multiple: false, prompt: Some(i18n::t!("app.import.prompt").into()) });
        cx.spawn_in(window, async move |this, cx| {
            if let Ok(Ok(Some(paths))) = rx.await
                && let Some(p) = paths.into_iter().next()
            {
                this.update(cx, |this, cx| then(this, p, cx)).ok();
            }
        })
        .detach();
    }

    pub fn import_csv(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.pick_file(window, cx, |this, path, cx| {
            let r = std::fs::read_to_string(&path).map_err(|e| e.to_string()).and_then(|s| nodo_core::export::import_csv(&s));
            match r {
                Ok(rows) => {
                    let Some(doc) = this.doc.as_mut() else { return };
                    match doc.import(rows, nodo_core::time::now()) {
                        Ok(n) => this.toast(i18n::tn!("app.import.done", n), cx),
                        Err(e) => this.toast(i18n::t!("app.error.save", detail = e), cx),
                    }
                    this.retarget();
                    this.touch(cx);
                }
                Err(e) => this.toast(i18n::t!("app.import.failed", detail = e), cx),
            }
        });
    }

    pub fn import_wg_dump(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.pick_file(window, cx, |this, path, cx| match std::fs::read_to_string(&path) {
            Ok(s) => {
                let d = nodo_core::discover::wireguard::parse_dump(&s, nodo_core::time::now());
                if d.networks.is_empty() && d.devices.is_empty() {
                    this.toast(i18n::t!("app.wg.empty_dump"), cx);
                    return;
                }
                this.apply(d, &[nodo_core::model::Source::WireGuard], cx);
            }
            Err(e) => this.toast(i18n::t!("app.import.failed", detail = e), cx),
        });
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn png_from_svg() {
        i18n::set_locale("en");
        let m = MapFixture::sample();
        let g = nodo_core::graph::build(&m, &Default::default());
        let p = nodo_core::layout::layout(&g, nodo_core::layout::Mode::Radial);
        let s = nodo_core::render::scene(&m, &g, &p, "Test", &|_| Some(true));
        let png = super::svg_to_png(&nodo_core::render::to_svg(&s, false)).unwrap();
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
    }

    struct MapFixture;
    impl MapFixture {
        fn sample() -> nodo_core::db::MapData {
            use nodo_core::model::*;
            let networks = vec![Network { id: 1, name: "lan".into(), layer: Layer::Lan, cidr: Some("192.168.1.0/24".into()), gateway: Some("192.168.1.1".parse().unwrap()), parent: None, key: "lan".into(), facts: vec![] }];
            let dev = |id, name: &str, ip: &str| Device { id, name: name.into(), addrs: vec![Addr { ip: ip.parse().unwrap(), layer: Layer::Lan, network: Some(1), source: Source::Arp, last_seen: 0 }], ..Default::default() };
            nodo_core::db::MapData { networks, devices: vec![dev(1, "router", "192.168.1.1"), dev(2, "nas", "192.168.1.10"), dev(3, "tv", "192.168.1.20")] }
        }
    }
}
