// Notes: your notes about your data (drafted from your databases) to
// review, and the organization's context pack, read only. A note opens in
// the sheet beside the panel. Notes are your own files: "Mark reviewed" is
// your decision, and once reviewed your assistant can no longer change the
// note.
import { h, el, invoke, button, pageHead, tabs, showError, when, plural, emptyState, toast, openSide, closeSide } from "../core.js";

const go = (hash) => { location.hash = hash; };

export async function render(main, rest) {
  const tab = rest[0] || "notes";
  const page = h("div", { class: "page" });
  main.append(page);
  page.append(pageHead("Notes", "Your notes about your data, and your organization's context pack: what your assistant reads to understand the data.", [], { href: "#/data", label: "Data" }));
  page.append(tabs([["notes", "My notes", "#/documents/notes"], ["pack", "Organization's pack", "#/documents/pack"], ["proposals", "Proposals", "#/documents/proposals"], ["import", "Import", "#/documents/import"]], tab, "Notes"));
  const body = h("div", { class: "pad stack" });
  page.append(h("div", { class: "scroll" }, body));
  if (tab === "pack") return pack(body);
  if (tab === "proposals") return proposals(body);
  if (tab === "import") return importView(body);
  return notes(body);
}

async function notes(body) {
  const error = h("p", { class: "error-text", role: "alert", hidden: true });
  body.append(error);
  const left = h("section", { class: "card" });
  body.append(left);
  let filter = "all";
  let data;
  let open = null;
  let shown = null; // the open note's box in the side sheet
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
      left.append(h("div", { class: "bd" }, emptyState("No notes yet", "Draft notes from a connection's database (Data › a connection › Draft notes): fields, keys, relationships and the codes short fields hold. Then fill in what they mean, and mark each one reviewed.", h("a", { class: "btn", href: "#/data" }, "Go to Data")),
        h("p", { class: "small muted wrap-any" }, `Your notes folder: ${data.dir}`)));
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
  // A note opens in the sheet beside the panel, where there is room to read.
  const show = async (n) => {
    const bd = h("div", { class: "stack", id: "note-open" });
    // (Opening closes the sheet that was open first: this note is the open
    // one only after that.)
    openSide(n.title || n.id || n.path, bd, [h("span", { class: `badge ${n.status === "reviewed" ? "pass" : "warn"}` }, n.status || "note")],
      () => { if (open === n && shown === bd) { open = null; shown = null; if (data) draw(); } });
    open = n;
    shown = bd;
    const dl = h("dl", { class: "kv" });
    for (const [k, v] of [["Connection", n.connection], ["Owner", n.owner], ["Last confirmed", n.last_confirmed], ["Changed", when(n.modified)], ["File", n.path]]) if (v) dl.append(h("dt", {}, k), h("dd", {}, String(v)));
    bd.append(dl);
    const err = h("p", { class: "error-text", role: "alert", hidden: true });
    // The note first; "Mark reviewed" only once the person can read it, and
    // it marks exactly what they read (its hash), nothing changed since.
    let read;
    try {
      read = await invoke("note_read", { path: n.path });
    } catch (e) {
      bd.append(el("p", "error-text", String(e)));
      draw();
      return;
    }
    if (shown !== bd) return;
    if (n.status === "draft") {
      bd.append(h("div", { class: "row wrap" },
        button("Mark reviewed", "primary", async () => {
          showError(err, null);
          try {
            await invoke("note_mark_reviewed", { path: n.path, sha256: read.sha256 });
            toast(`Marked reviewed: ${n.title || n.path}. Your assistant can no longer change it.`);
            await load();
            const again = data.notes.find((x) => x.path === n.path);
            if (again) show(again);
          } catch (e) { showError(err, e); }
        }),
        h("span", { class: "small muted" }, "Check what it says below first. Once reviewed, your assistant can no longer change this note; you still can, in the file.")), err);
    }
    bd.append(el("pre", "box", read.text));
    draw();
  };
  const load = async () => {
    try {
      data = await invoke("notes_list");
      draw();
    } catch (e) { showError(error, e); }
  };
  await load();
  // Arriving from Proposals with a note to open.
  let want = null;
  try { want = sessionStorage.getItem("columbus.note"); sessionStorage.removeItem("columbus.note"); } catch { /* none */ }
  const n = want && data && data.notes.find((x) => x.path === want);
  if (n) show(n);
  return () => { open = null; shown = null; closeSide(); };
}

async function pack(body) {
  let r;
  try { r = await invoke("context_pack"); } catch (e) { body.append(el("p", "error-text", String(e))); return; }
  if (!r.exists) {
    body.append(emptyState("No context pack on this computer",
      `Your organization's context pack (datasets, domains, LRS, SOPs, roles, delivery specs) is a folder IT places at ${r.dir}. Columbus works without one; your own notes still help your assistant.`));
    return;
  }
  body.append(h("div", { class: "notice" }, h("div", {}, h("b", {}, "Read only here. "), "The pack belongs to your organization. ", h("span", { class: "mono" }, r.dir))));
  const left = h("section", { class: "card" });
  body.append(left);
  left.append(h("div", { class: "hd" }, h("h2", {}, "Entries"), h("span", { class: "muted small more" }, plural(r.entries.length, "entry", "entries"))));
  const ul = h("ul", { class: "list", id: "pack-list" });
  for (const e of r.entries) {
    const li = h("li", { class: "clickable", tabindex: "0" }, h("div", { class: "text" }, h("b", {}, e.title || e.path), h("span", { class: "where mono" }, e.path)), e.kind ? h("span", { class: "chip" }, String(e.kind)) : null);
    // An entry opens in the sheet beside the panel.
    const go = async () => {
      const bd = h("div", { class: "stack", id: "pack-open" });
      for (const x of ul.children) x.classList.toggle("sel", x === li);
      openSide(e.title || e.path, bd, [], () => li.classList.remove("sel"));
      try { bd.append(el("pre", "box", await invoke("context_read", { path: e.path }))); } catch (x) { bd.append(el("p", "error-text", String(x))); }
    };
    li.addEventListener("click", go);
    li.addEventListener("keydown", (k) => { if (k.key === "Enter" || k.key === " ") { k.preventDefault(); go(); } });
    ul.append(li);
  }
  left.append(h("div", { class: "bd flush" }, r.entries.length ? ul : h("p", { class: "pad muted" }, "The pack has no entries.")));
  if (r.pack) left.append(h("details", { class: "bd" }, h("summary", { class: "small" }, "pack.yaml"), el("pre", "box", r.pack)));
  return () => closeSide();
}

// ---- Import: a document becomes a draft note ---------------------------
// Previewed first (nothing written), then added to your notes as a draft,
// with the original kept beside it. Only its text comes in: nothing in the
// document is run or followed.
function importView(body) {
  const err = h("p", { id: "imp-error", class: "error-text", role: "alert", hidden: true });
  const out = h("div", { id: "imp-preview", class: "stack" });
  const choose = button("Choose a document…", "primary", pick, { id: "imp-choose" });
  body.append(h("section", { class: "card" }, h("div", { class: "bd stack" },
    h("p", {}, "Bring a procedure, a data dictionary or any reference into your notes, for your assistant to read: Word (.docx), Excel (.xlsx), PDF, CSV, Markdown or text."),
    h("p", { class: "small muted" }, "You see what it becomes before anything is added. It comes in as a draft with the original kept beside it; check it, then mark it reviewed under My notes. Only its text comes in: headings, lists and tables. Scanned PDFs (pictures of pages) have no text to bring yet."),
    h("div", { class: "row" }, choose), err)), out);

  async function pick() {
    showError(err, null);
    choose.disabled = true;
    choose.textContent = "Reading…";
    let p;
    try {
      p = await invoke("doc_preview");
    } catch (e) {
      showError(err, e);
    } finally {
      choose.disabled = false;
      choose.textContent = "Choose a document…";
    }
    if (p) draw(p);
  }

  function draw(p) {
    const firstLine = (p.markdown || "").split("\n").map((l) => l.replace(/^[#>\-*\d.\s|]+/, "").trim()).find((l) => l.length > 20) || "";
    const title = h("input", { id: "imp-title", type: "text", maxlength: "120", class: "grow", value: p.title || "" });
    const kind = h("select", { id: "imp-kind", "aria-label": "What it is" },
      h("option", { value: "sop" }, "A procedure (SOP): steps to follow"),
      h("option", { value: "note" }, "A reference: what something is or means"),
      h("option", { value: "glossary" }, "A glossary: terms and what they mean"));
    if (/procedure|sop|steps|checklist/i.test(`${p.title} ${p.file}`)) kind.value = "sop"; else kind.value = "note";
    const summary = h("textarea", { id: "imp-summary", rows: "2", maxlength: "400", placeholder: "One or two sentences: what it is about" });
    summary.value = firstLine.slice(0, 300).replace(/([.!?]).*$/, "$1");
    const whenUse = h("input", { id: "imp-when", type: "text", maxlength: "300", class: "grow", placeholder: "When to use it, e.g. the first week of each month" });
    const whenRow = h("label", { class: "field", for: "imp-when" }, "When to use it", whenUse);
    const showWhen = () => { whenRow.hidden = kind.value !== "sop"; };
    kind.addEventListener("change", showWhen);
    showWhen();
    const problem = h("p", { class: "error-text", role: "alert", hidden: true });
    const add = button("Add to my notes as a draft", "primary", async () => {
      showError(problem, null);
      add.disabled = true;
      try {
        const r = await invoke("doc_import", { form: { sha256: p.sha256, kind: kind.value, title: title.value, summary: summary.value, when_to_use: whenUse.value } });
        toast(`Added ${title.value} to your notes as a draft. Check it against the original, then mark it reviewed.`);
        out.replaceChildren(h("section", { class: "card" }, h("div", { class: "bd stack" },
          h("p", {}, `Added as ${r.path}, with the original at ${r.source}.`),
          h("div", { class: "row" }, h("a", { class: "btn", href: "#/documents/notes", id: "imp-done" }, "Go to My notes")))));
      } catch (e) {
        showError(problem, e);
        add.disabled = false;
      }
    }, { id: "imp-add" });
    const warnings = (p.warnings || []).length ? h("ul", { class: "small notice warn", id: "imp-warnings" }, ...p.warnings.map((x) => el("li", null, x))) : null;
    out.replaceChildren(h("section", { class: "card" },
      h("div", { class: "hd" }, h("h2", {}, p.file), h("span", { class: "badge mute more" }, p.format)),
      h("div", { class: "bd stack" }, warnings,
        h("label", { class: "field", for: "imp-kind" }, "What it is", kind),
        h("label", { class: "field", for: "imp-title" }, "Title", title),
        h("label", { class: "field", for: "imp-summary" }, "Summary", summary), whenRow,
        h("h3", {}, "What will come in"),
        h("pre", { class: "box", id: "imp-text" }, p.markdown || "(no text found)"),
        h("div", { class: "row wrap" }, add, button("Cancel", "", () => out.replaceChildren())), problem)));
    title.focus();
  }
}

// ---- Proposals: what in your notes needs you --------------------------
// Notes whose database changed since they were drafted (checked through
// Columbus, under the rules in force), and notes due for review. Nothing
// here changes your notes or your data until you choose an action.
// Text as the person sees it: no invisible characters (controls, zero-width
// and direction marks, Unicode tag characters, variation selectors), at
// most `max` characters.
const HIDDEN = /[\u0000-\u001f\u007f-\u009f\u00ad\u034f\u061c\u115f\u1160\u17b4\u17b5\u180b-\u180f\u200b-\u200f\u2028-\u202e\u2060-\u206f\u3164\ufe00-\ufe0f\ufeff\uffa0\ufff9-\ufffb]/g;
function shown(s, max) {
  // Tag characters (U+E0000–E0FFF) are the surrogate pairs \udb40–\udb43.
  const t = String(s ?? "").replace(/[\udb40-\udb43][\udc00-\udfff]/g, "").replace(HIDDEN, "");
  const chars = Array.from(t); // whole characters, never half a pair
  return chars.length > max ? `${chars.slice(0, max).join("")}…` : t;
}

// Changes your assistant suggested in a chat, waiting for you.
function suggestions(body) {
  const err = h("p", { class: "error-text", role: "alert", hidden: true });
  const list = h("div", { id: "sg-list", class: "stack" });
  body.append(h("section", { class: "card" }, h("div", { class: "hd" }, h("h3", {}, "From your assistant")),
    h("div", { class: "bd stack" },
      h("p", { class: "small muted" }, "Changes an assistant suggested in a chat, for you to accept or dismiss when you get to them. Nothing in a note changes until you accept; accepting checks the change again against the note as it is now."),
      err, list)));
  load();

  async function load() {
    showError(err, null);
    let r;
    try { r = await invoke("note_suggestions", {}); } catch (e) { showError(err, e); return; }
    const all = r.suggestions || [];
    if (!all.length) {
      list.replaceChildren(el("p", "muted small", "No suggestions waiting."));
      return;
    }
    list.replaceChildren(...all.map(card));
  }

  function card(sg) {
    const e2 = h("p", { class: "error-text small", role: "alert", hidden: true });
    const act = async (what) => {
      accept.disabled = dismiss.disabled = true;
      try {
        await invoke("note_suggestions", what === "accept" ? { accept: sg.id, shown: sg.shown } : { dismiss: sg.id });
        toast(what === "accept" ? "Done: the note is changed." : "Dismissed.");
        load();
      } catch (e) { showError(e2, e); accept.disabled = dismiss.disabled = false; }
    };
    const accept = button("Accept", "primary sm", () => act("accept"), { title: "Make these changes to your notes" });
    const dismiss = button("Dismiss", "sm", () => act("dismiss"), { title: "Drop the suggestion; nothing changes" });
    const when = (sg.created || "").slice(0, 16).replace("T", " ");
    const notes = (sg.notes || []).map((n) => {
      const flags = [];
      if (n.gone) flags.push("this note isn't there any more");
      if (n.changed_since) flags.push("the note changed after this was suggested");
      if (n.status === "reviewed") flags.push("you've reviewed this note: accepting changes it anyway");
      if ((n.carries || []).length) flags.push(`the note will count as holding data from ${n.carries.map((c) => c.replace(/^connection:/, "")).join(", ")}`);
      return h("div", { class: "stack" },
        h("b", {}, n.path
          ? h("a", { href: "#/documents/notes", title: n.path, onclick: () => { try { sessionStorage.setItem("columbus.note", n.path); } catch { /* none */ } } }, shown(n.title, 200))
          : shown(n.title, 200)),
        ...flags.map((f) => h("p", { class: "small notice warn" }, f)),
        h("pre", { class: "box" }, (n.lines || []).join("\n")));
    });
    if (sg.problem) accept.disabled = true;
    const extra = [];
    if (sg.problem) extra.push(h("p", { class: "notice warn small" }, `This can't be accepted now: ${shown(sg.problem, 400)}`));
    if ((sg.asks || []).length) extra.push(h("p", { class: "notice warn small" }, `Your organization's data rule${sg.asks.length > 1 ? "s" : ""} ${sg.asks.join(", ")} ask${sg.asks.length > 1 ? "" : "s"} for your approval for this: accepting is that approval.`));
    return h("div", { class: "tool stack" },
      h("p", { class: "small muted" }, `${shown(sg.client, 80)} · ${when}`),
      h("p", {}, h("span", { class: "muted" }, "Why, in the assistant's words: "), shown(sg.why, 600)),
      ...extra, ...notes, e2,
      h("div", { class: "row" }, accept, dismiss));
  }
}

function proposals(body) {
  suggestions(body);
  const err = h("p", { id: "pr-error", class: "error-text", role: "alert", hidden: true });
  const out = h("div", { id: "pr-results", class: "stack" });
  const check = button("Check my notes", "primary", run, { id: "pr-check" });
  body.append(h("section", { class: "card" }, h("div", { class: "bd stack" },
    h("p", {}, "Finds notes whose database changed since they were drafted (fields added, gone or of another type, or a dataset no longer there), and notes past their review date."),
    h("p", { class: "small muted" }, "It reads each connection's list of datasets through Columbus, under the rules in force now, like your assistant would; nothing is changed."),
    h("div", { class: "row" }, check), err)), out);
  run();

  async function run() {
    showError(err, null);
    check.disabled = true;
    check.textContent = "Checking…";
    out.replaceChildren(el("p", "muted small", "Checking your notes against your databases…"));
    let r;
    try {
      r = await invoke("notes_check");
    } catch (e) {
      out.replaceChildren();
      showError(err, e);
      return;
    } finally {
      check.disabled = false;
      check.textContent = "Check my notes";
    }
    draw(r);
  }

  function noteLink(n) {
    return h("a", { href: "#/documents/notes", title: n.path, onclick: () => { try { sessionStorage.setItem("columbus.note", n.path); } catch { /* none */ } } }, n.title || n.id);
  }

  function draw(r) {
    out.replaceChildren();
    if (!r.notes) { out.append(emptyState("No notes yet", "Draft notes from a connection (Data › a connection › Draft notes), or import a document (the Import tab).")); return; }
    for (const p of r.problems || []) out.append(h("p", { class: "notice warn small" }, `${p.connection} couldn't be checked: ${p.error}`));
    const changed = r.changed || [];
    const sec = h("section", { class: "card", id: "pr-changed" }, h("div", { class: "hd" }, h("h2", {}, `Changed since drafted (${changed.length})`)));
    const bd = h("div", { class: "bd flush" });
    sec.append(bd);
    if (!changed.length) bd.append(h("p", { class: "pad muted small" }, "Every note about a dataset matches what its database has now."));
    else {
      const ul = h("ul", { class: "list" });
      for (const n of changed) {
        const what = n.gone ? [`${n.dataset} is no longer there, or the connection no longer allows it`]
          : [n.added?.length ? `new: ${n.added.join(", ")}` : null,
            n.removed?.length ? `gone or now hidden: ${n.removed.join(", ")}` : null,
            ...(n.retyped || []).map((t) => `${t.field}: ${t.was} → ${t.now}`)].filter(Boolean);
        const err2 = h("p", { class: "error-text small", role: "alert", hidden: true });
        let act;
        if (n.untouched && !n.gone) {
          act = button("Draft again", "sm", async () => {
            act.disabled = true;
            try {
              await invoke("settings_draft_notes", { id: n.connection });
              toast(`Drafted ${n.connection}'s notes again.`);
              run();
            } catch (e) { showError(err2, e); act.disabled = false; }
          }, { title: "No one has edited this draft, so Columbus can simply draft it again from the database" });
        } else {
          act = button("Ask my assistant", "sm", () => {
            // Only what the person can see, and short enough to read before
            // sending: names come from the database and the note.
            const said = shown(what.join("; "), 1500);
            const text = `My note ${shown(n.id, 120)} about ${shown(n.connection, 120)}'s ${shown(n.dataset, 200)} no longer matches the database (${said}). Look at the dataset and tell me what to change in the note.`;
            try { sessionStorage.setItem("columbus.prefill", JSON.stringify({ text, at: Date.now() })); } catch { /* none */ }
            go("#/chats/fresh");
          }, { title: n.status === "reviewed" ? "It's reviewed (or edited), so it's yours to update: your assistant can look and tell you what to change" : "It's been edited, so drafting again would leave it alone" });
        }
        ul.append(h("li", { class: "tool" }, h("div", { class: "text" },
          h("b", {}, noteLink(n)),
          h("span", { class: "where" }, `${n.connection} · ${n.dataset} · ${n.status}${n.untouched ? ", not edited" : ""}`),
          h("span", { class: "small" }, what.join(" · ")), err2),
          h("div", { class: "acts" }, act)));
      }
      bd.append(ul);
    }
    out.append(sec);

    const due = r.due || [];
    const sec2 = h("section", { class: "card", id: "pr-due" }, h("div", { class: "hd" }, h("h2", {}, `Due for review (${due.length})`)));
    const bd2 = h("div", { class: "bd flush" });
    sec2.append(bd2);
    if (!due.length) bd2.append(h("p", { class: "pad muted small" }, `Every note was confirmed within the last ${plural(r.interval_months, "month")}.`));
    else {
      const ul = h("ul", { class: "list" });
      for (const n of due) {
        const err2 = h("p", { class: "error-text small", role: "alert", hidden: true });
        const shownText = h("div", { class: "stack", hidden: true });
        let act;
        if (n.status === "draft" || !String(n.path).endsWith(".md")) {
          // A draft is reviewed in Notes (Mark reviewed), not confirmed here;
          // nor is an entry that isn't a Markdown note.
          act = h("a", { class: "btn sm", href: "#/documents/notes", onclick: () => { try { sessionStorage.setItem("columbus.note", n.path); } catch { /* none */ } } }, "Review it");
        } else {
          // Confirmed only after reading it here: exactly the text shown.
          act = button("Read and confirm", "sm", async () => {
            act.disabled = true;
            let read;
            try { read = await invoke("note_read", { path: n.path }); } catch (e) { showError(err2, e); act.disabled = false; return; }
            const yes = button("Yes, it's still right", "primary sm", async () => {
              yes.disabled = true;
              try {
                await invoke("note_confirm", { path: n.path, sha256: read.sha256 });
                toast(`Confirmed ${n.title} as of today.`);
                run();
              } catch (e) { showError(err2, e); yes.disabled = false; }
            }, { title: "Its last-confirmed date becomes today; nothing else in it changes" });
            // Tabs and line breaks are fine; anything else shown() drops isn't.
            const plain = read.text.replace(/[\t\n\r]/g, " ");
            const unseen = shown(plain, Infinity) !== plain;
            shownText.replaceChildren(
              ...(unseen ? [h("p", { class: "notice warn small" }, "This note has characters you can't see (control, zero-width or hidden characters). Open it in Notes or an editor and check it before you confirm it.")] : []),
              h("pre", { class: "box" }, read.text), h("div", { class: "row" }, yes));
            shownText.hidden = false;
          }, { title: "Read the note; if it's still right, confirm it as of today" });
        }
        ul.append(h("li", { class: "tool" }, h("div", { class: "text" },
          h("b", {}, noteLink(n)),
          h("span", { class: "where" }, `last confirmed ${n.last_confirmed}, ${plural(n.months, "month")} ago · ${n.status}`), err2, shownText),
          h("div", { class: "acts" }, act)));
      }
      bd2.append(ul);
    }
    out.append(sec2);
  }
}
