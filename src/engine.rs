//! Engine boundary. One `Engine` hosts every tab of one container (one process in the final design).
use cairn_core::container::TabId;

pub struct Frame {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

pub enum Event {
    Title(TabId, String),
    Url(TabId, String),
    Loading(TabId, bool),
    Blocked(u32),
    Redraw,
}

pub enum Input {
    Move(f32, f32),
    Down(f32, f32, u8),
    Up(f32, f32, u8),
    Scroll(f32, f32, f32, f32),
    Key(String, bool),
}

pub trait Engine {
    fn open(&mut self, tab: TabId, url: &str);
    fn close(&mut self, tab: TabId);
    fn navigate(&mut self, tab: TabId, url: &str);
    fn back(&mut self, tab: TabId);
    fn forward(&mut self, tab: TabId);
    fn reload(&mut self, tab: TabId);
    fn resize(&mut self, width: u32, height: u32);
    fn input(&mut self, tab: TabId, input: Input);
    /// Drive the engine; returns state changes since the last call.
    fn pump(&mut self) -> Vec<Event>;
    fn frame(&mut self, tab: TabId) -> Option<Frame>;
}

/// Placeholder engine used when built without the `servo` feature.
#[derive(Default)]
pub struct StubEngine {
    tabs: Vec<(TabId, String)>,
    pending: Vec<Event>,
    size: (u32, u32),
}

impl Engine for StubEngine {
    fn open(&mut self, tab: TabId, url: &str) {
        self.tabs.push((tab, url.into()));
        self.navigate(tab, url);
    }
    fn close(&mut self, tab: TabId) {
        self.tabs.retain(|(t, _)| *t != tab);
    }
    fn navigate(&mut self, tab: TabId, url: &str) {
        if let Some(t) = self.tabs.iter_mut().find(|(t, _)| *t == tab) {
            t.1 = url.into();
        }
        self.pending.push(Event::Url(tab, url.into()));
        self.pending.push(Event::Title(tab, url.into()));
    }
    fn back(&mut self, _: TabId) {}
    fn forward(&mut self, _: TabId) {}
    fn reload(&mut self, _: TabId) {}
    fn resize(&mut self, width: u32, height: u32) {
        self.size = (width, height);
    }
    fn input(&mut self, _: TabId, _: Input) {}
    fn pump(&mut self) -> Vec<Event> {
        std::mem::take(&mut self.pending)
    }
    fn frame(&mut self, tab: TabId) -> Option<Frame> {
        let (w, h) = self.size;
        let url = &self.tabs.iter().find(|(t, _)| *t == tab)?.1;
        let hash = url.bytes().fold(0u32, |a, b| a.wrapping_mul(31).wrapping_add(b as u32));
        let px = [200 + (hash & 0x3f) as u8, 200 + ((hash >> 8) & 0x3f) as u8, 200 + ((hash >> 16) & 0x3f) as u8, 255];
        Some(Frame { width: w.max(1), height: h.max(1), rgba: px.iter().copied().cycle().take((w.max(1) * h.max(1) * 4) as usize).collect() })
    }
}
