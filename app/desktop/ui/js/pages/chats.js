// Chats: the app's own chat, saved chats, and sessions started by other AI
// apps. A chat is the panel; its session's map fills the window behind it,
// and its files open in the side sheet. Everything the model, the server
// or a file says is shown as text, never as markup.
import { $, el, h, icon, iconButton, menuButton, fitSelect, invoke, listen, store, button, redraw, placeHead, sec, when, ago, shortWhen, era, size, plural, timeLeft, clientName, toast, emptyState, askConfirm, askText, openSide, closeSide, openOutputs, closeOutputs, outputsOpen, setOutputsFull, setOutputsHead, insets, onLayout, savePrefs, numbered } from "../core.js";
import { openWebLayerEditor, onChange as onWebLayersChange } from "./maplayers.js";
import { setupState, notices, firstSteps } from "./start.js";

// Model and thinking names, once the chat settings are read.
const NAMES = new Map();
const nameOf = (id) => NAMES.get(id) || id;
const MODEL_NOTE = "The chat sends your messages and the results it reads to the AI service you chose (Settings › Assistant).";

// ---------- The live chat (one at a time, as before) ----------
const live = {
  // The project this chat works in (null: Unfiled). Set before it starts.
  project: null,
  // A workflow run whose results this chat opened ({workflow, run, name}), or null.
  run: null,
  started: false,
  busy: false,
  chatId: null,
  session: null,
  view: null, // persistent DOM
  log: null,
  renderer: null,
  cards: new Map(), // prompt id -> { row, card, title }
  map: null, // its session's map, on the stage
  shown: false, // whether the live chat is the page being looked at
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

/** The titles of the requests waiting in the app's own chat. */
export function waitingTitles() {
  return [...live.cards.values()].filter((c) => c.row.isConnected).map((c) => c.title);
}

/** Settings were saved while the sheet lay over the live chat: it takes the
 * new defaults (the model and thinking level a new chat starts with, and
 * whether an AI is connected). A chat's own choice stays. */
export function settingsSaved() {
  if (!live.shown || !live.view) return;
  updateChip();
  emptyChat();
}

/** On the start the question is drawn first, above what to try. It comes
 * first in the document then too, so Tab and a screen reader meet things in
 * the order they are seen; in a chat it comes last, under the conversation. */
export function questionFirst(first) {
  if (!live.view) return;
  const col = live.view.querySelector(".chatcol");
  const head = col && col.querySelector(".chathead");
  const composer = $("composer");
  if (!head || !composer || composer.parentElement !== col) return;
  // (The greeting stays right under the head; it shows on the start only.)
  const top = col.querySelector(":scope > .greeting") || head;
  if ((top.nextElementSibling === composer) === first) return;
  const held = composer.contains(document.activeElement) ? document.activeElement : null;
  const note = col.querySelector(":scope > .project-note");
  if (first) {
    top.after(composer);
    if (note) col.append(note);
  } else {
    col.append(composer);
    if (note) top.after(note);
  }
  // Moving it took the focus away: give it back.
  if (held) held.focus({ preventScroll: true });
}

/** Whether the app's own chat is the page on show. */
export function liveShown() {
  return !!live.shown;
}

/** Whether the page shown is a chat with nothing asked yet: the start. */
export function isStart() {
  return live.shown && !live.started && !live.run && !!live.log && !live.log.querySelector(".msg, .ask, .error, .status");
}


/** Recent chats and sessions, for the start and Ctrl+K. */
export function recent(n) {
  const out = [];
  for (const c of store.chats) {
    out.push({ kind: c.live ? "Chat (open)" : "Chat", title: c.title, sub: ago(c.updated), href: c.live ? "#/chats/new" : `#/chats/c/${encodeURIComponent(c.id)}`, t: c.updated || "",
      live: !!c.live, waiting: !!c.live && waitingCount() > 0 });
  }
  for (const s of otherSessions()) {
    out.push({ kind: clientName(s.client), title: `${clientName(s.client)} session`, sub: s.last_used ? `used ${ago(s.last_used)}` : "not used yet", href: `#/chats/s/${encodeURIComponent(s.session)}`, t: s.last_used || s.started || "" ,
      who: clientName(s.client), waiting: ((store.inbox.find((x) => x.session === s.session) || {}).pending || []).length > 0 });
  }
  out.sort((a, b) => String(b.t).localeCompare(String(a.t)));
  return out.slice(0, n);
}

// The app's own chat is known by the session id its start returned, never
// by the name a client gives itself (any client could call itself so).
function otherSessions() {
  return store.sessions.filter((s) => s.session !== live.session && s.session !== store.ownSession);
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
      // A heading that opens the text is its headline; any other is a
      // section's.
      const big = !box.childNodes.length && /^#{1,2}\s/.test(line);
      const hd = el(big ? "h3" : "h4", big ? "md-h" : "md-h small");
      inline(hd, head[1]);
      box.append(hd);
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
    case "map_snapshot": return `Looking at ${Array.isArray(i.layers) && i.layers.length > 1 ? `${i.layers.length} layers` : "a layer"} on a map`;
    case "propose_workflow": return "Proposing a workflow";
    case "export_output": return `Exporting${i.folder ? ` to ${q(i.folder)}` : ""}`;
    case "registry_search": return `Searching open data${i.query ? ` for ${q(i.query)}` : ""}`;
    case "registry_fetch": return "Downloading open data";
    case "context_search": return "Searching your organization's notes";
    case "context_get": return "Reading a note";
    case "draft_notes": return `Drafting notes${conn}`;
    case "update_notes": return "Changing your notes";
    case "suggest_note_changes": return "Suggesting a change to your notes";
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
    case "settings.draft_notes": return `Drafting notes from the app${conn}`;
    case "settings.reload": return "Settings changed: read again";
    case "window_layer": return "Showing a layer on the map";
    case "window_rows": return "Showing rows in the table";
    case "project_db": return "Using a project database";
    case "open_run": return "Opening a workflow run in a chat";
    case "email_report": return "Emailing a workflow's report";
    case "workflow_review": return "Reviewing a workflow's results";
    case "org_install_tool": return "Installing an organization tool";
    default: return `Running ${q(name)}`;
  }
}

/** Draws chat events into a log: live (cards can be answered) or a saved transcript. */
class Renderer {
  constructor(log, isLive, onProvenance, onTable = null) {
    this.log = log;
    this.live = isLive;
    this.current = null;
    this.group = null;
    this.said = null; // words folded into the steps by the step that followed
    this.made = null; // the last result this turn made
    this.steps = new Map();
    this.pastCards = new Map();
    this.onProvenance = onProvenance;
    this.onTable = onTable;
  }

  endTurn() { this.current = null; this.group = null; this.said = null; }

  /** The turn is over: the last thing it made is shown as a result. */
  finish() {
    const m = this.made;
    this.made = null;
    if (!m) return;
    const acts = h("div", { class: "acts" });
    if (this.onTable) acts.append(h("button", { type: "button", class: "btn sm result-table", onclick: () => this.onTable(m.handle) }, icon("table"), "Table"));
    acts.append(h("button", { type: "button", class: "btn sm result-made", title: "Inputs, steps and checks of this result", onclick: () => this.onProvenance && this.onProvenance(m.provenance) }, "How it was made"));
    const state = m.fails ? el("span", "bad", `${plural(m.fails, "check")} failed`) : m.warns ? el("span", "warn", `${m.warns} to review`) : "checks passed";
    this.log.append(h("div", { class: "result" }, h("span", { class: "swatch", "aria-hidden": "true" }),
      h("div", { class: "text" }, h("b", {}, m.title), h("span", { class: "where" }, m.summary, m.summary ? " · " : null, state)), acts));
  }

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
      const summary = h("summary", {}, h("span", { class: "state", "aria-hidden": "true" }), count, last, badges, icon("forward"));
      const ol = el("ol");
      details.append(summary, ol);
      this.log.append(details);
      this.group = { details, count, last, badges, ol, n: 0, fails: 0, warns: 0, running: 0, failed: 0 };
    }
    return this.group;
  }

  updateGroup(g) {
    g.count.textContent = plural(g.n, "step");
    g.details.classList.toggle("running", g.running > 0);
    g.details.classList.toggle("failed", g.failed > 0 || g.fails > 0);
    g.badges.replaceChildren();
    if (g.fails) g.badges.append(el("span", "badge fail", `${g.fails} failed`));
    if (g.warns) g.badges.append(el("span", "badge warn", `${g.warns} to review`));
  }

  event(e) {
    switch (e.kind) {
      case "you":
        this.endTurn();
        this.made = null;
        this.log.append(el("div", "msg you", e.text));
        break;
      case "text":
        this.said = null;
        markdown(this.assistant(), e.text);
        break;
      case "tool_started": {
        const g = this.stepGroup();
        // Words said on the way to a step are kept with the steps, so the
        // turn reads as one line of work and then the answer.
        if (this.current) {
          const say = h("li", { class: "say" }, this.current);
          g.ol.append(say);
          this.said = { el: this.current, li: say };
        } else this.said = null;
        this.current = null;
        const li = h("li", { class: "step running" }, el("span", "what", describe(e.name, e.input)));
        const raw = h("details", {}, h("summary", {}, "Details"), el("pre", "box", `${e.name} ${JSON.stringify(e.input ?? {}, null, 2)}`));
        li.append(raw);
        g.ol.append(li);
        g.n += 1;
        g.running += 1;
        g.last.textContent = `· ${describe(e.name, e.input)}…`;
        this.updateGroup(g);
        this.steps.set(e.id, { li, g, raw, input: e.input });
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
        this.said = null;
        if (!e.ok) g.failed += 1;
        li.classList.remove("running");
        const result = el("span", e.ok ? null : "bad", e.ok ? e.summary : `Refused or failed: ${e.summary}`);
        li.insertBefore(result, s.raw || null);
        // What the AI looked at, shown to the person too (a PNG only).
        if (e.ok && typeof e.image === "string" && /^data:image\/png;base64,[A-Za-z0-9+/=]+$/.test(e.image)) {
          const img = h("img", { class: "snapshot", alt: "The map picture the assistant looked at" });
          img.src = e.image;
          li.insertBefore(h("figure", { class: "snapshot" }, img, h("figcaption", { class: "small muted" }, "What the assistant saw (geometry only, no basemap)")), s.raw || null);
        }
        if (e.fails) li.insertBefore(el("span", "bad", `${e.fails} check${e.fails > 1 ? "s" : ""} failed`), s.raw || null);
        if (e.warns) li.insertBefore(el("span", "warn", `${e.warns} to review`), s.raw || null);
        if (e.ok && e.provenance) {
          // Whose rows its Table shows: the layer it made, or (an export,
          // which makes files) the layer it was given.
          const given = s.input && typeof s.input.handle === "string" ? s.input.handle : null;
          this.made = { title: resultName(e, s.input), summary: e.summary, provenance: e.provenance, handle: e.handle || given, fails: e.fails || 0, warns: e.warns || 0 };
          if (this.onResult) this.onResult(this.made);
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
        // What the assistant said about the step that is asking stays in
        // sight, above the request.
        if (this.said) { this.log.append(this.said.el); this.said.li.remove(); }
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
          c.out.textContent = { allow_once: "Allowed once.", always_this_session: "Allowed for this chat.", decline: "Declined." }[e.decision] || "Answered.";
        }
        break;
      }
      case "status":
        this.endTurn();
        this.log.append(el("div", "status", e.text));
        break;
      case "error":
        this.endTurn();
        this.made = null;
        this.log.append(el("div", "error", e.text === "stopped" ? "Stopped." : `Could not finish: ${e.text}`));
        break;
      case "done":
        this.endTurn();
        this.finish();
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
    card.append(el("h3", null, askTitle(e.title)), el("p", "body", e.summary));
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

// A result's name: the dataset it came from, or the file it made.
function resultName(e, input) {
  const i = input || {};
  if (typeof i.name === "string" && i.name) return i.name;
  if (typeof i.dataset === "string" && i.dataset) return i.dataset;
  // A processing step is named for what it did: "Clip", "Buffer".
  if (typeof i.op === "string" && i.op) return `${i.op[0].toUpperCase()}${i.op.slice(1).replace(/_/g, " ")}`;
  const stem = String(e.handle || "").split("/").pop().replace(/\.[A-Za-z0-9]+$/, "").replace(/-p?_?[0-9a-f]{6,}$/, "");
  return stem || describe(e.name, i);
}

// The server's title for a request, without its own name in front (the
// card already says who is asking).
function askTitle(t) {
  const s = String(t || "").replace(/^Columbus:\s*/i, "");
  return s ? s[0].toUpperCase() + s.slice(1) : "A request";
}

// An approval prompt from the chat's own session, answered here as the
// chat window always did: buttons wait a second, scripts are once only
// unless the server offers "Allow this script for the session" (policy).
function liveCard(r, e) {
  const card = el("div", "ask");
  card.setAttribute("role", "alertdialog");
  card.setAttribute("aria-label", e.title);
  card.append(h("p", { class: "who" }, h("span", { class: "dot warn" }), "Columbus needs your OK"), el("h3", null, askTitle(e.title)));
  // The server's own words.
  card.append(el("p", "body", e.summary));
  if (e.code != null) {
    // Written by the AI: in its own box, never mixed with the server's words.
    const lines = e.code.replace(/\r?\n$/, "").split(/\r?\n/);
    const n = lines.length;
    const beside = iconButton("popout", "Read the code beside the chat", () => {
      const { pre: big } = numbered(e.code);
      openSide("The script", big, [h("span", { class: "muted small" }, `Python · ${plural(n, "line")} · written by the AI`)]);
    }, { class: "iconbtn read-beside" });
    card.append(el("p", "code-label", `Code written by the AI (${n} line${n === 1 ? "" : "s"}). Read it before allowing:`));
    const pre = el("pre", "code");
    lines.forEach((line) => pre.append(el("span", "ln", line === "" ? " " : line)));
    card.append(h("div", { class: "codewrap" }, pre, beside));
  }
  const row = el("div", "row");
  const note = el("p", "note", "Either way, it is written to Activity.");
  const answer = async (decision, label) => {
    for (const b of row.querySelectorAll("button")) b.disabled = true;
    try {
      await invoke("chat_answer", { id: e.id, decision });
      row.replaceWith(el("p", "done", label));
    } catch (err) {
      row.replaceWith(el("p", "error", String(err)));
    }
    note.remove();
    // Decided: no longer the loud thing in the panel.
    card.classList.add("answered");
    store.set("waiting", waitingCount());
    drawTray();
  };
  const buttons = [
    ["Allow once", "allow_once", "primary", "Allowed once."],
    ...(e.offer_always
      ? [e.code != null
          ? ["Allow this script for this chat", "always_this_session", "", "Allowed this script for this chat."]
          : ["Allow for this chat", "always_this_session", "", "Allowed for this chat."]]
      : []),
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
  card.append(row, note);
  r.log.append(card);
  live.cards.set(e.id, { row, card, title: askTitle(e.title) });
  store.set("waiting", waitingCount());
  drawTray();
  // In front of the person: the panel comes back if it was put away, and a
  // word is said when this chat is not what they are looking at.
  if (document.body.classList.contains("out-full")) {
    // The chat was put away so the map had the window: it comes back, and
    // the focus goes to the request, so Tab reaches its buttons.
    setOutputsFull(false);
    if (live.map) live.map.head();
    card.tabIndex = -1;
    setTimeout(() => { if (card.isConnected && live.shown && !document.querySelector("dialog[open]")) card.focus({ preventScroll: true }); }, 0);
  }
  if (!live.shown || document.querySelector("dialog[open]")) toast(`Your chat is asking: ${e.title}`);
  // Show the card from its title, not its end.
  card.scrollIntoView({ block: "start" });
}

function gone(id) {
  const c = live.cards.get(id);
  live.cards.delete(id);
  if (c && c.row.isConnected) c.row.replaceWith(el("p", "done", "No longer waiting: the request was withdrawn or timed out."));
  if (c) { c.card.classList.add("answered"); c.card.querySelector(".note")?.remove(); }
  store.set("waiting", waitingCount());
  drawTray();
}


// ---------- A session's map, on the stage ----------

const APP_FRAME = "allow-scripts allow-same-origin";
let onStage = null; // the map shown now (one at a time)

/** One session's own page (its map, results and table) in a sandboxed
 * frame in the panel on the right; and the files the session made, to read
 * there too. */
class SessionMap {
  constructor() {
    this.session = null;
    // Whether the panel on the right is open for this map (the person, or
    // a first result, opened it), and whether the person closed it.
    this.open = false;
    this.dismissed = false;
    this.onOpenChange = null;
    this.frame = null;
    this.origin = null;
    this.loading = false;
    this.view = null; // "table" once the person asked for the table
    this.output = null; // whose table: a result's handle, when a card asked
    this.ready = false; // the page said it listens
    this.host = h("div", { class: "frame-host", hidden: true });
    $("stage-frames").append(this.host);
    this.onMessage = (ev) => {
      if (!this.frame || ev.source !== this.frame.contentWindow || ev.origin !== this.origin) return;
      if (ev.data && ev.data.type === "columbus-embed-ready") { this.ready = true; this.post(); }
      // The map's Add web layer: the app's own dialog (the page gets nothing
      // but word that the list changed).
      if (ev.data && ev.data.type === "columbus-add-web-layer") openWebLayerEditor(null);
    };
    window.addEventListener("message", this.onMessage);
    this.unsub = store.on((k) => { if (k === "prefs") this.post(); });
    this.unsubLayers = onWebLayersChange(() => {
      if (this.frame && this.origin && this.frame.contentWindow) this.frame.contentWindow.postMessage({ type: "columbus-web-layers" }, this.origin);
    });
    // The panel moved or a sheet opened: the page keeps its controls clear.
    this.unsubLayout = onLayout(() => this.post());
  }

  destroy() {
    window.removeEventListener("message", this.onMessage);
    this.unsub();
    this.unsubLayers();
    this.unsubLayout();
    if (onStage === this) onStage = null;
    this.host.remove();
    this.frame = null;
  }

  /** Put this map on the stage in place of the one there, or take it off. */
  show(on = true) {
    if (!on) {
      this.host.hidden = true;
      if (onStage === this) onStage = null;
      return;
    }
    if (onStage && onStage !== this) onStage.host.hidden = true;
    onStage = this;
    this.host.hidden = false;
    this.head();
    // The panel is as this chat left it: open with its map, or closed.
    if (this.open) this.unfold();
    else closeOutputs();
    if (this.onOpenChange) this.onOpenChange();
  }

  /** The panel's head while this map has the stage. */
  head() {
    setOutputsHead("Map and table", [
      iconButton("files", "Files this chat made", () => this.files(), { id: "out-files" }),
      iconButton("popout", "Open the map in its own window", () => this.popOut(), { id: "out-popout" }),
    ], () => this.setOpen(false));
  }

  // Open the panel with this map in it (the frame is made only now: a map
  // drawn out of sight would have no size to draw to).
  unfold() {
    this.head();
    openOutputs();
    this.ensureFrame();
    this.post();
  }

  /** Open or close the panel for this map: the person asked. */
  setOpen(open) {
    this.open = !!open;
    this.dismissed = !open;
    if (onStage === this) {
      if (this.open) this.unfold();
      else closeOutputs();
    }
    if (this.onOpenChange) this.onOpenChange();
  }

  /** Something was made: the panel opens to show it, unless the person
   * closed it in this chat. */
  reveal() {
    if (this.open || this.dismissed) return;
    this.open = true;
    if (onStage === this) this.unfold();
    if (this.onOpenChange) this.onOpenChange();
  }

  /** As new: for a new chat. */
  reset() {
    this.open = false;
    this.dismissed = false;
    this.view = null;
    this.output = null;
    if (onStage === this) closeOutputs();
    if (this.onOpenChange) this.onOpenChange();
  }

  // Plain values only: how the page should look, and which part of it the
  // panel covers.
  post() {
    if (!this.frame || !this.origin || !this.frame.contentWindow) return;
    const msg = { type: "columbus-embed", basemap: !!store.prefs.basemap, theme: store.prefs.theme || "system", inset: insets() };
    // Asked once, when the page listens: after that the page keeps the
    // table as the person leaves it.
    if (this.view && this.ready) {
      msg.view = this.view;
      if (this.output) msg.output = this.output;
      this.view = null;
      this.output = null;
    }
    this.frame.contentWindow.postMessage(msg, this.origin);
  }

  /** Open the table sheet on the map: of the result named (a plain
   * workspace handle the page checks against its own list), else of the
   * result the page has active. The panel opens for it. */
  table(handle) {
    if (!this.session) return toast("The map is not open yet.");
    this.view = "table";
    this.output = typeof handle === "string" && /^[A-Za-z0-9_\-./:]{1,200}$/.test(handle) ? handle : null;
    this.open = true;
    this.dismissed = false;
    if (onStage === this) this.unfold();
    if (this.onOpenChange) this.onOpenChange();
  }

  setSession(session) {
    if (session === this.session) return;
    this.session = session;
    this.frame = null;
    this.origin = null;
    this.ready = false;
    this.host.replaceChildren();
    if (!this.host.hidden && outputsOpen()) this.ensureFrame();
  }

  /** A few words on the stage in place of a map. */
  say(title, text, ...actions) {
    this.host.replaceChildren(h("div", { class: "state" }, h("h3", {}, title), h("p", {}, text),
      actions.length ? h("div", { class: "row wrap" }, ...actions) : null));
  }

  async popOut() {
    const s = store.sessions.find((x) => x.session === this.session);
    if (!s) return toast("The session is not running.");
    try { await invoke("open_session", { port: s.port }); } catch (e) { toast(String(e)); }
  }

  async ensureFrame() {
    if (this.frame || this.loading) return;
    // No session yet: the chart paper shows.
    if (!this.session) return;
    this.loading = true;
    const want = this.session;
    this.host.replaceChildren(h("div", { class: "state", role: "status" }, h("p", {}, "Opening the session's map…")));
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
      this.ready = false;
      this.host.replaceChildren(f);
    } catch (e) {
      if (want !== this.session) return;
      // A session that has gone has no map to show; anything else can be tried again.
      if (/no longer running|not running|has ended/i.test(String(e))) this.say("Its session has ended", "The map and table show a running session only. The files it made stay in its workspace.");
      else {
        this.say("The map could not be shown here", String(e),
          button("Try again", "sm", () => { this.frame = null; this.ensureFrame(); }),
          button("Open in its own window", "sm", () => this.popOut()));
      }
    } finally {
      this.loading = false;
    }
  }

  /** The files the session made, in the side sheet; with `prov`, that
   * result's provenance above them. */
  async files(prov = null) {
    const box = h("div", { class: "stack outbody", id: "session-files" });
    openSide(prov ? "How it was made" : "Files", box);
    if (!this.session) {
      box.append(h("div", { class: "state" }, h("h3", {}, "No files yet"), h("p", {}, "Files the session makes are listed here.")));
      return;
    }
    if (prov) {
      const card = h("section", { class: "card" }, h("div", { class: "hd" }, h("h3", {}, "Provenance"), h("span", { class: "chip mono" }, prov)));
      const bd = h("div", { class: "bd" }, el("p", "muted", "Reading…"));
      card.append(bd);
      box.append(card);
      try {
        const p = await invoke("session_provenance", { session: this.session, id: prov });
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
      box.append(h("table", { class: "t" }, h("thead", {}, h("tr", {}, h("th", {}, "File"), h("th", { class: "num" }, "Size"), h("th", {}, "Made"))), tbody));
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

// What every chat's head offers for its map: a button that opens or closes
// the panel on the right, and behind "more" the session's files, the map
// filling the window, and the map in its own window.
function mapMenu(map, more = []) {
  return menuButton("More", () => [
    { label: "Files this chat made", cls: "chat-files", onclick: () => map.files() },
    { label: "Fill the window with the map", cls: "chat-away", onclick: () => { map.setOpen(true); setOutputsFull(true); map.head(); } },
    { label: "Open the map in its own window", cls: "chat-popout", onclick: () => map.popOut() },
    ...(typeof more === "function" ? more() : more),
  ], { class: "iconbtn chat-more" });
}
function mapButton(map) {
  const b = h("button", { type: "button", class: "btn sm chat-map", "aria-pressed": "false", title: "This chat's map and table, on the right" }, icon("map"), "Map");
  b.addEventListener("click", () => map.setOpen(!map.open));
  map.onOpenChange = () => b.setAttribute("aria-pressed", map.open ? "true" : "false");
  return b;
}

// ---------- The waiting tray ----------
let trayEl = null;
let trayFor = null; // "live" or a session id

function drawTray() {
  if (!trayEl) return;
  trayEl.replaceChildren();
  let items = [];
  if (trayFor === "live") {
    // A request the person can see needs no second mention: the tray
    // names only those scrolled out of sight.
    const box = live.log.getBoundingClientRect();
    const inSight = (c) => { const r = c.row.getBoundingClientRect(); return r.bottom > box.top && r.top < box.bottom; };
    for (const [id, c] of live.cards) if (c.row.isConnected && !inSight(c)) items.push({ title: c.title, go: () => c.card.scrollIntoView({ block: "start" }), id });
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

// The live chat's renderer: a result's "How it was made" and Table open in
// the panel on the right, and the first thing a chat makes opens the panel.
function liveRenderer(log) {
  const r = new Renderer(log, true, (id) => live.map.files(id), (handle) => live.map.table(handle));
  r.onResult = () => live.map.reveal();
  return r;
}

// ---------- The live chat view (built once, kept while the app runs) ----------
function buildLive() {
  const view = h("div", { class: "chatview", id: "chatview" });
  const title = h("h1", { id: "chat-title" }, "New chat");
  live.map = new SessionMap();
  const head = h("header", { class: "chathead" },
    h("div", { class: "titlebox" }, title),
    h("div", { class: "actions" }, mapButton(live.map),
      // (A new chat in the same project as this one is under "more".)
      mapMenu(live.map, () => [{ label: live.project ? "New chat in this project" : "New chat", id: "new", onclick: () => newChat(live.project) }])));
  // The start's greeting (shown only while nothing is said yet).
  const greeting = h("p", { class: "greeting", id: "greeting" }, "What do you want to know?");
  const tray = h("section", { class: "tray", "aria-label": "Waiting for you", hidden: true });
  const log = h("div", { id: "log", role: "log", "aria-live": "polite", "aria-label": "Conversation" });
  const text = h("textarea", { id: "text", rows: "1", placeholder: "Ask about your data" });
  const status = h("span", { id: "status", role: "status" });
  const stop = h("button", { type: "button", id: "stop", class: "btn sm", hidden: true }, "Stop");
  const send = h("button", { type: "submit", id: "send", class: "sendbtn", "aria-label": "Send", title: "Send (Enter)" }, icon("send"));
  // Where this chat works (its project, a run it opened), under the box.
  const sub = h("span", { class: "sub", id: "chat-sub" });
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
  modelSel.addEventListener("change", () => { fitSelect(modelSel); onPick(); });
  effortSel.addEventListener("change", () => { fitSelect(effortSel); onPick(); });
  // The message box with its send button; under it, quietly: where the
  // chat works, and the model it uses.
  const composer = h("form", { id: "composer" },
    h("label", { for: "text", class: "visually-hidden" }, "Message"),
    h("div", { class: "box" }, text, stop, send),
    h("div", { class: "under", id: "chat-foot" }, sub, status, h("span", { class: "spacer" }), modelSel, effortSel));
  const projectBox = h("details", { class: "project-note", id: "chat-project", hidden: true });
  const col = h("section", { class: "chatcol", "aria-label": "Chat" }, head, greeting, projectBox, tray, log, composer);
  view.append(col);
  live.view = view;
  live.log = log;
  live.renderer = liveRenderer(log);

  composer.addEventListener("submit", async (ev) => {
    ev.preventDefault();
    const t = text.value.trim();
    if (!t || live.busy) return;
    if (!(await start())) return;
    log.querySelector(".chat-empty")?.remove();
    live.renderer.endTurn();
    log.append(el("div", "msg you", t));
    text.value = "";
    fitText();
    log.scrollTop = log.scrollHeight;
    setBusy(true);
    // Send gave way to Stop: the focus stays in the message box, for the
    // next thing the person wants to say (unless they have moved on to
    // something else while the chat started).
    const at = document.activeElement;
    if (!at || at === document.body || composer.contains(at)) text.focus({ preventScroll: true });
    store.set("chatstate", "asked");
    try {
      await invoke("chat_send", { text: t });
      if (!title.dataset.set) { title.textContent = t.length > 70 ? `${t.slice(0, 70)}…` : t; title.title = t; title.dataset.set = "1"; }
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
  text.addEventListener("input", fitText);
  stop.addEventListener("click", () => invoke("chat_stop"));
  // A waiting request scrolled out of sight is named above the conversation.
  let trayQueued = false;
  log.addEventListener("scroll", () => {
    if (trayQueued || !live.cards.size) return;
    trayQueued = true;
    requestAnimationFrame(() => { trayQueued = false; if (trayFor === "live") drawTray(); });
  });
  // The project's name and instructions, once projects are loaded or change.
  store.on((k) => { if (k === "projects" || k === "info") drawSub(); });
  return view;
}

// The message box is as tall as what is typed (up to a limit), and the
// send button is quiet while there is nothing to send.
function fitText() {
  const t = $("text");
  if (!t) return;
  t.style.height = "auto";
  if (t.value) t.style.height = `${Math.min(t.scrollHeight, 180)}px`;
  $("composer").classList.toggle("no-text", !t.value.trim());
}

/** Put words in the message box for the person to send. */
function setText(v) {
  const t = $("text");
  t.value = v;
  fitText();
  t.focus();
}

function setBusy(b) {
  live.busy = b;
  $("send").disabled = b;
  // While it works, Stop stands where Send was.
  $("send").hidden = b;
  $("stop").hidden = !b;
  $("chat-model").disabled = b;
  $("chat-effort").disabled = b;
  $("status").textContent = b ? "Working…" : "";
  $("composer").classList.toggle("busy", b);
}

async function start() {
  if (live.started) return true;
  $("status").textContent = "Starting a Columbus session…";
  try {
    // What the two boxes under the message show is what this chat runs on:
    // it is sent with the start and kept as the chat's own from then on. A
    // later change of the default in Settings is for new chats.
    const boxes = { model: $("chat-model").value, effort: $("chat-effort").value };
    if (!live.pick && boxes.model) live.pick = boxes;
    const s = await invoke("chat_start", { ...(live.pick || {}), project: live.project, run: live.run ? { workflow: live.run.workflow, run: live.run.run } : null });
    live.started = true;
    live.session = s.session || null;
    live.chatId = s.chat || null;
    live.map.setSession(live.session);
    if (live.chatId) history.replaceState(null, "", `#/chats/c/${encodeURIComponent(live.chatId)}`);
    $("status").textContent = "";
    $("text").placeholder = "Ask a follow-up";
    drawSub();
    store.set("chatstate", "started");
    refreshFns && (refreshFns.chats(), refreshFns.sessions());
    return true;
  } catch (err) {
    $("status").textContent = "";
    live.log.append(el("div", "error", String(err)));
    if (/API key|server was not found/i.test(String(err))) {
      live.log.append(h("p", { class: "small" }, h("a", { href: "#/settings/ai" }, "Set up the AI in Settings › Assistant")));
    }
    store.set("chatstate", "failed");
    return false;
  }
}

/** A project's name, or "Unfiled". */
export function projectName(id) {
  if (!id) return "Unfiled";
  const p = (store.projects?.projects || []).find((x) => x.id === id);
  return p ? p.name : id;
}

function drawSub() {
  const sub = $("chat-sub");
  if (!sub) return;
  const projects = store.projects?.projects || [];
  if (!live.started && !live.run) {
    // Before the first message, the project can still be chosen.
    const pick = h("select", { id: "chat-project-pick", class: "pick", "aria-label": "Project for this chat", title: "A project gives a chat its instructions and narrows the connections it may use." },
      h("option", { value: "" }, "No project"), ...projects.map((p) => h("option", { value: p.id }, p.name)), h("option", { value: "+" }, "Projects…"));
    pick.value = live.project && projects.some((p) => p.id === live.project) ? live.project : "";
    pick.addEventListener("change", () => {
      if (pick.value === "+") { pick.value = live.project || ""; location.hash = "#/projects"; return; }
      live.project = pick.value || null;
      drawSub();
    });
    sub.replaceChildren(pick);
    fitSelect(pick);
  } else {
    // Under the message box: the project this chat works in; a chat opened
    // from a workflow run says which, with a way back to its report.
    const project = live.project
      ? h("a", { href: `#/projects/${encodeURIComponent(live.project)}`, title: "This chat's project: its instructions and the connections it may use" }, projectName(live.project))
      : live.run ? null : h("a", { href: "#/projects", title: "Not in a project. Projects give a chat instructions and narrow the connections it may use." }, "No project");
    const run = live.run ? h("a", { id: "chat-run", href: `#/workflows/${encodeURIComponent(live.run.workflow)}/${encodeURIComponent(live.run.run)}`, title: "The run this chat opened" }, `${live.run.name || live.run.workflow} run`) : null;
    sub.replaceChildren(...[project, project && run ? h("span", { class: "sep" }) : null, run].filter(Boolean));
  }
  // What the assistant is following, in plain sight.
  const box = $("chat-project");
  if (!box) return;
  const p = live.project && (store.projects?.projects || []).find((x) => x.id === live.project);
  box.hidden = !(p && p.instructions);
  if (p && p.instructions) {
    box.replaceChildren(h("summary", {}, "This project's instructions to your assistant"),
      h("p", { class: "small pre-wrap" }, p.instructions),
      h("p", { class: "small muted" }, "Not what you wrote? Edit them on the ", h("a", { href: `#/projects/${encodeURIComponent(p.id)}` }, "project's page"), "."));
  }
}

async function newChat(project = null, run = null) {
  const was = live.chatId;
  // What the old chat had open beside the panel goes with it.
  closeSide();
  live.project = project || null;
  live.run = run || null;
  await invoke("chat_end");
  live.started = false;
  live.busy = false;
  live.chatId = null;
  live.session = null;
  live.pick = null;
  live.cards.clear();
  live.log.replaceChildren();
  live.renderer = liveRenderer(live.log);
  $("chat-title").textContent = "New chat";
  $("chat-title").removeAttribute("title");
  delete $("chat-title").dataset.set;
  $("text").placeholder = "Ask about your data";
  $("text").value = "";
  fitText();
  drawSub();
  live.map.reset();
  live.map.setSession(null);
  setBusy(false);
  await updateChip();
  store.set("waiting", 0);
  drawTray();
  // Only if the person is still looking at this chat.
  if (location.hash === "#/chats/new" || (was && location.hash === `#/chats/c/${encodeURIComponent(was)}`)) history.replaceState(null, "", "#/chats/new");
  await emptyChat();
  store.set("chatstate", "new");
  refreshFns && refreshFns.chats();
  $("text").focus();
}

// The start of a chat: what to try, what is recent, what waits, and the
// first steps when nothing is set up. Drawn afresh each time it is shown.
let emptySeq = 0;
async function emptyChat() {
  const old = live.log.querySelector(".chat-empty");
  if (live.log.childNodes.length && !(old && live.log.childNodes.length === 1)) return;
  const seq = ++emptySeq;
  const box = h("div", { class: "chat-empty" }, h("h2", { class: "visually-hidden" }, "Ask about your data"));
  if (old) old.replaceWith(box); else live.log.append(box);
  const s = await setupState();
  if (seq !== emptySeq || !box.isConnected) return;
  box.append(...notices(s.st));
  const c = s.chat;
  if (c && c.has_key && c.server_found && (c.server_older || !c.engine)) {
    box.append(h("div", { class: "notice warn" }, h("div", {},
      c.server_older ? `The Columbus server this chat uses is version ${c.server_version}, older than this app (${c.app_version}), so the map and newer tools may not work. ` : "The GIS engine was not found, so tools that read data will fail. ",
      h("a", { href: "#/settings/ai" }, "See Settings › Assistant"), ".")));
  }
  // Nothing set up yet: the first run. With no AI connected there is no
  // question to ask, so it stands in for the message box; with one, it sits
  // under it.
  document.body.classList.toggle("setup", !!s.needsSetup && !s.done.ai);
  if (s.needsSetup) {
    box.append(firstSteps(s, () => emptyChat()));
    return;
  }
  if (c && (!c.has_key || !c.server_found)) {
    box.append(h("div", { class: "notice warn" }, h("div", {},
      !c.has_key ? "No AI is connected yet. " : "Columbus's server was not found. ",
      h("a", { href: "#/settings/ai" }, "Set it up in Settings › Assistant"), ".")));
  }
  if (document.activeElement === document.body) $("text")?.focus();
  const ids = s.st ? s.st.connections.filter((x) => x.id).map((x) => x.id) : [];
  const prompts = ids.length
    ? [`Which datasets can I use on ${ids[0]}, and what CRS are they in?`,
      `Describe the largest dataset on ${ids[0]}: fields, codes and row count.`,
      ids[1] ? `Which datasets on ${ids[0]} and ${ids[1]} share a key field?` : `Check the datasets on ${ids[0]} for empty or invalid geometries.`]
    : ["Which connections and datasets can you use?", "What can you do with my GIS data through Columbus?"];
  const starters = h("div", { class: "starters" });
  for (const p of prompts) starters.append(h("button", { type: "button", onclick: () => setText(p) }, h("span", {}, p), icon("out")));
  box.append(sec("Try"), starters);
  if (!s.needsSetup && s.anyLeft) {
    box.append(h("p", { class: "small muted" }, "Setup steps you skipped are in Settings. ",
      h("button", { type: "button", class: "linkbtn", onclick: async () => { await savePrefs({ skipped: [], setup_done: false }); emptyChat(); } }, "Show the first steps again")));
  }
  // What waits for the person, and workflows whose last run needs them.
  const waitLine = h("div", { class: "waitlines" });
  box.append(waitLine);
  const drawWait = () => {
    const n = otherInbox().reduce((k, x) => k + (x.pending || []).length, 0) + (store.tools ? store.tools.waiting.length : 0) + (store.workflows ? store.workflows.waiting.length : 0);
    const w = store.workflows;
    const runs = w ? w.installed.filter((x) => x.last_run && ["failed", "needs_you", "needs_reapproval", "stopped_at_limit"].includes(x.last_run.status)).length : 0;
    waitLine.replaceChildren(...[
      n ? h("a", { class: "waitline", href: "#/activity", id: "start-waiting" }, h("span", { class: "dot warn" }), h("span", {}, `${plural(n, "thing")} ${n === 1 ? "is" : "are"} waiting for you`), icon("forward")) : null,
      runs ? h("a", { class: "waitline", href: "#/workflows", id: "start-runs" }, h("span", { class: "dot bad" }), h("span", {}, `${plural(runs, "workflow")} ${runs === 1 ? "needs" : "need"} you after ${runs === 1 ? "its" : "their"} last run`), icon("forward")) : null,
    ].filter(Boolean));
  };
  drawWait();
  box.drawWait = drawWait;
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
    fill($("chat-effort"), s.efforts, pick.effort, (id, l) => id ? `Thinking: ${l.replace(/ \(.*\)$/, "").toLowerCase()}` : "Thinking: default");
    const rows = store.info?.policy?.in_force?.return_rows;
    $("chat-model").title = `${MODEL_NOTE} ${(s.models.find(([id]) => id === pick.model) || [0, ""])[1]}${rows ? ` It sees at most ${Number(rows).toLocaleString()} rows per call (your organization's policy).` : ""}`;
    $("chat-effort").title = "How much the model thinks before it answers. Higher is slower, costs more and is more thorough.";
    fitSelect($("chat-model"));
    fitSelect($("chat-effort"));
    drawSub();
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

// What waits changed while the start is shown: its line follows.
store.on((k) => {
  if (k !== "inbox" && k !== "tools" && k !== "workflows") return;
  const box = live.log && live.log.querySelector(".chat-empty");
  if (box && box.drawWait) box.drawWait();
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
  // "fresh": a New chat button anywhere. It always starts a new chat; "new"
  // is the app's current chat (where "Answer in the chat" goes).
  // "fresh/<project>": a new chat in that project.
  if (kind === "fresh") {
    const project = id || null;
    history.replaceState(null, "", "#/chats/new");
    const r = await showLive(main);
    if (live.started || live.chatId || [...live.log.children].some((x) => !x.classList.contains("chat-empty")) || live.project !== project) await newChat(project);
    // A question another page left for the new chat (the person sends it).
    try {
      // {text, at}: only one left in the last minute, for this new chat.
      const raw = sessionStorage.getItem("columbus.prefill");
      sessionStorage.removeItem("columbus.prefill");
      const pre = raw ? JSON.parse(raw) : null;
      if (pre && typeof pre.text === "string" && Date.now() - Number(pre.at) < 60000) setText(pre.text);
    } catch { /* none */ }
    return r;
  }
  // "run/<workflow>/<run>": a new chat with that run's results (REQ-27):
  // its layers on the map and in the table, its report in the assistant's
  // context. The session starts at once, so they show before any message.
  if (kind === "run" && id && rest[2]) {
    const run = { workflow: id, run: rest[2], name: rest[3] || id };
    history.replaceState(null, "", "#/chats/new");
    const r = await showLive(main);
    await newChat(null, run);
    live.log.querySelector(".chat-empty")?.remove();
    live.log.append(h("div", { class: "notice", id: "chat-run-note" }, h("div", {},
      `Opened the results of ${run.name}'s run. Its layers are on the map and in the table, and your assistant has its report. `,
      "They are copies: the run itself, its files and its exports stay as they are. Ask, for example: ",
      h("i", {}, "check these three areas"), " or ", h("i", {}, "remove route X, then export the rest"), ".")));
    store.set("chatstate", "run");
    // Its layers on the map and in the table, in the panel on the right, as
    // soon as the session's page opens.
    live.map.view = "table";
    live.map.setOpen(true);
    if (await start()) {
      $("chat-title").textContent = `Review: ${run.name}`;
      $("chat-title").dataset.set = "1";
    }
    return r;
  }
  if (kind === "new" || (kind === "c" && id && id === live.chatId)) return showLive(main);
  if (kind === "c" && id) return showSaved(main, id);
  if (kind === "s" && id) return showSession(main, id);
  return showIndex(main);
}

/** The live chat leaves the panel now. (The page shown next may have been
 * asked for while the live chat's own page was still being drawn, before
 * its clean-up was handed back.) */
export function leaveLive() {
  if (live.map) live.map.show(false);
  if (!host || host.hidden) return;
  host.hidden = true;
  live.shown = false;
  if (trayFor === "live") trayEl = null;
}

let liveShows = 0;
function showLive(main) {
  const mine = ++liveShows;
  const hostEl = liveHost();
  if (!hostEl.isConnected) main.parentElement.append(hostEl);
  main.hidden = true;
  hostEl.hidden = false;
  live.shown = true;
  trayEl = live.view.querySelector(".tray");
  trayFor = "live";
  drawTray();
  updateChip();
  emptyChat();
  fitText();
  live.map.setSession(live.session);
  live.map.show();
  setTimeout(() => $("text")?.focus(), 0);
  return () => {
    // Shown again since (a page drawn late is cleaned up late): it is not
    // this showing's to hide.
    if (mine !== liveShows) return;
    live.map.show(false);
    hostEl.hidden = true;
    main.hidden = false;
    live.shown = false;
    // (Only its own: a session's page shown since has its own.)
    if (trayFor === "live") trayEl = null;
  };
}

// Leaving a saved chat or another app's session: its map goes with it.
function backToLiveMap(map) {
  map.destroy();
}

async function showSaved(main, id) {
  const view = h("div", { class: "chatview" });
  const map = new SessionMap();
  const title = h("h1", {}, "Chat");
  const sub = h("p", { class: "sub" }, "");
  const del = async () => {
    if (!(await askConfirm("Delete this chat?", "Its saved conversation is deleted from this computer. Files its session made stay where they are.", "Delete", true))) return;
    try { await invoke("chat_delete", { id }); refreshFns.chats(); location.hash = "#/chats"; } catch (e) { toast(String(e)); }
  };
  const rename = async () => {
    const t = await askText("Rename this chat", "Title", title.textContent);
    if (!t) return;
    try { await invoke("chat_rename", { id, title: t }); title.textContent = t; refreshFns.chats(); } catch (e) { toast(String(e)); }
  };
  const head = h("header", { class: "chathead" },
    h("div", { class: "titlebox" }, title, sub),
    h("div", { class: "actions" }, mapButton(map), mapMenu(map, [{ label: "Rename", id: "chat-rename", onclick: rename }, { label: "Delete this chat", id: "chat-delete", danger: true, onclick: del }])));
  const ended = h("div", { class: "ended" }, h("span", {}, "This chat has ended. Its conversation is kept here; Columbus cannot continue it."), button("New chat", "sm primary", () => { location.hash = "#/chats/fresh"; }));
  const logEl = h("div", { id: "saved-log", class: "log", role: "log", "aria-label": "Saved conversation" });
  const col = h("section", { class: "chatcol", "aria-label": "Saved chat" }, head, logEl);
  view.append(col);
  main.append(view);
  map.show();
  try {
    const lines = await invoke("chat_transcript", { id });
    const meta = lines.find((l) => l.kind === "meta") || {};
    const s = store.chats.find((c) => c.id === id);
    title.textContent = s ? s.title : "Chat";
    title.title = title.textContent;
    sub.replaceChildren(...(meta.project ? [h("a", { href: `#/projects/${encodeURIComponent(meta.project)}` }, projectName(meta.project)), " · "] : []), when(lines[0]?.ts));
    const r = new Renderer(logEl, false, (pid) => map.files(pid), (handle) => map.table(handle));
    for (const l of lines) r.event(l);
    r.finish();
    logEl.append(ended);
    const running = meta.session && store.sessions.some((x) => x.session === meta.session);
    map.setSession(running ? meta.session : null);
    if (!running) map.say("Its session has ended", "The map and table show a running session only. The files it made stay in its workspace.");
  } catch (e) {
    logEl.append(el("p", "error-text", String(e)));
  }
  return () => backToLiveMap(map);
}

async function showSession(main, session) {
  const view = h("div", { class: "chatview" });
  const map = new SessionMap();
  const s = store.sessions.find((x) => x.session === session);
  const who = clientName(s?.client);
  const title = h("h1", {}, s ? `${who} session` : "Session");
  const sub = h("p", { class: "sub" }, s ? `Started ${when(s.started)} · ${s.last_used ? `used ${ago(s.last_used)}` : "not used yet"} · the conversation lives in ${who}` : "This session is not running.");
  const head = h("header", { class: "chathead" },
    h("div", { class: "titlebox" }, title, sub), h("div", { class: "actions" }, mapButton(map), mapMenu(map)));
  // What the session makes is the thing to watch here: its map is open.
  map.open = !!s;
  const tray = h("section", { class: "tray", "aria-label": "Waiting for you", hidden: true });
  const acts = h("ol", { class: "activity", "aria-label": "Tool calls in this session" });
  const body = h("div", { class: "scroll pad stack" }, h("p", { class: "muted small" }, `What ${who}'s assistant did through Columbus in this session, from the audit log. The conversation itself is in ${who}.`), acts);
  const col = h("section", { class: "chatcol", "aria-label": "Session activity" }, head, tray, body);
  view.append(col);
  main.append(view);
  trayEl = tray;
  trayFor = session;
  drawTray();
  map.setSession(s ? session : null);
  map.show();
  if (!s) map.say("This session is not running", "The map and table show a running session only.");
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
  return () => { clearInterval(timer); unsub(); if (trayEl === tray) trayEl = null; backToLiveMap(map); };
}

function showIndex(main) {
  const search = h("input", { type: "search", placeholder: "Search chats", "aria-label": "Search chats" });
  const head = placeHead("Chats", search, [button("New chat", "primary", () => { location.hash = "#/chats/fresh"; })]);
  const list = h("div", { class: "stack", id: "chat-list" });
  // Which chats show: all of them, one project's, or those in no project.
  const filter = h("select", { id: "chats-project", class: "pick", "aria-label": "Show the chats of" });
  const count = h("span", { id: "chats-count" });
  const foot = h("div", { class: "footline" }, filter, count);
  main.append(h("div", { class: "page" }, head, h("div", { class: "scroll" }, h("div", { class: "pad" }, list)), foot));
  let project = "*";
  let seq = 0;
  const draw = async () => {
    const ql = search.value.trim().toLowerCase();
    const mine = ++seq;
    let hits = null;
    if (ql.length >= 2) {
      try { hits = await invoke("chats_search", { query: search.value }); } catch { hits = []; }
      if (mine !== seq) return;
    }
    const projects = store.projects?.projects || [];
    const known = new Set(projects.map((p) => p.id));
    if (project !== "*" && project !== "" && !known.has(project)) project = "*";
    redraw(filter, h("option", { value: "*" }, "All chats"), ...projects.map((p) => h("option", { value: p.id }, p.name)), h("option", { value: "" }, "No project"));
    filter.value = project;
    filter.hidden = !projects.length;
    fitSelect(filter);
    let chats = store.chats;
    const snippets = new Map();
    if (hits) {
      for (const f of hits) snippets.set(f.id, f.snippet);
      chats = chats.filter((c) => snippets.has(c.id) || c.title.toLowerCase().includes(ql));
    } else if (ql) chats = chats.filter((c) => c.title.toLowerCase().includes(ql));
    if (project !== "*") chats = chats.filter((c) => (project ? c.project === project : !c.project || !known.has(c.project)));
    const rows = chats.map((c) => ({
      t: c.updated || "", title: c.title, href: c.live ? "#/chats/new" : `#/chats/c/${encodeURIComponent(c.id)}`,
      sub: snippets.get(c.id) || [project === "*" && c.project && known.has(c.project) ? projectName(c.project) : null, c.live ? "Open now" : null, plural(c.messages, "message")].filter(Boolean).join(" · "),
      waiting: c.live && waitingCount() > 0, project: c.project && known.has(c.project) ? c.project : "",
    }));
    // Sessions other AI apps run: their activity and map (the conversation
    // itself is in that app).
    if (project === "*") {
      for (const x of otherSessions()) {
        const who = clientName(x.client);
        if (ql && !`${who} session`.toLowerCase().includes(ql)) continue;
        const pending = ((store.inbox.find((i) => i.session === x.session) || {}).pending || []).length;
        rows.push({ t: x.last_used || x.started || "", title: `${who} session`, href: `#/chats/s/${encodeURIComponent(x.session)}`, session: x.session,
          sub: `Running in ${who}${pending ? ". Waiting for your OK." : ". The conversation is in that app."}`, waiting: pending > 0 });
      }
    }
    rows.sort((a, b) => String(b.t).localeCompare(String(a.t)));
    const out = [];
    let ul = null;
    let last = null;
    for (const r of rows) {
      const e = era(r.t);
      if (e !== last) {
        last = e;
        ul = h("ul", { class: "list" });
        out.push(h("section", { class: "card" }, sec(e), h("div", { class: "bd flush" }, ul)));
      }
      const li = h("li", { class: "clickable", "data-href": r.href, "data-project": r.project || null },
        h("div", { class: "text" }, h("b", {}, h("a", { href: r.href }, r.title)), h("span", { class: "where clip" }, r.sub)),
        h("span", { class: "when" }, r.waiting ? h("span", { class: "dot warn", role: "img", "aria-label": "Waiting for you", title: "Waiting for you" }) : null, shortWhen(r.t)));
      ul.append(li);
    }
    if (!rows.length) out.push(emptyState(ql ? "No chat matches" : "No chats yet", ql ? "Chats are searched by title and by what was said." : "Ask something and it is saved here, on this computer."));
    redraw(list, ...out);
    count.textContent = project === "*" ? "" : plural(chats.length, "chat");
  };
  list.addEventListener("click", (e) => {
    const li = e.target.closest("li[data-href]");
    if (li && e.target.tagName !== "A") location.hash = li.dataset.href;
  });
  filter.addEventListener("change", () => { project = filter.value; draw(); });
  search.addEventListener("input", draw);
  const unsub = store.on((k) => { if (k === "chats" || k === "sessions" || k === "projects" || k === "inbox" || k === "waiting") draw(); });
  draw();
  return () => unsub();
}

export { emptyState };
