// Home: what needs the person, and the first steps when nothing is set up.
import { h, invoke, store, button, pageHead, plural, ago, clientName, savePrefs, showError } from "../core.js";
import { recent, otherInbox, waitingCount } from "./chats.js";
import { openEditor } from "./connections.js";

const STEPS = ["ai", "engine", "connection"];

export async function render(main) {
  const body = h("div", { class: "pad stack" });
  main.append(h("div", { class: "page" },
    pageHead("Home", "What needs you, across your chats and the AI apps that use Columbus on this computer.",
      [h("a", { class: "btn primary", href: "#/chats/new" }, "New chat")]),
    h("div", { class: "scroll" }, body)));

  const [chat, st, notes, engine] = await Promise.all([
    invoke("chat_settings").catch(() => null),
    invoke("settings_state").catch(() => null),
    invoke("notes_list").catch(() => null),
    invoke("engine_check").catch(() => null),
  ]);
  const done = {
    ai: !!(chat && chat.has_key && chat.server_found),
    engine: !!(engine && engine.server_found),
    connection: !!(st && st.connections.some((c) => c.id)),
  };
  const skipped = new Set(store.prefs.skipped || []);
  const needsSetup = !store.prefs.setup_done && STEPS.some((s) => !done[s] && !skipped.has(s));

  // Something changed that the person should know.
  const info = store.info;
  if (info && info.policy.problem) {
    body.append(h("div", { class: "notice fail", role: "alert" }, h("div", {}, h("b", {}, "Your organization's settings file cannot be read, "),
      "so Columbus allows nothing beyond its defaults until it is fixed: ", info.policy.problem, " ", h("a", { href: "#/settings/org" }, "See Organization"))));
  }
  if (st && st.other_folders && st.other_folders.length) {
    body.append(h("div", { class: "notice warn" }, h("div", {}, `A running Columbus session reads its connections from ${st.other_folders.join(", ")}, not the folder this app edits.`)));
  }

  if (needsSetup) body.append(firstRun(done, skipped, chat, engine, main));

  const grid = h("div", { class: "home-grid" });
  const left = h("div", { class: "stack" });
  const right = h("div", { class: "stack" });
  grid.append(left, right);
  body.append(grid);

  // What needs the person.
  const waitCard = h("section", { class: "card", "aria-labelledby": "h-wait" });
  const drawWait = () => {
    const groups = otherInbox().filter((s) => (s.pending || []).length);
    const own = waitingCount();
    const n = groups.reduce((k, s) => k + s.pending.length, 0) + own;
    waitCard.replaceChildren(h("div", { class: "hd" }, h("h2", { id: "h-wait" }, "Waiting for you"), h("a", { class: "btn sm more", href: "#/activity/approvals" }, "Open approvals")));
    const bd = h("div", { class: "bd stack" });
    if (!n) bd.append(h("p", { class: "muted" }, "No requests are waiting. Requests from your AI apps appear here as they come."));
    else {
      bd.append(h("p", { class: "stat" }, h("b", {}, String(n)), plural(n, "request").replace(/^\d[\d,]* /, ""), " waiting"));
      const ul = h("ul", { class: "list" });
      if (own) ul.append(h("li", {}, h("div", { class: "text" }, h("b", {}, "Your chat in this app"), h("span", { class: "where" }, `${plural(own, "request")}, answered in the chat`)), h("a", { class: "btn sm", href: "#/chats/new" }, "Answer")));
      for (const s of groups) {
        const kinds = {};
        for (const a of s.pending) kinds[a.risk] = (kinds[a.risk] || 0) + 1;
        ul.append(h("li", {}, h("div", { class: "text" }, h("b", {}, `${clientName(sessionClient(s.session))} session`),
          h("span", { class: "where" }, Object.entries(kinds).map(([k, v]) => plural(v, { notes: "note change", export: "export", script: "script", edit: "edit set", download: "download", fetch: "download", tool: "tool" }[k] || k)).join(" · "))),
          h("a", { class: "btn sm", href: "#/activity/approvals" }, "Review")));
      }
      bd.append(ul);
    }
    waitCard.append(bd);
  };
  drawWait();
  left.append(waitCard);

  // Recent chats.
  const rc = h("section", { class: "card", "aria-labelledby": "h-recent" }, h("div", { class: "hd" }, h("h2", { id: "h-recent" }, "Recent chats and sessions"), h("a", { class: "btn sm more", href: "#/chats" }, "All chats")));
  const rl = h("ul", { class: "list" });
  const items = recent(6);
  for (const c of items) rl.append(h("li", { class: "clickable", onclick: () => { location.hash = c.href; } }, h("div", { class: "text" }, h("b", {}, c.title), h("span", { class: "where" }, `${c.kind} · ${c.sub}`)), h("a", { class: "btn sm", href: c.href }, "Open")));
  rc.append(h("div", { class: "bd flush" }, items.length ? rl : h("div", { class: "pad" }, h("p", { class: "muted" }, "No chats yet."))));
  left.append(rc);

  // Workflows that need the person: waiting to be approved, or a last run
  // that failed or stopped for them.
  const wf = store.workflows;
  if (wf && (wf.waiting.length || wf.installed.length)) {
    const bad = wf.installed.filter((w) => w.last_run && ["failed", "needs_you", "needs_reapproval", "stopped_at_limit"].includes(w.last_run.status));
    const words = { failed: "failed", needs_you: "needs you", needs_reapproval: "needs re-approval", stopped_at_limit: "stopped at its time limit" };
    const ul = h("ul", { class: "list" });
    for (const w of bad) ul.append(h("li", {}, h("div", { class: "text" }, h("b", {}, w.name), h("span", { class: "where" }, `Last run ${words[w.last_run.status]}${w.last_run.started ? `, ${ago(w.last_run.started)}` : ""}`)), h("a", { class: "btn sm", href: `#/workflows/${encodeURIComponent(w.id)}` }, "See run")));
    for (const w of wf.waiting) ul.append(h("li", {}, h("div", { class: "text" }, h("b", {}, w.name), h("span", { class: "where" }, "Waiting for you to approve")), h("a", { class: "btn sm", href: "#/workflows" }, "Review")));
    right.append(h("section", { class: "card", "aria-labelledby": "h-wf" },
      h("div", { class: "hd" }, h("h2", { id: "h-wf" }, "Workflows"), h("a", { class: "btn sm more", href: "#/workflows" }, "All workflows")),
      h("div", { class: "bd flush" }, ul.childNodes.length ? ul : h("div", { class: "pad" }, h("p", { class: "muted" }, `${plural(wf.installed.length, "workflow")} approved; their last runs went fine.`)))));
  }

  // Notes in draft.
  const drafts = notes ? notes.notes.filter((n) => n.status === "draft") : [];
  right.append(h("section", { class: "card", "aria-labelledby": "h-notes" },
    h("div", { class: "hd" }, h("h2", { id: "h-notes" }, "Notes in draft"), h("a", { class: "btn sm more", href: "#/documents/notes" }, "Review")),
    h("div", { class: "bd" }, drafts.length
      ? h("p", { class: "stat" }, h("b", {}, String(drafts.length)), drafts.length === 1 ? "note waits for your review" : "notes wait for your review")
      : h("p", { class: "muted" }, notes && notes.notes.length ? "All your notes are reviewed." : "No notes yet. Draft them from a connection under Connections."))));

  // Tools waiting.
  const tw = store.tools ? store.tools.waiting.length : 0;
  if (tw) {
    right.append(h("section", { class: "card" }, h("div", { class: "hd" }, h("h2", {}, "Tools to review"), h("a", { class: "btn sm more", href: "#/tools" }, "Review")),
      h("div", { class: "bd" }, h("p", {}, `${plural(tw, "tool")} your assistant proposed ${tw === 1 ? "waits" : "wait"} for you to read and install or decline.`))));
  }

  // What is set up.
  right.append(h("section", { class: "card" }, h("div", { class: "hd" }, h("h2", {}, "This computer")),
    h("div", { class: "bd" }, h("dl", { class: "kv" },
      h("dt", {}, "AI for chats"), h("dd", {}, done.ai ? "Connected (API key)" : "Not set up"),
      h("dt", {}, "Connections"), h("dd", {}, st ? plural(st.connections.filter((c) => c.id).length, "connection") : "—"),
      h("dt", {}, "Sessions running"), h("dd", {}, String(store.sessions.length)),
      h("dt", {}, "Policy"), h("dd", {}, info ? { organization: "your organization's (managed)", file: "this computer's file", defaults: "Columbus defaults (no file)", unreadable: "unreadable" }[info.policy.source] : "—"),
      h("dt", {}, "Workflows"), h("dd", {}, store.workflows ? (store.workflows.allowed ? `${store.workflows.installed.length} approved, ${store.workflows.installed.filter((x) => x.schedule_entry === "on").length} on a schedule` : "Off (organization settings)") : "—")))));

  if (!needsSetup && STEPS.some((s) => !done[s])) {
    right.append(h("p", { class: "small muted" }, "Setup steps you skipped are in Settings. ",
      h("button", { type: "button", class: "linkbtn", onclick: async () => { await savePrefs({ skipped: [], setup_done: false }); location.reload(); } }, "Show the first steps again")));
  }

  const unsub = store.on((k) => { if (k === "inbox" || k === "sessions") drawWait(); });
  return () => unsub();
}

function sessionClient(id) {
  return (store.sessions.find((s) => s.session === id) || {}).client;
}

/** The first steps, every one skippable (design 5.1). */
function firstRun(done, skipped, chat, engine, main) {
  const card = h("section", { class: "card", "aria-labelledby": "h-setup" });
  card.append(h("div", { class: "hd" }, h("h2", { id: "h-setup" }, "Welcome to Columbus: first steps"),
    h("span", { class: "muted small" }, "about 5 minutes; skip anything and find it again in Settings"),
    button("Skip setup", "sm ghost more", async () => { await savePrefs({ setup_done: true }); main.replaceChildren(); render(main); })));
  const ol = h("ol", { class: "steps-list" });
  const skip = (k) => button("Skip", "sm", async () => { await savePrefs({ skipped: [...new Set([...(store.prefs.skipped || []), k])] }); main.replaceChildren(); render(main); });
  const state = (k) => (done[k] ? "done" : skipped.has(k) ? "skipped" : "");

  // 1. Connect an AI (run by the app itself: no AI is connected yet).
  const err1 = h("p", { class: "error-text", role: "alert", hidden: true });
  const key = h("input", { type: "password", id: "fr-key", autocomplete: "off", spellcheck: "false", placeholder: "sk-ant-…", "aria-label": "Anthropic API key", class: "grow" });
  const s1 = h("div", { class: "body" }, h("h3", {}, "Connect an AI"),
    h("p", { class: "small muted" }, "The app's chat uses an Anthropic API key, billed by Anthropic per use (separately from a Claude subscription). It is kept in your computer's credential store, never in a file, and this app can never read it back. Work accounts and agent apps come later. You can also keep using Columbus from Claude desktop without this."));
  if (done.ai) s1.append(h("p", { class: "small" }, "Connected. Change it in ", h("a", { href: "#/settings/ai" }, "Settings › Models and accounts"), "."));
  else if (chat && chat.has_key && !chat.server_found) s1.append(h("p", { class: "small" }, "A key is saved, but Columbus's server was not found. Set where it is in ", h("a", { href: "#/settings/ai" }, "Settings › Models and accounts"), "."));
  else {
    s1.append(h("form", { class: "row wrap", onsubmit: async (e) => {
      e.preventDefault();
      showError(err1, null);
      try { await invoke("chat_set_key", { key: key.value }); key.value = ""; main.replaceChildren(); render(main); } catch (x) { showError(err1, x); }
    } }, key, h("button", { type: "submit", class: "btn primary" }, "Save key"), skip("ai")), err1);
  }
  ol.append(h("li", { class: state("ai") }, h("span", { class: "n", "aria-hidden": "true" }, done.ai ? "✓" : "1"), s1));

  // 2. The GIS engine: checked, nothing run.
  const s2 = h("div", { class: "body" }, h("h3", {}, "A ready GIS engine"));
  if (engine && engine.server_found) {
    s2.append(h("p", { class: "small" }, `Columbus's server is at ${engine.server}. `, engine.engine_found ? `Its Python engine is at ${engine.engine}.` : "Its Python engine environment was not found beside it: run engine\\setup-engine.ps1 (Windows) or follow the install guide, then check again."));
  } else {
    s2.append(h("p", { class: "small muted" }, "Columbus's server (columbus-mcp) was not found in the usual places. Unpack the Columbus folder as the install guide shows, or set where it is in Settings › Models and accounts."));
  }
  s2.append(h("div", { class: "row wrap" }, button("Check again", "sm", () => { main.replaceChildren(); render(main); }), done.engine ? null : skip("engine")));
  ol.append(h("li", { class: state("engine") }, h("span", { class: "n", "aria-hidden": "true" }, done.engine ? "✓" : "2"), s2));

  // 3. A first read-only connection, tested before saving.
  const s3 = h("div", { class: "body" }, h("h3", {}, "A first read-only connection"),
    h("p", { class: "small muted" }, "A GeoPackage, a folder of rasters or a database. Test it before saving: Test opens it as Columbus would and saves nothing. Adding one asks you to confirm at the keyboard."));
  if (!done.connection) s3.append(h("div", { class: "row wrap" }, button("Add a read-only connection", "sm primary", () => openEditor(null, () => { main.replaceChildren(); render(main); })), skip("connection")));
  else s3.append(h("p", { class: "small" }, "Done. See ", h("a", { href: "#/connections" }, "Connections"), "."));
  ol.append(h("li", { class: state("connection") }, h("span", { class: "n", "aria-hidden": "true" }, done.connection ? "✓" : "3"), s3));

  card.append(h("div", { class: "bd flush" }, ol));
  card.append(h("div", { class: "bd" }, h("p", { class: "small muted" }, "Coming later in setup: offering the ArcPy worker when ArcGIS Pro is found, an inventory of the GIS engines installed here, and importing your SOPs.")));
  return card;
}
