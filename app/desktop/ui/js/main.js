// The main window: the four places, the router, Ctrl+K search, the
// settings sheet, and the polling that keeps counts fresh. Pages live in
// pages/*.js and draw inside the panel; a session's map shows behind it.
import { $, h, icon, logo, iconButton, invoke, store, applyTheme, toast, closeSide, layoutChanged } from "./core.js";
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

// The four places a person goes to.
const PLACES = [["chats", "Chats"], ["data", "Data"], ["workflows", "Workflows"], ["activity", "Activity"]];
// Every page: what draws it, the place it belongs to, and its name.
const PAGES = {
  chats: [chats.render, "chats", "Chats"],
  projects: [projects.render, "chats", "Projects"],
  data: [connections.render, "data", "Data"],
  connections: [connections.render, "data", "Data"],
  documents: [documents.render, "data", "Notes"],
  datagroups: [datagroups.render, "data", "Data groups"],
  registry: [registry.render, "data", "Registry"],
  workflows: [workflows.render, "workflows", "Workflows"],
  tools: [tools.render, "workflows", "Tools"],
  skills: [tools.renderSkills, "workflows", "Skills"],
  activity: [activity.render, "activity", "Activity"],
};
// What Ctrl+K offers besides chats (name, where, what kind).
const GOTO = [
  ["New chat", "#/chats/fresh", "Chats"], ["Chats", "#/chats", "Place"], ["Projects", "#/projects", "Chats"],
  ["Data", "#/data", "Place"], ["Datasets", "#/data/datasets", "Data"], ["Notes", "#/documents", "Data"],
  ["Organization's pack", "#/documents/pack", "Data"], ["Note proposals", "#/documents/proposals", "Data"], ["Import a document", "#/documents/import", "Data"],
  ["Data groups", "#/datagroups", "Data"], ["Map layers", "#/data/maplayers", "Data"], ["Project databases", "#/data/projectdbs", "Data"],
  ["Accounts and services", "#/data/accounts", "Data"], ["Registry: open data and GIS tools", "#/registry", "Data"],
  ["Workflows", "#/workflows", "Place"], ["Tools", "#/tools", "Workflows"], ["Skills", "#/skills", "Workflows"],
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
const pageHost = h("div", { class: "page", id: "page-host" });
$("main").append(pageHost);

async function show() {
  const r = route();
  if (r.page === "settings") {
    // Settings is a sheet over whatever page is open.
    if (!shown) await showPage({ page: "chats", rest: ["new"] });
    settings.openSheet(r.rest[0], () => {
      if (location.hash.startsWith("#/settings")) history.replaceState(null, "", underSettings);
      drawNav();
    });
    drawNav(r);
    return;
  }
  settings.closeSheet();
  underSettings = location.hash || "#/chats/new";
  await showPage(r);
}

async function showPage(r) {
  if (unmount) {
    try { unmount(); } catch (e) { console.error(e); }
    unmount = null;
  }
  closeSide();
  const main = pageHost;
  main.hidden = false;
  main.replaceChildren();
  shown = r;
  drawNav(r);
  document.title = `${PAGES[r.page][2]} · Columbus`;
  try {
    unmount = (await PAGES[r.page][0](main, r.rest)) || null;
    // After following a link, a screen reader starts at the new page's title.
    const a = document.activeElement;
    if (booted && (!a || a === document.body || $("nav").contains(a))) {
      const t = main.hidden ? null : $("page-title");
      if (t) t.focus({ preventScroll: true });
    }
  } catch (e) {
    main.replaceChildren(h("div", { class: "pad" }, h("p", { class: "error-text", role: "alert" }, `This page could not be shown: ${e}`)));
  }
  posture();
}

// Nothing on the map and nothing said yet: the panel stands in the middle
// with the question first. Anything else: it sits at the left, over the map.
function posture() {
  const start = !!shown && shown.page === "chats" && chats.isStart();
  if (document.body.classList.contains("start") !== start) {
    document.body.classList.toggle("start", start);
    layoutChanged();
    // Once more when the panel has finished moving.
    setTimeout(layoutChanged, 320);
  }
}

/** What waits for the person: requests, and things to read before they run. */
function waiting() {
  const requests = chats.otherInbox().reduce((n, s) => n + (s.pending || []).length, 0) + chats.waitingCount();
  const reviews = (store.tools ? store.tools.waiting.length : 0) + (store.workflows ? store.workflows.waiting.length : 0);
  return { requests, reviews };
}

let navKey = "";
function drawNav(r = route()) {
  const nav = $("nav");
  const place = r.page === "settings" ? (shown ? PAGES[shown.page][1] : "chats") : PAGES[r.page][1];
  const w = waiting();
  const n = w.requests + w.reviews;
  // Redrawn only when something it shows changed, keeping the focus.
  const key = JSON.stringify([place, n, r.page === "settings"]);
  if (key === navKey) return;
  navKey = key;
  const focused = nav.contains(document.activeElement) ? document.activeElement.getAttribute("href") || document.activeElement.id : null;
  const tabs = PLACES.map(([k, label]) => {
    const count = k === "activity" ? n : 0;
    const a = h("a", { class: "tab", href: `#/${k}`, "aria-label": count ? `${label}, ${count} waiting` : label },
      h("span", {}, label), count ? h("span", { class: "count", "aria-hidden": "true" }, String(count)) : null);
    if (place === k && r.page !== "settings") a.setAttribute("aria-current", "page");
    return a;
  });
  const set = h("a", { class: "iconbtn", href: "#/settings", id: "nav-settings", "aria-label": "Settings", title: "Settings" }, icon("settings"));
  if (r.page === "settings") set.setAttribute("aria-current", "page");
  nav.replaceChildren(logo(), ...tabs, h("span", { class: "spacer" }),
    h("a", { class: "iconbtn", href: "#/chats/fresh", id: "nav-new", "aria-label": "New chat", title: "New chat" }, icon("plus")),
    iconButton("search", `Search chats, sessions and pages (${navigator.platform.startsWith("Mac") ? "⌘K" : "Ctrl+K"})`, openPalette, { id: "nav-search" }),
    set);
  if (focused) {
    const again = [...nav.querySelectorAll("a, button")].find((x) => x.getAttribute("href") === focused || x.id === focused);
    if (again) again.focus();
  }
}

// ---------- The panel put away: the map alone ----------
function drawPill() {
  const pill = $("pill");
  pill.replaceChildren(logo(), h("span", { class: "t" }, chats.liveTitle() || "Columbus"),
    iconButton("panel", "Show the panel", () => setAway(false), { id: "pill-show" }));
}
function setAway(away) {
  document.body.classList.toggle("panel-away", away);
  if (away) drawPill();
  layoutChanged();
  if (!away) setTimeout(() => $("main").focus({ preventScroll: true }), 0);
}
store.setAway = setAway;

// ---------- Ctrl+K ----------
let paletteItems = [];
let paletteSel = 0;
let searchSeq = 0;

function openPalette() {
  const d = $("palette");
  if (d.open) return;
  $("palette-q").value = "";
  fillPalette("");
  d.showModal();
  $("palette-q").focus();
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
  if (["inbox", "tools", "info", "chats", "sessions", "waiting", "workflows"].includes(k) && !navQueued) {
    navQueued = true;
    requestAnimationFrame(() => { navQueued = false; drawNav(); });
  }
  // The live chat started, or was cleared: the panel takes its place.
  if (k === "chatstate") {
    posture();
    if (document.body.classList.contains("panel-away")) drawPill();
  }
});
window.addEventListener("resize", layoutChanged);

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
