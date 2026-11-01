//! Servo-backed engine: one Servo instance (+ software surface) per container, tabs are webviews.
use crate::engine::{Engine, Event, Frame, Input};
use cairn_core::container::TabId;
use servo::{
    DeviceIntRect, EventLoopWaker, InputEvent, LoadStatus, MouseButton, MouseButtonAction, MouseButtonEvent, MouseMoveEvent,
    Opts, RenderingContext, Servo, ServoBuilder, SoftwareRenderingContext, WebView, WebViewBuilder, WebViewDelegate, WheelDelta,
    WheelEvent, WheelMode,
};
use std::{cell::RefCell, collections::HashMap, path::Path, rc::Rc};

#[derive(Clone)]
struct NoWake;
impl EventLoopWaker for NoWake {
    fn clone_box(&self) -> Box<dyn EventLoopWaker> {
        Box::new(NoWake)
    }
    fn wake(&self) {}
}

type Queue = Rc<RefCell<Vec<Event>>>;

struct Delegate {
    tab: TabId,
    queue: Queue,
    size: Rc<std::cell::Cell<(u32, u32)>>,
}

impl WebViewDelegate for Delegate {
    fn screen_geometry(&self, _: WebView) -> Option<servo::ScreenGeometry> {
        use euclid::{Point2D, Size2D};
        let (w, h) = self.size.get();
        let sz = Size2D::new(w as i32, h as i32);
        Some(servo::ScreenGeometry { size: sz, available_size: sz, window_rect: DeviceIntRect::new(Point2D::new(0, 0), Point2D::new(w as i32, h as i32)) })
    }
    fn notify_new_frame_ready(&self, _: WebView) {
        self.queue.borrow_mut().push(Event::Redraw);
    }
    fn notify_url_changed(&self, _: WebView, url: url::Url) {
        self.queue.borrow_mut().push(Event::Url(self.tab, url.to_string()));
    }
    fn notify_page_title_changed(&self, _: WebView, t: Option<String>) {
        if let Some(t) = t.filter(|t| !t.is_empty()) {
            self.queue.borrow_mut().push(Event::Title(self.tab, t));
        }
    }
    fn notify_load_status_changed(&self, _: WebView, s: LoadStatus) {
        self.queue.borrow_mut().push(Event::Loading(self.tab, s != LoadStatus::Complete));
    }
}

// Servo 0.7's `webcrypto` feature does not resolve from crates.io; give pages a minimal crypto.
const CRYPTO_POLYFILL: &str = r#"(function(){
if (typeof globalThis.crypto !== 'undefined') return;
var gv = function(a){ if (!a || !a.buffer) throw new TypeError('not a typed array');
  var u = new Uint8Array(a.buffer, a.byteOffset, a.byteLength);
  for (var i = 0; i < u.length; i++) u[i] = Math.floor(Math.random() * 256);
  return a; };
var uuid = function(){ var b = new Uint8Array(16); gv(b); b[6] = b[6] & 15 | 64; b[8] = b[8] & 63 | 128;
  var h = Array.prototype.map.call(b, function(x){ return (x + 256).toString(16).slice(1); }).join('');
  return h.slice(0,8)+'-'+h.slice(8,12)+'-'+h.slice(12,16)+'-'+h.slice(16,20)+'-'+h.slice(20); };
Object.defineProperty(globalThis, 'crypto', { value: { getRandomValues: gv, randomUUID: uuid, subtle: undefined }, configurable: true });
})();"#;

pub struct ServoEngine {
    servo: Servo,
    ctx: Rc<SoftwareRenderingContext>,
    ucm: Rc<servo::UserContentManager>,
    tabs: HashMap<TabId, WebView>,
    shown: Option<TabId>,
    queue: Queue,
    size: Rc<std::cell::Cell<(u32, u32)>>,
    cursor: (f32, f32),
    dirty: bool,
}

impl ServoEngine {
    pub fn new(profile_dir: &Path, vp: (u32, u32)) -> Self {
        let _ = std::fs::create_dir_all(profile_dir);
        let prefs = servo::Preferences { dom_intersection_observer_enabled: true, dom_visual_viewport_enabled: true, dom_indexeddb_enabled: true, ..Default::default() };
        let opts = Opts { config_dir: Some(profile_dir.to_path_buf()), ..Opts::default() };
        let servo = ServoBuilder::default().opts(opts).preferences(prefs).event_loop_waker(Box::new(NoWake)).build();
        let ctx = Rc::new(SoftwareRenderingContext::new(dpi::PhysicalSize::new(vp.0.max(1), vp.1.max(1))).expect("rendering context"));
        let ucm = Rc::new(servo::UserContentManager::new(&servo));
        ucm.add_script(Rc::new(servo::UserScript::new(CRYPTO_POLYFILL.to_string(), None)));
        Self { servo, ctx, ucm, tabs: HashMap::new(), shown: None, queue: Rc::default(), size: Rc::new(std::cell::Cell::new(vp)), cursor: (0.0, 0.0), dirty: true }
    }

    fn show_only(&mut self, tab: TabId) {
        if self.shown == Some(tab) {
            return;
        }
        for (id, wv) in &self.tabs {
            if *id == tab {
                wv.show();
                wv.focus_hint();
            } else {
                wv.hide();
            }
        }
        self.shown = Some(tab);
        self.dirty = true;
    }
}

trait FocusHint {
    fn focus_hint(&self);
}
impl FocusHint for WebView {
    fn focus_hint(&self) {
        self.set_focused(true);
    }
}

fn parse(url: &str) -> url::Url {
    url::Url::parse(url).unwrap_or_else(|_| url::Url::parse("about:blank").unwrap())
}

impl Engine for ServoEngine {
    fn open(&mut self, tab: TabId, url: &str) {
        let delegate = Rc::new(Delegate { tab, queue: self.queue.clone(), size: self.size.clone() });
        let wv = WebViewBuilder::new(&self.servo, self.ctx.clone())
            .user_content_manager(self.ucm.clone())
            .url(parse(url))
            .delegate(delegate)
            .build();
        let (w, h) = self.size.get();
        wv.resize(dpi::PhysicalSize::new(w, h));
        self.tabs.insert(tab, wv);
        self.shown = None;
        self.show_only(tab);
    }
    fn close(&mut self, tab: TabId) {
        self.tabs.remove(&tab);
        if self.shown == Some(tab) {
            self.shown = None;
        }
    }
    fn navigate(&mut self, tab: TabId, url: &str) {
        if let Some(wv) = self.tabs.get(&tab) {
            wv.load(parse(url));
        }
    }
    fn back(&mut self, tab: TabId) {
        if let Some(wv) = self.tabs.get(&tab) {
            if wv.can_go_back() {
                wv.go_back(1);
            }
        }
    }
    fn forward(&mut self, tab: TabId) {
        if let Some(wv) = self.tabs.get(&tab) {
            if wv.can_go_forward() {
                wv.go_forward(1);
            }
        }
    }
    fn reload(&mut self, tab: TabId) {
        if let Some(wv) = self.tabs.get(&tab) {
            wv.reload();
        }
    }
    fn resize(&mut self, width: u32, height: u32) {
        self.size.set((width, height));
        for wv in self.tabs.values() {
            wv.resize(dpi::PhysicalSize::new(width, height));
        }
        self.dirty = true;
    }
    fn input(&mut self, tab: TabId, input: Input) {
        self.show_only(tab);
        let Some(wv) = self.tabs.get(&tab) else { return };
        let pt = |x: f32, y: f32| servo::DevicePoint::new(x, y);
        match input {
            Input::Move(x, y) => {
                self.cursor = (x, y);
                wv.notify_input_event(InputEvent::MouseMove(MouseMoveEvent::new(pt(x, y).into())));
            }
            Input::Down(x, y, b) | Input::Up(x, y, b) => {
                let down = matches!(input, Input::Down(..));
                let button = match b {
                    0 => MouseButton::Primary,
                    2 => MouseButton::Secondary,
                    _ => MouseButton::Auxiliary,
                };
                let a = if down { MouseButtonAction::Down } else { MouseButtonAction::Up };
                wv.notify_input_event(InputEvent::MouseButton(MouseButtonEvent::new(a, button, pt(x, y).into())));
            }
            Input::Scroll(x, y, dx, dy) => {
                wv.notify_input_event(InputEvent::Wheel(WheelEvent::new(WheelDelta { x: dx as f64, y: dy as f64, z: 0.0, mode: WheelMode::DeltaPixel }, pt(x, y).into())));
            }
            Input::Key(text, down) => {
                use servo::NamedKey as N;
                let named = match text.as_str() {
                    "\n" | "\r" => Some(N::Enter),
                    "\u{8}" => Some(N::Backspace),
                    "\t" => Some(N::Tab),
                    "\u{1b}" => Some(N::Escape),
                    "\u{7f}" => Some(N::Delete),
                    "\u{f700}" => Some(N::ArrowUp),
                    "\u{f701}" => Some(N::ArrowDown),
                    "\u{f702}" => Some(N::ArrowLeft),
                    "\u{f703}" => Some(N::ArrowRight),
                    "\u{f729}" => Some(N::Home),
                    "\u{f72b}" => Some(N::End),
                    "\u{f72c}" => Some(N::PageUp),
                    "\u{f72d}" => Some(N::PageDown),
                    _ => None,
                };
                let key = match named {
                    Some(n) => servo::Key::Named(n),
                    None if text.is_empty() => return,
                    None => servo::Key::Character(text),
                };
                let st = if down { servo::KeyState::Down } else { servo::KeyState::Up };
                wv.notify_input_event(InputEvent::Keyboard(servo::KeyboardEvent::new_without_event(
                    st, key, servo::Code::Unidentified, servo::Location::Standard, servo::Modifiers::empty(), false, false,
                )));
            }
        }
    }
    fn pump(&mut self) -> Vec<Event> {
        self.servo.spin_event_loop();
        std::mem::take(&mut *self.queue.borrow_mut())
    }
    fn frame(&mut self, tab: TabId) -> Option<Frame> {
        self.show_only(tab);
        let wv = self.tabs.get(&tab)?;
        wv.paint();
        let (w, h) = self.size.get();
        let img = self.ctx.read_to_image(DeviceIntRect::from_size(euclid::Size2D::new(w as i32, h as i32)))?;
        self.dirty = false;
        Some(Frame { width: img.width(), height: img.height(), rgba: img.into_raw() })
    }
}
