// What a new chat shows before anything is asked: the first steps when
// nothing is set up, and what changed that the person should know. (Home's
// parts, moved here: the app opens on a question now.)
import { h, put, icon, invoke, store, button, savePrefs, showError } from "../core.js";
import { openEditor } from "./connections.js";
import { workMode, COPILOT_LINE } from "./settings.js";

const STEPS = ["ai", "engine", "connection"];

/** What is set up, and whether the first steps should show. */
export async function setupState() {
  const [chat, st, engine] = await Promise.all([
    invoke("chat_settings").catch(() => null),
    invoke("settings_state").catch(() => null),
    invoke("engine_check").catch(() => null),
  ]);
  const done = {
    ai: !!(chat && chat.ready && chat.server_found),
    engine: !!(engine && engine.server_found),
    connection: !!(st && st.connections.some((c) => c.id)),
  };
  const skipped = new Set(store.prefs.skipped || []);
  const needsSetup = !store.prefs.setup_done && STEPS.some((s) => !done[s] && !skipped.has(s));
  return { chat, st, engine, done, skipped, needsSetup, anyLeft: STEPS.some((s) => !done[s]) };
}

/** Something changed that the person should know (nodes, possibly none). */
export function notices(st) {
  const out = [];
  const info = store.info;
  if (info && info.policy.problem) {
    out.push(h("div", { class: "notice fail", role: "alert" }, h("div", {}, h("b", {}, "Your organization's settings file cannot be read, "),
      "so Columbus allows nothing beyond its defaults until it is fixed: ", info.policy.problem, " ", h("a", { href: "#/settings/org" }, "See Organization"))));
  }
  if (st && st.other_folders && st.other_folders.length) {
    out.push(h("div", { class: "notice warn" }, h("div", {}, `A running Columbus session reads its connections from ${st.other_folders.join(", ")}, not the folder this app edits.`)));
  }
  return out;
}

/** The first run: start with your data, and two checks under it (an AI for
 * the chat, the GIS engine). Every step can be skipped (design 5.1).
 * `again` draws the start afresh after a step is done or skipped. */
export function firstSteps(s, again) {
  const { done, skipped, chat, engine } = s;
  const box = h("section", { class: "welcome", id: "first-steps", "aria-labelledby": "h-setup" });
  const skip = (k) => button("Skip", "ghost sm", async () => { await savePrefs({ skipped: [...new Set([...(store.prefs.skipped || []), k])] }); again(); });
  const mark = (k) => h("span", { class: done[k] ? "check done" : skipped.has(k) ? "check skipped" : "check", "aria-hidden": "true" }, done[k] ? icon("check") : null);

  // Your data first: a read-only connection, tested before it is saved.
  box.append(h("h2", { id: "h-setup", class: "hero" }, "Start with your data."),
    h("p", { class: "lead" }, "Point Columbus at a file, a folder, a database or an ArcGIS layer. You decide what your assistant may do with it before anything is read."));
  if (done.connection) box.append(h("p", { class: "small" }, "Your first connection is added. See ", h("a", { href: "#/data" }, "Data"), "."));
  else {
    box.append(h("div", { class: "row wrap" }, button("Add data", "primary", () => openEditor(null, () => again()), { id: "fr-add" }), skipped.has("connection") ? null : skip("connection")),
      h("p", { class: "small muted" }, "Test opens it as Columbus would and saves nothing. Adding one asks you to confirm at the keyboard."));
  }

  const checks = h("ul", { class: "checks" });
  // An AI for the chat (run by the app itself: no AI is connected yet):
  // which service, and its key where it takes one. The same choice, in
  // full, is in Settings › Assistant.
  const err1 = h("p", { id: "fr-ai-error", class: "error-text small", role: "alert", hidden: true });
  const ai = h("div", { class: "text" });
  const svc = chat ? chat.services.find((v) => v.id === chat.service) : null;
  if (done.ai) ai.append(h("b", {}, `${chat.service_name} is connected for the chat.`), h("span", { class: "where" }, "Change it in ", h("a", { href: "#/settings/ai" }, "Settings › Assistant"), "."));
  else if (chat && chat.ready && !chat.server_found) ai.append(h("b", {}, "An AI is set up, but Columbus's server was not found."), h("span", { class: "where" }, "See the check below."));
  else if (!chat) ai.append(h("b", {}, "Connect an AI for the chat"), h("span", { class: "where" }, "Set it up in ", h("a", { href: "#/settings/ai" }, "Settings › Assistant"), "."));
  else {
    const pick = h("select", { id: "fr-service", "aria-label": "AI service" }, ...chat.services.map((v) => {
      const o = h("option", { value: v.id }, `${v.name}${v.allowed ? "" : " (not allowed by your organization)"}`);
      o.selected = v.id === chat.service;
      return o;
    }));
    pick.addEventListener("change", async () => {
      showError(err1, null);
      try { await invoke("chat_use_service", { service: pick.value }); again(); } catch (x) { showError(err1, x); pick.value = chat.service; }
    });
    const takesKey = svc && svc.key !== "none" && !chat.no_store;
    const key = h("input", { type: "password", id: "fr-key", autocomplete: "off", spellcheck: "false", placeholder: "Paste the key here", "aria-label": `API key for ${chat.service_name}`, class: "grow" });
    const form = h("form", { class: "row wrap", id: "fr-ai-form", onsubmit: async (e) => {
      e.preventDefault();
      showError(err1, null);
      try { await invoke("chat_set_key", { key: key.value, service: chat.service }); key.value = ""; again(); } catch (x) { showError(err1, x); }
    } }, pick, takesKey ? key : null, takesKey ? h("button", { type: "submit", class: "btn" }, "Save key") : null, skipped.has("ai") ? null : skip("ai"));
    // What is still missing, in a line: a key, a model, or the organization's leave.
    const need = svc && !svc.allowed ? svc.why_not
      : chat.key_needed && !chat.has_key ? (chat.no_store || "")
        : !chat.model ? `${chat.service_name} is chosen. Choose its model in Settings › Assistant: Columbus has no list of what is on your computer.`
          : "";
    // (`put`, not `append`: with nothing missing there is no line to add.)
    put(ai, h("b", {}, "Connect an AI for the chat"), form, err1,
      need ? h("span", { class: "where", id: "fr-ai-need" }, need, !chat.model && svc && svc.allowed ? [" ", h("a", { href: "#/settings/ai" }, "Open Settings › Assistant")] : null) : null,
      h("span", { class: "where", id: "fr-ai-about" }, svc ? svc.about : "", " ",
        takesKey ? "The key is kept in your computer's credential store, never in a file, and this app can never read it back. " : "",
        // GitHub Copilot is asked for by name and is not in the list.
        h("span", { id: "fr-ai-copilot" }, COPILOT_LINE, " Claude desktop and other AI apps can use Columbus the same way, without any of this: see ",
          h("a", { href: "#/settings/ai/copilot" }, "Use Columbus from another AI app"), ".")));
  }
  checks.append(h("li", {}, mark("ai"), ai));

  // The GIS engine: checked, nothing run.
  const eng = h("div", { class: "text" });
  if (engine && engine.server_found) {
    eng.append(h("b", {}, engine.engine_found ? "Found Columbus's server and its Python engine." : "Found Columbus's server, but not its Python engine."),
      h("span", { class: "where wrap-any" }, engine.engine_found ? `${engine.server} · ${engine.engine}` : "Run engine\\setup-engine.ps1 (Windows) or follow the install guide, then check again."));
    if (!engine.engine_found) eng.append(h("div", { class: "row" }, button("Check again", "sm", () => again()), skipped.has("engine") ? null : skip("engine")));
  } else {
    eng.append(h("b", {}, "Columbus's server was not found"),
      h("span", { class: "where" }, "Unpack the Columbus folder as the install guide shows, then check again."),
      h("div", { class: "row" }, button("Check again", "sm", () => again()), skipped.has("engine") ? null : skip("engine")));
  }
  checks.append(h("li", {}, mark("engine"), eng));
  // How the assistant works: freely on copies, or asking before each script
  // (left out when the settings could not be read: there is nothing to show).
  const mode = s.st ? h("div", { class: "stack", id: "fr-mode" }, h("h3", {}, "How should your assistant work?"), workMode(s.st, () => again(), "fr-work", false)) : null;
  put(box, checks, mode,
    h("div", { class: "row welcome-foot" }, h("span", { class: "small muted grow" }, "Your assistant sees only what your connections allow; anything that changes data or leaves this computer asks you first."),
      button("Skip setup", "ghost sm", async () => { await savePrefs({ setup_done: true }); again(); }, { id: "fr-skip" })));
  return box;
}
