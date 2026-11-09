// Settings: the AI for chats (ported from the chat window), the app's own
// preferences, privacy, keyboard, the organization's policy in force
// (read only, with today's switches, each turned on only after the person
// confirms at the keyboard in the app's own code), and About.
import { $, h, el, invoke, store, pageHead, showError, savePrefs, applyTheme, toast, button, openDialog } from "../core.js";

const SWITCHES = {
  allow_edits: ["Allow edits.", "Your assistant may write to connections set to read-write, and only after you approve each change set. When off, every connection is read-only."],
  allow_scripts: ["Allow Python scripts.", "Your assistant may run Python scripts it writes, after you read and approve each one. They run in a sandbox with no network."],
  allow_personal_tools: ["Allow personal tools.", "Your assistant may propose tools made from its scripts, and use the ones you install without asking each time. They run in the same sandbox as scripts."],
  allow_shared_tools: ["Allow shared tools and workflows.", "You may open tools and workflows coworkers share with you as a file. Each arrives as a proposal you read and approve yourself, like your assistant's; nothing from a shared file is installed or run before that."],
  allow_workflows: ["Allow workflows.", "Your assistant may propose workflows, and the ones you approve run by themselves on their schedule, without asking each time, within what they list."],
};

/** One of the organization's switches. Turning one on is confirmed by the
 * person (Touch ID, password or Windows Hello) in the app's Rust code. */
export function switchRow(name, st, after) {
  const p = st.policy;
  const box = h("input", { type: "checkbox", id: name });
  box.checked = !!p[name];
  box.disabled = !p.writable;
  const [b, text] = SWITCHES[name];
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

export async function render(main, rest) {
  const sub = rest[0] || "ai";
  const page = h("div", { class: "page" });
  main.append(page);
  const titles = { ai: "Models and accounts", general: "General", privacy: "Privacy and sandbox", keyboard: "Keyboard", environments: "Environments", org: "Organization" };
  page.append(pageHead(titles[sub] || "Settings", {
    ai: "The AI the app's chat uses. Columbus's rules are the same whichever AI you use.",
    general: "How the app looks, and about this version.",
    privacy: "What stays on this computer, and where files live.",
    keyboard: "Shortcuts in the main window.",
    environments: "Python interpreters your assistant's scripts may run in, besides the GIS engine's own, such as an ArcGIS Pro clone. You register and confirm each one; your assistant cannot.",
    org: "The policy in force on this computer, and the switches your organization lets you change. Your assistant cannot change any of these.",
  }[sub]));
  const body = h("div", { class: "pad stack" });
  page.append(h("div", { class: "scroll" }, body));
  const fn = { ai, general, privacy, keyboard, environments, org }[sub] || ai;
  return fn(body);
}

// What the chat's server and engine are, in plain words.
export function serverLines(s) {
  if (!s.server_found) return ["No Columbus server was found: unpack the Columbus folder, or enter where columbus-mcp is."];
  const v = s.server_version ? `version ${s.server_version}` : "version unknown";
  const out = [s.server_bundled ? `New chats use the Columbus server that comes with the app (${v}).` : `New chats use ${s.server_in_use} (${v}).`];
  if (s.server_older) out.push(`It is older than this app (${s.app_version}): the map beside a chat and newer tools may not work. Install this version's app again.`);
  out.push(s.engine ? `GIS engine: ${s.engine}` : "The GIS engine was not found in your Columbus folder: set it up with the install guide's engine step, or enter where its Python is above.");
  return out;
}

// ---------- Models and accounts ----------
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
    h("section", { class: "card" }, h("div", { class: "hd" }, h("h2", {}, "API key or local model"), h("span", { class: "chip blue more" }, "In use for chats")),
      h("div", { class: "bd stack" },
        h("p", { class: "small muted" }, "The chat uses an Anthropic API key, billed by Anthropic per use, separately from a Claude subscription. Create one at console.anthropic.com. It is kept in your computer's credential store, never in a file, and this window can set or remove it but never read it back."),
        keyForm, prefs)),
    h("section", { class: "card" }, h("div", { class: "hd" }, h("h2", {}, "Work account"), h("span", { class: "badge mute more" }, "Coming")),
      h("div", { class: "bd" }, h("p", { class: "small muted" }, "GitHub Copilot, ChatGPT Enterprise, Azure OpenAI or a company AI gateway, signed in with your work identity and pre-filled by your organization's org.json."))),
    h("section", { class: "card" }, h("div", { class: "hd" }, h("h2", {}, "Agent app"), h("span", { class: "badge mute more" }, "Coming")),
      h("div", { class: "bd" }, h("p", { class: "small muted" }, "Claude Code, Gemini CLI, GitHub Copilot CLI or Codex as the chat's brain, with Columbus's tools and their own file and shell tools turned off. Today you can already use Columbus from Claude desktop and other AI apps directly; their sessions show under Chats."))));

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

// ---------- General ----------
async function general(body) {
  const theme = h("div", { class: "seg", role: "group", "aria-label": "Theme" });
  for (const [v, t] of [["system", "Follow the system"], ["light", "Light"], ["dark", "Dark"]]) {
    const b = h("button", { type: "button", "aria-pressed": String((store.prefs.theme || "system") === v) }, t);
    b.addEventListener("click", async () => {
      await savePrefs({ theme: v });
      applyTheme(v);
      for (const x of theme.children) x.setAttribute("aria-pressed", String(x === b));
    });
    theme.append(b);
  }
  const basemap = h("input", { type: "checkbox", id: "pref-basemap" });
  basemap.checked = !!store.prefs.basemap;
  basemap.addEventListener("change", () => savePrefs({ basemap: basemap.checked }));
  const info = store.info;
  body.append(
    h("section", { class: "card" }, h("div", { class: "hd" }, h("h2", {}, "Appearance")), h("div", { class: "bd stack" },
      h("div", { class: "row wrap" }, h("span", { class: "strong" }, "Theme"), theme),
      h("label", { class: "switch-row" }, basemap, h("span", {}, h("b", {}, "Street basemap on by default."), " When on, the map in the output panel fetches street tiles from OpenFreeMap, which sends that site the area in view. Off until you turn it on; your organization's policy can remove it. You can still switch it per map.")))),
    h("section", { class: "card", id: "about" }, h("div", { class: "hd" }, h("h2", {}, "About")), h("div", { class: "bd" }, h("dl", { class: "kv" },
      h("dt", {}, "Columbus app"), h("dd", {}, info ? `version ${info.version}` : "—"),
      h("dt", {}, "Sessions running"), h("dd", {}, String(store.sessions.length)),
      h("dt", {}, "Licence"), h("dd", {}, "Apache-2.0"),
      h("dt", {}, "Telemetry"), h("dd", {}, "None. The app sends nothing about you anywhere.")))),
    h("section", { class: "card" }, h("div", { class: "hd" }, h("h2", {}, "Notifications")), h("div", { class: "bd" }, h("p", { class: "small muted" }, "One notification per batch of requests, with quiet hours, comes with the approvals inbox's next stage. Today the main window shows counts in the rail and a message when your chat asks you."))));
}

// ---------- Privacy and sandbox ----------
async function privacy(body) {
  const info = store.info;
  const p = info ? info.places : null;
  body.append(
    h("section", { class: "card" }, h("div", { class: "hd" }, h("h2", {}, "What stays on this computer")), h("div", { class: "bd stack" },
      h("p", {}, "The app sends nothing about you anywhere (no telemetry). The chat sends your messages, and the results its tools return, to the AI service you set up under Models and accounts; at most the rows your organization's policy allows per call."),
      h("p", {}, "Your chats are saved on this computer, in the app's data folder (one file per chat), so you can find and search them. Delete a chat from its page to delete its file."),
      h("p", {}, "The map's basemap, when you turn it on, fetches tiles from OpenFreeMap, which sends that site the area in view."))),
    h("section", { class: "card" }, h("div", { class: "hd" }, h("h2", {}, "The sandbox")), h("div", { class: "bd" },
      h("p", {}, "Scripts and your own tools run confined: no network, a fresh folder each run, reading only the layers they are given and writing only their own folder. Columbus's server enforces this, not the app."))),
    h("section", { class: "card" }, h("div", { class: "hd" }, h("h2", {}, "Where files live")), h("div", { class: "bd" }, p ? h("dl", { class: "kv" },
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
    [mac ? "⌘ K" : "Ctrl K", "Search chats, sessions and pages"],
    ["Enter", "Send a message (Shift Enter for a new line)"],
    ["Esc", "Close a dialog, the search or the map sheet"],
    ["Tab / Shift Tab", "Move between controls; every control can be reached"],
    ["← →", "Move between the output panel's tabs"],
  ];
  const tbody = h("tbody", {}, ...rows.map(([k, t]) => h("tr", {}, h("td", { class: "nowrap" }, h("span", { class: "kbd" }, k)), h("td", {}, t))));
  body.append(h("section", { class: "card" }, h("table", { class: "t" }, h("thead", {}, h("tr", {}, h("th", {}, "Keys"), h("th", {}, "Does"))), tbody)),
    h("p", { class: "small muted" }, "Approval cards in your chat are answered with their buttons, which wait a second before they work so a keystroke meant for something else cannot approve a request. Editable shortcuts come later."));
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
  const none = h("p", { id: "no-env", class: "pad muted", hidden: true }, "No environments yet. Scripts run in the GIS engine's own Python.");
  const waiting = h("ul", { id: "env-proposals", class: "list", "aria-live": "polite" });
  const noneWaiting = h("p", { id: "no-proposals", class: "pad muted small", hidden: true }, "Nothing waiting. When your assistant asks for a new environment (propose_environment), it waits here for you.");
  const conf = h("p", { id: "env-confirm-note", class: "small muted" });
  const foot = h("p", { class: "small muted wrap-any" }, "Registrations are kept in ", h("code", { id: "env-dir" }), ".");
  const add = button("Add environment", "primary", () => openAdd(), { id: "env-add" });
  body.append(error, notice, off,
    h("section", { class: "card" }, h("div", { class: "hd" }, h("h2", {}, "Registered"), h("span", { class: "more" }, add)), h("div", { class: "bd flush" }, list, none)),
    h("section", { class: "card" }, h("div", { class: "hd" }, h("h2", {}, "Asked for by your assistant")), h("div", { class: "bd flush" }, waiting, noneWaiting)),
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
function sentences(f) {
  const out = [];
  const list = (a) => (a && a.length ? a.join(", ") : "none");
  out.push(["Connections", `Connection types allowed: ${list(f.connection_types)}.`]);
  out.push(["Edits", f.allow_edits ? "Your assistant may propose edits to read-write connections; you approve each change set." : "No edits: every connection is read-only."]);
  out.push(["Scripts", f.allow_scripts ? "Your assistant may run Python scripts, after you read and approve each one." : "No Python scripts."]);
  out.push(["Personal tools", f.allow_personal_tools ? "Tools you install from what your assistant proposes may run without asking." : "Personal tools are off."]);
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
  const drawSw = async () => {
    try {
      const s = await invoke("settings_state");
      sw.replaceChildren(
        h("p", { id: "org-managed", class: "small muted", hidden: s.policy.writable }, "These are managed by your organization. Ask your IT team to change them."),
        ...Object.keys(SWITCHES).map((n) => switchRow(n, s, drawSw)),
        h("p", { class: "small muted" }, `Turning one on asks for ${st.confirm_with} first; turning one off does not. The rest of the file, comments included, is kept as it is.`));
    } catch (e) { showError(error, e); }
  };
  await drawSw();
  body.append(h("section", { class: "card" }, h("div", { class: "hd" }, h("h2", {}, "Switches")), h("div", { class: "bd" }, sw)));
  if (info) {
    const dl = h("dl", { class: "kv" });
    for (const [k, v] of sentences(info.policy.in_force || {})) dl.append(h("dt", {}, k), h("dd", {}, v));
    body.append(h("section", { class: "card" }, h("div", { class: "hd" }, h("h2", {}, "In plain words")), h("div", { class: "bd" }, dl)));
    if (info.policy.text) {
      body.append(h("section", { class: "card" }, h("div", { class: "hd" }, h("h2", {}, "The file as written"), h("span", { class: "muted small more" }, "read only")),
        h("div", { class: "bd" }, el("pre", "box", info.policy.text))));
    }
  }
  body.append(h("p", { class: "small muted" }, "The history of settings changes (what each reload withdrew) and audit health come later; the audit log itself is under ", h("a", { href: "#/activity/audit" }, "Activity › Audit log"), "."));
}
