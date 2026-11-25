# Brisklark — product spec

The brief Brisklark is built against. Every change should be checked against it,
and the status column kept true.

## The idea, in one paragraph

A browser that gets out of the way and costs almost nothing to run. A main window
with optional private windows: a row of tabs across the top in place of Windows' title bar, one field, and the
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
   real disagreements (search engine), not to hide unfinished work.

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
| Tabs across the top, hideable (Ctrl+S) | ✅ |
| `+` right after the last tab; a new tab stays until Esc on one nobody has typed into | ✅ |
| Right-click menu: New tab to the right, Reload, Duplicate, Pin/Unpin, Mute/Unmute, Split view with ▸, Separate split view, Bookmark, Copy link, Close, Close other tabs, Close tabs to the right | ✅ |
| Pinned tabs as icons, first in the row | ✅ |
| Private tabs and separate private windows; isolated InPrivate sessions; no private history or session restore | ✅ |
| Audio indicator on tabs playing sound; click it to mute | ✅ |
| Split view: two tabs side by side, draggable divider, the clicked pane gets the keys, pairs kept across restarts | ✅ |
| Middle-click closes; Ctrl+1–9; Ctrl+Tab; Ctrl+K switcher | ✅ |
| Drag to reorder tabs | later |
| Park tabs: off the strip and out of memory, kept on disk until opened | ✅ |
| Close duplicate tabs, keeping the one you are on | ✅ |
| Tab groups, tab hover previews, tab sleeping timer setting | later |

## Field and navigation

| What | Status |
|---|---|
| The bar is the field: address or search, suggestions from open tabs, bookmarks, history | ✅ |
| Search engine choice (Google, DuckDuckGo, Bing, Startpage, Ecosia, Kagi, Qwant) | ✅ |
| Live search suggestions from the engine (sends keystrokes: off by default) | later |
| Back/forward/reload buttons for mouse users | ✅ |
| Copy as Markdown (Ctrl+Shift+M): the selection or the page, with its title and link | ✅ |
| Empty tabs show bookmarked websites as clickable tiles; add/edit names and URLs; confirmed removal; website icons with letter fallback | ✅ |
| Home settings: show/hide bookmarks, small/medium/large icons, show/hide labels; saved across restarts | ✅ |

## Settings (Ctrl+, or ⚙)

One panel, sections in this order, every change applies immediately and is saved:

1. **General** — On startup; search engine; import and export.
   **Home** — Show bookmarked websites; icon size; website names. The Customize
   button on Home opens this section. Private start pages remain separate.
2. **Appearance** — Hide the tab bar; page zoom.
3. **Shields** — Block ads and trackers; Blocking level; Block cookie notices; the
   site-specific switches; filter lists: last updated, Update now.
4. **Privacy** — Clear browsing history; Clear cookies and site data; Clear cached
   files; private tabs and separate private windows (`Ctrl+Shift+N`). Private
   sessions use isolated InPrivate profiles, never restore/save private tabs,
   and do not overwrite the regular session. Password saving and form autofill
   are disabled in private profiles. The indicator and start page explain that
   private mode does not hide network destinations. No built-in VPN/Tor tunnel.
5. **Passwords and autofill** — Offer to save passwords; fill addresses and forms.
6. **Extensions** — Install from the Chrome Web Store or a folder; installed list.
7. **Downloads** — The folder; show the downloads list.
8. **Languages** — The language websites are asked for.
9. **System** — Graphics acceleration; Windows proxy; accessibility; relaunch; reset.
10. **Goodies** — Copy as Markdown; duplicate tabs; parked tabs.
11. **About** — version, source link, licences.

Status: ✅ all six. Later: downloads folder, default zoom, homepage, import from
Chrome/Edge/Brave, password manager settings (WebView2 autofill already works).

## Keyboard

Everything in the README table ✅, plus `Ctrl+,` settings ✅.

## Quality bar for each change

- `cargo test` passes; filter-list tests pass against the real lists (`-- --ignored`).
- Release build launches, restores the session, and the host stays small.
- New UI checked in light and dark, and with the tabs folded away.
