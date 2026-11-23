//! Shields: the ad blocker, set up the way Brave ships it. Brave's own engine
//! (adblock-rust) runs the lists from Brave's default catalog, with uBlock
//! Origin's scriptlets from Brave's resources file. As in Brave's Standard mode,
//! everything blocks cross-site requests, and only Brave's first-party list
//! touches a site's requests to itself.

use adblock::{lists::ParseOptions, request::Request, resources::Resource, Engine, FilterSet};
use base64::Engine as _;
use std::{
    cell::{Cell, RefCell},
    collections::HashSet,
    fs,
    os::windows::process::CommandExt,
    path::PathBuf,
    process::Command,
    rc::Rc,
    time::{Duration, SystemTime},
};
use webview2_com::{
    take_pwstr, AddScriptToExecuteOnDocumentCreatedCompletedHandler, ExecuteScriptCompletedHandler,
    Microsoft::Web::WebView2::Win32::*, NavigationStartingEventHandler, WebMessageReceivedEventHandler,
    WebResourceRequestedEventHandler,
};
use windows::{
    core::{Interface, HSTRING, PWSTR, w},
    Win32::UI::Shell::SHCreateMemStream,
};
use wry::{WebView, WebViewExtWindows};

const UBO: &str = "https://raw.githubusercontent.com/uBlockOrigin/uAssets/master/filters/";
const BRAVE: &str = "https://raw.githubusercontent.com/brave/adblock-lists/master/brave-lists/";

/// Brave's "Default Adblock Filters" and "Default Privacy Filters" (desktop).
fn main_lists() -> Vec<String> {
    let ubo = [
        "filters", "filters-2020", "filters-2021", "filters-2022", "filters-2023", "filters-2024",
        "filters-2025", "filters-2026", "filters-general", "badware", "resource-abuse", "unbreak",
        "quick-fixes", "ubo-link-shorteners", "privacy",
    ];
    let brave = ["brave-specific", "brave-social", "brave-unbreak", "brave-sugarcoat"];
    let mut v: Vec<String> = ubo.iter().map(|n| format!("{UBO}{n}.txt")).collect();
    v.extend(brave.iter().map(|n| format!("{BRAVE}{n}.txt")));
    v.extend([
        "https://raw.githubusercontent.com/brave/adblock-lists/master/brave-unbreak.txt".into(),
        "https://easylist.to/easylist/easylist.txt".into(),
        "https://easylist.to/easylist/easyprivacy.txt".into(),
        "https://malware-filter.gitlab.io/malware-filter/urlhaus-filter-agh-online.txt".into(),
    ]);
    v
}
/// Brave's "Cookie notice blocker".
fn cookie_lists() -> Vec<String> {
    vec![
        "https://secure.fanboy.co.nz/fanboy-cookiemonster_ubo.txt".into(),
        format!("{UBO}annoyances-cookies.txt"),
        format!("{BRAVE}brave-cookie-specific.txt"),
    ]
}
/// Brave's "First Party Adblock Filters": the only rules a site's own requests meet.
fn first_lists() -> Vec<String> {
    vec![format!("{BRAVE}brave-firstparty.txt"), format!("{BRAVE}brave-firstparty-regional.txt")]
}
const RESOURCES: &str = "https://raw.githubusercontent.com/brave/adblock-resources/master/dist/resources.json";
const STALE: Duration = Duration::from_secs(4 * 24 * 3600); // lists change daily; four days is plenty

/// Used only until the lists have been downloaded once.
const FALLBACK: &[&str] = &[
    "doubleclick.net", "googlesyndication.com", "googleadservices.com",
    "googletagservices.com", "google-analytics.com", "googletagmanager.com",
    "adservice.google.com", "amazon-adsystem.com", "adnxs.com", "adsrvr.org",
    "criteo.com", "criteo.net", "taboola.com", "outbrain.com",
    "rubiconproject.com", "pubmatic.com", "openx.net", "casalemedia.com",
    "smartadserver.com", "sharethrough.com", "indexww.com", "bidswitch.net",
    "33across.com", "teads.tv", "moatads.com", "adroll.com",
    "scorecardresearch.com", "quantserve.com", "chartbeat.com",
    "hotjar.com", "mouseflow.com", "fullstory.com", "clarity.ms",
    "mixpanel.com", "amplitude.com", "segment.com", "segment.io",
    "branch.io", "appsflyer.com", "adjust.com", "analytics.tiktok.com",
    "connect.facebook.net", "ads-twitter.com", "analytics.twitter.com",
];

#[derive(Default)]
pub struct Shield {
    main: Option<Engine>,
    first: Option<Engine>,
    /// Shields up at all (Settings › Shields).
    pub on: bool,
    /// Sites the person switched Shields off for.
    pub off: HashSet<String>,
}
pub type Shared = Rc<RefCell<Shield>>;

/// What one tab knows about its page, for the blocker.
#[derive(Clone, Default)]
pub struct Page {
    pub url: Rc<RefCell<String>>,
    pub blocked: Rc<Cell<u32>>,
    exceptions: Rc<RefCell<HashSet<String>>>,
}

/// A freshly compiled set, handed over from the download thread as bytes.
pub struct Compiled {
    main: Vec<u8>,
    first: Vec<u8>,
    resources: Vec<u8>,
}

fn folder() -> PathBuf {
    crate::dir("LOCALAPPDATA").join("Shields")
}
fn main_file(cookies: bool) -> PathBuf {
    folder().join(if cookies { "main+cookies.dat" } else { "main.dat" })
}

fn engine_from(bytes: &[u8]) -> Option<Engine> {
    let mut e = Engine::default();
    e.deserialize(bytes).ok().map(|_| e)
}

impl Shield {
    pub fn load(on: bool, off: HashSet<String>, cookies: bool) -> Shared {
        let mut s = Shield { on, off, ..Default::default() };
        s.reload(cookies);
        Rc::new(RefCell::new(s))
    }

    /// Loads the compiled lists from disk; false when they aren't there yet.
    pub fn reload(&mut self, cookies: bool) -> bool {
        let (Ok(main), Ok(first), Ok(resources)) = (
            fs::read(main_file(cookies)),
            fs::read(folder().join("first.dat")),
            fs::read(folder().join("resources.json")),
        ) else {
            return false;
        };
        self.install(Compiled { main, first, resources });
        true
    }

    pub fn install(&mut self, c: Compiled) {
        if let (Some(mut main), Some(first)) = (engine_from(&c.main), engine_from(&c.first)) {
            if let Ok(res) = serde_json::from_slice::<Vec<Resource>>(&c.resources) {
                main.use_resources(res);
            }
            (self.main, self.first) = (Some(main), Some(first));
        }
    }

    #[cfg(test)]
    pub fn ready(&self) -> bool {
        self.main.is_some()
    }

    /// Shields are up for the page at this address.
    pub fn up_for(&self, page: &str) -> bool {
        self.on && !self.off.contains(&crate::host(page).to_ascii_lowercase())
    }

    pub fn check(&self, url: &str, page: &str, kind: &str) -> Verdict {
        if !self.up_for(page) {
            return Verdict::Allow;
        }
        let Some(main) = &self.main else {
            let (h, t) = (crate::host(url).to_ascii_lowercase(), crate::host(page).to_ascii_lowercase());
            return if FALLBACK.iter().any(|d| under(&h, d) && !under(&t, d)) { Verdict::Block } else { Verdict::Allow };
        };
        let Ok(req) = Request::new(url, page, kind, "get") else { return Verdict::Allow };
        let engine = if req.is_third_party { Some(main) } else { self.first.as_ref() };
        let Some(engine) = engine else { return Verdict::Allow };
        let r = engine.check_network_request(&req);
        match (r.should_block(), r.redirect) {
            (true, Some(stub)) => Verdict::Redirect(stub),
            (true, None) => Verdict::Block,
            _ => Verdict::Allow,
        }
    }

    /// The script a page gets before its own: this site's hiding rules, its
    /// scriptlets, and (unless the lists say not to) the scanner that asks for
    /// generic hiding rules as classes and ids appear.
    fn page_script(&self, url: &str) -> Option<(String, HashSet<String>)> {
        let main = self.main.as_ref().filter(|_| self.up_for(url))?;
        if !url.starts_with("http") {
            return None;
        }
        let res = main.url_cosmetic_resources(url);
        let selectors: Vec<&str> =
            res.hide_selectors.iter().filter(|s| !res.exceptions.contains(*s)).map(String::as_str).collect();
        let css = if selectors.is_empty() { String::new() } else { format!("{}{{display:none!important}}", selectors.join(",")) };
        let script = format!(
            "{scriptlets}\n;(()=>{{const css={css};const generic={generic};\
const add=t=>{{const s=document.createElement('style');s.textContent=t;(document.head||document.documentElement).appendChild(s)}};\
const ready=f=>document.documentElement?f():new MutationObserver((_,o)=>{{if(document.documentElement){{o.disconnect();f()}}}}).observe(document,{{childList:true}});\
if(css)ready(()=>add(css));\
if(!generic||window.top!==window||!(window.chrome&&chrome.webview))return;\
const sc=new Set(),si=new Set();let qc=[],qi=[],t=0;\
const flush=()=>{{t=0;if(qc.length||qi.length){{chrome.webview.postMessage({{brisklark:'cosmetic',c:qc,i:qi}});qc=[];qi=[]}}}};\
const take=e=>{{if(e.id&&!si.has(e.id)){{si.add(e.id);qi.push(e.id)}}if(e.classList)for(const c of e.classList)if(!sc.has(c)){{sc.add(c);qc.push(c)}}}};\
const scan=r=>{{if(r.nodeType!==1)return;take(r);for(const e of r.querySelectorAll('[id],[class]'))take(e)}};\
const soon=()=>{{if(!t)t=setTimeout(flush,50)}};\
ready(()=>{{scan(document.documentElement);soon();new MutationObserver(ms=>{{for(const m of ms){{if(m.type==='attributes')take(m.target);for(const n of m.addedNodes)scan(n)}}soon()}})\
.observe(document.documentElement,{{childList:true,subtree:true,attributes:true,attributeFilter:['class','id']}})}})}})()",
            scriptlets = res.injected_script,
            css = serde_json::to_string(&css).unwrap_or_default(),
            generic = !res.generichide,
        );
        Some((script, res.exceptions))
    }

    fn generic_css(&self, classes: &[String], ids: &[String], exceptions: &HashSet<String>) -> Option<String> {
        let sel = self.main.as_ref()?.hidden_class_id_selectors(classes, ids, exceptions);
        (!sel.is_empty()).then(|| format!("{}{{display:none!important}}", sel.join(",")))
    }
}

#[derive(Debug, PartialEq)]
pub enum Verdict {
    Allow,
    Block,
    Redirect(String),
}

fn under(host: &str, domain: &str) -> bool {
    host.strip_suffix(domain).is_some_and(|r| r.is_empty() || r.ends_with('.'))
}

/// Downloads and compiles the lists off the UI thread when they are missing or
/// old (or `force`), then hands them back as bytes; the engines themselves stay
/// on the UI thread. All lists come down at once with Windows' own curl.exe.
/// `done` gets None when the download failed (offline): the old lists stay.
pub fn refresh(cookies: bool, force: bool, done: impl FnOnce(Option<Compiled>) + Send + 'static) {
    let fresh = fs::metadata(main_file(cookies))
        .and_then(|m| m.modified())
        .is_ok_and(|t| t.elapsed().unwrap_or(STALE) < STALE);
    if fresh && !force {
        return;
    }
    std::thread::spawn(move || done(download(cookies)));
}

fn download(cookies: bool) -> Option<Compiled> {
    {
        let dir = folder().join("download");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).ok()?;
        let main = [main_lists(), if cookies { cookie_lists() } else { vec![] }].concat();
        let first = first_lists();
        let all: Vec<&String> = main.iter().chain(&first).collect();
        let mut cmd = Command::new("curl.exe");
        cmd.args(["-sfL", "--parallel", "--max-time", "120"]).creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        for (i, url) in all.iter().enumerate() {
            cmd.arg("-o").arg(dir.join(format!("{i}.txt"))).arg(url);
        }
        cmd.arg("-o").arg(dir.join("resources.json")).arg(RESOURCES);
        let _ = cmd.output(); // a list that fails is skipped; the rest still count
        let read = |i: usize| fs::read_to_string(dir.join(format!("{i}.txt"))).ok().filter(|t| !t.is_empty());
        let mut main_set = FilterSet::new(false);
        let mut got = 0;
        for i in 0..main.len() {
            if let Some(text) = read(i) {
                main_set.add_filter_list(text, ParseOptions::default());
                got += 1;
            }
        }
        let mut first_set = FilterSet::new(false);
        for i in main.len()..all.len() {
            if let Some(text) = read(i) {
                first_set.add_filter_list(text, ParseOptions::default());
            }
        }
        let resources = fs::read(dir.join("resources.json")).unwrap_or_default();
        if got < main.len() / 2 || resources.is_empty() {
            return None; // offline or mostly failed: keep what we have, try next launch
        }
        let c = Compiled {
            main: Engine::new_with_filter_set(main_set).serialize(),
            first: Engine::new_with_filter_set(first_set).serialize(),
            resources,
        };
        let save = |p: PathBuf, b: &[u8]| {
            let tmp = p.with_extension("tmp");
            if fs::write(&tmp, b).is_ok() {
                let _ = fs::rename(tmp, p);
            }
        };
        save(main_file(cookies), &c.main);
        save(folder().join("first.dat"), &c.first);
        save(folder().join("resources.json"), &c.resources);
        let _ = fs::remove_dir_all(&dir);
        Some(c)
    }
}

/// When the lists on disk were last compiled, for Settings.
pub fn updated(cookies: bool) -> Option<SystemTime> {
    fs::metadata(main_file(cookies)).and_then(|m| m.modified()).ok()
}

fn kind(ctx: COREWEBVIEW2_WEB_RESOURCE_CONTEXT) -> &'static str {
    match ctx {
        COREWEBVIEW2_WEB_RESOURCE_CONTEXT_DOCUMENT => "sub_frame",
        COREWEBVIEW2_WEB_RESOURCE_CONTEXT_STYLESHEET => "stylesheet",
        COREWEBVIEW2_WEB_RESOURCE_CONTEXT_IMAGE => "image",
        COREWEBVIEW2_WEB_RESOURCE_CONTEXT_MEDIA => "media",
        COREWEBVIEW2_WEB_RESOURCE_CONTEXT_FONT => "font",
        COREWEBVIEW2_WEB_RESOURCE_CONTEXT_SCRIPT => "script",
        COREWEBVIEW2_WEB_RESOURCE_CONTEXT_XML_HTTP_REQUEST | COREWEBVIEW2_WEB_RESOURCE_CONTEXT_FETCH => "xhr",
        COREWEBVIEW2_WEB_RESOURCE_CONTEXT_PING => "ping",
        COREWEBVIEW2_WEB_RESOURCE_CONTEXT_WEBSOCKET => "websocket",
        _ => "other",
    }
}

/// A redirect stub ("data:mime;base64,…") as a response body and content type.
fn stub(data: &str) -> (Vec<u8>, String) {
    if let Some(rest) = data.strip_prefix("data:") {
        if let Some((head, body)) = rest.split_once(',') {
            let mime = head.split(';').next().unwrap_or("text/plain").to_string();
            let bytes = if head.ends_with(";base64") {
                base64::engine::general_purpose::STANDARD.decode(body).unwrap_or_default()
            } else {
                body.as_bytes().to_vec()
            };
            return (bytes, mime);
        }
    }
    (data.as_bytes().to_vec(), "text/plain".into())
}

/// Puts Shields on one tab.
pub fn attach(view: &WebView, shared: Shared, page: Page) {
    let core = view.webview();
    let env = view.environment();
    let script_id: Rc<RefCell<Option<String>>> = Default::default();
    unsafe {
        // Every request comes here first; the engine answers in microseconds.
        let filter = w!("*");
        let _ = match core.cast::<ICoreWebView2_22>() {
            Ok(v) => v.AddWebResourceRequestedFilterWithRequestSourceKinds(
                filter,
                COREWEBVIEW2_WEB_RESOURCE_CONTEXT_ALL,
                COREWEBVIEW2_WEB_RESOURCE_REQUEST_SOURCE_KINDS_ALL,
            ),
            Err(_) => core.AddWebResourceRequestedFilter(filter, COREWEBVIEW2_WEB_RESOURCE_CONTEXT_ALL),
        };
        let (shield, p) = (shared.clone(), page.clone());
        let requested = WebResourceRequestedEventHandler::create(Box::new(move |_, args| {
            let Some(args) = args else { return Ok(()) };
            let mut uri = PWSTR::null();
            args.Request()?.Uri(&mut uri)?;
            let uri = take_pwstr(uri);
            let top = p.url.borrow();
            if uri == *top || !uri.starts_with("http") {
                return Ok(()); // the page itself, or data:/blob: and the like
            }
            let mut ctx = COREWEBVIEW2_WEB_RESOURCE_CONTEXT::default();
            args.ResourceContext(&mut ctx)?;
            let response = match shield.borrow().check(&uri, &top, kind(ctx)) {
                Verdict::Allow => return Ok(()),
                Verdict::Block => env.CreateWebResourceResponse(None, 403, w!("Blocked"), w!(""))?,
                Verdict::Redirect(data) => {
                    let (body, mime) = stub(&data);
                    let stream = SHCreateMemStream(Some(&body));
                    let headers = HSTRING::from(format!("Content-Type: {mime}\r\nAccess-Control-Allow-Origin: *"));
                    env.CreateWebResourceResponse(stream.as_ref(), 200, w!("OK"), &headers)?
                }
            };
            p.blocked.set(p.blocked.get() + 1);
            args.SetResponse(&response)?;
            Ok(())
        }));
        let mut token = 0i64;
        let _ = core.add_WebResourceRequested(&requested, &mut token);

        // Each page gets its own site's rules and scriptlets, swapped in as it starts.
        let (shield, p) = (shared.clone(), page.clone());
        let starting = NavigationStartingEventHandler::create(Box::new(move |sender, args| {
            let (Some(core), Some(args)) = (sender, args) else { return Ok(()) };
            let mut uri = PWSTR::null();
            args.Uri(&mut uri)?;
            let uri = take_pwstr(uri);
            *p.url.borrow_mut() = uri.clone();
            p.blocked.set(0);
            if let Some(old) = script_id.borrow_mut().take() {
                let _ = core.RemoveScriptToExecuteOnDocumentCreated(&HSTRING::from(old));
            }
            let made = shield.borrow().page_script(&uri);
            let Some((script, exceptions)) = made else {
                p.exceptions.borrow_mut().clear();
                return Ok(());
            };
            *p.exceptions.borrow_mut() = exceptions;
            let slot = script_id.clone();
            let _ = core.AddScriptToExecuteOnDocumentCreated(
                &HSTRING::from(script),
                &AddScriptToExecuteOnDocumentCreatedCompletedHandler::create(Box::new(move |hr, id| {
                    if hr.is_ok() {
                        *slot.borrow_mut() = Some(id);
                    }
                    Ok(())
                })),
            );
            Ok(())
        }));
        let _ = core.add_NavigationStarting(&starting, &mut token);

        // Generic hiding: the page reports new classes and ids, and gets back
        // only the rules that match them.
        let (shield, p) = (shared, page);
        let message = WebMessageReceivedEventHandler::create(Box::new(move |sender, args| {
            let (Some(core), Some(args)) = (sender, args) else { return Ok(()) };
            let mut json = PWSTR::null();
            args.WebMessageAsJson(&mut json)?;
            let Ok(m) = serde_json::from_str::<serde_json::Value>(&take_pwstr(json)) else { return Ok(()) };
            if m["brisklark"] != "cosmetic" {
                return Ok(());
            }
            let list = |k: &str| -> Vec<String> {
                m[k].as_array().map_or(vec![], |a| a.iter().take(4000).filter_map(|v| v.as_str().map(String::from)).collect())
            };
            let css = shield.borrow().generic_css(&list("c"), &list("i"), &p.exceptions.borrow());
            if let Some(css) = css {
                let js = format!(
                    "(()=>{{const s=document.createElement('style');s.textContent={};(document.head||document.documentElement).appendChild(s)}})()",
                    serde_json::to_string(&css).unwrap_or_default()
                );
                let _ = core.ExecuteScript(&HSTRING::from(js), &ExecuteScriptCompletedHandler::create(Box::new(|_, _| Ok(()))));
            }
            Ok(())
        }));
        let _ = core.add_WebMessageReceived(&message, &mut token);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shield_with(rules: &str) -> Shield {
        let mut set = FilterSet::new(false);
        set.add_filter_list(rules.into(), ParseOptions::default());
        let mut first = FilterSet::new(false);
        first.add_filter_list("||news.com/firstparty-ad.js^".into(), ParseOptions::default());
        let mut s = Shield { on: true, ..Default::default() };
        // Round-trip through the on-disk format, as on a real launch.
        s.install(Compiled {
            main: Engine::new_with_filter_set(set).serialize(),
            first: Engine::new_with_filter_set(first).serialize(),
            resources: br#"[{"name":"noop.js","aliases":[],"kind":{"mime":"application/javascript"},"content":"KGZ1bmN0aW9uKCl7fSkoKTs="}]"#.to_vec(),
        });
        s
    }

    #[test]
    fn fallback_list() {
        let s = Shield { on: true, ..Default::default() };
        assert_eq!(s.check("https://stats.g.doubleclick.net/x.js", "https://news.com/", "script"), Verdict::Block);
        assert_eq!(s.check("https://notdoubleclick.net/x.js", "https://news.com/", "script"), Verdict::Allow);
        assert_eq!(s.check("https://www.hotjar.com/a.js", "https://www.hotjar.com/", "script"), Verdict::Allow);
    }

    #[test]
    fn standard_mode_and_switches() {
        let mut s = shield_with("||ads.example.net^\n||news.com/ads/\n||imasdk.example^$redirect=noop.js\nnews.com##.sponsored\n##.ad-banner");
        let page = "https://news.com/story";
        assert_eq!(s.check("https://ads.example.net/b.js", page, "script"), Verdict::Block);
        assert_eq!(s.check("https://cdn.news.com/app.js", page, "script"), Verdict::Allow);
        // A site's own requests meet only the first-party list.
        assert_eq!(s.check("https://news.com/ads/x.js", page, "script"), Verdict::Allow);
        assert_eq!(s.check("https://news.com/firstparty-ad.js", page, "script"), Verdict::Block);
        // Redirect rules hand back a working stub instead of a failure.
        assert!(matches!(s.check("https://imasdk.example/ima3.js", page, "script"), Verdict::Redirect(_)));
        // Cosmetics: site rules in the page script, generic ones on request.
        let (js, _) = s.page_script(page).unwrap();
        assert!(js.contains(".sponsored"));
        let css = s.generic_css(&["ad-banner".into(), "story".into()], &[], &HashSet::new()).unwrap();
        assert!(css.contains(".ad-banner") && !css.contains("story"));
        // Shields down for this site, or everywhere.
        s.off.insert("news.com".into());
        assert_eq!(s.check("https://ads.example.net/b.js", page, "script"), Verdict::Allow);
        assert!(s.page_script(page).is_none());
        s.off.clear();
        s.on = false;
        assert_eq!(s.check("https://ads.example.net/b.js", page, "script"), Verdict::Allow);
    }

    #[test]
    fn stubs() {
        assert_eq!(stub("data:application/javascript;base64,KCk7"), (b"();".to_vec(), "application/javascript".into()));
        assert_eq!(stub("data:text/plain,hi"), (b"hi".to_vec(), "text/plain".into()));
    }

    /// Against the real lists on this machine: `cargo test -- --ignored`.
    #[test]
    #[ignore]
    fn real_lists() {
        let shared = Shield::load(true, HashSet::new(), true);
        let s = shared.borrow();
        assert!(s.ready(), "lists not downloaded yet: run the app once");
        let page = "https://www.theverge.com/";
        for ad in [
            "https://securepubads.g.doubleclick.net/tag/js/gpt.js",
            "https://pagead2.googlesyndication.com/pagead/js/adsbygoogle.js",
            "https://www.google-analytics.com/analytics.js",
            "https://static.criteo.net/js/ld/publishertag.js",
            "https://cdn.taboola.com/libtrc/x/loader.js",
        ] {
            assert_ne!(s.check(ad, page, "script"), Verdict::Allow, "should block {ad}");
        }
        for ok in ["https://www.theverge.com/app.js", "https://cdn.jsdelivr.net/npm/react@18.3.1/umd/react.production.min.js"] {
            assert_eq!(s.check(ok, page, "script"), Verdict::Allow, "should allow {ok}");
        }
        // Google's own pings stay (blocking them is what trips Google's robot check).
        let g = "https://www.google.com/search?q=github";
        for ok in ["https://www.google.com/gen_204?atyp=i", "https://www.google.com/recaptcha/api.js", "https://www.google.com/log?format=json"] {
            assert_eq!(s.check(ok, g, "xhr"), Verdict::Allow, "should allow {ok}");
        }
        // YouTube gets its scriptlets, which is what takes its video ads away.
        let (yt, _) = s.page_script("https://www.youtube.com/watch?v=x").unwrap();
        assert!(yt.len() > 2000, "YouTube scriptlets missing");
    }
}
