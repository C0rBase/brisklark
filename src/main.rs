//! Brisklark: one window, a row of tabs, one field, and the page.
//! The page engine is WebView2, the Edge runtime already on every Windows 11
//! machine, so this process only draws the frame and keeps the lists.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod shield;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    cell::RefCell,
    fs,
    path::PathBuf,
    rc::Rc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tao::{
    dpi::LogicalSize,
    event::{Event, StartCause, WindowEvent},
    event_loop::{ControlFlow, EventLoopBuilder, EventLoopProxy},
    platform::windows::WindowBuilderExtWindows,
    window::{ResizeDirection, Window, WindowBuilder},
};
use webview2_com::{AcceleratorKeyPressedEventHandler, Microsoft::Web::WebView2::Win32::*};
use windows::{
    Win32::UI::Input::KeyboardAndMouse::{GetKeyState, VK_CONTROL, VK_MENU, VK_SHIFT},
};
use wry::{
    dpi::{LogicalPosition, LogicalSize as WLogicalSize},
    MemoryUsageLevel, NewWindowResponse, PageLoadEvent, Rect, WebContext, WebView, WebViewBuilder,
    WebViewBuilderExtWindows, WebViewExtWindows,
};

const SIDE: f64 = 240.0; // sidebar width
const BAR: f64 = 44.0; // top bar height
const HISTORY_CAP: usize = 5000;
const BLANK: &str = "about:blank"; // a new tab nobody has typed into yet

// ---------- what is kept on disk ----------

#[derive(Serialize, Deserialize, Clone, Default)]
struct Saved {
    url: String,
    title: String,
    #[serde(default)]
    pinned: bool,
}

#[derive(Serialize, Deserialize, Clone)]
struct Visit {
    url: String,
    title: String,
    visits: u32,
    last: u64,
}

#[derive(Serialize, Deserialize)]
#[serde(default)]
struct State {
    tabs: Vec<Saved>,
    active: usize,
    bookmarks: Vec<Saved>,
    top: bool,
    folded: bool,
    engine: String,
}

impl Default for State {
    fn default() -> Self {
        State {
            tabs: vec![],
            active: 0,
            bookmarks: vec![],
            top: false,
            folded: false,
            engine: "https://www.google.com/search?q=%s".into(),
        }
    }
}

pub(crate) fn dir(var: &str) -> PathBuf {
    PathBuf::from(std::env::var(var).unwrap_or_else(|_| ".".into())).join("Brisklark")
}

fn load<T: for<'a> Deserialize<'a> + Default>(name: &str) -> T {
    fs::read(dir("APPDATA").join(name))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

fn save<T: Serialize>(name: &str, value: &T) {
    let d = dir("APPDATA");
    let _ = fs::create_dir_all(&d);
    // Write then rename, so a crash mid-write never leaves half a file.
    let tmp = d.join(format!("{name}.tmp"));
    if fs::write(&tmp, serde_json::to_vec(value).unwrap_or_default()).is_ok() {
        let _ = fs::rename(tmp, d.join(name));
    }
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

// ---------- addresses ----------

pub(crate) fn host(url: &str) -> &str {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    let rest = rest.split(['/', '?', '#']).next().unwrap_or("");
    let rest = rest.rsplit_once('@').map_or(rest, |(_, h)| h);
    rest.split(':').next().unwrap_or("")
}

fn encode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
            b' ' => "+".into(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// Words go to the search engine; anything shaped like an address is visited.
fn resolve(text: &str, engine: &str) -> String {
    let t = text.trim();
    let b = t.as_bytes();
    if t.contains("://") || ["about:", "data:", "file:", "edge:"].iter().any(|p| t.starts_with(p)) {
        return t.into();
    }
    if b.len() > 2 && b[0].is_ascii_alphabetic() && &b[1..3] == b":\\" {
        return format!("file:///{}", t.replace('\\', "/"));
    }
    let local = t.starts_with("localhost") || t.starts_with("127.") || host(t).ends_with(".local");
    let looks = !t.contains(char::is_whitespace)
        && (local || (t.contains('.') && !t.starts_with('.') && !t.ends_with('.')));
    if looks {
        return format!("{}://{t}", if local { "http" } else { "https" });
    }
    engine.replace("%s", &encode(t))
}

// ---------- keys ----------

#[derive(Debug, Clone, Copy, PartialEq)]
enum Cmd {
    Field,    // Ctrl+L
    Switch,   // Ctrl+K
    NewTab,   // Ctrl+T
    Private,  // Ctrl+Shift+N
    Close,    // Ctrl+W
    Reopen,   // Ctrl+Shift+T
    Back,     // Ctrl+[
    Forward,  // Ctrl+]
    PrevTab,  // Ctrl+Shift+[ , Ctrl+Shift+Tab
    NextTab,  // Ctrl+Shift+] , Ctrl+Tab
    Nth(usize), // Ctrl+1..8, Ctrl+9 = last
    Layout,   // Ctrl+Shift+S
    Fold,     // Ctrl+S
    Bookmark, // Ctrl+Shift+B
    Duplicate, // Ctrl+D
    CopyUrl,  // Ctrl+Shift+C
    PasteGo,  // Ctrl+Shift+V
    History,  // Ctrl+Y
    Bookmarks, // Ctrl+Shift+O
}

fn shortcut(vk: u32, ctrl: bool, shift: bool, alt: bool) -> Option<Cmd> {
    use Cmd::*;
    if !ctrl || alt {
        return None;
    }
    let key = char::from_u32(vk).unwrap_or('\0');
    Some(match (key, shift) {
        ('L', false) => Field,
        ('K', false) => Switch,
        ('T', false) => NewTab,
        ('N', true) => Private,
        ('W', false) => Close,
        ('T', true) => Reopen,
        ('S', true) => Layout,
        ('S', false) => Fold,
        ('B', true) => Bookmark,
        ('D', false) => Duplicate,
        ('C', true) => CopyUrl,
        ('V', true) => PasteGo,
        ('Y', false) => History,
        ('O', true) => Bookmarks,
        ('1'..='8', false) => Nth(key as usize - '1' as usize),
        ('9', false) => Nth(usize::MAX),
        _ => match (vk, shift) {
            (0xDB, false) => Back,    // [
            (0xDD, false) => Forward, // ]
            (0xDB, true) | (0x09, true) => PrevTab,
            (0xDD, true) | (0x09, false) => NextTab,
            _ => return None,
        },
    })
}

/// Our shortcuts work wherever focus is, page or frame, and a page cannot fake
/// them: they come from WebView2's own key handling, not from page script.
fn hook_keys(view: &WebView, proxy: EventLoopProxy<Ev>) {
    let handler = AcceleratorKeyPressedEventHandler::create(Box::new(move |_, args| {
        let Some(args) = args else { return Ok(()) };
        unsafe {
            let mut kind = COREWEBVIEW2_KEY_EVENT_KIND::default();
            args.KeyEventKind(&mut kind)?;
            if kind != COREWEBVIEW2_KEY_EVENT_KIND_KEY_DOWN {
                return Ok(());
            }
            let mut vk = 0u32;
            args.VirtualKey(&mut vk)?;
            let down = |k: u16| GetKeyState(k as i32) < 0;
            if let Some(cmd) = shortcut(vk, down(VK_CONTROL.0), down(VK_SHIFT.0), down(VK_MENU.0)) {
                args.SetHandled(true)?;
                let _ = proxy.send_event(Ev::Cmd(cmd));
            }
        }
        Ok(())
    }));
    let mut token = 0i64;
    unsafe {
        let _ = view.controller().add_AcceleratorKeyPressed(&handler, &mut token);
    }
}

// ---------- the browser ----------

enum Ev {
    Cmd(Cmd),
    Ipc(String),
    Title(u32, String),
    Url(u32, String),
    Loaded(u32, String),
    Open(String, bool),
    Shield(Vec<u8>), // a freshly compiled ad blocker
}

struct Tab {
    id: u32,
    url: String,
    title: String,
    pinned: bool,
    private: bool,
    view: Option<WebView>, // None until first shown: restored tabs cost nothing
    top: Rc<RefCell<String>>, // address of the page, for the blocker
}

struct App {
    window: Window,
    env: ICoreWebView2Environment,
    shield: shield::Shared,
    chrome: WebView,
    proxy: EventLoopProxy<Ev>,
    tabs: Vec<Tab>,
    active: Option<u32>,
    next: u32,
    closed: Vec<Saved>,
    history: Vec<Visit>,
    state: State,
    overlay: bool,
    dirty: bool,
    last_save: Instant,
    quit: bool,
}

fn rect(x: f64, y: f64, w: f64, h: f64) -> Rect {
    Rect {
        position: LogicalPosition::new(x, y).into(),
        size: WLogicalSize::new(w.max(0.0), h.max(0.0)).into(),
    }
}

impl App {
    fn idx(&self, id: u32) -> Option<usize> {
        self.tabs.iter().position(|t| t.id == id)
    }
    fn current(&self) -> Option<&Tab> {
        self.active.and_then(|id| self.tabs.iter().find(|t| t.id == id))
    }

    fn page_rect(&self) -> Rect {
        let s = self.window.inner_size().to_logical::<f64>(self.window.scale_factor());
        if self.state.folded {
            rect(0.0, 0.0, s.width, s.height)
        } else if self.state.top {
            rect(0.0, BAR, s.width, s.height - BAR)
        } else {
            rect(SIDE, 0.0, s.width - SIDE, s.height)
        }
    }

    fn layout(&self) {
        let s = self.window.inner_size().to_logical::<f64>(self.window.scale_factor());
        let chrome = if self.overlay {
            rect(0.0, 0.0, s.width, s.height)
        } else if self.state.folded {
            rect(0.0, 0.0, 0.0, 0.0)
        } else if self.state.top {
            rect(0.0, 0.0, s.width, BAR)
        } else {
            rect(0.0, 0.0, SIDE, s.height)
        };
        let _ = self.chrome.set_bounds(chrome);
        let _ = self.chrome.set_visible(self.overlay || !self.state.folded);
        if let Some(view) = self.current().and_then(|t| t.view.as_ref()) {
            let _ = view.set_bounds(self.page_rect());
            let _ = view.set_visible(!self.overlay);
        }
    }

    fn make_view(&mut self, i: usize) {
        let (id, private, url, top) = {
            let t = &self.tabs[i];
            (t.id, t.private, t.url.clone(), t.top.clone())
        };
        *top.borrow_mut() = url.clone();
        let (p1, p2, p3) = (self.proxy.clone(), self.proxy.clone(), self.proxy.clone());
        // Every tab shares the sidebar's environment: one Edge browser process
        // for the whole app. Private tabs are an InPrivate controller on it.
        let built = WebViewBuilder::new()
            .with_environment(self.env.clone())
            .with_incognito(private)
            .with_url(&url)
            .with_bounds(self.page_rect())
            .with_visible(false)
            .with_browser_accelerator_keys(true) // keeps Ctrl+F, Ctrl+P, F5, Alt+arrows
            .with_document_title_changed_handler(move |t| {
                let _ = p1.send_event(Ev::Title(id, t));
            })
            .with_on_page_load_handler(move |e, url| {
                let _ = p2.send_event(match e {
                    PageLoadEvent::Started => Ev::Url(id, url),
                    PageLoadEvent::Finished => Ev::Loaded(id, url),
                });
            })
            .with_new_window_req_handler(move |url, features| {
                // A sized window.open is a sign-in popup that needs its opener:
                // let WebView2 open it. A plain target=_blank link is a tab.
                if features.size.is_some() {
                    return NewWindowResponse::Allow;
                }
                let _ = p3.send_event(Ev::Open(url, private));
                NewWindowResponse::Deny
            })
            .build_as_child(&self.window);
        if let Ok(view) = built {
            hook_keys(&view, self.proxy.clone());
            shield::attach(&view, self.shield.clone(), top);
            self.tabs[i].view = Some(view);
        }
    }

    fn select(&mut self, id: u32) {
        let Some(i) = self.idx(id) else { return };
        if let Some(old) = self.current().and_then(|t| t.view.as_ref()) {
            if self.active != Some(id) {
                let _ = old.set_visible(false);
                let _ = old.set_memory_usage_level(MemoryUsageLevel::Low);
            }
        }
        self.active = Some(id);
        if self.tabs[i].url == BLANK {
            // A new tab has no page until an address is typed: just the field.
            self.render();
            return self.focus_field("here", true);
        }
        if self.tabs[i].view.is_none() {
            self.make_view(i);
        }
        if let Some(v) = &self.tabs[i].view {
            let _ = v.set_memory_usage_level(MemoryUsageLevel::Normal);
            let _ = v.focus();
        }
        self.overlay = false;
        self.layout();
        self.render();
    }

    fn add_tab(&mut self, url: String, private: bool, at: Option<usize>) -> u32 {
        let id = self.next;
        self.next += 1;
        let title = if url == BLANK { "New Tab".into() } else { url.clone() };
        let tab = Tab { id, title, url, pinned: false, private, view: None, top: Default::default() };
        let pins = self.tabs.iter().filter(|t| t.pinned).count();
        let at = at.unwrap_or(self.tabs.len()).clamp(pins, self.tabs.len());
        self.tabs.insert(at, tab);
        self.dirty = true;
        id
    }

    fn close(&mut self, id: u32) {
        let Some(i) = self.idx(id) else { return };
        let tab = self.tabs.remove(i); // dropping the view frees its page
        if !tab.private {
            self.closed.push(Saved { url: tab.url, title: tab.title, pinned: false });
        }
        if self.active == Some(id) {
            self.active = None;
            match self.tabs.get(i).or_else(|| self.tabs.last()).map(|t| t.id) {
                Some(next) => self.select(next),
                None => self.focus_field("new", false),
            }
        }
        self.dirty = true;
        self.layout();
        self.render();
    }

    fn go(&mut self, text: &str, new: bool, private: bool) {
        if text.trim().is_empty() {
            return;
        }
        let url = resolve(text, &self.state.engine);
        match self.active.filter(|_| !new) {
            Some(id) => {
                let i = self.idx(id).unwrap();
                self.tabs[i].url = url.clone();
                match &self.tabs[i].view {
                    Some(v) => {
                        let _ = v.load_url(&url);
                        let _ = v.focus();
                    }
                    None => self.select(id),
                }
            }
            None => {
                let id = self.add_tab(url, private, None);
                self.select(id);
            }
        }
        self.overlay = false;
        self.layout();
        self.render();
    }

    fn eval(&self, f: &str, arg: Value) {
        let _ = self.chrome.evaluate_script(&format!("{f}({arg})"));
    }

    fn render(&self) {
        let tabs: Vec<Value> = self
            .tabs
            .iter()
            .map(|t| json!({"id": t.id, "title": t.title, "url": t.url, "pinned": t.pinned,
                            "private": t.private, "active": Some(t.id) == self.active}))
            .collect();
        let url = self.current().map(|t| t.url.as_str()).unwrap_or("");
        let marked = self.state.bookmarks.iter().any(|b| b.url == url);
        self.eval("render", json!({"tabs": tabs, "top": self.state.top, "folded": self.state.folded, "overlay": self.overlay, "bookmarked": marked}));
        let title = self.current().map_or("Brisklark".into(), |t| t.title.clone());
        self.window.set_title(&title);
    }

    fn focus_field(&mut self, mode: &str, overlay_always: bool) {
        self.overlay = overlay_always || self.state.top || self.state.folded;
        self.layout();
        let _ = self.chrome.focus();
        let url = if mode == "here" { self.current().map_or("", |t| t.url.as_str()) } else { "" };
        let url = if url == BLANK { "" } else { url };
        self.eval("focusField", json!({"mode": mode, "overlay": self.overlay, "text": url}));
    }

    fn done(&mut self) {
        // Leaving a new tab without typing anything takes it away again, back
        // to the tab before. With nothing to go back to, the cursor goes back
        // into the box instead, so the click is never lost.
        if let Some(t) = self.current().filter(|t| t.url == BLANK) {
            return if self.tabs.len() > 1 { self.close(t.id) } else { self.focus_field("here", true) };
        }
        if self.tabs.is_empty() {
            return self.focus_field("new", false);
        }
        self.overlay = false;
        self.layout();
        if let Some(v) = self.current().and_then(|t| t.view.as_ref()) {
            let _ = v.focus();
        }
    }

    fn visit(&mut self, url: &str, title: &str) {
        if url.starts_with("about:") || url.starts_with("data:") {
            return;
        }
        match self.history.iter_mut().find(|v| v.url == url) {
            Some(v) => {
                v.visits += 1;
                v.last = now();
                if !title.is_empty() {
                    v.title = title.into();
                }
            }
            None => self.history.push(Visit { url: url.into(), title: title.into(), visits: 1, last: now() }),
        }
        if self.history.len() > HISTORY_CAP {
            self.history.sort_by(|a, b| b.last.cmp(&a.last));
            self.history.truncate(HISTORY_CAP * 9 / 10);
        }
        self.dirty = true;
    }

    fn suggest(&self, q: &str, tabs_only: bool) {
        let q = q.trim().to_lowercase();
        let hit = |title: &str, url: &str| title.to_lowercase().contains(&q) || url.to_lowercase().contains(&q);
        let mut out: Vec<Value> = vec![];
        if !tabs_only && !q.is_empty() {
            out.push(json!({"kind": "go", "title": q, "url": resolve(&q, &self.state.engine)}));
        }
        for t in self.tabs.iter().filter(|t| hit(&t.title, &t.url)).take(if tabs_only { 20 } else { 3 }) {
            out.push(json!({"kind": "tab", "id": t.id, "title": t.title, "url": t.url}));
        }
        if !tabs_only && !q.is_empty() {
            for b in self.state.bookmarks.iter().filter(|b| hit(&b.title, &b.url)).take(3) {
                out.push(json!({"kind": "bookmark", "title": b.title, "url": b.url}));
            }
            // ponytail: linear scan of ≤5000 visits per keystroke; an index if history grows past that.
            let mut found: Vec<&Visit> = self.history.iter().filter(|v| hit(&v.title, &v.url)).collect();
            let starts = |v: &Visit| host(&v.url).trim_start_matches("www.").starts_with(&q);
            found.sort_by_key(|v| (!starts(v), std::cmp::Reverse(v.visits), std::cmp::Reverse(v.last)));
            let room = 8 - out.len().min(8);
            let fresh: Vec<&Visit> = found.into_iter().filter(|v| !out.iter().any(|o| o["url"] == v.url)).take(room).collect();
            out.extend(fresh.iter().map(|v| json!({"kind": "history", "title": v.title, "url": v.url})));
        }
        self.eval("suggest", Value::Array(out));
    }

    fn panel(&self, kind: &str, q: &str) {
        let q = q.to_lowercase();
        let hit = |t: &str, u: &str| q.is_empty() || t.to_lowercase().contains(&q) || u.to_lowercase().contains(&q);
        let items: Vec<Value> = if kind == "history" {
            let mut h: Vec<&Visit> = self.history.iter().filter(|v| hit(&v.title, &v.url)).collect();
            h.sort_by(|a, b| b.last.cmp(&a.last));
            h.iter().take(300).map(|v| json!({"title": v.title, "url": v.url, "last": v.last})).collect()
        } else {
            self.state.bookmarks.iter().rev().filter(|b| hit(&b.title, &b.url))
                .map(|b| json!({"title": b.title, "url": b.url})).collect()
        };
        self.eval("panel", json!({"kind": kind, "items": items}));
    }

    fn command(&mut self, cmd: Cmd) {
        let cur = self.active.and_then(|id| self.idx(id));
        fn view(s: &App) -> Option<&WebView> {
            s.current().and_then(|t| t.view.as_ref())
        }
        match cmd {
            Cmd::Field => self.focus_field("here", false),
            Cmd::Switch => self.focus_field("tabs", false),
            Cmd::NewTab | Cmd::Private => {
                let id = self.add_tab(BLANK.into(), cmd == Cmd::Private, None);
                self.select(id);
            }
            Cmd::Close => {
                if let Some(id) = self.active {
                    self.close(id)
                }
            }
            Cmd::Reopen => {
                if let Some(s) = self.closed.pop() {
                    let id = self.add_tab(s.url, false, None);
                    self.select(id);
                }
            }
            Cmd::Back => {
                if let Some(v) = view(self) {
                    let _ = v.evaluate_script("history.back()");
                }
            }
            Cmd::Forward => {
                if let Some(v) = view(self) {
                    let _ = v.evaluate_script("history.forward()");
                }
            }
            Cmd::PrevTab | Cmd::NextTab if !self.tabs.is_empty() => {
                let n = self.tabs.len();
                let i = cur.map_or(0, |i| if cmd == Cmd::NextTab { (i + 1) % n } else { (i + n - 1) % n });
                self.select(self.tabs[i].id);
            }
            Cmd::Nth(k) if !self.tabs.is_empty() => {
                let i = k.min(self.tabs.len() - 1);
                self.select(self.tabs[i].id);
            }
            Cmd::Layout => {
                self.state.top = !self.state.top;
                self.dirty = true;
                self.layout();
                self.render();
            }
            Cmd::Fold => {
                self.state.folded = !self.state.folded;
                self.dirty = true;
                self.layout();
            }
            Cmd::Bookmark => {
                if let Some(t) = self.current().filter(|t| !t.private) {
                    let (url, title) = (t.url.clone(), t.title.clone());
                    match self.state.bookmarks.iter().position(|b| b.url == url) {
                        Some(i) => drop(self.state.bookmarks.remove(i)),
                        None => self.state.bookmarks.push(Saved { url, title, pinned: false }),
                    }
                    self.dirty = true;
                    self.render();
                }
            }
            Cmd::Duplicate => {
                if let Some(i) = cur {
                    let (url, private) = (self.tabs[i].url.clone(), self.tabs[i].private);
                    let id = self.add_tab(url, private, Some(i + 1));
                    self.select(id);
                }
            }
            Cmd::CopyUrl => {
                if let (Some(t), Ok(mut c)) = (self.current(), arboard::Clipboard::new()) {
                    let _ = c.set_text(t.url.clone());
                }
            }
            Cmd::PasteGo => {
                if let Some(text) = arboard::Clipboard::new().ok().and_then(|mut c| c.get_text().ok()) {
                    self.go(&text, false, false);
                }
            }
            Cmd::History | Cmd::Bookmarks => {
                let kind = if cmd == Cmd::History { "history" } else { "bookmarks" };
                self.overlay = true;
                self.layout();
                let _ = self.chrome.focus();
                self.panel(kind, "");
            }
            _ => {}
        }
    }

    fn ipc(&mut self, msg: &str) {
        let Ok(m) = serde_json::from_str::<Value>(msg) else { return };
        let s = |k: &str| m[k].as_str().unwrap_or("").to_string();
        let id = m["id"].as_u64().unwrap_or(0) as u32;
        match m["cmd"].as_str().unwrap_or("") {
            "ready" => {
                self.layout();
                self.render();
                if self.tabs.is_empty() {
                    self.focus_field("new", false);
                }
            }
            "go" => {
                let mode = s("mode");
                self.go(&s("text"), mode == "new" || mode == "private", mode == "private")
            }
            "select" => self.select(id),
            "close" => self.close(id),
            "pin" => {
                if let Some(i) = self.idx(id).filter(|&i| !self.tabs[i].private) {
                    let mut t = self.tabs.remove(i);
                    t.pinned = !t.pinned;
                    let pins = self.tabs.iter().filter(|t| t.pinned).count();
                    self.tabs.insert(pins, t);
                    self.dirty = true;
                    self.render();
                }
            }
            "suggest" => self.suggest(&s("q"), m["tabs"].as_bool().unwrap_or(false)),
            "panel" => self.panel(&s("kind"), &s("q")),
            "unbookmark" => {
                let url = s("url");
                self.state.bookmarks.retain(|b| b.url != url);
                self.dirty = true;
                self.panel("bookmarks", "");
                self.render();
            }
            "command" => match s("name").as_str() {
                "new" => self.command(Cmd::NewTab),
                "bookmark" => self.command(Cmd::Bookmark),
                "history" => self.command(Cmd::History),
                "bookmarks" => self.command(Cmd::Bookmarks),
                "layout" => self.command(Cmd::Layout),
                "fold" => self.command(Cmd::Fold),
                _ => {}
            },
            "done" => self.done(),
            // Our own title bar: the tab row or sidebar is where the window is held.
            "drag" => drop(self.window.drag_window()),
            "resize-top" => drop(self.window.drag_resize_window(ResizeDirection::North)),
            "minimize" => self.window.set_minimized(true),
            "maximize" => self.window.set_maximized(!self.window.is_maximized()),
            "quit" => self.quit = true,
            _ => {}
        }
    }

    fn event(&mut self, ev: Ev) {
        match ev {
            Ev::Cmd(c) => self.command(c),
            Ev::Ipc(m) => self.ipc(&m),
            Ev::Title(id, title) => {
                if let Some(i) = self.idx(id) {
                    // Single-page apps change address without loading: the title is the tell.
                    if let Some(url) = self.tabs[i].view.as_ref().and_then(|v| v.url().ok()) {
                        self.tabs[i].url = url;
                    }
                    self.tabs[i].title = if title.is_empty() { self.tabs[i].url.clone() } else { title };
                    if !self.tabs[i].private {
                        let (u, t) = (self.tabs[i].url.clone(), self.tabs[i].title.clone());
                        if let Some(v) = self.history.iter_mut().find(|v| v.url == u) {
                            v.title = t;
                        }
                    }
                    self.dirty = true;
                    self.render();
                }
            }
            Ev::Url(id, url) => {
                if let Some(i) = self.idx(id) {
                    self.tabs[i].url = url;
                    self.render();
                }
            }
            Ev::Loaded(id, url) => {
                if let Some(i) = self.idx(id) {
                    self.tabs[i].url = url.clone();
                    if !self.tabs[i].private {
                        let title = self.tabs[i].title.clone();
                        self.visit(&url, &title);
                    }
                    self.render();
                }
            }
            Ev::Shield(bytes) => shield::install(&self.shield, &bytes),
            Ev::Open(url, private) => {
                let at = self.active.and_then(|id| self.idx(id)).map(|i| i + 1);
                let id = self.add_tab(url, private, at);
                self.select(id);
            }
        }
    }

    fn persist(&mut self) {
        let kept = |t: &&Tab| !t.private && t.url != BLANK;
        self.state.tabs = self.tabs.iter().filter(kept)
            .map(|t| Saved { url: t.url.clone(), title: t.title.clone(), pinned: t.pinned }).collect();
        self.state.active = self.tabs.iter().filter(kept)
            .position(|t| Some(t.id) == self.active).unwrap_or(0);
        save("state.json", &self.state);
        save("history.json", &self.history);
        self.dirty = false;
        self.last_save = Instant::now();
    }
}

fn main() {
    let event_loop = EventLoopBuilder::<Ev>::with_user_event().build();
    let proxy = event_loop.create_proxy();
    let window = WindowBuilder::new()
        .with_title("Brisklark")
        .with_inner_size(LogicalSize::new(1280.0, 820.0))
        .with_min_inner_size(LogicalSize::new(480.0, 320.0))
        // No Windows title bar: the tabs take its place. Windows still draws the
        // shadow and the invisible borders to resize by.
        .with_decorations(false)
        .with_undecorated_shadow(true)
        .build(&event_loop)
        .expect("window");

    let state: State = load("state.json");
    let history: Vec<Visit> = load("history.json");
    let ipc = proxy.clone();
    let mut ctx = WebContext::new(Some(dir("LOCALAPPDATA").join("WebView2")));
    let chrome = WebViewBuilder::new_with_web_context(&mut ctx)
        .with_html(include_str!("chrome.html"))
        .with_default_context_menus(false)
        .with_ipc_handler(move |req| {
            let _ = ipc.send_event(Ev::Ipc(req.into_body()));
        })
        .with_bounds(rect(0.0, 0.0, SIDE, 800.0))
        .build_as_child(&window)
        .expect("WebView2 runtime");
    hook_keys(&chrome, proxy.clone());

    let mut app = App {
        window,
        env: chrome.environment(),
        shield: shield::load(),
        chrome,
        proxy,
        tabs: vec![],
        active: None,
        next: 1,
        closed: vec![],
        history,
        state,
        overlay: false,
        dirty: false,
        last_save: Instant::now(),
        quit: false,
    };
    for s in std::mem::take(&mut app.state.tabs) {
        let id = app.add_tab(s.url, false, None);
        let t = app.tabs.last_mut().unwrap();
        (t.title, t.pinned) = (s.title, s.pinned);
        let _ = id;
    }
    app.dirty = false;
    let lists = app.proxy.clone();
    shield::refresh(move |bytes| {
        let _ = lists.send_event(Ev::Shield(bytes));
    });
    if let Some(t) = app.tabs.get(app.state.active).or(app.tabs.first()) {
        app.select(t.id);
    }

    event_loop.run(move |event, _, flow| {
        *flow = if app.dirty { ControlFlow::WaitUntil(app.last_save + Duration::from_secs(30)) } else { ControlFlow::Wait };
        match event {
            Event::NewEvents(StartCause::ResumeTimeReached { .. }) if app.dirty => app.persist(),
            Event::UserEvent(ev) => {
                app.event(ev);
                if app.quit {
                    app.persist();
                    *flow = ControlFlow::Exit;
                }
            }
            Event::WindowEvent { event, .. } => match event {
                WindowEvent::CloseRequested => {
                    app.persist();
                    *flow = ControlFlow::Exit;
                }
                WindowEvent::Resized(_) | WindowEvent::ScaleFactorChanged { .. } => app.layout(),
                _ => {}
            },
            _ => {}
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn addresses() {
        let e = "https://s/?q=%s";
        assert_eq!(resolve("example.com", e), "https://example.com");
        assert_eq!(resolve("localhost:3000", e), "http://localhost:3000");
        assert_eq!(resolve("https://a.b/c", e), "https://a.b/c");
        assert_eq!(resolve("rust lang", e), "https://s/?q=rust+lang");
        assert_eq!(resolve("cats", e), "https://s/?q=cats");
        assert_eq!(resolve("C:\\x\\y.html", e), "file:///C:/x/y.html");
        assert_eq!(resolve("a&b?", e), "https://s/?q=a%26b%3F");
    }

    #[test]
    fn hosts() {
        assert_eq!(host("https://u@www.a.com:8/x?y"), "www.a.com");
    }

    #[test]
    fn keys() {
        assert_eq!(shortcut('L' as u32, true, false, false), Some(Cmd::Field));
        assert_eq!(shortcut('T' as u32, true, true, false), Some(Cmd::Reopen));
        assert_eq!(shortcut('9' as u32, true, false, false), Some(Cmd::Nth(usize::MAX)));
        assert_eq!(shortcut(0x09, true, true, false), Some(Cmd::PrevTab));
        assert_eq!(shortcut('L' as u32, false, false, false), None);
        assert_eq!(shortcut('F' as u32, true, false, false), None); // WebView2's own find
    }
}
