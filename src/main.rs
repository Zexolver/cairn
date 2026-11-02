mod engine;
#[cfg(feature = "servo")]
mod servo_engine;

use cairn_core::{
    blocker::Blocker,
    container::Workspace,
    library::Library,
    lists, nav,
    settings::{host_of, Settings, SEARCH_ENGINES, Theme},
};
use engine::{Engine, Event, Input, StubEngine};
use slint::language::ColorScheme;
use slint::{ComponentHandle, Image, ModelRc, Rgba8Pixel, SharedPixelBuffer, SharedString, VecModel};
use std::{cell::RefCell, collections::HashMap, path::PathBuf, rc::Rc, time::Duration};

slint::include_modules!();

struct State {
    ws: Workspace,
    selected_container: u32,
    engines: HashMap<u32, Box<dyn Engine>>,
    viewport: (u32, u32),
    blocked: u32,
    settings: Settings,
    library: Library,
    folder_cwd: Option<u32>,
    history_query: String,
    data_dir: PathBuf,
}

impl State {
    fn engine_for(&mut self, container: u32) -> &mut Box<dyn Engine> {
        let vp = self.viewport;
        let dir = self.data_dir.clone();
        self.engines.entry(container).or_insert_with(|| {
            #[cfg(feature = "servo")]
            let mut e: Box<dyn Engine> = Box::new(servo_engine::ServoEngine::new(&dir.join("containers").join(container.to_string()), vp));
            #[cfg(not(feature = "servo"))]
            let mut e: Box<dyn Engine> = Box::new(StubEngine::default());
            e.resize(vp.0, vp.1);
            e
        })
    }

    fn active_container(&self) -> Option<u32> {
        let a = self.ws.active?;
        self.ws.tabs.iter().find(|t| t.id == a).map(|t| t.container)
    }

    fn save_settings(&self) {
        let _ = self.settings.save(&self.data_dir.join("settings.json"));
    }

    fn save_library(&self) {
        let _ = self.library.save(&self.data_dir.join("library.json"));
    }

    fn container_name(&self, id: u32) -> String {
        self.ws.containers.iter().find(|c| c.id == id).map(|c| c.name.clone()).unwrap_or_default()
    }

    /// Moves a tab to another container: its engine session is closed and reopened in the target.
    fn move_tab(&mut self, tab: u32, to: u32) {
        let Some(t) = self.ws.tabs.iter().find(|t| t.id == tab).cloned() else { return };
        if t.container == to || !self.ws.containers.iter().any(|c| c.id == to) {
            return;
        }
        self.engine_for(t.container).close(tab);
        if let Some(m) = self.ws.tab_mut(tab) {
            m.container = to;
            m.group = None;
        }
        self.ws.prune_groups();
        self.engine_for(to).open(tab, &t.url);
        if self.ws.active == Some(tab) {
            self.selected_container = to;
        }
    }

    fn open_tab(&mut self, url: &str) {
        let c = self.settings.container_for(url).unwrap_or(self.selected_container);
        if let Ok(id) = self.ws.open_tab(c, url) {
            self.selected_container = c;
            self.engine_for(c).open(id, url);
        }
    }

    fn navigate_active(&mut self, url: &str) {
        match self.ws.active {
            Some(tab) => {
                if let Some(t) = self.ws.tab_mut(tab) {
                    t.url = url.to_string();
                }
                if let Some(to) = self.settings.container_for(url) {
                    self.move_tab(tab, to);
                    return;
                }
                let c = self.active_container().unwrap_or(self.selected_container);
                self.engine_for(c).navigate(tab, url);
            }
            None => self.open_tab(url),
        }
    }
}

fn color(hex: &str) -> slint::Color {
    let v = u32::from_str_radix(hex.trim_start_matches('#'), 16).unwrap_or(0x7aa2f7);
    slint::Color::from_rgb_u8((v >> 16) as u8, (v >> 8) as u8, v as u8)
}

fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn apply_theme(ui: &MainWindow, theme: Theme) {
    ui.global::<Palette>().set_color_scheme(match theme {
        Theme::System => ColorScheme::Unknown,
        Theme::Light => ColorScheme::Light,
        Theme::Dark => ColorScheme::Dark,
    });
}

fn model<T: Clone + 'static>(v: Vec<T>) -> ModelRc<T> {
    ModelRc::new(VecModel::from(v))
}

fn refresh(ui: &MainWindow, st: &mut State) {
    let d = ui.global::<AppData>();
    let rows: Vec<ContainerRow> = st
        .ws
        .containers
        .iter()
        .map(|c| ContainerRow {
            id: c.id as i32,
            name: c.name.clone().into(),
            color: color(&c.color),
            tabs: st.ws.tabs_in(c.id) as i32,
            selected: c.id == st.selected_container,
        })
        .collect();
    d.set_containers(model(rows));

    let sel = st.selected_container;
    let tab_color = st.ws.containers.iter().find(|c| c.id == sel).map(|c| color(&c.color)).unwrap_or_default();
    let active = st.ws.active;
    let mut tabs: Vec<TabRow> = vec![];
    let row = |t: &cairn_core::container::Tab, gc: slint::Color, grouped: bool| TabRow {
        kind: 0,
        id: t.id as i32,
        title: t.title.clone().into(),
        color: tab_color,
        selected: active == Some(t.id),
        loading: t.loading,
        group_color: gc,
        grouped,
        collapsed: false,
    };
    for g in st.ws.groups.iter().filter(|g| g.container == sel) {
        let gc = color(&g.color);
        tabs.push(TabRow {
            kind: 1,
            id: g.id as i32,
            title: g.name.clone().into(),
            color: gc,
            selected: false,
            loading: false,
            group_color: gc,
            grouped: true,
            collapsed: g.collapsed,
        });
        if !g.collapsed {
            tabs.extend(st.ws.tabs.iter().filter(|t| t.group == Some(g.id)).map(|t| row(t, gc, true)));
        }
    }
    tabs.extend(st.ws.tabs.iter().filter(|t| t.container == sel && t.group.is_none()).map(|t| row(t, tab_color, false)));
    d.set_tabs(model(tabs));
    d.set_tab_count(st.ws.tabs.len() as i32);

    let tab = active.and_then(|a| st.ws.tabs.iter().find(|t| t.id == a)).cloned();
    d.set_has_tab(tab.is_some());
    let url = tab.as_ref().map(|t| t.url.clone()).unwrap_or_default();
    let shown = if url == "about:blank" { String::new() } else { url.clone() };
    d.set_url(shown.into());
    let host = host_of(&url).unwrap_or_default();
    d.set_has_rule(st.settings.site_rules.iter().any(|r| r.host == host));
    d.set_host(host.into());
    d.set_loading(tab.as_ref().is_some_and(|t| t.loading));
    d.set_bookmarked(st.library.is_bookmarked(&url));
    d.set_groups(model(
        st.ws
            .groups
            .iter()
            .filter(|g| g.container == sel)
            .map(|g| GroupRow { id: g.id as i32, name: g.name.clone().into(), color: color(&g.color), selected: tab.as_ref().is_some_and(|t| t.group == Some(g.id)) })
            .collect(),
    ));
    d.set_status(SharedString::from("No tab open. Press + to start."));
    d.set_blocked_count(st.blocked as i32);

    d.set_theme(st.settings.theme.index());
    d.set_engine(SEARCH_ENGINES.iter().position(|e| e.id == st.settings.search_engine).unwrap_or(0) as i32);
    d.set_engines(model(SEARCH_ENGINES.iter().map(|e| SharedString::from(e.name)).collect()));
    d.set_homepage(st.settings.homepage.clone().into());
    d.set_block_ads(st.settings.block_ads);
    d.set_show_shield(st.settings.show_shield_count);
    d.set_save_history(st.settings.save_history);
    d.set_rules(model(
        st.settings.site_rules.iter().map(|r| RuleRow { host: r.host.clone().into(), container: st.container_name(r.container).into() }).collect(),
    ));

    d.set_history(model(
        st.library.recent(&st.history_query, 200).into_iter().map(|h| HistoryRow { title: h.title.clone().into(), url: h.url.clone().into() }).collect(),
    ));
    let mut bm: Vec<BookmarkRow> = st
        .library
        .subfolders(st.folder_cwd)
        .into_iter()
        .map(|f| BookmarkRow { title: f.name.clone().into(), url: "".into(), folder_id: f.id as i32, is_folder: true })
        .collect();
    bm.extend(st.library.bookmarks_in(st.folder_cwd).into_iter().map(|b| BookmarkRow {
        title: if b.title.is_empty() { b.url.clone().into() } else { b.title.clone().into() },
        url: b.url.clone().into(),
        folder_id: b.folder.unwrap_or(0) as i32,
        is_folder: false,
    }));
    d.set_bookmarks(model(bm));
    d.set_folders(model(st.library.folders.iter().map(|f| FolderRow { id: f.id as i32, name: f.name.clone().into(), selected: false }).collect()));
    let path = st.folder_cwd.and_then(|f| st.library.folders.iter().find(|x| x.id == f)).map(|f| f.name.clone()).unwrap_or_else(|| "Bookmarks".into());
    d.set_folder_path(path.into());
}

fn paint(ui: &MainWindow, st: &mut State) {
    let (Some(tab), Some(c)) = (st.ws.active, st.active_container()) else { return };
    if let Some(f) = st.engine_for(c).frame(tab) {
        let buf = SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(&f.rgba, f.width, f.height);
        ui.global::<AppData>().set_frame(Image::from_rgba8(buf));
    }
}

fn opt(id: i32) -> Option<u32> {
    (id > 0).then_some(id as u32)
}

fn main() -> Result<(), slint::PlatformError> {
    #[cfg(feature = "servo")]
    rustls::crypto::aws_lc_rs::default_provider().install_default().ok();
    if std::env::var_os("SLINT_BACKEND").is_none() {
        // FemtoVG's EGL surface breaks once Servo initialises GL ("bad surface" swap error); the page is a CPU image anyway.
        unsafe { std::env::set_var("SLINT_BACKEND", "winit-software") };
    }
    let ui = MainWindow::new()?;
    let data_dir = std::env::var_os("CAIRN_DATA").map(PathBuf::from).unwrap_or_else(|| std::env::temp_dir().join("cairn"));
    let blocker = Rc::new(Blocker::new());
    blocker.load_lists(lists::load_cached(&data_dir.join("lists")));

    let settings = Settings::load(&data_dir.join("settings.json"));
    let library = Library::load(&data_dir.join("library.json"));
    apply_theme(&ui, settings.theme);

    let st = Rc::new(RefCell::new(State {
        ws: Workspace::default(),
        selected_container: 0,
        engines: HashMap::new(),
        viewport: (800, 600),
        blocked: 0,
        settings,
        library,
        folder_cwd: None,
        history_query: String::new(),
        data_dir,
    }));
    let logic = ui.global::<Logic>();
    let weak = ui.as_weak();

    macro_rules! on {
        ($reg:ident, |$st:ident, $ui:ident $(, $a:ident)*| $body:block) => {{
            let st = st.clone();
            let weak = weak.clone();
            logic.$reg(move |$($a),*| {
                let $ui = weak.unwrap();
                let mut $st = st.borrow_mut();
                $body
                refresh(&$ui, &mut $st);
                paint(&$ui, &mut $st);
            });
        }};
    }

    on!(on_select_container, |s, _ui, id| {
        s.selected_container = id as u32;
        let c = id as u32;
        s.ws.active = s.ws.tabs.iter().find(|t| t.container == c).map(|t| t.id);
    });
    on!(on_add_container, |s, _ui, name| {
        if let Ok(id) = s.ws.add_container(&name) {
            s.selected_container = id;
            s.ws.active = None;
        }
    });
    on!(on_delete_container, |s, _ui, id| {
        if let Ok(gone) = s.ws.remove_container(id as u32) {
            s.engines.remove(&(id as u32));
            let _ = gone;
            s.settings.forget_container(id as u32);
            s.save_settings();
            if s.selected_container == id as u32 {
                s.selected_container = s.ws.containers.first().map(|c| c.id).unwrap_or(0);
                let c = s.selected_container;
                s.ws.active = s.ws.tabs.iter().find(|t| t.container == c).map(|t| t.id);
            }
        }
    });
    on!(on_select_tab, |s, _ui, id| {
        s.ws.active = Some(id as u32);
    });
    on!(on_close_tab, |s, _ui, id| {
        if let Some(tab) = s.ws.close_tab(id as u32) {
            s.engine_for(tab.container).close(tab.id);
            if s.ws.active.is_none() {
                let c = s.selected_container;
                s.ws.active = s.ws.tabs.iter().find(|t| t.container == c).map(|t| t.id);
            }
        }
    });
    on!(on_new_tab, |s, _ui| {
        let home = s.settings.homepage.clone();
        s.open_tab(&home);
    });
    on!(on_navigate, |s, _ui, text| {
        let url = nav::resolve_input(&text, s.settings.search_template());
        s.navigate_active(&url);
    });
    on!(on_open_url, |s, ui, url, new_tab| {
        let url = url.to_string();
        if new_tab {
            s.open_tab(&url);
        } else {
            s.navigate_active(&url);
        }
        ui.global::<AppData>().set_page(Page::Web);
    });
    macro_rules! simple {
        ($reg:ident, $m:ident) => {
            on!($reg, |s, _ui| {
                if let Some((tab, c)) = s.ws.active.zip(s.active_container()) {
                    s.engine_for(c).$m(tab);
                }
            });
        };
    }
    simple!(on_back, back);
    simple!(on_forward, forward);
    simple!(on_reload, reload);
    on!(on_refresh, |_s, _ui| {});
    on!(on_viewport_resized, |s, _ui, w, h| {
        s.viewport = (w.max(1.0) as u32, h.max(1.0) as u32);
        let vp = s.viewport;
        for e in s.engines.values_mut() {
            e.resize(vp.0, vp.1);
        }
    });

    on!(on_toggle_bookmark, |s, _ui| {
        if let Some(t) = s.ws.active.and_then(|a| s.ws.tabs.iter().find(|t| t.id == a)).cloned() {
            if s.library.is_bookmarked(&t.url) {
                let found: Vec<_> = s.library.bookmarks.iter().filter(|b| b.url == t.url).map(|b| (b.url.clone(), b.folder)).collect();
                for (u, f) in found {
                    s.library.remove_bookmark(&u, f);
                }
            } else {
                s.library.add_bookmark(&t.url, &t.title, None);
            }
            s.save_library();
        }
    });
    on!(on_bookmark_in, |s, ui, folder| {
        if let Some(t) = s.ws.active.and_then(|a| s.ws.tabs.iter().find(|t| t.id == a)).cloned() {
            let found: Vec<_> = s.library.bookmarks.iter().filter(|b| b.url == t.url).map(|b| (b.url.clone(), b.folder)).collect();
            for (u, f) in found {
                s.library.remove_bookmark(&u, f);
            }
            s.library.add_bookmark(&t.url, &t.title, opt(folder));
            s.save_library();
        }
        ui.global::<AppData>().set_page(Page::Web);
    });
    on!(on_add_folder, |s, _ui, name| {
        let cwd = s.folder_cwd;
        s.library.add_folder(&name, cwd);
        s.save_library();
    });
    on!(on_open_folder, |s, _ui, id| {
        s.folder_cwd = opt(id);
    });
    on!(on_folder_up, |s, _ui| {
        s.folder_cwd = s.folder_cwd.and_then(|f| s.library.folders.iter().find(|x| x.id == f)).and_then(|f| f.parent);
    });
    on!(on_remove_bookmark, |s, _ui, url, folder| {
        s.library.remove_bookmark(&url, opt(folder));
        s.save_library();
    });
    on!(on_remove_folder, |s, _ui, id| {
        s.library.remove_folder(id as u32);
        s.save_library();
    });
    on!(on_history_query, |s, _ui, q| {
        s.history_query = q.to_string();
    });
    on!(on_clear_history, |s, _ui| {
        s.library.clear_history();
        s.save_library();
    });

    on!(on_toggle_group, |s, _ui, id| {
        s.ws.toggle_group(id as u32);
    });
    on!(on_set_tab_group, |s, ui, id| {
        if let Some(a) = s.ws.active {
            s.ws.set_tab_group(a, opt(id));
        }
        ui.global::<AppData>().set_page(Page::Web);
    });
    on!(on_new_group, |s, ui, name| {
        if let Some((a, c)) = s.ws.active.zip(s.active_container()) {
            if let Some(g) = s.ws.add_group(c, &name) {
                s.ws.set_tab_group(a, Some(g));
            }
        }
        ui.global::<AppData>().set_page(Page::Web);
    });
    on!(on_move_tab, |s, ui, id| {
        if let Some(a) = s.ws.active {
            s.move_tab(a, id as u32);
        }
        ui.global::<AppData>().set_page(Page::Web);
    });
    on!(on_set_rule, |s, ui, id| {
        let host = s.ws.active.and_then(|a| s.ws.tabs.iter().find(|t| t.id == a)).and_then(|t| host_of(&t.url));
        if let Some(h) = host {
            s.settings.set_rule(&h, id as u32);
            s.save_settings();
            if let Some(a) = s.ws.active {
                s.move_tab(a, id as u32);
            }
        }
        ui.global::<AppData>().set_page(Page::Web);
    });
    on!(on_remove_rule, |s, _ui, host| {
        s.settings.remove_rule(&host);
        s.save_settings();
    });

    on!(on_set_theme, |s, ui, i| {
        s.settings.theme = Theme::from_index(i);
        apply_theme(&ui, s.settings.theme);
        s.save_settings();
    });
    on!(on_set_engine, |s, _ui, i| {
        if let Some(e) = SEARCH_ENGINES.get(i as usize) {
            s.settings.search_engine = e.id.to_string();
            s.save_settings();
        }
    });
    on!(on_set_homepage, |s, _ui, h| {
        let h = h.trim();
        s.settings.homepage = if h.is_empty() { "about:blank".into() } else { nav::resolve_input(h, s.settings.search_template()) };
        s.save_settings();
    });
    on!(on_set_block_ads, |s, _ui, v| {
        s.settings.block_ads = v;
        s.save_settings();
    });
    on!(on_set_show_shield, |s, _ui, v| {
        s.settings.show_shield_count = v;
        s.save_settings();
    });
    on!(on_set_save_history, |s, _ui, v| {
        s.settings.save_history = v;
        s.save_settings();
    });

    macro_rules! input {
        ($reg:ident, |$($a:ident),*| $inp:expr) => {{
            let st = st.clone();
            logic.$reg(move |$($a),*| {
                let mut s = st.borrow_mut();
                if let Some((tab, c)) = s.ws.active.zip(s.active_container()) {
                    s.engine_for(c).input(tab, $inp);
                }
            });
        }};
    }
    input!(on_pointer_moved, |x, y| Input::Move(x, y));
    input!(on_pointer_pressed, |x, y, b| Input::Down(x, y, b as u8));
    input!(on_pointer_released, |x, y, b| Input::Up(x, y, b as u8));
    input!(on_scrolled, |x, y, dx, dy| Input::Scroll(x, y, dx, dy));
    input!(on_key_event, |k, down| Input::Key(k.to_string(), down));

    {
        let mut s = st.borrow_mut();
        let home = std::env::var("CAIRN_URL").unwrap_or_else(|_| s.settings.homepage.clone());
        s.open_tab(&home);
        refresh(&ui, &mut s);
    }

    let timer = slint::Timer::default();
    {
        let st = st.clone();
        let weak = ui.as_weak();
        timer.start(slint::TimerMode::Repeated, Duration::from_millis(16), move || {
            let ui = weak.unwrap();
            let mut s = st.borrow_mut();
            let mut changed = false;
            let mut moves: Vec<(u32, u32)> = vec![];
            let mut lib_dirty = false;
            let ids: Vec<u32> = s.engines.keys().copied().collect();
            for c in ids {
                for ev in s.engines.get_mut(&c).unwrap().pump() {
                    changed = true;
                    match ev {
                        Event::Title(t, v) => {
                            let url = s.ws.tabs.iter().find(|x| x.id == t).map(|x| x.url.clone());
                            if let Some(tab) = s.ws.tab_mut(t) {
                                tab.title = v.clone();
                            }
                            if let (Some(u), true) = (url, s.settings.save_history) {
                                s.library.set_title(&u, &v);
                                lib_dirty = true;
                            }
                        }
                        Event::Url(t, v) => {
                            if let Some(tab) = s.ws.tab_mut(t) {
                                tab.url = v.clone();
                            }
                            if s.settings.save_history {
                                s.library.visit(&v, "", now());
                                lib_dirty = true;
                            }
                            if let Some(to) = s.settings.container_for(&v) {
                                if s.ws.tabs.iter().any(|x| x.id == t && x.container != to) {
                                    moves.push((t, to));
                                }
                            }
                        }
                        Event::Loading(t, v) => {
                            if let Some(tab) = s.ws.tab_mut(t) {
                                tab.loading = v;
                            }
                        }
                        Event::Blocked(n) => s.blocked += n,
                        Event::Redraw => {}
                    }
                }
            }
            for (t, to) in moves {
                s.move_tab(t, to);
            }
            if lib_dirty {
                s.save_library();
            }
            if changed {
                refresh(&ui, &mut s);
                paint(&ui, &mut s);
            }
        });
    }
    // CAIRN_BENCH=<secs>: print RSS every 5s and quit (for RAM comparisons)
    let bench = std::env::var("CAIRN_BENCH").ok().and_then(|s| s.parse::<u64>().ok());
    let t0 = std::time::Instant::now();
    let bench_timer = slint::Timer::default();
    if let Some(limit) = bench {
        bench_timer.start(slint::TimerMode::Repeated, Duration::from_secs(5), move || {
            let rss: String = std::fs::read_to_string("/proc/self/status").unwrap_or_default().lines().filter(|l| l.starts_with("VmRSS") || l.starts_with("RssAnon")).collect::<Vec<_>>().join(" ");
            eprintln!("[{:>4}s] {rss}", t0.elapsed().as_secs());
            if t0.elapsed().as_secs() >= limit {
                let _ = slint::quit_event_loop();
            }
        });
    }
    ui.run()
}
