//! Headless layout screenshots: cairn_shot out.png W H [page] [dark|light] [switcher|menu]
use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType};
use slint::platform::{Platform, WindowAdapter};
use slint::{ComponentHandle, ModelRc, PhysicalSize, Rgb8Pixel, SharedString, VecModel};
use std::rc::Rc;

slint::include_modules!();

struct P(Rc<MinimalSoftwareWindow>);
impl Platform for P {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
        Ok(self.0.clone())
    }
}

fn m<T: Clone + 'static>(v: Vec<T>) -> ModelRc<T> {
    ModelRc::new(VecModel::from(v))
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let (out, w, h) = (a[1].clone(), a[2].parse::<u32>().unwrap(), a[3].parse::<u32>().unwrap());
    let page = a.get(4).map(String::as_str).unwrap_or("web");
    let theme = a.get(5).map(String::as_str).unwrap_or("dark");
    let overlay = a.get(6).map(String::as_str).unwrap_or("");

    let win = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    slint::platform::set_platform(Box::new(P(win.clone()))).unwrap();
    let ui = MainWindow::new().unwrap();
    win.set_size(PhysicalSize::new(w, h));
    ui.global::<Palette>().set_color_scheme(if theme == "light" { slint::language::ColorScheme::Light } else { slint::language::ColorScheme::Dark });

    let col = |r, g, b| slint::Color::from_rgb_u8(r, g, b);
    let d = ui.global::<AppData>();
    d.set_containers(m(vec![
        ContainerRow { id: 0, name: "Default".into(), color: col(122, 162, 247), tabs: 3, selected: true },
        ContainerRow { id: 1, name: "Work".into(), color: col(158, 206, 106), tabs: 1, selected: false },
        ContainerRow { id: 2, name: "Shopping".into(), color: col(247, 118, 142), tabs: 0, selected: false },
    ]));
    let t = |kind, id, title: &str, sel, grouped, gc| TabRow { kind, id, title: title.into(), color: col(122, 162, 247), selected: sel, loading: false, group_color: gc, grouped, collapsed: false };
    let g = col(224, 175, 104);
    d.set_tabs(m(vec![
        t(1, 1, "Research", false, true, g),
        t(0, 1, "SirenRender", true, true, g),
        t(0, 2, "Perchance generators", false, true, g),
        t(0, 3, "A very long page title that should be elided nicely", false, false, col(122, 162, 247)),
    ]));
    d.set_tab_count(5);
    d.set_has_tab(true);
    d.set_url("https://search.brave.com/search?q=cairn".into());
    d.set_host("search.brave.com".into());
    d.set_status(SharedString::from("No tab open"));
    d.set_blocked_count(42);
    d.set_show_shield(true);
    d.set_block_ads(true);
    d.set_save_history(true);
    d.set_homepage("about:blank".into());
    d.set_engines(m(vec!["Brave Search".into(), "DuckDuckGo".into(), "DuckDuckGo Lite".into()]));
    d.set_engine(0);
    d.set_groups(m(vec![GroupRow { id: 1, name: "Research".into(), color: g, selected: true }]));
    d.set_folders(m(vec![FolderRow { id: 1, name: "Reading".into(), selected: false }]));
    d.set_folder_path("Bookmarks".into());
    d.set_bookmarked(true);
    d.set_history(m(vec![
        HistoryRow { title: "SirenRender".into(), url: "https://sirenrender.com/".into() },
        HistoryRow { title: "".into(), url: "https://perchance.org/welcome".into() },
    ]));
    d.set_bookmarks(m(vec![
        BookmarkRow { title: "Reading".into(), url: "".into(), folder_id: 1, is_folder: true },
        BookmarkRow { title: "Brave Search".into(), url: "https://search.brave.com/".into(), folder_id: 0, is_folder: false },
    ]));
    d.set_rules(m(vec![RuleRow { host: "perchance.org".into(), container: "Work".into() }]));
    d.set_page(match page {
        "settings" => Page::Settings,
        "history" => Page::History,
        "bookmarks" => Page::Bookmarks,
        "tab-options" => Page::TabOptions,
        _ => Page::Web,
    });
    ui.show().unwrap();
    // overlays are window-local state; drive them via the public test hook on the window
    match overlay {
        "switcher" => ui.set_switcher_open(true),
        "menu" => ui.set_menu_open(true),
        _ => {}
    }
    slint::platform::update_timers_and_animations();
    let mut buf = vec![Rgb8Pixel { r: 0, g: 0, b: 0 }; (w * h) as usize];
    for _ in 0..3 {
        win.draw_if_needed(|r| {
            r.render(&mut buf, w as usize);
        });
    }
    let f = std::fs::File::create(&out).unwrap();
    let mut enc = png::Encoder::new(std::io::BufWriter::new(f), w, h);
    enc.set_color(png::ColorType::Rgb);
    enc.set_depth(png::BitDepth::Eight);
    let bytes: Vec<u8> = buf.iter().flat_map(|p| [p.r, p.g, p.b]).collect();
    enc.write_header().unwrap().write_image_data(&bytes).unwrap();
}
