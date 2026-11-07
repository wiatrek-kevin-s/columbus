// Projects: a piece of work with its own instructions for the assistant,
// the connections its chats may use and a default export folder. Columbus's
// server enforces the connections (a project only narrows what you have);
// the instructions go to the assistant in every chat of the project. Your
// projects are yours alone: nothing here is shared.
import { h, el, invoke, store, button, pageHead, showError, toast, emptyState, askConfirm, ago, plural } from "../core.js";

const go = (hash) => { location.hash = hash; };
const projHash = (id) => `#/projects/${encodeURIComponent(id)}`;
const MAX = 8000;

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

function connectionsText(p) {
  return p.connections && p.connections.length ? `Uses ${p.connections.join(", ")}` : "Uses all your connections";
}

export async function render(main, rest) {
  const [id] = (rest || []).map((x) => decodeURIComponent(x));
  const page = h("div", { class: "page" });
  main.append(page);
  const body = h("div", { class: "pad stack" });
  const err = h("p", { id: "pj-error", class: "error-text", role: "alert", hidden: true });
  let state;
  try { state = await load(); } catch (e) {
    page.append(pageHead("Projects"), h("div", { class: "scroll" }, body));
    body.append(err);
    showError(err, e);
    return;
  }
  if (id === "new") return formView(null);
  if (id) {
    const p = state.projects.find((x) => x.id === id);
    if (p) return formView(p);
    page.append(pageHead("Project"), h("div", { class: "scroll" }, body));
    body.append(h("p", { class: "small" }, h("a", { href: "#/projects" }, "← All projects")),
      emptyState("Not found", "This project isn't here any more (it may have been deleted). Its chats are listed under Unfiled."));
    return;
  }
  return listView();

  // ---- The list --------------------------------------------------------
  function listView() {
    page.append(pageHead("Projects", "A project is a piece of work: instructions your assistant follows in every chat of it, the connections those chats may use, and where its results go. Yours alone.",
      [button("New project", "primary", () => go("#/projects/new"), { id: "pj-new" })]));
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
        h("div", { class: "acts" }, button("New chat", "primary sm", () => go(`#/chats/fresh/${encodeURIComponent(p.id)}`), { title: `Start a chat in ${p.name}` })));
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
    body.append(h("p", { class: "small muted" }, "Data groups, documents and skills for each project come with their own pages. Sharing a project with your team comes later."));
  }

  // ---- A project: its settings, and its chats --------------------------
  function formView(p) {
    const isNew = !p;
    page.append(pageHead(isNew ? "New project" : p.name, isNew ? "Name it, tell your assistant how to work in it, and choose what its chats may use." : null,
      isNew ? [] : [button("New chat in this project", "primary", () => go(`#/chats/fresh/${encodeURIComponent(p.id)}`), { id: "pj-chat" })]));
    page.append(h("div", { class: "scroll" }, body));
    body.append(h("p", { class: "small" }, h("a", { href: "#/projects" }, "← All projects")), err);

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
    if (!state.connections.length) boxes.append(h("p", { class: "small muted" }, "You have no connections yet. ", h("a", { href: "#/connections" }, "Add one"), "."));
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
      body.append(h("p", { class: "small muted" }, "Project databases your assistant makes in this project's chats (", h("code", {}, "project_db"), ", scope project) belong to this project alone. Data groups, documents and skills for a project come with their own pages."));
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
