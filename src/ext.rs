//! Chrome extensions: installed from the Chrome Web Store (the same .crx Chrome
//! downloads) or from an unpacked folder, and run by WebView2 itself.

use serde_json::Value;
use std::{
    fs,
    os::windows::process::CommandExt,
    path::{Path, PathBuf},
    process::Command,
};
use webview2_com::{
    take_pwstr, BrowserExtensionEnableCompletedHandler, BrowserExtensionRemoveCompletedHandler,
    Microsoft::Web::WebView2::Win32::*, ProfileAddBrowserExtensionCompletedHandler,
    ProfileGetBrowserExtensionsCompletedHandler,
};
use windows::core::{Interface, BOOL, HSTRING, PWSTR};

#[derive(Clone, Debug)]
pub struct Ext {
    pub id: String,
    pub name: String,
    pub enabled: bool,
}

/// Where extensions from the Web Store are unpacked.
pub fn dir() -> PathBuf {
    crate::dir("LOCALAPPDATA").join("Extensions")
}

/// The extension id in a Chrome Web Store address.
pub fn store_id(url: &str) -> Option<String> {
    let host = crate::host(url).to_ascii_lowercase();
    let on_store = host == "chromewebstore.google.com" || (host == "chrome.google.com" && url.contains("/webstore/"));
    if !on_store {
        return None;
    }
    let path = url.split(['?', '#']).next().unwrap_or(url);
    path.split('/')
        .find(|s| s.len() == 32 && s.bytes().all(|b| (b'a'..=b'p').contains(&b)))
        .map(String::from)
}

/// An extension id is the first 16 bytes of the SHA-256 of its public key, as a–p.
fn id_of(key: &[u8]) -> String {
    crate::sha::sha256(key)[..16].iter().flat_map(|b| [b >> 4, b & 15]).map(|n| (b'a' + n) as char).collect()
}

fn varint(b: &[u8], i: &mut usize) -> Option<u64> {
    let (mut v, mut shift) = (0u64, 0);
    loop {
        let byte = *b.get(*i)?;
        *i += 1;
        v |= ((byte & 0x7f) as u64) << shift;
        if byte < 0x80 {
            return Some(v);
        }
        shift += 7;
        if shift > 63 {
            return None;
        }
    }
}

/// The length-delimited fields of a protobuf message with the given number.
fn fields(b: &[u8], want: u64) -> Vec<&[u8]> {
    let (mut out, mut i) = (vec![], 0);
    while i < b.len() {
        let Some(tag) = varint(b, &mut i) else { break };
        match tag & 7 {
            0 => {
                if varint(b, &mut i).is_none() {
                    break;
                }
            }
            1 => i += 8,
            5 => i += 4,
            2 => {
                let Some(n) = varint(b, &mut i) else { break };
                let end = i.saturating_add(n as usize);
                if end > b.len() {
                    break;
                }
                if tag >> 3 == want {
                    out.push(&b[i..end]);
                }
                i = end;
            }
            _ => break,
        }
    }
    out
}

/// A CRX3 file: its zip, and the public key whose id matches (Chrome keeps the
/// id stable with it, so the extension's own pages and sign-ins keep working).
pub fn split_crx(crx: &[u8], id: &str) -> Result<(Vec<u8>, Option<Vec<u8>>), String> {
    if crx.len() < 12 || &crx[..4] != b"Cr24" {
        return Err("The Web Store didn't send an extension (it may not be available for this browser).".into());
    }
    let version = u32::from_le_bytes(crx[4..8].try_into().unwrap());
    let len = u32::from_le_bytes(crx[8..12].try_into().unwrap()) as usize;
    if version != 3 || crx.len() < 12 + len {
        return Err(format!("Unsupported extension package (CRX version {version})."));
    }
    let header = &crx[12..12 + len];
    let key = fields(header, 2)
        .into_iter()
        .chain(fields(header, 3))
        .filter_map(|proof| fields(proof, 1).first().map(|k| k.to_vec()))
        .find(|k| id_of(k) == id);
    Ok((crx[12 + len..].to_vec(), key))
}

fn hidden(cmd: &mut Command) -> &mut Command {
    cmd.creation_flags(0x0800_0000) // CREATE_NO_WINDOW
}

/// Downloads an extension from the Chrome Web Store and unpacks it. Blocking.
pub fn fetch(id: &str) -> Result<PathBuf, String> {
    let root = dir();
    fs::create_dir_all(&root).map_err(|e| e.to_string())?;
    let crx_path = root.join(format!("{id}.crx"));
    let url = format!(
        "https://clients2.google.com/service/update2/crx?response=redirect&prodversion=140.0.0.0&acceptformat=crx2,crx3&x=id%3D{id}%26uc"
    );
    let out = hidden(Command::new("curl.exe").args(["-sfL", "--max-time", "120", "-o"]).arg(&crx_path).arg(&url))
        .output()
        .map_err(|e| format!("Couldn't run curl: {e}"))?;
    if !out.status.success() {
        return Err("Couldn't download the extension from the Chrome Web Store.".into());
    }
    let crx = fs::read(&crx_path).map_err(|e| e.to_string())?;
    let _ = fs::remove_file(&crx_path);
    let (zip, key) = split_crx(&crx, id)?;
    let dest = root.join(id);
    let _ = fs::remove_dir_all(&dest);
    fs::create_dir_all(&dest).map_err(|e| e.to_string())?;
    let zip_path = root.join(format!("{id}.zip"));
    fs::write(&zip_path, zip).map_err(|e| e.to_string())?;
    let out = hidden(Command::new("tar.exe").arg("-xf").arg(&zip_path).arg("-C").arg(&dest)).output();
    let _ = fs::remove_file(&zip_path);
    if !out.map(|o| o.status.success()).unwrap_or(false) {
        return Err("Couldn't unpack the extension.".into());
    }
    // Chrome's own signature files: an unpacked extension may not have them.
    let _ = fs::remove_dir_all(dest.join("_metadata"));
    if let Some(key) = key {
        use base64::Engine as _;
        let path = dest.join("manifest.json");
        let text = fs::read_to_string(&path).map_err(|e| e.to_string())?;
        let mut manifest: Value = serde_json::from_str(text.trim_start_matches('\u{feff}')).map_err(|e| e.to_string())?;
        manifest["key"] = Value::String(base64::engine::general_purpose::STANDARD.encode(key));
        fs::write(&path, serde_json::to_string_pretty(&manifest).unwrap_or_default()).map_err(|e| e.to_string())?;
    }
    Ok(dest)
}

/// The popup (toolbar button) and options pages an extension's manifest names.
pub fn pages(folder: &Path) -> (Option<String>, Option<String>) {
    let Ok(text) = fs::read_to_string(folder.join("manifest.json")) else { return (None, None) };
    let Ok(m) = serde_json::from_str::<Value>(text.trim_start_matches('\u{feff}')) else { return (None, None) };
    let s = |v: &Value| v.as_str().filter(|s| !s.is_empty()).map(|s| s.trim_start_matches('/').to_string());
    let popup = ["action", "browser_action", "page_action"].iter().find_map(|k| s(&m[*k]["default_popup"]));
    let options = s(&m["options_ui"]["page"]).or_else(|| s(&m["options_page"]));
    (popup, options)
}

pub fn profile(core: &ICoreWebView2) -> Option<ICoreWebView2Profile7> {
    unsafe { core.cast::<ICoreWebView2_13>().ok()?.Profile().ok()?.cast().ok() }
}

fn read(e: &ICoreWebView2BrowserExtension) -> Option<Ext> {
    unsafe {
        let (mut id, mut name, mut on) = (PWSTR::null(), PWSTR::null(), BOOL(0));
        e.Id(&mut id).ok()?;
        e.Name(&mut name).ok()?;
        e.IsEnabled(&mut on).ok()?;
        Some(Ext { id: take_pwstr(id), name: take_pwstr(name), enabled: on.as_bool() })
    }
}

/// Installs an unpacked extension folder.
pub fn add(p: &ICoreWebView2Profile7, folder: &Path, done: impl FnOnce(Result<Ext, String>) + 'static) {
    let path = HSTRING::from(folder.as_os_str());
    let handler = ProfileAddBrowserExtensionCompletedHandler::create(Box::new(move |hr, e| {
        done(match (hr, e.as_ref().and_then(read)) {
            (Ok(()), Some(x)) => Ok(x),
            (Err(err), _) => Err(format!("WebView2 couldn't load it: {}", err.message())),
            _ => Err("WebView2 couldn't load it.".into()),
        });
        Ok(())
    }));
    if let Err(e) = unsafe { p.AddBrowserExtension(&path, &handler) } {
        eprintln!("AddBrowserExtension: {e}");
    }
}

fn each(p: &ICoreWebView2Profile7, f: impl FnOnce(Vec<(Ext, ICoreWebView2BrowserExtension)>) + 'static) {
    let handler = ProfileGetBrowserExtensionsCompletedHandler::create(Box::new(move |_, list| {
        let mut out = vec![];
        if let Some(list) = list {
            let mut n = 0u32;
            unsafe {
                let _ = list.Count(&mut n);
                for i in 0..n {
                    if let Ok(e) = list.GetValueAtIndex(i) {
                        if let Some(x) = read(&e) {
                            out.push((x, e));
                        }
                    }
                }
            }
        }
        f(out);
        Ok(())
    }));
    let _ = unsafe { p.GetBrowserExtensions(&handler) };
}

pub fn list(p: &ICoreWebView2Profile7, done: impl FnOnce(Vec<Ext>) + 'static) {
    each(p, move |all| done(all.into_iter().map(|(x, _)| x).collect()));
}

pub enum Act {
    Enable(bool),
    Remove,
}

pub fn act(p: &ICoreWebView2Profile7, id: String, what: Act, done: impl FnOnce() + 'static) {
    each(p, move |all| {
        let Some((_, e)) = all.into_iter().find(|(x, _)| x.id == id) else { return done() };
        unsafe {
            let _ = match what {
                Act::Enable(on) => e.Enable(on, &BrowserExtensionEnableCompletedHandler::create(Box::new(move |_| {
                    done();
                    Ok(())
                }))),
                Act::Remove => e.Remove(&BrowserExtensionRemoveCompletedHandler::create(Box::new(move |_| {
                    done();
                    Ok(())
                }))),
            };
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn store_links() {
        let id = "cjpalhdlnbpafiamejdnhcphjbkeiagm";
        assert_eq!(store_id(&format!("https://chromewebstore.google.com/detail/ublock-origin/{id}")).as_deref(), Some(id));
        assert_eq!(store_id(&format!("https://chromewebstore.google.com/detail/{id}?hl=en")).as_deref(), Some(id));
        assert_eq!(store_id(&format!("https://chrome.google.com/webstore/detail/x/{id}")).as_deref(), Some(id));
        assert_eq!(store_id("https://example.com/cjpalhdlnbpafiamejdnhcphjbkeiagm"), None);
    }

    #[test]
    fn crx_header() {
        // A CRX3 with one key proof (field 2 > field 1) and an empty zip.
        let key = b"not really a key".to_vec();
        let id = id_of(&key);
        let mut proof = vec![0x0a, key.len() as u8];
        proof.extend(&key);
        let mut header = vec![0x12, proof.len() as u8];
        header.extend(&proof);
        let mut crx = b"Cr24".to_vec();
        crx.extend(3u32.to_le_bytes());
        crx.extend((header.len() as u32).to_le_bytes());
        crx.extend(&header);
        crx.extend(b"PK\x05\x06");
        let (zip, found) = split_crx(&crx, &id).unwrap();
        assert_eq!(zip, b"PK\x05\x06");
        assert_eq!(found, Some(key));
        assert!(split_crx(b"<html>", &id).is_err());
    }
}
