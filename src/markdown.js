// Runs in the page: turns the selection, or the whole page, into Markdown.
// The result is a JSON object the host puts on the clipboard.
(function () {
  const SKIP = new Set(["SCRIPT", "STYLE", "NOSCRIPT", "SVG", "CANVAS", "IFRAME",
    "FORM", "BUTTON", "INPUT", "SELECT", "TEXTAREA", "NAV", "HEADER", "FOOTER", "ASIDE"]);
  const abs = (href) => { try { return new URL(href, location.href).href; } catch { return href; } };

  function walk(n) {
    if (n.nodeType === 3) return n.nodeValue.replace(/\s+/g, " ");
    if (n.nodeType !== 1) return "";
    const t = n.tagName;
    if (SKIP.has(t)) return "";
    const kids = () => Array.from(n.childNodes, walk).join("");
    if (t === "BR") return "\n";
    if (/^H[1-6]$/.test(t)) {
      const s = kids().trim();
      return s ? "\n\n" + "#".repeat(+t[1]) + " " + s + "\n\n" : "";
    }
    if (t === "P") { const s = kids().trim(); return s ? "\n\n" + s + "\n\n" : ""; }
    if (t === "STRONG" || t === "B") { const s = kids().trim(); return s ? "**" + s + "**" : ""; }
    if (t === "EM" || t === "I") { const s = kids().trim(); return s ? "*" + s + "*" : ""; }
    if (t === "CODE") return "`" + n.textContent.trim().replace(/`+/g, "'") + "`";
    if (t === "PRE") return "\n\n```\n" + n.textContent.replace(/\n+$/, "") + "\n```\n\n";
    if (t === "A") {
      const s = kids().trim();
      const href = n.getAttribute("href") || "";
      if (!s) return "";
      if (!href || href.startsWith("javascript:")) return s;
      return "[" + s + "](" + abs(href) + ")";
    }
    if (t === "IMG") {
      const alt = n.getAttribute("alt") || "";
      return alt ? "![" + alt + "](" + abs(n.getAttribute("src") || "") + ")" : "";
    }
    if (t === "HR") return "\n\n---\n\n";
    if (t === "BLOCKQUOTE") {
      const s = kids().trim();
      return s ? "\n\n" + s.split("\n").map(l => "> " + l).join("\n") + "\n\n" : "";
    }
    if (t === "UL" || t === "OL") {
      const items = Array.from(n.children).filter(c => c.tagName === "LI");
      const lines = items.map((li, i) => {
        const s = walk(li).trim().replace(/\n/g, "\n   ");
        return t === "OL" ? (i + 1) + ". " + s : "- " + s;
      });
      return lines.length ? "\n\n" + lines.join("\n") + "\n\n" : "";
    }
    if (t === "LI") return kids();
    if (t === "TABLE") {
      const rows = Array.from(n.querySelectorAll("tr")).map(tr =>
        Array.from(tr.children).map(c => walk(c).trim().replace(/\n+/g, " ")).join(" | ")).filter(r => r.trim());
      return rows.length ? "\n\n" + rows.join("\n") + "\n\n" : "";
    }
    return kids();
  }

  const sel = window.getSelection();
  const partial = !!(sel && sel.rangeCount && !sel.isCollapsed);
  let md;
  if (partial) {
    const box = document.createElement("div");
    box.appendChild(sel.getRangeAt(0).cloneContents());
    md = walk(box);
  } else {
    md = walk(document.body);
  }
  md = md.replace(/[ \t]+\n/g, "\n").replace(/[ \t]{2,}/g, " ").replace(/\n{3,}/g, "\n\n").trim();
  if (!md) return { ok: false };

  const title = (document.title || location.host || "Brisklark").trim();
  md = partial
    ? md + "\n\n— [" + title + "](" + location.href + ")"
    : "# " + title + "\n\n<" + location.href + ">\n\n" + md;
  return { ok: true, md: md };
})()
