//! Probe: load a URL in Servo (software rendering) and save a screenshot.
//! usage: servo_probe <url[,url...]> <seconds> <out.png>   (several urls = several tabs in one engine)
use servo::{DeviceIntRect, ServoBuilder, SoftwareRenderingContext, WebViewBuilder, RenderingContext};
use std::{rc::Rc, time::{Duration, Instant}};

/// Estimate zram-style savings: compress every resident anonymous 4K page of this process.
fn compress_report() {
    use std::{fs::File, io::Read, os::unix::fs::FileExt};
    let smaps = std::fs::read_to_string("/proc/self/smaps").unwrap();
    let mem = File::open("/proc/self/mem").unwrap();
    let pm = File::open("/proc/self/pagemap").unwrap();
    let mut regions = vec![];
    let mut cur: Option<(u64, u64)> = None;
    for l in smaps.lines() {
        let mut it = l.split_whitespace();
        let first = it.next().unwrap_or("");
        if first.contains('-') && first.chars().next().map_or(false, |c| c.is_ascii_hexdigit()) && !first.ends_with(':') {
            let perms = it.next().unwrap_or("");
            let path = it.nth(3).unwrap_or("");
            cur = None;
            if perms.starts_with("rw") && perms.ends_with('p') && (path.is_empty() || path == "[heap]") {
                let (a, b) = first.split_once('-').unwrap();
                cur = Some((u64::from_str_radix(a, 16).unwrap(), u64::from_str_radix(b, 16).unwrap()));
                regions.push(cur.unwrap());
            }
        }
    }
    let (mut pages, mut zero, mut zs, mut l4, mut raw) = (0u64, 0u64, 0u64, 0u64, 0u64);
    let (mut tz, mut tl) = (std::time::Duration::ZERO, std::time::Duration::ZERO);
    let mut buf = vec![0u8; 4096];
    for (a, b) in regions {
        let n = ((b - a) / 4096) as usize;
        let mut ent = vec![0u8; n * 8];
        if pm.read_exact_at(&mut ent, a / 4096 * 8).is_err() { continue; }
        for i in 0..n {
            let e = u64::from_le_bytes(ent[i * 8..i * 8 + 8].try_into().unwrap());
            if e >> 63 & 1 == 0 { continue; }
            if mem.read_exact_at(&mut buf, a + i as u64 * 4096).is_err() { continue; }
            pages += 1;
            raw += 4096;
            if buf.iter().all(|&x| x == 0) { zero += 1; continue; }
            let t = std::time::Instant::now();
            zs += zstd::bulk::compress(&buf, 1).map(|v| v.len().min(4096)).unwrap_or(4096) as u64;
            tz += t.elapsed();
            let t = std::time::Instant::now();
            l4 += lz4_flex::block::compress(&buf).len().min(4096) as u64;
            tl += t.elapsed();
        }
    }
    let _ = File::open("/dev/null").and_then(|mut f| f.read(&mut [0u8; 1]));
    let nz = pages - zero;
    let mb = |x: u64| x / 1048576;
    eprintln!("COMPRESS resident-anon={}MB pages={} zero-pages={} nonzero={}MB", mb(raw), pages, zero, mb(nz * 4096));
    eprintln!("COMPRESS zstd1: {}MB ({:.0}% of nonzero) {:.2}s | lz4: {}MB ({:.0}%) {:.2}s", mb(zs), zs as f64 * 100.0 / (nz * 4096).max(1) as f64, tz.as_secs_f64(), mb(l4), l4 as f64 * 100.0 / (nz * 4096).max(1) as f64, tl.as_secs_f64());
    eprintln!("COMPRESS if all cold: {}MB -> zstd1 {}MB (saves {}MB), lz4 {}MB (saves {}MB)", mb(raw), mb(zs), mb(raw) - mb(zs), mb(l4), mb(raw) - mb(l4));
}

fn main() {
    let mut args = std::env::args().skip(1);
    let url = args.next().unwrap_or_else(|| "data:text/html,<body style='background:red'>hi".into());
    let secs: u64 = args.next().and_then(|s| s.parse().ok()).unwrap_or(15);
    let out = args.next().unwrap_or_else(|| "/tmp/probe.png".into());
    let (w, h) = (1280u32, 800u32);
    let servo = ServoBuilder::default().build();
    let shared = std::env::var("SHARED").is_ok();
    let shared_ctx = Rc::new(SoftwareRenderingContext::new(dpi::PhysicalSize::new(w, h)).expect("ctx"));
    let tabs: Vec<_> = url.split(',').enumerate().map(|(i, u)| {
        let ctx = if shared { shared_ctx.clone() } else { Rc::new(SoftwareRenderingContext::new(dpi::PhysicalSize::new(w, h)).expect("ctx")) };
        let wv = WebViewBuilder::new(&servo, ctx.clone()).url(url::Url::parse(u).expect("url")).build();
        if shared && i > 0 { wv.hide(); }
        (wv, ctx)
    }).collect();
    let (wv, ctx) = (&tabs[0].0, &tabs[0].1);
    let start = Instant::now();
    let mut last = String::new();
    while start.elapsed() < Duration::from_secs(secs) {
        servo.spin_event_loop();
        if shared { tabs[0].0.paint(); } else { for t in &tabs { t.0.paint(); } }
        let s = format!("{:?} title={:?} url={:?}", wv.load_status(), wv.page_title(), wv.url().map(|u| u.to_string()));
        if s != last {
            eprintln!("[{:>5.1}s] {s}", start.elapsed().as_secs_f32());
            last = s;
        }
        std::thread::sleep(Duration::from_millis(16));
    }
    if std::env::var("COMPRESS").is_ok() { compress_report(); }
    for (i, t) in tabs.iter().enumerate() { eprintln!("tab{i}: {:?} title={:?}", t.0.load_status(), t.0.page_title()); }
    if let Ok(st) = std::fs::read_to_string("/proc/self/status") {
        for l in st.lines().filter(|l| l.starts_with("VmRSS") || l.starts_with("VmHWM") || l.starts_with("Threads") || l.starts_with("RssAnon")) { eprintln!("{l}"); }
    }
    match ctx.read_to_image(DeviceIntRect::from_size(euclid::Size2D::new(w as i32, h as i32))) {
        Some(img) => { img.save(&out).expect("save"); eprintln!("saved {out}"); }
        None => eprintln!("no image"),
    }
}
