//! Shields: the ad blocker and Brave's other protections, set up the way Brave
//! ships them. Brave's own engine (adblock-rust) runs the lists Brave's catalog
//! turns on, in Brave's two engines; uBlock Origin's scriptlets and Brave's
//! resources answer the scriptlet and redirect rules. Around it: tracking
//! parameters stripped, bounce trackers skipped, HTTPS by default, Global
//! Privacy Control, a capped referrer, and fingerprinting protection.

use adblock::{
    blocker::BlockerResult,
    lists::ParseOptions,
    request::Request,
    resources::{MimeType, PermissionMask, Resource, ResourceType},
    Engine, FilterSet,
};
use base64::Engine as _;
use serde::{Deserialize, Serialize};
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
    take_pwstr, AddScriptToExecuteOnDocumentCreatedCompletedHandler,
    Microsoft::Web::WebView2::Win32::*, NavigationCompletedEventHandler, NavigationStartingEventHandler,
    WebMessageReceivedEventHandler, WebResourceRequestedEventHandler,
};
use windows::{
    core::{Interface, BOOL, HSTRING, PWSTR, w},
    Win32::UI::Shell::SHCreateMemStream,
};
use wry::{WebView, WebViewExtWindows};

const CATALOG: &str = "https://raw.githubusercontent.com/brave/adblock-resources/master/filter_lists/list_catalog.json";
const RESOURCES: &str = "https://raw.githubusercontent.com/brave/adblock-resources/master/dist/resources.json";
const BL: &str = "https://raw.githubusercontent.com/brave/adblock-lists/master/brave-lists/";
/// Brave's data files for its network protections, kept next to the lists.
const DATA: &[&str] = &["query-filter.json", "debounce.json", "https-upgrade-exceptions-list.txt", "webcompat-exceptions.json"];
/// uBlock Origin's own scriptlets and redirect stubs. Brave's resources.json
/// holds only Brave's extras; Brave's build adds these from uBO's source, which
/// is what YouTube's rules (json-prune, trusted-replace-xhr-response, …) need.
const UBO_SRC: &str = "https://cdn.jsdelivr.net/gh/gorhill/uBlock@master/src/";
const STALE: Duration = Duration::from_secs(4 * 24 * 3600); // lists change daily; four days is plenty

/// Lists Brave turns on that aren't marked default in its catalog: the
/// Facebook and X embed exceptions (Brave's embed settings default to allow).
const EMBEDS: &[&str] = &["A5E6EC21-F01F-4547-9F0A-1EE1C3F2AE8D", "84960ADD-1CC1-419F-81FC-9F116F5205CC"];
/// Cookie notices: its own switch in Settings.
const COOKIE_LIST: &str = "AC023D22-AE88-4060-A978-4FEEEC4221693";

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

// ---------- settings ----------

/// Shields settings beyond the on/off switches, kept in state.json.
#[derive(Serialize, Deserialize, Clone, PartialEq)]
#[serde(default)]
pub struct Prefs {
    /// Brave's "Aggressive" blocking: the main lists apply to a site's own requests too.
    pub aggressive: bool,
    pub fingerprint: bool,
    pub https: bool,
    /// Strip tracking parameters (fbclid, gclid, …) from links to other sites.
    pub query: bool,
    /// Skip bounce-tracking redirects (debouncing).
    pub debounce: bool,
    pub gpc: bool,
    /// Optional lists turned on, and default ones turned off, by catalog uuid.
    pub lists_on: Vec<String>,
    pub lists_off: Vec<String>,
}

impl Default for Prefs {
    fn default() -> Self {
        Prefs {
            aggressive: false, fingerprint: true, https: true, query: true, debounce: true, gpc: true,
            lists_on: vec![], lists_off: vec![],
        }
    }
}

// ---------- the list catalog ----------

#[derive(Deserialize, Clone)]
pub struct Entry {
    pub uuid: String,
    pub title: String,
    #[serde(default)]
    pub desc: String,
    #[serde(default)]
    pub langs: Vec<String>,
    #[serde(default)]
    pub hidden: bool,
    #[serde(default)]
    pub default_enabled: bool,
    #[serde(default)]
    first_party_protections: bool,
    #[serde(default)]
    permission_mask: u8,
    #[serde(default)]
    platforms: Vec<String>,
    sources: Vec<Src>,
}
#[derive(Deserialize, Clone)]
struct Src {
    url: String,
    #[serde(default)]
    title: String,
}

/// Brave's catalog as last downloaded, or the default lists built in.
pub fn catalog() -> Vec<Entry> {
    fs::read(folder().join("catalog.json"))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_else(|| serde_json::from_str(include_str!("catalog.json")).unwrap_or_default())
}

/// The language Windows shows itself in ("en" for en-ZA), for regional lists.
pub fn language() -> String {
    use windows::Win32::Globalization::GetUserDefaultLocaleName;
    let mut buf = [0u16; 85]; // LOCALE_NAME_MAX_LENGTH
    let n = unsafe { GetUserDefaultLocaleName(&mut buf) } as usize;
    let name = String::from_utf16_lossy(&buf[..n.saturating_sub(1)]);
    name.split('-').next().unwrap_or("").to_ascii_lowercase()
}

/// Which lists to build with: what the catalog turns on by default (plus the
/// regional list for the Windows language), adjusted by the person's choices.
#[derive(Clone, PartialEq)]
pub struct Config {
    pub cookies: bool,
    pub lists_on: Vec<String>,
    pub lists_off: Vec<String>,
}

impl Config {
    pub fn chosen<'a>(&self, cat: &'a [Entry]) -> Vec<&'a Entry> {
        let lang = language();
        cat.iter()
            .filter(|e| e.platforms.is_empty())
            .filter(|e| {
                if e.uuid == COOKIE_LIST {
                    return self.cookies;
                }
                let default = e.default_enabled || EMBEDS.contains(&e.uuid.as_str()) || e.langs.contains(&lang);
                if default { !self.lists_off.contains(&e.uuid) } else { self.lists_on.contains(&e.uuid) }
            })
            .collect()
    }
    /// What a build with these lists is called, to tell whether it's current.
    pub fn key(&self) -> String {
        let cat = catalog();
        let mut ids: Vec<&str> = self.chosen(&cat).iter().map(|e| e.uuid.as_str()).collect();
        ids.sort();
        ids.join(",")
    }
    /// Optional lists for Settings: everything a person may switch.
    pub fn options(&self) -> Vec<serde_json::Value> {
        let cat = catalog();
        let on: HashSet<&str> = self.chosen(&cat).iter().map(|e| e.uuid.as_str()).collect();
        cat.iter()
            .filter(|e| !e.hidden && e.platforms.is_empty() && e.uuid != COOKIE_LIST)
            .map(|e| serde_json::json!({"uuid": e.uuid, "title": e.title, "desc": e.desc, "on": on.contains(e.uuid.as_str()),
                                        "regional": !e.langs.is_empty()}))
            .collect()
    }
}

// ---------- the blocker ----------

/// As in Brave and uBO: only lists the catalog trusts (uBO's and Brave's own)
/// may use the "trusted" scriptlets, which can rewrite a page's network responses.
const TRUST: PermissionMask = PermissionMask::from_bits(1);

#[derive(Default)]
pub struct Shield {
    /// Brave's two engines: lists with first-party protections (Default,
    /// Privacy, some regionals) and the rest (First Party, cookie notices, …).
    a: Option<Engine>,
    b: Option<Engine>,
    /// What the engines were built from (Config::key).
    pub key: String,
    /// Shields up at all (Settings › Shields).
    pub on: bool,
    /// Sites the person switched Shields off for.
    pub off: HashSet<String>,
    pub prefs: Prefs,
    /// Brave's resources.json as last installed, to merge uBO's into again.
    brave: Vec<u8>,
    data: Data,
    /// Hosts whose HTTPS failed this session: left on http, as Brave does.
    fellback: HashSet<String>,
    /// Random for each run: fingerprinting seeds change every session.
    session: [u64; 2],
}
pub type Shared = Rc<RefCell<Shield>>;

/// What one tab knows about its page, for the blocker.
#[derive(Clone, Default)]
pub struct Page {
    pub url: Rc<RefCell<String>>,
    pub blocked: Rc<Cell<u32>>,
    exceptions: Rc<RefCell<HashSet<String>>>,
    /// The http address an HTTPS upgrade came from, until the page loads.
    upgraded: Rc<RefCell<Option<String>>>,
    /// Hosts this navigation was debounced to, so two trackers can't loop.
    bounced: Rc<RefCell<Vec<String>>>,
    /// The next navigation is one Shields started itself.
    ours: Rc<Cell<bool>>,
}

/// A freshly compiled set, handed over from the download thread as bytes.
pub struct Compiled {
    a: Vec<u8>,
    b: Vec<u8>,
    resources: Vec<u8>,
    pub key: String,
}

fn folder() -> PathBuf {
    crate::dir("LOCALAPPDATA").join("Shields")
}
fn ubo_file() -> PathBuf {
    folder().join("ubo.json")
}

fn engine_from(bytes: &[u8]) -> Option<Engine> {
    let mut e = Engine::default();
    e.deserialize(bytes).ok().map(|_| e)
}

/// uBO's resources as kept on disk.
#[derive(Serialize, Deserialize, Default)]
struct Ubo {
    redirects: Vec<Resource>,
    scriptlets: Vec<Resource>,
}

/// What the hidden page that reads uBO's source sends back.
#[derive(Deserialize)]
struct UboJs {
    scriptlets: Vec<UboScriptlet>,
    redirects: Vec<UboRedirect>,
}
#[derive(Deserialize)]
struct UboScriptlet {
    name: String,
    #[serde(default)]
    aliases: Vec<String>,
    #[serde(rename = "fn")]
    code: String,
    #[serde(default)]
    deps: Vec<String>,
    #[serde(default)]
    trust: bool,
}
#[derive(Deserialize)]
struct UboRedirect {
    name: String,
    #[serde(default)]
    alias: serde_json::Value, // a name or a list of names
    content: String,          // base64
}

/// Brave's order: uBO's redirect stubs, Brave's own, then uBO's scriptlets.
/// A later resource with the same name replaces an earlier one.
fn merged(brave: &[u8]) -> Vec<Resource> {
    let ubo: Ubo = fs::read(ubo_file()).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
    let brave: Vec<Resource> = serde_json::from_slice(brave).unwrap_or_default();
    let mut all: Vec<Resource> = vec![];
    for r in ubo.redirects.into_iter().chain(brave).chain(ubo.scriptlets) {
        all.retain(|o| o.name != r.name);
        all.push(r);
    }
    all
}

/// Turns the hidden page's report into adblock-rust resources.
fn ubo_from_js(json: &str) -> Option<Ubo> {
    let js: UboJs = serde_json::from_str(json).ok()?;
    let b64 = |s: &str| base64::engine::general_purpose::STANDARD.encode(s);
    let scriptlets: Vec<Resource> = js
        .scriptlets
        .into_iter()
        .filter(|s| s.code.starts_with("function"))
        .map(|s| Resource {
            // ".fn" entries are helpers other scriptlets depend on, never injected alone.
            kind: ResourceType::Mime(if s.name.ends_with(".fn") { MimeType::FnJavascript } else { MimeType::ApplicationJavascript }),
            content: b64(&s.code),
            aliases: s.aliases,
            dependencies: s.deps,
            permission: if s.trust { TRUST } else { PermissionMask::default() },
            name: s.name,
        })
        .collect();
    let redirects = js
        .redirects
        .into_iter()
        .map(|r| {
            let aliases = match r.alias {
                serde_json::Value::String(a) => vec![a],
                serde_json::Value::Array(a) => a.into_iter().filter_map(|v| v.as_str().map(String::from)).collect(),
                _ => vec![],
            };
            let mime = match MimeType::from_extension(&r.name) {
                MimeType::Unknown => MimeType::TextPlain, // "empty"
                m => m,
            };
            Resource { kind: ResourceType::Mime(mime), content: r.content, aliases, dependencies: vec![], permission: Default::default(), name: r.name }
        })
        .collect();
    // A half-loaded report would take working scriptlets away: keep the old ones.
    (scriptlets.len() >= 50).then_some(Ubo { redirects, scriptlets })
}

/// The page a hidden WebView loads to read uBO's scriptlets, as Brave's build
/// does with Node: import the modules and hand back each function's source.
pub fn ubo_page() -> String {
    format!(
        r#"<!doctype html><script type="module">
const base = {base};
const b64 = buf => {{ const a = new Uint8Array(buf); let s = ''; for (let i = 0; i < a.length; i += 0x8000) s += String.fromCharCode.apply(null, a.subarray(i, i + 0x8000)); return btoa(s); }};
try {{
  const {{ builtinScriptlets }} = await import(base + 'js/resources/scriptlets.js');
  const scriptlets = builtinScriptlets.map(s => ({{ name: s.name, aliases: s.aliases || [], fn: s.fn.toString(),
    deps: (s.dependencies || []).map(d => typeof d === 'function' ? d.details.name : d), trust: !!s.requiresTrust }}));
  const map = (await import(base + 'js/redirect-resources.js')).default;
  const redirects = [];
  await Promise.all([...map].filter(([, d]) => !d.params).map(async ([name, d]) => {{
    try {{ const r = await fetch(base + 'web_accessible_resources/' + name); if (r.ok) redirects.push({{ name, alias: d.alias ?? null, content: b64(await r.arrayBuffer()) }}); }} catch {{}}
  }}));
  window.ipc.postMessage(JSON.stringify({{ scriptlets, redirects }}));
}} catch (e) {{ window.ipc.postMessage(JSON.stringify({{ error: String(e) }})); }}
</script>"#,
        base = serde_json::to_string(UBO_SRC).unwrap_or_default()
    )
}

/// uBO's resources are missing or as old as the lists get.
pub fn ubo_stale() -> bool {
    !fs::metadata(ubo_file()).and_then(|m| m.modified()).is_ok_and(|t| t.elapsed().unwrap_or(STALE) < STALE)
}

/// uBO's scriptlets need this before them, as Brave puts it
/// (components/cosmetic_filters/common/scriptlet_constants.cc).
const PRELUDE: &str = r#"const scriptletGlobals=(()=>{const forwardedMapMethods=["has","get","set"];const handler={get(t,p){if(forwardedMapMethods.includes(p))return Map.prototype[p].bind(t);return t.get(p)},set(t,p,v){if(!forwardedMapMethods.includes(p))t.set(p,v)}};return new Proxy(new Map(),handler)})();let deAmpEnabled=false;"#;

/// The registrable domain ("bbc.co.uk" for "www.bbc.co.uk").
pub fn site(host: &str) -> String {
    let h = host.trim_end_matches('.').to_ascii_lowercase();
    psl::domain_str(&h).map_or(h.clone(), String::from)
}
fn under(host: &str, domain: &str) -> bool {
    host.strip_suffix(domain).is_some_and(|r| r.is_empty() || r.ends_with('.'))
}

impl Shield {
    pub fn load(on: bool, off: HashSet<String>, prefs: Prefs, cfg: &Config) -> Shared {
        use std::hash::{BuildHasher, RandomState};
        let r = RandomState::new();
        let mut s = Shield { on, off, prefs, session: [r.hash_one(1u8), r.hash_one(2u8)], ..Default::default() };
        s.reload(cfg);
        Rc::new(RefCell::new(s))
    }

    /// Loads the compiled lists from disk; false when they're missing or were
    /// built from other lists than `cfg` asks for (a download is due).
    pub fn reload(&mut self, cfg: &Config) -> bool {
        self.data = Data::load();
        let (Ok(a), Ok(b), Ok(resources)) =
            (fs::read(folder().join("a.dat")), fs::read(folder().join("b.dat")), fs::read(folder().join("resources.json")))
        else {
            return false;
        };
        let key = fs::read_to_string(folder().join("key.txt")).unwrap_or_default();
        self.install(Compiled { a, b, resources, key: key.clone() });
        key == cfg.key()
    }

    pub fn install(&mut self, c: Compiled) {
        if let (Some(mut a), Some(mut b)) = (engine_from(&c.a), engine_from(&c.b)) {
            a.use_resources(merged(&c.resources));
            b.use_resources(merged(&c.resources));
            self.brave = c.resources;
            (self.a, self.b, self.key) = (Some(a), Some(b), c.key);
            self.data = Data::load();
        }
    }

    /// uBO's scriptlets, freshly read by the hidden page: kept, and put to use.
    /// False when the report was unusable (offline, or uBO's source moved).
    pub fn set_ubo(&mut self, json: &str) -> bool {
        let Some(ubo) = ubo_from_js(json) else { return false };
        let _ = fs::create_dir_all(folder());
        let tmp = ubo_file().with_extension("tmp");
        if fs::write(&tmp, serde_json::to_vec(&ubo).unwrap_or_default()).is_err() || fs::rename(&tmp, ubo_file()).is_err() {
            return false;
        }
        for e in [&mut self.a, &mut self.b].into_iter().flatten() {
            e.use_resources(merged(&self.brave));
        }
        true
    }

    #[cfg(test)]
    pub fn ready(&self) -> bool {
        self.a.is_some()
    }

    /// Shields are up for the page at this address.
    pub fn up_for(&self, page: &str) -> bool {
        self.on && !self.off.contains(&crate::host(page).to_ascii_lowercase())
    }

    /// Brave forces Aggressive on YouTube, whose ads come from its own domain.
    fn aggressive(&self, page: &str) -> bool {
        self.prefs.aggressive
    }

    /// Brave's check across its two engines (AdBlockEngineWrapper::ShouldStartRequest).
    pub fn check(&self, url: &str, page: &str, kind: &str, method: &str) -> Verdict {
        if !self.up_for(page) {
            return Verdict::Allow;
        }
        let (Some(a), Some(b)) = (&self.a, &self.b) else {
            let (h, t) = (crate::host(url).to_ascii_lowercase(), crate::host(page).to_ascii_lowercase());
            return if FALLBACK.iter().any(|d| under(&h, d) && !under(&t, d)) { Verdict::Block } else { Verdict::Allow };
        };
        let Ok(req) = Request::new(url, page, kind, method) else { return Verdict::Allow };
        let mut first = a.check_network_request_subset(&req, false, true);
        if self.aggressive(page) || req.is_third_party {
            if first.important {
                return verdict(&first);
            }
        } else {
            // A site's own requests: the main lists may only make exceptions.
            first = BlockerResult { exception: first.exception, ..Default::default() };
        }
        let r = b.check_network_request_subset(&req, first.filter.is_some() || first.exception.is_some(), first.exception.is_none());
        verdict(&BlockerResult {
            important: first.important || r.important,
            filter: r.filter.or(first.filter),
            exception: r.exception.or(first.exception),
            redirect: r.redirect.or(first.redirect),
            rewritten_url: None,
        })
    }

    /// A new window (a pop-up or pop-under) to an address the lists block.
    pub fn popup_blocked(&self, url: &str, opener: &str) -> bool {
        url.starts_with("http")
            && site(crate::host(url)) != site(crate::host(opener))
            && matches!(self.check(url, opener, "sub_frame", "get"), Verdict::Block | Verdict::Redirect(_))
    }

    /// The script a page gets before its own: Global Privacy Control,
    /// fingerprinting protection, this site's scriptlets and hiding rules, and
    /// (unless the lists say not to) the scanner that asks for generic hiding
    /// rules as classes and ids appear.
    pub fn page_script(&self, url: &str) -> Option<(String, HashSet<String>)> {
        if !url.starts_with("http") || !self.up_for(url) {
            return None;
        }
        let mut js = String::new();
        if self.prefs.gpc {
            js += "Object.defineProperty(Navigator.prototype,'globalPrivacyControl',{get:()=>true,enumerable:true,configurable:true});\n";
        }
        if self.prefs.fingerprint {
            js += &self.farble(url);
        }
        if under(&crate::host(url).to_ascii_lowercase(), "youtube.com") {
            js += YT_SKIP;
        }
        let (Some(a), Some(b)) = (&self.a, &self.b) else { return Some((js, HashSet::new())) };
        let ra = a.url_cosmetic_resources(url);
        let rb = b.url_cosmetic_resources(url);
        let exceptions: HashSet<String> = ra.exceptions.union(&rb.exceptions).cloned().collect();
        // In Standard mode the main lists' :has() rules could hide a site's own content.
        let strict = self.aggressive(url);
        let selectors: Vec<&String> = ra.hide_selectors.iter().filter(|s| strict || !s.contains(":has("))
            .chain(&rb.hide_selectors)
            .filter(|s| !exceptions.contains(*s))
            .collect();
        let scriptlets = [ra.injected_script.as_str(), rb.injected_script.as_str()].concat();
        if !scriptlets.trim().is_empty() {
            js += &format!("(function(){{{PRELUDE}\n{scriptlets}\n}})();\n");
        }
        js += &format!(
            "(()=>{{const sels={sels};const generic={generic};\
const add=list=>{{const s=document.createElement('style');(document.head||document.documentElement).appendChild(s);\
for(const x of list)try{{s.sheet.insertRule(x+'{{display:none!important}}',s.sheet.cssRules.length)}}catch(e){{}}}};\
const ready=f=>document.documentElement?f():new MutationObserver((_,o)=>{{if(document.documentElement){{o.disconnect();f()}}}}).observe(document,{{childList:true}});\
if(sels.length)ready(()=>add(sels));\
if(!(window.chrome&&chrome.webview))return;\
chrome.webview.addEventListener('message',e=>{{if(e.data&&e.data.brisklark==='hide')ready(()=>add(e.data.sels))}});\
if(!generic||window.top!==window)return;\
const sc=new Set(),si=new Set();let qc=[],qi=[],t=0;\
const flush=()=>{{t=0;if(qc.length||qi.length){{chrome.webview.postMessage({{brisklark:'cosmetic',c:qc,i:qi}});qc=[];qi=[]}}}};\
const take=e=>{{if(e.id&&!si.has(e.id)){{si.add(e.id);qi.push(e.id)}}if(e.classList)for(const c of e.classList)if(!sc.has(c)){{sc.add(c);qc.push(c)}}}};\
const scan=r=>{{if(r.nodeType!==1)return;take(r);for(const e of r.querySelectorAll('[id],[class]'))take(e)}};\
const soon=()=>{{if(!t)t=setTimeout(flush,100)}};\
ready(()=>{{scan(document.documentElement);soon();new MutationObserver(ms=>{{for(const m of ms){{if(m.type==='attributes')take(m.target);for(const n of m.addedNodes)scan(n)}}soon()}})\
.observe(document.documentElement,{{childList:true,subtree:true,attributes:true,attributeFilter:['class','id']}})}})}})()",
            sels = serde_json::to_string(&selectors).unwrap_or_default(),
            generic = !(ra.generichide || rb.generichide),
        );
        Some((js, exceptions))
    }

    /// Generic hiding rules for classes and ids seen on the page, from both engines.
    fn generic(&self, classes: &[String], ids: &[String], exceptions: &HashSet<String>) -> Vec<String> {
        [&self.a, &self.b].into_iter().flatten().flat_map(|e| e.hidden_class_id_selectors(classes, ids, exceptions)).collect()
    }

    /// Fingerprinting protection, seeded for this site and this session.
    fn farble(&self, url: &str) -> String {
        let site = site(crate::host(url));
        let mut msg = Vec::from(self.session[0].to_le_bytes());
        msg.extend(self.session[1].to_le_bytes());
        msg.extend(site.as_bytes());
        let seed: String = crate::sha::sha256(&msg).iter().map(|b| format!("{b:02x}")).collect();
        let off: Vec<&str> = self.data.compat.iter()
            .filter(|c| c.include.iter().any(|p| pattern(p, url)))
            .flat_map(|c| c.exceptions.iter().map(String::as_str))
            .collect();
        include_str!("farble.js").replace("__SEED__", &seed).replace("__OFF__", &serde_json::to_string(&off).unwrap_or("[]".into()))
    }

    /// Where a navigation should go instead, if anywhere: past a bounce
    /// tracker, without tracking parameters, or over HTTPS.
    fn rewrite(&self, url: &str, prev: &str, redirected: bool, page: &Page) -> Option<String> {
        if !url.starts_with("http") || !self.up_for(url) {
            return None;
        }
        if self.prefs.debounce {
            if let Some(to) = debounce(&self.data.bounce, url) {
                let h = crate::host(&to).to_ascii_lowercase();
                let mut seen = page.bounced.borrow_mut();
                if !seen.contains(&h) {
                    seen.push(h);
                    return Some(to);
                }
            }
        }
        if self.prefs.query {
            if let Some(to) = strip_query(&self.data.query, url, prev, redirected) {
                return Some(to);
            }
        }
        if self.prefs.https && url.starts_with("http://") {
            let h = crate::host(url).to_ascii_lowercase();
            let local = !h.contains('.') || h.parse::<std::net::IpAddr>().is_ok() || h.starts_with('[')
                || [".local", ".onion", ".lan", ".internal", ".home.arpa", ".localhost", ".test"].iter().any(|s| h.ends_with(s));
            if !local && !self.data.https_exceptions.contains(&h) && !self.fellback.contains(&h) {
                *page.upgraded.borrow_mut() = Some(url.into());
                return Some(format!("https://{}", &url[7..]));
            }
        }
        None
    }
}

fn verdict(r: &BlockerResult) -> Verdict {
    match (r.important || (r.filter.is_some() && r.exception.is_none()), &r.redirect) {
        (true, Some(stub)) => Verdict::Redirect(stub.clone()),
        (true, None) => Verdict::Block,
        _ => Verdict::Allow,
    }
}

#[derive(Debug, PartialEq)]
pub enum Verdict {
    Allow,
    Block,
    Redirect(String),
}

// ---------- Brave's network privacy data ----------

#[derive(Deserialize, Default)]
struct QueryRule {
    include: Vec<String>,
    #[serde(default)]
    exclude: Vec<String>,
    params: Vec<String>,
}
#[derive(Deserialize, Default)]
struct BounceRule {
    include: Vec<String>,
    #[serde(default)]
    exclude: Vec<String>,
    action: String,
    param: String,
    #[serde(default)]
    prepend_scheme: Option<String>,
    #[serde(default)]
    redirect_url_template: Option<String>,
}
#[derive(Deserialize, Default)]
struct Compat {
    include: Vec<String>,
    exceptions: Vec<String>,
}

#[derive(Default)]
struct Data {
    query: Vec<QueryRule>,
    bounce: Vec<BounceRule>,
    https_exceptions: HashSet<String>,
    compat: Vec<Compat>,
}

impl Data {
    fn load() -> Data {
        let json = |n: &str| fs::read(folder().join(n)).ok();
        Data {
            query: json("query-filter.json").and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default(),
            bounce: json("debounce.json").and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default(),
            https_exceptions: fs::read_to_string(folder().join("https-upgrade-exceptions-list.txt"))
                .unwrap_or_default()
                .lines()
                .map(|l| l.trim().to_ascii_lowercase())
                .filter(|l| !l.is_empty() && !l.starts_with('#'))
                .collect(),
            compat: json("webcompat-exceptions.json").and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default(),
        }
    }
}

/// An address split as match patterns see it: scheme, host, and path with query.
fn parts(url: &str) -> (&str, String, &str) {
    let url = url.split('#').next().unwrap_or("");
    let (scheme, rest) = url.split_once("://").unwrap_or(("", url));
    let end = rest.find(['/', '?']).unwrap_or(rest.len());
    (scheme, crate::host(url).to_ascii_lowercase(), &rest[end..])
}

/// A Chrome match pattern ("*://*.example.com/path*") against an address.
fn pattern(p: &str, url: &str) -> bool {
    let Some((ps, rest)) = p.split_once("://") else { return false };
    let (ph, pp) = rest.find('/').map_or((rest, "/*"), |i| (&rest[..i], &rest[i..]));
    let (scheme, host, path) = parts(url);
    let path = if path.starts_with('/') { path.to_string() } else { format!("/{path}") };
    let scheme_ok = if ps == "*" { scheme == "http" || scheme == "https" } else { ps == scheme };
    let ph = ph.to_ascii_lowercase();
    let host_ok = ph == "*" || ph.strip_prefix("*.").map_or(ph == host, |d| under(&host, d));
    scheme_ok && host_ok && glob(pp, &path)
}

/// `*` matches any run of characters.
fn glob(p: &str, s: &str) -> bool {
    let (p, s) = (p.as_bytes(), s.as_bytes());
    let (mut i, mut j, mut star, mut mark) = (0, 0, None, 0);
    while j < s.len() {
        if i < p.len() && p[i] == b'*' {
            star = Some(i);
            mark = j;
            i += 1;
        } else if i < p.len() && p[i] == s[j] {
            i += 1;
            j += 1;
        } else if let Some(st) = star {
            i = st + 1;
            mark += 1;
            j = mark;
        } else {
            return false;
        }
    }
    p[i..].iter().all(|&c| c == b'*')
}

/// %XX escapes decoded.
fn unescape(s: &str) -> String {
    let b = s.as_bytes();
    let hex = |c: u8| (c as char).to_digit(16).map(|d| d as u8);
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let (Some(h), Some(l)) = (hex(b[i + 1]), hex(b[i + 2])) {
                out.push(h * 16 + l);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn query_value(url: &str, key: &str) -> Option<String> {
    let q = url.split('#').next()?.split_once('?')?.1;
    q.split('&').find_map(|kv| kv.split_once('=').filter(|(k, _)| *k == key).map(|(_, v)| unescape(v)))
}

/// Brave's debouncing (components/debounce): the address a bounce tracker
/// would have sent us to, taken straight from the link.
fn debounce(rules: &[BounceRule], url: &str) -> Option<String> {
    let host = crate::host(url).to_ascii_lowercase();
    for r in rules {
        if r.exclude.iter().any(|p| pattern(p, url)) || !r.include.iter().any(|p| pattern(p, url)) {
            continue;
        }
        let path = parts(url).2.split('?').next().unwrap_or("").to_string();
        let value = match r.action.as_str() {
            "redirect" => query_value(url, &r.param),
            "base64,redirect" => query_value(url, &r.param).and_then(|v| {
                let v = v.trim_end_matches('=');
                base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(v)
                    .or_else(|_| base64::engine::general_purpose::STANDARD_NO_PAD.decode(v)).ok()
                    .and_then(|b| String::from_utf8(b).ok())
            }),
            "regex-path" | "regex-path-template" => {
                let re = regex::Regex::new(&r.param).ok().filter(|_| r.param.len() <= 200)?;
                let caps = re.captures(&path)?;
                let got: Vec<&str> = caps.iter().skip(1).flatten().map(|m| m.as_str()).collect();
                if r.action == "regex-path" {
                    Some(unescape(&got.concat()))
                } else {
                    let mut t = r.redirect_url_template.clone()?;
                    for (i, g) in got.iter().enumerate().rev() {
                        t = t.replace(&format!("${}", i + 1), &unescape(g));
                    }
                    Some(t)
                }
            }
            _ => None,
        };
        let Some(mut to) = value else { continue };
        if let Some(scheme) = &r.prepend_scheme {
            if to.contains("://") {
                continue;
            }
            to = format!("{scheme}://{to}");
        }
        let naive = to.split_once("://").map(|(_, r)| r.split([':', '/', '?']).next().unwrap_or("")).unwrap_or("");
        let th = crate::host(&to).to_ascii_lowercase();
        if (to.starts_with("http://") || to.starts_with("https://")) && !th.is_empty() && naive.eq_ignore_ascii_case(&th)
            && site(&th) != site(&host) && to != url
        {
            return Some(to);
        }
    }
    None
}

/// Brave's query filter (components/query_filter): tracking parameters come
/// off links between sites and off redirects.
fn strip_query(rules: &[QueryRule], url: &str, prev: &str, redirected: bool) -> Option<String> {
    let (base, frag) = url.split_once('#').map_or((url, None), |(b, f)| (b, Some(f)));
    let (path, query) = base.split_once('?')?;
    let host = crate::host(url).to_ascii_lowercase();
    if query.is_empty() || host == "urldefense.com" {
        return None;
    }
    let from = crate::host(prev).to_ascii_lowercase();
    if !from.is_empty() && site(&from) == site(&host) {
        return None; // same site, whether a link or a redirect
    }
    let _ = redirected;
    let mut params: HashSet<&str> = HashSet::new();
    for r in rules {
        if !r.exclude.iter().any(|p| pattern(p, url)) && r.include.iter().any(|p| pattern(p, url)) {
            params.extend(r.params.iter().map(String::as_str));
        }
    }
    let conditional = |k: &str| -> bool {
        let keep = |re: &str| regex::Regex::new(re).is_ok_and(|r| r.is_match(url));
        match k {
            "ck_subscriber_id" => !keep("/unsubscribe"),
            "h_sid" | "h_slt" => !keep("/email/"),
            "mkt_tok" => !keep("([uU]nsubscribe|emailWebview)"),
            _ => false,
        }
    };
    let tokens: Vec<&str> = query.split('&').collect();
    let kept: Vec<&str> = tokens.iter().copied()
        .filter(|t| match t.split_once('=') {
            Some((k, v)) if !k.is_empty() && !v.is_empty() => !(params.contains(k) || conditional(k)),
            _ => true,
        })
        .collect();
    if kept.len() == tokens.len() {
        return None;
    }
    let mut out = path.to_string();
    if !kept.is_empty() {
        out += "?";
        out += &kept.join("&");
    }
    if let Some(f) = frag {
        out += "#";
        out += f;
    }
    Some(out)
}

// ---------- downloading the lists ----------

/// Brave's list preprocessor (brave-core-crx-packager, adBlockRustUtils.js):
/// `!#if` blocks for other platforms are dropped, as uBO itself does.
fn preprocess(text: &str) -> String {
    #[derive(PartialEq, Clone, Copy)]
    enum S { Yes, No, Whatever }
    let cond = |v: &str| -> Option<bool> {
        Some(match v {
            "ext_ublock" | "env_chromium" | "cap_user_stylesheet" | "adguard_ext_chromium" | "adguard_ext_opera" => true,
            "ext_devbuild" | "env_devbuild" | "env_edge" | "env_firefox" | "env_legacy" | "env_safari" | "cap_html_filtering"
            | "false" | "ext_abp" | "adguard" | "adguard_app_android" | "adguard_app_ios" | "adguard_app_mac"
            | "adguard_app_windows" | "adguard_ext_android_cb" | "adguard_ext_edge" | "adguard_ext_firefox"
            | "adguard_ext_safari" => false,
            _ => return None,
        })
    };
    let mut stack: Vec<S> = vec![];
    let mut out = String::with_capacity(text.len());
    for raw in text.lines() {
        let line = raw.trim();
        let top = stack.last().copied();
        if let Some(rest) = line.strip_prefix("!#if ") {
            if top == Some(S::No) {
                stack.push(S::No);
                continue;
            }
            let (neg, var) = rest.strip_prefix('!').map_or((false, rest), |v| (true, v));
            stack.push(match cond(var.trim()) {
                None => S::Whatever,
                Some(v) => if v != neg { S::Yes } else { S::No },
            });
            continue;
        }
        if line == "!#else" {
            let s = stack.pop().unwrap_or(S::Whatever);
            stack.push(match s { S::Yes => S::No, S::No => S::Yes, S::Whatever => S::Whatever });
            continue;
        }
        if line == "!#endif" {
            stack.pop();
            continue;
        }
        if top != Some(S::No) {
            out += line;
            out.push('\n');
        }
    }
    out
}

/// Downloads and compiles the lists off the UI thread when they are missing,
/// old, or not the ones `cfg` asks for (or `force`), then hands them back as
/// bytes; the engines themselves stay on the UI thread. Everything comes down
/// at once with Windows' own curl.exe. `done` gets None when the download
/// failed (offline): the old lists stay.
pub fn refresh(cfg: Config, force: bool, done: impl FnOnce(Option<Compiled>) + Send + 'static) {
    let fresh = fs::metadata(folder().join("a.dat"))
        .and_then(|m| m.modified())
        .is_ok_and(|t| t.elapsed().unwrap_or(STALE) < STALE);
    let same = fs::read_to_string(folder().join("key.txt")).is_ok_and(|k| k == cfg.key());
    if fresh && same && !force {
        return;
    }
    std::thread::spawn(move || done(download(&cfg)));
}

fn curl(pairs: &[(PathBuf, String)]) {
    let mut cmd = Command::new("curl.exe");
    cmd.args(["-sfL", "--parallel", "--max-time", "120"]).creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    for (path, url) in pairs {
        cmd.arg("-o").arg(path).arg(url);
    }
    let _ = cmd.output(); // a list that fails is skipped; the rest still count
}

fn download(cfg: &Config) -> Option<Compiled> {
    let dir = folder().join("download");
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).ok()?;
    // The catalog first: it says which lists there are.
    curl(&[(dir.join("catalog.json"), CATALOG.into())]);
    if let Ok(b) = fs::read(dir.join("catalog.json")) {
        if serde_json::from_slice::<Vec<Entry>>(&b).is_ok_and(|c| !c.is_empty()) {
            let _ = fs::rename(dir.join("catalog.json"), folder().join("catalog.json"));
        }
    }
    let cat = catalog();
    let chosen = cfg.chosen(&cat);
    let mut urls: Vec<String> = vec![];
    for e in &chosen {
        for s in &e.sources {
            if !urls.contains(&s.url) {
                urls.push(s.url.clone());
            }
        }
    }
    let mut pairs: Vec<(PathBuf, String)> = urls.iter().enumerate().map(|(i, u)| (dir.join(format!("{i}.txt")), u.clone())).collect();
    pairs.push((dir.join("resources.json"), RESOURCES.into()));
    for d in DATA {
        pairs.push((dir.join(d), format!("{BL}{d}")));
    }
    curl(&pairs);
    let read = |u: &str| urls.iter().position(|x| x == u).and_then(|i| fs::read_to_string(dir.join(format!("{i}.txt"))).ok()).filter(|t| !t.is_empty());
    let (mut a, mut b) = (FilterSet::new(false), FilterSet::new(false));
    let (mut got, mut wanted) = (0, 0);
    for e in &chosen {
        let mut text = String::new();
        for s in &e.sources {
            wanted += 1;
            let Some(t) = read(&s.url) else { continue };
            got += 1;
            let mut t = preprocess(&t);
            // Brave's own scriptlets only from Brave's own lists.
            if !s.title.starts_with("Brave ") && !e.title.starts_with("Brave ") {
                t = t.lines().filter(|l| !l.contains("+js(brave-")).collect::<Vec<_>>().join("\n");
            }
            text += &t;
            text.push('\n');
        }
        let opts = ParseOptions { permissions: PermissionMask::from_bits(e.permission_mask), ..Default::default() };
        if e.first_party_protections { a.add_filter_list(text, opts) } else { b.add_filter_list(text, opts) };
    }
    let resources = fs::read(dir.join("resources.json")).unwrap_or_default();
    if got < wanted / 2 || resources.is_empty() {
        return None; // offline or mostly failed: keep what we have, try next launch
    }
    let c = Compiled {
        a: Engine::new_with_filter_set(a).serialize(),
        b: Engine::new_with_filter_set(b).serialize(),
        resources,
        key: cfg.key(),
    };
    let save = |p: PathBuf, bytes: &[u8]| {
        let tmp = p.with_extension("tmp");
        if fs::write(&tmp, bytes).is_ok() {
            let _ = fs::rename(tmp, p);
        }
    };
    save(folder().join("a.dat"), &c.a);
    save(folder().join("b.dat"), &c.b);
    save(folder().join("resources.json"), &c.resources);
    save(folder().join("key.txt"), c.key.as_bytes());
    for d in DATA {
        if fs::metadata(dir.join(d)).is_ok_and(|m| m.len() > 0) {
            let _ = fs::rename(dir.join(d), folder().join(d));
        }
    }
    let _ = fs::remove_dir_all(&dir);
    // Lists from earlier versions of Brisklark.
    for old in ["main.dat", "main+cookies.dat", "main.2.dat", "main+cookies.2.dat", "first.dat"] {
        let _ = fs::remove_file(folder().join(old));
    }
    Some(c)
}

/// When the lists on disk were last compiled, for Settings.
pub fn updated() -> Option<SystemTime> {
    fs::metadata(folder().join("a.dat")).and_then(|m| m.modified()).ok()
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
const KEEP_SCRIPTS: usize = 8;

/// The last line on YouTube: an ad the lists let through is muted and run to its
/// end at once, and its skip button pressed.
const YT_SKIP: &str = "if(window===top)(()=>{let muted=false;setInterval(()=>{const p=document.getElementById('movie_player');if(!p)return;\
const v=p.querySelector('video');if(p.classList.contains('ad-showing')){if(v){if(!v.muted){v.muted=true;muted=true}\
if(isFinite(v.duration)&&v.duration>0&&v.currentTime<v.duration-0.1)v.currentTime=v.duration;else v.playbackRate=16}\
for(const b of document.querySelectorAll('.ytp-skip-ad-button,.ytp-ad-skip-button,.ytp-ad-skip-button-modern'))b.click()}\
else if(muted&&v){v.muted=false;muted=false;if(v.playbackRate===16)v.playbackRate=1}},200)})();\n";

/// A site's script, registered with one page.
struct Cached {
    host: String,
    hash: u64,
    id: Option<String>,
}

/// Runs the script only on its own site: on the page itself, and in the frames of
/// a page from that site.
fn gated(host: &str, script: &str) -> String {
    format!(
        "(()=>{{let h=location.hostname;try{{const a=location.ancestorOrigins;if(window!==top&&a&&a.length)h=new URL(a[a.length-1]).hostname}}catch(e){{}}\
if(h!=={host})return;\n{script}\n}})();",
        host = serde_json::to_string(host).unwrap_or_default()
    )
}

pub fn attach(view: &WebView, shared: Shared, page: Page) {
    let core = view.webview();
    let env = view.environment();
    let scripts: Rc<RefCell<Vec<Cached>>> = Default::default();
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
            let req = args.Request()?;
            let mut uri = PWSTR::null();
            req.Uri(&mut uri)?;
            let uri = take_pwstr(uri);
            let top = p.url.borrow();
            if !uri.starts_with("http") {
                return Ok(()); // data:, blob: and the like
            }
            let s = shield.borrow();
            if uri != *top {
                let mut ctx = COREWEBVIEW2_WEB_RESOURCE_CONTEXT::default();
                args.ResourceContext(&mut ctx)?;
                let mut method = PWSTR::null();
                req.Method(&mut method)?;
                let method = take_pwstr(method).to_ascii_lowercase();
                let response = match s.check(&uri, &top, kind(ctx), &method) {
                    Verdict::Allow => None,
                    Verdict::Block => Some(env.CreateWebResourceResponse(None, 403, w!("Blocked"), w!(""))?),
                    Verdict::Redirect(data) => {
                        let (body, mime) = stub(&data);
                        let stream = SHCreateMemStream(Some(&body));
                        let headers = HSTRING::from(format!("Content-Type: {mime}\r\nAccess-Control-Allow-Origin: *"));
                        Some(env.CreateWebResourceResponse(stream.as_ref(), 200, w!("OK"), &headers)?)
                    }
                };
                if let Some(r) = response {
                    p.blocked.set(p.blocked.get() + 1);
                    return args.SetResponse(&r);
                }
            }
            if !s.up_for(&top) {
                return Ok(());
            }
            // What goes out with requests, as in Brave. Only GETs are touched:
            // changing a POST's headers here makes WebView2 send it again without
            // its body, and YouTube's video stream (POSTs) stalls.
            let mut method = PWSTR::null();
            req.Method(&mut method)?;
            if !take_pwstr(method).eq_ignore_ascii_case("get") {
                return Ok(());
            }
            let headers = req.Headers()?;
            let has = |name: windows::core::PCWSTR| {
                let mut b = BOOL(0);
                unsafe { headers.Contains(name, &mut b) }.is_ok() && b.as_bool()
            };
            if has(w!("X-Client-Data")) {
                let _ = headers.RemoveHeader(w!("X-Client-Data"));
            }
            if s.prefs.gpc && !has(w!("Sec-GPC")) {
                let _ = headers.SetHeader(w!("Sec-GPC"), w!("1"));
            }
            let mut referer = PWSTR::null();
            if headers.GetHeader(w!("Referer"), &mut referer).is_ok() {
                let r = take_pwstr(referer);
                if let Some(capped) = cap_referrer(&r, &uri) {
                    let _ = if capped.is_empty() { headers.RemoveHeader(w!("Referer")) } else { headers.SetHeader(w!("Referer"), &HSTRING::from(capped)) };
                }
            }
            Ok(())
        }));
        let mut token = 0i64;
        let _ = core.add_WebResourceRequested(&requested, &mut token);

        // Each page gets its own site's rules and scriptlets, swapped in as it
        // starts; trackers' redirects and parameters are dealt with first.
        let (shield, p) = (shared.clone(), page.clone());
        let starting = NavigationStartingEventHandler::create(Box::new(move |sender, args| {
            let (Some(core), Some(args)) = (sender, args) else { return Ok(()) };
            let mut uri = PWSTR::null();
            args.Uri(&mut uri)?;
            let uri = take_pwstr(uri);
            let mut redirected = BOOL(0);
            args.IsRedirected(&mut redirected)?;
            let prev = p.url.borrow().clone();
            if !redirected.as_bool() && !p.ours.replace(false) {
                p.bounced.borrow_mut().clear();
                *p.upgraded.borrow_mut() = None;
            }
            let to = shield.borrow().rewrite(&uri, &prev, redirected.as_bool(), &p);
            if let Some(to) = to {
                args.SetCancel(true)?;
                p.ours.set(true);
                *p.url.borrow_mut() = to.clone();
                return core.Navigate(&HSTRING::from(to));
            }
            *p.url.borrow_mut() = uri.clone();
            p.blocked.set(0);
            if shield.borrow().prefs.gpc && shield.borrow().up_for(&uri) {
                let _ = args.RequestHeaders()?.SetHeader(w!("Sec-GPC"), w!("1"));
            }
            // Each site's script stays registered (the last few sites), and runs only
            // on its own site: a page never starts before its scriptlets are in.
            let host = crate::host(&uri).to_ascii_lowercase();
            let made = shield.borrow().page_script(&uri);
            let Some((script, exceptions)) = made else {
                p.exceptions.borrow_mut().clear();
                let gone: Vec<Option<String>> = {
                    let mut list = scripts.borrow_mut();
                    let (gone, keep) = list.drain(..).partition(|e: &Cached| e.host == host);
                    *list = keep;
                    gone.into_iter().map(|e| e.id).collect()
                };
                for id in gone.into_iter().flatten() {
                    let _ = core.RemoveScriptToExecuteOnDocumentCreated(&HSTRING::from(id));
                }
                return Ok(());
            };
            *p.exceptions.borrow_mut() = exceptions;
            let script = gated(&host, &script);
            let hash = {
                use std::hash::{Hash, Hasher};
                let mut h = std::collections::hash_map::DefaultHasher::new();
                script.hash(&mut h);
                h.finish()
            };
            {
                let mut list = scripts.borrow_mut();
                if let Some(i) = list.iter().position(|e| e.host == host && e.hash == hash) {
                    let e = list.remove(i);
                    list.push(e);
                    return Ok(());
                }
                list.push(Cached { host: host.clone(), hash, id: None });
            }
            let (slot, c) = (scripts.clone(), core.clone());
            let _ = core.AddScriptToExecuteOnDocumentCreated(
                &HSTRING::from(script),
                &AddScriptToExecuteOnDocumentCreatedCompletedHandler::create(Box::new(move |hr, id| {
                    if hr.is_err() {
                        return Ok(());
                    }
                    let mut drop_ids = vec![];
                    {
                        let mut list = slot.borrow_mut();
                        match list.iter_mut().find(|e| e.host == host && e.hash == hash) {
                            Some(e) => e.id = Some(id),
                            None => drop_ids.push(id),
                        }
                        // The same site's older script, and sites beyond the last few.
                        let mut keep = vec![];
                        let n = list.len();
                        for (i, e) in list.drain(..).enumerate() {
                            let old_version = e.host == host && e.hash != hash && e.id.is_some();
                            let too_old = n > KEEP_SCRIPTS && i < n - KEEP_SCRIPTS && e.id.is_some();
                            if old_version || too_old { drop_ids.extend(e.id) } else { keep.push(e) }
                        }
                        *list = keep;
                    }
                    for id in drop_ids {
                        let _ = c.RemoveScriptToExecuteOnDocumentCreated(&HSTRING::from(id));
                    }
                    Ok(())
                })),
            );
            Ok(())
        }));
        let _ = core.add_NavigationStarting(&starting, &mut token);

        // HTTPS that didn't work: back to http for this host, for this session.
        let (shield, p) = (shared.clone(), page.clone());
        let completed = NavigationCompletedEventHandler::create(Box::new(move |sender, args| {
            let (Some(core), Some(args)) = (sender, args) else { return Ok(()) };
            let mut ok = BOOL(0);
            args.IsSuccess(&mut ok)?;
            let mut status = COREWEBVIEW2_WEB_ERROR_STATUS::default();
            args.WebErrorStatus(&mut status)?;
            if ok.as_bool() {
                *p.upgraded.borrow_mut() = None;
            } else if status != COREWEBVIEW2_WEB_ERROR_STATUS_OPERATION_CANCELED {
                let http = p.upgraded.borrow_mut().take();
                if let Some(http) = http {
                    shield.borrow_mut().fellback.insert(crate::host(&http).to_ascii_lowercase());
                    p.ours.set(true);
                    let _ = core.Navigate(&HSTRING::from(http));
                }
            }
            Ok(())
        }));
        let _ = core.add_NavigationCompleted(&completed, &mut token);

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
            let sels = shield.borrow().generic(&list("c"), &list("i"), &p.exceptions.borrow());
            if !sels.is_empty() {
                let msg = serde_json::json!({"brisklark": "hide", "sels": sels}).to_string();
                let _ = core.PostWebMessageAsJson(&HSTRING::from(msg));
            }
            Ok(())
        }));
        let _ = core.add_WebMessageReceived(&message, &mut token);
    }
}

/// Brave's referrer cap: another site sees only where a visit came from, not
/// the page; none at all from https to http. None: leave it as it is.
fn cap_referrer(referrer: &str, url: &str) -> Option<String> {
    let origin = |u: &str| -> Option<String> {
        let (scheme, rest) = u.split_once("://")?;
        Some(format!("{scheme}://{}", rest.split(['/', '?', '#']).next()?))
    };
    let (from, to) = (origin(referrer)?, origin(url)?);
    if from == to {
        return None;
    }
    if from.starts_with("https://") && to.starts_with("http://") {
        return Some(String::new());
    }
    let capped = format!("{from}/");
    (capped != referrer).then_some(capped)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Engine A from "a" (first-party protections), engine B from "b".
    fn shield_with(a: &str, b: &str) -> Shield {
        let set = |rules: &str| {
            let mut s = FilterSet::new(false);
            s.add_filter_list(rules.to_string(), ParseOptions { permissions: TRUST, ..Default::default() });
            Engine::new_with_filter_set(s).serialize()
        };
        let mut s = Shield { on: true, ..Default::default() };
        // Round-trip through the on-disk format, as on a real launch.
        s.install(Compiled {
            a: set(a),
            b: set(b),
            resources: br#"[{"name":"noop.js","aliases":[],"kind":{"mime":"application/javascript"},"content":"KGZ1bmN0aW9uKCl7fSkoKTs="}]"#.to_vec(),
            key: String::new(),
        });
        s
    }

    #[test]
    fn fallback_list() {
        let s = Shield { on: true, ..Default::default() };
        assert_eq!(s.check("https://stats.g.doubleclick.net/x.js", "https://news.com/", "script", "get"), Verdict::Block);
        assert_eq!(s.check("https://notdoubleclick.net/x.js", "https://news.com/", "script", "get"), Verdict::Allow);
        assert_eq!(s.check("https://www.hotjar.com/a.js", "https://www.hotjar.com/", "script", "get"), Verdict::Allow);
    }

    #[test]
    fn brave_engines_and_switches() {
        let mut s = shield_with(
            "||ads.example.net^\n||news.com/ads/\n||imasdk.example^$redirect=noop.js\nnews.com##.sponsored\n##.ad-banner\n||youtube.com/pagead/",
            "||news.com/firstparty-ad.js^",
        );
        let page = "https://news.com/story";
        assert_eq!(s.check("https://ads.example.net/b.js", page, "script", "get"), Verdict::Block);
        assert_eq!(s.check("https://cdn.news.com/app.js", page, "script", "get"), Verdict::Allow);
        // Standard: a site's own requests meet only the second engine.
        assert_eq!(s.check("https://news.com/ads/x.js", page, "script", "get"), Verdict::Allow);
        assert_eq!(s.check("https://news.com/firstparty-ad.js", page, "script", "get"), Verdict::Block);
        // Pop-unders to blocked addresses never open; ordinary links do.
        assert!(s.popup_blocked("https://ads.example.net/landing", page));
        assert!(!s.popup_blocked("https://cdn.news.com/next", page));
        s.prefs.aggressive = true;
        assert_eq!(s.check("https://news.com/ads/x.js", page, "script", "get"), Verdict::Block);
        s.prefs.aggressive = false;
        // Redirect rules hand back a working stub instead of a failure.
        assert!(matches!(s.check("https://imasdk.example/ima3.js", page, "script", "get"), Verdict::Redirect(_)));
        // Cosmetics: site rules in the page script, generic ones on request.
        let (js, _) = s.page_script(page).unwrap();
        assert!(js.contains(".sponsored"));
        let sels = s.generic(&["ad-banner".into(), "story".into()], &[], &HashSet::new());
        assert_eq!(sels, vec![".ad-banner".to_string()]);
        // Shields down for this site, or everywhere.
        s.off.insert("news.com".into());
        assert_eq!(s.check("https://ads.example.net/b.js", page, "script", "get"), Verdict::Allow);
        assert!(s.page_script(page).is_none());
        s.off.clear();
        s.on = false;
        assert_eq!(s.check("https://ads.example.net/b.js", page, "script", "get"), Verdict::Allow);
    }

    #[test]
    fn preprocessor() {
        let t = "a\n!#if env_firefox\nff\n!#else\nchrome\n!#endif\n!#if !env_chromium\nno\n!#endif\n!#if mystery\nkept\n!#endif\nz";
        assert_eq!(preprocess(t), "a\nchrome\nkept\nz\n");
    }

    #[test]
    fn match_patterns() {
        assert!(pattern("*://*.example.com/*", "https://a.example.com/x?y"));
        assert!(pattern("*://*.example.com/*", "http://example.com/"));
        assert!(!pattern("*://*.example.com/*", "https://notexample.com/"));
        assert!(pattern("*://go.redirectingat.com/*", "https://go.redirectingat.com/?url=x"));
        assert!(pattern("*://*.prsm1.com/r?*", "https://t.prsm1.com/r?u=1"));
        assert!(!pattern("*://*.prsm1.com/r?*", "https://t.prsm1.com/rx"));
    }

    #[test]
    fn query_filter() {
        let rules: Vec<QueryRule> = serde_json::from_str(r#"[{"include":["*://*/*"],"exclude":[],"params":["fbclid","gclid"]},
            {"include":["*://*.youtube.com/*","*://youtu.be/*"],"exclude":[],"params":["si"]}]"#).unwrap();
        let from = "https://www.facebook.com/";
        assert_eq!(strip_query(&rules, "https://news.com/a?id=3&fbclid=xyz#top", from, false).as_deref(), Some("https://news.com/a?id=3#top"));
        assert_eq!(strip_query(&rules, "https://news.com/a?gclid=1", from, false).as_deref(), Some("https://news.com/a"));
        assert_eq!(strip_query(&rules, "https://youtu.be/abc?si=tracker", from, false).as_deref(), Some("https://youtu.be/abc"));
        assert_eq!(strip_query(&rules, "https://news.com/a?id=3", from, false), None);
        // A site's links to itself are left alone, as in Brave.
        assert_eq!(strip_query(&rules, "https://news.com/a?fbclid=1", "https://www.news.com/", false), None);
    }

    #[test]
    fn debouncing() {
        let rules: Vec<BounceRule> = serde_json::from_str(r#"[
            {"include":["*://go.redirectingat.com/*"],"exclude":[],"action":"redirect","param":"url"},
            {"include":["*://*.ouo.today/*"],"exclude":[],"action":"base64,redirect","param":"cr"},
            {"include":["*://www.google.com/amp/s/*"],"exclude":[],"prepend_scheme":"https","action":"regex-path","param":"^/amp/s/(.*)$"},
            {"include":["*://y2u.be/*"],"exclude":[],"action":"regex-path-template","param":"^/([^/]+)$","redirect_url_template":"https://www.youtube.com/watch?v=$1"}]"#).unwrap();
        assert_eq!(debounce(&rules, "https://go.redirectingat.com/?id=1&url=https%3A%2F%2Fshop.com%2Fp").as_deref(), Some("https://shop.com/p"));
        assert_eq!(debounce(&rules, "https://x.ouo.today/?cr=aHR0cHM6Ly9leGFtcGxlLm9yZy8").as_deref(), Some("https://example.org/"));
        assert_eq!(debounce(&rules, "https://www.google.com/amp/s/news.com/story.amp").as_deref(), Some("https://news.com/story.amp"));
        assert_eq!(debounce(&rules, "https://y2u.be/dQw4w9WgXcQ").as_deref(), Some("https://www.youtube.com/watch?v=dQw4w9WgXcQ"));
        // Never to the same site, never to something that isn't a web address.
        assert_eq!(debounce(&rules, "https://go.redirectingat.com/?url=https%3A%2F%2Fredirectingat.com%2F"), None);
        assert_eq!(debounce(&rules, "https://go.redirectingat.com/?url=javascript%3Aalert(1)"), None);
    }

    #[test]
    fn https_and_referrer() {
        let mut s = Shield { on: true, prefs: Prefs::default(), ..Default::default() };
        let p = Page::default();
        assert_eq!(s.rewrite("http://example.com/a", "", false, &p).as_deref(), Some("https://example.com/a"));
        assert_eq!(p.upgraded.borrow().as_deref(), Some("http://example.com/a"));
        for local in ["http://localhost:3000/", "http://192.168.1.1/", "http://router/", "http://printer.local/"] {
            assert_eq!(s.rewrite(local, "", false, &p), None, "{local}");
        }
        s.fellback.insert("old.example".into());
        assert_eq!(s.rewrite("http://old.example/", "", false, &p), None);
        assert_eq!(cap_referrer("https://a.com/secret/page?q=1", "https://b.com/x").as_deref(), Some("https://a.com/"));
        assert_eq!(cap_referrer("https://a.com/x", "https://a.com/y"), None);
        assert_eq!(cap_referrer("https://a.com/x", "http://b.com/").as_deref(), Some(""));
    }

    #[test]
    fn fingerprint_seed() {
        let s = Shield { on: true, prefs: Prefs::default(), session: [1, 2], ..Default::default() };
        let a = s.farble("https://www.example.com/x");
        assert!(!a.contains("__SEED__") && !a.contains("__OFF__"));
        assert_eq!(a, s.farble("https://sub.example.com/y"), "one seed per site");
        assert_ne!(a, s.farble("https://other.org/"), "a different seed per site");
    }

    #[test]
    fn stubs() {
        assert_eq!(stub("data:application/javascript;base64,KCk7"), (b"();".to_vec(), "application/javascript".into()));
        assert_eq!(stub("data:text/plain,hi"), (b"hi".to_vec(), "text/plain".into()));
    }

    /// Against the real lists on this machine: cargo test -- --ignored
    #[test]
    #[ignore]
    fn real_lists() {
        let cfg = Config { cookies: true, lists_on: vec![], lists_off: vec![] };
        let shared = Shield::load(true, HashSet::new(), Prefs::default(), &cfg);
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
            assert_ne!(s.check(ad, page, "script", "get"), Verdict::Allow, "should block {ad}");
        }
        for ok in ["https://www.theverge.com/app.js", "https://cdn.jsdelivr.net/npm/react@18.3.1/umd/react.production.min.js"] {
            assert_eq!(s.check(ok, page, "script", "get"), Verdict::Allow, "should allow {ok}");
        }
        // Google's own pings stay (blocking them is what trips Google's robot check).
        let g = "https://www.google.com/search?q=github";
        for ok in ["https://www.google.com/gen_204?atyp=i", "https://www.google.com/recaptcha/api.js", "https://www.google.com/log?format=json"] {
            assert_eq!(s.check(ok, g, "xhr", "get"), Verdict::Allow, "should allow {ok}");
        }
        // YouTube gets uBO's scriptlets, trusted ones included, after Brave's
        // prelude: what takes its video ads away.
        let (yt, _) = s.page_script("https://www.youtube.com/watch?v=x").unwrap();        for f in ["scriptletGlobals", "function jsonPrune", "function trustedReplaceXhrResponse", "function safeSelf"] {
            assert!(yt.contains(f), "YouTube scriptlet missing: {f}");
        }
        // The data files for the network protections came down too.
        assert!(!s.data.query.is_empty() && !s.data.bounce.is_empty() && !s.data.https_exceptions.is_empty());
    }
}
