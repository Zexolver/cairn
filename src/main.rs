mod engine;

use cairn_core::{
    blocker::Blocker,
    container::Workspace,
    lists, nav,
};
use engine::{Engine, Event, Input, StubEngine};
use slint::{ComponentHandle, Image, Model, ModelRc, Rgba8Pixel, SharedPixelBuffer, SharedString, VecModel};
use std::{cell::RefCell, collections::HashMap, rc::Rc, time::Duration};

slint::include_modules!();

const SEARCH: &str = "https://duckduckgo.com/?q=%s";

struct State {
    ws: Workspace,
    selected_container: u32,
    engines: HashMap<u32, Box<dyn Engine>>,
    viewport: (u32, u32),
    blocked: u32,
}

impl State {
    fn engine_for(&mut self, container: u32) -> &mut Box<dyn Engine> {
        let vp = self.viewport;
        self.engines.entry(container).or_insert_with(|| {
            let mut e: Box<dyn Engine> = Box::new(StubEngine::default());
            e.resize(vp.0, vp.1);
            e
        })
    }

    fn active_container(&self) -> Option<u32> {
        let a = self.ws.active?;
        self.ws.tabs.iter().find(|t| t.id == a).map(|t| t.container)
    }
}

fn color(hex: &str) -> slint::Color {
    let v = u32::from_str_radix(hex.trim_start_matches('#'), 16).unwrap_or(0x7aa2f7);
    slint::Color::from_rgb_u8((v >> 16) as u8, (v >> 8) as u8, v as u8)
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
    d.set_containers(ModelRc::new(VecModel::from(rows)));
    let tab_color = st.ws.containers.iter().find(|c| c.id == st.selected_container).map(|c| color(&c.color)).unwrap_or_default();
    let tabs: Vec<TabRow> = st
        .ws
        .tabs
        .iter()
        .filter(|t| t.container == st.selected_container)
        .map(|t| TabRow { id: t.id as i32, title: t.title.clone().into(), color: tab_color, selected: st.ws.active == Some(t.id), loading: t.loading })
        .collect();
    d.set_tabs(ModelRc::new(VecModel::from(tabs)));
    let active = st.ws.active.and_then(|a| st.ws.tabs.iter().find(|t| t.id == a)).cloned();
    d.set_has_tab(active.is_some());
    d.set_url(active.as_ref().map(|t| t.url.clone()).unwrap_or_default().into());
    d.set_status(SharedString::from("No tab open — press + to start"));
    d.set_blocked_count(st.blocked as i32);
}

fn paint(ui: &MainWindow, st: &mut State) {
    let (Some(tab), Some(c)) = (st.ws.active, st.active_container()) else { return };
    if let Some(f) = st.engine_for(c).frame(tab) {
        let buf = SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(&f.rgba, f.width, f.height);
        ui.global::<AppData>().set_frame(Image::from_rgba8(buf));
    }
}

fn open_tab(st: &mut State, url: &str) {
    let c = st.selected_container;
    if let Ok(id) = st.ws.open_tab(c, url) {
        st.engine_for(c).open(id, url);
    }
}

fn main() -> Result<(), slint::PlatformError> {
    let ui = MainWindow::new()?;
    let data_dir = std::env::var_os("CAIRN_DATA").map(std::path::PathBuf::from).unwrap_or_else(|| std::env::temp_dir().join("cairn"));
    let blocker = Rc::new(Blocker::new());
    blocker.load_lists(lists::load_cached(&data_dir.join("lists")));

    let st = Rc::new(RefCell::new(State { ws: Workspace::default(), selected_container: 0, engines: HashMap::new(), viewport: (800, 600), blocked: 0 }));
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
        if let Some(t) = s.ws.tabs.iter().find(|t| t.container == id as u32) {
            s.ws.active = Some(t.id);
        } else {
            s.ws.active = None;
        }
    });
    on!(on_add_container, |s, _ui, name| {
        if let Ok(id) = s.ws.add_container(&name) {
            s.selected_container = id;
            s.ws.active = None;
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
        open_tab(&mut s, "about:blank");
    });
    on!(on_navigate, |s, _ui, text| {
        let url = nav::resolve_input(&text, SEARCH);
        match s.ws.active.zip(s.active_container()) {
            Some((tab, c)) => {
                if let Some(t) = s.ws.tab_mut(tab) {
                    t.url = url.clone();
                }
                s.engine_for(c).navigate(tab, &url);
            }
            None => open_tab(&mut s, &url),
        }
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
    on!(on_viewport_resized, |s, _ui, w, h| {
        s.viewport = (w as u32, h as u32);
        for e in s.engines.values_mut() {
            e.resize(w as u32, h as u32);
        }
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
        open_tab(&mut s, "about:blank");
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
            let ids: Vec<u32> = s.engines.keys().copied().collect();
            for c in ids {
                for ev in s.engines.get_mut(&c).unwrap().pump() {
                    changed = true;
                    match ev {
                        Event::Title(t, v) => {
                            if let Some(tab) = s.ws.tab_mut(t) {
                                tab.title = v;
                            }
                        }
                        Event::Url(t, v) => {
                            if let Some(tab) = s.ws.tab_mut(t) {
                                tab.url = v;
                            }
                        }
                        Event::Loading(t, v) => {
                            if let Some(tab) = s.ws.tab_mut(t) {
                                tab.loading = v;
                            }
                        }
                        Event::Blocked(n) => s.blocked += n,
                    }
                }
            }
            if changed {
                refresh(&ui, &mut s);
                paint(&ui, &mut s);
            }
        });
    }
    let _ = ui.global::<AppData>().get_tabs().row_count();
    ui.run()
}
