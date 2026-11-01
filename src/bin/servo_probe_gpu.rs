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
    ctx: Option<Rc<dyn servo::RenderingContext>>,
    spawned: Rc<RefCell<Vec<WebView>>>,
    cursor: Rc<std::cell::Cell<(f32, f32)>>,
    menu: RefCell<Option<servo::ContextMenu>>,
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
    fn request_create_new(&self, _parent: WebView, request: servo::CreateNewWebViewRequest) {
        let Some(ctx) = self.ctx.clone() else { return };
        let delegate = Rc::new(Delegate { window: self.window.clone(), ctx: Some(ctx.clone()), spawned: self.spawned.clone(), cursor: self.cursor.clone(), menu: RefCell::default() });
        let wv = request.builder(ctx).delegate(delegate).build();
        log("NEWTAB requested (window.open / target=_blank)".to_string());
        self.spawned.borrow_mut().push(wv);
    }
    fn show_embedder_control(&self, wv: WebView, control: servo::EmbedderControl) {
        let servo::EmbedderControl::ContextMenu(menu) = control else { return };
        let scale = self.window.as_ref().map(|w| w.scale_factor() as f32).unwrap_or(1.0);
        let (cx, cy) = self.cursor.get();
        let (x, y) = if cx > 0.0 || cy > 0.0 { (cx / scale, cy / scale) } else { (menu.position().min.x as f32 / scale, menu.position().min.y as f32 / scale) };
        let items: Vec<serde_json::Value> = menu
            .items()
            .iter()
            .enumerate()
            .map(|(i, it)| match it {
                servo::ContextMenuItem::Item { label, enabled, .. } => serde_json::json!({"i": i, "l": label, "e": enabled}),
                servo::ContextMenuItem::Separator => serde_json::json!({"sep": true}),
            })
            .collect();
        let js = format!("({})({},{},{})", CONTEXT_MENU_JS, serde_json::Value::Array(items), x, y);
        *self.menu.borrow_mut() = Some(menu);
        wv.evaluate_javascript(js, |_| {});
    }
    fn hide_embedder_control(&self, wv: WebView, _: servo::EmbedderControlId) {
        self.menu.borrow_mut().take();
        wv.evaluate_javascript("var o=document.getElementById('__cairn_menu');o&&o.remove()", |_| {});
    }
    fn show_console_message(&self, wv: WebView, level: ConsoleLogLevel, m: String) {
        if let Some(rest) = m.strip_prefix("CAIRN_MENU:") {
            let menu = self.menu.borrow_mut().take();
            if let (Some(menu), Ok(i)) = (menu, rest.parse::<usize>()) {
                if let Some(servo::ContextMenuItem::Item { action, .. }) = menu.items().get(i).cloned() {
                    menu.select(action);
                }
            }
            return;
        }
        if let Some(rest) = m.strip_prefix("CAIRN_DL:") {
            save_download(rest);
            return;
        }
        if matches!(level, ConsoleLogLevel::Error | ConsoleLogLevel::Warn) || std::env::var_os("CFDEBUG").is_some() {
            log(format!("CONSOLE {level:?} {}", m.chars().take(if std::env::var_os("CFDEBUG").is_some() { 3000 } else { 300 }).collect::<String>()));
        }
    }
}

const CONTEXT_MENU_JS: &str = r#"function(items,x,y){
var o=document.getElementById('__cairn_menu'); if(o) o.remove();
var h=document.createElement('div'); h.id='__cairn_menu';
h.style.cssText='position:fixed;left:0;top:0;width:100%;height:100%;z-index:2147483647;margin:0;padding:0';
var m=document.createElement('div');
m.style.cssText='position:absolute;background:#2b2b2f;color:#eee;font:14px sans-serif;border:1px solid #555;border-radius:6px;padding:4px 0;min-width:180px;box-shadow:0 4px 14px rgba(0,0,0,.5)';
function close(){ h.remove(); document.removeEventListener('keydown',kd,true); console.log('CAIRN_MENU:x'); }
function kd(e){ if(e.key==='Escape'){ e.preventDefault(); close(); } }
items.forEach(function(it){
  var d=document.createElement('div');
  if(it.sep){ d.style.cssText='height:1px;background:#555;margin:4px 0'; }
  else {
    d.textContent=it.l;
    d.style.cssText='padding:6px 16px;cursor:default;'+(it.e?'':'color:#777');
    if(it.e){
      d.onmouseenter=function(){ d.style.background='#3d6fd1'; };
      d.onmouseleave=function(){ d.style.background=''; };
      d.onclick=function(ev){ ev.stopPropagation(); h.remove(); document.removeEventListener('keydown',kd,true); console.log('CAIRN_MENU:'+it.i); };
    }
  }
  m.appendChild(d);
});
h.onmousedown=function(e){ if(e.target===h){ e.preventDefault(); close(); } };
h.oncontextmenu=function(e){ e.preventDefault(); if(e.target===h) close(); };
h.appendChild(m); document.documentElement.appendChild(h);
var w=m.offsetWidth,hh=m.offsetHeight;
m.style.left=Math.max(0,Math.min(x,window.innerWidth-w))+'px';
m.style.top=Math.max(0,Math.min(y,window.innerHeight-hh))+'px';
document.addEventListener('keydown',kd,true);
}"#;

// field order = drop order: webviews, then servo, then GL context, then window
struct Running {
    tabs: Vec<WebView>,
    servo: Servo,
    ctx: Rc<WindowRenderingContext>,
    window: Rc<Window>,
    active: usize,
    cursor: servo::DevicePoint,
    mods: winit::keyboard::ModifiersState,
}

struct App {
    waker: Waker,
    urls: Vec<String>,
    secs: u64,
    start: Instant,
    last_mem: Instant,
    state: Option<Running>,
    spawned: Rc<RefCell<Vec<WebView>>>,
    cursor: Rc<std::cell::Cell<(f32, f32)>>,
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

const CF_DEBUG: &str = r#"(function(){
var tag = function(){ try { return location.href.slice(0,60); } catch(e) { return '?'; } };
window.addEventListener('message', function(e){ var d=e.data, ev=''; try{ ev = (typeof d==='string'?d:JSON.stringify(d)).slice(0,(d&&d.cs)?1500:100);}catch(x){}
 var rel=[]; try{ rel.push('src==self:'+(e.source===window)); rel.push('src==top:'+(e.source===window.top)); rel.push('src.parent==self:'+(e.source&&e.source.parent===window)); rel.push('nframes:'+window.length); for(var i=0;i<window.length;i++){ rel.push('f'+i+'==src:'+(window.frames[i]===e.source)); } }catch(x){ rel.push('err '+x); }
 console.error('PM ['+tag()+'] from='+e.origin+' '+rel.join(' ')+' data='+ev); }, true);
var oas = Element.prototype.attachShadow; Element.prototype.attachShadow = function(o){ console.error('SHADOW ['+tag()+'] mode='+(o&&o.mode)+' host='+this.tagName+'#'+this.id); return oas.call(this,o); };
['appendChild','insertBefore'].forEach(function(n){ var o=Node.prototype[n]; Node.prototype[n]=function(a){ try{ if(a && a.tagName==='IFRAME') console.error('IFRAME-ADD ['+tag()+'] via '+n+' into '+(this.nodeName)+' src='+String(a.src).slice(0,150)+' sandbox='+a.getAttribute('sandbox')+' style='+a.getAttribute('style')); }catch(x){} return o.apply(this,arguments); }; });
window.addEventListener('error',function(e){ console.error('ONERROR ['+tag()+'] '+e.message+' @'+String(e.filename).slice(0,80)+':'+e.lineno); },true);
window.addEventListener('unhandledrejection',function(e){ console.error('UNHANDLED ['+tag()+'] '+String(e.reason&&(e.reason.stack||e.reason)).slice(0,300)); });
try{ var of=window.fetch; window.fetch=function(u,o){ var url=String(u&&u.url||u).slice(0,140); return of.apply(this,arguments).then(function(r){ console.error('FETCH ['+tag()+'] '+url+' -> '+r.status); return r; },function(er){ console.error('FETCH-FAIL ['+tag()+'] '+url+' '+er); throw er; }); }; }catch(x){}
try{ var ox=XMLHttpRequest.prototype.open; XMLHttpRequest.prototype.open=function(m,u){ console.error('XHR ['+tag()+'] '+m+' '+String(u).slice(0,140)); this.addEventListener('loadend',function(){ console.error('XHR-END '+String(u).slice(0,100)+' -> '+this.status); }); return ox.apply(this,arguments); }; }catch(x){}
try{ var OB=window.Blob; window.Blob=function(p,o){ try{ if(o&&/javascript/.test(o.type||'')) console.error('BLOB-JS '+String(p.join('')).slice(0,700)); }catch(_){} return new OB(p,o); }; window.Blob.prototype=OB.prototype; }catch(x){}
try{ var OW=window.Worker; window.Worker=function(u,o){ console.error('WORKER ['+tag()+'] '+String(u).slice(0,100)); var w=new OW(u,o); w.addEventListener('error',function(e){ console.error('WORKER-ERR '+e.message); }); w.addEventListener('message',function(e){ var d; try{d=JSON.stringify(e.data)}catch(_){d=String(e.data)} console.error('WORKER-MSG '+String(d).slice(0,400)); }); var op=w.postMessage; w.postMessage=function(m){ var d; try{d=JSON.stringify(m)}catch(_){d=String(m)} console.error('WORKER-POST '+String(d).slice(0,400)); return op.apply(this,arguments); }; return w; }; window.Worker.prototype=OW.prototype; }catch(x){}
try{ ['instantiate','compile'].forEach(function(n){ var o=WebAssembly[n]; WebAssembly[n]=function(){ console.error('WASM.'+n+' ['+tag()+']'); return o.apply(this,arguments).catch(function(e){ console.error('WASM-FAIL '+n+' '+e); throw e; }); }; }); }catch(x){}
var cc = document.createElement; document.createElement = function(t){ var el = cc.apply(document, arguments); if (String(t).toLowerCase()==='iframe') console.error('IFRAME-CREATE ['+tag()+']'); return el; };
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

const DOWNLOAD_HOOK: &str = r#"(function(){
if (window !== window.top) return;
var send = function(a){
  var name = a.getAttribute('download') || 'download';
  var href = a.href;
  fetch(href).then(function(r){ return r.blob(); }).then(function(b){
    var fr = new FileReader();
    fr.onload = function(){ console.log('CAIRN_DL:' + JSON.stringify({name: name, mime: b.type, url: href.slice(0,80), data: String(fr.result).split(',')[1] || ''})); };
    fr.readAsDataURL(b);
  }).catch(function(e){ console.error('CAIRN_DL failed: ' + e); });
};
document.addEventListener('click', function(e){
  var a = e.target && e.target.closest ? e.target.closest('a[download]') : null;
  if (a && a.href) { e.preventDefault(); send(a); }
}, true);
if (!('download' in HTMLAnchorElement.prototype)) {
  Object.defineProperty(HTMLAnchorElement.prototype, 'download', { get: function(){ return this.getAttribute('download') || ''; }, set: function(v){ this.setAttribute('download', String(v)); }, configurable: true });
}
var oc = HTMLAnchorElement.prototype.click;
HTMLAnchorElement.prototype.click = function(){ if (this.hasAttribute('download') && this.href) { send(this); return; } return oc.call(this); };
})();"#;

fn save_download(json: &str) {
    let v: serde_json::Value = match serde_json::from_str(json) { Ok(v) => v, Err(e) => { log(format!("DOWNLOAD bad json: {e}")); return; } };
    let name: String = v["name"].as_str().unwrap_or("download").chars().filter(|c| c.is_alphanumeric() || *c == '.' || *c == '-' || *c == '_').collect();
    let bytes = base64_decode(v["data"].as_str().unwrap_or(""));
    let dir = std::env::current_dir().unwrap().join("cairn-probe-data").join("downloads");
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join(if name.is_empty() { "download".to_string() } else { name });
    match std::fs::write(&path, &bytes) {
        Ok(()) => log(format!("DOWNLOAD saved {} ({} bytes, mime {})", path.display(), bytes.len(), v["mime"].as_str().unwrap_or("?"))),
        Err(e) => log(format!("DOWNLOAD write failed: {e}")),
    }
}

fn base64_decode(s: &str) -> Vec<u8> {
    let val = |c: u8| match c { b'A'..=b'Z' => Some(c - b'A'), b'a'..=b'z' => Some(c - b'a' + 26), b'0'..=b'9' => Some(c - b'0' + 52), b'+' => Some(62), b'/' => Some(63), _ => None };
    let mut out = Vec::new();
    let (mut acc, mut bits) = (0u32, 0u32);
    for c in s.bytes() {
        if let Some(v) = val(c) {
            acc = (acc << 6) | v as u32;
            bits += 6;
            if bits >= 8 { bits -= 8; out.push((acc >> bits) as u8); acc &= (1 << bits) - 1; }
        }
    }
    out
}

fn user_content(servo: &Servo) -> Rc<servo::UserContentManager> {
    let m = Rc::new(servo::UserContentManager::new(servo));
    m.add_script(Rc::new(servo::UserScript::new(CRYPTO_POLYFILL.to_string(), None)));
    m.add_script(Rc::new(servo::UserScript::new(DOWNLOAD_HOOK.to_string(), None)));
    if std::env::var_os("CFDEBUG").is_some() {
        m.add_script(Rc::new(servo::UserScript::new(CF_DEBUG.to_string(), None)));
    }
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
        let window = Rc::new(el.create_window(Window::default_attributes().with_title("cairn gpu probe (Alt+1-9 switch tab, Ctrl+Q quits)")).expect("window"));
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
        let dctx: Rc<dyn servo::RenderingContext> = ctx.clone();
        let delegate = Rc::new(Delegate { window: Some(window.clone()), ctx: Some(dctx), spawned: self.spawned.clone(), cursor: self.cursor.clone(), menu: RefCell::default() });
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
        self.state = Some(Running { window, servo, ctx, tabs, active: 0, cursor: servo::DevicePoint::default(), mods: Default::default() });
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
                self.cursor.set((position.x as f32, position.y as f32));
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
            WindowEvent::ModifiersChanged(m) => s.mods = m.state(),
            WindowEvent::KeyboardInput { event, .. } => {
                let pressed = event.state == ElementState::Pressed;
                if pressed && s.mods.control_key() && event.logical_key == Key::Character("q".into()) {
                    el.exit();
                    return;
                }
                if pressed && s.mods.alt_key() {
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
                            return;
                        }
                    }
                }
                let key = match &event.logical_key {
                    Key::Character(c) => servo::Key::Character(c.to_string()),
                    Key::Named(n) => match n {
                        NamedKey::Space => servo::Key::Character(" ".into()),
                        NamedKey::Enter => servo::Key::Named(servo::NamedKey::Enter),
                        NamedKey::Tab => servo::Key::Named(servo::NamedKey::Tab),
                        NamedKey::Backspace => servo::Key::Named(servo::NamedKey::Backspace),
                        NamedKey::Delete => servo::Key::Named(servo::NamedKey::Delete),
                        NamedKey::Escape => servo::Key::Named(servo::NamedKey::Escape),
                        NamedKey::ArrowLeft => servo::Key::Named(servo::NamedKey::ArrowLeft),
                        NamedKey::ArrowRight => servo::Key::Named(servo::NamedKey::ArrowRight),
                        NamedKey::ArrowUp => servo::Key::Named(servo::NamedKey::ArrowUp),
                        NamedKey::ArrowDown => servo::Key::Named(servo::NamedKey::ArrowDown),
                        NamedKey::Home => servo::Key::Named(servo::NamedKey::Home),
                        NamedKey::End => servo::Key::Named(servo::NamedKey::End),
                        NamedKey::PageUp => servo::Key::Named(servo::NamedKey::PageUp),
                        NamedKey::PageDown => servo::Key::Named(servo::NamedKey::PageDown),
                        NamedKey::Shift => servo::Key::Named(servo::NamedKey::Shift),
                        NamedKey::Control => servo::Key::Named(servo::NamedKey::Control),
                        NamedKey::Alt => servo::Key::Named(servo::NamedKey::Alt),
                        _ => servo::Key::Named(servo::NamedKey::Unidentified),
                    },
                    _ => servo::Key::Named(servo::NamedKey::Unidentified),
                };
                let mut m = servo::Modifiers::empty();
                if s.mods.shift_key() { m |= servo::Modifiers::SHIFT; }
                if s.mods.control_key() { m |= servo::Modifiers::CONTROL; }
                if s.mods.alt_key() { m |= servo::Modifiers::ALT; }
                if s.mods.super_key() { m |= servo::Modifiers::META; }
                let st = if pressed { servo::KeyState::Down } else { servo::KeyState::Up };
                s.tabs[s.active].notify_input_event(InputEvent::Keyboard(servo::KeyboardEvent::new_without_event(
                    st, key, servo::Code::Unidentified, servo::Location::Standard, m, event.repeat, false,
                )));
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
        if let Some(s) = self.state.as_mut() {
            let fresh: Vec<WebView> = self.spawned.borrow_mut().drain(..).collect();
            for wv in fresh {
                s.tabs[s.active].hide();
                wv.show();
                wv.resize(s.window.inner_size());
                s.tabs.push(wv);
                s.active = s.tabs.len() - 1;
                log(format!("NEWTAB active: tab {}", s.active + 1));
                s.window.request_redraw();
            }
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
    let spawned: Rc<RefCell<Vec<WebView>>> = Rc::default();
    let dctx: Rc<dyn servo::RenderingContext> = ctx.clone();
    let mk_delegate = || Rc::new(Delegate { window: None, ctx: Some(dctx.clone()), spawned: spawned.clone(), cursor: Rc::default(), menu: RefCell::default() });
    let tabs: RefCell<Vec<WebView>> = RefCell::new(
        urls.iter()
            .enumerate()
            .map(|(i, u)| {
                let wv = WebViewBuilder::new(&servo, ctx.clone()).user_content_manager(ucm.clone()).url(url::Url::parse(u).unwrap()).delegate(mk_delegate()).build();
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
    let click_b = |wv: &WebView, x: f32, y: f32, b: MouseButton| {
        let p: servo::WebViewPoint = servo::DevicePoint::new(x, y).into();
        wv.notify_input_event(InputEvent::MouseMove(MouseMoveEvent::new(p)));
        wv.notify_input_event(InputEvent::MouseButton(MouseButtonEvent::new(MouseButtonAction::Down, b, p)));
        wv.notify_input_event(InputEvent::MouseButton(MouseButtonEvent::new(MouseButtonAction::Up, b, p)));
    };
    let click = |wv: &WebView, x: f32, y: f32| click_b(wv, x, y, MouseButton::Primary);
    let key = |wv: &WebView, k: servo::Key, code: servo::Code| {
        for st in [servo::KeyState::Down, servo::KeyState::Up] {
            wv.notify_input_event(InputEvent::Keyboard(servo::KeyboardEvent::new_without_event(
                st, k.clone(), code, servo::Location::Standard, servo::Modifiers::empty(), false, false,
            )));
        }
    };
    while t.elapsed() < limit {
        servo.spin_event_loop();
        let fresh: Vec<WebView> = spawned.borrow_mut().drain(..).collect();
        for wv in fresh {
            for t in tabs.borrow().iter() {
                t.hide();
            }
            wv.show();
            tabs.borrow_mut().insert(0, wv);
            log("NEWTAB active".to_string());
        }
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
                "rclick" => {
                    let v: Vec<f32> = arg.split(',').filter_map(|x| x.parse().ok()).collect();
                    click_b(wv, v[0], v[1], MouseButton::Secondary);
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
    let mut app = App { waker: Waker(el.create_proxy()), urls, secs, start: Instant::now(), last_mem: Instant::now(), state: None, spawned: Rc::default(), cursor: Rc::default() };
    let r = el.run_app(&mut app);
    log(format!("exit: {r:?}"));
    log(mem());
    eprintln!("\nSend back: {name}");
}
