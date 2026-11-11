// A project's Sync section: link the project's files to a repository on a
// connected Git server, check for changes, sync, look at what changed,
// settle files that need a look, read the history, disconnect.
//
// Nothing is sent unless the person presses Sync, and a sync sends only
// the changes this section listed: the app's own code keeps what the last
// check showed, and anything that changed after it waits. Everything shown
// here that comes from a file, a server or a colleague is shown as text.
import { h, el, invoke, button, showError, toast, askConfirm, openDialog, ago, plural } from "../core.js";

const WHAT = { new: "new", changed: "changed", deleted: "deleted" };
const BOTH = {
  both_changed: "changed here and on the server",
  deleted_on_server: "changed here, deleted on the server",
  deleted_here: "deleted here, changed on the server",
};

/** Where a file that was taken out of the folder is kept, in words. */
const keptWhere = (k) => (k === "trash" ? "in your computer's trash" : k && k.folder ? `kept by Columbus in ${k.folder}` : "kept by Columbus");

/** Put the Sync section on a project's page, after what is there (the
 * project's page calls this after its settings and before its chats). */
export function mount(body, project) {
  const card = syncCard(project);
  body.append(card);
  return card;
}

function syncCard(project) {
  const id = project.id;
  const bd = h("div", { class: "bd stack", id: "sync-body" }, h("p", { class: "muted small" }, "Looking…"));
  const disconnect = button("Disconnect", "sm", onDisconnect, { id: "sync-disconnect", hidden: true });
  const card = h("section", { class: "card", id: "pj-sync" }, h("div", { class: "hd" }, h("h2", {}, "Sync"), h("span", { class: "more" }, disconnect)), bd);
  const error = h("p", { id: "sync-error", class: "error-text", role: "alert", hidden: true });
  const status = h("p", { id: "sync-status", class: "small", role: "status", "aria-live": "polite" });
  const result = h("div", { id: "sync-result", class: "stack" });
  const changesBox = h("div", { id: "sync-changes-box" });
  let view = null;
  let changes = null;
  let busy = false;

  const act = async (btn, label, fn) => {
    if (busy) return;
    busy = true;
    const was = btn ? btn.textContent : "";
    if (btn) { btn.disabled = true; btn.textContent = label; }
    showError(error, null);
    try { await fn(); } catch (e) { showError(error, e); } finally {
      busy = false;
      if (btn && btn.isConnected) { btn.disabled = false; btn.textContent = was; }
    }
  };

  function summary(c) {
    const parts = [];
    if (c.here.length) parts.push(`${c.here.length} changed here`);
    if (c.server.length) parts.push(`${c.server.length} on the server`);
    if (c.both.length) parts.push(`${c.both.length} on both sides`);
    return parts.length ? `${parts.join(", ")}.` : "Everything is in sync.";
  }

  // ---- What changed -----------------------------------------------------
  function fileRow(path, note, show) {
    const li = h("li", {}, h("div", { class: "text" }, h("b", { class: "wrap-any" }, path), h("span", { class: "where" }, note)));
    if (show) li.append(h("div", { class: "acts" }, button("Show", "sm", show, { "aria-label": `Show ${path}` })));
    li.dataset.path = path;
    return li;
  }

  function group(idName, title, about, rows) {
    if (!rows.length) return null;
    return h("div", { class: "stack" }, h("p", { class: "small" }, h("b", {}, `${title} (${rows.length}). `), about), h("ul", { class: "list compact", id: idName }, ...rows));
  }

  async function showPreview(path, labels) {
    const box = h("div", { class: "grid2", id: "sync-preview" }, h("p", { class: "muted small wide" }, "Looking…"));
    const close = h("button", { type: "button", class: "btn" }, "Close");
    const d = openDialog(path, box, [h("span", { class: "spacer" }), close], `What changed in ${path}`);
    d.id = "sync-preview-dialog";
    close.addEventListener("click", () => d.close());
    try {
      const p = await labels.load();
      const side = (title, s, idName) => h("div", { class: "stack" }, h("p", { class: "small" }, h("b", {}, title)),
        s.text != null ? h("pre", { class: "box", id: idName, tabindex: "0" }, s.text) : h("p", { class: "muted small", id: idName }, `(${s.note || "nothing"})`));
      box.replaceChildren(side(labels.left, labels.swap ? p.mine : p.theirs, "sync-preview-left"), side(labels.right, labels.swap ? p.theirs : p.mine, "sync-preview-right"));
    } catch (e) {
      box.replaceChildren(h("p", { class: "error-text wide", role: "alert" }, String(e)));
    }
  }

  const preview = (path) => () => invoke("sync_preview", { project: id, path });

  function drawChanges() {
    changesBox.replaceChildren();
    if (!changes) return;
    const c = changes;
    const here = c.here.map((i) => fileRow(i.path, WHAT[i.what], () => showPreview(i.path, { left: "On the server now", right: "Here (what a sync sends)", load: preview(i.path) })));
    const server = c.server.map((i) => fileRow(i.path, WHAT[i.what], () => showPreview(i.path, { left: "Here now", right: "On the server (what a sync gets)", swap: true, load: preview(i.path) })));
    const both = c.both.map((i) => fileRow(i.path, BOTH[i.kind], () => showPreview(i.path, { left: "The server's", right: "Yours", load: preview(i.path) })));
    const n = here.length + server.length + both.length;
    const skipped = c.skipped.length ? h("details", { id: "sync-skipped" }, h("summary", { class: "small" }, `Not syncing (${c.skipped.length})`),
      h("ul", { class: "small" }, ...c.skipped.map((s) => el("li", "wrap-any", `${s.path}${s.on_server ? " (on the server)" : ""}: ${s.why}`)))) : null;
    if (!n && !skipped) return;
    const details = h("details", { id: "sync-changes", class: "stack" }, h("summary", { class: "small" }, n ? `What changed (${n})` : "What changed"),
      group("sync-here", "Changed here", "A sync sends these, as they are now.", here),
      group("sync-there", "Changed on the server", "A sync gets these.", server),
      group("sync-both", "Changed on both sides", "A sync keeps both versions and lists the file under Needs a look, unless the two turn out to hold the same.", both),
      skipped);
    details.open = n > 0;
    changesBox.append(details);
  }

  // ---- Needs a look -----------------------------------------------------
  function lookList() {
    if (!view.needs_look.length) return null;
    const list = h("div", { class: "stack", id: "sync-look", role: "list" });
    for (const c of view.needs_look) {
      const say = c.kind === "both_changed" ? `Changed here and on the server. The server's version has the name; yours is kept beside it as ${c.copy}.`
        : c.kind === "deleted_on_server" ? `You changed it; on the server it was deleted. Your version is kept as ${c.copy}.`
          : "You deleted it; on the server it was changed. The server's version is back in the folder.";
      const settle = (choice, done) => async (ev) => act(ev.currentTarget, "Working…", async () => {
        const r = await invoke("sync_resolve", { project: id, path: c.path, copy: c.copy || null, choice });
        view = r.view;
        toast(done(r.kept ? ` ${keptWhere(r.kept)}` : ""));
        draw();
        await doCheck();
      });
      const gone = c.kind === "deleted_here";
      const item = h("div", { class: "notice warn", role: "listitem" }, h("div", { class: "stack" },
        h("p", {}, h("b", { class: "wrap-any" }, c.path)),
        h("p", { class: "wrap-any" }, say),
        h("p", {}, gone ? "Keep mine deletes it again (your next sync sends that). Keep theirs keeps the file."
          : "Keep mine puts your version back under the name (your next sync sends it). Keep theirs removes your copy. Keep both leaves your copy as a file of its own."),
        h("div", { class: "row wrap" },
          button("Keep mine", "sm", settle("mine", (kept) => (gone ? `Deleted ${c.path} again${kept ? `; the server's version is${kept}` : ""}. Your next sync sends that.`
            : `Your version of ${c.path} has its name back. Your next sync sends it.${kept ? ` The version that had the name is${kept}.` : ""}`)), { "aria-label": `Keep mine: ${c.path}` }),
          button("Keep theirs", "sm", settle("theirs", (kept) => (gone ? `Kept ${c.path}.` : `Kept the server's ${c.path}.${kept ? ` Your copy was taken out of the folder, not erased: it is${kept}.` : ""}`)), { "aria-label": `Keep theirs: ${c.path}` }),
          gone ? null : button("Keep both", "sm", settle("both", () => `Kept both. ${c.copy} is a file of its own now.`), { "aria-label": `Keep both: ${c.path}` }),
          gone ? null : button("Show both", "sm", () => showPreview(c.path, { left: "The server's (has the name)", right: "Yours (the copy)", load: () => invoke("sync_preview_kept", { project: id, path: c.path, copy: c.copy || null }) }), { "aria-label": `Show both: ${c.path}` }))));
      item.dataset.path = c.path;
      list.append(item);
    }
    return h("div", { class: "stack", id: "sync-look-box" }, h("p", { class: "small" }, h("b", {}, `Needs a look (${view.needs_look.length}). `),
      "These files were changed on both sides. Both versions were kept as they are; neither was overwritten."), list);
  }

  // ---- An arriving change to the project's shared settings --------------
  function settingsBox() {
    const s = view.settings;
    if (!s) return null;
    const pair = (label, mine, theirs, idName) => h("div", { class: "grid2", id: idName },
      h("div", { class: "stack" }, h("p", { class: "small" }, h("b", {}, `${label} now`)), h("pre", { class: "box", tabindex: "0" }, mine || "(none)")),
      h("div", { class: "stack" }, h("p", { class: "small" }, h("b", {}, `${label} that arrived`)), h("pre", { class: "box", tabindex: "0" }, theirs || "(none)")));
    const decide = (accept) => async (ev) => act(ev.currentTarget, "Working…", async () => {
      view = await invoke("sync_settings", { project: id, accept });
      toast(accept ? "The project now uses the settings that arrived. New chats in it follow them." : "Kept this project's own settings.");
      // The page is drawn again, so that its name and instructions show
      // what the project has now.
      if (accept) { window.dispatchEvent(new HashChangeEvent("hashchange")); return; }
      draw();
      await doCheck();
    });
    return h("div", { class: "notice warn", id: "sync-settings" }, h("div", { class: "stack" },
      h("p", {}, h("b", {}, "New shared settings for this project have arrived. "), "They are in columbus-project.yaml, which syncs with the project's files. Instructions steer your assistant, so Columbus keeps the project's own until you choose."),
      s.mine.name !== s.theirs.name ? pair("Name", s.mine.name, s.theirs.name, "sync-settings-name") : null,
      s.mine.instructions !== s.theirs.instructions ? pair("Instructions", s.mine.instructions, s.theirs.instructions, "sync-settings-instructions") : null,
      h("div", { class: "row wrap" }, button("Use the new ones", "sm primary", decide(true), { id: "sync-settings-accept" }),
        button("Keep mine", "sm", decide(false), { id: "sync-settings-keep" }))));
  }

  // ---- What a sync did ---------------------------------------------------
  function drawReport(r) {
    result.replaceChildren();
    if (r.stopped) {
      result.append(h("div", { class: "notice fail", id: "sync-stopped" }, h("div", { class: "stack" },
        h("p", {}, h("b", {}, "Sync stopped before anything was sent. "), "These files look like they hold a secret:"),
        h("ul", {}, ...r.stopped.map((f) => el("li", "wrap-any", `${f.path}, line ${f.line}: ${f.what}`))),
        h("p", { class: "small" }, "Take the secret out of the file (a script can read a password from the environment instead), or move the file into a folder named outputs, which never syncs. Then press Sync again."))));
    }
    const said = [];
    if (r.got.length) said.push(`Got ${plural(r.got.length, "file")} from the server.`);
    if (r.sent.length) said.push(`Sent ${plural(r.sent.length, "change")}.`);
    if (r.removed.length) said.push(`${plural(r.removed.length, "file")} deleted on the server ${r.removed.length === 1 ? "was" : "were"} taken out of the folder, not erased: ${r.removed.map((x) => `${x.path} (${keptWhere(x.kept)})`).join(", ")}.`);
    if (r.conflicts.length) said.push(`${plural(r.conflicts.length, "file")} ${r.conflicts.length === 1 ? "needs" : "need"} a look: both versions were kept.`);
    // Each with its reason, in the sync's own words (it changed after the
    // person looked, or this sync was full).
    if (r.not_sent.length) said.push(`${plural(r.not_sent.length, "change")} ${r.not_sent.length === 1 ? "was" : "were"} not sent: ${r.not_sent.map((x) => `${x.path} (${x.why})`).join(", ")}.`);
    if (r.rounds) said.push("The server changed while Columbus was syncing, so it read it again; nothing was overwritten.");
    if (!r.stopped && !said.length) said.push("Nothing to get and nothing to send.");
    if (said.length) result.append(h("p", { class: "small", id: "sync-report" }, said.join(" ")));
  }

  // ---- Actions ----------------------------------------------------------
  async function doCheck() {
    status.textContent = "Checking for changes…";
    try {
      const r = await invoke("sync_check", { project: id });
      view = r.view;
      changes = r.changes;
      status.textContent = summary(changes);
      drawChanges();
      drawLook();
    } catch (e) {
      status.textContent = "";
      throw e;
    }
  }
  const check = (btn) => act(btn || null, "Checking…", doCheck);

  async function onSync(ev) {
    const note = card.querySelector("#sync-note");
    await act(ev.currentTarget, "Syncing…", async () => {
      status.textContent = "Syncing…";
      try {
        const r = await invoke("sync_now", { project: id, note: note && note.value.trim() ? note.value.trim() : null });
        view = r.view;
        changes = r.changes;
        if (note && !r.report.stopped && !r.report.not_sent.length) note.value = "";
        draw();
        drawReport(r.report);
        status.textContent = changes ? summary(changes) : "";
        drawChanges();
      } catch (e) {
        status.textContent = "";
        throw e;
      }
    });
  }

  async function onDisconnect() {
    const ok = await askConfirm("Disconnect this project?", "Columbus forgets which repository this project syncs with. The files stay as they are, here and on the server.", "Disconnect");
    if (!ok) return;
    await act(disconnect, "Disconnecting…", async () => {
      view = await invoke("sync_disconnect", { project: id });
      changes = null;
      result.replaceChildren();
      toast("Disconnected. The files are still here and on the server.");
      draw();
    });
  }

  let lookSlot = h("div", {});
  function drawLook() {
    const fresh = lookList() || h("div", {});
    lookSlot.replaceWith(fresh);
    lookSlot = fresh;
  }

  function history() {
    const box = h("div", { id: "sync-history-list" }, h("p", { class: "muted small" }, "Looking…"));
    const d = h("details", { id: "sync-history" }, h("summary", { class: "small" }, "History"), box);
    d.addEventListener("toggle", async () => {
      if (!d.open) return;
      try {
        const items = await invoke("sync_history", { project: id });
        box.replaceChildren(items.length ? h("ul", { class: "list compact" }, ...items.map((c) => h("li", {}, h("div", { class: "text" },
          h("b", { class: "wrap-any" }, (c.note || "(no note)").split("\n")[0]), h("span", { class: "where" }, `${c.who || "someone"} · ${ago(c.when) || c.when}`)))))
          : h("p", { class: "muted small" }, "Nothing has been synced to this repository yet."));
      } catch (e) {
        box.replaceChildren(h("p", { class: "error-text small", role: "alert" }, String(e)));
      }
    });
    return d;
  }

  // ---- Link -------------------------------------------------------------
  function openLink() {
    const usable = view.servers.filter((s) => !s.problem);
    const server = h("select", { id: "sync-server", name: "server" }, ...view.servers.map((s) => h("option", { value: s.id, disabled: !!s.problem }, `${s.user} on ${s.host}${s.problem ? " (not allowed by your organization)" : ""}`)));
    if (usable.length) server.value = usable[0].id;
    const radio = (value, idName, checked) => { const r = h("input", { type: "radio", name: "sync-which", id: idName, value }); r.checked = !!checked; return r; };
    const pick = radio("pick", "sync-pick", true);
    const other = radio("other", "sync-other", false);
    const fresh = radio("new", "sync-new", false);
    const repo = h("select", { id: "sync-repo", "aria-label": "One of my repositories" }, h("option", { value: "" }, "Looking…"));
    const otherName = h("input", { id: "sync-other-name", type: "text", autocomplete: "off", spellcheck: "false", placeholder: "owner/name", "aria-label": "Another repository, as owner/name" });
    const newName = h("input", { id: "sync-new-name", type: "text", autocomplete: "off", spellcheck: "false", maxlength: "100", "aria-label": "Name of the new private repository" });
    newName.value = id;
    const branch = h("select", { id: "sync-branch", name: "branch" }, h("option", { value: "" }, "The repository's own (default)"));
    const branchField = h("label", { class: "field", for: "sync-branch" }, "Branch", branch, h("small", {}, "The line of work in the repository this project syncs with. Leave it unless your team told you another."));
    const err = h("p", { id: "sync-link-error", class: "error-text", role: "alert", hidden: true });
    const form = h("form", { id: "sync-link-form", class: "stack" },
      h("label", { class: "field", for: "sync-server" }, "Server", server),
      h("fieldset", { class: "stack" }, h("legend", { class: "field" }, "Repository"),
        h("label", { class: "row", for: "sync-pick" }, pick, " One of my repositories"), repo,
        h("label", { class: "row", for: "sync-other" }, other, " Another repository I may use"), otherName,
        h("label", { class: "row", for: "sync-new" }, fresh, " A new private repository"), newName),
      branchField,
      h("p", { class: "small muted" }, "Linking sends none of your files. This project's files folder syncs with that repository when you press Sync."),
      err);
    const save = h("button", { type: "submit", id: "sync-link-save", class: "btn primary", form: "sync-link-form" }, "Link");
    const cancel = h("button", { type: "button", class: "btn" }, "Cancel");
    const d = openDialog("Link this project", form, [h("span", { class: "spacer" }), cancel, save], "Link this project to a repository");
    d.id = "sync-link-dialog";
    cancel.addEventListener("click", () => d.close());
    let repos = [];
    const which = () => (fresh.checked ? "new" : other.checked ? "other" : "pick");
    const chosen = () => {
      if (which() === "pick") { const r = repos[Number(repo.value)]; return r ? [r.owner, r.name] : null; }
      if (which() === "other") { const m = otherName.value.trim().match(/^([^/\s]+)\/([^/\s]+)$/); return m ? [m[1], m[2]] : null; }
      return null;
    };
    let branchSeq = 0;
    const loadBranches = async () => {
      const seq = ++branchSeq;
      branch.replaceChildren(h("option", { value: "" }, "The repository's own (default)"));
      branchField.hidden = which() === "new";
      const c = chosen();
      if (!c || which() === "new") return;
      try {
        const b = await invoke("sync_branches", { server: server.value, owner: c[0], repo: c[1] });
        if (seq !== branchSeq) return;
        for (const name of b.branches) if (name !== b.default) branch.append(h("option", { value: name }, name));
        branch.firstChild.textContent = b.branches.length ? `${b.default} (the repository's own)` : "The repository is empty: its first files make its branch";
      } catch (e) { if (seq === branchSeq && which() === "other") showError(err, e); }
    };
    let repoSeq = 0;
    const loadRepos = async () => {
      // The answer for a server chosen earlier is dropped when it comes late.
      const seq = ++repoSeq;
      repos = [];
      repo.replaceChildren(h("option", { value: "" }, "Looking…"));
      showError(err, null);
      if (!server.value) { repo.replaceChildren(h("option", { value: "" }, "No server can be used")); return; }
      try {
        const found = await invoke("sync_repos", { server: server.value });
        if (seq !== repoSeq) return;
        repos = found;
        repo.replaceChildren(...(repos.length ? repos.map((r, i) => h("option", { value: String(i) }, `${r.owner}/${r.name}${r.private ? "" : " (public)"}${r.can_send ? "" : " (read only for you)"}`))
          : [h("option", { value: "" }, "You have no repositories there yet")]));
        if (!repos.length) { fresh.checked = true; }
      } catch (e) {
        if (seq !== repoSeq) return;
        repo.replaceChildren(h("option", { value: "" }, "Could not be listed"));
        showError(err, e);
      }
      await loadBranches();
    };
    server.addEventListener("change", loadRepos);
    repo.addEventListener("change", () => { pick.checked = true; loadBranches(); });
    otherName.addEventListener("change", () => { other.checked = true; loadBranches(); });
    newName.addEventListener("input", () => { fresh.checked = true; branchField.hidden = true; });
    for (const r of [pick, other, fresh]) r.addEventListener("change", loadBranches);
    form.addEventListener("submit", async (ev) => {
      ev.preventDefault();
      showError(err, null);
      const w = which();
      const c = chosen();
      if (w !== "new" && !c) { showError(err, w === "pick" ? "Choose a repository." : "Write the repository as owner/name."); return; }
      if (w === "new" && !newName.value.trim()) { showError(err, "Give the new repository a name."); return; }
      save.disabled = true;
      save.textContent = w === "new" ? "Creating…" : "Linking…";
      try {
        view = await invoke("sync_link", { form: { project: id, server: server.value, owner: c ? c[0] : "", repo: c ? c[1] : newName.value.trim(), branch: w === "new" ? "" : branch.value, create: w === "new" } });
        d.close();
        toast(`Linked to ${view.link.owner}/${view.link.repo} on ${view.link.host}. No file was sent.`);
        draw();
        await check();
      } catch (e) {
        showError(err, e);
      } finally {
        save.disabled = false;
        save.textContent = "Link";
      }
    });
    loadRepos();
    server.focus();
  }

  // ---- The section ------------------------------------------------------
  function draw() {
    disconnect.hidden = !(view && view.allowed && view.link);
    if (!view.allowed) {
      bd.replaceChildren(h("p", { id: "sync-off", class: "small muted" }, view.why_not || "Sync is turned off."));
      return;
    }
    if (!view.link) {
      const usable = view.servers.some((s) => !s.problem);
      bd.replaceChildren(error,
        h("p", { class: "small", id: "sync-unlinked" }, "This project's files can sync with a repository on GitHub or your organization's own Git server: get your colleagues' changes, send yours, see what changed. Nothing is sent until you press Sync."),
        usable ? h("div", { class: "row" }, button("Link this project", "primary sm", openLink, { id: "sync-link" }))
          : h("p", { class: "small muted", id: "sync-no-server" }, view.servers.length ? "None of your Git servers is allowed by your organization's settings. " : "No Git server is connected yet. ",
            h("a", { href: "#/settings/git", id: "sync-to-servers" }, "Connect one in Settings › Git servers"), ", then link this project here."));
      return;
    }
    const l = view.link;
    const note = h("input", { id: "sync-note", type: "text", maxlength: "500", class: "grow", autocomplete: "off", placeholder: "For example: widths check, fixed the units" });
    lookSlot = lookList() || h("div", {});
    bd.replaceChildren(...[error,
      h("p", { class: "small wrap-any", id: "sync-where" }, `Linked to ${l.owner}/${l.repo} (${l.branch}) on ${l.host || "a server that is no longer connected"}. `,
        view.synced_at ? `Last synced ${ago(view.synced_at)}.` : "Not synced yet."),
      l.problem ? h("div", { class: "notice warn", id: "sync-problem" }, h("div", {}, l.problem)) : null,
      settingsBox(),
      h("div", { class: "row wrap" },
        button("Check for changes", "sm", (ev) => check(ev.currentTarget), { id: "sync-check", disabled: !!l.problem }),
        button("Sync", "primary sm", onSync, { id: "sync-go", disabled: !!l.problem })),
      h("label", { class: "field", for: "sync-note" }, "What did you change? (optional)", note),
      status, result, changesBox, lookSlot, history(),
      h("p", { class: "small muted wrap-any" }, "What syncs is this project's files folder, ", h("code", {}, view.folder), ". Data, keys and outputs stay out of it.")].filter(Boolean));
    drawChanges();
  }

  (async () => {
    try {
      view = await invoke("sync_state", { project: id });
      draw();
      // Once, when the page opens: what changed (this only reads).
      if (view.allowed && view.link && !view.link.problem) await check();
    } catch (e) {
      bd.replaceChildren(error);
      showError(error, e);
    }
  })();
  return card;
}
