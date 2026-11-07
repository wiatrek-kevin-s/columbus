// Data groups: a named set of datasets across your connections, each with
// an optional filter on its rows. Attach one to a project and that
// project's chats see only those datasets, and only those rows: Columbus's
// server enforces it. A group can only narrow what each connection allows.
// Your groups are yours alone; your organization's come from its context
// pack and are read only.
import { h, el, invoke, store, button, pageHead, showError, toast, emptyState, askConfirm, plural } from "../core.js";

const go = (hash) => { location.hash = hash; };
const gHash = (id) => `#/datagroups/${encodeURIComponent(id)}`;

async function load() {
  const s = await invoke("datagroups_state");
  store.set("datagroups", s);
  return s;
}

/** The projects that name a group. */
function usedBy(id) {
  return (store.projects?.projects || []).filter((p) => (p.data_groups || []).includes(id));
}

function connectionsOf(g) {
  return [...new Set(g.datasets.map((d) => d.connection))];
}

export async function render(main, rest) {
  const parts = (rest || []).map((x) => decodeURIComponent(x));
  const page = h("div", { class: "page" });
  main.append(page);
  const body = h("div", { class: "pad stack" });
  const err = h("p", { id: "dg-error", class: "error-text", role: "alert", hidden: true });
  let state;
  try {
    state = await load();
    if (!store.projects) store.set("projects", await invoke("projects_state"));
  } catch (e) {
    page.append(pageHead("Data groups"), h("div", { class: "scroll" }, body));
    body.append(err);
    showError(err, e);
    return;
  }
  if (parts[0] === "org" && parts[1]) return orgView(parts[1]);
  if (parts[0] === "new") return formView(null);
  if (parts[0]) {
    const g = state.groups.find((x) => x.id === parts[0]);
    if (g) return formView(g);
    page.append(pageHead("Data group"), h("div", { class: "scroll" }, body));
    body.append(h("p", { class: "small" }, h("a", { href: "#/datagroups" }, "← All data groups")),
      emptyState("Not found", "This data group isn't here any more (it may have been deleted)."));
    return;
  }
  return listView();

  // ---- The list --------------------------------------------------------
  function listView() {
    page.append(pageHead("Data groups", "Named sets of datasets across your connections, each with an optional filter on its rows. Attach one to a project and its chats see only those datasets, and only those rows.",
      [button("New data group", "primary", () => go("#/datagroups/new"), { id: "dg-new" })]));
    page.append(h("div", { class: "scroll" }, body));
    body.append(err);
    const ul = h("ul", { class: "list", id: "dg-list" });
    for (const g of state.groups) {
      const li = el("li", "tool clickable");
      li.dataset.id = g.id;
      li.addEventListener("click", (ev) => { if (!ev.target.closest("a, button")) go(gHash(g.id)); });
      const used = usedBy(g.id);
      li.append(h("div", { class: "text" },
        h("b", {}, h("a", { href: gHash(g.id) }, g.name)),
        h("span", { class: "where" }, `${plural(g.datasets.length, "dataset")} from ${connectionsOf(g).join(", ")} · ${used.length ? `used by ${used.map((p) => p.name).join(", ")}` : "not used by a project yet"}`),
        g.description ? h("span", { class: "muted small" }, g.description) : null));
      ul.append(li);
    }
    body.append(h("section", { class: "card" }, h("div", { class: "hd" }, h("h2", {}, "Your data groups")),
      h("div", { class: "bd flush" }, state.groups.length ? ul
        : h("div", { class: "pad" }, h("p", { class: "muted small" }, "None yet. Make one for a set of datasets you use together, for example the District 14 roadway core: the inventory, the centerline (without retired segments) and the signs.")))));
    if (state.organization.length) {
      const ol = h("ul", { class: "list", id: "dg-org" });
      for (const g of state.organization) {
        const href = `#/datagroups/org/${encodeURIComponent(g.id)}`;
        const li = el("li", "tool clickable");
        li.addEventListener("click", (ev) => { if (!ev.target.closest("a")) go(href); });
        li.append(h("div", { class: "text" }, h("b", {}, h("a", { href }, g.title || g.id)),
          h("span", { class: "where" }, `${plural(g.datasets.length, "dataset")} · from your organization's context pack, read only`),
          g.summary ? h("span", { class: "muted small" }, g.summary) : null));
        ol.append(li);
      }
      body.append(h("section", { class: "card" }, h("div", { class: "hd" }, h("h2", {}, "Your organization's")), h("div", { class: "bd flush" }, ol)));
    }
    if (state.problems.length) {
      body.append(h("section", { class: "card" }, h("div", { class: "hd" }, h("h2", {}, "Data groups that could not be read")),
        h("div", { class: "bd" }, h("ul", { class: "small" }, ...state.problems.map(([gid, why]) => el("li", null, `${gid}: ${why}`))),
          h("p", { class: "small muted" }, "A project that names one of these can't start a chat until it's fixed or removed from the project."))));
    }
    body.append(h("p", { class: "small muted" }, "Attach a group to a project on the project's page. You can mention one to your assistant by its id; it can read it with describe_data_group. Sharing groups with your team comes later."));
  }

  // ---- One of the organization's, read only ----------------------------
  function orgView(id) {
    const g = state.organization.find((x) => x.id === id);
    page.append(pageHead(g ? (g.title || g.id) : "Data group", g ? g.summary : null));
    page.append(h("div", { class: "scroll" }, body));
    body.append(h("p", { class: "small" }, h("a", { href: "#/datagroups" }, "← All data groups")));
    if (!g) { body.append(emptyState("Not found", "Your organization's context pack doesn't have this group.")); return; }
    body.append(h("section", { class: "card" }, h("div", { class: "bd stack" },
      h("p", { class: "small muted" }, `Id: ${g.id} · from your organization's context pack: read only here; its owners change it.`),
      h("h3", {}, "Datasets (context pack entries)"),
      h("ul", { class: "small" }, ...g.datasets.map((d) => el("li", null, typeof d === "string" ? d : JSON.stringify(d)))),
      h("p", { class: "small muted" }, "Its filters and how its datasets join are in the pack; your assistant reads them with describe_data_group."))));
    const used = usedBy(g.id);
    if (used.length) body.append(h("p", { class: "small" }, "Used by ", ...used.flatMap((p, i) => [i ? ", " : "", h("a", { href: `#/projects/${encodeURIComponent(p.id)}` }, p.name)])));
  }

  // ---- One of yours: made or edited here --------------------------------
  function formView(g) {
    const isNew = !g;
    page.append(pageHead(isNew ? "New data group" : g.name, isNew ? "Name it, then list the datasets it holds and, for any of them, which rows." : null));
    page.append(h("div", { class: "scroll" }, body));
    body.append(h("p", { class: "small" }, h("a", { href: "#/datagroups" }, "← All data groups")), err);
    const name = h("input", { id: "dg-name", type: "text", maxlength: "80", value: g ? g.name : "", placeholder: "District 14 roadway core", class: "grow" });
    const desc = h("textarea", { id: "dg-description", rows: "2", maxlength: "2000", placeholder: "What it's for (optional)" });
    desc.value = g ? g.description || "" : "";
    const rows = h("tbody", { id: "dg-rows" });
    const conns = state.connections.map((c) => c.id);
    const addRow = (m = { connection: conns[0] || "", dataset: "", filter: "" }) => {
      const sel = h("select", { class: "dg-conn", "aria-label": "Connection" });
      for (const c of new Set([...conns, m.connection].filter(Boolean))) {
        const o = h("option", { value: c }, conns.includes(c) ? c : `${c} (no longer one of your connections)`);
        o.selected = c === m.connection;
        sel.append(o);
      }
      const ds = h("input", { type: "text", class: "dg-dataset grow", value: m.dataset || "", placeholder: "Dataset name, as the connection lists it", "aria-label": "Dataset" });
      const filter = h("input", { type: "text", class: "dg-filter grow", value: m.filter || "", placeholder: "Optional: which rows, e.g. STATUS <> 'R'", "aria-label": "Filter" });
      const tr = h("tr", {}, h("td", {}, sel), h("td", {}, ds), h("td", {}, filter),
        h("td", {}, button("Remove", "sm", () => tr.remove(), { title: "Remove this dataset from the group" })));
      rows.append(tr);
      return ds;
    };
    for (const m of g ? g.datasets : []) addRow(m);
    if (isNew) addRow();
    const add = button("Add dataset", "sm", () => addRow().focus(), { id: "dg-add" });
    if (!conns.length) add.disabled = true;
    const save = button(isNew ? "Create data group" : "Save changes", "primary", onSave, { id: "dg-save" });
    body.append(h("section", { class: "card" }, h("div", { class: "bd stack" },
      h("label", { class: "field", for: "dg-name" }, "Name", name),
      h("label", { class: "field", for: "dg-description" }, "Description", desc),
      h("div", { class: "field" }, "Datasets",
        h("table", { class: "t dg-table" }, h("thead", {}, h("tr", {}, h("th", {}, "Connection"), h("th", {}, "Dataset"), h("th", {}, "Rows (filter)"), h("th", {}, ""))), rows),
        h("div", { class: "row" }, add)),
      conns.length ? null : h("p", { class: "small muted" }, "You have no connections yet. ", h("a", { href: "#/connections" }, "Add one"), " first."),
      h("p", { class: "small muted" }, "A filter is a condition on the dataset's own fields (=, <>, <, >, IN, LIKE, AND, OR, IS NULL). It is added to the connection's own row filter, never instead of it. A dataset the connection doesn't allow stays unreadable."),
      h("div", { class: "row wrap" }, save, isNew ? button("Cancel", "", () => go("#/datagroups")) : null, h("span", { class: "spacer" }),
        isNew ? null : button("Delete data group", "danger", onDelete, { id: "dg-delete" })))));
    if (!isNew) {
      const used = usedBy(g.id);
      body.append(h("p", { class: "small", id: "dg-used" }, used.length ? ["Used by ", ...used.flatMap((p, i) => [i ? ", " : "", h("a", { href: `#/projects/${encodeURIComponent(p.id)}` }, p.name)]), ". A change applies to their new chats."] : "Not used by a project yet. Attach it on a project's page."),
        h("p", { class: "small muted" }, `Id: ${g.id} (mention it to your assistant by this).`));
    }
    name.focus();

    async function onSave() {
      showError(err, null);
      const datasets = [...rows.querySelectorAll("tr")].map((tr) => ({
        connection: tr.querySelector(".dg-conn").value,
        dataset: tr.querySelector(".dg-dataset").value.trim(),
        filter: tr.querySelector(".dg-filter").value.trim() || null,
      })).filter((d) => d.dataset);
      if (!datasets.length) { showError(err, "List at least one dataset."); return; }
      save.disabled = true;
      try {
        const saved = await invoke("datagroup_save", { form: { id: g ? g.id : null, name: name.value, description: desc.value, datasets } });
        await load();
        toast(isNew ? `Created ${saved.name}.` : "Saved. New chats in projects that use it use the change.");
        if (isNew) go(gHash(saved.id));
        else save.disabled = false;
      } catch (e) {
        showError(err, e);
        save.disabled = false;
      }
    }
    async function onDelete() {
      const used = usedBy(g.id);
      const ok = await askConfirm(`Delete ${g.name}?`,
        used.length ? `It's used by ${used.map((p) => p.name).join(", ")}: their chats won't start until you remove it from them, on their pages.` : "It isn't used by a project.",
        "Delete data group", true);
      if (!ok) return;
      try {
        await invoke("datagroup_remove", { id: g.id });
        await load();
        toast(`Deleted ${g.name}.`);
        go("#/datagroups");
      } catch (e) { showError(err, e); }
    }
  }
}
