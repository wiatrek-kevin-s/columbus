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

/** Put children in `e`: nodes, or strings as text, alone or in a list. What
 * is not there (null, undefined, false) is left out. Use this, not
 * `e.append` or `e.replaceChildren`, wherever a child may be missing: those
 * two write a missing child into the page as the word "null" or
 * "undefined". Returns `e`. */
export function put(e, ...kids) {
  for (const k of kids.flat()) {
    if (k == null || k === false) continue;
    e.append(k instanceof Node ? k : document.createTextNode(String(k)));
  }
  return e;
}

/** An element with children (nodes or strings, as text; see `put`). */
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
  return put(e, ...kids);
}

export function button(label, cls, onclick, attrs = {}) {
  const b = h("button", { type: "button", class: `btn ${cls || ""}`.trim(), ...attrs }, label);
  if (onclick) b.addEventListener("click", onclick);
  return b;
}

// Same elements, attributes and child counts, whatever the text says.
function sameShape(a, b) {
  if (a.nodeType !== b.nodeType || a.nodeName !== b.nodeName) return false;
  if (a.nodeType === Node.ELEMENT_NODE) {
    if (a.attributes.length !== b.attributes.length) return false;
    for (const at of a.attributes) if (b.getAttribute(at.name) !== at.value) return false;
  }
  if (a.childNodes.length !== b.childNodes.length) return false;
  for (let i = 0; i < a.childNodes.length; i += 1) if (!sameShape(a.childNodes[i], b.childNodes[i])) return false;
  return true;
}

function copyText(a, b) {
  if (a.nodeType === Node.TEXT_NODE) {
    if (a.data !== b.data) a.data = b.data;
    return;
  }
  for (let i = 0; i < a.childNodes.length; i += 1) copyText(a.childNodes[i], b.childNodes[i]);
}

/** Show `kids` in `box`. A poll that changed only words (a count down, "used
 * 2 min ago") changes those words in place, so the buttons the person is
 * pointing at stay where they are; anything else is drawn afresh. Controls
 * whose action differs must differ in an attribute (data-*, href). */
export function redraw(box, ...kids) {
  const fresh = h("div", {}, ...kids);
  if (box.childNodes.length === fresh.childNodes.length
    && [...box.childNodes].every((n, i) => sameShape(n, fresh.childNodes[i]))) {
    [...box.childNodes].forEach((n, i) => copyText(n, fresh.childNodes[i]));
    return false;
  }
  box.replaceChildren(...fresh.childNodes);
  return true;
}

// The app's own icons (fixed paths on a 16 px grid).
const ICONS = {
  search: '<circle cx="7" cy="7" r="4.25"/><path d="M10.2 10.2 13.5 13.5"/>',
  settings: '<path d="M2.5 5h5.25M11.75 5h1.75M2.5 11h1.75M8.25 11h5.25"/><circle cx="9.75" cy="5" r="1.75"/><circle cx="6.25" cy="11" r="1.75"/>',
  plus: '<path d="M8 3.5v9M3.5 8h9"/>',
  close: '<path d="M4 4l8 8M12 4l-8 8"/>',
  back: '<path d="M9.75 3.5 5.25 8l4.5 4.5"/>',
  forward: '<path d="M6.25 3.5 10.75 8l-4.5 4.5"/>',
  down: '<path d="M3.5 6.25 8 10.75l4.5-4.5"/>',
  more: '<circle cx="3.5" cy="8" r=".9"/><circle cx="8" cy="8" r=".9"/><circle cx="12.5" cy="8" r=".9"/>',
  send: '<path d="M8 12.5v-9M4 7.5l4-4 4 4"/>',
  out: '<path d="M4.5 11.5l7-7M5.5 4.5h6v6"/>',
  check: '<path d="M3.5 8.5l3 3 6-7"/>',
  panel: '<rect x="2.5" y="3" width="11" height="10" rx="2"/><path d="M6.5 3v10"/>',
  expand: '<path d="M9.5 2.75h3.75V6.500M6.500 13.250H2.750V9.500M13.250 2.750 9 7M2.750 13.250 7 9"/>',
  chat: '<path d="M3.5 3h9A1.5 1.5 0 0 1 14 4.500v5a1.500 1.500 0 0 1-1.500 1.500H7.500L4.500 13.500V11h-1A1.500 1.500 0 0 1 2 9.500v-5A1.500 1.500 0 0 1 3.500 3z"/>',
  compose: '<path d="M7.5 3H4a1.500 1.500 0 0 0-1.500 1.500V12A1.500 1.500 0 0 0 4 13.500h7.500A1.500 1.500 0 0 0 13 12V8.500"/><path d="M11.500 2.500 13.500 4.500 8.250 9.750l-2.500.5.5-2.500L11.500 2.500z"/>',
  project: '<rect x="2.500" y="2.500" width="4.500" height="4.500" rx="1"/><rect x="9" y="2.500" width="4.500" height="4.500" rx="1"/><rect x="2.500" y="9" width="4.500" height="4.500" rx="1"/><rect x="9" y="9" width="4.500" height="4.500" rx="1"/>',
  wrench: '<path d="M10.2 2.300a3.250 3.250 0 0 0-3.900 4.200L2.500 10.300a1.500 1.500 0 0 0 2.100 2.100l3.800-3.800a3.250 3.250 0 0 0 4.200-3.900L10.700 6.600 9.300 5.200l1.900-1.900a3.200 3.200 0 0 0-1-1z"/>',
  pulse: '<path d="M1.750 8.500h2.750L6.250 4l2.500 8.500 1.750-4h3.750"/>',
  branch: '<circle cx="4.500" cy="3.750" r="1.500"/><circle cx="4.500" cy="12.250" r="1.500"/><circle cx="11.500" cy="5.750" r="1.500"/><path d="M4.500 5.250v5.500M11.500 7.250c0 2-2 2.500-4 2.750-1.500.2-3 .5-3 1.250"/>',
  sidebar: '<rect x="2.500" y="3" width="11" height="10" rx="2"/><path d="M6.500 3v10"/>',
  map: '<path d="M2 4.5 6 3l4 1.5L14 3v8.5L10 13l-4-1.5L2 13z"/><path d="M6 3v8.5M10 4.5V13"/>',
  files: '<path d="M2.5 5A1.5 1.5 0 0 1 4 3.5h2.3l1.4 1.5H12A1.5 1.5 0 0 1 13.5 6.5V11a1.5 1.5 0 0 1-1.5 1.5H4A1.5 1.5 0 0 1 2.5 11V5z"/>',
  popout: '<path d="M6.5 3.5H4A1.5 1.5 0 0 0 2.5 5v7A1.5 1.5 0 0 0 4 13.500h7a1.500 1.500 0 0 0 1.500-1.500V9.500M9 2.500h4.500V7M13.500 2.500 7.500 8.500"/>',
  doc: '<path d="M4.5 2.5h5l3 3v7A1.5 1.5 0 0 1 11 14H4.500A1.500 1.500 0 0 1 3 12.500V4a1.500 1.500 0 0 1 1.500-1.500zM9.500 2.500v3h3M5.500 8.500h5M5.500 11h3.500"/>',
  shield: '<path d="M8 2 3 4v4c0 3 2.200 5 5 6 2.800-1 5-3 5-6V4L8 2z"/><path d="M5.900 8.200l1.500 1.500 2.800-3.100"/>',
  lock: '<rect x="3.5" y="7" width="9" height="6.5" rx="1.5"/><path d="M5.5 7V5.250a2.500 2.500 0 0 1 5 0V7"/>',
  eye: '<path d="M1.75 8S4 3.75 8 3.750 14.250 8 14.250 8 12 12.250 8 12.250 1.750 8 1.750 8z"/><circle cx="8" cy="8" r="1.75"/>',
  pen: '<path d="M10.5 3 13 5.500 5.750 12.750 2.750 13.250l.5-3L10.500 3z"/>',
  layers: '<path d="M8 2.500 13.750 5.500 8 8.500 2.250 5.500 8 2.500z"/><path d="M2.750 8.500 8 11.250l5.250-2.750"/>',
  filter: '<path d="M2.500 3.500h11L9.500 8.250v4l-3 1.250v-5.250L2.500 3.500z"/>',
  table: '<rect x="2.500" y="3" width="11" height="10" rx="1.750"/><path d="M2.500 6.500h11M2.500 9.750h11M6.250 6.500V13"/>',
  play: '<path d="M5 3.250v9.500L12.500 8 5 3.250z"/>',
  code: '<path d="M5.500 4.500 2 8l3.500 3.500M10.500 4.500 14 8l-3.500 3.500"/>',
  clock: '<circle cx="8" cy="8" r="5.750"/><path d="M8 4.750V8l2.250 1.500"/>',
  user: '<circle cx="8" cy="5.500" r="2.500"/><path d="M3 13.250c.6-2.300 2.600-3.500 5-3.500s4.400 1.200 5 3.500"/>',
  cpu: '<rect x="4" y="4" width="8" height="8" rx="1.500"/><path d="M6.500 1.750V4M9.500 1.750V4M6.500 12v2.250M9.500 12v2.250M1.750 6.500H4M1.750 9.500H4M12 6.500h2.250M12 9.500h2.250"/>',
  key: '<circle cx="5.250" cy="10.750" r="2.500"/><path d="M7.100 8.900 13 3M10.750 5.250l1.750 1.750"/>',
  // Kept by name for the "coming" states and older callers.
  connections: '<ellipse cx="8" cy="4.25" rx="4.75" ry="1.75"/><path d="M3.250 4.250v7.500c0 .970 2.130 1.750 4.750 1.750s4.750-.780 4.750-1.750v-7.500M3.250 8c0 .970 2.130 1.750 4.750 1.750S12.750 8.970 12.750 8"/>',
  skills: '<path d="M8 2.200l1.700 3.800 4.100.4-3.100 2.800.9 4.100L8 11.200l-3.600 2.100.9-4.100L2.200 6.400l4.100-.4z"/>',
  registry: '<circle cx="8" cy="8" r="5.5"/><path d="M2.500 8h11M8 2.500c1.800 1.600 2.600 3.500 2.600 5.500S9.800 11.900 8 13.500C6.200 11.900 5.400 10 5.400 8S6.200 4.100 8 2.500z"/>',
};
const LOGO = '<circle class="ring" cx="12" cy="12" r="10"/><path class="n" d="M12 5 14.4 12H9.6z"/><path class="s" d="M12 19 9.600 12h4.800z"/>';

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
  return svgFrom("0 0 16 16", ICONS[name] || "", {
    class: "ic", fill: "none", stroke: "currentColor", "stroke-width": "1.5", "stroke-linejoin": "round", "stroke-linecap": "round",
  });
}

/** The Columbus mark: a compass, its north in the colour of the answer. */
export function logo() {
  return svgFrom("0 0 24 24", LOGO, { class: "mark", fill: "none" });
}

/** A button that is only an icon, named for screen readers and on hover. */
export function iconButton(name, label, onclick, attrs = {}) {
  const b = h("button", { type: "button", class: "iconbtn", "aria-label": label, title: label, ...attrs }, icon(name));
  if (onclick) b.addEventListener("click", onclick);
  return b;
}

// ---------- Text helpers ----------

/** Where the app has no way to confirm who is at the keyboard (no Touch
 * ID, no Windows Hello): how a button that would otherwise ask begins what
 * it says. The words Organization rules uses for the same case. */
export const CANNOT_CONFIRM = "On this system the app cannot confirm it is you (there is no Touch ID or Windows Hello)";
/** Whether that is so, from what the app says of itself (`info.confirm_with`). */
export const cannotConfirm = (info) => !!info && info.confirm_with === "not available on this system";

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

const dayStart = (x) => new Date(x.getFullYear(), x.getMonth(), x.getDate()).getTime();
const asDate = (v) => (typeof v === "number" ? new Date(v * 1000) : new Date(v));

/** A time for a list: the hour today, the weekday this week, else the date. */
export function shortWhen(v) {
  if (v == null || v === "") return "";
  const d = asDate(v);
  if (isNaN(+d)) return String(v);
  const now = new Date();
  const days = Math.round((dayStart(now) - dayStart(d)) / 86400000);
  if (days === 0) return d.toLocaleTimeString([], { hour: "numeric", minute: "2-digit" });
  if (days === 1) return "Yesterday";
  if (days > 1 && days < 7) return d.toLocaleDateString([], { weekday: "short" });
  return d.toLocaleDateString([], d.getFullYear() === now.getFullYear() ? { month: "short", day: "numeric" } : { month: "short", day: "numeric", year: "numeric" });
}

/** The hour and minute alone. */
export function clock(v) {
  const d = asDate(v);
  return v == null || v === "" || isNaN(+d) ? "" : d.toLocaleTimeString([], { hour: "numeric", minute: "2-digit" });
}

/** The day something happened on, as a heading: Today, Yesterday, or the date. */
export function dayName(v) {
  const d = asDate(v);
  if (v == null || v === "" || isNaN(+d)) return "";
  const now = new Date();
  const days = Math.round((dayStart(now) - dayStart(d)) / 86400000);
  if (days === 0) return "Today";
  if (days === 1) return "Yesterday";
  return d.toLocaleDateString([], d.getFullYear() === now.getFullYear() ? { weekday: "long", month: "long", day: "numeric" } : { month: "long", day: "numeric", year: "numeric" });
}

/** Which part of the past a time falls in, for grouping a list. */
export function era(v) {
  const d = asDate(v);
  if (v == null || v === "" || isNaN(+d)) return "Earlier";
  const days = Math.round((dayStart(new Date()) - dayStart(d)) / 86400000);
  return days <= 0 ? "Today" : days === 1 ? "Yesterday" : days < 7 ? "This week" : "Earlier";
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
  // Shown above an open sheet or dialog too, where the browser can.
  if (t.showPopover) {
    try { t.hidePopover(); } catch { /* was not showing */ }
    try { t.showPopover(); } catch { /* shown in place */ }
  }
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => {
    t.hidden = true;
    if (t.hidePopover) { try { t.hidePopover(); } catch { /* was not showing */ } }
  }, 6000);
}

// ---------- Page building blocks ----------

/** The page head: a way back (for a page inside another), the title, a
 * lede, and actions on the right. `back` is {href, label, id}. */
export function pageHead(title, lede, actions = [], back = null) {
  const bar = h("div", { class: "bar" },
    back ? h("a", { class: "iconbtn back", href: back.href, id: back.id || null, "aria-label": back.label, title: back.label }, icon("back")) : null,
    h("div", { class: "titles" }, h("h1", { id: "page-title", tabindex: "-1" }, title)),
    actions.length ? h("div", { class: "actions" }, ...actions) : null);
  return h("header", { class: back ? "pagehead has-back" : "pagehead" }, bar, lede ? h("p", { class: "lede" }, lede) : null);
}

/** The head of one of the places in the sidebar: its name, then a search
 * box and its actions. */
export function placeHead(title, search, actions = []) {
  return h("header", { class: "placehead" },
    h("h1", { id: "page-title", tabindex: "-1" }, title),
    h("div", { class: "row" }, search ? h("label", { class: "searchbox" }, icon("search"), search) : h("span", { class: "grow" }), ...actions));
}

/** A quiet label over a group of rows, with how many there are. */
export function sec(label, n = null, ...more) {
  return h("div", { class: "sec" }, h("h2", {}, label), n != null && n !== "" ? h("span", { class: "n" }, String(n)) : null, ...more);
}

/** A quiet select is as wide as the choice it shows, not its longest one. */
export function fitSelect(sel) {
  const o = sel.selectedOptions && sel.selectedOptions[0];
  if (!o || !sel.isConnected) return;
  // Measured in a span drawn out of sight, in the select's own type.
  const probe = h("span", { "aria-hidden": "true" }, o.textContent);
  const cs = getComputedStyle(sel);
  probe.style.position = "absolute";
  probe.style.visibility = "hidden";
  probe.style.whiteSpace = "nowrap";
  probe.style.font = cs.font;
  probe.style.letterSpacing = cs.letterSpacing;
  document.body.append(probe);
  const w = probe.getBoundingClientRect().width;
  probe.remove();
  if (w > 4) sel.style.width = `${Math.ceil(w) + 32}px`;
}

/** A "more" button: a short list of actions that opens under it. `items`
 * is a list (or a function giving one) of {label, onclick, href, id, cls,
 * danger, disabled, title}; empty entries are left out. */
export function menuButton(label, items, attrs = {}) {
  const wrap = h("span", { class: "menu-wrap" });
  const list = h("div", { class: "menu", role: "menu", hidden: true });
  const btn = iconButton("more", label, () => (list.hidden ? open() : close()), { "aria-haspopup": "menu", "aria-expanded": "false", ...attrs });
  const away = (e) => { if (!wrap.contains(e.target)) close(); };
  const esc = (e) => { if (e.key === "Escape") { e.stopPropagation(); e.preventDefault(); close(); btn.focus(); } };
  function close() {
    list.hidden = true;
    btn.setAttribute("aria-expanded", "false");
    document.removeEventListener("pointerdown", away, true);
    document.removeEventListener("keydown", esc, true);
  }
  function open() {
    const all = (typeof items === "function" ? items() : items).filter(Boolean);
    list.replaceChildren(...all.map((it) => {
      const cls = [it.cls, it.danger ? "danger" : ""].filter(Boolean).join(" ") || null;
      const node = it.href ? h("a", { role: "menuitem", href: it.href, id: it.id || null, class: cls, title: it.title || null }, it.label)
        : h("button", { type: "button", role: "menuitem", id: it.id || null, class: cls, title: it.title || null, disabled: !!it.disabled }, it.label);
      node.addEventListener("click", () => { close(); if (it.onclick) it.onclick(); });
      return node;
    }));
    list.hidden = false;
    btn.setAttribute("aria-expanded", "true");
    document.addEventListener("pointerdown", away, true);
    document.addEventListener("keydown", esc, true);
  }
  wrap.append(btn, list);
  return wrap;
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

// ---------- The panel on the right: what the assistant made ----------
// It holds a session's map and table (the stage), and over them anything
// long to read: files, how a result was made, a script, a note (the side).

/** Whether the panel on the right is on show. */
export function outputsOpen() {
  const o = $("outputs");
  return !!o && !o.hidden;
}

/** Open the panel on the right. */
export function openOutputs() {
  const o = $("outputs");
  if (!o || !o.hidden) return;
  o.hidden = false;
  document.body.classList.add("out-open");
  layoutChanged();
}

/** Close it (and whatever is being read in it). */
export function closeOutputs() {
  const o = $("outputs");
  if (!o || o.hidden) return;
  sideOpened = false;
  closeSide();
  setOutputsFull(false);
  o.hidden = true;
  document.body.classList.remove("out-open");
  layoutChanged();
}

/** The middle put away, so the panel on the right has the window (or back). */
export function setOutputsFull(full) {
  if (full) openOutputs();
  if (document.body.classList.contains("out-full") === !!full) return;
  document.body.classList.toggle("out-full", !!full);
  // The button in the panel's head says what it does now.
  const old = $("out-full");
  if (old) {
    const held = document.activeElement === old;
    const fresh = fullButton();
    old.replaceWith(fresh);
    if (held) fresh.focus({ preventScroll: true });
  }
  layoutChanged();
}

function fullButton() {
  const full = document.body.classList.contains("out-full");
  return iconButton(full ? "panel" : "expand", full ? "Show the chat again" : "Fill the window", () => { setOutputsFull(!document.body.classList.contains("out-full")); }, { id: "out-full", "aria-pressed": full ? "true" : "false" });
}

/** The panel's head: a title and the actions that go with what it shows.
 * `actions` are nodes; the two that are always there (fill the window,
 * close) are added here. `onClose` is what the close button does when
 * closing is more than hiding the panel. With no title the head is not
 * shown. */
export function setOutputsHead(title, actions = [], onClose = null) {
  const head = $("out-head");
  if (!head) return;
  head.hidden = !title;
  $("outputs").classList.toggle("detail-only", !title);
  if (!title) { head.replaceChildren(); return; }
  head.replaceChildren(h("h2", { id: "out-title" }, title), ...actions.filter(Boolean), fullButton(),
    iconButton("close", "Close the panel", () => (onClose ? onClose() : closeOutputs()), { id: "out-close" }));
}

let sideClose = null;
// Whether the panel was opened only to read this (closing the reading then
// closes the panel too).
let sideOpened = false;

/** Show `body` to read in the panel on the right, over the map if one is
 * there. `extra` nodes go in its head, before the close button. Returns a
 * function that closes it. */
export function openSide(title, body, extra = [], onClose = null) {
  const side = $("side");
  const wasOpen = outputsOpen();
  const keep = sideOpened && !side.hidden;
  closeSide({ keepPanel: true });
  sideOpened = keep || !wasOpen;
  openOutputs();
  const close = () => { if (sideClose === close) closeSide(); };
  side.replaceChildren(
    h("div", { class: "hd" }, h("h2", { id: "side-title" }, title),
      h("span", { class: "more" }, ...extra, iconButton("close", "Close", close, { id: "side-close" }))),
    h("div", { class: "bd", id: "side-body" }, body));
  side.setAttribute("aria-labelledby", "side-title");
  side.hidden = false;
  // One head at a time: the reading's own, while it is open.
  $("outputs").classList.add("reading");
  sideClose = close;
  side.onClose = onClose;
  layoutChanged();
  return close;
}

export function closeSide({ keepPanel = false } = {}) {
  const side = $("side");
  if (!side || side.hidden) return;
  side.hidden = true;
  side.replaceChildren();
  $("outputs").classList.remove("reading");
  sideClose = null;
  const fn = side.onClose;
  side.onClose = null;
  if (fn) { try { fn(); } catch (e) { console.error(e); } }
  if (sideOpened && !keepPanel) {
    sideOpened = false;
    closeOutputs();
  }
  layoutChanged();
}

// ---------- The stage: where a session's map shows, in the panel on the right ----------

/** How much of the map's frame something covers, in CSS pixels from each
 * edge. Nothing does: the map has the panel's body to itself. (The map
 * page still takes the numbers, for a host that does cover part of it.) */
export function insets() {
  return { left: 0, top: 0, right: 0, bottom: 0 };
}

const layoutListeners = new Set();
export function onLayout(fn) {
  layoutListeners.add(fn);
  return () => layoutListeners.delete(fn);
}
/** The panel opened, closed or changed its size: maps on the stage are told. */
export function layoutChanged() {
  for (const fn of layoutListeners) {
    try { fn(); } catch (e) { console.error(e); }
  }
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
    // The app's own chat (the name its session gives itself), and the
    // app's own settings pages.
    "Columbus app": "Columbus's own chat",
    "columbus-chat": "Columbus's own chat",
    "columbus-app (settings)": "Columbus's Settings",
    "Visual Studio Code": "VS Code",
    "github-copilot": "GitHub Copilot",
    cursor: "Cursor",
  };
  return known[c] || c;
}
