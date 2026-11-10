// What a new chat shows before anything is asked: the first steps when
// nothing is set up, and what changed that the person should know. (Home's
// parts, moved here: the app opens on a question now.)
import { h, invoke, store, button, savePrefs, showError } from "../core.js";
import { openEditor } from "./connections.js";

const STEPS = ["ai", "engine", "connection"];

/** What is set up, and whether the first steps should show. */
export async function setupState() {
  const [chat, st, engine] = await Promise.all([
    invoke("chat_settings").catch(() => null),
    invoke("settings_state").catch(() => null),
    invoke("engine_check").catch(() => null),
  ]);
  const done = {
    ai: !!(chat && chat.has_key && chat.server_found),
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

/** The first steps, every one skippable (design 5.1). `again` draws the
 * start afresh after a step is done or skipped. */
export function firstSteps(s, again) {
  const { done, skipped, chat, engine } = s;
  const card = h("section", { class: "card", id: "first-steps", "aria-labelledby": "h-setup" });
  card.append(h("div", { class: "hd" }, h("h2", { id: "h-setup" }, "Welcome to Columbus: first steps"),
    button("Skip setup", "sm ghost more", async () => { await savePrefs({ setup_done: true }); again(); })));
  const ol = h("ol", { class: "steps-list" });
  const skip = (k) => button("Skip", "sm", async () => { await savePrefs({ skipped: [...new Set([...(store.prefs.skipped || []), k])] }); again(); });
  const state = (k) => (done[k] ? "done" : skipped.has(k) ? "skipped" : "");

  // 1. Connect an AI (run by the app itself: no AI is connected yet).
  const err1 = h("p", { class: "error-text", role: "alert", hidden: true });
  const key = h("input", { type: "password", id: "fr-key", autocomplete: "off", spellcheck: "false", placeholder: "sk-ant-…", "aria-label": "Anthropic API key", class: "grow" });
  const s1 = h("div", { class: "body" }, h("h3", {}, "Connect an AI"),
    h("p", { class: "small muted" }, "The app's chat uses an Anthropic API key, billed by Anthropic per use (separately from a Claude subscription). It is kept in your computer's credential store, never in a file, and this app can never read it back. Work accounts and agent apps come later. You can also keep using Columbus from Claude desktop without this."));
  if (done.ai) s1.append(h("p", { class: "small" }, "Connected. Change it in ", h("a", { href: "#/settings/ai" }, "Settings › Assistant"), "."));
  else if (chat && chat.has_key && !chat.server_found) s1.append(h("p", { class: "small" }, "A key is saved, but Columbus's server was not found. Set where it is in ", h("a", { href: "#/settings/ai" }, "Settings › Assistant"), "."));
  else {
    s1.append(h("form", { class: "row wrap", onsubmit: async (e) => {
      e.preventDefault();
      showError(err1, null);
      try { await invoke("chat_set_key", { key: key.value }); key.value = ""; again(); } catch (x) { showError(err1, x); }
    } }, key, h("button", { type: "submit", class: "btn primary" }, "Save key"), skip("ai")), err1);
  }
  ol.append(h("li", { class: state("ai") }, h("span", { class: "n", "aria-hidden": "true" }, done.ai ? "✓" : "1"), s1));

  // 2. The GIS engine: checked, nothing run.
  const s2 = h("div", { class: "body" }, h("h3", {}, "A ready GIS engine"));
  if (engine && engine.server_found) {
    s2.append(h("p", { class: "small" }, `Columbus's server is at ${engine.server}. `, engine.engine_found ? `Its Python engine is at ${engine.engine}.` : "Its Python engine environment was not found beside it: run engine\\setup-engine.ps1 (Windows) or follow the install guide, then check again."));
  } else {
    s2.append(h("p", { class: "small muted" }, "Columbus's server (columbus-mcp) was not found in the usual places. Unpack the Columbus folder as the install guide shows, or set where it is in Settings › Assistant."));
  }
  s2.append(h("div", { class: "row wrap" }, button("Check again", "sm", () => again()), done.engine ? null : skip("engine")));
  ol.append(h("li", { class: state("engine") }, h("span", { class: "n", "aria-hidden": "true" }, done.engine ? "✓" : "2"), s2));

  // 3. A first read-only connection, tested before saving.
  const s3 = h("div", { class: "body" }, h("h3", {}, "A first read-only connection"),
    h("p", { class: "small muted" }, "A GeoPackage, a folder of rasters or a database. Test it before saving: Test opens it as Columbus would and saves nothing. Adding one asks you to confirm at the keyboard."));
  if (!done.connection) s3.append(h("div", { class: "row wrap" }, button("Add a read-only connection", "sm primary", () => openEditor(null, () => again())), skip("connection")));
  else s3.append(h("p", { class: "small" }, "Done. See ", h("a", { href: "#/data" }, "Data"), "."));
  ol.append(h("li", { class: state("connection") }, h("span", { class: "n", "aria-hidden": "true" }, done.connection ? "✓" : "3"), s3));

  card.append(h("div", { class: "bd" }, ol));
  card.append(h("div", { class: "bd" }, h("p", { class: "small muted" }, "Coming later in setup: offering the ArcPy worker when ArcGIS Pro is found, an inventory of the GIS engines installed here, and importing your SOPs.")));
  return card;
}
