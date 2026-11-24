// Brave's Standard fingerprinting protection ("farbling"), reproduced in page
// script: canvas and audio readbacks, hardware, screen, plugins and voices are
// perturbed with a seed that is per site and changes every session.
// Ported from brave-core (brave_session_cache.cc and friends); see SPEC.md.
(() => {
'use strict';
const SEED = '__SEED__';
const OFF = new Set(__OFF__); // Brave's per-site webcompat exceptions
if (/^(file|chrome|edge|devtools|chrome-extension|extension):$/.test(location.protocol)) return;
// ---- sync SHA-256 / HMAC (Brave: crypto::hmac::SignSha256(token, data)) ----
const P = []; for (let n = 2; P.length < 64; n++) if (P.every(p => n % p)) P.push(n);
const frac = x => ((x - Math.floor(x)) * 4294967296) >>> 0;
const K = P.map(p => frac(Math.cbrt(p))), H0 = P.slice(0, 8).map(p => frac(Math.sqrt(p)));
const ror = (x, n) => (x >>> n) | (x << (32 - n));
function sha256(b) {
  const l = b.length, n = ((l + 72) >> 6) << 6, m = new Uint8Array(n); m.set(b); m[l] = 0x80;
  const dv = new DataView(m.buffer); dv.setUint32(n - 4, (l * 8) >>> 0); dv.setUint32(n - 8, Math.floor(l / 0x20000000));
  const h = H0.slice(), w = new Uint32Array(64);
  for (let o = 0; o < n; o += 64) {
    for (let i = 0; i < 16; i++) w[i] = dv.getUint32(o + i * 4);
    for (let i = 16; i < 64; i++) { const a = w[i - 15], c = w[i - 2];
      w[i] = ((ror(a, 7) ^ ror(a, 18) ^ (a >>> 3)) + w[i - 16] + (ror(c, 17) ^ ror(c, 19) ^ (c >>> 10)) + w[i - 7]) >>> 0; }
    let [A, B, C, D, E, F, G, H] = h;
    for (let i = 0; i < 64; i++) {
      const t1 = (H + (ror(E, 6) ^ ror(E, 11) ^ ror(E, 25)) + ((E & F) ^ (~E & G)) + K[i] + w[i]) >>> 0;
      const t2 = ((ror(A, 2) ^ ror(A, 13) ^ ror(A, 22)) + ((A & B) ^ (A & C) ^ (B & C))) >>> 0;
      H = G; G = F; F = E; E = (D + t1) >>> 0; D = C; C = B; B = A; A = (t1 + t2) >>> 0;
    }
    [A, B, C, D, E, F, G, H].forEach((x, i) => { h[i] = (h[i] + x) >>> 0; });
  }
  const out = new Uint8Array(32), ov = new DataView(out.buffer); h.forEach((x, i) => ov.setUint32(i * 4, x)); return out;
}
const utf8 = s => new TextEncoder().encode(s);
const TOKEN = sha256(utf8('brisklark-farble|' + SEED)).slice(0, 16); // stands in for Brave's 128-bit farbling_token
function hmac(msg) {
  const k = new Uint8Array(64); k.set(TOKEN);
  const i = new Uint8Array(64 + msg.length), o = new Uint8Array(96);
  for (let j = 0; j < 64; j++) { i[j] = k[j] ^ 0x36; o[j] = k[j] ^ 0x5c; }
  i.set(msg, 64); o.set(sha256(i), 64); return sha256(o);
}
const M64 = (1n << 64n) - 1n, TOP = 0xC000000000000000n;
const lfsr = v => (v >> 1n) | (((v << 62n) ^ (v << 61n)) & TOP);     // brave_session_cache.cc lfsr_next
const u64 = (b, off) => new DataView(b.buffer, b.byteOffset).getBigUint64(off, true);
const TH = u64(TOKEN, 0), TL = u64(TOKEN, 8);
function prng(key) { // stand-in for FarblingPRNG(high ^ low ^ key) (Brave uses absl randen; splitmix64 here)
  let s = (TH ^ TL ^ BigInt(key)) & M64;
  return () => { s = (s + 0x9E3779B97F4A7C15n) & M64; let z = s;
    z = ((z ^ (z >> 30n)) * 0xBF58476D1CE4E5B9n) & M64; z = ((z ^ (z >> 27n)) * 0x94D049BB133111EBn) & M64; return z ^ (z >> 31n); };
}
const rnd = (mod, key = 0, discard = 0) => { const g = prng(key); while (discard--) g(); return Number(g() % BigInt(mod)); };
const LET = 'ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789';
function randStr(seed, len) { let v = u64(hmac(utf8(seed)), 0), s = ''; while (len--) { s += LET[Number(v % 62n)]; v = lfsr(v); } return s; }
// PerturbPixelsInternal: 32-byte HMAC of the pixels picks 512 R/G/B bytes and XORs their low bit
function perturb(d) {
  const n = d.length >> 2; if (!n) return d;
  let src = new Uint8Array(d.buffer, d.byteOffset, d.length);
  if (src.length > 262144) { // keep big readbacks fast: hash a fixed stride sample (Brave hashes everything, natively)
    const step = Math.ceil(src.length / 65536), s = new Uint8Array(65540); new DataView(s.buffer).setUint32(65536, src.length);
    for (let i = 0, j = 0; j < src.length; i++, j += step) s[i] = src[j]; src = s; }
  const key = hmac(src), N = BigInt(n);
  let v = u64(key, 0);
  for (const k of key) { let bit = k;
    for (let j = 0; j < 16; j++) { if (j % 8 === 0) bit = k;
      d[4 * Number(v % N) + Number(v % 3n)] ^= bit & 1; bit >>= 1; v = lfsr(v); } }
  return d;
}
const fudge = 0.999 + Number(TH) / 18446744073709551615 / 1000;   // GetAudioFarblingHelper
const dbOff = 20 * Math.log10(fudge);
// ---- patching, with Function.prototype.toString masking ----
const natives = new WeakMap(), done = new WeakSet();
function patch(W) {
  if (!W || done.has(W)) return; done.add(W);
  const fts = W.Function.prototype.toString;
  const show = f => natives.has(f) ? natives.get(f) : fts.call(f);
  const ts = { toString() { return show(this); } }.toString; natives.set(ts, fts.call(fts));
  Object.defineProperty(W.Function.prototype, 'toString', { value: ts, writable: true, configurable: true });
  const hook = (o, name, fn) => { const d = o && Object.getOwnPropertyDescriptor(o, name); if (!d || typeof d.value !== 'function') return;
    const orig = d.value, f = { [name](...a) { return fn(orig, this, a); } }[name];
    natives.set(f, fts.call(orig)); Object.defineProperty(f, 'length', { value: orig.length }); d.value = f; Object.defineProperty(o, name, d); return orig; };
  const get = (o, name, fn) => { const d = o && Object.getOwnPropertyDescriptor(o, name); if (!d || !d.get) return;
    const orig = d.get, g = Object.getOwnPropertyDescriptor({ get [name]() { return fn(orig, this); } }, name).get;
    natives.set(g, fts.call(orig)); d.get = g; Object.defineProperty(o, name, d); return orig; };
  const set = (o, name, fn) => { const d = o && Object.getOwnPropertyDescriptor(o, name); if (!d || !d.set) return;
    const orig = d.set, s = Object.getOwnPropertyDescriptor({ set [name](v) { fn(orig, this, v); } }, name).set;
    natives.set(s, fts.call(orig)); d.set = s; Object.defineProperty(o, name, d); };
  if (!OFF.has('canvas')) {
  // Canvas: getImageData, toDataURL, toBlob, convertToBlob (readback only; the canvas itself is untouched)
  const gids = [W.CanvasRenderingContext2D, W.OffscreenCanvasRenderingContext2D].map(C =>
    hook(C && C.prototype, 'getImageData', (o, t, a) => { const r = o.apply(t, a); perturb(r.data); return r; }));
  const copy = (src, make) => { try { const w = src.width, h = src.height; if (!w || !h || w * h > 16777216) return src;
    const c = make(w, h), x = c.getContext('2d'); x.drawImage(src, 0, 0);
    const id = gids[x instanceof W.CanvasRenderingContext2D ? 0 : 1].call(x, 0, 0, w, h); perturb(id.data); x.putImageData(id, 0, 0); return c; } catch (e) { return src; } };
  const mk = (w, h) => { const c = W.document.createElement('canvas'); c.width = w; c.height = h; return c; };
  const HC = W.HTMLCanvasElement && W.HTMLCanvasElement.prototype;
  hook(HC, 'toDataURL', (o, t, a) => o.apply(copy(t, mk), a));
  hook(HC, 'toBlob', (o, t, a) => o.apply(copy(t, mk), a));
  if (W.OffscreenCanvas) hook(W.OffscreenCanvas.prototype, 'convertToBlob', (o, t, a) => o.apply(copy(t, (w, h) => new W.OffscreenCanvas(w, h)), a));
  }
  if (!OFF.has('audio')) {
  // Audio (Standard = multiply by fudge factor in [0.999, 1.0))
  const AB = W.AudioBuffer && W.AudioBuffer.prototype, fudged = new WeakSet();
  hook(AB, 'getChannelData', (o, t, a) => { const r = o.apply(t, a); if (!fudged.has(r)) { fudged.add(r); for (let i = 0; i < r.length; i++) r[i] *= fudge; } return r; });
  hook(AB, 'copyFromChannel', (o, t, a) => { o.apply(t, a); const d = a[0], n = Math.min(d.length, t.length - (a[2] >>> 0)); for (let i = 0; i < n; i++) d[i] *= fudge; });
  const AN = W.AnalyserNode && W.AnalyserNode.prototype;
  const ffd = hook(AN, 'getFloatFrequencyData', (o, t, a) => { o.apply(t, a); const d = a[0]; for (let i = 0; i < d.length; i++) d[i] += dbOff; });
  const ftd = hook(AN, 'getFloatTimeDomainData', (o, t, a) => { o.apply(t, a); const d = a[0]; for (let i = 0; i < d.length; i++) d[i] *= fudge; });
  const clip = v => v < 0 ? 0 : v > 255 ? 255 : v;
  hook(AN, 'getByteFrequencyData', (o, t, a) => { const d = a[0], f = new Float32Array(Math.min(d.length, t.frequencyBinCount)); ffd.call(t, f);
    const lo = t.minDecibels, sc = 255 / (t.maxDecibels - lo); for (let i = 0; i < f.length; i++) d[i] = clip((f[i] + dbOff - lo) * sc); });
  hook(AN, 'getByteTimeDomainData', (o, t, a) => { const d = a[0], f = new Float32Array(Math.min(d.length, t.fftSize)); ftd.call(t, f);
    for (let i = 0; i < f.length; i++) d[i] = clip(128 * (f[i] * fudge + 1)); });
  }
  // Navigator
  const NP = W.Navigator && W.Navigator.prototype;
  if (!OFF.has('hardware-concurrency')) get(NP, 'hardwareConcurrency', (o, t) => { const v = o.call(t); return v < 4 ? v : 4 + rnd(v + 1 - 4, 1); });
  get(NP, 'deviceMemory', (o, t) => { const vals = [2, 4, 8, 16, 32]; let i = vals.indexOf(o.call(t)); if (i < 0) i = 4; return i <= 1 ? 4 : vals[1 + rnd(i, 2)]; });
  let langs; if (!OFF.has('language')) get(NP, 'languages', (o, t) => langs || (langs = Object.freeze(o.call(t).slice(0, 1))));
  if (!OFF.has('keyboard')) get(NP, 'keyboard', () => null);
  const BM = W.BatteryManager && W.BatteryManager.prototype;
  for (const [k, v] of [['charging', true], ['chargingTime', 0], ['dischargingTime', Infinity], ['level', 1]]) get(BM, k, () => v);
  hook(W.MediaDevices && W.MediaDevices.prototype, 'enumerateDevices', (o, t, a) => o.apply(t, a).then(l => {
    if (l.length <= 2) return l; const g = prng(3), r = l.slice();
    for (let i = r.length - 1; i > 1; i--) { const j = 1 + Number(g() % BigInt(i)); [r[i], r[j]] = [r[j], r[i]]; } return r; }));
  // Plugins: rename Chrome PDF plugin(s), add two random plugins, shuffle
  let plugs; if (!OFF.has('plugins')) get(NP, 'plugins', (o, t) => plugs || (plugs = fakePlugins(W, [...o.call(t)])));
  // Speech voices: real list + one fake copy of the default voice
  const SS = W.SpeechSynthesis && W.SpeechSynthesis.prototype, real = new WeakMap(); let fake;
  hook(SS, 'getVoices', (o, t, a) => { const v = o.apply(t, a), d = v.find(x => x.default); if (!d) return v;
    if (!fake) { const NAMES = ['Hubert', 'Vernon', 'Rudolph', 'Clayton', 'Irving', 'Wilson', 'Alva', 'Harley', 'Beauregard', 'Cleveland', 'Cecil', 'Reuben', 'Sylvester', 'Jasper'];
      const name = NAMES[rnd(NAMES.length, 4)];
      fake = Object.create(W.SpeechSynthesisVoice.prototype, { voiceURI: { get: () => d.voiceURI }, name: { get: () => name },
        lang: { get: () => d.lang }, localService: { get: () => d.localService }, default: { get: () => false } }); real.set(fake, d); }
    v.push(fake); return v; });
  const SU = W.SpeechSynthesisUtterance && W.SpeechSynthesisUtterance.prototype, picked = new WeakMap();
  set(SU, 'voice', (o, t, v) => { picked.set(t, v); o.call(t, real.get(v) || v); });
  get(SU, 'voice', (o, t) => picked.has(t) && real.has(picked.get(t)) ? picked.get(t) : o.call(t));
  // WebGL2 getParameter: 12 limits, each maybe minus 1
  const GL2 = W.WebGL2RenderingContext;
  if (GL2 && !OFF.has('webgl2')) { const m = new Map(['MAX_VERTEX_UNIFORM_COMPONENTS', 'MAX_VERTEX_UNIFORM_BLOCKS', 'MAX_VERTEX_OUTPUT_COMPONENTS', 'MAX_VARYING_COMPONENTS',
      'MAX_TRANSFORM_FEEDBACK_INTERLEAVED_COMPONENTS', 'MAX_FRAGMENT_UNIFORM_COMPONENTS', 'MAX_FRAGMENT_UNIFORM_BLOCKS', 'MAX_FRAGMENT_INPUT_COMPONENTS',
      'MAX_UNIFORM_BUFFER_BINDINGS', 'MAX_COMBINED_UNIFORM_BLOCKS', 'MAX_COMBINED_VERTEX_UNIFORM_COMPONENTS', 'MAX_COMBINED_FRAGMENT_UNIFORM_COMPONENTS'].map((n, i) => [GL2[n], i + 1]));
    hook(GL2.prototype, 'getParameter', (o, t, a) => { const v = o.apply(t, a), k = m.get(a[0]);
      return k && v > 0 && rnd(2, 0, k) ? (typeof v === 'bigint' ? v - 1n : v - 1) : v; }); }
  if (!OFF.has('screen')) {
  // Screen (kBraveBlockScreenFingerprinting, on by default on desktop)
  const off = k => rnd(9, 10 + k);   // FarbledInteger(key, spoof, 0, 8)
  const top = () => { try { return W.top.innerWidth >= 0 ? W.top : W; } catch (e) { return W; } };
  const ow = () => top().innerWidth + off(1), oh = () => top().innerHeight + off(2);
  get(W, 'outerWidth', ow); get(W, 'outerHeight', oh);
  for (const [k, n] of [['screenX', 3], ['screenLeft', 3], ['screenY', 4], ['screenTop', 4]]) get(W, k, () => off(n));
  const SIZES = [[1280, 800], [1366, 768], [1440, 900], [1680, 1050], [1920, 1080], [2560, 1440], [3840, 2160]];
  const scr = () => { const w = ow(), h = oh(); return SIZES.find(s => s[0] >= w && s[1] >= h) || SIZES[6]; };
  const SP = W.Screen && W.Screen.prototype;
  get(SP, 'width', () => scr()[0]); get(SP, 'availWidth', () => scr()[0]);
  get(SP, 'height', () => scr()[1]); get(SP, 'availHeight', () => scr()[1]);
  get(SP, 'availLeft', () => 0); get(SP, 'availTop', () => 0); get(SP, 'isExtended', () => false);
  const ME = W.MouseEvent && W.MouseEvent.prototype;
  get(ME, 'screenX', (o, t) => t.clientX + off(5)); get(ME, 'screenY', (o, t) => t.clientY + off(6));
  }
  // Same-origin child frames / popups (about:blank, srcdoc, javascript:) get the same treatment
  for (const E of [W.HTMLIFrameElement, W.HTMLFrameElement, W.HTMLObjectElement]) {
    get(E && E.prototype, 'contentWindow', (o, t) => { const w = o.call(t); try { w && w.document && patch(w); } catch (e) {} return w; });
    get(E && E.prototype, 'contentDocument', (o, t) => { const d = o.call(t); try { d && patch(d.defaultView); } catch (e) {} return d; }); }
  hook(W, 'open', (o, t, a) => { const w = o.apply(t, a); try { w && w.document && patch(w); } catch (e) {} return w; });
}
function fakePlugins(W, list) {
  const g = prng(5), pick = a => a[Number(g() % BigInt(a.length))];
  const mkMime = (type, desc, suf, p) => Object.create(W.MimeType.prototype, { type: { value: type }, description: { value: desc }, suffixes: { value: suf }, enabledPlugin: { value: p } });
  const mkPlugin = (name, filename, description, types, mdesc, suf) => {
    const p = Object.create(W.Plugin.prototype, { name: { value: name }, filename: { value: filename }, description: { value: description }, length: { value: types.length } });
    types.forEach((t, i) => { p[i] = mkMime(t, mdesc, suf, p); }); p.item = i => p[i] || null; p.namedItem = n => types.includes(n) ? p[types.indexOf(n)] : null; return p; };
  list = list.map(p => /^Chrome PDF (Plugin|Viewer)$/.test(p.name) ? mkPlugin(
    pick(['Chrome ', 'Chromium ', 'Brave ', 'Web ', 'Browser ', 'OpenSource ', 'Online ', 'JavaScript ', '']) +
    pick(['PDF ', 'Portable Document Format ', 'portable-document-format ', 'document ', 'doc ', 'PDF and PS ', 'com.adobe.pdf ']) +
    pick(['Viewer', 'Renderer', 'Display', 'Plugin', 'plug-in', 'plug in', 'extension', '']),
    randStr(p.filename, 32), p.description, ['application/pdf', 'text/pdf'], p.description, 'pdf') : p);
  for (const [n, a, b, c] of [[1, 8, 16, 32], [2, 7, 15, 31]]) list.push(mkPlugin(randStr('PLUGIN_' + n + '_NAME', a), randStr('PLUGIN_' + n + '_FILENAME', b),
    randStr('PLUGIN_' + n + '_DESCRIPTION', c), ['', ''], randStr('MIME_' + n + '_DESCRIPTION', 32), randStr('PLUGIN_' + n + '_EXTENSION', 3)));
  for (let i = list.length - 1; i > 0; i--) { const j = Number(g() % BigInt(i + 1)); [list[i], list[j]] = [list[j], list[i]]; }
  const arr = Object.create(W.PluginArray.prototype);
  list.forEach((p, i) => { arr[i] = p; if (p.name) arr[p.name] = p; });
  Object.defineProperties(arr, { length: { value: list.length }, item: { value: i => list[i] || null }, namedItem: { value: n => list.find(p => p.name === n) || null },
    refresh: { value: () => {} }, [Symbol.iterator]: { value: () => list[Symbol.iterator]() } });
  return arr;
}
patch(window);
})();