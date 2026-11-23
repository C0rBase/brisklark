//! The ad blocker: Brave's own engine (adblock-rust) running EasyList and
//! EasyPrivacy. Requests to ads and trackers are refused before they leave the
//! machine, and each site's known ad slots are hidden before the page draws.

use adblock::{lists::ParseOptions, request::Request, Engine, FilterSet};
use std::{cell::RefCell, fs, os::windows::process::CommandExt, path::PathBuf, process::Command, rc::Rc, time::Duration};
use webview2_com::{
    take_pwstr, AddScriptToExecuteOnDocumentCreatedCompletedHandler, Microsoft::Web::WebView2::Win32::*,
    NavigationStartingEventHandler, WebResourceRequestedEventHandler,
};
use windows::core::{Interface, HSTRING, PWSTR, w};
use wry::{WebView, WebViewExtWindows};

const LISTS: &[&str] = &[
    "https://easylist.to/easylist/easylist.txt",
    "https://easylist.to/easylist/easyprivacy.txt",
];
const STALE: Duration = Duration::from_secs(4 * 24 * 3600); // lists change daily; four days is plenty

/// Used only until the lists have been downloaded once: the Mac app's own
/// list of domains whose only job is to watch or to sell.
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
    "intergient.com", "btloader.com", "ad-delivery.net", "mentorsticks.com",
    "dns-finder.com", "confiant-integrations.net", "cloudflareinsights.com",
];

/// Shared by every tab; `None` until a compiled engine is on disk.
pub type Shared = Rc<RefCell<Option<Engine>>>;

fn cache() -> PathBuf {
    crate::dir("LOCALAPPDATA").join("adblock.dat")
}

pub fn load() -> Shared {
    let engine = fs::read(cache()).ok().and_then(|bytes| {
        let mut e = Engine::default();
        e.deserialize(&bytes).ok().map(|_| e)
    });
    Rc::new(RefCell::new(engine))
}

pub fn install(shared: &Shared, bytes: &[u8]) {
    let mut e = Engine::default();
    if e.deserialize(bytes).is_ok() {
        *shared.borrow_mut() = Some(e);
    }
}

/// Downloads and compiles the lists off the UI thread when they are missing or
/// old, then hands back the compiled engine as bytes (the engine itself stays
/// on one thread). Uses Windows' own curl.exe rather than an HTTP library.
pub fn refresh(done: impl FnOnce(Vec<u8>) + Send + 'static) {
    let fresh = fs::metadata(cache())
        .and_then(|m| m.modified())
        .is_ok_and(|t| t.elapsed().unwrap_or(STALE) < STALE);
    if fresh {
        return;
    }
    std::thread::spawn(move || {
        let mut set = FilterSet::new(false);
        for url in LISTS {
            let out = Command::new("curl.exe")
                .args(["-sfL", "--max-time", "60", url])
                .creation_flags(0x0800_0000) // CREATE_NO_WINDOW
                .output();
            match out {
                Ok(o) if o.status.success() && !o.stdout.is_empty() => {
                    set.add_filter_list(String::from_utf8_lossy(&o.stdout).into_owned(), ParseOptions::default());
                }
                _ => return, // offline: keep what we have, try next launch
            }
        }
        let bytes = Engine::new_with_filter_set(set).serialize();
        let path = cache();
        let tmp = path.with_extension("tmp");
        if fs::write(&tmp, &bytes).is_ok() {
            let _ = fs::rename(tmp, path);
        }
        done(bytes);
    });
}

fn host(url: &str) -> &str {
    crate::host(url)
}

fn under(host: &str, domain: &str) -> bool {
    host.strip_suffix(domain).is_some_and(|r| r.is_empty() || r.ends_with('.'))
}

/// Whether a request should be refused. `top` is the address of the page in the tab.
/// As Brave's Standard mode: only cross-site requests are ever blocked. A
/// site's requests to itself are the site; refusing Google's own pings on
/// google.com is what makes Google take the browser for a robot.
pub fn blocks(engine: Option<&Engine>, url: &str, top: &str, kind: &str) -> bool {
    match engine {
        Some(e) => Request::new(url, top, kind, "get")
            .is_ok_and(|r| r.is_third_party && e.check_network_request(&r).should_block()),
        None => {
            let (h, t) = (host(url).to_ascii_lowercase(), host(top).to_ascii_lowercase());
            FALLBACK.iter().any(|d| under(&h, d) && !under(&t, d))
        }
    }
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

/// CSS that hides this site's known ad slots, as a script that adds it the
/// moment the document exists, before anything is drawn.
fn hiding_script(engine: &Engine, url: &str) -> Option<String> {
    let res = engine.url_cosmetic_resources(url);
    let selectors: Vec<&str> = res
        .hide_selectors
        .iter()
        .filter(|s| !res.exceptions.contains(*s))
        .map(String::as_str)
        .collect();
    if selectors.is_empty() {
        return None;
    }
    let css = format!("{}{{display:none!important}}", selectors.join(","));
    Some(format!(
        "(()=>{{const css={};const add=()=>{{const s=document.createElement('style');s.textContent=css;\
         (document.head||document.documentElement).appendChild(s)}};\
         if(document.documentElement)add();else new MutationObserver((_,o)=>{{if(document.documentElement)\
         {{o.disconnect();add()}}}}).observe(document,{{childList:true}})}})()",
        serde_json::to_string(&css).unwrap_or_default()
    ))
}

/// Puts the blocker on one tab.
pub fn attach(view: &WebView, shared: Shared, top: Rc<RefCell<String>>) {
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
        let (engine, page) = (shared.clone(), top.clone());
        let requested = WebResourceRequestedEventHandler::create(Box::new(move |_, args| {
            let Some(args) = args else { return Ok(()) };
            let mut uri = PWSTR::null();
            args.Request()?.Uri(&mut uri)?;
            let uri = take_pwstr(uri);
            let top = page.borrow();
            if uri == *top || !uri.starts_with("http") {
                return Ok(()); // the page itself, or data:/blob: and the like
            }
            let mut ctx = COREWEBVIEW2_WEB_RESOURCE_CONTEXT::default();
            args.ResourceContext(&mut ctx)?;
            if blocks(engine.borrow().as_ref(), &uri, &top, kind(ctx)) {
                args.SetResponse(&env.CreateWebResourceResponse(None, 403, w!("Blocked"), w!(""))?)?;
            }
            Ok(())
        }));
        let mut token = 0i64;
        let _ = core.add_WebResourceRequested(&requested, &mut token);

        // Each page gets its own site's hiding rules, swapped in as it starts.
        let starting = NavigationStartingEventHandler::create(Box::new(move |sender, args| {
            let (Some(core), Some(args)) = (sender, args) else { return Ok(()) };
            let mut uri = PWSTR::null();
            args.Uri(&mut uri)?;
            let uri = take_pwstr(uri);
            *top.borrow_mut() = uri.clone();
            if let Some(old) = script_id.borrow_mut().take() {
                let _ = core.RemoveScriptToExecuteOnDocumentCreated(&HSTRING::from(old));
            }
            let script = shared.borrow().as_ref().and_then(|e| hiding_script(e, &uri));
            if let Some(script) = script {
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
            }
            Ok(())
        }));
        let _ = core.add_NavigationStarting(&starting, &mut token);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fallback_list() {
        assert!(blocks(None, "https://stats.g.doubleclick.net/x.js", "https://news.com/", "script"));
        assert!(!blocks(None, "https://notdoubleclick.net/x.js", "https://news.com/", "script"));
        assert!(!blocks(None, "https://www.hotjar.com/a.js", "https://www.hotjar.com/", "script"));
    }

    #[test]
    fn engine_rules_and_hiding() {
        let mut set = FilterSet::new(false);
        set.add_filter_list("||ads.example.net^
news.com##.sponsored
##.ad-banner".into(), ParseOptions::default());
        let e = Engine::new_with_filter_set(set);
        let mut round = Engine::default(); // the on-disk cache round-trips
        round.deserialize(&e.serialize()).unwrap();
        assert!(blocks(Some(&round), "https://ads.example.net/b.js", "https://news.com/", "script"));
        assert!(!blocks(Some(&round), "https://cdn.news.com/app.js", "https://news.com/", "script"));
        let js = hiding_script(&round, "https://news.com/").unwrap();
        assert!(js.contains(".sponsored"));
    }
}

/// Against the real lists on this machine: `cargo test -- --ignored`.
#[cfg(test)]
#[test]
#[ignore]
fn real_lists() {
    let shared = load();
    let e = shared.borrow();
    let e = e.as_ref().expect("lists not downloaded yet: run the app once");
    let page = "https://www.theverge.com/";
    for ad in [
        "https://securepubads.g.doubleclick.net/tag/js/gpt.js",
        "https://pagead2.googlesyndication.com/pagead/js/adsbygoogle.js",
        "https://www.google-analytics.com/analytics.js",
        "https://static.criteo.net/js/ld/publishertag.js",
        "https://cdn.taboola.com/libtrc/x/loader.js",
    ] {
        assert!(blocks(Some(e), ad, page, "script"), "should block {ad}");
    }
    for ok in ["https://www.theverge.com/app.js", "https://cdn.jsdelivr.net/npm/react@18.3.1/umd/react.production.min.js"] {
        assert!(!blocks(Some(e), ok, page, "script"), "should allow {ok}");
    }
    assert!(hiding_script(e, "https://www.reddit.com/").is_some());
}

#[cfg(test)]
#[test]
#[ignore]
fn probe_google() {
    let shared = load();
    let e = shared.borrow();
    let e = e.as_ref().unwrap();
    let page = "https://www.google.com/search?q=github";
    for (u, k) in [
        ("https://www.google.com/xjs/_/js/k=xjs.s.en/m=cdos/rt=j", "script"),
        ("https://www.gstatic.com/og/_/js/k=og.qtm.en/rt=j", "script"),
        ("https://www.google.com/gen_204?atyp=i&ei=x", "ping"),
        ("https://www.google.com/gen_204?atyp=i&ei=x", "image"),
        ("https://www.google.com/client_204?atyp=i", "image"),
        ("https://www.google.com/log?format=json", "xhr"),
        ("https://www.google.com/async/bgasy?ei=x", "xhr"),
        ("https://www.google.com/complete/search?q=g", "xhr"),
        ("https://www.google.com/recaptcha/api.js", "script"),
        ("https://www.gstatic.com/recaptcha/releases/x/recaptcha__en.js", "script"),
        ("https://www.google.com/recaptcha/api2/anchor?k=x", "sub_frame"),
        ("https://play.google.com/log?format=json", "xhr"),
        ("https://ogs.google.com/widget/app", "sub_frame"),
        ("https://apis.google.com/js/api.js", "script"),
        ("https://www.google.com/sorry/index?continue=x", "sub_frame"),
        ("https://consent.google.com/save", "xhr"),
        ("https://www.googleadservices.com/pagead/aclk?sa=L", "sub_frame"),
    ] {
        println!("{:5} {k:9} {u}", if blocks(Some(e), u, page, k) { "BLOCK" } else { "ok" });
    }
}
