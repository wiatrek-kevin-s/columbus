// Documents: your notes (drafted from your databases) to review, and the
// organization's context pack, read only. Notes are your own files: "Mark
// reviewed" is your decision, and once reviewed your assistant can no
// longer change the note.
import { h, el, invoke, button, pageHead, tabs, showError, when, plural, emptyState, coming, toast } from "../core.js";

export async function render(main, rest) {
  const tab = rest[0] || "notes";
  const page = h("div", { class: "page" });
  main.append(page);
  page.append(pageHead("Documents", "Your notes about your data, and your organization's context pack: what your assistant reads to understand the data."));
  page.append(tabs([["notes", "My notes", "#/documents/notes"], ["pack", "Organization's pack", "#/documents/pack"], ["proposals", "Proposals", "#/documents/proposals"], ["import", "Import", "#/documents/import"]], tab, "Documents"));
  const body = h("div", { class: "pad stack" });
  page.append(h("div", { class: "scroll" }, body));
  if (tab === "pack") return pack(body);
  if (tab === "proposals") {
    body.append(coming("documents", "Proposals",
      "An inbox of suggested changes that do not hold your assistant up: schema drift (a database changed since its note was drafted, with Update note), suggestions your assistant made in a chat (Accept or Dismiss), and entries past their review date.",
      ["Today, your assistant's note changes are approvals, decided in your AI app or on the browser page (or answered in this app's own chat)."], "Coming in stage 2"));
    return;
  }
  if (tab === "import") {
    body.append(coming("documents", "Import",
      "Word, PDF, Excel, CSV and Markdown become draft entries in your documents, keeping the original beside them; the sop-conversion and writing-clearly-and-concisely skills help.", [], "Coming in stage 2"));
    return;
  }
  return notes(body);
}

async function notes(body) {
  const error = h("p", { class: "error-text", role: "alert", hidden: true });
  body.append(error);
  const host = h("div", { class: "cols" });
  body.append(host);
  const left = h("section", { class: "card" });
  const right = h("section", { class: "card", "aria-live": "polite" });
  host.append(left, right);
  let filter = "all";
  let data;
  let open = null;
  const draw = () => {
    const all = data.notes;
    const shown = all.filter((n) => filter === "all" || n.status === filter);
    const f = h("div", { class: "seg", role: "group", "aria-label": "Show" });
    for (const [k, t] of [["all", `All ${all.length}`], ["draft", `Draft ${all.filter((n) => n.status === "draft").length}`], ["reviewed", `Reviewed ${all.filter((n) => n.status === "reviewed").length}`]]) {
      const b = h("button", { type: "button", "aria-pressed": String(filter === k) }, t);
      b.addEventListener("click", () => { filter = k; draw(); });
      f.append(b);
    }
    left.replaceChildren(h("div", { class: "hd" }, h("h2", {}, "Your notes"), h("span", { class: "more" }, f)));
    if (!all.length) {
      left.append(h("div", { class: "bd" }, emptyState("No notes yet", "Draft notes from a connection's database (Connections › Draft notes): fields, keys, relationships and the codes short fields hold. Then fill in what they mean, and mark each one reviewed.", h("a", { class: "btn", href: "#/connections" }, "Go to Connections"))));
      right.replaceChildren(h("div", { class: "bd" }, h("p", { class: "small muted wrap-any" }, `Your notes folder: ${data.dir}`)));
      return;
    }
    const ul = h("ul", { class: "list", id: "notes-list" });
    for (const n of shown) {
      const li = h("li", { class: "clickable" }, h("div", { class: "text" }, h("b", {}, n.title || n.id || n.path), h("span", { class: "where mono" }, n.path)),
        h("span", { class: `badge ${n.status === "reviewed" ? "pass" : n.status === "draft" ? "warn" : "mute"}` }, n.status || "note"));
      li.tabIndex = 0;
      const go = () => show(n);
      li.addEventListener("click", go);
      li.addEventListener("keydown", (e) => { if (e.key === "Enter" || e.key === " ") { e.preventDefault(); go(); } });
      if (open && open.path === n.path) li.classList.add("sel");
      ul.append(li);
    }
    left.append(h("div", { class: "bd flush" }, ul), h("div", { class: "bd" }, h("p", { class: "small muted wrap-any" }, `In ${data.dir}`)));
  };
  const show = async (n) => {
    open = n;
    right.replaceChildren(h("div", { class: "hd" }, h("h2", {}, n.title || n.id || n.path), h("span", { class: `badge ${n.status === "reviewed" ? "pass" : "warn"} more` }, n.status || "note")));
    const bd = h("div", { class: "bd stack" });
    right.append(bd);
    const dl = h("dl", { class: "kv" });
    for (const [k, v] of [["Connection", n.connection], ["Owner", n.owner], ["Last confirmed", n.last_confirmed], ["Changed", when(n.modified)], ["File", n.path]]) if (v) dl.append(h("dt", {}, k), h("dd", {}, String(v)));
    bd.append(dl);
    const err = h("p", { class: "error-text", role: "alert", hidden: true });
    if (n.status === "draft") {
      bd.append(h("div", { class: "row wrap" },
        button("Mark reviewed", "primary", async () => {
          showError(err, null);
          try {
            await invoke("note_mark_reviewed", { path: n.path });
            toast(`Marked reviewed: ${n.title || n.path}. Your assistant can no longer change it.`);
            await load();
            const again = data.notes.find((x) => x.path === n.path);
            if (again) show(again);
          } catch (e) { showError(err, e); }
        }),
        h("span", { class: "small muted" }, "Check what it says first. Once reviewed, your assistant can no longer change this note; you still can, in the file.")), err);
    }
    try {
      const text = await invoke("note_read", { path: n.path });
      bd.append(el("pre", "box", text));
    } catch (e) { bd.append(el("p", "error-text", String(e))); }
    draw();
  };
  const load = async () => {
    try {
      data = await invoke("notes_list");
      draw();
      if (!open) right.replaceChildren(h("div", { class: "bd" }, h("p", { class: "muted" }, data.notes.length ? "Choose a note to read it." : "")));
    } catch (e) { showError(error, e); }
  };
  await load();
}

async function pack(body) {
  let r;
  try { r = await invoke("context_pack"); } catch (e) { body.append(el("p", "error-text", String(e))); return; }
  if (!r.exists) {
    body.append(emptyState("No context pack on this computer",
      `Your organization's context pack (datasets, domains, LRS, SOPs, roles, delivery specs) is a folder IT places at ${r.dir}. Columbus works without one; your own notes still help your assistant.`));
    return;
  }
  body.append(h("div", { class: "notice" }, h("div", {}, h("b", {}, "Read only here. "), "The pack belongs to your organization; editing it, and suggesting changes to its owners, comes later. ", h("span", { class: "mono" }, r.dir))));
  const host = h("div", { class: "cols" });
  const left = h("section", { class: "card" });
  const right = h("section", { class: "card", "aria-live": "polite" }, h("div", { class: "bd" }, h("p", { class: "muted" }, "Choose an entry to read it.")));
  host.append(left, right);
  body.append(host);
  left.append(h("div", { class: "hd" }, h("h2", {}, "Entries"), h("span", { class: "muted small more" }, plural(r.entries.length, "entry", "entries"))));
  const ul = h("ul", { class: "list" });
  for (const e of r.entries) {
    const li = h("li", { class: "clickable", tabindex: "0" }, h("div", { class: "text" }, h("b", {}, e.title || e.path), h("span", { class: "where mono" }, e.path)), e.kind ? h("span", { class: "chip" }, String(e.kind)) : null);
    const go = async () => {
      right.replaceChildren(h("div", { class: "hd" }, h("h2", {}, e.title || e.path)));
      const bd = h("div", { class: "bd" });
      right.append(bd);
      try { bd.append(el("pre", "box", await invoke("context_read", { path: e.path }))); } catch (x) { bd.append(el("p", "error-text", String(x))); }
    };
    li.addEventListener("click", go);
    li.addEventListener("keydown", (k) => { if (k.key === "Enter") go(); });
    ul.append(li);
  }
  left.append(h("div", { class: "bd flush" }, r.entries.length ? ul : h("p", { class: "pad muted" }, "The pack has no entries.")));
  if (r.pack) left.append(h("details", { class: "bd" }, h("summary", { class: "small" }, "pack.yaml"), el("pre", "box", r.pack)));
}
