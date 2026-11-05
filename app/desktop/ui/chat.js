// The chat window. Everything the model or the server says is shown as
// text (DOM nodes and textContent), never as markup.
const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

const $ = (id) => document.getElementById(id);
const log = $("log");
const text = $("text");
const send = $("send");
const stop = $("stop");
const status = $("status");
let started = false;
let busy = false;
let current = null; // the assistant message being written
const steps = new Map();

function el(tag, cls, t) {
  const e = document.createElement(tag);
  if (cls) e.className = cls;
  if (t != null) e.textContent = t;
  return e;
}

function scroll() {
  log.scrollTop = log.scrollHeight;
}

// A small, safe Markdown reading: paragraphs, lists, code blocks, inline
// code and bold. Anything else stays as written.
function inline(parent, s) {
  const parts = s.split(/(`[^`]+`|\*\*[^*]+\*\*)/g);
  for (const p of parts) {
    if (!p) continue;
    if (p.startsWith("`") && p.endsWith("`") && p.length > 2) parent.append(el("code", null, p.slice(1, -1)));
    else if (p.startsWith("**") && p.endsWith("**") && p.length > 4) parent.append(el("strong", null, p.slice(2, -2)));
    else parent.append(document.createTextNode(p));
  }
}

function render(box, md) {
  const lines = md.replace(/\r/g, "").split("\n");
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
    if (!line.trim()) {
      flush();
      continue;
    }
    const h = /^#{1,6}\s+(.*)$/.exec(line);
    if (h) {
      flush();
      const p = el("p");
      p.append(el("strong", null, h[1]));
      box.append(p);
      continue;
    }
    para.push(line.trim());
  }
  flush();
}

// The model chose these values: quoted and cut, so they read as names,
// not as the app speaking.
function q(v) {
  const t = String(v).replace(/\s+/g, " ");
  return `"${t.length > 60 ? `${t.slice(0, 60)}…` : t}"`;
}

// What a tool call is doing, in plain words.
function describe(name, input) {
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
    case "run_python": return "Running a Python script";
    default: return `Running ${q(name)}`;
  }
}

function assistant() {
  if (!current) {
    current = el("div", "msg ai");
    log.append(current);
  }
  return current;
}

function setBusy(b) {
  busy = b;
  send.disabled = b;
  stop.hidden = !b;
  status.textContent = b ? "Working…" : "";
}

const cards = new Map();

function approval(e) {
  current = null;
  const card = el("div", "ask");
  card.setAttribute("role", "alertdialog");
  card.setAttribute("aria-label", e.title);
  card.append(el("h3", null, e.title));
  // The server's own words.
  card.append(el("p", "body", e.summary));
  if (e.code != null) {
    // Written by the AI: in its own box, never mixed with the server's words.
    const n = e.code.split("\n").length;
    card.append(el("p", "code-label", `Code written by the AI (${n} line${n === 1 ? "" : "s"}). Read it before allowing:`));
    const pre = el("pre", "code");
    e.code.split("\n").forEach((line) => pre.append(el("span", "ln", line === "" ? " " : line)));
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
  log.append(card);
  cards.set(e.id, row);
  // Show the card from its title, not its end.
  card.scrollIntoView({ block: "start" });
}

function gone(id) {
  const row = cards.get(id);
  cards.delete(id);
  if (row && row.isConnected) row.replaceWith(el("p", "done", "No longer waiting: the request was withdrawn or timed out."));
}

listen("chat-event", ({ payload: e }) => {
  switch (e.kind) {
    case "text":
      render(assistant(), e.text);
      break;
    case "tool_started": {
      current = null;
      const s = el("div", "step running");
      s.append(el("span", "what", describe(e.name, e.input)));
      steps.set(e.id, s);
      log.append(s);
      break;
    }
    case "tool_finished": {
      const s = steps.get(e.id) || el("div", "step");
      s.classList.remove("running");
      s.append(el("span", e.ok ? null : "bad", e.ok ? e.summary : `Refused or failed: ${e.summary}`));
      if (e.fails) s.append(el("span", "bad", `${e.fails} check${e.fails > 1 ? "s" : ""} failed`));
      if (e.warns) s.append(el("span", "warn", `${e.warns} to review`));
      if (!s.isConnected) log.append(s);
      break;
    }
    case "approval":
      approval(e);
      return;
    case "approval_gone":
      gone(e.id);
      break;
    case "status":
      current = null;
      log.append(el("div", "status", e.text));
      break;
    case "error":
      current = null;
      log.append(el("div", "error", e.text === "stopped" ? "Stopped." : `Could not finish: ${e.text}`));
      setBusy(false);
      break;
    case "done":
      current = null;
      setBusy(false);
      break;
  }
  scroll();
});

async function start() {
  if (started) return true;
  status.textContent = "Starting a Columbus session…";
  try {
    const s = await invoke("chat_start");
    started = true;
    $("map").disabled = !s.session;
    status.textContent = "";
    return true;
  } catch (err) {
    status.textContent = "";
    log.append(el("div", "error", String(err)));
    scroll();
    return false;
  }
}

$("composer").addEventListener("submit", async (ev) => {
  ev.preventDefault();
  const t = text.value.trim();
  if (!t || busy) return;
  if (!(await start())) return;
  current = null;
  log.append(el("div", "msg you", t));
  text.value = "";
  scroll();
  setBusy(true);
  try {
    await invoke("chat_send", { text: t });
  } catch (err) {
    log.append(el("div", "error", String(err)));
    setBusy(false);
  }
});

text.addEventListener("keydown", (ev) => {
  if (ev.key === "Enter" && !ev.shiftKey && !ev.isComposing) {
    ev.preventDefault();
    $("composer").requestSubmit();
  }
});

stop.addEventListener("click", () => invoke("chat_stop"));

$("map").addEventListener("click", async () => {
  try {
    await invoke("chat_open_map");
  } catch (err) {
    log.append(el("div", "error", String(err)));
  }
});

$("new").addEventListener("click", async () => {
  await invoke("chat_end");
  started = false;
  current = null;
  steps.clear();
  log.replaceChildren();
  cards.clear();
  $("map").disabled = true;
  setBusy(false);
  text.focus();
});

// Settings
const panel = $("settings");
function showSettings(on) {
  panel.hidden = !on;
  $("settings-toggle").setAttribute("aria-expanded", String(on));
}
$("settings-toggle").addEventListener("click", () => showSettings(panel.hidden));

async function loadSettings() {
  const s = await invoke("chat_settings");
  const keyState = $("key-state");
  keyState.textContent = s.has_key ? "A key is saved." : "No key saved yet.";
  keyState.className = s.has_key ? "note" : "note bad";
  const model = $("model");
  model.replaceChildren();
  for (const [id, label] of s.models) {
    const o = el("option", null, label);
    o.value = id;
    o.selected = id === s.model;
    model.append(o);
  }
  $("server").value = s.server || "";
  const ss = $("server-state");
  ss.textContent = s.server_found ? "Found." : "Not found: unpack the Columbus folder, or enter where columbus-mcp is.";
  ss.className = s.server_found ? "note" : "note bad";
  if (!s.has_key || !s.server_found) showSettings(true);
  return s;
}

$("key-form").addEventListener("submit", async (ev) => {
  ev.preventDefault();
  try {
    await invoke("chat_set_key", { key: $("key").value });
    $("key").value = "";
    await loadSettings();
  } catch (err) {
    $("key-state").textContent = String(err);
    $("key-state").className = "note bad";
  }
});
$("key-clear").addEventListener("click", async () => {
  await invoke("chat_clear_key");
  await loadSettings();
});
$("prefs-form").addEventListener("submit", async (ev) => {
  ev.preventDefault();
  try {
    await invoke("chat_save_settings", { model: $("model").value, server: $("server").value });
    await loadSettings();
    if (started) log.append(el("div", "status", "Saved. A new chat uses the new settings."));
  } catch (err) {
    $("server-state").textContent = String(err);
    $("server-state").className = "note bad";
  }
});

loadSettings();
text.focus();
