// The main window: the sidebar (a new chat, the search, the places, recent
// chats), the router, Ctrl+K search, the settings sheet, and the polling
// that keeps counts fresh. Pages live in pages/*.js and draw in the middle;
// what the assistant made (a session's map and table, files, a script, a
// note) shows in the panel on the right.
import { $, h, icon, logo, iconButton, invoke, store, applyTheme, closeSide, closeOutputs, setOutputsHead, layoutChanged, outputsOpen, setOutputsFull } from "./core.js";
import * as chats from "./pages/chats.js";
import * as documents from "./pages/documents.js";
import * as connections from "./pages/connections.js";
import * as tools from "./pages/tools.js";
import * as workflows from "./pages/workflows.js";
import * as activity from "./pages/activity.js";
import * as settings from "./pages/settings.js";
import * as projects from "./pages/projects.js";
import * as datagroups from "./pages/datagroups.js";
import * as registry from "./pages/registry.js";

// The places a person goes to, in the sidebar: [page, name, icon].
const PLACES = [["chats", "Chats", "chat"], ["projects", "Projects", "project"], ["data", "Data", "connections"], ["documents", "Documentation", "doc"],
  ["workflows", "Workflows", "play"], ["tools", "Tools", "wrench"], ["activity", "Activity", "pulse"]];
// Every page: what draws it, the place it belongs to, and its name.
const PAGES = {
  chats: [chats.render, "chats", "Chats"],
  projects: [projects.render, "projects", "Projects"],
  data: [connections.render, "data", "Data"],
  connections: [connections.render, "data", "Data"],
  documents: [documents.render, "documents", "Documentation"],
  datagroups: [datagroups.render, "data", "Data groups"],
  registry: [registry.render, "data", "Registry"],
  workflows: [workflows.render, "workflows", "Workflows"],
  tools: [tools.render, "tools", "Tools"],
  skills: [tools.renderSkills, "tools", "Skills"],
  activity: [activity.render, "activity", "Activity"],
};
// What Ctrl+K offers besides chats (name, where, what kind).
const GOTO = [
  ["New chat", "#/chats/fresh", "Chats"], ["Chats", "#/chats", "Place"], ["Projects", "#/projects", "Place"],
  ["Data", "#/data", "Place"], ["Datasets", "#/data/datasets", "Data"], ["Documentation", "#/documents", "Place"],
  ["Organization's pack", "#/documents/pack", "Documentation"], ["Note proposals", "#/documents/proposals", "Documentation"], ["Import a document", "#/documents/import", "Documentation"],
  ["Data groups", "#/datagroups", "Data"], ["Map layers", "#/data/maplayers", "Data"], ["Project databases", "#/data/projectdbs", "Data"],
  ["Registry: open data and GIS tools", "#/registry", "Data"],
  ["Workflows", "#/workflows", "Place"], ["Tools", "#/tools", "Place"], ["Skills", "#/skills", "Tools"],
  ["Activity", "#/activity", "Place"], ["History (the audit log)", "#/activity/audit", "Activity"], ["Exports", "#/activity/exports", "Activity"],
  ["Settings", "#/settings", "Settings"], ...settings.SECTIONS.map(([k, t]) => [t, `#/settings/${k}`, "Settings"]),
];

function route() {
  const parts = (location.hash.replace(/^#\/?/, "") || "chats/new").split("/").map(decodeURIComponent);
  // Home is the start of a new chat now.
  if (parts[0] === "home") return { page: "chats", rest: ["new"] };
  if (parts[0] === "settings") return { page: "settings", rest: parts.slice(1) };
  return PAGES[parts[0]] ? { page: parts[0], rest: parts.slice(1) } : { page: "chats", rest: ["new"] };
}

let unmount = null;
let booted = false;
let shown = null; // the page under the settings sheet, once one is drawn
let underSettings = "#/chats/new";
// Pages draw here; the live chat keeps its own host beside it (chats.js),
// so its conversation and its map are never rebuilt when the person looks
// elsewhere.
let pageHost = h("div", { class: "page", id: "page-host" });
$("main").append(pageHost);
let showSeq = 0;
let shows = 0;
// Whether the person has typed or chosen anything on the page that is open
// (it is then never drawn again under them).
let touched = false;
for (const t of ["input", "change"]) {
  $("main").addEventListener(t, (e) => { if (pageHost.contains(e.target)) touched = true; }, true);
}

async function show(ev) {
  const mine = ++shows;
  let r = route();
  if (r.page === "settings") {
    // Settings is a sheet over whatever page is open. The address to put
    // back when it closes is the one the person came from: a page may have
    // rewritten its own address after it was drawn ("fresh" becomes "new").
    const from = ev && typeof ev.oldURL === "string" && ev.oldURL.includes("#") ? ev.oldURL.slice(ev.oldURL.indexOf("#")) : "";
    if (from && !from.startsWith("#/settings")) underSettings = from;
    if (!shown) await showPage({ page: "chats", rest: ["new"] });
    settings.openSheet(r.rest[0], (saved) => {
      if (location.hash.startsWith("#/settings")) history.replaceState(null, "", underSettings);
      drawNav();
      // The focus goes back to where Settings is opened from (the nav was
      // drawn again while the sheet was open, so the browser cannot do it).
      // A moment later: until the sheet has left the screen, nothing under
      // it can take the focus. Then the page under it is brought up to
      // date, without moving the focus again.
      setTimeout(() => {
        const a = document.activeElement;
        if ((!a || a === document.body) && !document.querySelector("dialog[open]") && $("nav-settings")) $("nav-settings").focus({ preventScroll: true });
        if (saved) refreshUnder();
      }, 60);
    });
    drawNav(r);
    return;
  }
  // Left by a link to a page: that page is drawn with what was saved.
  if (settings.closeSheet()) {
    await settings.saves();
    try { await refresh.info(); } catch (e) { console.error(e); }
    // The person went on again while that was read: the later call draws.
    if (mine !== shows) return;
    r = route();
  }
  underSettings = location.hash || "#/chats/new";
  await showPage(r);
}

// What the sheet saved shows on the page under it. The chat takes its new
// defaults in place (the model, the thinking level, whether an AI is
// connected). Another page is drawn again, unless the person has typed into
// it: their words are worth more than a fresh page.
async function refreshUnder() {
  const r = shown;
  // A save still on its way (a confirmation at the keyboard) lands first.
  await settings.saves();
  try { await refresh.info(); } catch (e) { console.error(e); }
  if (!r || r !== shown || route().page === "settings") return;
  if (r.page === "chats") {
    chats.settingsSaved();
    return;
  }
  // Left as it is: a page the person has typed or chosen something on, and
  // a page with a dialog open over it (its buttons belong to the page as
  // it was drawn).
  if (touched || document.querySelector("dialog[open]")) return;
  await showPage(r, { keepFocus: true });
}

async function showPage(r, { keepFocus = false } = {}) {
  const seq = ++showSeq;
  if (unmount) {
    try { unmount(); } catch (e) { console.error(e); }
    unmount = null;
  }
  // The live chat leaves the panel now, even if the page before this one
  // was still being drawn and has handed back no clean-up yet.
  chats.leaveLive();
  // The panel on the right belonged to the page that is left; the next one
  // opens it again if it has something for it.
  closeOutputs();
  setOutputsHead(null);
  touched = false;
  // A fresh sheet for each page. One that is still being drawn when the
  // person goes on keeps drawing on the sheet that was taken away, not on
  // the next page's.
  const main = h("div", { class: "page", id: "page-host" });
  pageHost.replaceWith(main);
  pageHost = main;
  shown = r;
  drawNav(r);
  document.title = `${PAGES[r.page][2]} · Columbus`;
  // A page that cannot be the start says so at once, not when its data has come.
  if (!(r.page === "chats" && ["new", "fresh", "run"].includes(r.rest[0]))) posture();
  try {
    const off = (await PAGES[r.page][0](main, r.rest)) || null;
    if (seq !== showSeq) {
      // The person went on while this page was being drawn: what it set up
      // (its map on the stage, its listeners) is taken down at once.
      if (off) {
        try { off(); } catch (e) { console.error(e); }
      }
      // What it changed while it was drawn (the chat shown, say) no longer
      // decides whether this is the start.
      posture();
      return;
    }
    unmount = off;
    // After following a link, a screen reader starts at the new page's title.
    const a = document.activeElement;
    if (!keepFocus && booted && (!a || a === document.body || $("nav").contains(a))) {
      const t = main.hidden ? null : $("page-title");
      if (t) t.focus({ preventScroll: true });
    }
  } catch (e) {
    if (seq !== showSeq) return;
    main.replaceChildren(h("div", { class: "pad" }, h("p", { class: "error-text", role: "alert" }, `This page could not be shown: ${e}`)));
  }
  posture();
}

// A new chat with nothing said yet is the start: a greeting, and the
// question in the middle of the window.
function posture() {
  const start = !!shown && shown.page === "chats" && chats.isStart();
  // The question is drawn first on the start: first in the document too.
  chats.questionFirst(start);
  document.body.classList.toggle("start", start);
}

/** What waits for the person: requests, and things to read before they run. */
function waiting() {
  const requests = chats.otherInbox().reduce((n, s) => n + (s.pending || []).length, 0) + chats.waitingCount();
  const reviews = (store.tools ? store.tools.waiting.length : 0) + (store.workflows ? store.workflows.waiting.length : 0);
  return { requests, reviews };
}

// ---------- The sidebar ----------
// Built once and then changed in place: a link that is drawn again while
// the pointer is down on it loses the click.
const navLinks = new Map(); // place -> { a, count, label }
let navBuilt = false;

function buildNav() {
  const mac = navigator.platform.startsWith("Mac");
  $("sb-top").replaceChildren(logo(), h("span", { class: "name" }, "Columbus"),
    iconButton("sidebar", "Close the sidebar", () => setSidebar(document.body.classList.contains("sb-closed")), { id: "sb-toggle" }));
  const item = (attrs, ic, label, ...more) => h(attrs.href ? "a" : "button",
    { class: "sb-item", ...(attrs.href ? {} : { type: "button" }), ...attrs }, icon(ic), h("span", { class: "label" }, label), ...more);
  const search = item({ id: "nav-search", title: `Search chats, data, workflows and pages (${mac ? "⌘K" : "Ctrl+K"})` }, "search", "Search",
    h("span", { class: "kbd", "aria-hidden": "true" }, mac ? "⌘K" : "Ctrl K"));
  search.addEventListener("click", openPalette);
  const nav = $("nav");
  nav.replaceChildren(item({ href: "#/chats/fresh", id: "nav-new", title: "New chat" }, "compose", "New chat"), search,
    h("span", { class: "gap", "aria-hidden": "true" }));
  for (const [k, label, ic] of PLACES) {
    const count = h("span", { class: "count", "aria-hidden": "true", hidden: true });
    const a = item({ href: `#/${k}`, class: "sb-item tab", "data-place": k, title: label }, ic, label, count);
    navLinks.set(k, { a, count, label });
    nav.append(a);
  }
  $("sb-foot").replaceChildren(item({ href: "#/settings", id: "nav-settings", title: "Settings" }, "settings", "Settings"));
  navBuilt = true;
}

// The place that is lit. None while a chat itself is open: its row under
// Recents is.
function placeOf(r) {
  if (!r) return null;
  if (r.page === "chats") return r.rest.length ? null : "chats";
  return PAGES[r.page][1];
}

function drawNav(r = route()) {
  if (!navBuilt) buildNav();
  const under = r.page === "settings" ? shown : r;
  const place = placeOf(under);
  const w = waiting();
  const n = w.requests + w.reviews;
  for (const [k, { a, count, label }] of navLinks) {
    if (place === k) a.setAttribute("aria-current", "page");
    else a.removeAttribute("aria-current");
    const c = k === "activity" ? n : 0;
    count.hidden = !c;
    count.textContent = c ? String(c) : "";
    a.setAttribute("aria-label", c ? `${label}, ${c} waiting` : label);
  }
  const set = $("nav-settings");
  if (r.page === "settings") set.setAttribute("aria-current", "page");
  else set.removeAttribute("aria-current");
  drawRecents(under);
}

// Recent chats, and the sessions other AI apps run, newest first.
let recentsKey = "";
function drawRecents(r) {
  const rows = chats.recent(16);
  const open = r && r.page === "chats" && r.rest.length ? `#/chats/${r.rest.map(encodeURIComponent).join("/")}` : "";
  const isOpen = (x) => (x.live ? chats.liveShown() : x.href === open);
  const key = JSON.stringify(rows.map((x) => [x.href, x.title, x.who || "", !!x.waiting, isOpen(x)]));
  if (key === recentsKey) return;
  recentsKey = key;
  const box = $("recents");
  const held = box.contains(document.activeElement) ? document.activeElement.getAttribute("href") : null;
  const ul = h("ul", { id: "recent-list" });
  for (const x of rows) {
    const a = h("a", { class: "sb-item", href: x.href, title: x.title },
      x.waiting ? h("span", { class: "dot warn", role: "img", "aria-label": "Waiting for you" }) : null,
      h("span", { class: "label" }, x.title), x.who ? h("span", { class: "who" }, x.who) : null);
    if (isOpen(x)) a.setAttribute("aria-current", "page");
    ul.append(h("li", {}, a));
  }
  box.replaceChildren(...(rows.length ? [h("h2", { id: "recents-title" }, "Recents"), ul,
    h("a", { class: "sb-item all", href: "#/chats", id: "recents-all" }, h("span", { class: "label" }, "All chats"))] : []));
  if (held) {
    const again = [...box.querySelectorAll("a")].find((x) => x.getAttribute("href") === held);
    if (again) again.focus({ preventScroll: true });
  }
}

/** Put the sidebar away (a rail of icons) or bring it back. */
function setSidebar(open, remember = true) {
  document.body.classList.toggle("sb-closed", !open);
  const t = $("sb-toggle");
  if (t) {
    const label = open ? "Close the sidebar" : "Open the sidebar";
    t.setAttribute("aria-label", label);
    t.title = label;
    t.setAttribute("aria-expanded", open ? "true" : "false");
  }
  if (remember) { try { localStorage.setItem("columbus.sidebar", open ? "open" : "closed"); } catch { /* not kept */ } }
  layoutChanged();
}

// ---------- The panel on the right: its width ----------
const OUT_MIN = 380;
// The width the person asked for; the stylesheet keeps the middle column
// wide enough whatever the window's size, so a window made smaller and then
// larger gets its panel back as it was.
function setOutWidth(px, remember = true) {
  const w = Math.round(Math.max(px, OUT_MIN));
  document.documentElement.style.setProperty("--out-w", `${w}px`);
  outWidthSaid();
  if (remember) { try { localStorage.setItem("columbus.outputs", String(w)); } catch { /* not kept */ } }
  layoutChanged();
}
function outWidthSaid() {
  const w = outputsOpen() ? Math.round($("outputs").getBoundingClientRect().width) : 0;
  if (w) $("splitter").setAttribute("aria-valuenow", String(w));
}
{
  const sp = $("splitter");
  sp.addEventListener("pointerdown", (e) => {
    if (e.button !== 0) return;
    e.preventDefault();
    sp.setPointerCapture(e.pointerId);
    document.body.classList.add("resizing");
    const move = (m) => setOutWidth(window.innerWidth - m.clientX, false);
    const up = () => {
      sp.removeEventListener("pointermove", move);
      document.body.classList.remove("resizing");
      setOutWidth($("outputs").getBoundingClientRect().width);
    };
    sp.addEventListener("pointermove", move);
    sp.addEventListener("pointerup", up, { once: true });
    sp.addEventListener("pointercancel", up, { once: true });
  });
  // By keyboard: the arrows move the edge.
  sp.addEventListener("keydown", (e) => {
    if (e.key !== "ArrowLeft" && e.key !== "ArrowRight") return;
    e.preventDefault();
    setOutWidth($("outputs").getBoundingClientRect().width + (e.key === "ArrowLeft" ? 32 : -32));
  });
}

// ---------- Ctrl+K ----------
let paletteItems = [];
let paletteSel = 0;
let searchSeq = 0;

// The person's connections, read when the search opens (their names are
// searched too).
let paletteConns = [];

function openPalette() {
  const d = $("palette");
  if (d.open) return;
  $("palette-q").value = "";
  fillPalette("");
  d.showModal();
  $("palette-q").focus();
  invoke("settings_state").then((st) => {
    paletteConns = (st.connections || []).filter((c) => c.id);
    if (d.open && $("palette-q").value.trim()) fillPalette($("palette-q").value);
  }).catch(() => {});
}

async function fillPalette(q) {
  const seq = ++searchSeq;
  const ql = q.trim().toLowerCase();
  const items = [];
  for (const [name, href, kind] of GOTO) {
    if (ql ? name.toLowerCase().includes(ql) : kind === "Place" || name === "New chat") items.push({ kind, title: name, href });
  }
  for (const c of chats.recent(50)) {
    if (!ql || c.title.toLowerCase().includes(ql)) items.push({ kind: c.kind, title: c.title, sub: c.sub, href: c.href });
  }
  // By name, once something is typed: connections, workflows, projects.
  if (ql) {
    for (const c of paletteConns) {
      if (c.id.toLowerCase().includes(ql)) items.push({ kind: "Data", title: c.id, sub: c.description || "", href: `#/data/c/${encodeURIComponent(c.id)}` });
    }
    for (const w of (store.workflows ? store.workflows.installed : [])) {
      if (String(w.name).toLowerCase().includes(ql)) items.push({ kind: "Workflow", title: w.name, sub: w.purpose || "", href: `#/workflows/${encodeURIComponent(w.id)}` });
    }
    for (const p of (store.projects ? store.projects.projects : [])) {
      if (String(p.name).toLowerCase().includes(ql)) items.push({ kind: "Project", title: p.name, href: `#/projects/${encodeURIComponent(p.id)}` });
    }
  }
  render(items);
  if (ql.length >= 2) {
    try {
      const found = await invoke("chats_search", { query: q });
      if (seq !== searchSeq) return;
      for (const f of found) {
        const href = `#/chats/c/${encodeURIComponent(f.id)}`;
        if (!items.some((i) => i.href === href)) items.push({ kind: "Chat content", title: f.title, sub: f.snippet, href });
      }
      render(items);
    } catch { /* titles only */ }
  }
  function render(list) {
    paletteItems = list.slice(0, 40);
    paletteSel = 0;
    const ul = $("palette-list");
    ul.replaceChildren();
    paletteItems.forEach((it, i) => {
      const li = h("li", { role: "option", id: `pal-${i}`, "aria-selected": i === 0 ? "true" : "false" },
        h("span", {}, it.title), h("span", { class: "k" }, it.kind), it.sub ? h("span", { class: "s" }, it.sub) : null);
      li.addEventListener("click", () => choose(i));
      ul.append(li);
    });
    if (!paletteItems.length) ul.append(h("li", { class: "muted", role: "option", "aria-disabled": "true" }, "Nothing found."));
    $("palette-q").setAttribute("aria-activedescendant", paletteItems.length ? "pal-0" : "");
  }
}

function choose(i) {
  const it = paletteItems[i];
  if (!it) return;
  $("palette").close();
  location.hash = it.href;
}

function movePalette(d) {
  if (!paletteItems.length) return;
  paletteSel = (paletteSel + d + paletteItems.length) % paletteItems.length;
  [...$("palette-list").children].forEach((li, i) => li.setAttribute("aria-selected", i === paletteSel ? "true" : "false"));
  $("palette-q").setAttribute("aria-activedescendant", `pal-${paletteSel}`);
  $(`pal-${paletteSel}`)?.scrollIntoView({ block: "nearest" });
}

$("palette-q").addEventListener("input", (e) => fillPalette(e.target.value));
$("palette-q").addEventListener("keydown", (e) => {
  if (e.key === "ArrowDown") { e.preventDefault(); movePalette(1); }
  else if (e.key === "ArrowUp") { e.preventDefault(); movePalette(-1); }
  else if (e.key === "Enter") { e.preventDefault(); choose(paletteSel); }
});
$("palette").addEventListener("click", (e) => { if (e.target === $("palette")) $("palette").close(); });

document.addEventListener("keydown", (e) => {
  if ((e.ctrlKey || e.metaKey) && !e.altKey && e.key.toLowerCase() === "k") {
    e.preventDefault();
    openPalette();
  }
  // Esc closes what is being read in the panel on the right, then gives
  // the chat its place back (dialogs and menus close themselves).
  if (e.key === "Escape" && !e.defaultPrevented && !document.querySelector("dialog[open]")) {
    if ($("side") && !$("side").hidden) {
      e.preventDefault();
      closeSide();
    } else if (document.body.classList.contains("out-full")) {
      e.preventDefault();
      setOutputsFull(false);
    }
  }
});

// "Skip to content" moves the focus past the nav. Its address is not a page
// (the router would open a new chat), so it is handled here.
document.querySelector("a.skip").addEventListener("click", (e) => {
  e.preventDefault();
  const to = (!pageHost.hidden && $("page-title")) || $("main");
  to.focus();
});

// ---------- Polling ----------
async function refreshInfo() {
  try { store.set("info", await invoke("shell_info")); } catch (e) { console.error(e); }
}
async function refreshSessions() {
  try { store.set("sessions", await invoke("sessions_live")); } catch (e) { console.error(e); }
}
async function refreshInbox() {
  try { store.set("inbox", await invoke("approvals_inbox")); } catch (e) { console.error(e); }
}
async function refreshTools() {
  try { store.set("tools", await invoke("settings_tools")); } catch (e) { console.error(e); }
}
async function refreshProjects() {
  try { store.set("projects", await invoke("projects_state")); } catch (e) { console.error(e); }
}

async function refreshWorkflows() {
  try { store.set("workflows", await invoke("workflows_state")); } catch (e) { console.error(e); }
}

async function refreshChats() {
  try { store.set("chats", await invoke("chats_list")); } catch (e) { console.error(e); }
}
const refresh = { info: refreshInfo, sessions: refreshSessions, inbox: refreshInbox, tools: refreshTools, chats: refreshChats, workflows: refreshWorkflows, projects: refreshProjects };
store.refresh = refresh;
chats.setRefresh(refresh);

let navQueued = false;
store.on((k) => {
  if (["inbox", "tools", "info", "chats", "sessions", "waiting", "workflows", "chatstate"].includes(k) && !navQueued) {
    navQueued = true;
    requestAnimationFrame(() => { navQueued = false; drawNav(); });
  }
  // The live chat started, or was cleared: the start comes or goes.
  if (k === "chatstate") posture();
});
window.addEventListener("resize", () => {
  outWidthSaid();
  layoutChanged();
});

// Tests only: a debug build may open the window with a test page to show
// in the same sandboxed frame as a session's page (e2e/frame_ipc.py). The
// window's navigation guard and its policy still apply to it.
function testFrame() {
  if (!store.info || store.info.debug !== true) return;
  const src = new URLSearchParams(location.search).get("test-frame");
  if (!src || !/^http:\/\/127\.0\.0\.1:\d+\/embed$/.test(src.split("#")[0])) return;
  const f = h("iframe", { id: "test-frame", title: "Test page", sandbox: "allow-scripts allow-same-origin", referrerpolicy: "no-referrer" });
  f.src = src;
  document.body.append(f);
}

async function boot() {
  try {
    const p = await invoke("prefs_get");
    store.prefs = p;
    applyTheme(p.theme);
  } catch (e) { console.error(e); }
  await Promise.all([refreshInfo(), refreshChats(), refreshSessions(), refreshTools(), refreshWorkflows(), refreshProjects(),
    invoke("chat_settings").then((s) => { store.ownSession = s.session || null; }).catch(() => {})]);
  testFrame();
  // The sidebar and the panel's width as the person left them. A small
  // window starts with the sidebar put away.
  let side = null;
  let width = 0;
  try { side = localStorage.getItem("columbus.sidebar"); width = Number(localStorage.getItem("columbus.outputs")) || 0; } catch { /* as new */ }
  drawNav();
  setSidebar(side ? side === "open" : window.innerWidth >= 1100, false);
  if (width) setOutWidth(width, false);
  window.addEventListener("hashchange", show);
  await show();
  booted = true;
  refreshInbox();
  setInterval(refreshSessions, 4000);
  setInterval(refreshInbox, 5000);
  setInterval(refreshWorkflows, 15000);
  setInterval(refreshTools, 15000);
  setInterval(refreshInfo, 20000);
  // New proposals or settings changed elsewhere while the app was away.
  window.addEventListener("focus", () => { refreshTools(); refreshInfo(); });
}

boot();
