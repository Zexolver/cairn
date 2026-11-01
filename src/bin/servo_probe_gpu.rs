//! GPU probe: opens perchance + sirenrender in a window (shared GPU surface, tabs 1/2 switch),
//! logs everything to one file (cairn-gpu-probe-<unix>.log) to send back.
//! usage: servo_probe_gpu [seconds=90] [url1,url2,...]   (data dir: ./cairn-probe-data, persists across runs)
//! If no GPU/window context can be created it falls back to CPU rendering (headless) and says so in the log.
use servo::{
    ConsoleLogLevel, DeviceIntRect, EventLoopWaker, InputEvent, LoadStatus, Opts, RenderingContext, Servo,
    MouseButton, MouseButtonAction, MouseButtonEvent, MouseMoveEvent, ServoBuilder, SoftwareRenderingContext, WebView, WebViewBuilder, WebViewDelegate, WheelDelta, WheelEvent,
    WheelMode, WindowRenderingContext,
};
use std::{
    cell::RefCell,
    fs::File,
    io::Write,
    rc::Rc,
    sync::Mutex,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use winit::{
    application::ApplicationHandler,
    event::{ElementState, MouseButton as WButton, MouseScrollDelta, WindowEvent},
    event_loop::{ActiveEventLoop, EventLoop, EventLoopProxy},
    keyboard::{Key, NamedKey},
    raw_window_handle::{HasDisplayHandle, HasWindowHandle},
    window::{Window, WindowId},
};

static LOG: Mutex<Option<(File, Instant)>> = Mutex::new(None);

fn log(msg: impl AsRef<str>) {
    if let Some((f, t)) = LOG.lock().unwrap().as_mut() {
        let line = format!("[{:>7.2}s] {}\n", t.elapsed().as_secs_f32(), msg.as_ref());
        let _ = f.write_all(line.as_bytes());
        let _ = f.flush();
    }
    eprintln!("{}", msg.as_ref());
}

fn cmd(c: &str, args: &[&str]) -> String {
    std::process::Command::new(c)
        .args(args)
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_else(|e| format!("<{c} unavailable: {e}>"))
}

fn sysinfo() {
    log(format!("cairn gpu probe, args={:?}", std::env::args().collect::<Vec<_>>()));
    log(format!("uname: {}", cmd("uname", &["-a"])));
    for l in std::fs::read_to_string("/proc/cpuinfo").unwrap_or_default().lines() {
        if l.starts_with("model name") {
            log(l);
            break;
        }
    }
    for l in std::fs::read_to_string("/proc/meminfo").unwrap_or_default().lines().take(2) {
        log(l);
    }
    for k in ["XDG_SESSION_TYPE", "WAYLAND_DISPLAY", "DISPLAY", "XDG_CURRENT_DESKTOP", "MESA_LOADER_DRIVER_OVERRIDE", "LIBGL_ALWAYS_SOFTWARE"] {
        log(format!("env {k}={:?}", std::env::var(k).ok()));
    }
    log(format!("lspci GPU: {}", cmd("sh", &["-c", "lspci 2>/dev/null | grep -i 'vga\\|3d\\|display'"])));
    log(format!("glxinfo: {}", cmd("sh", &["-c", "glxinfo -B 2>/dev/null | grep -i 'renderer\\|version\\|vendor'"])));
    log(format!("eglinfo: {}", cmd("sh", &["-c", "eglinfo -B 2>/dev/null | grep -i 'renderer\\|vendor' | head -6"])));
}

fn mem() -> String {
    let st = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
    let pick = |k: &str| st.lines().find(|l| l.starts_with(k)).unwrap_or("").split_whitespace().nth(1).unwrap_or("?").to_string();
    let pss = std::fs::read_to_string("/proc/self/smaps_rollup")
        .unwrap_or_default()
        .lines()
        .filter(|l| l.starts_with("Pss:") || l.starts_with("Pss_Anon:"))
        .map(|l| l.split_whitespace().take(2).collect::<Vec<_>>().join(""))
        .collect::<Vec<_>>()
        .join(" ");
    format!("MEM RssAnon={}kB VmRSS={}kB VmHWM={}kB Threads={} {}", pick("RssAnon"), pick("VmRSS"), pick("VmHWM"), pick("Threads"), pss)
}

#[derive(Clone)]
struct Waker(EventLoopProxy<()>);
impl EventLoopWaker for Waker {
    fn clone_box(&self) -> Box<dyn EventLoopWaker> {
        Box::new(self.clone())
    }
    fn wake(&self) {
        let _ = self.0.send_event(());
    }
}

struct Delegate {
    window: Option<Rc<Window>>,
}
impl WebViewDelegate for Delegate {
    fn notify_new_frame_ready(&self, _: WebView) {
        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }
    fn notify_load_status_changed(&self, wv: WebView, s: LoadStatus) {
        log(format!("LOAD {:?} -> {:?}", wv.url().map(|u| u.to_string()), s));
    }
    fn notify_page_title_changed(&self, _: WebView, t: Option<String>) {
        log(format!("TITLE {t:?}"));
    }
    fn notify_crashed(&self, _: WebView, reason: String, bt: Option<String>) {
        log(format!("CRASH {reason} {bt:?}"));
    }
    fn show_console_message(&self, _: WebView, level: ConsoleLogLevel, m: String) {
        if matches!(level, ConsoleLogLevel::Error | ConsoleLogLevel::Warn) {
            log(format!("CONSOLE {level:?} {}", m.chars().take(300).collect::<String>()));
        }
    }
}

// field order = drop order: webviews, then servo, then GL context, then window
struct Running {
    tabs: Vec<WebView>,
    servo: Servo,
    ctx: Rc<WindowRenderingContext>,
    window: Rc<Window>,
    active: usize,
    cursor: servo::DevicePoint,
}

struct App {
    waker: Waker,
    urls: Vec<String>,
    secs: u64,
    start: Instant,
    last_mem: Instant,
    state: Option<Running>,
}

// Stopgap: Servo 0.7's `webcrypto` feature does not resolve from crates.io (RustCrypto prereleases).
const CRYPTO_POLYFILL: &str = r#"(function(){
if (typeof globalThis.crypto !== 'undefined') return;
var bad = function(){ throw new Error('crypto.subtle is not available'); };
var gv = function(a){ if (!a || !a.buffer) throw new TypeError('not a typed array');
  if (a.byteLength > 65536) throw new DOMException('too large','QuotaExceededError');
  var u = new Uint8Array(a.buffer, a.byteOffset, a.byteLength);
  for (var i = 0; i < u.length; i++) u[i] = Math.floor(Math.random() * 256);
  return a; };
var uuid = function(){ var b = new Uint8Array(16); gv(b); b[6] = b[6] & 15 | 64; b[8] = b[8] & 63 | 128;
  var h = Array.prototype.map.call(b, function(x){ return (x + 256).toString(16).slice(1); }).join('');
  return h.slice(0,8)+'-'+h.slice(8,12)+'-'+h.slice(12,16)+'-'+h.slice(16,20)+'-'+h.slice(20); };
Object.defineProperty(globalThis, 'crypto', { value: { getRandomValues: gv, randomUUID: uuid, subtle: undefined }, configurable: true });
})();"#;

const CSP_DEBUG: &str = r#"(function(){
var tag = function(){ try { return location.href.slice(0,70); } catch(e) { return '?'; } };
document.addEventListener('securitypolicyviolation', function(e){ console.error('CSPV ['+tag()+'] blocked='+e.blockedURI+' dir='+e.violatedDirective+' policy='+e.originalPolicy); });
try { var d = Object.getOwnPropertyDescriptor(HTMLIFrameElement.prototype,'src'); Object.defineProperty(HTMLIFrameElement.prototype,'src',{get:d.get,set:function(v){ console.error('IFRAME.src= ['+tag()+'] '+String(v).slice(0,200)); d.set.call(this,v); },configurable:true});
var d2 = Object.getOwnPropertyDescriptor(HTMLIFrameElement.prototype,'srcdoc'); Object.defineProperty(HTMLIFrameElement.prototype,'srcdoc',{get:d2.get,set:function(v){ console.error('IFRAME.srcdoc= ['+tag()+'] len='+String(v).length+' '+String(v).slice(0,200)); d2.set.call(this,v); },configurable:true}); } catch(e) {}
if (window !== window.top) { try { fetch(location.href.split('#')[0]).then(function(r){ console.error('HDRS ['+tag()+'] csp='+r.headers.get('content-security-policy')+' xfo='+r.headers.get('x-frame-options')); }); } catch(e) {} }
var oa = Element.prototype.setAttribute; Element.prototype.setAttribute = function(k,v){ if (this.tagName==='IFRAME' && (k==='src'||k==='srcdoc')) console.error('IFRAME.setAttribute '+k+' ['+tag()+'] '+String(v).slice(0,200)); return oa.call(this,k,v); };
new MutationObserver(function(ms){ ms.forEach(function(m){ m.addedNodes.forEach(function(n){ if (n.tagName === 'IFRAME') console.error('IFRAME ['+tag()+'] src='+String(n.getAttribute('src')).slice(0,120)+' srcdoc='+(n.hasAttribute('srcdoc')?n.getAttribute('srcdoc').length:'no')+' sandbox='+n.getAttribute('sandbox')); }); }); }).observe(document, {childList:true, subtree:true});
})();"#;

fn user_content(servo: &Servo) -> Rc<servo::UserContentManager> {
    let m = Rc::new(servo::UserContentManager::new(servo));
    m.add_script(Rc::new(servo::UserScript::new(CRYPTO_POLYFILL.to_string(), None)));
    if std::env::var_os("CSPDEBUG").is_some() {
        m.add_script(Rc::new(servo::UserScript::new(CSP_DEBUG.to_string(), None)));
    }
    m
}

fn prefs() -> servo::Preferences {
    servo::Preferences { dom_intersection_observer_enabled: true, dom_visual_viewport_enabled: true, dom_indexeddb_enabled: true, ..Default::default() }
}

fn opts() -> Opts {
    Opts { config_dir: Some(std::env::current_dir().unwrap().join("cairn-probe-data")), ..Opts::default() }
}

impl ApplicationHandler<()> for App {
    fn resumed(&mut self, el: &ActiveEventLoop) {
        if self.state.is_some() {
            return;
        }
        let window = Rc::new(el.create_window(Window::default_attributes().with_title("cairn gpu probe (keys 1-9 switch tab)")).expect("window"));
        let ctx = match WindowRenderingContext::new(el.display_handle().unwrap(), window.window_handle().unwrap(), window.inner_size()) {
            Ok(c) => Rc::new(c),
            Err(e) => {
                log(format!("RENDER MODE: GPU window context FAILED: {e:?}"));
                software_fallback(&self.urls, self.secs);
                el.exit();
                return;
            }
        };
        let _ = ctx.make_current();
        log("RENDER MODE: window rendering context created (GPU if the driver is hardware; see glxinfo/eglinfo lines above)");
        let servo = ServoBuilder::default().opts(opts()).preferences(prefs()).event_loop_waker(Box::new(self.waker.clone())).build();
        let scale = window.scale_factor() as f32;
        let delegate = Rc::new(Delegate { window: Some(window.clone()) });
        let ucm = user_content(&servo);
        let tabs: Vec<WebView> = self
            .urls
            .iter()
            .enumerate()
            .map(|(i, u)| {
                let wv = WebViewBuilder::new(&servo, ctx.clone())
                    .user_content_manager(ucm.clone())
                    .url(url::Url::parse(u).expect("url"))
                    .hidpi_scale_factor(euclid::Scale::new(scale))
                    .delegate(delegate.clone())
                    .build();
                if i > 0 {
                    wv.hide();
                }
                wv
            })
            .collect();
        self.state = Some(Running { window, servo, ctx, tabs, active: 0, cursor: servo::DevicePoint::default() });
        log(mem());
    }

    fn exiting(&mut self, _: &ActiveEventLoop) {
        self.state.take();
    }

    fn user_event(&mut self, el: &ActiveEventLoop, _: ()) {
        self.tick(el);
    }

    fn window_event(&mut self, el: &ActiveEventLoop, _: WindowId, ev: WindowEvent) {
        self.tick(el);
        let Some(s) = self.state.as_mut() else { return };
        match ev {
            WindowEvent::CloseRequested => el.exit(),
            WindowEvent::RedrawRequested => {
                s.tabs[s.active].paint();
                s.ctx.present();
            }
            WindowEvent::Resized(sz) => {
                for t in &s.tabs {
                    t.resize(sz);
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let (x, y, mode) = match delta {
                    MouseScrollDelta::LineDelta(x, y) => ((x * 76.0) as f64, (y * 76.0) as f64, WheelMode::DeltaLine),
                    MouseScrollDelta::PixelDelta(p) => (p.x, p.y, WheelMode::DeltaPixel),
                };
                s.tabs[s.active].notify_input_event(InputEvent::Wheel(WheelEvent::new(
                    WheelDelta { x, y, z: 0.0, mode },
                    s.cursor.into(),
                )));
            }
            WindowEvent::CursorMoved { position, .. } => {
                s.cursor = servo::DevicePoint::new(position.x as f32, position.y as f32);
                s.tabs[s.active].notify_input_event(InputEvent::MouseMove(MouseMoveEvent::new(s.cursor.into())));
            }
            WindowEvent::MouseInput { state, button, .. } => {
                let b = match button {
                    WButton::Left => MouseButton::Primary,
                    WButton::Right => MouseButton::Secondary,
                    WButton::Middle => MouseButton::Auxiliary,
                    WButton::Back => MouseButton::Back,
                    WButton::Forward => MouseButton::Forward,
                    WButton::Other(n) => MouseButton::Other(n),
                };
                let a = if state == ElementState::Pressed { MouseButtonAction::Down } else { MouseButtonAction::Up };
                s.tabs[s.active].notify_input_event(InputEvent::MouseButton(MouseButtonEvent::new(a, b, s.cursor.into())));
            }
            WindowEvent::KeyboardInput { event, .. } if event.state == ElementState::Pressed => {
                if let Key::Character(c) = &event.logical_key {
                    if let Some(n) = c.chars().next().and_then(|c| c.to_digit(10)) {
                        let n = n as usize;
                        if n >= 1 && n <= s.tabs.len() && n - 1 != s.active {
                            s.tabs[s.active].hide();
                            s.active = n - 1;
                            s.tabs[s.active].show();
                            log(format!("SWITCH to tab {n}"));
                            s.window.request_redraw();
                        }
                    }
                }
                if event.logical_key == Key::Named(NamedKey::Escape) {
                    el.exit();
                }
            }
            _ => {}
        }
    }
}

impl App {
    fn tick(&mut self, el: &ActiveEventLoop) {
        if let Some(s) = &self.state {
            s.servo.spin_event_loop();
        }
        if self.last_mem.elapsed() > Duration::from_secs(5) {
            self.last_mem = Instant::now();
            log(mem());
        }
        if self.start.elapsed() > Duration::from_secs(self.secs) {
            log("time limit reached, exiting");
            el.exit();
        }
    }
}

/// Headless CPU fallback: load the urls in a shared software surface, log RAM, save a screenshot.
fn software_fallback(urls: &[String], secs: u64) {
    log("RENDER MODE: SOFTWARE (CPU) fallback");
    let (w, h) = (1280u32, 800u32);
    let servo = ServoBuilder::default().opts(opts()).preferences(prefs()).build();
    let ctx = Rc::new(SoftwareRenderingContext::new(dpi::PhysicalSize::new(w, h)).expect("software ctx"));
    let ucm = user_content(&servo);
    let tabs: RefCell<Vec<WebView>> = RefCell::new(
        urls.iter()
            .enumerate()
            .map(|(i, u)| {
                let wv = WebViewBuilder::new(&servo, ctx.clone()).user_content_manager(ucm.clone()).url(url::Url::parse(u).unwrap()).delegate(Rc::new(Delegate { window: None })).build();
                if i > 0 {
                    wv.hide();
                }
                wv
            })
            .collect(),
    );
    // STEPS="wait:5;click:x,y;type:text;key:Enter;scroll:dy;shot:name.png;js:expr" (';'-separated), run on tab 0
    let steps: Vec<String> = std::env::var("STEPS").map(|s| s.split(';').map(|x| x.trim().to_string()).collect()).unwrap_or_default();
    let mut step = 0usize;
    let mut wait_until = Instant::now();
    let t = Instant::now();
    let limit = Duration::from_secs(if steps.is_empty() { secs.min(60) } else { secs });
    let mut last = Instant::now() - Duration::from_secs(10);
    let shot = |name: &str| {
        if let Some(img) = ctx.read_to_image(DeviceIntRect::from_size(euclid::Size2D::new(w as i32, h as i32))) {
            let _ = img.save(name);
        }
    };
    let click = |wv: &WebView, x: f32, y: f32| {
        let p: servo::WebViewPoint = servo::DevicePoint::new(x, y).into();
        wv.notify_input_event(InputEvent::MouseMove(MouseMoveEvent::new(p)));
        wv.notify_input_event(InputEvent::MouseButton(MouseButtonEvent::new(MouseButtonAction::Down, MouseButton::Primary, p)));
        wv.notify_input_event(InputEvent::MouseButton(MouseButtonEvent::new(MouseButtonAction::Up, MouseButton::Primary, p)));
    };
    let key = |wv: &WebView, k: servo::Key, code: servo::Code| {
        for st in [servo::KeyState::Down, servo::KeyState::Up] {
            wv.notify_input_event(InputEvent::Keyboard(servo::KeyboardEvent::new_without_event(
                st, k.clone(), code, servo::Location::Standard, servo::Modifiers::empty(), false, false,
            )));
        }
    };
    while t.elapsed() < limit {
        servo.spin_event_loop();
        tabs.borrow()[0].paint();
        if step < steps.len() && Instant::now() >= wait_until {
            let s = steps[step].clone();
            step += 1;
            let (cmd, arg) = s.split_once(':').unwrap_or((s.as_str(), ""));
            let wv = &tabs.borrow()[0];
            log(format!("STEP {step}/{} {s}", steps.len()));
            match cmd {
                "wait" => wait_until = Instant::now() + Duration::from_secs_f32(arg.parse().unwrap_or(1.0)),
                "click" => {
                    let v: Vec<f32> = arg.split(',').filter_map(|x| x.parse().ok()).collect();
                    click(wv, v[0], v[1]);
                }
                "scroll" => {
                    wv.notify_input_event(InputEvent::Wheel(WheelEvent::new(
                        WheelDelta { x: 0.0, y: arg.parse().unwrap_or(0.0), z: 0.0, mode: WheelMode::DeltaPixel },
                        servo::DevicePoint::new(600.0, 400.0).into(),
                    )));
                }
                "type" => {
                    for c in arg.chars() {
                        key(wv, servo::Key::Character(c.to_string()), servo::Code::Unidentified);
                    }
                }
                "key" => key(wv, servo::Key::Named(match arg { "Enter" => servo::NamedKey::Enter, "Tab" => servo::NamedKey::Tab, "Escape" => servo::NamedKey::Escape, _ => servo::NamedKey::Unidentified }), servo::Code::Unidentified),
                "shot" => shot(arg),
                "js" => wv.evaluate_javascript(arg, |r| log(format!("JS result: {r:?}"))),
                _ => log(format!("unknown step {s}")),
            }
        } else if step >= steps.len() && !steps.is_empty() && Instant::now() >= wait_until + Duration::from_secs(2) {
            break;
        }
        if last.elapsed() > Duration::from_secs(5) {
            last = Instant::now();
            log(mem());
        }
        std::thread::sleep(Duration::from_millis(16));
    }
    shot("cairn-gpu-probe-fallback.png");
}

fn main() {
    let secs: u64 = std::env::args().nth(1).and_then(|s| s.parse().ok()).unwrap_or(90);
    let urls: Vec<String> = std::env::args()
        .nth(2)
        .map(|s| s.split(',').map(String::from).collect())
        .unwrap_or_else(|| vec!["https://sirenrender.com".into(), "https://perchance.org".into()]);
    let name = format!("cairn-gpu-probe-{}.log", SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs());
    *LOG.lock().unwrap() = Some((File::create(&name).expect("log file"), Instant::now()));
    std::panic::set_hook(Box::new(|i| log(format!("PANIC {i}\n{}", std::backtrace::Backtrace::force_capture()))));
    rustls::crypto::aws_lc_rs::default_provider().install_default().ok();
    sysinfo();
    let el = match EventLoop::<()>::with_user_event().build() {
        Ok(el) => el,
        Err(e) => {
            log(format!("RENDER MODE: no window system ({e}); using CPU fallback"));
            software_fallback(&urls, secs);
            log(mem());
            eprintln!("\nSend back: {name}");
            return;
        }
    };
    let mut app = App { waker: Waker(el.create_proxy()), urls, secs, start: Instant::now(), last_mem: Instant::now(), state: None };
    let r = el.run_app(&mut app);
    log(format!("exit: {r:?}"));
    log(mem());
    eprintln!("\nSend back: {name}");
}
