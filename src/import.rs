//! Bringing bookmarks and history over from other browsers, and handing ours
//! out again. Chromium browsers (Chrome, Edge, Brave, Vivaldi, Opera, Arc) keep
//! bookmarks as JSON and history in SQLite; Firefox keeps both in SQLite. SQLite
//! is Windows' own (winsqlite3.dll), so this costs the exe nothing.

use serde_json::Value;
use std::{
    ffi::{c_void, CStr, CString},
    fs,
    path::{Path, PathBuf},
};
use windows::{
    core::{w, PCSTR},
    Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW},
};

/// A profile of another browser that we found on this PC.
#[derive(Clone)]
pub struct Source {
    pub label: String,
    firefox: bool,
    dir: PathBuf,
}

/// What one import brought in.
#[derive(Default)]
pub struct Found {
    pub bookmarks: Vec<(String, String)>,         // url, title
    pub history: Vec<(String, String, u32, u64)>, // url, title, visits, last visit (unix seconds)
}

const CHROMIUM: &[(&str, &str, &str)] = &[
    ("Google Chrome", "LOCALAPPDATA", r"Google\Chrome\User Data"),
    ("Microsoft Edge", "LOCALAPPDATA", r"Microsoft\Edge\User Data"),
    ("Brave", "LOCALAPPDATA", r"BraveSoftware\Brave-Browser\User Data"),
    ("Vivaldi", "LOCALAPPDATA", r"Vivaldi\User Data"),
    ("Chromium", "LOCALAPPDATA", r"Chromium\User Data"),
    ("Arc", "LOCALAPPDATA", r"Packages\TheBrowserCompany.Arc_ttt1ap7aakyb4\LocalCache\Local\Arc\User Data"),
    ("Opera", "APPDATA", r"Opera Software\Opera Stable"),
    ("Opera GX", "APPDATA", r"Opera Software\Opera GX Stable"),
];
const FIREFOX: &[(&str, &str)] = &[
    ("Firefox", r"Mozilla\Firefox\Profiles"),
    ("Zen", r"zen\Profiles"),
    ("LibreWolf", r"librewolf\Profiles"),
];

fn env(var: &str) -> PathBuf {
    PathBuf::from(std::env::var(var).unwrap_or_default())
}

/// Every browser profile on this PC with bookmarks or history to bring over.
pub fn sources() -> Vec<Source> {
    let mut out = vec![];
    let has = |d: &Path| d.join("Bookmarks").is_file() || d.join("History").is_file();
    for (name, var, rel) in CHROMIUM {
        let root = env(var).join(rel);
        if has(&root) {
            out.push(Source { label: name.to_string(), firefox: false, dir: root });
            continue;
        }
        let names: Value = fs::read(root.join("Local State")).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
        let mut found: Vec<PathBuf> = fs::read_dir(&root)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                let n = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
                (n == "Default" || n.starts_with("Profile ")) && has(p)
            })
            .collect();
        found.sort();
        let many = found.len() > 1;
        for dir in found {
            let key = dir.file_name().and_then(|n| n.to_str()).unwrap_or("").to_string();
            let who = names["profile"]["info_cache"][&key]["name"].as_str().unwrap_or(&key).to_string();
            let label = if many { format!("{name}: {who}") } else { name.to_string() };
            out.push(Source { label, firefox: false, dir });
        }
    }
    for (name, rel) in FIREFOX {
        let mut found: Vec<PathBuf> = fs::read_dir(env("APPDATA").join(rel))
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.join("places.sqlite").is_file())
            .collect();
        // The profile used most recently first.
        found.sort_by_key(|p| std::cmp::Reverse(fs::metadata(p.join("places.sqlite")).and_then(|m| m.modified()).ok()));
        let many = found.len() > 1;
        for dir in found {
            let n = dir.file_name().and_then(|n| n.to_str()).unwrap_or("");
            let who = n.split_once('.').map_or(n, |(_, w)| w).to_string();
            let label = if many { format!("{name}: {who}") } else { name.to_string() };
            out.push(Source { label, firefox: true, dir });
        }
    }
    out
}

/// Reads one profile. The browser may be open: its files are copied first.
pub fn read(src: &Source) -> Result<Found, String> {
    let mut f = Found::default();
    let tmp = crate::dir("LOCALAPPDATA").join("import");
    let _ = fs::remove_dir_all(&tmp);
    fs::create_dir_all(&tmp).map_err(|e| e.to_string())?;
    let copy = |name: &str| -> Option<PathBuf> {
        let from = src.dir.join(name);
        let to = tmp.join(name);
        fs::copy(&from, &to).ok().map(|_| to)
    };
    if src.firefox {
        let db = copy("places.sqlite").ok_or("Couldn't read Firefox's data. Close Firefox and try again.")?;
        copy("places.sqlite-wal"); // recent changes not yet folded into the file
        f.bookmarks = query(&db, "SELECT p.url, COALESCE(b.title, p.title, '') FROM moz_bookmarks b JOIN moz_places p ON p.id = b.fk \
                                  WHERE b.type = 1 AND p.url LIKE 'http%' ORDER BY b.dateAdded")
            .into_iter()
            .map(|r| (r[0].clone(), r[1].clone()))
            .collect();
        f.history = query(&db, "SELECT url, COALESCE(title, ''), visit_count, COALESCE(last_visit_date, 0) FROM moz_places \
                                WHERE visit_count > 0 AND hidden = 0 AND url LIKE 'http%' ORDER BY last_visit_date DESC LIMIT 10000")
            .into_iter()
            .map(|r| (r[0].clone(), r[1].clone(), r[2].parse().unwrap_or(1), r[3].parse::<u64>().unwrap_or(0) / 1_000_000))
            .collect();
    } else {
        if let Ok(b) = fs::read(src.dir.join("Bookmarks")) {
            let v: Value = serde_json::from_slice(&b).map_err(|e| e.to_string())?;
            for root in ["bookmark_bar", "other", "synced"] {
                walk(&v["roots"][root], &mut f.bookmarks);
            }
        }
        if let Some(db) = copy("History") {
            // Chromium counts microseconds from 1601.
            f.history = query(&db, "SELECT url, title, visit_count, last_visit_time FROM urls WHERE hidden = 0 \
                                    AND url LIKE 'http%' ORDER BY last_visit_time DESC LIMIT 10000")
                .into_iter()
                .map(|r| {
                    let t = (r[3].parse::<u64>().unwrap_or(0) / 1_000_000).saturating_sub(11_644_473_600);
                    (r[0].clone(), r[1].clone(), r[2].parse().unwrap_or(1), t)
                })
                .collect();
        }
    }
    let _ = fs::remove_dir_all(&tmp);
    if f.bookmarks.is_empty() && f.history.is_empty() {
        return Err(format!("Nothing found in {}. If it's open, close it and try again.", src.label));
    }
    Ok(f)
}

fn walk(node: &Value, out: &mut Vec<(String, String)>) {
    match node["type"].as_str() {
        Some("url") => {
            if let Some(u) = node["url"].as_str().filter(|u| u.starts_with("http")) {
                out.push((u.into(), node["name"].as_str().unwrap_or("").into()));
            }
        }
        _ => {
            for c in node["children"].as_array().into_iter().flatten() {
                walk(c, out);
            }
        }
    }
}

// ---------- bookmark files (every browser can export and import these) ----------

fn unescape(s: &str) -> String {
    s.replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&#39;", "'").replace("&amp;", "&")
}
fn escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// The links in a Netscape bookmark file, as Chrome, Edge, Firefox and Safari write them.
pub fn read_html(text: &str) -> Vec<(String, String)> {
    let lower = text.to_ascii_lowercase();
    let mut out = vec![];
    let mut at = 0;
    while let Some(i) = lower[at..].find("<a ").map(|i| i + at) {
        let Some(end) = lower[i..].find('>').map(|e| e + i) else { break };
        let close = lower[end..].find("</a>").map_or(end + 1, |c| c + end);
        let tag = &text[i..end];
        let href = lower[i..end].find("href=").and_then(|h| {
            let rest = &tag[h + 5..];
            let q = rest.chars().next()?;
            if q == '"' || q == '\'' {
                rest[1..].split(q).next()
            } else {
                rest.split([' ', '>']).next()
            }
        });
        if let Some(h) = href.map(unescape).filter(|h| h.starts_with("http")) {
            out.push((h, unescape(text[end + 1..close].trim())));
        }
        at = close.max(end + 1);
    }
    out
}

pub fn write_html(bookmarks: &[(String, String)]) -> String {
    let mut s = String::from(
        "<!DOCTYPE NETSCAPE-Bookmark-file-1>\n<!-- This is an automatically generated file. -->\n\
<META HTTP-EQUIV=\"Content-Type\" CONTENT=\"text/html; charset=UTF-8\">\n<TITLE>Bookmarks</TITLE>\n<H1>Bookmarks</H1>\n\
<DL><p>\n    <DT><H3>Brisklark</H3>\n    <DL><p>\n",
    );
    for (url, title) in bookmarks {
        s += &format!("        <DT><A HREF=\"{}\">{}</A>\n", escape(url), escape(if title.is_empty() { url } else { title }));
    }
    s + "    </DL><p>\n</DL><p>\n"
}

/// History as a spreadsheet: address, title, visits, last visit (UTC).
pub fn write_csv(history: &[(String, String, u32, u64)]) -> String {
    let q = |s: &str| format!("\"{}\"", s.replace('"', "\"\""));
    let mut s = String::from("url,title,visits,last_visit_utc\r\n");
    for (url, title, visits, last) in history {
        s += &format!("{},{},{visits},{}\r\n", q(url), q(title), date(*last));
    }
    s
}

/// Unix seconds as "YYYY-MM-DD HH:MM:SS".
pub fn date(t: u64) -> String {
    let (days, secs) = ((t / 86400) as i64, t % 86400);
    // Howard Hinnant's days-to-civil.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02}", secs / 3600, secs % 3600 / 60, secs % 60)
}

// ---------- Windows' SQLite ----------

type Db = *mut c_void;
struct Sqlite {
    open: unsafe extern "system" fn(*const i8, *mut Db, i32, *const i8) -> i32,
    prepare: unsafe extern "system" fn(Db, *const i8, i32, *mut Db, *mut *const i8) -> i32,
    step: unsafe extern "system" fn(Db) -> i32,
    text: unsafe extern "system" fn(Db, i32) -> *const i8,
    finalize: unsafe extern "system" fn(Db) -> i32,
    close: unsafe extern "system" fn(Db) -> i32,
}

fn sqlite() -> Option<Sqlite> {
    unsafe {
        let lib = LoadLibraryW(w!("winsqlite3.dll")).ok()?;
        let f = |name: &[u8]| GetProcAddress(lib, PCSTR(name.as_ptr()));
        Some(Sqlite {
            open: std::mem::transmute(f(b"sqlite3_open_v2\0")?),
            prepare: std::mem::transmute(f(b"sqlite3_prepare_v2\0")?),
            step: std::mem::transmute(f(b"sqlite3_step\0")?),
            text: std::mem::transmute(f(b"sqlite3_column_text\0")?),
            finalize: std::mem::transmute(f(b"sqlite3_finalize\0")?),
            close: std::mem::transmute(f(b"sqlite3_close\0")?),
        })
    }
}

/// Every row of a query, each column as text (SQLite turns numbers into text).
pub fn query(path: &Path, sql: &str) -> Vec<Vec<String>> {
    let (Some(s), Ok(p), Ok(q)) = (sqlite(), CString::new(path.to_string_lossy().as_bytes()), CString::new(sql)) else {
        return vec![];
    };
    let cols = sql.split(" FROM ").next().map_or(0, |c| c.matches(',').count() + 1);
    let mut rows = vec![];
    unsafe {
        let mut db: Db = std::ptr::null_mut();
        // Read-write on our own copy, so Firefox's WAL file can be folded in.
        if (s.open)(p.as_ptr(), &mut db, 0x2, std::ptr::null()) == 0 {
            let mut st: Db = std::ptr::null_mut();
            if (s.prepare)(db, q.as_ptr(), -1, &mut st, std::ptr::null_mut()) == 0 {
                while (s.step)(st) == 100 {
                    rows.push(
                        (0..cols as i32)
                            .map(|i| {
                                let t = (s.text)(st, i);
                                if t.is_null() { String::new() } else { CStr::from_ptr(t).to_string_lossy().into_owned() }
                            })
                            .collect(),
                    );
                }
                (s.finalize)(st);
            }
        }
        (s.close)(db);
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bookmark_files() {
        let html = r#"<DL><p><DT><H3>Bar</H3><DL><p>
            <DT><A HREF="https://a.com/?x=1&amp;y=2" ADD_DATE="1">A &amp; B</A>
            <DT><a href='https://b.com'>b</a><DT><A HREF="javascript:alert(1)">no</A></DL></DL>"#;
        let got = read_html(html);
        assert_eq!(got, vec![("https://a.com/?x=1&y=2".into(), "A & B".into()), ("https://b.com".into(), "b".into())]);
        assert_eq!(read_html(&write_html(&got)), got);
    }

    #[test]
    fn dates() {
        assert_eq!(date(0), "1970-01-01 00:00:00");
        assert_eq!(date(1_764_000_000), "2025-11-24 16:00:00");
    }

    #[test]
    fn windows_sqlite() {
        let dir = std::env::temp_dir().join("brisklark-sqlite-test");
        let _ = fs::create_dir_all(&dir);
        let db = dir.join("t.db");
        let _ = fs::remove_file(&db);
        // An empty file is a valid empty database; the query just finds no table.
        fs::write(&db, b"").unwrap();
        assert!(query(&db, "SELECT name FROM sqlite_master").is_empty());
        assert_eq!(query(&db, "SELECT 1, 'two'"), vec![vec!["1".to_string(), "two".to_string()]]);
    }
}
