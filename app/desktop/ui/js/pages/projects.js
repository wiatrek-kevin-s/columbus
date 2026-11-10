// Projects: a piece of work with its own instructions for the assistant,
// the connections its chats may use and a default export folder. Columbus's
// server enforces the connections (a project only narrows what you have);
// the instructions go to the assistant in every chat of the project. Your
// projects are yours alone: nothing here is shared.
import { h, el, invoke, store, button, pageHead, showError, toast, emptyState, askConfirm, ago, plural, size } from "../core.js";

const go = (hash) => { location.hash = hash; };
const projHash = (id) => `#/projects/${encodeURIComponent(id)}`;
const MAX = 8000;

/** The project databases of a project (or, with null, the shared ones
 * from before projects), listed by Columbus's server for that project. The
 * card fills in when the list arrives. */
function dbsCard(projectId) {
  const bd = h("div", { class: "bd flush" }, h("p", { class: "pad muted small" }, "Looking for project databases…"));
  const title = h("h2", {}, projectId ? "Project databases" : "Project databases outside projects");
  const card = h("section", { class: "card", id: "pj-dbs" }, h("div", { class: "hd" }, title), bd);
  invoke("project_dbs", { project: projectId }).then((r) => {
    const dbs = r.databases || [];
    if (!dbs.length) {
      if (!projectId) { card.remove(); return; }
      bd.replaceChildren(h("p", { class: "pad muted small" }, "None yet. Your assistant makes one when you ask it to join or keep large tables (project_db, scope project); it belongs to this project alone."));
      return;
    }
    title.textContent = `${projectId ? "Project databases" : "Project databases outside projects"} (${dbs.length})`;
    const ul = h("ul", { class: "list", id: "pj-db-list" });
    for (const d of dbs) {
      const tables = d.tables || [];
      const facts = [tables.length ? plural(tables.length, "table") : "no tables yet", d.bytes != null ? size(d.bytes) : null].filter(Boolean).join(" · ");
      const li = h("li", { class: "tool" }, h("div", { class: "text" },
        h("b", {}, String(d.db)),
        h("span", { class: "where" }, facts),
        d.description ? h("span", { class: "muted small" }, String(d.description)) : null,
        tables.length ? h("span", { class: "muted small wrap-any" }, tables.join(", ")) : null,
        d.withheld ? h("span", { class: "small error-text" }, `Withheld here: ${d.withheld}.`) : null),
      h("div", { class: "acts" }, d.withheld ? el("span", "badge warn", "withheld") : el("span", "badge pass", "usable")));
      ul.append(li);
    }
    bd.replaceChildren(ul, h("p", { class: "pad small muted" }, projectId
      ? "Your assistant reads and adds to these in this project's chats; ask it to describe one or delete it (deleting asks you first)."
      : "Made before projects, or in chats not in a project: usable in chats outside projects."));
  }).catch((e) => {
    bd.replaceChildren(h("p", { class: "pad small error-text" }, `Couldn't list them: ${e}`));
  });
  return card;
}

async function load() {
  const s = await invoke("projects_state");
  store.set("projects", s);
  return s;
}

function chatsOf(id) {
  return (store.chats || []).filter((c) => c.project === id);
}

function lastActivity(p) {
  const times = chatsOf(p.id).map((c) => c.updated).filter(Boolean);
  if (p.updated) times.push(p.updated);
  times.sort();
  return times.length ? times[times.length - 1] : null;
}

/** The data groups the project form offers (loaded with the page). */
let groupsCache = { mine: [], org: [] };
function state_groups() { return groupsCache; }

function connectionsText(p) {
  const c = p.connections && p.connections.length ? `Uses ${p.connections.join(", ")}` : "Uses all your connections";
  return p.data_groups && p.data_groups.length ? `${c} · data groups ${p.data_groups.join(", ")}` : c;
}

export async function render(main, rest) {
  const [id] = (rest || []).map((x) => decodeURIComponent(x));
  const page = h("div", { class: "page" });
  main.append(page);
  const body = h("div", { class: "pad stack" });
  const err = h("p", { id: "pj-error", class: "error-text", role: "alert", hidden: true });
  let state;
  try {
    state = await load();
    try {
      const g = await invoke("datagroups_state");
      store.set("datagroups", g);
      groupsCache = { mine: g.groups, org: g.organization };
    } catch { groupsCache = { mine: [], org: [] }; }
  } catch (e) {
    page.append(pageHead("Projects"), h("div", { class: "scroll" }, body));
    body.append(err);
    showError(err, e);
    return;
  }
  if (id === "new") return formView(null);
  if (id) {
    const p = state.projects.find((x) => x.id === id);
    if (p) return formView(p);
    page.append(pageHead("Project", null, [], { href: "#/projects", label: "All projects" }), h("div", { class: "scroll" }, body));
    body.append(emptyState("Not found", "This project isn't here any more (it may have been deleted). Its chats are listed under Unfiled."));
    return;
  }
  return listView();

  // ---- The list --------------------------------------------------------
  function listView() {
    page.append(pageHead("Projects", "A project is a piece of work: instructions your assistant follows in every chat of it, the connections those chats may use, and where its results go. Yours alone.",
      [button("New project", "primary sm", () => go("#/projects/new"), { id: "pj-new" })], { href: "#/chats", label: "Chats" }));
    page.append(h("div", { class: "scroll" }, body));
    body.append(err);
    const ul = h("ul", { class: "list", id: "pj-list" });
    for (const p of state.projects) {
      const n = chatsOf(p.id).length;
      const last = lastActivity(p);
      const li = el("li", "tool clickable");
      li.dataset.id = p.id;
      li.addEventListener("click", (ev) => { if (!ev.target.closest("button, a")) go(projHash(p.id)); });
      li.append(h("div", { class: "text" },
        h("b", {}, h("a", { href: projHash(p.id) }, p.name)),
        h("span", { class: "where" }, `${plural(n, "chat")}${last ? ` · active ${ago(last)}` : ""} · ${connectionsText(p)}`),
        p.instructions ? h("span", { class: "muted small" }, p.instructions.length > 140 ? `${p.instructions.slice(0, 140)}…` : p.instructions) : null),
        h("div", { class: "acts" }, button("New chat", "sm", () => go(`#/chats/fresh/${encodeURIComponent(p.id)}`), { title: `Start a chat in ${p.name}` })));
      ul.append(li);
    }
    if (state.projects.length) {
      body.append(h("section", { class: "card" }, h("div", { class: "hd" }, h("h2", {}, "Your projects")), h("div", { class: "bd flush" }, ul)));
    } else {
      body.append(emptyState("No projects yet",
        "Make one for a piece of work you'll come back to: give it instructions (\"report lengths in miles; district 14 only\"), choose which connections its chats may use, and start its chats from here.",
        button("New project", "primary", () => go("#/projects/new"))));
    }
    const unfiled = (store.chats || []).filter((c) => !c.project || !state.projects.some((p) => p.id === c.project)).length;
    if (unfiled) body.append(h("p", { class: "small muted" }, h("a", { href: "#/chats" }, `${plural(unfiled, "chat")} not in a project`), " are under Unfiled on the Chats page."));
    if (state.problems.length) {
      body.append(h("section", { class: "card" }, h("div", { class: "hd" }, h("h2", {}, "Projects that could not be read")),
        h("div", { class: "bd" }, h("ul", { class: "small" }, ...state.problems.map(([pid, why]) => el("li", null, `${pid}: ${why}`))),
          h("p", { class: "small muted" }, "Chats can't start in these until their files are fixed or removed."))));
    }
    body.append(dbsCard(null));
    body.append(h("p", { class: "small muted" }, "Attach data groups on a project's page."));
  }

  // ---- A project: its settings, and its chats --------------------------
  function formView(p) {
    const isNew = !p;
    page.append(pageHead(isNew ? "New project" : p.name, isNew ? "Name it, tell your assistant how to work in it, and choose what its chats may use." : null,
      isNew ? [] : [button("New chat in this project", "primary sm", () => go(`#/chats/fresh/${encodeURIComponent(p.id)}`), { id: "pj-chat" })], { href: "#/projects", label: "All projects" }));
    page.append(h("div", { class: "scroll" }, body));
    body.append(err);

    const name = h("input", { id: "pj-name", type: "text", maxlength: "80", value: p ? p.name : "", placeholder: "Roadway inventory QC, 2027", class: "grow" });
    const instr = h("textarea", { id: "pj-instructions", rows: "7", maxlength: String(MAX), placeholder: "For example: Work only on District 14. Report lengths in miles to two decimals. Leave retired segments (STATUS = 'R') out." });
    instr.value = p ? p.instructions || "" : "";
    const count = h("span", { class: "muted small", id: "pj-count" });
    const counted = () => { count.textContent = `${instr.value.length.toLocaleString()} of ${MAX.toLocaleString()} characters`; };
    instr.addEventListener("input", counted);
    counted();

    const some = p && p.connections && p.connections.length;
    const allR = h("input", { type: "radio", name: "pj-conn", id: "pj-conn-all", value: "all" });
    const someR = h("input", { type: "radio", name: "pj-conn", id: "pj-conn-some", value: "some" });
    allR.checked = !some;
    someR.checked = !!some;
    const boxes = h("div", { class: "stack", id: "pj-conns" });
    const missing = (p?.connections || []).filter((c) => !state.connections.includes(c));
    for (const c of [...state.connections, ...missing]) {
      const box = h("input", { type: "checkbox", value: c, id: `pj-c-${c}` });
      box.checked = !!(p && p.connections && p.connections.includes(c));
      box.addEventListener("change", () => { if (box.checked) someR.checked = true; });
      boxes.append(h("label", { class: "row", for: `pj-c-${c}` }, box, " ", c,
        missing.includes(c) ? h("span", { class: "badge warn" }, "no longer one of your connections") : null));
    }
    if (!state.connections.length) boxes.append(h("p", { class: "small muted" }, "You have no connections yet. ", h("a", { href: "#/data" }, "Add one"), "."));
    const syncBoxes = () => { boxes.classList.toggle("muted", allR.checked); for (const b of boxes.querySelectorAll("input")) b.disabled = allR.checked; };
    allR.addEventListener("change", syncBoxes);
    someR.addEventListener("change", syncBoxes);
    syncBoxes();

    const folder = h("select", { id: "pj-folder" }, h("option", { value: "" }, "None: the assistant asks where to export"));
    for (const f of state.export_folders) {
      const o = h("option", { value: f.id }, `${f.id} (${f.path})${f.granted ? "" : ", each export is approved"}`);
      o.selected = !!(p && p.export_folder === f.id);
      folder.append(o);
    }
    if (p && p.export_folder && !state.export_folders.some((f) => f.id === p.export_folder)) {
      folder.append(h("option", { value: p.export_folder, selected: true }, `${p.export_folder} (no longer one of your organization's export folders)`));
    }

    // Data groups: yours and your organization's.
    const groups = h("div", { class: "stack", id: "pj-groups" });
    const dg = state_groups();
    const chosen = new Set(p ? p.data_groups || [] : []);
    const known = new Set([...dg.mine.map((g) => g.id), ...dg.org.map((g) => g.id)]);
    for (const [gid, label, note] of [
      ...dg.mine.map((g) => [g.id, g.name, `${g.datasets.length} datasets · yours`]),
      ...dg.org.map((g) => [g.id, g.title || g.id, "your organization's"]),
      ...[...chosen].filter((x) => !known.has(x)).map((x) => [x, x, "no longer found: chats won't start until you remove it"]),
    ]) {
      const box = h("input", { type: "checkbox", value: gid, id: `pj-g-${gid}` });
      box.checked = chosen.has(gid);
      groups.append(h("label", { class: "row", for: `pj-g-${gid}` }, box, " ", label, h("span", { class: "muted small" }, ` (${note})`)));
    }
    if (!known.size && !chosen.size) groups.append(h("p", { class: "small muted" }, "You have no data groups yet. ", h("a", { href: "#/datagroups/new" }, "Make one"), "."));

    const save = button(isNew ? "Create project" : "Save changes", "primary", onSave, { id: "pj-save" });
    const acts = h("div", { class: "row wrap" }, save, isNew ? button("Cancel", "", () => go("#/projects")) : null,
      h("span", { class: "spacer" }), isNew ? null : button("Delete project", "danger", onDelete, { id: "pj-delete" }));
    body.append(h("section", { class: "card" }, h("div", { class: "bd stack" },
      h("label", { class: "field", for: "pj-name" }, "Name", name),
      h("label", { class: "field", for: "pj-instructions" }, "Instructions for your assistant", instr, count),
      h("p", { class: "small muted" }, "Given to the assistant in every chat of this project. They guide it; they never loosen Columbus's rules."),
      h("fieldset", { class: "stack" }, h("legend", { class: "field" }, "Connections its chats may use"),
        h("label", { class: "row", for: "pj-conn-all" }, allR, " All my connections"),
        h("label", { class: "row", for: "pj-conn-some" }, someR, " Only these:"), boxes,
        h("p", { class: "small muted" }, "Columbus's server enforces this: in this project's chats, your other Columbus connections can't be read. (Connected apps, such as a drive, aren't narrowed by projects yet.)")),
      h("fieldset", { class: "stack" }, h("legend", { class: "field" }, "Data groups"),
        h("p", { class: "small muted" }, "Choose any to keep this project's chats to their datasets, and only the rows their filters keep. None: every dataset its connections allow."),
        groups),
      h("label", { class: "field", for: "pj-folder" }, "Default export folder", folder),
      acts)));

    if (!isNew) {
      const chats = chatsOf(p.id);
      const ul = h("ul", { class: "list", id: "pj-chats" });
      for (const c of chats) {
        const href = `#/chats/c/${encodeURIComponent(c.id)}`;
        const li = h("li", { class: "clickable" }, h("div", { class: "text" }, h("b", {}, c.title),
          h("span", { class: "where" }, `${plural(c.messages, "message")} · ${ago(c.updated)}`)),
          c.live ? h("span", { class: "badge info" }, "Open now") : null, h("a", { class: "btn sm", href }, "Open"));
        li.addEventListener("click", (e) => { if (e.target.tagName !== "A") go(href); });
        ul.append(li);
      }
      body.append(h("section", { class: "card" }, h("div", { class: "hd" }, h("h2", {}, `Chats (${chats.length})`)),
        h("div", { class: "bd flush" }, chats.length ? ul : h("p", { class: "pad muted small" }, "No chats yet. Start one with New chat in this project."))));
      body.append(dbsCard(p.id));
    }
    name.focus();

    async function onSave() {
      showError(err, null);
      const form = {
        id: p ? p.id : null,
        name: name.value,
        instructions: instr.value,
        connections: someR.checked ? [...boxes.querySelectorAll("input:checked")].map((b) => b.value) : [],
        export_folder: folder.value || null,
        data_groups: [...groups.querySelectorAll("input:checked")].map((b) => b.value),
      };
      if (someR.checked && !form.connections.length) { showError(err, "Choose at least one connection, or All my connections."); return; }
      save.disabled = true;
      try {
        const saved = await invoke("project_save", { form });
        await load();
        toast(isNew ? `Created ${saved.name}.` : "Saved. New chats in this project use it; a chat already open keeps what it started with.");
        if (isNew) go(projHash(saved.id));
        else save.disabled = false;
      } catch (e) {
        showError(err, e);
        save.disabled = false;
      }
    }
    async function onDelete() {
      const ok = await askConfirm(`Delete ${p.name}?`, "The project's settings are deleted. Its chats are kept, under Unfiled, and its project databases stay in your workspaces folder.", "Delete project", true);
      if (!ok) return;
      try {
        await invoke("project_remove", { id: p.id });
        await load();
        toast(`Deleted ${p.name}.`);
        go("#/projects");
      } catch (e) { showError(err, e); }
    }
  }
}
