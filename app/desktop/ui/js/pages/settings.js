// Settings: one sheet over the page that is open. Its first view holds what
// a person changes (the assistant, Python, the map, this computer); the
// longer parts open inside it: the key and engine, the Python environments,
// where files live, the keyboard, and the organization's policy in force
// (read only, with today's switches, each turned on only after the person
// confirms at the keyboard in the app's own code).
import * as gitServers from "./git_servers.js";
import { $, h, el, icon, invoke as call, store, showError, savePrefs, applyTheme, toast, button, iconButton, sec, openDialog } from "../core.js";

// Whether anything was saved since the sheet opened: the page under it is
// brought up to date when the sheet closes (main.js). Every command here
// but these reads saves something.
let changed = false;
const READS = new Set(["chat_settings", "settings_state", "settings_environments", "settings_pick", "open_classic"]);
const pending = new Set();
const invoke = (cmd, args) => {
  if (READS.has(cmd)) return call(cmd, args);
  changed = true;
  const p = call(cmd, args);
  // Kept until it is answered, either way: the page under the sheet is
  // brought up to date only after every save has landed.
  const done = p.then(() => {}, () => {}).finally(() => pending.delete(done));
  pending.add(done);
  return p;
};

/** Resolves when every save begun in the sheet has been answered (a
 * confirmation at the keyboard can keep one waiting after the sheet closed). */
export function saves() {
  return Promise.all([...pending]);
}

// What the scripts switch gives depends on the approval rule in force
// (scripts.approval: each, session or none).
function scriptsText(rule) {
  if (rule === "none") return "Your assistant may run Python scripts it writes. They run in a sandbox with no network, on copies, without asking you each time; one that needs the internet or leaves the sandbox still asks.";
  if (rule === "session") return "Your assistant may run Python scripts it writes, after you read and approve each one; you can allow a script for a whole chat. They run in a sandbox with no network.";
  return "Your assistant may run Python scripts it writes, after you read and approve each one. They run in a sandbox with no network.";
}

const SWITCHES = {
  allow_edits: ["Allow edits.", "Your assistant may write to connections set to read-write, and only after you approve each change set. When off, every connection is read-only."],
  allow_scripts: ["Allow Python scripts.", (p) => scriptsText(p.script_approval)],
  allow_personal_tools: ["Allow personal tools.", "Your assistant may propose tools made from its scripts, and use the ones you install without asking each time. They run in the same sandbox as scripts."],
  allow_shared_tools: ["Allow shared tools and workflows.", "You may open tools and workflows coworkers share with you as a file. Each arrives as a proposal you read and approve yourself, like your assistant's; nothing from a shared file is installed or run before that."],
  allow_workflows: ["Allow workflows.", "Your assistant may propose workflows, and the ones you approve run by themselves on their schedule, without asking each time, within what they list."],
  allow_project_files: ["Allow project files.", "Your assistant may list, read and write text files in a project's own folder (scripts and notes), and run scripts kept there in the sandbox. Data still leaves a chat only through an export."],
};

/** One of the organization's switches. Turning one on is confirmed by the
 * person (Touch ID, password or Windows Hello) in the app's Rust code. */
export function switchRow(name, st, after) {
  const p = st.policy;
  const box = h("input", { type: "checkbox", id: name });
  box.checked = !!p[name];
  box.disabled = !p.writable;
  const [b, words] = SWITCHES[name];
  const text = typeof words === "function" ? words(p) : words;
  const err = h("p", { class: "error-text small", role: "alert", hidden: true });
  box.addEventListener("change", async () => {
    const want = box.checked;
    box.disabled = true;
    showError(err, null);
    try {
      await invoke("settings_set_switch", { name, on: want });
      toast("Saved. Your assistant uses the change from its next request.");
    } catch (e) {
      showError(err, e);
      box.checked = !want;
      const g = $("error");
      if (g) showError(g, e);
    } finally {
      box.disabled = !p.writable;
      if (after) after();
    }
  });
  return h("div", {}, h("label", { class: "switch-row" }, box, h("span", {}, h("b", {}, b), " ", text)), err);
}

/** How the assistant works (build brief 3, D-16): one choice between two
 * cards, Work freely on copies or Ask me before each script, with the mode
 * in force said under them. Choosing the first is confirmed by the person
 * once (Touch ID, password or Windows Hello) in the app's Rust code, which
 * then writes the settings; where the policy file is not the person's to
 * change, the cards only show what is in force. `id` names the controls
 * (the first run has its own); `after` runs once a choice is saved. */
export function workMode(st, after, id = "work", inSheet = true) {
  const p = st.policy;
  const send = inSheet ? invoke : call;
  const mine = p.writable && p.exists;
  const free = p.work_mode === "free";
  const err = h("p", { id: `${id}-error`, class: "error-text small", role: "alert", hidden: true });
  const state = h("p", { id: `${id}-state`, class: "small", role: "status" });
  const say = () => {
    const now = p.work_mode === "free" ? "On now: Work freely on copies."
      : p.work_mode === "ask" ? `On now: Ask me before each script.${p.script_approval === "session" ? " You can also allow a script for a whole chat." : ""}`
        : "Python scripts are off, so your assistant runs none. Work freely on copies turns them on.";
    const whose = !p.writable ? " This is set by your organization: this account cannot change it."
      : !p.exists ? " There is no organization settings file yet, so Columbus's strict defaults apply. Create it as the install guide shows, then choose here."
        : "";
    state.replaceChildren(h("b", {}, now), whose ? h("span", { id: `${id}-managed` }, whose) : "");
  };
  const radio = (value, checked) => {
    const r = h("input", { type: "radio", name: `${id}_mode`, id: `${id}-${value}`, value });
    r.checked = checked;
    r.disabled = !mine;
    return r;
  };
  const freeBox = radio("free", free);
  const askBox = radio("ask", !free);
  const card = (box, title, more, text) => h("label", { class: "choice", "data-mode": box.value }, box,
    h("span", { class: "text" }, h("b", {}, title, more ? h("span", { class: "muted" }, more) : ""), h("span", {}, text)));
  const choose = async (want) => {
    freeBox.disabled = askBox.disabled = true;
    showError(err, null);
    try {
      await send("settings_set_work_mode", { free: want });
      toast(want ? "Saved. Your assistant works freely on copies from its next request." : "Saved. Columbus asks you before each script from the next one.");
      if (after) after();
    } catch (e) {
      showError(err, e);
      freeBox.checked = free;
      askBox.checked = !free;
    } finally {
      freeBox.disabled = askBox.disabled = !mine;
    }
  };
  freeBox.addEventListener("change", () => { if (freeBox.checked) choose(true); });
  askBox.addEventListener("change", () => { if (askBox.checked) choose(false); });
  say();
  const how = st.confirm_with === "not available on this system"
    ? "On this system the app cannot confirm it is you (there is no Touch ID or Windows Hello), so set it in the policy file instead."
    : `Choosing Work freely on copies asks for ${st.confirm_with} once. Going back to asking does not.`;
  return h("div", { class: "stack", id },
    h("div", { class: "choices", role: "radiogroup", "aria-label": "How your assistant works" },
      card(freeBox, "Work freely on copies", " (recommended)", "Scripts and tools run without asking you each time, in a sandbox, on copies of your data. They cannot reach your connections, your passwords, your other files or the internet."),
      card(askBox, "Ask me before each script", "", "You read each script your assistant writes and allow it before it runs, in the same sandbox.")),
    state, err,
    h("p", { class: "small muted" }, "Either way your assistant reads your data only through Columbus, under each source's rules, and Columbus still asks you before anything leaves the workspace or changes a source. ", mine ? how : ""));
}

/** The parts that open inside the sheet (also offered by the search). */
export const SECTIONS = [["ai", "Assistant: key, model and engine"], ["environments", "Python environments"], ["privacy", "Privacy and files"], ["keyboard", "Keyboard shortcuts"], ["org", "Organization rules"]];
const LEDES = {
  ai: "The AI the app's chat uses. Columbus's rules are the same whichever AI you use.",
  privacy: "What stays on this computer, and where files live.",
  keyboard: "Shortcuts in the main window.",
  environments: "Python interpreters your assistant's scripts may run in, besides the GIS engine's own, such as an ArcGIS Pro clone. You register and confirm each one; your assistant cannot.",
  org: "The policy in force on this computer, and the switches your organization lets you change. Your assistant cannot change any of these.",
};

// Git servers (sync) is a part of its own, in its own file.
SECTIONS.splice(SECTIONS.length - 1, 0, gitServers.PART);
LEDES[gitServers.PART[0]] = gitServers.LEDE;

let sheet = null;
let sheetClosed = null;

/** Open the settings sheet (or show one of its parts in it). */
export function openSheet(sub, onClosed) {
  const part = SECTIONS.find(([k]) => k === sub);
  if (!sheet) {
    sheet = h("dialog", { class: "settings", id: "settings", "aria-labelledby": "settings-title" });
    document.body.append(sheet);
    sheet.addEventListener("close", () => {
      sheet.replaceChildren();
      const fn = sheetClosed;
      sheetClosed = null;
      const saved = changed;
      changed = false;
      if (fn) fn(saved);
    });
    // A click on the veil beside the sheet closes it.
    sheet.addEventListener("click", (e) => { if (e.target === sheet) sheet.close(); });
  }
  sheetClosed = onClosed || null;
  const body = h("div", { class: "pad stack" });
  const foot = h("div", { class: "footline", hidden: true });
  const title = h("h1", { id: "settings-title", tabindex: "-1" }, part ? part[1].replace(/:.*$/, "") : "Settings");
  sheet.replaceChildren(h("div", { class: "dlg" },
    h("header", { class: part ? "pagehead has-back" : "pagehead" }, h("div", { class: "bar" },
      part ? h("a", { class: "iconbtn back", href: "#/settings", id: "settings-back", "aria-label": "Settings", title: "Settings" }, icon("back")) : null,
      h("div", { class: "titles" }, title),
      h("div", { class: "actions" }, iconButton("close", "Close settings", () => sheet.close(), { id: "settings-close" }))),
    part ? h("p", { class: "lede" }, LEDES[part[0]]) : null),
    h("div", { class: "scroll" }, body), foot));
  if (!sheet.open) {
    changed = false;
    sheet.showModal();
  }
  // A screen reader starts at the sheet's title, not its close button.
  title.focus({ preventScroll: true });
  if (part) ({ ai, privacy, keyboard, environments, org, git: (b) => gitServers.part(b, () => { changed = true; }) })[part[0]](body);
  else overview(body, foot);
}

/** Close the sheet (the person went to a page, which is drawn fresh).
 * Says whether anything was saved while it was open. */
export function closeSheet() {
  if (!sheet || !sheet.open) return false;
  const saved = changed;
  changed = false;
  sheet.close();
  return saved;
}

// Today's session list and settings window, kept beside the main window.
function classicMenu() {
  const close = () => d.close();
  const go = async (which) => {
    try { await invoke("open_classic", { which }); } catch (e) { toast(String(e)); }
    close();
  };
  const d = openDialog("Classic windows",
    h("p", { class: "muted" }, "The earlier session list and settings window, kept beside the main window until it has proved itself. They change the same settings."),
    [h("button", { type: "button", class: "btn", onclick: () => go("sessions") }, "Session list"),
      h("button", { type: "button", class: "btn", onclick: () => go("settings") }, "Settings window"),
      h("span", { class: "spacer" }),
      h("button", { type: "button", class: "btn", onclick: close }, "Close")]);
}

// What the chat's server and engine are, in plain words.
export function serverLines(s) {
  if (!s.server_found) return ["No Columbus server was found: unpack the Columbus folder as the install guide shows."];
  const v = s.server_version ? `version ${s.server_version}` : "version unknown";
  const out = [s.server_bundled ? `New chats use the Columbus server that comes with the app (${v}).` : `New chats use ${s.server_in_use} (${v}).`];
  if (s.server_older) out.push(`It is older than this app (${s.app_version}): the map beside a chat and newer tools may not work. Install this version's app again.`);
  out.push(s.engine ? `GIS engine: ${s.engine}` : "The GIS engine was not found in your Columbus folder: set it up with the install guide's engine step, or enter where its Python is above.");
  return out;
}

// ---------- Assistant: the key, the model, the engine ----------
async function ai(body) {
  const keyState = h("p", { id: "key-state", class: "small muted" });
  const key = h("input", { id: "key", type: "password", autocomplete: "off", spellcheck: "false", placeholder: "sk-ant-…", class: "grow" });
  const model = h("select", { id: "model" });
  const effort = h("select", { id: "effort" });
  const engine = h("input", { id: "engine", type: "text", spellcheck: "false", placeholder: "Empty: the engine in your Columbus folder", class: "grow" });
  const suggest = h("div", { class: "row wrap", id: "engine-suggest" });
  const serverState = h("p", { id: "server-state", class: "small muted" });
  const keyForm = h("form", { id: "key-form", class: "stack" },
    h("label", { class: "field", for: "key" }, "API key"),
    h("div", { class: "row wrap" }, key, h("button", { type: "submit", class: "btn primary" }, "Save key"), h("button", { type: "button", id: "key-clear", class: "btn" }, "Remove key")),
    keyState);
  const prefs = h("form", { id: "prefs-form", class: "stack" },
    h("label", { class: "field" }, "Model for new chats", model),
    h("label", { class: "field" }, "Thinking level for new chats", effort),
    h("p", { class: "small muted" }, "Each chat can change both below its message box. Higher thinking is slower, costs more and is more thorough."),
    h("label", { class: "field" }, "GIS engine (Python)", h("div", { class: "row" }, engine)),
    suggest,
    serverState,
    h("div", { class: "row" }, h("button", { type: "submit", class: "btn primary" }, "Save settings")));
  body.append(
    h("section", { class: "card" }, sec("API key"),
      h("div", { class: "bd stack" },
        h("p", { class: "small muted" }, "The chat uses an Anthropic API key, billed by Anthropic per use, separately from a Claude subscription. Create one at console.anthropic.com. It is kept in your computer's credential store, never in a file, and this window can set or remove it but never read it back."),
        keyForm, prefs)),
    h("p", { class: "small muted" }, "You can also use Columbus from Claude desktop and other AI apps directly, without a key here; their sessions show under Chats."));

  const load = async () => {
    const s = await invoke("chat_settings");
    keyState.replaceChildren(...(s.has_key
      ? [h("span", { class: "badge pass" }, "✓ Key saved"), ` A key ending in ${s.key_hint || "…"} is saved in your computer's credential store. Chats use it; paste a new one to replace it.`]
      : ["No key saved yet: chats can't use the AI until you save one."]));
    keyState.className = s.has_key ? "small" : "small error-text";
    key.placeholder = s.has_key ? `Saved (ends in ${s.key_hint || "…"}); paste a new key to replace it` : "sk-ant-…";
    $("key-clear").hidden = !s.has_key;
    model.replaceChildren(...s.models.map(([id, label]) => { const o = el("option", null, label); o.value = id; o.selected = id === s.model; return o; }));
    effort.replaceChildren(...s.efforts.map(([id, label]) => { const o = el("option", null, label); o.value = id; o.selected = id === s.effort; return o; }));
    engine.value = s.engine_chosen || "";
    suggest.replaceChildren(...(s.engine_suggestions || []).map((p) => button(`Use ${p}`, "sm", () => { engine.value = p; })));
    serverState.replaceChildren(...serverLines(s).map((t) => el("span", "block", t)));
    serverState.className = !s.server_found || s.server_older || !s.engine ? "small error-text" : "small muted";
    return s;
  };
  keyForm.addEventListener("submit", async (ev) => {
    ev.preventDefault();
    try { await invoke("chat_set_key", { key: key.value }); key.value = ""; await load(); toast("Key saved. New chats will use it."); } catch (err) { keyState.textContent = String(err); keyState.className = "small error-text"; }
  });
  $("key-clear").addEventListener("click", async () => { await invoke("chat_clear_key"); await load(); toast("Key removed."); });
  prefs.addEventListener("submit", async (ev) => {
    ev.preventDefault();
    try {
      await invoke("chat_save_settings", { model: model.value, effort: effort.value, engine: engine.value });
      await load();
      toast("Saved. A new chat uses the new settings.");
    } catch (err) { serverState.textContent = String(err); serverState.className = "small error-text"; }
  });
  await load();
}

// ---------- The first view: what a person changes ----------
// One setting: its name, a line about it, and its control.
function setRow(name, about, control, id) {
  return h("div", { class: "setrow", id: id || null }, h("div", { class: "text" }, h("b", {}, name), about ? h("span", { class: "where" }, about) : null), control);
}

async function overview(body, foot) {
  const info = store.info;
  const go = (label, href, id) => h("a", { class: "btn sm", href, id: id || null }, label);

  // The assistant: its key, and what new chats start with.
  const keyLine = h("span", {}, "…");
  const model = h("select", { id: "set-model", "aria-label": "Model for new chats" });
  const effort = h("select", { id: "set-effort", "aria-label": "Thinking for new chats" });
  const engineLine = h("span", {}, "…");
  let chat = null;
  const saveChat = async () => {
    try {
      await invoke("chat_save_settings", { model: model.value, effort: effort.value, engine: (chat && chat.engine_chosen) || "" });
      toast("Saved. A new chat uses it.");
    } catch (e) { toast(String(e)); }
  };
  model.addEventListener("change", saveChat);
  effort.addEventListener("change", saveChat);

  // Python: where scripts run.
  const envLine = h("span", {}, "…");

  // The map and the look.
  const basemap = h("input", { type: "checkbox", class: "switch", id: "pref-basemap", "aria-label": "Street background" });
  basemap.checked = !!store.prefs.basemap;
  basemap.addEventListener("change", () => savePrefs({ basemap: basemap.checked }));
  const theme = h("div", { class: "seg", role: "group", "aria-label": "Appearance" });
  for (const [v, t] of [["light", "Light"], ["dark", "Dark"], ["system", "Match the system"]]) {
    const b = h("button", { type: "button", "aria-pressed": String((store.prefs.theme || "system") === v) }, t);
    b.addEventListener("click", async () => {
      await savePrefs({ theme: v });
      applyTheme(v);
      for (const x of theme.children) x.setAttribute("aria-pressed", String(x === b));
    });
    theme.append(b);
  }

  // How the assistant works: freely on copies, or asking before each script.
  const modeBox = h("div", { class: "bd", id: "work-mode" }, h("p", { class: "small muted" }, "…"));
  const drawMode = async () => {
    try {
      const st = await invoke("settings_state");
      modeBox.replaceChildren(workMode(st, drawMode));
    } catch (e) { modeBox.replaceChildren(h("p", { class: "error-text small" }, String(e))); }
  };
  drawMode();

  // This computer: where things are kept, and whose rules apply.
  const src = info ? info.policy.source : "";
  const rules = { organization: "Set by your organization.", file: "From a policy file on this computer.", defaults: "No policy file: Columbus's strict defaults apply.", unreadable: "The policy file cannot be read, so nothing beyond the defaults is allowed." }[src] || "";
  body.append(
    h("section", { class: "card" }, sec("How your assistant works"), modeBox),
    h("section", { class: "card" }, sec("Assistant"),
      setRow("API key", keyLine, go("Change", "#/settings/ai", "set-key")),
      setRow("Model for new chats", "You can change it in any chat.", model),
      setRow("Thinking for new chats", "Higher is slower, costs more and is more thorough.", effort)),
    h("section", { class: "card" }, sec("Python"),
      setRow("Scripts run in", engineLine, go("Change", "#/settings/ai", "set-engine")),
      setRow("Other environments", envLine, go("Manage", "#/settings/environments", "set-envs"))),
    h("section", { class: "card" }, sec("Map"),
      setRow("Street background", "What new maps start with; each map has its own switch. On, street tiles come from OpenFreeMap, which then sees the area in view. Your organization can remove it.", basemap),
      setRow("Appearance", null, theme)),
    h("section", { class: "card" }, sec("This computer"),
      setRow("Chats, results and the audit log are kept here", info && info.places ? h("span", { class: "mono" }, info.places.workspaces) : "On this computer only.", go("Show", "#/settings/privacy", "set-privacy")),
      setRow("Organization rules", h("span", { id: "set-policy" }, rules, " They win over anything set here."), go("Read them", "#/settings/org", "set-org"))));
  foot.hidden = false;
  foot.replaceChildren(h("span", { class: "grow", id: "about" }, `Columbus ${info ? info.version : ""} · Apache-2.0 · no telemetry`),
    h("a", { href: "#/settings/keyboard" }, "Keyboard shortcuts"),
    h("button", { type: "button", class: "linkbtn classic", onclick: classicMenu, title: "The earlier session list and settings window, kept until the main window has proved itself" }, "Classic windows"));
  body.append(h("section", { class: "card" }, sec("Sync"),
    setRow("Git servers", gitServers.overviewLine(), go("Manage", "#/settings/git", "set-git"))));

  invoke("chat_settings").then((s) => {
    chat = s;
    keyLine.textContent = s.has_key ? `A key ending in ${s.key_hint || "…"} is saved in your computer's credential store.` : "No key yet: chats can't use the AI until you save one.";
    keyLine.className = s.has_key ? "" : "error-text";
    model.replaceChildren(...s.models.map(([id, label]) => { const o = el("option", null, label.replace(/ \(.*\)$/, "")); o.value = id; o.selected = id === s.model; return o; }));
    effort.replaceChildren(...s.efforts.map(([id, label]) => { const o = el("option", null, label.replace(/ \(.*\)$/, "")); o.value = id; o.selected = id === s.effort; return o; }));
    engineLine.replaceChildren(...(s.engine ? [h("span", { class: "mono" }, s.engine)] : ["The GIS engine was not found. Set where its Python is."]));
    engineLine.className = s.engine && s.server_found && !s.server_older ? "" : "error-text";
    if (s.server_found && s.server_older) engineLine.append(` The Columbus server is older than this app (${s.app_version}).`);
    if (!s.server_found) engineLine.replaceChildren("No Columbus server was found: unpack the Columbus folder as the install guide shows.");
  }).catch((e) => { keyLine.textContent = String(e); });
  invoke("settings_environments").then((st) => {
    const n = st.environments.length;
    const w = st.proposals.length;
    envLine.textContent = !st.allow ? "Off in your organization's settings."
      : `${n ? `${n} registered` : "None yet"}${w ? `. ${w} asked for by your assistant, waiting for you` : ""}.`;
    if (w) envLine.className = "warn-text";
  }).catch(() => { envLine.textContent = "Interpreters you register, such as an ArcGIS Pro clone."; });
}

// ---------- Privacy and sandbox ----------
async function privacy(body) {
  const info = store.info;
  const p = info ? info.places : null;
  body.append(
    h("section", { class: "card" }, sec("What stays on this computer"), h("div", { class: "bd stack" },
      h("p", {}, "The app sends nothing about you anywhere (no telemetry). The chat sends your messages, and the results its tools return, to the AI service you set up under Assistant; at most the rows your organization's policy allows per call."),
      h("p", {}, "Your chats are saved on this computer, in the app's data folder (one file per chat), so you can find and search them. Delete a chat from its page to delete its file."),
      h("p", {}, "The map's basemap, when you turn it on, fetches tiles from OpenFreeMap, which sends that site the area in view."))),
    h("section", { class: "card" }, sec("The sandbox"), h("div", { class: "bd" },
      h("p", {}, "Scripts and your own tools run confined: no network, a fresh folder each run, reading only the layers they are given and writing only their own folder. Columbus's server enforces this, not the app."))),
    h("section", { class: "card" }, sec("Where files live"), h("div", { class: "bd" }, p ? h("dl", { class: "kv" },
      h("dt", {}, "Audit log"), h("dd", { class: "mono" }, p.audit),
      h("dt", {}, "Workspaces"), h("dd", { class: "mono" }, p.workspaces),
      h("dt", {}, "Your notes"), h("dd", { class: "mono" }, p.notes),
      h("dt", {}, "Organization's pack"), h("dd", { class: "mono" }, p.context),
      h("dt", {}, "Policy"), h("dd", { class: "mono" }, info.policy.path)) : el("p", "muted", "—"),
      p && !p.from_session ? h("p", { class: "small muted" }, "No session is running, so these are Columbus's default places; a session your AI app starts may use others.") : null)));
}

// ---------- Keyboard ----------
async function keyboard(body) {
  const mac = navigator.platform.startsWith("Mac");
  const rows = [
    [mac ? "⌘ K" : "Ctrl K", "Search chats, data, workflows and pages"],
    ["Enter", "Send a message (Shift Enter for a new line)"],
    ["Esc", "Close a dialog, a menu, the search, this sheet, or what is open in the panel on the right"],
    ["Tab / Shift Tab", "Move between controls; every control can be reached"],
  ];
  const tbody = h("tbody", {}, ...rows.map(([k, t]) => h("tr", {}, h("td", { class: "nowrap" }, h("span", { class: "kbd" }, k)), h("td", {}, t))));
  body.append(h("section", { class: "card" }, h("table", { class: "t" }, h("thead", {}, h("tr", {}, h("th", {}, "Keys"), h("th", {}, "Does"))), tbody)),
    h("p", { class: "small muted" }, "Approval cards in your chat are answered with their buttons, which wait a second before they work so a keystroke meant for something else cannot approve a request."));
}

// ---------- Environments ----------
// Registering, confirming again, removing and approving are confirmed at
// the keyboard (Touch ID, password or Windows Hello) in the app's Rust
// code, like a connection; this page cannot skip that.
async function environments(body) {
  const error = h("p", { id: "env-error", class: "error-text", role: "alert", hidden: true });
  const notice = h("p", { id: "env-notice", class: "notice ok", role: "status", hidden: true });
  const off = h("div", { id: "env-off", class: "notice warn", hidden: true });
  const list = h("ul", { id: "environments", class: "list", "aria-live": "polite" });
  const none = h("p", { id: "no-env", class: "muted small quiet", hidden: true }, "No environments yet. Scripts run in the GIS engine's own Python.");
  const waiting = h("ul", { id: "env-proposals", class: "list", "aria-live": "polite" });
  const noneWaiting = h("p", { id: "no-proposals", class: "muted small quiet", hidden: true }, "Nothing waiting. When your assistant asks for a new environment (propose_environment), it waits here for you.");
  const conf = h("p", { id: "env-confirm-note", class: "small muted" });
  const foot = h("p", { class: "small muted wrap-any" }, "Registrations are kept in ", h("code", { id: "env-dir" }), ".");
  const add = button("Add environment", "sm", () => openAdd(), { id: "env-add" });
  body.append(error, notice, off,
    h("section", { class: "card" }, sec("Registered", null, h("span", { class: "more" }, add)), h("div", { class: "bd flush" }, list), none),
    h("section", { class: "card" }, sec("Asked for by your assistant"), h("div", { class: "bd flush" }, waiting), noneWaiting),
    conf, foot);
  let st = null;
  const say = (text) => { notice.textContent = text; notice.hidden = false; showError(error, null); };
  const act = async (btn, label, fn) => {
    btn.disabled = true;
    const was = btn.textContent;
    btn.textContent = label;
    showError(error, null);
    notice.hidden = true;
    try { await fn(); } catch (e) { showError(error, e); } finally { btn.disabled = false; btn.textContent = was; }
    await refresh();
  };
  const draw = () => {
    $("env-dir").textContent = st.dir;
    conf.textContent = st.confirm_with === "not available on this system"
      ? "On this system, environments cannot be registered here (there is no Touch ID or Windows Hello to confirm them): use columbus-mcp env add in a terminal instead."
      : `Adding, confirming, removing and approving ask for ${st.confirm_with} first. Declining does not.`;
    off.hidden = st.allow;
    off.replaceChildren(h("div", {}, h("b", {}, "Environments are off. "), "Your organization's policy (environments.allow) does not allow them, so scripts run only in the GIS engine's Python and none can be added."));
    add.disabled = !st.allow;
    list.replaceChildren();
    none.hidden = st.environments.length > 0;
    for (const e of st.environments) {
      const li = el("li", e.status === "ready" ? "conn" : "conn bad");
      const text = el("div", "text");
      text.append(h("div", { class: "row wrap" }, el("b", null, e.id), el("span", "where", e.python || "")));
      const chips = el("div", "chips");
      chips.append(el("span", `badge ${e.status === "ready" ? "pass" : "warn"}`, e.status === "ready" ? "Ready" : e.status === "changed" ? "Changed: confirm it again" : "Cannot be read"));
      if (e.trust) chips.append(el("span", e.trust === "trusted" ? "chip rw" : "chip ro", e.trust === "trusted" ? "Trusted (experimental)" : `Governed, level ${e.confinement}`));
      if (e.python_version) chips.append(el("span", "chip", `Python ${e.python_version}`));
      text.append(chips);
      if (e.arcpy_line) text.append(el("span", "where env-arcpy", e.arcpy_line));
      if (e.trust_line) text.append(el("span", "where", e.trust_line));
      if (e.reason) text.append(el("span", "where error-text env-reason", `Why: ${e.reason}.`));
      if (e.description) text.append(el("span", "where", e.description));
      const acts = h("div", { class: "acts" });
      if (e.status !== "invalid") {
        const c = button(e.status === "ready" ? "Confirm again" : "Confirm", e.status === "ready" ? "sm" : "sm primary", () => act(c, "Checking…", async () => {
          await invoke("settings_env_confirm", { id: e.id });
          say(`Confirmed ${e.id}. Scripts can run in it again.`);
        }), { title: "Check the interpreter and its packages again, and confirm it as it is now" });
        acts.append(c);
      }
      const r = button("Remove", "sm danger", () => act(r, "Removing…", async () => say(await invoke("settings_env_remove", { id: e.id }))));
      acts.append(r);
      li.append(text, acts);
      list.append(li);
    }
    waiting.replaceChildren();
    noneWaiting.hidden = st.proposals.length > 0;
    for (const p of st.proposals) {
      const li = el("li", "conn");
      const text = el("div", "text");
      text.append(h("div", { class: "row wrap" }, el("b", null, p.id), el("span", "where", `Python ${p.python_version || "(the engine's own version)"}`)));
      const chips = el("div", "chips");
      for (const pkg of p.packages) chips.append(el("span", "chip mono", pkg));
      text.append(chips, el("span", "where", `Why: ${p.reason}`));
      text.append(el("span", "where", `Columbus creates it with its own uv from ${st.index_hosts.join(", ")} only, wheels only, and registers it governed.`));
      const a = button("Approve", "sm primary", () => act(a, "Creating…", async () => {
        await invoke("settings_env_approve", { id: p.id, fingerprint: p.fingerprint });
        say(`Created and registered ${p.id}.`);
      }), { disabled: !st.allow });
      const d = button("Decline", "sm", () => act(d, "Declining…", async () => {
        await invoke("settings_env_decline", { id: p.id });
        say(`Declined ${p.id}.`);
      }));
      li.append(text, h("div", { class: "acts" }, a, d));
      waiting.append(li);
    }
  };
  const refresh = async () => {
    try { st = await invoke("settings_environments"); draw(); } catch (e) { showError(error, e); }
  };

  const openAdd = () => {
    const inp = (name, attrs = {}) => h("input", { name, autocomplete: "off", spellcheck: "false", ...attrs });
    const trust = h("select", { name: "trust" }, h("option", { value: "governed" }, "Governed: confined, extracts only"),
      h("option", { value: "trusted", disabled: !st.allow_trusted }, st.allow_trusted ? "Trusted (experimental): unconfined, each run approved" : "Trusted (your organization does not allow it)"));
    const confinement = h("select", { name: "confinement" }, h("option", { value: "A" }, "A: no network"),
      h("option", { value: "B", disabled: !st.allow_network }, st.allow_network ? "B: may reach the internet" : "B (your organization does not allow it)"));
    const browse = h("button", { type: "button", class: "btn" }, "Choose file…");
    const err = h("p", { id: "env-form-error", class: "error-text", role: "alert", hidden: true });
    const form = h("form", { id: "env-form", class: "stack" },
      h("div", { class: "grid2" },
        h("label", { class: "field" }, "Id", inp("env_id", { required: true, maxlength: "40", placeholder: "e.g. pro-clone" }), h("small", {}, "Lowercase letters, digits and -.")),
        h("label", { class: "field" }, "Trust", trust),
        h("label", { class: "field wide" }, "Python interpreter", h("span", { class: "pick" }, inp("env_python", { required: true }), browse), h("small", {}, "The full path to python.exe (Windows) or bin/python, e.g. an ArcGIS Pro clone's.")),
        h("label", { class: "field" }, "Confinement", confinement),
        h("label", { class: "field" }, "Description", inp("env_description"))),
      err);
    const save = h("button", { type: "submit", id: "env-save", class: "btn primary", form: "env-form" }, "Add");
    const cancel = h("button", { type: "button", class: "btn" }, "Cancel");
    const d = openDialog("Add environment", form, [h("span", { class: "spacer" }), cancel, save], "Add environment");
    d.id = "env-dialog";
    cancel.addEventListener("click", () => d.close());
    browse.addEventListener("click", async () => {
      try { const p = await invoke("settings_pick", { kind: "python" }); if (p) form.env_python.value = p; } catch (e) { showError(err, e); }
    });
    form.addEventListener("submit", async (ev) => {
      ev.preventDefault();
      save.disabled = true;
      save.textContent = "Checking…";
      showError(err, null);
      try {
        const e = await invoke("settings_env_add", { id: form.env_id.value, python: form.env_python.value, trust: trust.value, confinement: confinement.value, description: form.env_description.value || null });
        d.close();
        say(`Registered ${e.id}. Your assistant's scripts can run in it.`);
        await refresh();
      } catch (e) {
        showError(err, e);
      } finally {
        save.disabled = false;
        save.textContent = "Add";
      }
    });
    form.env_id.focus();
  };
  await refresh();
}

// ---------- Organization ----------
function sentences(f, rule) {
  const out = [];
  const list = (a) => (a && a.length ? a.join(", ") : "none");
  out.push(["Connections", `Connection types allowed: ${list(f.connection_types)}.`]);
  out.push(["Edits", f.allow_edits ? "Your assistant may propose edits to read-write connections; you approve each change set." : "No edits: every connection is read-only."]);
  out.push(["Scripts", !f.allow_scripts ? "No Python scripts."
    : rule === "none" ? "Your assistant may run Python scripts without asking you each time, in a sandbox with no network, on copies. A script that needs the internet or leaves the sandbox still asks you every time."
      : rule === "session" ? "Your assistant may run Python scripts, after you read and approve each one; you can allow a script for a whole chat."
        : "Your assistant may run Python scripts, after you read and approve each one."]);
  out.push(["Python environments", f.environments && f.environments.allow ? "Scripts may run in Python environments you register." : "Scripts run only in the GIS engine's own Python."]);
  out.push(["Personal tools", f.allow_personal_tools ? "Tools you install from what your assistant proposes may run without asking." : "Personal tools are off."]);
  out.push(["Toolboxes", f.allow_toolboxes ? "Toolboxes you install may be used; their tools run in the sandbox." : "Toolboxes are off."]);
  out.push(["Project files", f.allow_project_files ? "Your assistant may read and write text files in a project's own folder, and run scripts kept there." : "Project files are off."]);
  out.push(["Sync", f.allow_sync ? `A project's files may sync with a Git server when you press Sync${f.sync && f.sync.hosts && f.sync.hosts.length ? ` (only ${f.sync.hosts.join(", ")})` : ""}.` : "Sync with Git servers is off."]);
  if (f.return_rows != null) out.push(["Rows to the AI", `Your assistant sees at most ${Number(f.return_rows).toLocaleString()} rows per call.`]);
  if (f.hide_fields && f.hide_fields.length) out.push(["Hidden fields", `Never shown or sent to the AI: ${f.hide_fields.join(", ")}.`]);
  if (Array.isArray(f.exports)) out.push(["Exports", f.exports.length ? `Export folders: ${f.exports.map((e) => e.id || e.path).join(", ")}.` : "No export folders are set up."]);
  if (f.fetch_hosts && f.fetch_hosts.length) out.push(["Downloads", `Open data may be downloaded from ${f.fetch_hosts.join(", ")} without asking.`]);
  if (f.deny_tools && f.deny_tools.length) out.push(["Tools refused", f.deny_tools.join(", ")]);
  return out;
}

async function org(body) {
  const error = h("p", { id: "error", class: "error-text", role: "alert", hidden: true });
  body.append(error);
  let st;
  try {
    await (store.refresh ? store.refresh.info() : null);
    st = await invoke("settings_state");
  } catch (e) { showError(error, e); return; }
  const info = store.info;
  const p = st.policy;
  const src = info ? info.policy.source : "";
  const where = { organization: "Managed by your organization: this account cannot change the file. Ask your IT team to change it.", file: "A policy file on this computer that this account may change.", defaults: "No policy file: Columbus's strict defaults apply (no edits, no scripts, no personal tools).", unreadable: "The policy file cannot be read, so Columbus allows nothing beyond its defaults." }[src] || "";
  body.append(h("div", { class: `notice ${src === "unreadable" ? "fail" : src === "defaults" ? "warn" : ""}` }, h("div", {}, h("b", {}, "Policy in force. "), where, " ", h("span", { class: "mono" }, p.path))));
  if (p.problem) body.append(h("p", { id: "policy-problem", class: "error-text" }, p.problem));
  const sw = h("div", {});
  const mode = h("div", { class: "bd" });
  const drawSw = async () => {
    try {
      const s = await invoke("settings_state");
      mode.replaceChildren(workMode(s, drawSw, "org-work"));
      sw.replaceChildren(
        h("p", { id: "org-managed", class: "small muted", hidden: s.policy.writable }, "These are managed by your organization. Ask your IT team to change them."),
        ...Object.keys(SWITCHES).map((n) => switchRow(n, s, drawSw)),
        h("p", { class: "small muted" }, `Turning one on asks for ${st.confirm_with} first; turning one off does not. The rest of the file, comments included, is kept as it is.`));
    } catch (e) { showError(error, e); }
  };
  await drawSw();
  body.append(h("section", { class: "card" }, sec("How your assistant works"), mode),
    h("section", { class: "card" }, sec("Switches"), h("div", { class: "bd" }, sw)));
  if (info) {
    const dl = h("dl", { class: "kv" });
    for (const [k, v] of sentences(info.policy.in_force || {}, info.policy.script_approval)) dl.append(h("dt", {}, k), h("dd", {}, v));
    body.append(h("section", { class: "card" }, sec("In plain words"), h("div", { class: "bd" }, dl)));
    if (info.policy.text) {
      body.append(h("section", { class: "card" }, sec("The file as written, read only"),
        h("div", { class: "bd" }, el("pre", "box", info.policy.text))));
    }
  }
  body.append(h("p", { class: "small muted" }, "The audit log is under ", h("a", { href: "#/activity/audit" }, "Activity"), "."));
}
