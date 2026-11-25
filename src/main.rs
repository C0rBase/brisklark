//! Brisklark: one window, a row of tabs, one field, and the page.
//! The page engine is WebView2, the Edge runtime already on every Windows 11
//! machine, so this process only draws the frame and keeps the lists.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod dialog;
mod ext;
mod import;
mod menu;
mod sha;
mod shield;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::{HashMap, HashSet},
    fs,
    path::PathBuf,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tao::{
    dpi::LogicalSize,
    event::{ElementState, Event, MouseButton, StartCause, WindowEvent},
    event_loop::{ControlFlow, EventLoopBuilder, EventLoopProxy},
    platform::windows::{IconExtWindows, WindowBuilderExtWindows, WindowExtWindows},
    window::{CursorIcon, Fullscreen, Icon, ResizeDirection, Theme, Window, WindowBuilder},
};
use menu::M;
use webview2_com::{
    take_pwstr, AcceleratorKeyPressedEventHandler, ClearBrowsingDataCompletedHandler, ContainsFullScreenElementChangedEventHandler,
    FaviconChangedEventHandler, FocusChangedEventHandler, ShowSaveAsUICompletedHandler,
    IsDocumentPlayingAudioChangedEventHandler, IsMutedChangedEventHandler, Microsoft::Web::WebView2::Win32::*,
};
use windows::{
    core::{Interface, BOOL, HSTRING, PWSTR},
    Win32::UI::Input::KeyboardAndMouse::{GetKeyState, VK_CONTROL, VK_MENU, VK_SHIFT},
};
use wry::{
    dpi::{LogicalPosition, LogicalSize as WLogicalSize},
    MemoryUsageLevel, NewWindowResponse, PageLoadEvent, Rect, WebContext, WebView, WebViewBuilder,
    WebViewBuilderExtWindows, WebViewExtWindows,
};

const BAR: f64 = 72.0; // tabs across the top: the tab row and the toolbar under it
const HISTORY_CAP: usize = 5000;
const BLANK: &str = "about:blank"; // a new tab nobody has typed into yet
const GAP: f64 = 6.0; // the divider between the two pages of a split view

// ---------- what is kept on disk ----------

#[derive(Serialize, Deserialize, Clone, Default)]
struct Saved {
    url: String,
    title: String,
    #[serde(default)]
    pinned: bool,
    /// Two saved tabs with the same nonzero number are a split view.
    #[serde(default)]
    pair: u32,
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
    folded: bool,
    engine: String,
    startup: String, // "continue" or "new"
    shields: bool,
    cookie_notices: bool,
    shields_off: Vec<String>,
    shield: shield::Prefs,
    split: f64, // share of the width the left page of a split view gets
    passwords: bool,  // offer to save passwords
    autofill: bool,   // fill in addresses and forms
    downloads: String, // download folder; empty: Windows' Downloads
    zoom: f64,        // default page zoom
    gpu: bool,        // hardware acceleration
    language: String, // websites' language; empty: Windows'
    ext_paths: HashMap<String, String>, // extension id → its folder
}

impl Default for State {
    fn default() -> Self {
        State {
            tabs: vec![],
            active: 0,
            bookmarks: vec![],
            folded: false,
            engine: "https://www.google.com/search?q=%s".into(),
            startup: "continue".into(),
            shields: true,
            cookie_notices: true,
            shields_off: vec![],
            shield: shield::Prefs::default(),
            split: 0.5,
            passwords: true,
            autofill: true,
            downloads: String::new(),
            zoom: 1.0,
            gpu: true,
            language: String::new(),
            ext_paths: HashMap::new(),
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
    Fold,     // Ctrl+S
    Bookmark, // Ctrl+Shift+B
    Duplicate, // Ctrl+D
    CopyUrl,  // Ctrl+Shift+C
    PasteGo,  // Ctrl+Shift+V
    History,  // Ctrl+Y
    Bookmarks, // Ctrl+Shift+O
    Settings, // Ctrl+,
    Downloads, // Ctrl+J
    Full,     // F11
    Menu,     // Alt+F
}

fn shortcut(vk: u32, ctrl: bool, shift: bool, alt: bool) -> Option<Cmd> {
    use Cmd::*;
    if vk == 0x7A && !ctrl && !alt && !shift {
        return Some(Full); // F11
    }
    if alt && !ctrl && !shift && vk == 'F' as u32 {
        return Some(Menu);
    }
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
        ('S', false) => Fold,
        ('B', true) => Bookmark,
        ('D', false) => Duplicate,
        ('C', true) => CopyUrl,
        ('V', true) => PasteGo,
        ('Y', false) => History,
        ('O', true) => Bookmarks,
        ('J', false) => Downloads,
        ('1'..='8', false) => Nth(key as usize - '1' as usize),
        ('9', false) => Nth(usize::MAX),
        _ => match (vk, shift) {
            (0xBC, false) => Settings, // ,
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
    Shield(Option<shield::Compiled>), // fresh lists (None: download failed)
    Audio(u32, bool, bool),                 // playing, muted
    Icon(u32, String),                      // the page's favicon address
    Focused(u32),
    Ubo(String), // uBO's scriptlets, read by the hidden page
    PageFull(bool), // a page went full screen (a video's own button) or left it
    Exts(Vec<ext::Ext>),                    // WebView2's list of extensions
    ExtFetched(Result<PathBuf, String>),    // a Web Store download, unpacked
    ExtAdded(Result<ext::Ext, String>, Option<PathBuf>),
}

struct Tab {
    id: u32,
    url: String,
    title: String,
    pinned: bool,
    private: bool,
    view: Option<WebView>, // None until first shown: restored tabs cost nothing
    page: shield::Page,    // what the blocker knows about the page
    audio: bool,
    muted: bool,
    partner: Option<u32>, // the other half of its split view
    loading: bool,
    icon: String, // favicon address, as the page declares it
}

/// Sound and focus, per tab: the speaker icon, and which half of a split view has the keys.
fn hook_tab(view: &WebView, id: u32, proxy: EventLoopProxy<Ev>) {
    let focus = proxy.clone();
    let icon = proxy.clone();
    // A video's own full-screen button: the window goes full screen with it.
    let full = proxy.clone();
    unsafe {
        let mut t = 0i64;
        let _ = view.webview().add_ContainsFullScreenElementChanged(
            &ContainsFullScreenElementChangedEventHandler::create(Box::new(move |w, _| {
                if let Some(w) = w {
                    let mut on = BOOL(0);
                    w.ContainsFullScreenElement(&mut on)?;
                    let _ = full.send_event(Ev::PageFull(on.as_bool()));
                }
                Ok(())
            })),
            &mut t,
        );
    }
    let send = move |core: Option<ICoreWebView2>| -> windows::core::Result<()> {
        if let Some(c) = core.and_then(|c| c.cast::<ICoreWebView2_8>().ok()) {
            let (mut playing, mut muted) = (BOOL(0), BOOL(0));
            unsafe {
                c.IsDocumentPlayingAudio(&mut playing)?;
                c.IsMuted(&mut muted)?;
            }
            let _ = proxy.send_event(Ev::Audio(id, playing.as_bool(), muted.as_bool()));
        }
        Ok(())
    };
    let mut token = 0i64;
    unsafe {
        if let Ok(c) = view.webview().cast::<ICoreWebView2_8>() {
            let s = send.clone();
            let _ = c.add_IsDocumentPlayingAudioChanged(
                &IsDocumentPlayingAudioChangedEventHandler::create(Box::new(move |w, _| s(w))),
                &mut token,
            );
            let _ = c.add_IsMutedChanged(&IsMutedChangedEventHandler::create(Box::new(move |w, _| send(w))), &mut token);
        }
        if let Ok(c) = view.webview().cast::<ICoreWebView2_15>() {
            let icon = icon.clone();
            let _ = c.add_FaviconChanged(
                &FaviconChangedEventHandler::create(Box::new(move |w, _| {
                    if let Some(w) = w.and_then(|w| w.cast::<ICoreWebView2_15>().ok()) {
                        let mut uri = PWSTR::null();
                        w.FaviconUri(&mut uri)?;
                        let _ = icon.send_event(Ev::Icon(id, take_pwstr(uri)));
                    }
                    Ok(())
                })),
                &mut token,
            );
        }
        let _ = view.controller().add_GotFocus(
            &FocusChangedEventHandler::create(Box::new(move |_, _| {
                let _ = focus.send_event(Ev::Focused(id));
                Ok(())
            })),
            &mut token,
        );
    }
}

const ENGINES: &[(&str, &str)] = &[
    ("Google", "https://www.google.com/search?q=%s"),
    ("DuckDuckGo", "https://duckduckgo.com/?q=%s"),
    ("Bing", "https://www.bing.com/search?q=%s"),
    ("Startpage", "https://www.startpage.com/do/search?q=%s"),
    ("Ecosia", "https://www.ecosia.org/search?q=%s"),
    ("Kagi", "https://kagi.com/search?q=%s"),
    ("Qwant", "https://www.qwant.com/?q=%s"),
];

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
    shown: Vec<u32>,       // tabs whose page is on screen (two in a split view)
    divider: Option<f64>, // left edge of a split view's divider, logical pixels
    pointer: f64,         // where the mouse last was over the window's own area
    dragging: bool,
    updating: bool, // Settings > Update now is running
    ubo: Option<WebView>, // the hidden page reading uBO's scriptlets, while it works
    full: bool,           // full screen: the page has the whole screen
    exts: Vec<ext::Ext>,  // installed extensions, as WebView2 last listed them
    installing: bool,
    relaunch: bool,
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

    /// Where pages go: x, y, width, height in logical pixels.
    fn page_area(&self) -> (f64, f64, f64, f64) {
        let s = self.window.inner_size().to_logical::<f64>(self.window.scale_factor());
        if self.state.folded || self.full {
            (0.0, 0.0, s.width, s.height)
        } else {
            (0.0, BAR, s.width, s.height - BAR)
        }
    }
    fn page_rect(&self) -> Rect {
        let (x, y, w, h) = self.page_area();
        rect(x, y, w, h)
    }

    fn layout(&mut self) {
        let s = self.window.inner_size().to_logical::<f64>(self.window.scale_factor());
        let chrome = if self.overlay {
            rect(0.0, 0.0, s.width, s.height)
        } else if self.state.folded || self.full {
            rect(0.0, 0.0, 0.0, 0.0)
        } else {
            rect(0.0, 0.0, s.width, BAR)
        };
        let _ = self.chrome.set_bounds(chrome);
        let _ = self.chrome.set_visible(self.overlay || !(self.state.folded || self.full));
        if self.overlay {
            // The field's list hangs over the page: the bar's own window has to
            // sit above the tab's, which was created after it.
            unsafe {
                use windows::Win32::UI::WindowsAndMessaging::{SetWindowPos, HWND_TOP, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE};
                let _ = SetWindowPos(self.chrome.hwnd(), Some(HWND_TOP), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
            }
        }

        // The page, or the two pages of a split view side by side in tab order.
        let (x, y, w, h) = self.page_area();
        let mut want: Vec<(u32, Rect)> = vec![];
        self.divider = None;
        if let Some(i) = self.active.and_then(|id| self.idx(id)) {
            let j = self.tabs[i].partner.and_then(|p| self.idx(p)).filter(|&j| self.tabs[j].view.is_some());
            match j {
                Some(j) => {
                    let (l, r) = if i < j { (i, j) } else { (j, i) };
                    let lw = ((w - GAP) * self.state.split).round();
                    want.push((self.tabs[l].id, rect(x, y, lw, h)));
                    want.push((self.tabs[r].id, rect(x + lw + GAP, y, w - lw - GAP, h)));
                    self.divider = Some(x + lw);
                }
                None => want.push((self.tabs[i].id, rect(x, y, w, h))),
            }
        }
        for id in std::mem::take(&mut self.shown) {
            if !want.iter().any(|w| w.0 == id) {
                if let Some(v) = self.idx(id).and_then(|i| self.tabs[i].view.as_ref()) {
                    let _ = v.set_visible(false);
                    let _ = v.set_memory_usage_level(MemoryUsageLevel::Low);
                }
            }
        }
        for (id, r) in want {
            if let Some(v) = self.idx(id).and_then(|i| self.tabs[i].view.as_ref()) {
                let _ = v.set_bounds(r);
                let _ = v.set_visible(true);
                let _ = v.set_memory_usage_level(MemoryUsageLevel::Normal);
                self.shown.push(id);
            }
        }
    }

    fn make_view(&mut self, i: usize) {
        let (id, private, url, page) = {
            let t = &self.tabs[i];
            (t.id, t.private, t.url.clone(), t.page.clone())
        };
        *page.url.borrow_mut() = url.clone();
        let (p1, p2, p3) = (self.proxy.clone(), self.proxy.clone(), self.proxy.clone());
        let (shield, opener) = (self.shield.clone(), page.url.clone());
        // Every tab shares the sidebar's environment: one Edge browser process
        // for the whole app. Private tabs are an InPrivate controller on it.
        let built = WebViewBuilder::new()
            .with_environment(self.env.clone())
            .with_incognito(private)
            .with_bounds(self.page_rect())
            .with_visible(false)
            .with_browser_accelerator_keys(true) // keeps Ctrl+F, Ctrl+P, F5, F12, Alt+arrows
            .with_devtools(true)
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
                // Pop-unders: a new window to an address the lists block never opens.
                if shield.borrow().popup_blocked(&url, &opener.borrow()) {
                    return NewWindowResponse::Deny;
                }
                if features.size.is_some() {
                    return NewWindowResponse::Allow;
                }
                let _ = p3.send_event(Ev::Open(url, private));
                NewWindowResponse::Deny
            })
            .build_as_child(&self.window);
        if let Ok(view) = built {
            hook_keys(&view, self.proxy.clone());
            hook_tab(&view, id, self.proxy.clone());
            shield::attach(&view, self.shield.clone(), page);
            self.apply_profile(&view);
            if (self.state.zoom - 1.0).abs() > 0.001 {
                let _ = unsafe { view.controller().SetZoomFactor(self.state.zoom) };
            }
            // Only now the first page: Shields must be in place before it starts.
            let _ = view.load_url(&url);
            self.tabs[i].view = Some(view);
        }
    }

    fn select(&mut self, id: u32) {
        let Some(i) = self.idx(id) else { return };
        self.active = Some(id);
        if self.tabs[i].url == BLANK {
            // A new tab has no page until an address is typed: just the field.
            self.render();
            return self.focus_field("here");
        }
        // Both halves of a split view come up together.
        for j in [Some(i), self.tabs[i].partner.and_then(|p| self.idx(p))].into_iter().flatten() {
            if self.tabs[j].view.is_none() && self.tabs[j].url != BLANK {
                self.make_view(j);
            }
        }
        self.overlay = false;
        self.layout();
        if let Some(v) = &self.tabs[i].view {
            let _ = v.focus();
        }
        self.render();
    }

    fn add_tab(&mut self, url: String, private: bool, at: Option<usize>) -> u32 {
        let id = self.next;
        self.next += 1;
        let title = if url == BLANK { "New Tab".into() } else { url.clone() };
        let tab = Tab {
            id, title, url, pinned: false, private, view: None,
            page: Default::default(), audio: false, muted: false, partner: None, loading: false, icon: String::new(),
        };
        let pins = self.tabs.iter().filter(|t| t.pinned).count();
        let at = at.unwrap_or(self.tabs.len()).clamp(pins, self.tabs.len());
        self.tabs.insert(at, tab);
        self.dirty = true;
        id
    }

    fn close(&mut self, id: u32) {
        let Some(i) = self.idx(id) else { return };
        let tab = self.tabs.remove(i); // dropping the view frees its page
        self.shown.retain(|&s| s != id);
        if let Some(p) = tab.partner.and_then(|p| self.idx(p)) {
            self.tabs[p].partner = None;
        }
        if !tab.private && tab.url != BLANK {
            self.closed.push(Saved { url: tab.url, title: tab.title, ..Default::default() });
        }
        if self.active == Some(id) {
            self.active = None;
            match self.tabs.get(i).or_else(|| self.tabs.last()).map(|t| t.id) {
                Some(next) => self.select(next),
                None => self.focus_field("new"),
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
                            "private": t.private, "active": Some(t.id) == self.active,
                            "shown": self.shown.contains(&t.id), "split": t.partner.is_some(),
                            "audio": t.audio, "muted": t.muted, "icon": t.icon}))
            .collect();
        let url = self.current().map(|t| t.url.as_str()).unwrap_or("");
        let marked = self.state.bookmarks.iter().any(|b| b.url == url);
        let shields = url.starts_with("http") && self.shield.borrow().up_for(url);
        let blocked = self.current().map_or(0, |t| t.page.blocked.get());
        let (back, forward) = self.current().and_then(|t| t.view.as_ref()).map_or((false, false), |v| unsafe {
            let (mut b, mut f) = (BOOL(0), BOOL(0));
            let core = v.webview();
            let _ = core.CanGoBack(&mut b);
            let _ = core.CanGoForward(&mut f);
            (b.as_bool(), f.as_bool())
        });
        let live = self.current().is_some_and(|t| t.view.is_some());
        let loading = self.current().is_some_and(|t| t.loading);
        self.eval("render", json!({"tabs": tabs, "folded": self.state.folded, "overlay": self.overlay,
                                   "bookmarked": marked, "shields": shields, "blocked": blocked,
                                   "back": back, "forward": forward, "live": live, "loading": loading,
                                   "store": self.current().and_then(|t| ext::store_id(&t.url)).is_some(), "installing": self.installing}));
        let title = self.current().map_or("Brisklark".into(), |t| t.title.clone());
        self.window.set_title(&title);
    }

    fn focus_field(&mut self, mode: &str) {
        // The tabs are across the top, so the field always floats over the page.
        self.overlay = true;
        self.layout();
        let _ = self.chrome.focus();
        let url = if mode == "here" { self.current().map_or("", |t| t.url.as_str()) } else { "" };
        let url = if url == BLANK { "" } else { url };
        self.eval("focusField", json!({"mode": mode, "overlay": self.overlay, "text": url}));
    }

    fn done(&mut self, cancel: bool) {
        // Esc on a tab nobody has typed into takes it away again. Any other way
        // out of the field (a click, or opening another tab) keeps it, as in
        // Chrome, so several new tabs can be open at once.
        if cancel {
            if let Some(t) = self.current().filter(|t| t.url == BLANK) {
                return if self.tabs.len() > 1 { self.close(t.id) } else { self.focus_field("here") };
            }
        }
        if self.tabs.is_empty() {
            return self.focus_field("new");
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
        // Tabs answer a query, or the tab switcher; never the tab you are on.
        if tabs_only || !q.is_empty() {
            for t in self.tabs.iter().filter(|t| Some(t.id) != self.active && hit(&t.title, &t.url))
                .take(if tabs_only { 20 } else { 3 }) {
                out.push(json!({"kind": "tab", "id": t.id, "title": t.title, "url": t.url}));
            }
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
            Cmd::Field => self.focus_field("here"),
            Cmd::Switch => self.focus_field("tabs"),
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
                    let _ = unsafe { v.webview().GoBack() };
                }
            }
            Cmd::Forward => {
                if let Some(v) = view(self) {
                    let _ = unsafe { v.webview().GoForward() };
                }
            }
            Cmd::Downloads => self.downloads(),
            Cmd::Full => self.set_full(!self.full),
            Cmd::Menu => self.main_menu(),
            Cmd::PrevTab | Cmd::NextTab if !self.tabs.is_empty() => {
                let n = self.tabs.len();
                let i = cur.map_or(0, |i| if cmd == Cmd::NextTab { (i + 1) % n } else { (i + n - 1) % n });
                self.select(self.tabs[i].id);
            }
            Cmd::Nth(k) if !self.tabs.is_empty() => {
                let i = k.min(self.tabs.len() - 1);
                self.select(self.tabs[i].id);
            }
            Cmd::Fold => {
                self.state.folded = !self.state.folded;
                self.dirty = true;
                self.layout();
            }
            Cmd::Bookmark => {
                if let Some(id) = self.active {
                    self.bookmark(id)
                }
            }
            Cmd::Duplicate => {
                if let Some(i) = cur {
                    self.duplicate(i)
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
            Cmd::Settings => {
                self.overlay = true;
                self.layout();
                let _ = self.chrome.focus();
                self.settings(true);
            }
            _ => {}
        }
    }

    fn duplicate(&mut self, i: usize) {
        let (url, private) = (self.tabs[i].url.clone(), self.tabs[i].private);
        if url != BLANK {
            let id = self.add_tab(url, private, Some(i + 1));
            self.select(id);
        }
    }

    fn pin(&mut self, id: u32) {
        if let Some(i) = self.idx(id).filter(|&i| !self.tabs[i].private && self.tabs[i].url != BLANK) {
            let mut t = self.tabs.remove(i);
            t.pinned = !t.pinned;
            let pins = self.tabs.iter().filter(|t| t.pinned).count();
            self.tabs.insert(pins, t);
            self.dirty = true;
            self.layout(); // the halves of a split view may have swapped sides
            self.render();
        }
    }

    fn bookmark(&mut self, id: u32) {
        let Some(t) = self.idx(id).map(|i| &self.tabs[i]).filter(|t| !t.private && t.url != BLANK) else { return };
        let (url, title) = (t.url.clone(), t.title.clone());
        match self.state.bookmarks.iter().position(|b| b.url == url) {
            Some(i) => drop(self.state.bookmarks.remove(i)),
            None => self.state.bookmarks.push(Saved { url, title, ..Default::default() }),
        }
        self.dirty = true;
        self.render();
    }

    /// The speaker icon follows from WebView2's IsMutedChanged.
    fn mute(&self, id: u32) {
        let Some(t) = self.idx(id).map(|i| &self.tabs[i]) else { return };
        if let Some(c) = t.view.as_ref().and_then(|v| v.webview().cast::<ICoreWebView2_8>().ok()) {
            unsafe {
                let _ = c.SetIsMuted(!t.muted);
            }
        }
    }

    /// Two tabs side by side; each leaves any split view it was in.
    fn pair(&mut self, a: u32, b: u32) {
        self.unpair(a);
        self.unpair(b);
        let (Some(i), Some(j)) = (self.idx(a), self.idx(b)) else { return };
        self.tabs[i].partner = Some(b);
        self.tabs[j].partner = Some(a);
        // Keep the two together in the tab row, the new one to the right.
        if !self.tabs[i].pinned && !self.tabs[j].pinned {
            let t = self.tabs.remove(j);
            let i = self.idx(a).unwrap();
            self.tabs.insert(i + 1, t);
        }
        self.dirty = true;
        self.select(a);
    }

    fn unpair(&mut self, id: u32) {
        if let Some(i) = self.idx(id) {
            if let Some(p) = self.tabs[i].partner.take().and_then(|p| self.idx(p)) {
                self.tabs[p].partner = None;
            }
            self.dirty = true;
        }
    }

    fn close_all(&mut self, ids: Vec<u32>, keep: u32) {
        if self.active.is_some_and(|a| ids.contains(&a)) {
            self.select(keep);
        }
        for id in ids {
            self.close(id);
        }
    }

    fn popup(&self, items: &[M]) -> u32 {
        menu::popup(self.window.hwnd(), self.window.theme() == Theme::Dark, items)
    }

    fn tab_menu(&mut self, id: u32) {
        let Some(i) = self.idx(id) else { return };
        let t = &self.tabs[i];
        let (url, pinned, private, muted, partner) = (t.url.clone(), t.pinned, t.private, t.muted, t.partner);
        let (blank, live) = (url == BLANK, t.view.is_some());
        let others: Vec<(u32, String)> = if blank {
            vec![]
        } else {
            self.tabs.iter()
                .filter(|o| o.id != id && o.url != BLANK && o.private == private && partner != Some(o.id))
                .map(|o| (o.id, o.title.clone()))
                .collect()
        };
        let marked = self.state.bookmarks.iter().any(|b| b.url == url);
        let after: Vec<u32> = self.tabs[i + 1..].iter().filter(|o| !o.pinned).map(|o| o.id).collect();
        let rest: Vec<u32> = self.tabs.iter().filter(|o| o.id != id && !o.pinned).map(|o| o.id).collect();
        let items = [
            M::Item(1, "New tab to the right".into(), true),
            M::Sep,
            M::Item(2, "Reload".into(), live),
            M::Item(3, "Duplicate".into(), !blank),
            M::Item(4, if pinned { "Unpin" } else { "Pin" }.into(), !private && !blank),
            M::Item(5, if muted { "Unmute site" } else { "Mute site" }.into(), live),
            M::Sep,
            M::Sub("Split view with".into(), others.iter().enumerate().map(|(k, o)| M::Item(100 + k as u32, o.1.clone(), true)).collect()),
            M::Item(6, "Separate split view".into(), partner.is_some()),
            M::Sep,
            M::Item(7, if marked { "Remove bookmark" } else { "Bookmark" }.into(), !private && !blank),
            M::Item(8, "Copy link".into(), !blank),
            M::Sep,
            M::Item(9, "Close".into(), true),
            M::Item(10, "Close other tabs".into(), !rest.is_empty()),
            M::Item(11, "Close tabs to the right".into(), !after.is_empty()),
        ];
        match self.popup(&items) {
            1 => {
                let new = self.add_tab(BLANK.into(), private, Some(i + 1));
                self.select(new);
            }
            2 => {
                if let Some(v) = &self.tabs[i].view {
                    let _ = v.reload();
                }
            }
            3 => self.duplicate(i),
            4 => self.pin(id),
            5 => self.mute(id),
            6 => {
                self.unpair(id);
                self.layout();
                self.render();
            }
            7 => self.bookmark(id),
            8 => {
                if let Ok(mut c) = arboard::Clipboard::new() {
                    let _ = c.set_text(url);
                }
            }
            9 => self.close(id),
            10 => self.close_all(rest, id),
            11 => self.close_all(after, id),
            k if k >= 100 => {
                if let Some(&(other, _)) = others.get(k as usize - 100) {
                    self.pair(id, other);
                }
            }
            _ => {}
        }
    }

    /// Passwords, autofill and the download folder, on a page's profile.
    fn apply_profile(&self, view: &WebView) {
        unsafe {
            let Ok(p) = view.webview().cast::<ICoreWebView2_13>().and_then(|w| w.Profile()) else { return };
            if let Ok(p6) = p.cast::<ICoreWebView2Profile6>() {
                let _ = p6.SetIsPasswordAutosaveEnabled(self.state.passwords);
                let _ = p6.SetIsGeneralAutofillEnabled(self.state.autofill);
            }
            if !self.state.downloads.is_empty() {
                let _ = p.SetDefaultDownloadFolderPath(&HSTRING::from(self.state.downloads.as_str()));
            }
        }
    }

    fn apply_profiles(&self) {
        self.apply_profile(&self.chrome);
        for t in &self.tabs {
            if let Some(v) = &t.view {
                self.apply_profile(v);
            }
        }
    }

    fn download_folder(&self) -> String {
        if !self.state.downloads.is_empty() {
            return self.state.downloads.clone();
        }
        unsafe {
            let mut p = PWSTR::null();
            self.chrome.webview().cast::<ICoreWebView2_13>().and_then(|w| w.Profile())
                .and_then(|pr| pr.DefaultDownloadFolderPath(&mut p))
                .map(|_| webview2_com::take_pwstr(p))
                .unwrap_or_default()
        }
    }

    fn ext_profile(&self) -> Option<ICoreWebView2Profile7> {
        ext::profile(&self.chrome.webview())
    }

    fn list_exts(&self) {
        let Some(p) = self.ext_profile() else { return };
        let proxy = self.proxy.clone();
        ext::list(&p, move |l| drop(proxy.send_event(Ev::Exts(l))));
    }

    /// From a Web Store link (or a bare id): downloaded on a thread, then added.
    fn install(&mut self, text: &str) {
        let text = text.trim();
        let id = ext::store_id(text).or_else(|| {
            (text.len() == 32 && text.bytes().all(|b| (b'a'..=b'p').contains(&b))).then(|| text.to_string())
        });
        let Some(id) = id else {
            self.message("Extensions", "That isn't a Chrome Web Store link. Open the extension's page on chromewebstore.google.com and copy its address.");
            return;
        };
        if self.installing {
            return;
        }
        self.installing = true;
        self.settings(false);
        let proxy = self.proxy.clone();
        std::thread::spawn(move || {
            let _ = proxy.send_event(Ev::ExtFetched(ext::fetch(&id)));
        });
    }

    fn install_current(&mut self) {
        if let Some(url) = self.current().map(|t| t.url.clone()) {
            self.install(&url);
        }
    }

    fn add_ext(&mut self, path: PathBuf) {
        let Some(p) = self.ext_profile() else {
            self.installing = false;
            self.message("Extensions", "This version of the WebView2 runtime can't run extensions. Update Microsoft Edge WebView2 and try again.");
            return;
        };
        self.installing = true;
        let proxy = self.proxy.clone();
        let keep = path.clone();
        ext::add(&p, &path, move |r| drop(proxy.send_event(Ev::ExtAdded(r, Some(keep)))));
    }

    fn ext_act(&self, id: &str, what: ext::Act) {
        let Some(p) = self.ext_profile() else { return };
        let proxy = self.proxy.clone();
        let p2 = p.clone();
        ext::act(&p, id.to_string(), what, move || {
            ext::list(&p2, move |l| drop(proxy.send_event(Ev::Exts(l))));
        });
    }

    fn ext_folder(&self, id: &str) -> Option<PathBuf> {
        self.state.ext_paths.get(id).map(PathBuf::from).or_else(|| Some(ext::dir().join(id))).filter(|p| p.join("manifest.json").exists())
    }

    /// An extension's popup (what its toolbar button would show) or options page, in a tab.
    fn ext_page(&mut self, id: &str, options: bool) {
        let (popup, opts) = self.ext_folder(id).map(|f| ext::pages(&f)).unwrap_or((None, None));
        let page = if options { opts.or(popup) } else { popup.or(opts) };
        match page {
            Some(page) => {
                let t = self.add_tab(format!("chrome-extension://{id}/{page}"), false, None);
                self.select(t);
            }
            None => self.message("Extensions", "This extension has no page of its own: it works on websites by itself."),
        }
    }

    /// The puzzle button: each extension's page, and the way to manage them.
    fn ext_menu(&mut self) {
        let mut items: Vec<M> = self.exts.iter().enumerate().filter(|(_, x)| x.enabled)
            .map(|(i, x)| M::Item(300 + i as u32, x.name.clone(), true)).collect();
        if items.is_empty() {
            items.push(M::Item(0, "No extensions yet".into(), false));
        }
        items.push(M::Sep);
        if self.current().and_then(|t| ext::store_id(&t.url)).is_some() {
            items.push(M::Item(50, "Add this extension to Brisklark".into(), !self.installing));
        }
        items.push(M::Item(51, "Manage extensions".into(), true));
        let k = self.popup(&items);
        self.menu_action(k);
    }

    fn open_settings_at(&mut self, section: &str) {
        self.command(Cmd::Settings);
        let _ = self.chrome.evaluate_script(&format!("typeof settingsGo==='function'&&settingsGo({})", json!(section)));
    }

    /// Settings back to how they came, keeping tabs, bookmarks, history and extensions.
    fn reset(&mut self) {
        let d = State::default();
        let s = &mut self.state;
        (s.folded, s.engine, s.startup) = (d.folded, d.engine, d.startup);
        (s.shields, s.cookie_notices, s.shields_off, s.shield) = (d.shields, d.cookie_notices, d.shields_off, d.shield);
        (s.passwords, s.autofill, s.downloads, s.zoom, s.gpu, s.language) = (d.passwords, d.autofill, d.downloads, d.zoom, d.gpu, d.language);
        {
            let mut sh = self.shield.borrow_mut();
            sh.on = self.state.shields;
            sh.off.clear();
            sh.prefs = self.state.shield.clone();
        }
        if !self.shield.borrow_mut().reload(&lists(&self.state)) {
            self.refresh_lists(false);
        }
        self.apply_profiles();
        self.layout();
    }

    /// Full screen: F11, the menu, or a page's own button (a video player).
    fn set_full(&mut self, on: bool) {
        if self.full == on {
            return;
        }
        self.full = on;
        self.window.set_fullscreen(on.then(|| Fullscreen::Borderless(None)));
        self.layout();
    }

    fn downloads(&self) {
        // The list hangs off a page; with no page open, off the frame.
        let v = self.current().and_then(|t| t.view.as_ref()).unwrap_or(&self.chrome);
        if let Ok(c) = v.webview().cast::<ICoreWebView2_9>() {
            let _ = unsafe { c.OpenDefaultDownloadDialog() };
        }
    }

    fn zoom(&self) -> f64 {
        let mut z = 1.0;
        if let Some(v) = self.current().and_then(|t| t.view.as_ref()) {
            let _ = unsafe { v.controller().ZoomFactor(&mut z) };
        }
        z
    }

    /// Chrome's zoom steps.
    fn set_zoom(&self, step: i32) {
        const STEPS: [f64; 15] = [0.25, 0.33, 0.5, 0.67, 0.75, 0.8, 0.9, 1.0, 1.1, 1.25, 1.5, 1.75, 2.0, 2.5, 3.0];
        let Some(v) = self.current().and_then(|t| t.view.as_ref()) else { return };
        let z = self.zoom();
        let to = match step {
            0 => 1.0,
            s if s > 0 => STEPS.iter().copied().find(|&x| x > z + 0.001).unwrap_or(3.0),
            _ => STEPS.iter().rev().copied().find(|&x| x < z - 0.001).unwrap_or(0.25),
        };
        let _ = unsafe { v.controller().SetZoomFactor(to) };
    }

    fn message(&self, title: &str, text: &str) {
        use windows::{core::HSTRING, Win32::{Foundation::HWND, UI::WindowsAndMessaging::{MessageBoxW, MB_ICONINFORMATION, MB_OK}}};
        unsafe {
            MessageBoxW(Some(HWND(self.window.hwnd() as _)), &HSTRING::from(text), &HSTRING::from(title), MB_OK | MB_ICONINFORMATION);
        }
    }

    /// Bookmarks and history from another browser, added to ours.
    fn take_in(&mut self, found: import::Found, from: &str) {
        let mut added = 0;
        for (url, title) in found.bookmarks {
            if !self.state.bookmarks.iter().any(|b| b.url == url) {
                self.state.bookmarks.push(Saved { url, title, ..Default::default() });
                added += 1;
            }
        }
        let visits = found.history.len();
        for (url, title, n, last) in found.history {
            match self.history.iter_mut().find(|v| v.url == url) {
                Some(v) => {
                    v.visits = v.visits.max(n);
                    v.last = v.last.max(last);
                }
                None => self.history.push(Visit { url, title, visits: n, last }),
            }
        }
        if self.history.len() > HISTORY_CAP {
            self.history.sort_by(|a, b| b.last.cmp(&a.last));
            self.history.truncate(HISTORY_CAP);
        }
        self.dirty = true;
        self.render();
        let kept = visits.min(HISTORY_CAP);
        self.message("Import", &format!("From {from}: {added} new bookmarks and {kept} history entries."));
    }

    /// The ⋯ menu, as in Chrome, Edge and Brave.
    fn main_menu(&mut self) {
        let live = self.current().is_some_and(|t| t.view.is_some());
        let sources = import::sources();
        let pct = (self.zoom() * 100.0).round();
        let store = self.current().and_then(|t| ext::store_id(&t.url)).is_some();
        let mut imports: Vec<M> = sources.iter().enumerate().map(|(i, s)| M::Item(200 + i as u32, format!("From {}", s.label), true)).collect();
        if !imports.is_empty() {
            imports.push(M::Sep);
        }
        imports.extend([
            M::Item(40, "From a bookmarks file (.html)…".into(), true),
            M::Sep,
            M::Item(41, "Export bookmarks…".into(), !self.state.bookmarks.is_empty()),
            M::Item(42, "Export history…".into(), !self.history.is_empty()),
        ]);
        let items = [
            M::Item(1, "New tab\tCtrl+T".into(), true),
            M::Item(2, "New private tab\tCtrl+Shift+N".into(), true),
            M::Sep,
            M::Item(3, "History\tCtrl+Y".into(), true),
            M::Item(4, "Bookmarks\tCtrl+Shift+O".into(), true),
            M::Item(5, "Downloads\tCtrl+J".into(), true),
            M::Sub("Import and export".into(), imports),
            M::Sub("Extensions".into(), {
                let mut e: Vec<M> = self.exts.iter().enumerate().filter(|(_, x)| x.enabled)
                    .map(|(i, x)| M::Item(300 + i as u32, x.name.clone(), true)).collect();
                if !e.is_empty() {
                    e.push(M::Sep);
                }
                if store {
                    e.push(M::Item(50, "Add this extension to Brisklark".into(), true));
                }
                e.push(M::Item(51, "Manage extensions".into(), true));
                e
            }),
            M::Sep,
            M::Sub(format!("Zoom ({pct}%)"), vec![
                M::Item(30, "Zoom in\tCtrl++".into(), live),
                M::Item(31, "Zoom out\tCtrl+-".into(), live),
                M::Item(32, "Reset to 100%\tCtrl+0".into(), live && pct != 100.0),
            ]),
            M::Check(6, "Full screen\tF11".into(), self.full, true),
            M::Sep,
            M::Item(7, "Print…\tCtrl+P".into(), live),
            M::Item(8, "Find on page…\tCtrl+F".into(), live),
            M::Item(9, "Save page as…".into(), live),
            M::Sep,
            M::Item(12, "Developer tools\tF12".into(), live),
            M::Sep,
            M::Item(10, "Settings\tCtrl+,".into(), true),
            M::Item(11, "Exit".into(), true),
        ];
        let k = self.popup(&items);
        self.menu_action(k);
    }

    /// What the ⋯ menu's items (and the matching buttons in Settings) do.
    fn menu_action(&mut self, k: u32) {
        let sources = import::sources();
        let hwnd = self.window.hwnd();
        let view = self.current().and_then(|t| t.view.as_ref()).map(|v| v.webview());
        match k {
            1 => self.command(Cmd::NewTab),
            2 => self.command(Cmd::Private),
            3 => self.command(Cmd::History),
            4 => self.command(Cmd::Bookmarks),
            5 => self.downloads(),
            6 => self.set_full(!self.full),
            7 => {
                if let Some(c) = view.and_then(|v| v.cast::<ICoreWebView2_16>().ok()) {
                    let _ = unsafe { c.ShowPrintUI(COREWEBVIEW2_PRINT_DIALOG_KIND_BROWSER) };
                }
            }
            8 => self.find(),
            9 => {
                if let Some(c) = view.and_then(|v| v.cast::<ICoreWebView2_25>().ok()) {
                    let _ = unsafe { c.ShowSaveAsUI(&ShowSaveAsUICompletedHandler::create(Box::new(|_, _| Ok(())))) };
                }
            }
            10 => self.command(Cmd::Settings),
            12 => {
                if let Some(v) = self.current().and_then(|t| t.view.as_ref()) {
                    v.open_devtools();
                }
            }
            13 => {}
            11 => self.quit = true,
            30 => self.set_zoom(1),
            31 => self.set_zoom(-1),
            32 => self.set_zoom(0),
            40 => {
                if let Some(p) = dialog::open(hwnd, "Import bookmarks", ("Bookmarks file", "*.html;*.htm")) {
                    let text = fs::read_to_string(&p).unwrap_or_default();
                    let bookmarks = import::read_html(&text);
                    if bookmarks.is_empty() {
                        self.message("Import", "No bookmarks found in that file.");
                    } else {
                        self.take_in(import::Found { bookmarks, history: vec![] }, "the file");
                    }
                }
            }
            41 => {
                if let Some(p) = dialog::save(hwnd, "Export bookmarks", "bookmarks.html", ("Bookmarks file", "*.html")) {
                    let list: Vec<(String, String)> = self.state.bookmarks.iter().map(|b| (b.url.clone(), b.title.clone())).collect();
                    let _ = fs::write(p, import::write_html(&list));
                }
            }
            42 => {
                if let Some(p) = dialog::save(hwnd, "Export history", "history.csv", ("Spreadsheet (CSV)", "*.csv")) {
                    let mut h: Vec<&Visit> = self.history.iter().collect();
                    h.sort_by(|a, b| b.last.cmp(&a.last));
                    let rows: Vec<(String, String, u32, u64)> = h.iter().map(|v| (v.url.clone(), v.title.clone(), v.visits, v.last)).collect();
                    let _ = fs::write(p, import::write_csv(&rows));
                }
            }
            50 => self.install_current(),
            51 => self.open_settings_at("extensions"),
            k if (300..400).contains(&k) => {
                if let Some(id) = self.exts.get(k as usize - 300).map(|e| e.id.clone()) {
                    self.ext_page(&id, false);
                }
            }
            k if k >= 200 => {
                if let Some(src) = sources.get(k as usize - 200) {
                    match import::read(src) {
                        Ok(found) => self.take_in(found, &src.label),
                        Err(e) => self.message("Import", &e),
                    }
                }
            }
            _ => {}
        }
    }

    /// WebView2's own find bar, the one Ctrl+F opens.
    fn find(&self) {
        use windows::Win32::UI::Input::KeyboardAndMouse::{keybd_event, KEYBD_EVENT_FLAGS, KEYEVENTF_KEYUP};
        let Some(v) = self.current().and_then(|t| t.view.as_ref()) else { return };
        let _ = v.focus();
        unsafe {
            keybd_event(VK_CONTROL.0 as u8, 0, KEYBD_EVENT_FLAGS(0), 0);
            keybd_event(b'F', 0, KEYBD_EVENT_FLAGS(0), 0);
            keybd_event(b'F', 0, KEYEVENTF_KEYUP, 0);
            keybd_event(VK_CONTROL.0 as u8, 0, KEYEVENTF_KEYUP, 0);
        }
    }

    /// The shield button: this site's switch and what was blocked.
    fn shield_menu(&mut self) {
        let Some(i) = self.active.and_then(|id| self.idx(id)) else { return };
        let url = self.tabs[i].url.clone();
        let site = host(&url).to_ascii_lowercase();
        let web = url.starts_with("http");
        let (on, up) = {
            let s = self.shield.borrow();
            (s.on, s.up_for(&url))
        };
        let n = self.tabs[i].page.blocked.get();
        let items = [
            M::Check(1, if web { format!("Shields up for {}", site.trim_start_matches("www.")) } else { "Shields work on web pages".into() }, web && up, web && on),
            M::Item(0, if on { format!("{n} ads and trackers blocked on this page") } else { "Shields are off in Settings".into() }, false),
            M::Sep,
            M::Item(2, "Shields settings…".into(), true),
        ];
        match self.popup(&items) {
            1 => {
                {
                    let mut s = self.shield.borrow_mut();
                    if !s.off.remove(&site) {
                        s.off.insert(site);
                    }
                }
                self.dirty = true;
                if let Some(v) = &self.tabs[i].view {
                    let _ = v.reload();
                }
                self.render();
            }
            2 => self.command(Cmd::Settings),
            _ => {}
        }
    }

    fn settings(&self, open: bool) {
        let s = self.shield.borrow();
        let mut off: Vec<&String> = s.off.iter().collect();
        off.sort();
        let updated = shield::updated()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_secs());
        let exts: Vec<Value> = self.exts.iter().map(|x| {
            let (popup, options) = self.ext_folder(&x.id).map(|f| ext::pages(&f)).unwrap_or((None, None));
            json!({"id": x.id, "name": x.name, "enabled": x.enabled, "popup": popup.is_some(), "options": options.is_some()})
        }).collect();
        let sources: Vec<String> = import::sources().into_iter().map(|s| s.label).collect();
        let o = json!({"folded": self.state.folded, "engine": self.state.engine,
            "engines": ENGINES, "startup": self.state.startup, "shields": s.on,
            "cookies": self.state.cookie_notices, "off": off, "updated": updated, "updating": self.updating,
            "version": env!("CARGO_PKG_VERSION"),
            "prefs": self.state.shield, "lists": lists(&self.state).options(), "sources": sources,
            "exts": exts, "installing": self.installing, "extsOk": self.ext_profile().is_some(),
            "passwords": self.state.passwords, "autofill": self.state.autofill,
            "downloads": self.download_folder(), "zoom": self.state.zoom, "gpu": self.state.gpu,
            "language": self.state.language, "bookmarks": self.state.bookmarks.len(), "history": self.history.len()});
        if open {
            self.eval("settings", o);
        } else {
            let _ = self.chrome.evaluate_script(&format!("panelKind==='settings'&&settings({o})"));
        }
    }

    fn set(&mut self, key: &str, v: &Value) {
        let b = v.as_bool().unwrap_or(false);
        match key {
            "folded" => {
                self.state.folded = b;
                self.layout();
            }
            "engine" => {
                if let Some(e) = v.as_str().filter(|e| e.starts_with("https://") && e.contains("%s")) {
                    self.state.engine = e.into();
                }
            }
            "startup" => self.state.startup = if v == "new" { "new" } else { "continue" }.into(),
            "shields" => {
                self.state.shields = b;
                self.shield.borrow_mut().on = b;
            }
            "cookies" => {
                self.state.cookie_notices = b;
                if !self.shield.borrow_mut().reload(&lists(&self.state)) {
                    self.refresh_lists(false);
                }
            }
            "unshield" => drop(self.shield.borrow_mut().off.remove(v.as_str().unwrap_or(""))),
            "pref" => {
                let (name, on) = (v["name"].as_str().unwrap_or(""), v["on"].as_bool().unwrap_or(false));
                let p = &mut self.state.shield;
                match name {
                    "aggressive" => p.aggressive = on,
                    "fingerprint" => p.fingerprint = on,
                    "https" => p.https = on,
                    "query" => p.query = on,
                    "debounce" => p.debounce = on,
                    "gpc" => p.gpc = on,
                    _ => return,
                }
                self.shield.borrow_mut().prefs = self.state.shield.clone();
            }
            "list" => {
                let (uuid, on) = (v["uuid"].as_str().unwrap_or("").to_string(), v["on"].as_bool().unwrap_or(false));
                if uuid.is_empty() {
                    return;
                }
                let p = &mut self.state.shield;
                p.lists_on.retain(|u| *u != uuid);
                p.lists_off.retain(|u| *u != uuid);
                if on { p.lists_on.push(uuid) } else { p.lists_off.push(uuid) }
                if !self.shield.borrow_mut().reload(&lists(&self.state)) {
                    self.refresh_lists(false);
                }
            }
            "passwords" => {
                self.state.passwords = b;
                self.apply_profiles();
            }
            "autofill" => {
                self.state.autofill = b;
                self.apply_profiles();
            }
            "downloads" => {
                if let Some(p) = dialog::folder(self.window.hwnd(), "Download location") {
                    self.state.downloads = p.to_string_lossy().into();
                    self.apply_profiles();
                }
            }
            "show-downloads" => self.downloads(),
            "zoom" => {
                let z = v.as_f64().unwrap_or(1.0).clamp(0.25, 3.0);
                self.state.zoom = z;
                for t in &self.tabs {
                    if let Some(view) = &t.view {
                        let _ = unsafe { view.controller().SetZoomFactor(z) };
                    }
                }
            }
            "gpu" => self.state.gpu = b,
            "language" => self.state.language = v.as_str().unwrap_or("").into(),
            "relaunch" => {
                self.relaunch = true;
                self.quit = true;
            }
            "reset" => self.reset(),
            "import" => {
                let k = v.as_u64().unwrap_or(0) as u32;
                self.menu_action(if k >= 1000 { k - 1000 + 200 } else { k });
            }
            "winsettings" => open_windows_settings(v.as_str().unwrap_or("")),
            "ext-install" => self.install(v.as_str().unwrap_or("")),
            "ext-unpacked" => {
                if let Some(p) = dialog::folder(self.window.hwnd(), "Choose the extension's folder (the one with manifest.json)") {
                    if p.join("manifest.json").exists() {
                        self.add_ext(p);
                    } else {
                        self.message("Extensions", "That folder has no manifest.json: choose the extension's own folder.");
                    }
                }
            }
            "ext-enable" => self.ext_act(v["id"].as_str().unwrap_or(""), ext::Act::Enable(v["on"].as_bool().unwrap_or(true))),
            "ext-remove" => {
                let id = v.as_str().unwrap_or("").to_string();
                self.ext_act(&id, ext::Act::Remove);
                // A Web Store download is ours to delete; an unpacked folder is the person's.
                self.state.ext_paths.remove(&id);
                if id.len() == 32 && id.bytes().all(|b| b.is_ascii_lowercase()) {
                    let _ = fs::remove_dir_all(ext::dir().join(&id));
                }
            }
            "ext-open" => self.ext_page(v.as_str().unwrap_or(""), false),
            "ext-options" => self.ext_page(v.as_str().unwrap_or(""), true),
            "open" => {
                let url = v.as_str().unwrap_or("").to_string();
                if url.starts_with("https://") {
                    let t = self.add_tab(url, false, None);
                    self.select(t);
                }
            }
            "update" => self.refresh_lists(true),
            "clear" => self.clear(v.as_str().unwrap_or("")),
            _ => return,
        }
        self.dirty = true;
        self.render();
        self.settings(false);
    }

    /// uBO's scriptlets come from uBO's own source, read by a hidden page on the
    /// shared Edge process; it goes away as soon as it has answered.
    fn fetch_ubo(&mut self, force: bool) {
        if self.ubo.is_some() || !(force || shield::ubo_stale()) {
            return;
        }
        let proxy = self.proxy.clone();
        self.ubo = WebViewBuilder::new()
            .with_environment(self.env.clone())
            .with_html(shield::ubo_page())
            .with_visible(false)
            .with_bounds(rect(0.0, 0.0, 0.0, 0.0))
            .with_ipc_handler(move |req| {
                let _ = proxy.send_event(Ev::Ubo(req.into_body()));
            })
            .build_as_child(&self.window)
            .ok();
    }

    fn refresh_lists(&mut self, force: bool) {
        self.fetch_ubo(force);
        let proxy = self.proxy.clone();
        self.updating = true;
        shield::refresh(lists(&self.state), force, move |c| {
            let _ = proxy.send_event(Ev::Shield(c));
        });
    }

    fn clear(&mut self, what: &str) {
        let kinds = match what {
            "history" => {
                self.history.clear();
                self.closed.clear();
                COREWEBVIEW2_BROWSING_DATA_KINDS_BROWSING_HISTORY
            }
            "cookies" => COREWEBVIEW2_BROWSING_DATA_KINDS_ALL_SITE,
            "cache" => COREWEBVIEW2_BROWSING_DATA_KINDS_DISK_CACHE,
            _ => return,
        };
        // Every tab shares the frame's profile, so clearing it clears theirs.
        unsafe {
            let profile = self.chrome.webview().cast::<ICoreWebView2_13>()
                .and_then(|w| w.Profile())
                .and_then(|p| p.cast::<ICoreWebView2Profile2>());
            if let Ok(p) = profile {
                let _ = p.ClearBrowsingData(kinds, &ClearBrowsingDataCompletedHandler::create(Box::new(|_| Ok(()))));
            }
        }
    }

    /// The mouse over the window's own area: in a split view, that's the divider.
    fn pointer(&mut self, x: f64) {
        self.pointer = x;
        if self.dragging {
            let (ax, _, w, _) = self.page_area();
            self.state.split = ((x - ax - GAP / 2.0) / (w - GAP)).clamp(0.15, 0.85);
            self.layout();
        }
        let near = self.dragging || self.on_divider();
        self.window.set_cursor_icon(if near { CursorIcon::ColResize } else { CursorIcon::Default });
    }

    fn on_divider(&self) -> bool {
        self.divider.is_some_and(|d| self.pointer >= d - 2.0 && self.pointer <= d + GAP + 2.0)
    }

    fn ipc(&mut self, msg: &str) {
        let Ok(m) = serde_json::from_str::<Value>(msg) else { return };
        let s = |k: &str| m[k].as_str().unwrap_or("").to_string();
        let id = m["id"].as_u64().unwrap_or(0) as u32;
        match m["cmd"].as_str().unwrap_or("") {
            "ready" => {
                self.layout();
                self.render();
                match self.current().map(|t| t.url == BLANK) {
                    None => self.focus_field("new"),
                    Some(true) => self.focus_field("here"),
                    Some(false) => {}
                }
            }
            "go" => {
                let mode = s("mode");
                self.go(&s("text"), mode == "new" || mode == "private", mode == "private")
            }
            "select" => self.select(id),
            "close" => self.close(id),
            "tabmenu" => self.tab_menu(id),
            "mute" => self.mute(id),
            "set" => self.set(&s("key"), &m["value"]),
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
                "fold" => self.command(Cmd::Fold),
                "settings" => self.command(Cmd::Settings),
                "shield" => self.shield_menu(),
                "menu" => self.main_menu(),
                "extensions" => self.ext_menu(),
                "install" => self.install_current(),
                "field" => self.command(Cmd::Field),
                "back" => self.command(Cmd::Back),
                "forward" => self.command(Cmd::Forward),
                "reload" => {
                    if let Some(v) = self.current().and_then(|t| t.view.as_ref()) {
                        let _ = v.reload();
                    }
                }
                "stop" => {
                    if let Some(v) = self.current().and_then(|t| t.view.as_ref()) {
                        let _ = unsafe { v.webview().Stop() };
                    }
                }
                _ => {}
            },
            "done" => self.done(m["cancel"].as_bool().unwrap_or(false)),
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
                    self.tabs[i].loading = true;
                    self.render();
                }
            }
            Ev::Loaded(id, url) => {
                if let Some(i) = self.idx(id) {
                    self.tabs[i].url = url.clone();
                    self.tabs[i].loading = false;
                    if !self.tabs[i].private {
                        let title = self.tabs[i].title.clone();
                        self.visit(&url, &title);
                    }
                    self.render();
                }
            }
            Ev::Shield(c) => {
                self.updating = false;
                if let Some(c) = c.filter(|c| c.key == lists(&self.state).key()) {
                    self.shield.borrow_mut().install(c);
                }
                self.settings(false);
            }
            Ev::Icon(id, uri) => {
                if let Some(i) = self.idx(id) {
                    self.tabs[i].icon = uri;
                    self.render();
                }
            }
            Ev::Audio(id, playing, muted) => {
                if let Some(i) = self.idx(id) {
                    (self.tabs[i].audio, self.tabs[i].muted) = (playing, muted);
                    self.render();
                }
            }
            // A click into the other half of a split view hands it the keys.
            Ev::Focused(id) => {
                if self.active != Some(id) && self.shown.contains(&id) && !self.overlay {
                    self.active = Some(id);
                    self.dirty = true;
                    self.render();
                }
            }
            Ev::PageFull(on) => self.set_full(on),
            Ev::Exts(list) => {
                // WebView2's own built-ins (the PDF viewer, the clipboard helper) stay out of sight.
                const BUILT_IN: [&str; 2] = ["mhjfbmdgcfjbbpaeojofohoefgiehjai", "dgiklkfkllikcanfonkcabmbdfmgleag"];
                self.exts = list.into_iter().filter(|x| !BUILT_IN.contains(&x.id.as_str())).collect();
                self.settings(false);
            }
            Ev::ExtFetched(r) => match r {
                Ok(path) => self.add_ext(path),
                Err(e) => {
                    self.installing = false;
                    self.settings(false);
                    self.message("Extensions", &e);
                }
            },
            Ev::ExtAdded(r, path) => {
                self.installing = false;
                match r {
                    Ok(x) => {
                        if let Some(p) = path {
                            self.state.ext_paths.insert(x.id.clone(), p.to_string_lossy().into());
                            self.dirty = true;
                        }
                        self.message("Extensions", &format!("Added {}. Find it in the ⋯ menu under Extensions.", x.name));
                    }
                    Err(e) => self.message("Extensions", &e),
                }
                self.list_exts();
            }
            Ev::Ubo(json) => {
                self.ubo = None;
                self.shield.borrow_mut().set_ubo(&json);
            }
            Ev::Open(url, private) => {
                let at = self.active.and_then(|id| self.idx(id)).map(|i| i + 1);
                let id = self.add_tab(url, private, at);
                self.select(id);
            }
        }
    }

    fn persist(&mut self) {
        let kept = |t: &&Tab| !t.private && t.url != BLANK;
        // A split view is saved as a number both halves share.
        let pair = |t: &Tab| t.partner.filter(|p| self.tabs.iter().any(|o| o.id == *p && kept(&o))).map_or(0, |p| p.min(t.id));
        self.state.tabs = self.tabs.iter().filter(kept)
            .map(|t| Saved { url: t.url.clone(), title: t.title.clone(), pinned: t.pinned, pair: pair(t) }).collect();
        let mut off: Vec<String> = self.shield.borrow().off.iter().cloned().collect();
        off.sort();
        self.state.shields_off = off;
        self.state.active = self.tabs.iter().filter(kept)
            .position(|t| Some(t.id) == self.active).unwrap_or(0);
        save("state.json", &self.state);
        save("history.json", &self.history);
        self.dirty = false;
        self.last_save = Instant::now();
    }
}

/// Which filter lists Shields builds with, from Settings.
fn browser_args(s: &State) -> String {
    let mut args = String::from("--disable-features=msWebOOUI,msPdfOOUI,msSmartScreenProtection");
    if !s.gpu {
        args += " --disable-gpu";
    }
    let lang = s.language.trim();
    if !lang.is_empty() && lang.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') {
        args += &format!(" --lang={lang} --accept-lang={lang}");
    }
    // For development: DevTools protocol on this port (WebView2 ignores
    // WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS once the app passes its own).
    if let Ok(port) = std::env::var("BRISKLARK_DEBUG_PORT") {
        if !port.is_empty() && port.bytes().all(|b| b.is_ascii_digit()) {
            args += &format!(" --remote-debugging-port={port}");
        }
    }
    args
}

fn lists(s: &State) -> shield::Config {
    shield::Config { cookies: s.cookie_notices, lists_on: s.shield.lists_on.clone(), lists_off: s.shield.lists_off.clone() }
}

/// Starts Brisklark again once this one (and its Edge processes) have gone.
fn relaunch() {
    use std::os::windows::process::CommandExt;
    if let Ok(exe) = std::env::current_exe() {
        let _ = std::process::Command::new("cmd.exe")
            .raw_arg(format!("/c ping -n 4 127.0.0.1 >nul & start \"\" \"{}\"", exe.display()))
            .creation_flags(0x0800_0000)
            .spawn();
    }
}

fn open_windows_settings(page: &str) {
    if page.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') {
        let _ = std::process::Command::new("explorer.exe").arg(format!("ms-settings:{page}")).spawn();
    }
}

/// The window's own colour shows only in a split view's divider: match the frame.
fn paint(window: &Window, theme: Theme) {
    window.set_background_color(Some(if theme == Theme::Dark { (31, 31, 31, 255) } else { (243, 243, 243, 255) }));
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
        .with_window_icon(Icon::from_resource(1, None).ok())
        .with_taskbar_icon(Icon::from_resource(1, None).ok())
        .build(&event_loop)
        .expect("window");
    paint(&window, window.theme());

    let mut state: State = load("state.json");
    // An engine the list no longer offers (Brave Search, a custom address from
    // an older state) goes back to the default.
    if !ENGINES.iter().any(|(_, u)| *u == state.engine) {
        state.engine = ENGINES[0].1.into();
    }
    let off: HashSet<String> = state.shields_off.iter().cloned().collect();
    let shield = shield::Shield::load(state.shields, off, state.shield.clone(), &lists(&state));
    let history: Vec<Visit> = load("history.json");
    let ipc = proxy.clone();
    let mut ctx = WebContext::new(Some(dir("LOCALAPPDATA").join("WebView2")));
    let chrome = WebViewBuilder::new_with_web_context(&mut ctx)
        .with_html(include_str!("chrome.html"))
        .with_default_context_menus(false)
        .with_ipc_handler(move |req| {
            let _ = ipc.send_event(Ev::Ipc(req.into_body()));
        })
        .with_bounds(rect(0.0, 0.0, 1200.0, BAR))
        // Transparent, so a suggestion list or a panel can hang over the page
        // without hiding it; the bar itself paints its own background.
        .with_background_color((0, 0, 0, 0))
        // The environment every page shares: extensions on, and the person's
        // choices that WebView2 only takes when it starts.
        .with_browser_extensions_enabled(true)
        .with_additional_browser_args(browser_args(&state))
        .build_as_child(&window)
        .expect("WebView2 runtime");
    hook_keys(&chrome, proxy.clone());

    let mut app = App {
        window,
        env: chrome.environment(),
        shield,
        chrome,
        proxy,
        tabs: vec![],
        active: None,
        next: 1,
        closed: vec![],
        history,
        state,
        overlay: false,
        shown: vec![],
        divider: None,
        pointer: 0.0,
        dragging: false,
        updating: false,
        ubo: None,
        full: false,
        exts: vec![],
        installing: false,
        relaunch: false,
        dirty: false,
        last_save: Instant::now(),
        quit: false,
    };
    app.apply_profile(&app.chrome);
    app.list_exts();
    // On startup: the last session, or (Settings) just the pinned tabs and a new tab.
    let fresh = app.state.startup == "new";
    let mut pairs: Vec<(u32, u32)> = vec![]; // saved pair number, tab
    for s in std::mem::take(&mut app.state.tabs) {
        if fresh && !s.pinned {
            continue;
        }
        let id = app.add_tab(s.url, false, None);
        let t = app.tabs.last_mut().unwrap();
        (t.title, t.pinned) = (s.title, s.pinned);
        if s.pair != 0 {
            match pairs.iter().find(|p| p.0 == s.pair) {
                Some(&(_, other)) => {
                    for (a, b) in [(id, other), (other, id)] {
                        let i = app.idx(a).unwrap();
                        app.tabs[i].partner = Some(b);
                    }
                }
                None => pairs.push((s.pair, id)),
            }
        }
    }
    app.dirty = false;
    let p = app.proxy.clone();
    shield::refresh(lists(&app.state), false, move |c| {
        let _ = p.send_event(Ev::Shield(c));
    });
    app.fetch_ubo(false);
    if fresh || app.tabs.is_empty() {
        let id = app.add_tab(BLANK.into(), false, None);
        app.select(id);
    } else if let Some(t) = app.tabs.get(app.state.active).or(app.tabs.first()) {
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
                    if app.relaunch {
                        relaunch();
                    }
                    *flow = ControlFlow::Exit;
                }
            }
            Event::WindowEvent { event, .. } => match event {
                WindowEvent::CloseRequested => {
                    app.persist();
                    *flow = ControlFlow::Exit;
                }
                WindowEvent::Resized(_) | WindowEvent::ScaleFactorChanged { .. } => app.layout(),
                WindowEvent::ThemeChanged(theme) => paint(&app.window, theme),
                WindowEvent::CursorMoved { position, .. } => app.pointer(position.x / app.window.scale_factor()),
                WindowEvent::MouseInput { state, button: MouseButton::Left, .. } => {
                    let down = state == ElementState::Pressed;
                    if down && app.on_divider() {
                        app.dragging = true;
                    } else if !down && app.dragging {
                        app.dragging = false;
                        app.dirty = true;
                    }
                }
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
        assert_eq!(shortcut(0xBC, true, false, false), Some(Cmd::Settings));
    }
}
