# Brisklark

A small, fast, quiet web browser for Windows, written in Rust. A row of tabs,
one field, and the page, with Brave's ad blocker built in.

It runs on **WebView2**, the Edge engine already on every Windows 11 PC, so
there's no second copy of Chromium to download or keep in memory. One ~3.5 MB
exe; its own process uses about 40 MB with Brave's full lists loaded, and every tab
shares one Edge browser process.

Brisklark is based on the design of [Search](https://github.com/driceroland/Search),
a Mac browser by Office Commun (MIT licensed). It's an independent project, not
made or endorsed by Office Commun.

## Shields (ad blocker)

On by default and set up the way Brave ships it: Brave's own engine
([adblock-rust](https://github.com/brave/adblock-rust)) running Brave's default
lists (uBlock Origin filters, EasyList, EasyPrivacy, URLhaus, Brave's own lists)
plus the cookie-notice lists, with uBlock Origin's scriptlets from Brave's
resources file (what takes YouTube's video ads away). As in Brave's Standard
mode, other sites' ads and trackers are blocked and a site's requests to itself
meet only Brave's first-party list. Known ad slots are hidden before the page
draws, and generic hiding rules are matched as the page's classes and ids appear.
Scores 97/100 on adblock-tester.com.

The shield button shows what was blocked on the page and turns Shields off for a
site. Lists are fetched with Windows' `curl.exe` and refreshed every four days
(Settings › Update now), kept compiled in `%LOCALAPPDATA%\Brisklark\Shields`.

## What's in it

- Tabs across the top, in place of Windows' title bar
- Right-click a tab: new tab to the right, reload, duplicate, pin, mute, split
  view, bookmark, copy link, close others / to the right
- Split view: two tabs side by side, drag the divider; the pair is remembered
- Speaker icon on tabs playing sound; click it to mute
- Settings (`Ctrl+,` or ⚙): search engine, on startup, Shields, Goodies,
  clear history / cookies / cache
- Pinned tabs, private tabs, and separate private windows (`Ctrl+Shift+N`)
- Copy as Markdown (`Ctrl+Shift+M`): the selection, or the whole page, with the title and link
- Park tabs to a list on disk — off the strip and out of memory until opened again
- Close duplicate tabs in one action, keeping the one you're on
- One field for addresses and searches, suggesting from history, bookmarks and open tabs
- History and bookmarks panels
- A Home page on empty tabs with your bookmarked websites and their icons;
  add websites, edit names and links, or remove a bookmark from its tile
- Settings > Home: show/hide bookmarked websites, choose small/medium/large
  icons, and show/hide names
- Your session restored on launch; restored tabs aren't loaded until clicked,
  and hidden tabs drop to WebView2's low-memory level

`+` or `Ctrl+T` adds a New Tab; Esc on one you haven't typed into takes it away
again. Middle-click a tab to close it. Drag the empty part of the tab row to
move the window; double-click it to maximise.

On Home, click **Add website** to save a shortcut, or use the star on a page to
bookmark it. Hover over a tile and click its pencil (or right-click the tile) to
edit the name and URL. Removing a tile removes that bookmark, after confirmation.
**Customize** opens the Home settings. Icons load from the website itself, with
a letter as the fallback; no third-party favicon service is used. Private tabs
keep their separate private start page.

| | |
|---|---|
| `Ctrl+L` address · `Ctrl+K` switch tab · `Ctrl+T` new tab · `Ctrl+W` close · `Ctrl+Shift+T` reopen | `Ctrl+[` `Ctrl+]` back, forward · `Ctrl+Tab`, `Ctrl+Shift+[` `]` previous/next tab · `Ctrl+1`–`9` jump |
| `Ctrl+S` hide the tabs · `Ctrl+Shift+B` bookmark | `Ctrl+D` duplicate · `Ctrl+Shift+C` copy address · `Ctrl+Shift+M` copy as Markdown · `Ctrl+Shift+V` paste and go |
| `Ctrl+Y` history · `Ctrl+Shift+O` bookmarks · `Ctrl+Shift+N` private window · `Ctrl+,` settings | `Ctrl+F` find, `Ctrl+P` print, `F5` reload: WebView2's own |

## Private browsing and network privacy

`Ctrl+Shift+N`, Menu > New private window, or `brisklark.exe --private` opens a
separate private window. Menu > New private tab keeps a private tab in the current
window. The purple **Private** indicator opens the privacy settings. New tabs
from a private tab stay private, including links and popups.

Private visits and searches are excluded from history, recently closed tabs and
session restore. Private tabs use a separate WebView2 InPrivate profile with
password saving and form autofill disabled. Closing the last private tab ends
that session; the next session uses a new profile. Each private window has its
own session and never replaces the regular window's saved state. Extensions,
imports and exports are managed in regular windows. Private popups open as tabs;
sign-in flows that require a popup's opener may need a regular window.

Downloads, copied text and explicitly saved files remain. This is not a promise
that Windows, crash dumps or other software leave no traces.

**Private mode does not hide destinations from a Wi-Fi administrator or ISP.**
HTTPS encrypts page content and search terms in transit, but connection metadata
and often site names remain visible. Network privacy needs a trusted VPN with
leak protection and a kill switch, or Tor Browser. Brisklark has no built-in VPN
or Tor tunnel and does not report your connection as protected. Settings >
Privacy includes a shortcut to Windows VPN settings. A proxy by itself is not
an encrypted tunnel. Websites can still identify you when you sign in.

See [Mozilla's private browsing explanation](https://support.mozilla.org/en-US/kb/private-browsing-use-firefox-without-history)
and [Tor's guidance on other browsers](https://support.torproject.org/tor-browser/security/using-tor-with-other-browsers/).

## Build

```powershell
cargo build --release   # target\release\brisklark.exe
cargo test              # add -- --ignored to also check the downloaded filter lists
```

Needs Rust (MSVC toolchain) and the Visual Studio C++ x64 libraries. If linking
fails with `LNK1104: cannot open file 'msvcrt.lib'`, your newest Visual Studio
is missing them: add "MSVC x64/x86 build tools" in the Visual Studio Installer,
or build from an older complete install's "x64 Native Tools" prompt.

## Your data

Everything stays on your PC. Tabs, bookmarks and settings are in
`%APPDATA%\Brisklark\state.json`, history in `history.json` beside it; cookies
and cache in `%LOCALAPPDATA%\Brisklark\WebView2`. The search engine is the
`engine` field in `state.json` (`%s` is the query).

## License

MIT. See [LICENSE](LICENSE).
