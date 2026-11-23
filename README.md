# Brisklark

A small, fast, quiet web browser for Windows, written in Rust. A row of tabs,
one field, and the page, with Brave's ad blocker built in.

It runs on **WebView2**, the Edge engine already on every Windows 11 PC, so
there's no second copy of Chromium to download or keep in memory. One ~3 MB
exe; its own process uses about 11 MB with the ad blocker loaded, and every tab
shares one Edge browser process.

Brisklark is based on the design of [Search](https://github.com/driceroland/Search),
a Mac browser by Office Commun (MIT licensed). It's an independent project, not
made or endorsed by Office Commun.

## Ad blocker

Built in and always on, using Brave's own engine
([adblock-rust](https://github.com/brave/adblock-rust)) with EasyList and
EasyPrivacy. As in Brave's Standard mode, ads and trackers from other sites are
blocked before they leave the machine; a site's requests to itself are left
alone. Each site's known ad slots are also hidden before the page draws.

The lists are fetched with Windows' `curl.exe` on first launch and every four
days after, then kept compiled in `%LOCALAPPDATA%\Brisklark\adblock.dat`, so
later launches load them in milliseconds. Until that first download finishes, a
short built-in list of ad and tracker domains is used.

Not covered yet: YouTube's own video ads (they come from YouTube itself and
need Brave's scriptlets), and generic hiding rules that need the page scanned.

## What's in it

- Tabs down the left or across the top, in place of Windows' title bar
- Pinned tabs (right-click a tab) and private tabs
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
| `Ctrl+Y` history · `Ctrl+Shift+O` bookmarks · `Ctrl+Shift+N` private tab | `Ctrl+F` find, `Ctrl+P` print, `F5` reload: WebView2's own |

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
