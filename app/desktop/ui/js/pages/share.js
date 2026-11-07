// Sharing a tool or workflow as a .columbus file, and opening one a coworker
// shared. What a shared file holds is filed as proposals: it waits under
// Tools or Workflows for the person to read and approve with Touch ID or
// Windows Hello, exactly as their own assistant's proposals do. Everything
// in a shared file is the sharer's own words, shown as text.
import { h, el, invoke, button, openDialog, showError, toast, when, plural } from "../core.js";

/** Save an installed tool or workflow as a shared file. */
export async function shareOut(kind, id, err) {
  try {
    const path = await invoke("share_export", { kind, id });
    if (path) toast(`Saved ${path}. Send it however you like: your coworker opens it in Columbus, reads it and approves it there.`);
  } catch (e) { showError(err, e); }
}

/** Choose a shared file, show what it holds and what it needs, and file it
 * for review. `done` runs after it is filed. */
export async function shareIn(err, done) {
  let info;
  try {
    info = await invoke("share_open");
  } catch (e) { showError(err, e); return; }
  if (!info) return;
  const problem = h("p", { class: "error-text", role: "alert", hidden: true, id: "share-error" });
  const body = h("div", { class: "stack", id: "share-open" });
  const what = info.kind === "workflow" ? `the workflow "${info.workflow.name}"` : `the tool "${info.tools[0]?.title || info.tools[0]?.id}"`;
  body.append(h("p", {}, `${info.file} holds ${what}. The file says it was shared by ${info.shared_by} on ${when(info.shared_at)}, from Columbus ${info.columbus}: that is the file's own word, so open only files you got from someone you know.`),
    h("p", { class: "small muted wrap-any" }, `From ${info.path}`),
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
  file.disabled = !info.allowed || blocked;
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
