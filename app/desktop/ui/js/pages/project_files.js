// A project's files (its own folder of scripts and notes): the Files
// section of a project's page. Names, sizes and when each changed, in
// folders, and a button that opens the folder in the system's file
// manager, so the person can put their own scripts in. The list is read by
// the app's own code (never following a link); the files' text is not shown
// here. Kept in a module of its own: the project page only places it.
import { h, el, invoke, button, showError, size, when, ago, plural } from "../core.js";

/** The Files card of project `p`. It fills in when the list arrives. */
export function filesCard(p) {
  const title = h("h2", {}, "Files");
  const err = h("p", { id: "pj-files-error", class: "pad error-text small", role: "alert", hidden: true });
  const bd = h("div", { class: "bd flush", id: "pj-files-body" }, h("p", { class: "pad muted small" }, "Looking in the project's folder…"));
  const open = button("Open the folder", "sm", async () => {
    showError(err, null);
    try {
      await invoke("project_files_open", { id: p.id });
      await draw();
    } catch (e) { showError(err, e); }
  }, { id: "pj-files-open", title: "Show this project's files in your file manager, to add your own scripts and notes" });
  const again = button("Check again", "sm", () => draw(), { id: "pj-files-refresh", title: "Read the folder again" });
  const card = h("section", { class: "card", id: "pj-files" }, h("div", { class: "hd" }, title, h("span", { class: "spacer" }), again, open), err, bd);

  // Only the read begun last is drawn: one begun earlier (Check again
  // pressed twice, or Open the folder and then Check again) can end later,
  // and would put back the list as it was before a file was added.
  let reads = 0;
  async function draw() {
    const mine = ++reads;
    let r;
    try {
      r = await invoke("project_files", { id: p.id });
    } catch (e) {
      if (mine !== reads) return;
      bd.replaceChildren(h("p", { class: "pad small error-text" }, `Couldn't list them: ${e}`));
      return;
    }
    if (mine !== reads) return;
    const files = r.files || [];
    title.textContent = files.length ? `Files (${files.length}${r.more ? "+" : ""})` : "Files";
    const parts = [];
    if (!r.allowed) {
      parts.push(h("p", { class: "pad small", id: "pj-files-off" }, "Your assistant can't use these yet: project files are turned off in your organization's settings. ",
        h("a", { href: "#/settings/org" }, "See Organization rules"), "."));
    }
    if (!files.length) {
      parts.push(h("p", { class: "pad muted small", id: "pj-files-none" },
        "No files yet. Scripts and notes for this project are kept here: your assistant can write them, and you can put your own in with Open the folder."));
    } else {
      // In folders: each folder once, then the files in it.
      const ul = h("ul", { class: "list", id: "pj-file-list" });
      let folder = null;
      for (const f of files) {
        const cut = f.path.lastIndexOf("/");
        const dir = cut < 0 ? "" : f.path.slice(0, cut);
        const name = cut < 0 ? f.path : f.path.slice(cut + 1);
        if (dir !== folder) {
          folder = dir;
          if (dir) ul.append(h("li", { class: "folder" }, h("div", { class: "text" }, h("span", { class: "where" }, `${dir}/`))));
        }
        const li = h("li", { class: dir ? "file in" : "file" }, h("div", { class: "text" },
          h("b", {}, name),
          h("span", { class: "where" }, [size(f.bytes), f.modified ? `changed ${ago(f.modified)}` : null].filter(Boolean).join(" · "))));
        li.dataset.path = f.path;
        if (f.modified) li.title = `Changed ${when(f.modified)}`;
        ul.append(li);
      }
      parts.push(ul);
      parts.push(h("p", { class: "pad small muted" }, `${plural(files.length, "file")}, ${size(r.total_bytes)} in all${r.more ? " (the first 1,000 are listed)" : ""}. `,
        r.allowed ? "Your assistant can list, read and write the text files here, and run a script from here in the sandbox. Data stays out: results leave a chat only through an export." : ""));
    }
    const skipped = r.skipped || [];
    if (skipped.length) {
      parts.push(h("p", { class: "pad small muted", id: "pj-files-skipped" }, "Not offered to your assistant: ",
        skipped.slice(0, 8).map((s) => `${s.path} (${s.why})`).join("; "), skipped.length > 8 ? `; and ${skipped.length - 8} more` : "", "."));
    }
    parts.push(h("p", { class: "pad small muted wrap-any" }, "Kept in ", el("code", null, r.dir), "."));
    bd.replaceChildren(...parts);
  }
  draw();
  return card;
}
