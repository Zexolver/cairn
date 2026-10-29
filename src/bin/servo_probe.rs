//! Probe: load a URL in Servo (software rendering) and save a screenshot.
//! usage: servo_probe <url> <seconds> <out.png>
use servo::{DeviceIntRect, ServoBuilder, SoftwareRenderingContext, WebViewBuilder, RenderingContext};
use std::{rc::Rc, time::{Duration, Instant}};

fn main() {
    let mut args = std::env::args().skip(1);
    let url = args.next().unwrap_or_else(|| "data:text/html,<body style='background:red'>hi".into());
    let secs: u64 = args.next().and_then(|s| s.parse().ok()).unwrap_or(15);
    let out = args.next().unwrap_or_else(|| "/tmp/probe.png".into());
    let (w, h) = (1280u32, 800u32);
    let ctx = Rc::new(SoftwareRenderingContext::new(dpi::PhysicalSize::new(w, h)).expect("ctx"));
    let servo = ServoBuilder::default().build();
    let wv = WebViewBuilder::new(&servo, ctx.clone()).url(url::Url::parse(&url).expect("url")).build();
    let start = Instant::now();
    let mut last = String::new();
    while start.elapsed() < Duration::from_secs(secs) {
        servo.spin_event_loop();
        wv.paint();
        let s = format!("{:?} title={:?} url={:?}", wv.load_status(), wv.page_title(), wv.url().map(|u| u.to_string()));
        if s != last {
            eprintln!("[{:>5.1}s] {s}", start.elapsed().as_secs_f32());
            last = s;
        }
        std::thread::sleep(Duration::from_millis(16));
    }
    if let Ok(st) = std::fs::read_to_string("/proc/self/status") {
        for l in st.lines().filter(|l| l.starts_with("VmRSS") || l.starts_with("VmHWM") || l.starts_with("Threads") || l.starts_with("RssAnon")) { eprintln!("{l}"); }
    }
    match ctx.read_to_image(DeviceIntRect::from_size(euclid::Size2D::new(w as i32, h as i32))) {
        Some(img) => { img.save(&out).expect("save"); eprintln!("saved {out}"); }
        None => eprintln!("no image"),
    }
}
