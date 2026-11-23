# Brisklark — product spec

The brief Brisklark is built against. Every change should be checked against it,
and the status column kept true.

## The idea, in one paragraph

A browser that gets out of the way and costs almost nothing to run. One window:
a row of tabs (left or top) in place of Windows' title bar, one field, and the
page. Native Windows engine (WebView2), Rust host of a few MB, Brave-grade ad
blocking on by default. Every feature earns its place by being something people
reach for daily in Chrome, Edge, Brave or Arc; anything else is left out.

## Principles (apply to every decision)

1. **Fast and light first.** Nothing runs per frame. Nothing loads until it's
   needed (tabs restore unloaded, panels build on open). Background tabs drop to
   low memory. Measure host RAM and exe size after big changes.
2. **Familiar over clever.** Use the gesture people already know from Chrome/Edge:
   right-click a tab for its menu, middle-click closes, Ctrl+T/W/Shift+T, drag the
   empty tab row to move the window, double-click to maximise.
3. **Native where Windows has it.** Native context menus (dark in dark mode), native
   window shadow and snap, WebView2's own find, print, downloads, autofill, PDF.
4. **Nothing is lost by accident.** Closing tabs can be undone (Ctrl+Shift+T).
   Clearing data asks first. A click outside a panel closes the panel, nothing else.
5. **Private by default, no account, no telemetry.** The only traffic of ours is
   the filter-list refresh.
6. **Every setting has a default that's right for most people.** Settings exist for
   real disagreements (tab position, search engine), not to hide unfinished work.

## Ad blocking ("Shields") — parity with Brave Standard

| What | Status |
|---|---|
| Brave's default lists from its own catalog: uBO filters (all years, general, badware, resource-abuse, unbreak, quick fixes, link shorteners), EasyList, URLhaus, Brave unbreak/specific/social/sugarcoat, EasyPrivacy, uBO privacy | ✅ |
| Brave's first-party list applied to a site's own requests; everything else only cross-site (Brave Standard mode) | ✅ |
| Cookie-notice blocking (Fanboy Cookiemonster, uBO cookie annoyances, Brave cookie list), on by default, switchable | ✅ |
| Scriptlets from Brave's resources.json (YouTube and other in-page ad defences) | ✅ |
| `$redirect` rules answered with the stub resource instead of a block (fewer broken players) | ✅ |
| Site-specific cosmetic hiding before first paint | ✅ |
| Generic cosmetic hiding: page classes/ids scanned as they appear, matching rules injected | ✅ |
| Shields per site: one switch, remembered; blocked-count badge on the shield button | ✅ |
| Lists refreshed every 4 days in the background; "Update now" in Settings | ✅ |
| Aggressive mode (first-party blocking from all lists) | later |
| Fingerprinting protection, HTTPS-only mode | later |
| Regional lists (pick by language) | later |

## Tabs

| What | Status |
|---|---|
| Tabs left (sidebar) or top, switchable (Ctrl+Shift+S), hideable (Ctrl+S) | ✅ |
| `+` right after the last tab; New Tab is a real tab that costs nothing until used | ✅ |
| Right-click menu: New tab to the right, Reload, Duplicate, Pin/Unpin, Mute/Unmute, Split view with ▸, Separate split view, Bookmark, Copy link, Close, Close other tabs, Close tabs to the right/below | ✅ |
| Pinned tabs as icons, first in the row | ✅ |
| Audio indicator on tabs playing sound; click it to mute | ✅ |
| Split view: two tabs side by side, draggable divider, the clicked pane gets the keys, pairs kept across restarts | ✅ |
| Middle-click closes; Ctrl+1–9; Ctrl+Tab; Ctrl+K switcher | ✅ |
| Drag to reorder tabs | later |
| Tab groups, tab hover previews, tab sleeping timer setting | later |

## Field and navigation

| What | Status |
|---|---|
| One field: address or search, suggestions from open tabs, bookmarks, history | ✅ |
| Search engine choice (Google, DuckDuckGo, Bing, Brave, Startpage, Ecosia, Kagi, Qwant) | ✅ |
| Live search suggestions from the engine (sends keystrokes: off by default) | later |
| Back/forward/reload buttons for mouse users | later |

## Settings (Ctrl+, or ⚙)

One panel, sections in this order, every change applies immediately and is saved:

1. **Appearance** — Tabs on the left / across the top; hide the tab bar.
2. **Search** — Search engine.
3. **On startup** — Continue where you left off / Open a new tab (pinned tabs kept).
4. **Shields** — Block ads and trackers; Block cookie notices; Sites with Shields
   off (each removable); filter lists: last updated, Update now.
5. **Privacy** — Clear browsing history; Clear cookies and site data; Clear cached
   files. Each asks for a second click to confirm.
6. **About** — version, source link, licences.

Status: ✅ all six. Later: downloads folder, default zoom, homepage, import from
Chrome/Edge/Brave, password manager settings (WebView2 autofill already works).

## Keyboard

Everything in the README table ✅, plus `Ctrl+,` settings ✅.

## Quality bar for each change

- `cargo test` passes; filter-list tests pass against the real lists (`-- --ignored`).
- Release build launches, restores the session, and the host stays small.
- New UI checked in both tab layouts and in light and dark.
