// Chats: the app's own chat (moved here from the chat window), saved
// chats, and sessions started by other AI apps, each beside the output
// panel (Map, Table, Chart, Document, Files). Everything the model, the
// server or a file says is shown as text, never as markup.
import { $, el, h, icon, invoke, listen, store, button, pageHead, when, ago, size, plural, timeLeft, clientName, toast, emptyState, askConfirm, askText } from "../core.js";

// Model and thinking names, once the chat settings are read.
const NAMES = new Map();
const nameOf = (id) => NAMES.get(id) || id;
const MODEL_NOTE = "The chat sends your messages and the results it reads to the AI service you chose (Settings › Models and accounts).";

// ---------- The live chat (one at a time, as before) ----------
const live = {
  started: false,
  busy: false,
  chatId: null,
  session: null,
  view: null, // persistent DOM
  log: null,
  renderer: null,
  cards: new Map(), // prompt id -> { row, card, title }
  panel: null,
  layout: "chat",
};

/** The app's own chat's session, whose prompts are answered in the chat. */
export function liveSession() {
  return live.session;
}

/** Requests waiting in sessions other than the app's own chat. */
export function otherInbox() {
  return store.inbox.filter((s) => s.session !== live.session);
}

export function waitingCount() {
  let n = 0;
  for (const c of live.cards.values()) if (c.row.isConnected) n += 1;
  return n;
}

/** Recent chats and sessions, for the rail and Ctrl+K. */
export function recent(n) {
  const out = [];
  for (const c of store.chats) {
    out.push({ kind: c.live ? "Chat (open)" : "Chat", title: c.title, sub: ago(c.updated), href: `#/chats/c/${encodeURIComponent(c.id)}`, t: c.updated || "" });
  }
  for (const s of otherSessions()) {
    out.push({ kind: clientName(s.client), title: `${clientName(s.client)} session`, sub: s.last_used ? `used ${ago(s.last_used)}` : "not used yet", href: `#/chats/s/${encodeURIComponent(s.session)}`, t: s.last_used || s.started || "" });
  }
  out.sort((a, b) => String(b.t).localeCompare(String(a.t)));
  return out.slice(0, n);
}

// The app's own chat is known by the session id its start returned, never
// by the name a client gives itself (any client could call itself so).
function otherSessions() {
  return store.sessions.filter((s) => s.session !== live.session);
}

let refreshFns = null;
export function setRefresh(r) { refreshFns = r; }

// ---------- Markdown, safely: paragraphs, lists, tables, code, bold ----------
function inline(parent, s) {
  for (const p of s.split(/(`[^`]+`|\*\*[^*]+\*\*)/g)) {
    if (!p) continue;
    if (p.startsWith("`") && p.endsWith("`") && p.length > 2) parent.append(el("code", null, p.slice(1, -1)));
    else if (p.startsWith("**") && p.endsWith("**") && p.length > 4) parent.append(el("strong", null, p.slice(2, -2)));
    else parent.append(document.createTextNode(p));
  }
}

export function markdown(box, md) {
  const lines = String(md).replace(/\r/g, "").split("\n");
  let list = null;
  let para = [];
  const flush = () => {
    if (para.length) {
      const p = el("p");
      inline(p, para.join(" "));
      box.append(p);
      para = [];
    }
  };
  for (let i = 0; i < lines.length; i++) {
    const line = lines[i];
    if (line.startsWith("```")) {
      flush();
      list = null;
      const code = [];
      for (i++; i < lines.length && !lines[i].startsWith("```"); i++) code.push(lines[i]);
      box.append(el("pre", null, code.join("\n")));
      continue;
    }
    // A table: a row of cells, then a |---|---| line under it.
    const cells = (l) => l.trim().replace(/^\|/, "").replace(/\|$/, "").split("|").map((c) => c.trim());
    const rule = (l) => l.trim().includes("-") && cells(l).every((c) => /^:?-{3,}:?$/.test(c));
    if (line.trim().startsWith("|") && line.trim().endsWith("|") && i + 1 < lines.length && rule(lines[i + 1])) {
      flush();
      list = null;
      const head = cells(line);
      const table = el("table", "md");
      const tr = el("tr");
      for (const c of head) { const th = el("th"); inline(th, c); tr.append(th); }
      table.append(h("thead", {}, tr));
      const body = el("tbody");
      for (i += 2; i < lines.length && /^\s*\|/.test(lines[i]); i++) {
        const row = el("tr");
        const cs = cells(lines[i]);
        for (let k = 0; k < head.length; k++) { const td = el("td"); inline(td, cs[k] ?? ""); row.append(td); }
        body.append(row);
      }
      i--;
      table.append(body);
      box.append(h("div", { class: "tablewrap" }, table));
      continue;
    }
    const bullet = /^\s*[-*•]\s+(.*)$/.exec(line);
    const number = /^\s*\d+[.)]\s+(.*)$/.exec(line);
    if (bullet || number) {
      flush();
      const kind = bullet ? "ul" : "ol";
      if (!list || list.tagName.toLowerCase() !== kind) {
        list = el(kind);
        box.append(list);
      }
      const li = el("li");
      inline(li, (bullet || number)[1]);
      list.append(li);
      continue;
    }
    list = null;
    if (!line.trim()) { flush(); continue; }
    const head = /^#{1,6}\s+(.*)$/.exec(line);
    if (head) {
      flush();
      const p = el("p");
      p.append(el("strong", null, head[1]));
      box.append(p);
      continue;
    }
    para.push(line.trim());
  }
  flush();
}

// The model chose these values: quoted and cut, so they read as names.
function q(v) {
  const t = String(v).replace(/\s+/g, " ");
  return `"${t.length > 60 ? `${t.slice(0, 60)}…` : t}"`;
}

/** What a tool call is doing, in plain words. */
export function describe(name, input) {
  const i = input || {};
  const ds = i.dataset ? ` ${q(i.dataset)}` : "";
  const conn = i.connection ? ` on ${q(i.connection)}` : "";
  switch (name) {
    case "whoami": return "Checking what this session may use";
    case "list_connections": return "Listing connections";
    case "list_datasets": return `Listing datasets${conn}`;
    case "describe_dataset": return `Describing${ds}${conn}`;
    case "sample_rows": return `Sampling rows of${ds}`;
    case "query_features": return `Querying${ds}${conn}`;
    case "sql_select": return "Running a checked SQL query";
    case "vector_op": return `Processing: ${i.op ? q(i.op) : "vector operation"}`;
    case "raster_op": return `Processing: ${i.op ? q(i.op) : "raster operation"}`;
    case "verify": return "Running checks";
    case "read_output": return "Reading a result";
    case "export_output": return `Exporting${i.folder ? ` to ${q(i.folder)}` : ""}`;
    case "registry_search": return `Searching open data${i.query ? ` for ${q(i.query)}` : ""}`;
    case "registry_fetch": return "Downloading open data";
    case "context_search": return "Searching your organization's notes";
    case "context_get": return "Reading a note";
    case "draft_notes": return `Drafting notes${conn}`;
    case "update_notes": return "Changing your notes";
    case "run_python": return "Running a Python script";
    case "propose_tool": return `Testing a tool to save${i.title ? `: ${q(i.title)}` : ""}`;
    case "run_tool": return `Running ${i.name ? q(i.name) : "a tool"}`;
    case "search_tools": return "Looking up tools";
    case "describe_tool": return "Reading a tool's card";
    case "approval_decision": return "A decision on a request";
    case "list_outputs": return "Listing results";
    case "list_providers": return "Listing providers";
    case "propose_edits": return `Proposing edits${conn}`;
    case "apply_edits": return `Applying approved edits${conn}`;
    case "settings.test_connection": return `Connection test from the app${conn}`;
    default: return `Running ${q(name)}`;
  }
}

/** Draws chat events into a log: live (cards can be answered) or a saved transcript. */
class Renderer {
  constructor(log, isLive, onProvenance) {
    this.log = log;
    this.live = isLive;
    this.current = null;
    this.group = null;
    this.steps = new Map();
    this.pastCards = new Map();
    this.onProvenance = onProvenance;
  }

  endTurn() { this.current = null; this.group = null; }

  assistant() {
    if (!this.current) {
      this.current = el("div", "msg ai");
      this.log.append(this.current);
    }
    return this.current;
  }

  stepGroup() {
    if (!this.group) {
      const details = h("details", { class: "steps" });
      const count = el("span", "count");
      const last = el("span", "last");
      const badges = el("span", "row");
      const summary = h("summary", {}, count, last, badges);
      const ol = el("ol");
      details.append(summary, ol);
      this.log.append(details);
      this.group = { details, count, last, badges, ol, n: 0, fails: 0, warns: 0, running: 0 };
    }
    return this.group;
  }

  updateGroup(g) {
    g.count.textContent = plural(g.n, "step");
    g.badges.replaceChildren();
    if (g.fails) g.badges.append(el("span", "badge fail", `${g.fails} failed`));
    if (g.warns) g.badges.append(el("span", "badge warn", `${g.warns} to review`));
  }

  event(e) {
    switch (e.kind) {
      case "you":
        this.endTurn();
        this.log.append(el("div", "msg you", e.text));
        break;
      case "text":
        this.group = null;
        markdown(this.assistant(), e.text);
        break;
      case "tool_started": {
        this.current = null;
        const g = this.stepGroup();
        const li = h("li", { class: "step running" }, el("span", "what", describe(e.name, e.input)));
        const raw = h("details", {}, h("summary", {}, "Details"), el("pre", "box", `${e.name} ${JSON.stringify(e.input ?? {}, null, 2)}`));
        li.append(raw);
        g.ol.append(li);
        g.n += 1;
        g.running += 1;
        g.last.textContent = `· ${describe(e.name, e.input)}…`;
        this.updateGroup(g);
        this.steps.set(e.id, { li, g, raw });
        break;
      }
      case "tool_finished": {
        let s = this.steps.get(e.id);
        if (!s) {
          const g = this.stepGroup();
          const li = h("li", { class: "step" }, el("span", "what", describe(e.name, {})));
          g.ol.append(li);
          g.n += 1;
          s = { li, g, raw: null };
        } else {
          s.g.running = Math.max(0, s.g.running - 1);
        }
        const { li, g } = s;
        li.classList.remove("running");
        const result = el("span", e.ok ? null : "bad", e.ok ? e.summary : `Refused or failed: ${e.summary}`);
        li.insertBefore(result, s.raw || null);
        if (e.fails) li.insertBefore(el("span", "bad", `${e.fails} check${e.fails > 1 ? "s" : ""} failed`), s.raw || null);
        if (e.warns) li.insertBefore(el("span", "warn", `${e.warns} to review`), s.raw || null);
        if (e.ok && e.provenance) {
          if (!e.fails && !e.warns) li.insertBefore(el("span", "badge pass", "Checks passed"), s.raw || null);
          const b = h("button", { type: "button", class: "linkbtn", title: "Inputs, steps and checks of this result" }, "Provenance");
          b.addEventListener("click", () => this.onProvenance && this.onProvenance(e.provenance));
          li.insertBefore(b, s.raw || null);
        }
        g.fails += e.fails || 0;
        g.warns += e.warns || 0;
        g.last.textContent = `· ${li.querySelector(".what").textContent}: ${e.ok ? e.summary : "refused or failed"}`;
        this.updateGroup(g);
        break;
      }
      case "approval":
        this.endTurn();
        if (this.live) liveCard(this, e);
        else this.pastCard(e);
        return;
      case "approval_gone":
        if (this.live) gone(e.id);
        else {
          const c = this.pastCards.get(e.id);
          if (c && !c.answered) c.out.textContent = "No longer waiting: the request was withdrawn or timed out.";
        }
        break;
      case "answer": {
        const c = this.pastCards.get(e.id);
        if (c) {
          c.answered = true;
          c.out.textContent = { allow_once: "Allowed once.", always_this_session: "Allowed for this session.", decline: "Declined." }[e.decision] || "Answered.";
        }
        break;
      }
      case "status":
        this.endTurn();
        this.log.append(el("div", "status", e.text));
        break;
      case "error":
        this.endTurn();
        this.log.append(el("div", "error", e.text === "stopped" ? "Stopped." : `Could not finish: ${e.text}`));
        break;
      case "done":
        this.endTurn();
        break;
      case "model":
        this.endTurn();
        this.log.append(el("div", "status", `From here on: ${nameOf(e.model)}${e.effort ? `, thinking ${nameOf(`effort:${e.effort}`).toLowerCase()}` : ""}.`));
        break;
      default:
        break;
    }
  }

  pastCard(e) {
    const card = el("div", "ask past");
    card.append(el("h3", null, e.title), el("p", "body", e.summary));
    if (e.code != null) {
      const lines = e.code.replace(/\r?\n$/, "").split(/\r?\n/);
      card.append(el("p", "code-label", `Code written by the AI (${lines.length} line${lines.length === 1 ? "" : "s"}):`));
      const pre = el("pre", "code");
      lines.forEach((line) => pre.append(el("span", "ln", line === "" ? " " : line)));
      card.append(pre);
    }
    const out = el("p", "done", "Not answered.");
    card.append(out);
    this.log.append(card);
    this.pastCards.set(e.id, { out, answered: false });
  }
}

// An approval prompt from the chat's own session, answered here as the
// chat window always did: buttons wait a second, scripts are once only.
function liveCard(r, e) {
  const card = el("div", "ask");
  card.setAttribute("role", "alertdialog");
  card.setAttribute("aria-label", e.title);
  card.append(el("h3", null, e.title));
  // The server's own words.
  card.append(el("p", "body", e.summary));
  if (e.code != null) {
    // Written by the AI: in its own box, never mixed with the server's words.
    const lines = e.code.replace(/\r?\n$/, "").split(/\r?\n/);
    const n = lines.length;
    card.append(el("p", "code-label", `Code written by the AI (${n} line${n === 1 ? "" : "s"}). Read it before allowing:`));
    const pre = el("pre", "code");
    lines.forEach((line) => pre.append(el("span", "ln", line === "" ? " " : line)));
    card.append(pre);
  }
  const row = el("div", "row");
  const answer = async (decision, label) => {
    for (const b of row.querySelectorAll("button")) b.disabled = true;
    try {
      await invoke("chat_answer", { id: e.id, decision });
      row.replaceWith(el("p", "done", label));
    } catch (err) {
      row.replaceWith(el("p", "error", String(err)));
    }
    store.set("waiting", waitingCount());
    drawTray();
  };
  const buttons = [
    ["Allow once", "allow_once", "primary", "Allowed once."],
    ...(e.offer_always ? [["Allow for this session", "always_this_session", "", "Allowed for this session."]] : []),
    ["Decline", "decline", "danger", "Declined."],
  ];
  for (const [label, decision, cls, done] of buttons) {
    const b = el("button", cls, label);
    b.type = "button";
    // Not clickable for a moment, so a click meant for something else
    // cannot land on it.
    b.disabled = true;
    setTimeout(() => (b.disabled = false), 1000);
    b.addEventListener("click", () => answer(decision, done));
    row.append(b);
  }
  card.append(row);
  r.log.append(card);
  live.cards.set(e.id, { row, card, title: e.title });
  store.set("waiting", waitingCount());
  drawTray();
  // Show the card from its title, not its end; bring the chat forward.
  if (!location.hash.startsWith("#/chats")) toast(`Your chat is asking: ${e.title}`);
  card.scrollIntoView({ block: "start" });
}

function gone(id) {
  const c = live.cards.get(id);
  live.cards.delete(id);
  if (c && c.row.isConnected) c.row.replaceWith(el("p", "done", "No longer waiting: the request was withdrawn or timed out."));
  store.set("waiting", waitingCount());
  drawTray();
}

// ---------- The output panel ----------

const TABS = [["map", "Map"], ["table", "Table"], ["chart", "Chart"], ["document", "Document"], ["files", "Files"]];
const APP_FRAME = "allow-scripts allow-same-origin";

/** The output panel for one session: the session's own page (map and
 * table) in a sandboxed frame, and the files it made. */
class Panel {
  constructor(onSheetClose) {
    this.session = null;
    this.tab = "map";
    this.split = false;
    this.frame = null;
    this.origin = null;
    this.prov = null;
    this.el = h("section", { class: "outpanel", "aria-label": "Output panel" });
    const list = h("div", { role: "tablist", "aria-label": "Output" });
    this.tabButtons = TABS.map(([k, label]) => {
      const b = h("button", { type: "button", role: "tab", id: `out-tab-${k}-${Panel.n = (Panel.n || 0) + 1}`, "aria-selected": "false", tabindex: "-1" }, label);
      b.addEventListener("click", () => this.setTab(k));
      b.addEventListener("keydown", (e) => {
        const i = TABS.findIndex(([x]) => x === k);
        if (e.key === "ArrowRight" || e.key === "ArrowLeft") {
          e.preventDefault();
          const n = TABS[(i + (e.key === "ArrowRight" ? 1 : TABS.length - 1)) % TABS.length][0];
          this.setTab(n);
          this.tabButtons[TABS.findIndex(([x]) => x === n)].focus();
        }
      });
      list.append(b);
      return b;
    });
    this.splitBtn = h("button", { type: "button", class: "btn sm", "aria-pressed": "false", title: "Map above table" }, "Split");
    this.splitBtn.addEventListener("click", () => { this.split = !this.split; this.splitBtn.setAttribute("aria-pressed", String(this.split)); this.post(); });
    this.popBtn = h("button", { type: "button", class: "btn sm", title: "Open the session's map in its own window" }, "Pop out");
    this.popBtn.addEventListener("click", () => this.popOut());
    const close = h("button", { type: "button", class: "btn sm sheet-close", "aria-label": "Close the map" }, icon("close"));
    close.addEventListener("click", () => onSheetClose && onSheetClose());
    this.el.append(h("div", { class: "outtabs" }, list, h("div", { class: "tools" }, this.splitBtn, this.popBtn, close)));
    this.body = el("div", "outbody");
    this.body.setAttribute("role", "tabpanel");
    this.frameHost = el("div", "outbody");
    this.paneHost = el("div", "pane");
    this.body.append(this.frameHost, this.paneHost);
    this.el.append(this.body);
    this.onMessage = (ev) => {
      if (!this.frame || ev.source !== this.frame.contentWindow || ev.origin !== this.origin) return;
      if (ev.data && ev.data.type === "columbus-embed-ready") this.post();
    };
    window.addEventListener("message", this.onMessage);
    this.unsub = store.on((k) => { if (k === "prefs") this.post(); });
    this.setTab("map");
  }

  destroy() {
    window.removeEventListener("message", this.onMessage);
    this.unsub();
    this.frame = null;
  }

  post() {
    if (!this.frame || !this.origin || !this.frame.contentWindow) return;
    const view = this.split ? "split" : this.tab === "table" ? "table" : "map";
    this.frame.contentWindow.postMessage({ type: "columbus-embed", view, basemap: !!store.prefs.basemap, theme: store.prefs.theme || "system" }, this.origin);
  }

  setTab(k) {
    this.tab = k;
    this.tabButtons.forEach((b, i) => {
      const on = TABS[i][0] === k;
      b.setAttribute("aria-selected", String(on));
      b.tabIndex = on ? 0 : -1;
    });
    this.body.setAttribute("aria-labelledby", this.tabButtons[TABS.findIndex(([x]) => x === k)].id);
    const framed = k === "map" || k === "table";
    this.frameHost.hidden = !framed;
    this.paneHost.hidden = framed;
    this.splitBtn.hidden = !framed;
    if (framed) {
      this.ensureFrame();
      this.post();
    } else this.drawPane();
  }

  setSession(session) {
    if (session === this.session) return;
    this.session = session;
    this.frame = null;
    this.origin = null;
    this.prov = null;
    this.frameHost.replaceChildren();
    this.setTab(this.tab);
  }

  showProvenance(id) {
    this.prov = id;
    this.setTab("files");
  }

  async popOut() {
    const s = store.sessions.find((x) => x.session === this.session);
    if (!s) return toast("The session is not running.");
    try { await invoke("open_session", { port: s.port }); } catch (e) { toast(String(e)); }
  }

  async ensureFrame() {
    if (this.frame || this.loading) return;
    this.popBtn.disabled = !this.session;
    if (!this.session) {
      this.frameHost.replaceChildren(h("div", { class: "state" }, h("h3", {}, "No map yet"),
        h("p", {}, "The session's layers and tables appear here once the chat has started its Columbus session.")));
      return;
    }
    this.loading = true;
    const want = this.session;
    this.frameHost.replaceChildren(h("div", { class: "state", role: "status" }, h("p", {}, "Opening the session's map…")));
    try {
      const url = await invoke("session_page", { session: want });
      if (want !== this.session) return;
      const f = h("iframe", {
        title: "The session's map and table",
        sandbox: APP_FRAME,
        referrerpolicy: "no-referrer",
      });
      this.origin = new URL(url).origin;
      f.src = url;
      this.frame = f;
      this.frameHost.replaceChildren(f);
    } catch (e) {
      if (want !== this.session) return;
      this.frameHost.replaceChildren(h("div", { class: "state" }, h("h3", {}, "The map could not be shown here"),
        h("p", {}, String(e)),
        h("div", { class: "row wrap" },
          button("Try again", "", () => { this.frame = null; this.ensureFrame(); }),
          button("Open in its own window", "", () => this.popOut()))));
    } finally {
      this.loading = false;
    }
  }

  async drawPane() {
    const pane = this.paneHost;
    pane.replaceChildren();
    if (this.tab === "chart") {
      pane.append(h("div", { class: "state" }, h("h3", {}, "Charts are not here yet"),
        h("p", {}, "Quick charts of a table (counts by code, by county) will show here. Until then, ask the chat to summarise a table, or open it in the Table tab.")));
      return;
    }
    if (this.tab === "document") {
      pane.append(h("div", { class: "state" }, h("h3", {}, "Documents are not here yet"),
        h("p", {}, "The note or report in use will show here. Your notes are under Documents in the rail."), h("a", { class: "btn", href: "#/documents/notes" }, "Open Documents")));
      return;
    }
    if (!this.session) {
      pane.append(h("div", { class: "state" }, h("h3", {}, "No files yet"), h("p", {}, "Files the session makes are listed here.")));
      return;
    }
    const box = h("div", { class: "pad stack" });
    pane.append(box);
    if (this.prov) {
      const card = h("section", { class: "card" }, h("div", { class: "hd" }, h("h3", {}, "Provenance"), h("span", { class: "chip mono" }, this.prov),
        h("button", { type: "button", class: "btn sm more", onclick: () => { this.prov = null; this.drawPane(); } }, "Close")));
      const bd = h("div", { class: "bd" }, el("p", "muted", "Reading…"));
      card.append(bd);
      box.append(card);
      try {
        const p = await invoke("session_provenance", { session: this.session, id: this.prov });
        bd.replaceChildren(provenanceView(p));
      } catch (e) {
        bd.replaceChildren(el("p", "error-text", String(e)));
      }
    }
    try {
      const f = await invoke("session_files", { session: this.session });
      const files = f.files || [];
      box.append(h("p", { class: "small muted wrap-any" }, `In ${f.folder}`));
      if (!files.length) {
        box.append(h("p", { class: "muted" }, "Nothing yet. Layers, tables and files the session makes are listed here."));
        return;
      }
      const tbody = h("tbody");
      for (const x of files) {
        tbody.append(h("tr", {}, h("td", { class: "code" }, x.path), h("td", { class: "num nowrap" }, size(x.bytes)), h("td", { class: "nowrap muted" }, when(x.modified))));
      }
      box.append(h("div", { class: "card" }, h("table", { class: "t" }, h("thead", {}, h("tr", {}, h("th", {}, "File"), h("th", { class: "num" }, "Size"), h("th", {}, "Made"))), tbody)));
      box.append(h("p", { class: "small muted" }, "Export goes through your organization's export rules: ask the chat to export a file."));
    } catch (e) {
      box.append(el("p", "error-text", String(e)));
    }
  }
}

function provenanceView(p) {
  const box = h("div", { class: "stack" });
  const checks = Array.isArray(p.checks) ? p.checks : [];
  if (checks.length) {
    const ul = h("ul", { class: "list" });
    for (const c of checks) {
      ul.append(h("li", {}, el("span", `badge ${c.severity === "fail" ? "fail" : c.severity === "warn" ? "warn" : "mute"}`, c.severity === "fail" ? "Failed" : c.severity === "warn" ? "Review" : "Note"),
        h("div", { class: "text" }, el("span", null, String(c.message ?? "")), c.fix ? el("span", "where", `Fix: ${c.fix}`) : null)));
    }
    box.append(ul);
  } else box.append(el("p", "muted", "No checks recorded."));
  const inputs = Array.isArray(p.inputs) ? p.inputs : [];
  if (inputs.length) {
    box.append(el("p", "strong small", "Made from"));
    const ul = h("ul", { class: "list" });
    for (const x of inputs) ul.append(h("li", {}, el("span", "wrap-any small", `${String(x.dataset ?? x.handle ?? x.path ?? JSON.stringify(x))}${x.connection ? ` (${x.connection})` : ""}`)));
    box.append(ul);
  }
  const d = h("details", {}, h("summary", { class: "small" }, "The full record"), el("pre", "box", JSON.stringify(p, null, 2)));
  box.append(d);
  return box;
}

// ---------- The waiting tray ----------
let trayEl = null;
let trayFor = null; // "live" or a session id

function drawTray() {
  if (!trayEl) return;
  trayEl.replaceChildren();
  let items = [];
  if (trayFor === "live") {
    for (const [id, c] of live.cards) if (c.row.isConnected) items.push({ title: c.title, go: () => c.card.scrollIntoView({ block: "start" }), id });
    if (!items.length) { trayEl.hidden = true; return; }
    trayEl.append(h("div", { class: "hd" }, h("span", { class: "dot" }), `Waiting for you · ${items.length}`, h("span", { class: "muted small" }, "Answer in the chat below")));
    const ul = h("ul");
    for (const it of items) ul.append(h("li", {}, h("span", { class: "s" }, it.title), button("Show", "sm", it.go)));
    trayEl.append(ul);
  } else {
    const s = store.inbox.find((x) => x.session === trayFor);
    items = (s && s.pending) || [];
    if (!items.length) { trayEl.hidden = true; return; }
    const open = button("Decide in browser", "primary sm", () => openBrowser(trayFor));
    trayEl.append(h("div", { class: "hd" }, h("span", { class: "dot" }), `Waiting for you · ${items.length}`, h("span", { class: "right" }, open)));
    const ul = h("ul");
    for (const a of items) ul.append(h("li", {}, h("span", { class: `risk ${a.risk}` }, a.risk), h("span", { class: "s", title: a.summary }, a.summary), h("span", { class: "tiny muted" }, timeLeft(a.expires))));
    trayEl.append(ul, h("p", { class: "tiny muted" }, "Requests from other AI apps are decided in that app's own prompt, or on the browser page Columbus opens. This app only shows them."));
  }
  trayEl.hidden = false;
}

export async function openBrowser(session) {
  try {
    const opened = await invoke("approvals_open_browser", { session });
    toast(opened ? "Columbus opened the requests in your browser. Decide there." : "Nothing is waiting in that session any more.");
  } catch (e) {
    toast(String(e));
  }
}

// ---------- Layout switch ----------
function layoutKey(id) { return `columbus.layout.${id || "new"}`; }
function getLayout(id) {
  try { return localStorage.getItem(layoutKey(id)) === "map" ? "map" : "chat"; } catch { return "chat"; }
}
function setLayout(id, v) {
  try { localStorage.setItem(layoutKey(id), v); } catch { /* not remembered */ }
}

function layoutSwitch(view, id) {
  const a = h("button", { type: "button", "aria-pressed": "true" }, "Chat first");
  const b = h("button", { type: "button", "aria-pressed": "false" }, "Map first");
  const apply = (v) => {
    view.classList.toggle("mapfirst", v === "map");
    a.setAttribute("aria-pressed", String(v !== "map"));
    b.setAttribute("aria-pressed", String(v === "map"));
  };
  a.addEventListener("click", () => { apply("chat"); setLayout(id(), "chat"); });
  b.addEventListener("click", () => { apply("map"); setLayout(id(), "map"); });
  const seg = h("div", { class: "seg", role: "group", "aria-label": "Layout" }, a, b);
  seg.apply = apply;
  return seg;
}

// ---------- The live chat view (built once, kept while the app runs) ----------
function buildLive() {
  const view = h("div", { class: "chatview", id: "chatview" });
  const title = h("h1", { id: "chat-title" }, "New chat");
  const sub = h("p", { class: "sub", id: "chat-sub" }, "In the Columbus app · Unfiled");
  const seg = layoutSwitch(view, () => live.chatId);
  const mapBtn = button("Map", "sm sheet-only", () => view.classList.add("sheet-open"), { id: "map", "aria-label": "Show the map and table" });
  const newBtn = button("New chat", "sm", newChat, { id: "new" });
  const head = h("header", { class: "chathead" }, h("div", { class: "titlebox" }, title, sub), h("div", { class: "actions" }, seg, mapBtn, newBtn));
  const tray = h("section", { class: "tray", "aria-label": "Waiting for you", hidden: true });
  const log = h("div", { id: "log", role: "log", "aria-live": "polite", "aria-label": "Conversation" });
  const text = h("textarea", { id: "text", rows: "3", placeholder: "Ask about your data, for example: Which datasets can I use, and what CRS are they in?" });
  const status = h("span", { id: "status" });
  const stop = h("button", { type: "button", id: "stop", class: "btn", hidden: true }, "Stop");
  const send = h("button", { type: "submit", id: "send", class: "btn primary" }, "Send");
  const chip = h("span", { class: "chip", id: "model-chip", hidden: true }, "");
  // This chat's model and thinking level: from Settings for a new chat,
  // changeable between messages.
  const modelSel = h("select", { id: "chat-model", class: "pick", "aria-label": "Model for this chat" });
  const effortSel = h("select", { id: "chat-effort", class: "pick", "aria-label": "Thinking level for this chat" });
  const onPick = async () => {
    const pick = { model: modelSel.value, effort: effortSel.value };
    if (!live.started) { live.pick = pick; return; }
    try {
      await invoke("chat_set_model", pick);
      live.pick = pick;
      live.renderer.event({ kind: "model", ...pick });
    } catch (err) {
      toast(String(err));
      await updateChip();
    }
  };
  modelSel.addEventListener("change", onPick);
  effortSel.addEventListener("change", onPick);
  const composer = h("form", { id: "composer" },
    h("label", { for: "text", class: "visually-hidden" }, "Message"), text,
    h("div", { class: "row" }, status, modelSel, effortSel, chip, stop, send));
  const col = h("section", { class: "chatcol", "aria-label": "Chat" }, head, tray, log, composer);
  live.panel = new Panel(() => view.classList.remove("sheet-open"));
  view.append(col, live.panel.el);
  live.view = view;
  live.log = log;
  live.seg = seg;
  live.renderer = new Renderer(log, true, (id) => { live.panel.showProvenance(id); view.classList.add("sheet-open"); });

  composer.addEventListener("submit", async (ev) => {
    ev.preventDefault();
    const t = text.value.trim();
    if (!t || live.busy) return;
    if (!(await start())) return;
    log.querySelector(".chat-empty")?.remove();
    live.renderer.endTurn();
    log.append(el("div", "msg you", t));
    text.value = "";
    log.scrollTop = log.scrollHeight;
    setBusy(true);
    try {
      await invoke("chat_send", { text: t });
      if (!title.dataset.set) { title.textContent = t.length > 70 ? `${t.slice(0, 70)}…` : t; title.dataset.set = "1"; }
      refreshFns && refreshFns.chats();
    } catch (err) {
      log.append(el("div", "error", String(err)));
      setBusy(false);
    }
  });
  text.addEventListener("keydown", (ev) => {
    if (ev.key === "Enter" && !ev.shiftKey && !ev.isComposing) {
      ev.preventDefault();
      composer.requestSubmit();
    }
  });
  stop.addEventListener("click", () => invoke("chat_stop"));
  view.addEventListener("keydown", (e) => { if (e.key === "Escape") view.classList.remove("sheet-open"); });
  return view;
}

function setBusy(b) {
  live.busy = b;
  $("send").disabled = b;
  $("stop").hidden = !b;
  $("chat-model").disabled = b;
  $("chat-effort").disabled = b;
  $("status").textContent = b ? "Working…" : "";
}

async function start() {
  if (live.started) return true;
  $("status").textContent = "Starting a Columbus session…";
  try {
    const s = await invoke("chat_start", live.pick || {});
    live.started = true;
    live.session = s.session || null;
    live.chatId = s.chat || null;
    live.panel.setSession(live.session);
    if (live.chatId) {
      history.replaceState(null, "", `#/chats/c/${encodeURIComponent(live.chatId)}`);
      const l = getLayout(null);
      setLayout(live.chatId, l);
    }
    $("status").textContent = "";
    refreshFns && (refreshFns.chats(), refreshFns.sessions());
    return true;
  } catch (err) {
    $("status").textContent = "";
    live.log.append(el("div", "error", String(err)));
    if (/API key|server was not found/i.test(String(err))) {
      live.log.append(h("p", { class: "small" }, h("a", { href: "#/settings/ai" }, "Set up the AI in Settings › Models and accounts")));
    }
    return false;
  }
}

async function newChat() {
  const was = live.chatId;
  await invoke("chat_end");
  live.started = false;
  live.busy = false;
  live.chatId = null;
  live.session = null;
  live.pick = null;
  live.cards.clear();
  live.log.replaceChildren();
  live.renderer = new Renderer(live.log, true, (id) => { live.panel.showProvenance(id); live.view.classList.add("sheet-open"); });
  $("chat-title").textContent = "New chat";
  delete $("chat-title").dataset.set;
  live.panel.setSession(null);
  setBusy(false);
  await updateChip();
  store.set("waiting", 0);
  drawTray();
  // Only if the person is still looking at this chat.
  if (location.hash === "#/chats/new" || (was && location.hash === `#/chats/c/${encodeURIComponent(was)}`)) history.replaceState(null, "", "#/chats/new");
  await emptyChat();
  refreshFns && refreshFns.chats();
  $("text").focus();
}

async function emptyChat() {
  if (live.log.childNodes.length) return;
  const box = h("div", { class: "chat-empty" }, h("h2", {}, "Ask about your data"),
    h("p", { class: "muted" }, "Your assistant works through Columbus: it sees only what your connections allow, every result is checked, and anything that changes data or leaves this computer asks you first."));
  const starters = h("div", { class: "starters" });
  box.append(starters);
  live.log.append(box);
  let ids = [];
  try {
    const st = await invoke("settings_state");
    ids = st.connections.filter((c) => c.id).map((c) => c.id);
  } catch { /* no starters from connections */ }
  const prompts = ids.length
    ? [`Which datasets can I use on ${ids[0]}, and what CRS are they in?`,
      `Describe the largest dataset on ${ids[0]}: fields, codes and row count.`,
      ids[1] ? `Which datasets on ${ids[0]} and ${ids[1]} share a key field?` : `Check the datasets on ${ids[0]} for empty or invalid geometries.`]
    : ["Which connections and datasets can you use?", "What can you do with my GIS data through Columbus?"];
  for (const p of prompts) starters.append(button(p, "", () => { $("text").value = p; $("text").focus(); }));
  try {
    const s = await invoke("chat_settings");
    if (s.has_key && s.server_found && (s.server_older || !s.engine)) {
      box.append(h("div", { class: "notice warn" }, h("div", {},
        s.server_older ? `The Columbus server this chat uses is version ${s.server_version}, older than this app (${s.app_version}), so the map and newer tools may not work. ` : "The GIS engine was not found, so tools that read data will fail. ",
        h("a", { href: "#/settings/ai" }, "See Settings › Models and accounts"), ".")));
    }
    if (!s.has_key || !s.server_found) {
      box.append(h("div", { class: "notice warn" }, h("div", {},
        !s.has_key ? "No AI is connected yet. " : "Columbus's server was not found. ",
        h("a", { href: "#/settings/ai" }, "Set it up in Settings › Models and accounts"), ", or follow the first steps on ", h("a", { href: "#/home" }, "Home"), ".")));
    }
  } catch { /* shown when sending */ }
}

async function updateChip() {
  try {
    const s = await invoke("chat_settings");
    const pick = live.pick || { model: s.model, effort: s.effort };
    for (const [id, l] of s.models) NAMES.set(id, l.replace(/ \(.*\)$/, ""));
    for (const [id, l] of s.efforts) if (id) NAMES.set(`effort:${id}`, l.replace(/ \(.*\)$/, ""));
    const fill = (sel, items, cur, label) => {
      sel.replaceChildren(...items.map(([id, l]) => {
        const o = el("option", null, label(id, l));
        o.value = id;
        o.selected = id === cur;
        return o;
      }));
    };
    fill($("chat-model"), s.models, pick.model, (_, l) => l.replace(/ \(.*\)$/, ""));
    fill($("chat-effort"), s.efforts, pick.effort, (id, l) => id ? `Thinking: ${l.replace(/ \(.*\)$/, "")}` : "Thinking: model's default");
    $("chat-model").title = `${MODEL_NOTE} ${(s.models.find(([id]) => id === pick.model) || [0, ""])[1]}`;
    $("chat-effort").title = "How much the model thinks before it answers. Higher is slower, costs more and is more thorough.";
    const rows = store.info?.policy?.in_force?.return_rows;
    const chip = $("model-chip");
    chip.hidden = !rows;
    chip.textContent = rows ? `AI sees at most ${Number(rows).toLocaleString()} rows per call` : "";
  } catch { /* none */ }
}

// Events of the live chat, wherever the window is.
listen("chat-event", ({ payload: e }) => {
  if (!live.view) buildLive();
  live.renderer.event(e);
  if (e.kind === "error" || e.kind === "done") setBusy(false);
  if (e.kind === "done") refreshFns && refreshFns.chats();
  live.log.scrollTop = live.log.scrollHeight;
});

// ---------- Pages ----------
let host = null; // where the live view sits (beside the page host)

export function liveHost() {
  if (!host) {
    host = h("div", { class: "page", id: "live-host", hidden: true });
    host.append(live.view || buildLive());
  }
  return host;
}

export async function render(main, rest) {
  const [kind, id] = rest;
  if (kind === "new" || (kind === "c" && id && id === live.chatId)) return showLive(main);
  if (kind === "c" && id) return showSaved(main, id);
  if (kind === "s" && id) return showSession(main, id);
  return showIndex(main);
}

function showLive(main) {
  const hostEl = liveHost();
  if (!hostEl.isConnected) main.parentElement.append(hostEl);
  main.hidden = true;
  hostEl.hidden = false;
  trayEl = live.view.querySelector(".tray");
  trayFor = "live";
  drawTray();
  live.seg.apply(getLayout(live.chatId));
  updateChip();
  emptyChat();
  if (live.session) live.panel.setSession(live.session);
  else live.panel.setTab(live.panel.tab);
  setTimeout(() => $("text")?.focus(), 0);
  return () => {
    hostEl.hidden = true;
    main.hidden = false;
    trayEl = null;
  };
}

async function showSaved(main, id) {
  const view = h("div", { class: "chatview" });
  const panel = new Panel(() => view.classList.remove("sheet-open"));
  const title = h("h1", {}, "Chat");
  const sub = h("p", { class: "sub" }, "");
  const seg = layoutSwitch(view, () => id);
  const del = button("Delete", "sm danger", async () => {
    if (!(await askConfirm("Delete this chat?", "Its saved conversation is deleted from this computer. Files its session made stay where they are.", "Delete", true))) return;
    try { await invoke("chat_delete", { id }); refreshFns.chats(); location.hash = "#/chats"; } catch (e) { toast(String(e)); }
  });
  const rename = button("Rename", "sm", async () => {
    const t = await askText("Rename this chat", "Title", title.textContent);
    if (!t) return;
    try { await invoke("chat_rename", { id, title: t }); title.textContent = t; refreshFns.chats(); } catch (e) { toast(String(e)); }
  });
  const mapBtn = button("Map", "sm sheet-only", () => view.classList.add("sheet-open"));
  const head = h("header", { class: "chathead" }, h("div", { class: "titlebox" }, title, sub), h("div", { class: "actions" }, seg, mapBtn, rename, del));
  const ended = h("div", { class: "ended" }, "This chat has ended. Its conversation is kept here; Columbus cannot continue it.", button("New chat", "sm primary", () => { location.hash = "#/chats/new"; }));
  const logEl = h("div", { id: "saved-log", class: "log", role: "log", "aria-label": "Saved conversation" });
  const col = h("section", { class: "chatcol", "aria-label": "Saved chat" }, head, logEl);
  view.append(col, panel.el);
  main.append(view);
  seg.apply(getLayout(id));
  try {
    const lines = await invoke("chat_transcript", { id });
    const meta = lines.find((l) => l.kind === "meta") || {};
    const s = store.chats.find((c) => c.id === id);
    title.textContent = s ? s.title : "Chat";
    sub.textContent = `In the Columbus app · Unfiled · ${when(lines[0]?.ts)}`;
    const r = new Renderer(logEl, false, (pid) => { panel.showProvenance(pid); view.classList.add("sheet-open"); });
    for (const l of lines) r.event(l);
    logEl.append(ended);
    const running = meta.session && store.sessions.some((x) => x.session === meta.session);
    panel.setSession(running ? meta.session : null);
    if (!running) panel.frameHost.replaceChildren(h("div", { class: "state" }, h("h3", {}, "Its session has ended"), h("p", {}, "The map and table show a running session only. The files it made stay in its workspace.")));
  } catch (e) {
    logEl.append(el("p", "error-text", String(e)));
  }
  return () => panel.destroy();
}

async function showSession(main, session) {
  const view = h("div", { class: "chatview" });
  const panel = new Panel(() => view.classList.remove("sheet-open"));
  const s = store.sessions.find((x) => x.session === session);
  const who = clientName(s?.client);
  const title = h("h1", {}, s ? `${who} session` : "Session");
  const sub = h("p", { class: "sub" }, s ? `Started ${when(s.started)} · ${s.last_used ? `used ${ago(s.last_used)}` : "not used yet"} · the conversation lives in ${who}` : "This session is not running.");
  const seg = layoutSwitch(view, () => `s-${session}`);
  const mapBtn = button("Map", "sm sheet-only", () => view.classList.add("sheet-open"));
  const head = h("header", { class: "chathead" }, h("div", { class: "titlebox" }, title, sub), h("div", { class: "actions" }, seg, mapBtn));
  const tray = h("section", { class: "tray", "aria-label": "Waiting for you", hidden: true });
  const acts = h("ol", { class: "activity", "aria-label": "Tool calls in this session" });
  const body = h("div", { class: "scroll pad stack", style: null }, h("p", { class: "muted small" }, `What ${who}'s assistant did through Columbus in this session, from the audit log. The conversation itself is in ${who}.`), acts);
  const col = h("section", { class: "chatcol", "aria-label": "Session activity" }, head, tray, body);
  view.append(col, panel.el);
  main.append(view);
  seg.apply(getLayout(`s-${session}`));
  trayEl = tray;
  trayFor = session;
  drawTray();
  panel.setSession(s ? session : null);
  let shown = -1;
  const load = async () => {
    try {
      const lines = await invoke("session_activity", { session });
      if (lines.length === shown) return;
      shown = lines.length;
      acts.replaceChildren();
      if (!lines.length) acts.append(h("li", {}, h("span", { class: "t" }, ""), h("span", { class: "d muted" }, "No tool calls yet.")));
      for (const l of lines) {
        const t = new Date(l.ts);
        const bits = [describe(l.tool, { dataset: l.dataset, connection: l.connection })];
        if (l.rows != null) bits.push(`${Number(l.rows).toLocaleString()} rows`);
        const dec = l.decision === "allowed" ? null : l.decision;
        acts.append(h("li", {}, h("span", { class: "t" }, isNaN(+t) ? "" : t.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" })),
          h("span", { class: "d" }, bits.join(" · "), dec ? " " : null, dec ? el("span", `badge ${dec === "denied" || dec === "error" ? "fail" : "warn"}`, dec.replace(/_/g, " ")) : null,
            l.reason && dec ? el("span", "where muted small", ` ${l.reason}`) : null)));
      }
      body.scrollTop = body.scrollHeight;
    } catch (e) {
      acts.replaceChildren(h("li", {}, h("span"), h("span", { class: "d error-text" }, String(e))));
    }
  };
  load();
  const timer = setInterval(load, 5000);
  const unsub = store.on((k) => { if (k === "inbox") drawTray(); });
  return () => { clearInterval(timer); unsub(); panel.destroy(); trayEl = null; };
}

function showIndex(main) {
  const search = h("input", { type: "search", placeholder: "Search titles and content", "aria-label": "Search chats", class: "grow" });
  const head = pageHead("Chats", "Your chats in the app, and sessions other AI apps run on this computer. Projects come later: every chat is Unfiled for now.",
    [button("New chat", "primary", () => { location.hash = "#/chats/new"; })]);
  const list = h("div", { class: "stack" });
  const body = h("div", { class: "scroll" }, h("div", { class: "pad stack" }, h("div", { class: "row" }, search), list));
  main.append(h("div", { class: "page" }, head, body));
  let seq = 0;
  const draw = async () => {
    const ql = search.value.trim().toLowerCase();
    const mine = ++seq;
    let hits = null;
    if (ql.length >= 2) {
      try { hits = await invoke("chats_search", { query: search.value }); } catch { hits = []; }
      if (mine !== seq) return;
    }
    list.replaceChildren();
    const others = otherSessions();
    if (others.length && !ql) {
      const ul = h("ul", { class: "list" });
      for (const s of others) {
        const li = h("li", { class: "clickable" }, h("div", { class: "text" },
          h("b", {}, `${clientName(s.client)} session`),
          h("span", { class: "where" }, `Started ${when(s.started)} · ${s.last_used ? `used ${ago(s.last_used)}` : "not used yet"}`)),
          h("span", { class: "chip blue" }, clientName(s.client)),
          h("a", { class: "btn sm", href: `#/chats/s/${encodeURIComponent(s.session)}` }, "Open"));
        li.addEventListener("click", (e) => { if (e.target.tagName !== "A") location.hash = `#/chats/s/${encodeURIComponent(s.session)}`; });
        ul.append(li);
      }
      list.append(h("section", { class: "card" }, h("div", { class: "hd" }, h("h2", {}, "Running in other AI apps"), h("span", { class: "muted small" }, "their activity and map; the conversation is in that app")), h("div", { class: "bd flush" }, ul)));
    }
    let chats = store.chats;
    const snippets = new Map();
    if (hits) {
      for (const f of hits) snippets.set(f.id, f.snippet);
      chats = chats.filter((c) => snippets.has(c.id) || c.title.toLowerCase().includes(ql));
    } else if (ql) chats = chats.filter((c) => c.title.toLowerCase().includes(ql));
    const ul = h("ul", { class: "list" });
    for (const c of chats) {
      const href = `#/chats/c/${encodeURIComponent(c.id)}`;
      const li = h("li", { class: "clickable" }, h("div", { class: "text" }, h("b", {}, c.title),
        h("span", { class: "where" }, snippets.get(c.id) || `${plural(c.messages, "message")} · ${ago(c.updated)}`)),
        c.live ? h("span", { class: "badge info" }, "Open now") : null,
        h("a", { class: "btn sm", href }, "Open"));
      li.addEventListener("click", (e) => { if (e.target.tagName !== "A") location.hash = href; });
      ul.append(li);
    }
    const unfiled = h("section", { class: "card" }, h("div", { class: "hd" }, h("h2", {}, "Unfiled"), h("span", { class: "muted small" }, plural(chats.length, "chat"))));
    unfiled.append(h("div", { class: "bd flush" }, chats.length ? ul : h("div", { class: "pad" }, h("p", { class: "muted" }, ql ? "No chat matches." : "No saved chats yet. Start one with New chat."))));
    list.append(unfiled);
  };
  search.addEventListener("input", draw);
  const unsub = store.on((k) => { if (k === "chats" || k === "sessions") draw(); });
  draw();
  return () => unsub();
}

export { emptyState };
