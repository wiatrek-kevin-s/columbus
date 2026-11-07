// Shared helpers for the main window. Everything shown that comes from the
// AI, data, files or a session is set as text (textContent, DOM nodes),
// never as markup. The only markup this file builds is the app's own icons,
// from fixed strings below.

const T = window.__TAURI__;
export const invoke = (cmd, args) => T.core.invoke(cmd, args);
export const listen = (ev, fn) => T.event.listen(ev, fn);

export const $ = (id) => document.getElementById(id);

/** An element with a class and text (text only, never markup). */
export function el(tag, cls, text) {
  const e = document.createElement(tag);
  if (cls) e.className = cls;
  if (text != null) e.textContent = String(text);
  return e;
}

/** An element with children (nodes or strings, as text). */
export function h(tag, attrs, ...kids) {
  const e = document.createElement(tag);
  for (const [k, v] of Object.entries(attrs || {})) {
    if (v == null || v === false) continue;
    if (k === "class") e.className = v;
    else if (k === "text") e.textContent = String(v);
    else if (k.startsWith("on") && typeof v === "function") e.addEventListener(k.slice(2), v);
    else if (k === "dataset") Object.assign(e.dataset, v);
    else e.setAttribute(k, v === true ? "" : String(v));
  }
  for (const k of kids.flat()) {
    if (k == null || k === false) continue;
    e.append(k instanceof Node ? k : document.createTextNode(String(k)));
  }
  return e;
}

export function button(label, cls, onclick, attrs = {}) {
  const b = h("button", { type: "button", class: `btn ${cls || ""}`.trim(), ...attrs }, label);
  if (onclick) b.addEventListener("click", onclick);
  return b;
}

// The app's own icons (fixed paths, design's rail set).
const ICONS = {
  home: '<path d="M3 10.5 10 4l7 6.5V17H12.5v-4h-5v4H3z"/>',
  chats: '<path d="M3 4.5h14v9H8l-4 3v-3H3z"/>',
  projects: '<path d="M2.5 5.5h5l1.5 2h8.5v8.5h-15z"/>',
  documents: '<path d="M5 2.5h7l3.5 3.5v11.5H5z"/><path d="M12 2.5V6h3.5M7.5 10h6M7.5 13h6"/>',
  datagroups: '<path d="M10 3 17 6.5 10 10 3 6.5z"/><path d="M3 10l7 3.5 7-3.5M3 13.5 10 17l7-3.5"/>',
  skills: '<path d="M10 2.5l2 5 5 .5-3.8 3.4 1.2 5.1L10 13.8l-4.4 2.7 1.2-5.1L3 8l5-.5z"/>',
  connections: '<ellipse cx="10" cy="5" rx="6.5" ry="2.5"/><path d="M3.5 5v10c0 1.4 2.9 2.5 6.5 2.5s6.5-1.1 6.5-2.5V5M3.5 10c0 1.4 2.9 2.5 6.5 2.5s6.5-1.1 6.5-2.5"/>',
  registry: '<circle cx="10" cy="10" r="7.5"/><path d="M2.5 10h15M10 2.5c2.2 2.3 2.2 12.7 0 15M10 2.5c-2.2 2.3-2.2 12.7 0 15"/>',
  tools: '<path d="M12.5 3.5a4 4 0 0 0-4.6 5.3L3.5 13.2l3.3 3.3 4.4-4.4a4 4 0 0 0 5.3-4.6l-2.4 2.4-2.6-.6-.6-2.6z"/>',
  workflows: '<circle cx="5" cy="5" r="2"/><circle cx="15" cy="10" r="2"/><circle cx="5" cy="15" r="2"/><path d="M7 5h3a3 3 0 0 1 3 3v0M7 15h3a3 3 0 0 0 3-3"/>',
  activity: '<path d="M2.5 10h3.5l2-5 4 10 2-5h3.5"/>',
  settings: '<circle cx="10" cy="10" r="2.6"/><path d="M10 2.5v2.2M10 15.3v2.2M2.5 10h2.2M15.3 10h2.2M4.7 4.7l1.6 1.6M13.7 13.7l1.6 1.6M4.7 15.3l1.6-1.6M13.7 6.3l1.6-1.6"/>',
  search: '<circle cx="8.5" cy="8.5" r="5"/><path d="m12.5 12.5 4.5 4.5"/>',
  map: '<path d="M2.5 5 7.5 3l5 2 5-2v12l-5 2-5-2-5 2z"/><path d="M7.5 3v12M12.5 5v12"/>',
  close: '<path d="M5 5l10 10M15 5 5 15"/>',
  plus: '<path d="M10 4v12M4 10h12"/>',
};
const LOGO = '<circle cx="12" cy="12" r="10" stroke="#5ca3ee" stroke-width="1.8"/><path d="M12 4.5 14.2 12 12 19.5 9.8 12z" fill="#5ca3ee"/><path d="M12 4.5 14.2 12H9.8z" fill="#e7b04f"/>';

function svgFrom(viewBox, inner, attrs) {
  const ns = "http://www.w3.org/2000/svg";
  const s = document.createElementNS(ns, "svg");
  s.setAttribute("viewBox", viewBox);
  s.setAttribute("aria-hidden", "true");
  s.setAttribute("focusable", "false");
  for (const [k, v] of Object.entries(attrs)) s.setAttribute(k, v);
  // Fixed strings from this file only.
  const tpl = document.createElementNS(ns, "g");
  tpl.innerHTML = inner;
  s.append(...tpl.childNodes);
  return s;
}

export function icon(name) {
  return svgFrom("0 0 20 20", ICONS[name] || "", {
    fill: "none", stroke: "currentColor", "stroke-width": "1.6", "stroke-linejoin": "round", "stroke-linecap": "round",
  });
}

export function logo() {
  return svgFrom("0 0 24 24", LOGO, { fill: "none" });
}

// ---------- Text helpers ----------

export function plural(n, one, many) {
  return `${n.toLocaleString()} ${n === 1 ? one : many || `${one}s`}`;
}

export function size(b) {
  if (b == null) return "";
  if (b < 1024) return `${b} B`;
  if (b < 1024 * 1024) return `${(b / 1024).toFixed(0)} KB`;
  if (b < 1024 * 1024 * 1024) return `${(b / 1024 / 1024).toFixed(1)} MB`;
  return `${(b / 1024 / 1024 / 1024).toFixed(1)} GB`;
}

export function when(v) {
  if (v == null || v === "") return "";
  const d = typeof v === "number" ? new Date(v * 1000) : new Date(v);
  return isNaN(+d) ? String(v) : d.toLocaleString([], { dateStyle: "medium", timeStyle: "short" });
}

export function ago(v) {
  if (!v) return "";
  const d = typeof v === "number" ? new Date(v * 1000) : new Date(v);
  if (isNaN(+d)) return "";
  const min = Math.floor((Date.now() - +d) / 60000);
  if (min < 1) return "just now";
  if (min < 60) return `${min} min ago`;
  const hr = Math.floor(min / 60);
  if (hr < 24) return `${hr} h ago`;
  return when(v);
}

export function timeLeft(iso) {
  const ms = new Date(iso) - Date.now();
  if (isNaN(ms)) return "";
  if (ms <= 0) return "expired";
  const m = Math.floor(ms / 60000);
  const s = Math.floor((ms % 60000) / 1000);
  return m ? `${m} min left` : `${s} s left`;
}

export function showError(box, err) {
  if (!box) return;
  box.textContent = err ? String(err) : "";
  box.hidden = !err;
}

let toastTimer = 0;
export function toast(text) {
  const t = $("toast");
  t.textContent = text;
  t.hidden = false;
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => (t.hidden = true), 6000);
}

// ---------- Page building blocks ----------

/** The page head: title, a lede, and actions on the right. */
export function pageHead(title, lede, actions = []) {
  return h("header", { class: "pagehead" },
    h("div", {}, h("h1", { id: "page-title", tabindex: "-1" }, title), lede ? h("p", { class: "lede" }, lede) : null),
    actions.length ? h("div", { class: "actions" }, ...actions) : null);
}

/** Tabs that are links to sub-pages. */
export function tabs(items, current, label) {
  const nav = h("nav", { class: "tabs", "aria-label": label });
  for (const [key, text, href, n] of items) {
    const a = h("a", { href }, text);
    if (n != null && n !== "") a.append(h("span", { class: "n" }, String(n)));
    if (key === current) a.setAttribute("aria-current", "page");
    nav.append(a);
  }
  return nav;
}

/** An honest "coming" state: what the page will do, and that it is not built yet. */
export function coming(iconName, title, what, items = [], stage = "") {
  const box = h("section", { class: "empty", "aria-label": title });
  const ico = icon(iconName);
  ico.classList.add("ico");
  box.append(ico, h("h2", {}, title), h("p", {}, what));
  if (items.length) box.append(h("ul", {}, ...items.map((i) => h("li", {}, i))));
  box.append(h("span", { class: "stage" }, stage || "Coming: not built yet"));
  return box;
}

export function emptyState(title, text, ...actions) {
  return h("section", { class: "empty" }, h("h2", {}, title), h("p", {}, text), actions.length ? h("div", { class: "row wrap" }, ...actions) : null);
}

/** Numbered lines, one per line, never scrolled sideways. */
export function numbered(text) {
  const pre = h("pre", { class: "box numbered", tabindex: "0" });
  const lines = text === "" ? [] : String(text).replace(/\n$/, "").split("\n");
  for (const line of lines) pre.append(el("span", "ln", line === "" ? " " : line));
  return { pre, count: lines.length };
}

/** A plain dialog, closed with Esc or its buttons; resolves on close. */
export function openDialog(title, body, actions = [], label) {
  const d = h("dialog", { "aria-label": label || title });
  const box = h("div", { class: "dlg" }, h("h2", {}, title), body);
  if (actions.length) box.append(h("div", { class: "actions" }, ...actions));
  d.append(box);
  document.body.append(d);
  d.addEventListener("close", () => d.remove());
  d.showModal();
  return d;
}

/** Ask the person to confirm, in the app (no browser prompt). */
export function askConfirm(title, text, okLabel, danger = false) {
  return new Promise((resolve) => {
    let answer = false;
    const ok = h("button", { type: "button", class: `btn ${danger ? "danger" : "primary"}` }, okLabel);
    const cancel = h("button", { type: "button", class: "btn" }, "Cancel");
    const d = openDialog(title, h("p", {}, text), [h("span", { class: "spacer" }), cancel, ok]);
    ok.addEventListener("click", () => { answer = true; d.close(); });
    cancel.addEventListener("click", () => d.close());
    d.addEventListener("close", () => resolve(answer));
    cancel.focus();
  });
}

/** Ask the person for a line of text, in the app. Resolves to it, or null. */
export function askText(title, label, value = "") {
  return new Promise((resolve) => {
    let answer = null;
    const input = h("input", { type: "text", "aria-label": label, maxlength: "120" });
    input.value = value;
    const form = h("form", { class: "stack" }, h("label", { class: "field" }, label, input));
    const ok = h("button", { type: "button", class: "btn primary" }, "Save");
    const cancel = h("button", { type: "button", class: "btn" }, "Cancel");
    const d = openDialog(title, form, [h("span", { class: "spacer" }), cancel, ok]);
    const done = () => { answer = input.value.trim() || null; d.close(); };
    ok.addEventListener("click", done);
    form.addEventListener("submit", (e) => { e.preventDefault(); done(); });
    cancel.addEventListener("click", () => d.close());
    d.addEventListener("close", () => resolve(answer));
    input.focus();
    input.select();
  });
}

/** Days and times as the person's computer shows them, from "YYYY-MM". */
export function monthName(m) {
  const [y, mo] = String(m).split("-").map(Number);
  if (!y || !mo) return m;
  return new Date(y, mo - 1, 1).toLocaleString([], { month: "long", year: "numeric" });
}

// ---------- App state shared by pages ----------

export const store = {
  info: null, // shell_info
  prefs: { theme: "system", basemap: false, skipped: [], setup_done: false },
  sessions: [], // sessions_live
  inbox: [], // approvals_inbox
  chats: [], // chats_list
  tools: null, // settings_tools
  projects: null, // projects_state
  listeners: new Set(),
  set(k, v) {
    this[k] = v;
    for (const fn of this.listeners) {
      try { fn(k); } catch (e) { console.error(e); }
    }
  },
  on(fn) {
    this.listeners.add(fn);
    return () => this.listeners.delete(fn);
  },
};

export async function savePrefs(p) {
  const next = { ...store.prefs, ...p };
  await invoke("prefs_set", { prefs: next });
  store.set("prefs", next);
}

export function applyTheme(theme) {
  if (theme === "light" || theme === "dark") document.documentElement.dataset.theme = theme;
  else delete document.documentElement.dataset.theme;
}

/** "Claude desktop" for a client name the server recorded. */
export function clientName(c) {
  if (!c) return "another AI app";
  const known = {
    "claude-ai": "Claude desktop",
    "claude-code": "Claude Code",
    "columbus-chat": "Columbus chat",
    "Visual Studio Code": "VS Code",
    "github-copilot": "GitHub Copilot",
    cursor: "Cursor",
  };
  return known[c] || c;
}
