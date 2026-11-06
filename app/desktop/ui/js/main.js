// The main window: the rail, the router, Ctrl+K search, and the polling
// that keeps counts fresh. Pages live in pages/*.js.
import { $, h, icon, logo, invoke, store, applyTheme } from "./core.js";
import * as home from "./pages/home.js";
import * as chats from "./pages/chats.js";
import * as documents from "./pages/documents.js";
import * as connections from "./pages/connections.js";
import * as tools from "./pages/tools.js";
import * as activity from "./pages/activity.js";
import * as settings from "./pages/settings.js";
import * as later from "./pages/later.js";

// The rail, as the design groups it (section 4.1).
const RAIL = [
  [null, [["home", "Home"], ["chats", "Chats"], ["projects", "Projects"]]],
  ["Know", [["documents", "Documents"], ["datagroups", "Data groups"], ["skills", "Skills"]]],
  ["Data", [["connections", "Connections"], ["registry", "Registry"]]],
  ["Automate", [["tools", "Tools"], ["workflows", "Workflows"]]],
  [null, [["activity", "Activity"], ["settings", "Settings"]]],
];
const SUBS = {
  activity: [["approvals", "Approvals"], ["audit", "Audit log"], ["exports", "Exports"]],
  settings: [["ai", "Models and accounts"], ["general", "General"], ["privacy", "Privacy and sandbox"], ["keyboard", "Keyboard"], ["org", "Organization"]],
};
const PAGES = {
  home: home.render,
  chats: chats.render,
  projects: later.projects,
  documents: documents.render,
  datagroups: later.dataGroups,
  skills: tools.renderSkills,
  connections: connections.render,
  registry: later.registry,
  tools: tools.render,
  workflows: later.workflows,
  activity: activity.render,
  settings: settings.render,
};
const PAGE_NAMES = Object.fromEntries(RAIL.flatMap(([, items]) => items));

function route() {
  const parts = (location.hash.replace(/^#\/?/, "") || "home").split("/").map(decodeURIComponent);
  return { page: PAGES[parts[0]] ? parts[0] : "home", rest: parts.slice(1) };
}

let unmount = null;
// Pages draw here; the live chat keeps its own host beside it (chats.js),
// so its map frame is never reloaded when the person looks elsewhere.
const pageHost = h("div", { class: "page", id: "page-host" });
$("main").append(pageHost);

async function show() {
  const r = route();
  if (unmount) {
    try { unmount(); } catch (e) { console.error(e); }
    unmount = null;
  }
  const main = pageHost;
  main.hidden = false;
  main.replaceChildren();
  drawRail(r);
  document.title = `${PAGE_NAMES[r.page] || "Columbus"} · Columbus`;
  try {
    unmount = (await PAGES[r.page](main, r.rest)) || null;
  } catch (e) {
    main.replaceChildren(h("div", { class: "pad" }, h("p", { class: "error-text", role: "alert" }, `This page could not be shown: ${e}`)));
  }
}

function count(n, quiet) {
  return n ? h("span", { class: quiet ? "count quiet" : "count", "aria-hidden": "true" }, String(n)) : null;
}

function drawRail(r = route()) {
  const rail = $("rail");
  const pending = store.inbox.reduce((n, s) => n + (s.pending || []).length, 0) + chats.waitingCount();
  const toolsWaiting = store.tools ? store.tools.waiting.length : 0;
  const nav = h("div", { class: "nav" });
  for (const [head, items] of RAIL) {
    nav.append(head ? h("div", { class: "navhead", "aria-hidden": "true" }, head) : h("div", { class: "navgap" }));
    for (const [key, label] of items) {
      const n = key === "activity" ? pending : key === "tools" ? toolsWaiting : 0;
      const a = h("a", { href: `#/${key}`, title: label, "aria-label": n ? `${label}, ${n} waiting` : label },
        icon(key), h("span", { class: "label" }, label), count(n));
      if (r.page === key) a.setAttribute("aria-current", "page");
      nav.append(a);
      // Sub-pages of the open page.
      if (r.page === key && SUBS[key]) {
        const sub = h("div", { class: "sub" });
        for (const [s, t] of SUBS[key]) {
          const sa = h("a", { href: `#/${key}/${s}` }, t);
          if ((r.rest[0] || SUBS[key][0][0]) === s) sa.setAttribute("aria-current", "page");
          sub.append(sa);
        }
        nav.append(sub);
      }
      if (key === "chats" && r.page === "chats") {
        const sub = h("div", { class: "sub" });
        for (const c of chats.recent(5)) {
          const sa = h("a", { href: c.href, title: c.title }, c.title);
          if (location.hash === c.href) sa.setAttribute("aria-current", "page");
          sub.append(sa);
        }
        if (sub.childNodes.length) nav.append(sub);
      }
    }
  }
  const info = store.info;
  const src = info ? info.policy.source : "";
  const policyText = { organization: "organization", file: "this computer's file", defaults: "Columbus defaults", unreadable: "file unreadable" }[src] || "…";
  const foot = h("div", { class: "rail-foot" },
    h("div", { class: "row", title: "Workflows are not built yet, so no runner is set up" },
      h("span", { class: "dot" }), h("span", { class: "text" }, h("b", {}, "Runner"), " not set up")),
    h("a", { class: "row", href: "#/settings/org", title: `Policy in force: ${policyText}` },
      h("span", { class: src === "unreadable" ? "dot bad" : src === "defaults" ? "dot warn" : "dot ok" }),
      h("span", { class: "text" }, "Policy: ", h("b", {}, policyText))),
    h("button", { type: "button", class: "classic", onclick: classicMenu, title: "Today's session list and settings window, kept until the main window has proved itself" }, "Classic windows…"));
  const search = h("button", { type: "button", class: "searchbtn", onclick: openPalette, "aria-label": "Search chats, sessions and pages (Ctrl+K)", title: "Search (Ctrl+K)" },
    icon("search"), h("span", { class: "label" }, "Search"), h("span", { class: "kbd" }, navigator.platform.startsWith("Mac") ? "⌘ K" : "Ctrl K"));
  const brand = h("div", { class: "brand" }, logo(), h("span", { class: "word" }, "Columbus"));
  rail.replaceChildren(brand, search, nav, h("div", { class: "spacer" }), foot);
}

function classicMenu() {
  const d = h("dialog", { "aria-label": "Classic windows" });
  const close = () => d.close();
  const go = async (which) => {
    try { await invoke("open_classic", { which }); } catch (e) { alert(String(e)); }
    close();
  };
  d.append(h("div", { class: "dlg" },
    h("h2", {}, "Classic windows"),
    h("p", { class: "muted" }, "Today's windows, kept beside the main window until it has proved itself. They change the same settings."),
    h("div", { class: "row wrap" },
      h("button", { type: "button", class: "btn", onclick: () => go("sessions") }, "Session list"),
      h("button", { type: "button", class: "btn", onclick: () => go("settings") }, "Settings window"),
      h("span", { class: "spacer grow" }),
      h("button", { type: "button", class: "btn", onclick: close }, "Close"))));
  document.body.append(d);
  d.addEventListener("close", () => d.remove());
  d.showModal();
}

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
  for (const [key, name] of Object.entries(PAGE_NAMES)) {
    if (!ql || name.toLowerCase().includes(ql)) items.push({ kind: "Page", title: name, href: `#/${key}` });
  }
  for (const [p, subs] of Object.entries(SUBS)) {
    for (const [s, t] of subs) if (ql && t.toLowerCase().includes(ql)) items.push({ kind: PAGE_NAMES[p], title: t, href: `#/${p}/${s}` });
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
        h("span", { class: "k" }, it.kind), h("span", {}, it.title), it.sub ? h("span", { class: "s" }, it.sub) : null);
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
async function refreshChats() {
  try { store.set("chats", await invoke("chats_list")); } catch (e) { console.error(e); }
}
const refresh = { info: refreshInfo, sessions: refreshSessions, inbox: refreshInbox, tools: refreshTools, chats: refreshChats };
store.refresh = refresh;
chats.setRefresh(refresh);

let railQueued = false;
store.on((k) => {
  if (["inbox", "tools", "info", "chats", "sessions", "waiting"].includes(k) && !railQueued) {
    railQueued = true;
    requestAnimationFrame(() => { railQueued = false; drawRail(); });
  }
});

async function boot() {
  try {
    const p = await invoke("prefs_get");
    store.prefs = p;
    applyTheme(p.theme);
  } catch (e) { console.error(e); }
  await Promise.all([refreshInfo(), refreshChats(), refreshSessions(), refreshTools()]);
  window.addEventListener("hashchange", show);
  await show();
  refreshInbox();
  setInterval(refreshSessions, 4000);
  setInterval(refreshInbox, 5000);
  setInterval(refreshTools, 15000);
  setInterval(refreshInfo, 20000);
  // New proposals or settings changed elsewhere while the app was away.
  window.addEventListener("focus", () => { refreshTools(); refreshInfo(); });
}

boot();
