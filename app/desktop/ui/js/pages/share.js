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
  body.append(h("p", {}, `${info.shared_by} shared ${what} on ${when(info.shared_at)} (from Columbus ${info.columbus}).`),
    h("p", { class: "small muted" }, "Opening it files it for you to review, as your own assistant's proposals are: nothing is installed or run until you read it and approve it. No data comes with it."));
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
        `${t.title || t.id} (${t.id}, version ${t.version || "?"}, ${plural(t.lines, "line")} of code)${t.already_installed ? ": you have this exact tool already" : t.replaces ? ": would replace your installed tool of this name, after you approve it" : ""}`))));
  }
  const selects = {};
  let exportSel = null;
  if (info.workflow) {
    const need = info.workflow.needs;
    if (need.connections.length) {
      body.append(h("h3", {}, "Your connections"), h("p", { class: "small muted" }, "It names the connections it was made with. Choose which of yours stands for each; it can only read those."));
      for (const c of need.connections) {
        const s = h("select", { class: "share-conn", "data-from": c, "aria-label": `Your connection for ${c}` },
          h("option", { value: "" }, "Choose…"), ...info.mine.connections.map((m) => h("option", { value: m }, m)));
        if (info.mine.connections.includes(c)) s.value = c;
        selects[c] = s;
        body.append(h("label", { class: "field" }, `For ${c}`, s));
      }
    }
    if (need.export) {
      exportSel = h("select", { id: "share-export", "aria-label": "Your export folder" },
        h("option", { value: "" }, "Choose…"), ...info.mine.exports.map((m) => h("option", { value: m }, m)));
      if (info.mine.exports.includes(need.export)) exportSel.value = need.export;
      body.append(h("label", { class: "field" }, `It exports to ${need.export}: which of your export folders?`, exportSel));
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
      const r = await invoke("share_import", { path: info.path, connections, export: exportSel ? exportSel.value : null });
      d.close();
      const n = (r.filed || []).length;
      toast(n ? `Waiting for you: review ${n === 1 ? "it" : `the ${n} parts`} under ${info.kind === "workflow" ? "Workflows (its tools under Tools)" : "Tools"}.` : "You have all of it already.");
      if (done) done();
    } catch (e) {
      showError(problem, e);
      file.disabled = false;
    }
  }, { id: "share-file" });
  file.disabled = !info.allowed;
  const cancel = button("Cancel", "", () => d.close());
  const d = openDialog("Open a shared file", body, [h("span", { class: "spacer" }), cancel, file]);
}

/** The line a proposal from a shared file shows. */
export function importedText(p) {
  const i = p && p.imported;
  if (!i) return "";
  return `Shared by ${i.from} on ${when(i.shared_at)}; opened here ${when(p.proposed)}. Not test-run on your computer: read the code before you approve it.`;
}
