//! Probe: can two Servo engines coexist in one process, and can we read pixels back?
use servo::{
    DeviceIntRect, RenderingContext, ServoBuilder, SoftwareRenderingContext, WebViewBuilder,
};
use std::{rc::Rc, time::{Duration, Instant}};

fn make(label: &str, html: &str) -> (servo::Servo, servo::WebView, Rc<SoftwareRenderingContext>) {
    let ctx = Rc::new(SoftwareRenderingContext::new(dpi::PhysicalSize::new(200, 100)).expect("ctx"));
    let servo = ServoBuilder::default().build();
    let url = url::Url::parse(&format!("data:text/html,{html}")).unwrap();
    let wv = WebViewBuilder::new(&servo, ctx.clone()).url(url).build();
    eprintln!("[{label}] engine + webview created");
    (servo, wv, ctx)
}

fn main() {
    let a = make("A", "<body style='background:red'>A</body>");
    let second = std::env::args().nth(1).as_deref() == Some("two");
    let b = second.then(|| make("B", "<body style='background:blue'>B</body>"));
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(8) {
        a.0.spin_event_loop();
        a.1.paint();
        if let Some(b) = &b { b.0.spin_event_loop(); b.1.paint(); }
        std::thread::sleep(Duration::from_millis(16));
    }
    for (name, e) in [("A", Some(&a)), ("B", b.as_ref())] {
        let Some(e) = e else { continue };
        let img = e.2.read_to_image(DeviceIntRect::from_size(euclid::Size2D::new(200, 100)));
        match img {
            Some(i) => eprintln!("[{name}] pixel(10,10) = {:?}", i.get_pixel(10, 10)),
            None => eprintln!("[{name}] no image"),
        }
    }
}
