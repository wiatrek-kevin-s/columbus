// Sharing a tool or workflow as a .columbus file, and opening one a coworker
// shared. What a shared file holds is filed as proposals: it waits under
// Tools or Workflows for the person to read and approve with Touch ID or
// Windows Hello, exactly as their own assistant's proposals do. Everything
// in a shared file is the sharer's own words, shown as text.
import { h, el, invoke, button, openDialog, showError, toast, when, plural } from "../core.js";

/** Save an installed tool or workflow as a shared file. */
export async function shareOut(kind, id, err) {
  try {
    const r = await invoke("share_export", { kind, id });
    if (!r) return;
    const signed = r.signed ? " Signed with your sharing key." : ` Not signed (${r.unsigned_why || "no key on this computer"}).`;
    toast(`Saved ${r.path}.${signed} Send it however you like, or keep it in your team library: your coworker opens it in Columbus, reads it and approves it there.`);
  } catch (e) { showError(err, e); }
}

/** Who signed a shared file, in words, with "Trust this sender…" for a
 * good signature by a key not trusted yet. */
function signerBlock(info, onTrusted) {
  const sg = info.signer || { status: "unsigned" };
  if (sg.status === "trusted") {
    return h("p", { class: "notice ok" }, `Signed by ${sg.name} (key ${sg.fingerprint}), whom ${sg.by === "organization" ? "your organization" : "you"} trust${sg.by === "organization" ? "s" : ""}. The file is exactly as they saved it.`);
  }
  if (sg.status === "invalid") {
    return h("p", { class: "notice warn" }, "Its signature doesn't match it: the file was changed after it was signed. It can't be opened.");
  }
  if (sg.status === "unknown") {
    const wrap = h("div", { class: "notice warn stack" });
    const name = h("input", { type: "text", maxlength: 80, placeholder: "Their name", "aria-label": "The sender's name", id: "share-trust-name" });
    const err = h("p", { class: "error-text small", role: "alert", hidden: true });
    const trust = button("Trust this sender", "sm", async () => {
      showError(err, null);
      try {
        await invoke("share_trust", { name: name.value, key: sg.key });
        toast(`Files signed with this key now show as ${name.value}'s.`);
        if (onTrusted) onTrusted();
      } catch (e) { showError(err, e); }
    }, { id: "share-trust" });
    wrap.append(
      h("p", {}, `Signed with a key you don't trust yet: ${sg.fingerprint}.`),
      h("p", { class: "small" }, "Trust it only if the sender read you this same fingerprint themselves (by phone or in person; Tools › Team library › Show my sharing key). Then files they sign show as theirs."),
      h("div", { class: "row" }, name, trust), err);
    return wrap;
  }
  return h("p", { class: "notice warn" }, `Not signed: nothing shows who made it.${info.require_signed ? " Your organization opens only signed files from people it or you trust." : ""}`);
}

/** Choose a shared file, show what it holds and what it needs, and file it
 * for review. `done` runs after it is filed. */
export async function shareIn(err, done, from = null) {
  let info;
  try {
    info = from ? await invoke("share_team_open", { name: from }) : await invoke("share_open");
  } catch (e) { showError(err, e); return; }
  if (!info) return;
  const problem = h("p", { class: "error-text", role: "alert", hidden: true, id: "share-error" });
  const body = h("div", { class: "stack", id: "share-open" });
  const what = info.kind === "workflow" ? `the workflow "${info.workflow.name}"` : `the tool "${info.tools[0]?.title || info.tools[0]?.id}"`;
  body.append(h("p", {}, `${info.file} holds ${what}. The file says it was shared by ${info.shared_by} on ${when(info.shared_at)}, from Columbus ${info.columbus}: that is the file's own word, so open only files you got from someone you know.`),
    h("p", { class: "small muted wrap-any" }, `From ${info.path}`),
    signerBlock(info, () => { d.close(); shareIn(err, done, from); }),
    h("p", { class: "notice" }, "Opening it files it for you to review, as your own assistant's proposals are: nothing is installed or run until you read it and approve it. A shared tool was not test-run on your computer, so read its code before you install it."));
  if (!info.allowed) {
    body.append(h("p", { class: "notice warn" }, "Opening shared tools and workflows is off in your organization's settings (Tools, Your organization's switch)."));
  }
  if (info.workflow) {
    const w = info.workflow;
    body.append(h("h3", {}, "What it does"), w.purpose ? h("p", {}, w.purpose) : null,
      h("ul", { class: "small" }, ...(w.plain || []).map((l) => el("li", null, l))));
    if (w.review) body.append(h("p", { class: "small" }, h("b", {}, "Its AI review looks for: "), w.review));
  }
  if (info.tools.length) {
    body.append(h("h3", {}, info.kind === "workflow" ? `Tools it brings (${info.tools.length})` : "The tool"),
      h("ul", { class: "small" }, ...info.tools.map((t) => el("li", null,
        `${t.title || t.id} (${t.id}, version ${t.version || "?"}, ${plural(t.lines, "line")} of code)${t.already_installed ? ": you have this exact tool already" : t.waiting ? ": one of this name is already waiting for your review; install or decline it first" : t.replaces ? ": would replace your installed tool of this name, after you approve it" : ""}`))));
  }
  const blocked = info.tools.some((t) => t.waiting && !t.already_installed) || (info.workflow && info.workflow.waiting);
  if (info.workflow && info.workflow.waiting) body.append(h("p", { class: "notice warn" }, "A workflow of this name is already waiting for your review: approve or decline it first."));
  else if (info.workflow && info.workflow.replaces) body.append(h("p", { class: "notice warn" }, "You have a workflow of this name: this one would replace it, after you approve it."));
  const selects = {};
  let exportSel = null;
  if (info.workflow) {
    const need = info.workflow.needs;
    if (need.connections.length) {
      body.append(h("h3", {}, "Your connections"), h("p", { class: "small muted" }, "It names the connections it was made with. Choose which of yours stands for each: it will read only those, so choose with care."));
      for (const c of need.connections) {
        const s = h("select", { class: "share-conn", "data-from": c, "aria-label": `Your connection for ${c}` },
          h("option", { value: "" }, "Choose…"), ...info.mine.connections.map((m) => h("option", { value: m }, m)));
        selects[c] = s;
        body.append(h("label", { class: "field" }, `For ${c}`, s));
      }
    }
    if (need.export) {
      exportSel = h("select", { id: "share-export", "aria-label": "Your export folder" },
        h("option", { value: "" }, "Choose…"), ...info.mine.exports.map((m) => h("option", { value: m.id }, `${m.id} (${m.path})`)));
      body.append(h("label", { class: "field" }, `It exports its results to a folder (${need.export} where it was made). Which of your export folders? Anyone who can read that folder will see the results.`, exportSel));
    }
    body.append(h("p", { class: "small muted" }, "It comes without a schedule: set one after you approve it."));
  }
  body.append(problem);
  const file = button("File it for review", "primary", async () => {
    showError(problem, null);
    const connections = {};
    for (const [from, s] of Object.entries(selects)) {
      if (!s.value) { showError(problem, `Choose your connection for ${from}.`); return; }
      connections[from] = s.value;
    }
    if (exportSel && !exportSel.value) { showError(problem, "Choose your export folder."); return; }
    file.disabled = true;
    try {
      const r = await invoke("share_import", { path: info.path, sha256: info.sha256, connections, export: exportSel ? exportSel.value : null });
      d.close();
      const n = (r.filed || []).length;
      toast(n ? `Waiting for you: review ${n === 1 ? "it" : `the ${n} parts`} under ${info.kind === "workflow" ? "Workflows (its tools under Tools)" : "Tools"}.` : "You have all of it already.");
      if (done) done();
    } catch (e) {
      showError(problem, e);
      file.disabled = false;
    }
  }, { id: "share-file" });
  const sgs = (info.signer || {}).status;
  file.disabled = !info.allowed || blocked || sgs === "invalid" || (info.require_signed && sgs !== "trusted");
  const cancel = button("Cancel", "", () => d.close());
  const d = openDialog("Open a shared file", body, [h("span", { class: "spacer" }), cancel, file]);
}

/** The line a proposal from a shared file shows; `code` adds the reminder
 * that a shared tool was not test-run here. */
export function importedText(p, code = true) {
  const i = p && p.imported;
  if (!i) return "";
  const base = `From a shared file (${i.file}) that says it was shared by ${i.from} on ${when(i.shared_at)}; opened here ${when(p.proposed)}.`;
  return code ? `${base} Not test-run on your computer: read the code before you approve it.` : base;
}

/** The team library: shared files kept in a folder the team shares, with
 * who signed each and how it stands against what is installed here; and
 * the sharing keys (yours, and the people you trust). */
export function teamLibrary(card, err, done) {
  const list = h("div", { id: "team-list", class: "stack" });
  const keys = h("div", { class: "stack" });
  card.append(list, h("h3", {}, "Sharing keys"), keys);
  load();

  async function load() {
    showError(err, null);
    let st;
    let lib;
    try {
      st = await invoke("share_settings");
      lib = await invoke("share_team_list");
    } catch (e) { showError(err, e); return; }
    const choose = button(st.team_folder ? "Change folder…" : "Choose a team folder…", "sm", async () => {
      try { await invoke("share_team_folder", {}); load(); } catch (e) { showError(err, e); }
    }, { id: "team-choose" });
    const head = [h("p", { class: "small muted" }, "A folder your team shares (a network drive, or a synced SharePoint, OneDrive or Dropbox folder) where people keep the tools and workflows they share. Opening one here works as Open a shared file: you review and approve it yourself.")];
    if (st.team_folder) {
      head.push(h("p", { class: "small wrap-any" }, `Folder: ${st.team_folder}`));
      head.push(h("div", { class: "row" }, choose, button("Stop using it", "sm", async () => {
        try { await invoke("share_team_folder", { clear: true }); load(); } catch (e) { showError(err, e); }
      })));
    } else head.push(h("div", { class: "row" }, choose));
    const rows = (lib.files || []).map((f) => {
      if (f.problem) return h("div", { class: "tool" }, h("b", {}, f.name), h("p", { class: "small notice warn" }, f.problem));
      const what = f.kind === "workflow" ? `Workflow: ${f.workflow?.name || f.workflow?.id}` : `Tool: ${f.tools[0]?.title || f.tools[0]?.id}`;
      const sg = f.signer || {};
      const who = sg.status === "trusted" ? `signed by ${sg.name}` : sg.status === "unknown" ? `signed, key ${sg.fingerprint} not trusted` : sg.status === "invalid" ? "signature doesn't match: changed after signing" : "not signed";
      const state = f.kind === "workflow"
        ? (f.workflow?.waiting ? "waiting for your review" : f.workflow?.replaces ? "you have one of this name (this may be newer)" : "not installed")
        : (f.tools[0]?.already_installed ? "installed, the same" : f.tools[0]?.waiting ? "waiting for your review" : f.tools[0]?.replaces ? "different from yours (an update?)" : "not installed");
      return h("div", { class: "tool row" },
        h("div", { class: "text" }, h("b", {}, what), h("span", { class: "where" }, `${f.name} · by ${f.shared_by} · ${when(f.shared_at)} · ${who} · ${state}`)),
        h("div", { class: "acts" }, button("Open…", "sm", () => shareIn(err, () => { load(); if (done) done(); }, f.name))));
    });
    list.replaceChildren(...head, ...(st.team_folder ? (rows.length ? rows : [el("p", "muted small", "No shared files in it yet.")]) : []));
    if (lib.more) list.append(el("p", "small muted", "Only the first 200 files are listed."));

    const mine = h("span", { id: "my-key" });
    const show = button("Show my sharing key", "sm", async () => {
      try { const k = await invoke("share_my_key"); mine.textContent = ` ${k.fingerprint}`; } catch (e) { showError(err, e); }
    }, { title: "Made the first time you share; kept in your keychain. Read it to a coworker so they can trust files you sign." });
    const trusted = (st.trusted || []).map((t) => h("div", { class: "row small" }, h("span", {}, `${t.name}: ${t.fingerprint}`),
      button("Remove", "sm", async () => { try { await invoke("share_untrust", { key: t.key }); load(); } catch (e) { showError(err, e); } })));
    const org = (st.organization || []).map((t) => h("p", { class: "small" }, `${t.name}: ${t.fingerprint} (your organization)`));
    keys.replaceChildren(
      h("p", { class: "small muted" }, "Files you share are signed with your own key, so coworkers can tell they're from you and unchanged. They trust your key once, after you read them its fingerprint."),
      h("div", { class: "row small" }, show, mine),
      h("p", { class: "small" }, h("b", {}, "People you trust: "), trusted.length || org.length ? "" : "none yet"), ...trusted, ...org,
      st.require_signed ? h("p", { class: "small notice" }, "Your organization opens only signed files from people it or you trust.") : null);
  }
}
