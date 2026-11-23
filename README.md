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

- Tabs down the left or across the top, in place of Windows' title bar
- Right-click a tab: new tab to the right, reload, duplicate, pin, mute, split
  view, bookmark, copy link, close others / to the right
- Split view: two tabs side by side, drag the divider; the pair is remembered
- Speaker icon on tabs playing sound; click it to mute
- Settings (`Ctrl+,` or ⚙): tab position, search engine, on startup, Shields,
  clear history / cookies / cache
- Pinned tabs and private tabs
- One field for addresses and searches, suggesting from history, bookmarks and open tabs
- History and bookmarks panels
- Your session restored on launch; restored tabs aren't loaded until clicked,
  and hidden tabs drop to WebView2's low-memory level

`+` or `Ctrl+T` adds a New Tab; Esc or a click outside the box on one you
haven't typed into takes it away again. Middle-click a tab to close it. Drag the
empty part of the tab row to move the window; double-click it to maximise.

| | |
|---|---|
| `Ctrl+L` address · `Ctrl+K` switch tab · `Ctrl+T` new tab · `Ctrl+W` close · `Ctrl+Shift+T` reopen | `Ctrl+[` `Ctrl+]` back, forward · `Ctrl+Tab`, `Ctrl+Shift+[` `]` previous/next tab · `Ctrl+1`–`9` jump |
| `Ctrl+Shift+S` tabs left/top · `Ctrl+S` hide the tabs · `Ctrl+Shift+B` bookmark | `Ctrl+D` duplicate · `Ctrl+Shift+C` copy address · `Ctrl+Shift+V` paste and go |
| `Ctrl+Y` history · `Ctrl+Shift+O` bookmarks · `Ctrl+Shift+N` private tab · `Ctrl+,` settings | `Ctrl+F` find, `Ctrl+P` print, `F5` reload: WebView2's own |

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
