// Connections: the person's connections (ported from the Settings
// window), what each lets the assistant see, Test and Draft notes; the
// datasets known per connection; and project databases. Changes that give
// the assistant more access are confirmed at the keyboard in the app's own
// code (Touch ID, the account password or Windows Hello); nothing here can
// skip that.
import { $, h, el, invoke, store, button, pageHead, tabs, showError, size, when, plural, emptyState, coming, toast } from "../core.js";
import * as maplayers from "./maplayers.js";

const KIND_NAMES = {
  gpkg: "GeoPackage", sqlite: "SQLite", rasters: "Rasters folder", shapefile: "Shapefiles", filegdb: "File geodatabase",
  postgis: "PostGIS", sqlserver: "SQL Server", oracle: "Oracle", files: "Folder of files", arcgis: "ArcGIS service",
};

/** An ArcGIS connection that signs in (auth: oauth), its portal; else null. */
function oauthPortal(c) {
  return c.auth && typeof c.auth === "object" && c.auth.portal ? c.auth.portal : null;
}

let state = null; // settings_state
let editing = null;
let afterSave = null;

function where(c) {
  if (c.path) return c.path;
  if (c.url) return c.url;
  const host = [c.host, c.port].filter(Boolean).join(":");
  return [c.user && `${c.user}@`, host, c.database && `/${c.database}`, c.dsn && ` (${c.dsn})`].filter(Boolean).join("");
}

function sees(c) {
  const allow = (c.datasets && c.datasets.allow) || [];
  const deny = (c.datasets && c.datasets.deny) || [];
  const parts = [allow.length === 1 && allow[0] === "*" ? "every dataset" : `datasets ${allow.join(", ")}`];
  if (deny.length) parts.push(`except ${deny.join(", ")}`);
  parts.push(c.row_filter ? `only rows where ${c.row_filter}` : "every row");
  parts.push(c.mode === "read-write" ? "and may propose edits you approve" : "read only");
  return `Your assistant may see ${parts.join(", ")}.`;
}

export async function loadState() {
  state = await invoke("settings_state");
  return state;
}

function confirmNote() {
  if (!state) return "Changes that give your assistant more access ask you to confirm first.";
  return state.confirm_with === "not available on this system"
    ? "On this system, changes that give your assistant more access cannot be made here (there is no Touch ID or Windows Hello to confirm them): edit the settings files instead."
    : `Changes that give your assistant more access (a new connection, read and write, more datasets, a changed row filter) ask for ${state.confirm_with} first. Your assistant cannot change anything here.`;
}

export async function render(main, rest) {
  const tab = rest[0] || "databases";
  const page = h("div", { class: "page" });
  main.append(page);
  const add = tab === "maplayers"
    ? button("Add web layer", "primary", () => maplayers.openWebLayerEditor(null), { id: "add-web-layer" })
    : button("Add connection", "primary", () => openEditor(null), { id: "add" });
  page.append(pageHead("Connections", "Databases and files your assistant may use through Columbus, and what each lets it see.", [add]));
  page.append(tabs([
    ["databases", "Databases and files", "#/connections/databases"],
    ["datasets", "Datasets", "#/connections/datasets"],
    ["projectdbs", "Project databases", "#/connections/projectdbs"],
    ["maplayers", "Map layers", "#/connections/maplayers"],
    ["accounts", "Accounts and services", "#/connections/accounts"],
  ], tab, "Connections"));
  const body = h("div", { class: "pad stack" });
  page.append(h("div", { class: "scroll" }, body));
  if (tab === "maplayers") return maplayers.render(body);
  if (tab === "datasets") return datasets(body);
  if (tab === "projectdbs") return projectDbs(body);
  if (tab === "accounts") {
    body.append(coming("connections", "Accounts and services",
      "ArcGIS Online and Enterprise, GitHub, Microsoft 365 (SharePoint, OneDrive) and Google Drive, each signed in through your browser with its token kept in your computer's credential store, and each a connected server under Columbus's rules.",
      ["Today, connected servers are set up by your organization (server manifests).", "AI accounts will be under Settings › Models and accounts."], "Coming in a later stage"));
    return;
  }
  return databases(body);
}

async function databases(body) {
  const notice = h("p", { id: "restart", class: "notice ok", role: "status", hidden: true });
  const error = h("p", { id: "error", class: "error-text", role: "alert", hidden: true });
  const conf = h("div", { class: "notice" }, h("div", { id: "confirm-note" }, confirmNote()));
  const list = h("ul", { id: "connections", class: "list", "aria-live": "polite" });
  const none = h("div", { id: "no-conn", class: "pad", hidden: true }, h("p", { class: "muted" }, "No connections yet. Add a GeoPackage, a file geodatabase, a folder of shapefiles, rasters or other files (CSV, Excel, GeoJSON, KML), an ArcGIS service, or a database."));
  const other = h("p", { id: "other-folders", class: "error-text", hidden: true });
  const foot = h("p", { class: "small muted wrap-any" }, "Stored in ", h("code", { id: "conn-dir" }), ". Passwords are kept in your computer's credential store, never in a file, and never shown again.");
  body.append(notice, error, conf, h("section", { class: "card" }, h("div", { class: "hd" }, h("h2", {}, "Databases and files")), h("div", { class: "bd flush" }, list, none)), other,
    h("p", { class: "small muted" }, h("b", {}, "Draft notes"), " writes notes about a connection's datasets from the database itself (fields, keys, relationships, the codes short fields hold) into your notes folder, so your assistant knows the data. Review them under ", h("a", { href: "#/documents/notes" }, "Documents › My notes"), "."), foot);

  const draw = () => {
    $("confirm-note").textContent = confirmNote();
    $("conn-dir").textContent = state.connections_dir;
    const o = state.other_folders || [];
    other.hidden = o.length === 0;
    other.textContent = o.length ? `A running Columbus session reads its connections from ${o.join(", ")}, not from this folder, so changes here do not reach it. Clear the Connections folder setting of the Columbus extension in Claude desktop to use this one.` : "";
    list.replaceChildren();
    none.hidden = state.connections.length > 0;
    for (const c of state.connections) {
      const li = el("li", c.problem ? "conn bad" : "conn");
      const text = el("div", "text");
      if (c.problem) {
        text.append(el("b", null, "A connection file Columbus cannot read"), el("span", "where", `${c.file}: ${c.problem}`));
        li.append(text);
        list.append(li);
        continue;
      }
      text.append(h("div", { class: "row wrap" }, el("b", null, c.id), el("span", "where", `${KIND_NAMES[c.type] || c.type} · ${where(c)}`)));
      const chips = el("div", "chips");
      chips.append(el("span", c.mode === "read-write" ? "chip rw" : "chip ro", c.mode === "read-write" ? "Read and write" : "Read only"));
      const allow = (c.datasets && c.datasets.allow) || [];
      chips.append(el("span", "chip", allow.length === 1 && allow[0] === "*" ? "All datasets" : `Datasets: ${allow.join(", ")}`));
      if (c.datasets && c.datasets.deny && c.datasets.deny.length) chips.append(el("span", "chip", `Refused: ${c.datasets.deny.join(", ")}`));
      if (c.row_filter) chips.append(el("span", "chip mono", `Rows: ${c.row_filter}`));
      if (c.has_password) chips.append(el("span", "chip", "Password stored"));
      if (c.expires) chips.append(el("span", "chip", `Expires ${when(c.expires)}`));
      if (c.recursive) chips.append(el("span", "chip", "With subfolders"));
      if (c.crs) chips.append(el("span", "chip mono", c.crs));
      if (c.type === "arcgis") chips.append(el("span", "chip", oauthPortal(c) ? `Signs in at ${oauthPortal(c)}` : "Public (no sign-in)"));
      text.append(chips, el("span", "where", sees(c)));
      if (c.description) text.append(el("span", "where", c.description));
      const status = el("span", "where draft-status");
      status.setAttribute("role", "status");
      text.append(status);
      const acts = h("div", { class: "acts" });
      // Drafted notes don't apply to ArcGIS services.
      if (c.type !== "arcgis") {
        const d = button("Draft notes", "sm", () => draftNotes(c.id, d, status), { title: "Write notes about this connection's datasets from the database itself (fields, keys, relationships, codes), for you to fill in" });
        acts.append(d);
      }
      if (c.type === "arcgis" && oauthPortal(c)) {
        const s = button("Sign in", "sm", () => signIn(c.id, s, status), { title: `Sign in to ArcGIS at ${oauthPortal(c)} in your browser` });
        acts.append(s);
      }
      acts.append(button("Edit", "sm", () => openEditor(c)));
      li.append(text, acts);
      list.append(li);
    }
  };
  afterSave = async () => {
    await refresh();
    notice.textContent = state && state.restart_needed
      ? "Saved. The Columbus running in your AI app is an older version: quit your AI app (in Claude desktop: Cmd+Q or Quit) and open it again for your assistant to use the change."
      : "Saved. Your assistant uses the change from its next request; there is no need to restart anything. If you narrowed what a connection allows, files your assistant already made from it are removed, and anything waiting for your approval is asked again.";
    notice.hidden = false;
    showError(error, null);
  };
  const refresh = async () => {
    try { await loadState(); draw(); } catch (e) { showError(error, e); }
  };
  await refresh();
  return () => { afterSave = null; };
}

async function draftNotes(id, btn, status) {
  btn.disabled = true;
  btn.textContent = "Drafting…";
  status.textContent = "";
  status.classList.remove("error-text");
  try {
    const r = await invoke("settings_draft_notes", { id });
    status.textContent = r.message;
    status.classList.toggle("error-text", !r.ok);
  } catch (e) {
    status.textContent = String(e);
    status.classList.add("error-text");
  } finally {
    btn.disabled = false;
    btn.textContent = "Draft notes";
  }
}

// Sign in to an ArcGIS connection: after the person confirms at the
// keyboard, Columbus opens the portal's sign-in page in their browser and
// keeps the sign-in in the credential store; the page never sees it.
async function signIn(id, btn, status) {
  btn.disabled = true;
  btn.textContent = "Waiting for the browser…";
  status.textContent = "Sign in on the page that opens in your browser, then come back here.";
  status.classList.remove("error-text");
  try {
    status.textContent = await invoke("settings_arcgis_signin", { id });
  } catch (e) {
    status.textContent = String(e);
    status.classList.add("error-text");
  } finally {
    btn.disabled = false;
    btn.textContent = "Sign in";
  }
}

// ---------- The connection editor (one dialog for the whole window) ----------

function field(label, input, opts = {}) {
  const l = h("label", { class: `field${opts.wide ? " wide" : ""}`, "data-for": opts.for || null }, label, input);
  if (opts.help) l.append(h("small", { id: opts.helpId || null }, opts.help));
  return l;
}

function buildEditor() {
  const inp = (name, attrs = {}) => h("input", { name, autocomplete: "off", ...attrs });
  const kind = h("select", { name: "kind" },
    ...Object.entries({ gpkg: "GeoPackage file", filegdb: "File geodatabase (.gdb)", shapefile: "Folder of shapefiles", files: "Folder of files (CSV, Excel, GeoJSON, KML…)", sqlite: "SQLite file", rasters: "Folder of rasters (GeoTIFF)", arcgis: "ArcGIS Online, Portal or Server service", postgis: "PostgreSQL / PostGIS", sqlserver: "SQL Server", oracle: "Oracle" })
      .map(([v, t]) => h("option", { value: v }, t)));
  const ssl = h("select", { name: "sslmode" }, h("option", { value: "" }, "Default (prefer)"), ...["disable", "prefer", "require", "verify-full"].map((v) => h("option", { value: v }, v)));
  const mode = h("select", { name: "mode" }, h("option", { value: "read" }, "Read only"), h("option", { value: "read-write" }, "Read and write (needs edits allowed)"));
  const browse = h("button", { type: "button", id: "browse", class: "btn" }, "Browse…");
  const auth = h("select", { name: "auth" }, h("option", { value: "none" }, "None (a public service)"), h("option", { value: "oauth" }, "Sign in with ArcGIS (OAuth)"));
  const recursive = h("input", { type: "checkbox", name: "recursive" });
  const form = h("form", { id: "form", method: "dialog", class: "dlg" },
    h("h2", { id: "editor-h" }, "Add connection"),
    h("div", { class: "grid2" },
      field("Name", inp("conn_id", { required: true, maxlength: "40" }), { help: "How your assistant refers to it, e.g. roads or parcels-pg." }),
      field("Type", kind),
      field("Location", h("span", { class: "pick" }, inp("path"), browse), { wide: true, for: "gpkg sqlite shapefile filegdb rasters files" }),
      h("label", { class: "switch-row wide", "data-for": "files" }, recursive, h("span", {}, h("b", {}, "Read subfolders too."), " Files in the folder's subfolders are datasets as well, named by their path.")),
      field("Coordinate system", inp("crs", { placeholder: "e.g. EPSG:4326" }), { for: "files", help: "For CSV and Excel geometry, and files without one of their own." }),
      field("X column", inp("x", { placeholder: "e.g. EASTING" }), { for: "files", help: "With Y: the columns holding coordinates." }),
      field("Y column", inp("y", { placeholder: "e.g. NORTHING" }), { for: "files" }),
      field("Service address", inp("url", { placeholder: "https://services.arcgis.com/…/FeatureServer" }), { wide: true, for: "arcgis", help: "A FeatureServer or MapServer, or one layer in it (…/FeatureServer/3). Its host must be on your organization's allowed hosts." }),
      field("Sign-in", auth, { for: "arcgis" }),
      field("Features per page", inp("page_size", { type: "number", min: "1", placeholder: "the layer's maximum" }), { for: "arcgis" }),
      field("Portal", inp("portal", { placeholder: "https://www.arcgis.com" }), { for: "arcgis-oauth", help: "Where you sign in: ArcGIS Online or your organization's portal." }),
      field("Client id", inp("client_id"), { for: "arcgis-oauth", help: "Your organization's own ArcGIS app (your ArcGIS administrator has it). Not a secret." }),
      field("Host", inp("host"), { for: "postgis sqlserver oracle" }),
      field("Port", inp("port", { type: "number", min: "1", max: "65535" }), { for: "postgis sqlserver oracle" }),
      field("Database", inp("database"), { for: "postgis sqlserver" }),
      field("Service name (DSN)", inp("dsn"), { for: "oracle" }),
      field("Login", inp("user"), { for: "postgis sqlserver oracle" }),
      field("Password", inp("password", { type: "password", autocomplete: "new-password" }), { for: "postgis sqlserver oracle", help: "Kept in your computer's credential store, never in a file.", helpId: "pw-note" }),
      field("SSL", ssl, { for: "postgis" }),
      field("Schemas", inp("search_path", { placeholder: "public" }), { for: "postgis" }),
      field("Access", mode),
      field("Datasets allowed", inp("allow", { value: "*" }), { wide: true, help: "Names or patterns, separated by commas. * means all." }),
      field("Datasets refused", inp("deny"), { wide: true, help: "Wins over allowed, e.g. *audit*." }),
      field("Row filter", inp("row_filter", { placeholder: "e.g. district_nbr = 14" }), { wide: true, help: "Added to every query: your assistant sees only the rows it keeps." }),
      field("Description", inp("description"), { wide: true })),
    h("p", { id: "form-error", class: "error-text", role: "alert", hidden: true }),
    h("p", { id: "test-result", role: "status", hidden: true }),
    h("p", { id: "confirm-hint", class: "small muted" }),
    h("div", { class: "actions" },
      h("button", { type: "button", id: "delete", class: "btn danger", hidden: true }, "Remove"),
      h("span", { class: "spacer" }),
      h("button", { type: "button", id: "test", class: "btn", title: "Try opening it as Columbus would, without saving" }, "Test"),
      h("button", { type: "button", id: "cancel", class: "btn" }, "Cancel"),
      h("button", { type: "submit", id: "save", class: "btn primary" }, "Save")));
  const dlg = h("dialog", { id: "editor", "aria-labelledby": "editor-h" }, form);
  document.body.append(dlg);
  form.kind.addEventListener("change", () => setKind(form.kind.value));
  form.auth.addEventListener("change", () => setKind(form.kind.value));
  browse.addEventListener("click", async () => {
    try {
      const p = await invoke("settings_pick", { kind: form.kind.value });
      if (p) form.path.value = p;
    } catch (e) { showError($("form-error"), e); }
  });
  $("cancel").addEventListener("click", () => dlg.close());
  $("test").addEventListener("click", test);
  $("delete").addEventListener("click", remove);
  form.addEventListener("submit", save);
  return dlg;
}

function setKind(kind) {
  const form = $("form");
  const kinds = [kind];
  if (kind === "arcgis" && form.auth.value === "oauth") kinds.push("arcgis-oauth");
  for (const label of form.querySelectorAll("[data-for]")) label.hidden = !label.dataset.for.split(" ").some((k) => kinds.includes(k));
  $("browse").textContent = ["rasters", "shapefile", "filegdb", "files"].includes(kind) ? "Choose folder…" : "Choose file…";
  // Shapefiles, file geodatabases, folders of files and ArcGIS services
  // are read only here.
  const ro = ["shapefile", "filegdb", "files", "arcgis"].includes(kind);
  const mode = $("form").mode;
  if (ro) mode.value = "read";
  for (const o of mode.options) if (o.value === "read-write") o.disabled = ro;
}

function showTest(result) {
  const box = $("test-result");
  if (!result) { box.hidden = true; box.textContent = ""; return; }
  box.textContent = result.message;
  box.className = result.ok ? "test-ok" : "test-bad";
  box.hidden = false;
}

/** Add (conn null) or edit a connection. `onSaved` runs after a save. */
export async function openEditor(c, onSaved) {
  const dlg = $("editor") || buildEditor();
  const form = $("form");
  if (!state) { try { await loadState(); } catch { /* shown on save */ } }
  editing = c || null;
  dlg.onSaved = onSaved || null;
  form.reset();
  showError($("form-error"), null);
  showTest(null);
  $("editor-h").textContent = c ? `Edit ${c.id}` : "Add connection";
  $("confirm-hint").textContent = confirmNote();
  form.conn_id.readOnly = !!c;
  $("delete").hidden = !c;
  const v = c || { type: "gpkg", mode: "read", datasets: { allow: ["*"], deny: [] } };
  form.conn_id.value = v.id || "";
  form.kind.value = v.type || "gpkg";
  form.kind.disabled = !!c;
  for (const f of ["path", "host", "database", "dsn", "user", "sslmode", "search_path", "row_filter", "description", "crs", "x", "y", "url"]) form[f].value = v[f] || "";
  form.port.value = v.port || "";
  form.recursive.checked = !!v.recursive;
  form.page_size.value = v.page_size || "";
  form.auth.value = oauthPortal(v) ? "oauth" : "none";
  form.portal.value = oauthPortal(v) || "";
  form.client_id.value = (v.auth && v.auth.client_id) || "";
  form.mode.value = v.mode || "read";
  form.allow.value = ((v.datasets && v.datasets.allow) || ["*"]).join(", ");
  form.deny.value = ((v.datasets && v.datasets.deny) || []).join(", ");
  $("pw-note").textContent = c && c.has_password ? "A password is stored. Leave this empty to keep it." : "Kept in your computer's credential store, never in a file.";
  setKind(form.kind.value);
  dlg.showModal();
  (c ? form.mode : form.conn_id).focus();
}

// Comma-separated patterns; a comma inside {…} belongs to the pattern.
function list(s) {
  const out = [];
  let cur = "";
  let depth = 0;
  for (const ch of s) {
    if (ch === "{") depth += 1;
    if (ch === "}") depth = Math.max(0, depth - 1);
    if (ch === "," && depth === 0) { out.push(cur); cur = ""; } else cur += ch;
  }
  out.push(cur);
  return out.map((x) => x.trim()).filter(Boolean);
}

function formValue() {
  const form = $("form");
  return {
    existing: !!editing,
    id: form.conn_id.value.trim(),
    kind: form.kind.value,
    path: form.path.value,
    host: form.host.value,
    port: form.port.value ? Number(form.port.value) : null,
    database: form.database.value,
    dsn: form.dsn.value,
    user: form.user.value,
    password: form.password.value || null,
    sslmode: form.sslmode.value,
    search_path: form.search_path.value,
    mode: form.mode.value,
    allow: list(form.allow.value),
    deny: list(form.deny.value),
    row_filter: form.row_filter.value,
    description: form.description.value,
    recursive: form.recursive.checked,
    crs: form.crs.value,
    x: form.x.value,
    y: form.y.value,
    url: form.url.value,
    auth: form.auth.value,
    portal: form.portal.value,
    client_id: form.client_id.value,
    page_size: form.page_size.value ? Number(form.page_size.value) : null,
  };
}

async function save(ev) {
  ev.preventDefault();
  const btn = $("save");
  btn.disabled = true;
  showError($("form-error"), null);
  try {
    await invoke("settings_save_connection", { form: formValue() });
    const dlg = $("editor");
    dlg.close();
    if (afterSave) await afterSave();
    else toast("Saved. Your assistant uses the change from its next request.");
    if (dlg.onSaved) dlg.onSaved();
  } catch (e) {
    showError($("form-error"), e);
  } finally {
    btn.disabled = false;
    $("form").password.value = "";
  }
}

async function test() {
  const btn = $("test");
  btn.disabled = true;
  btn.textContent = "Testing…";
  showError($("form-error"), null);
  showTest(null);
  try {
    showTest(await invoke("settings_test_connection", { form: formValue() }));
  } catch (e) {
    showTest({ ok: false, message: String(e) });
  } finally {
    btn.disabled = false;
    btn.textContent = "Test";
  }
}

async function remove() {
  if (!editing) return;
  try {
    await invoke("settings_delete_connection", { id: editing.id, forgetPassword: true });
    $("editor").close();
    if (afterSave) await afterSave();
  } catch (e) {
    showError($("form-error"), e);
  }
}

// ---------- Datasets ----------
async function datasets(body) {
  body.append(h("div", { class: "notice" }, h("div", {}, "The datasets Columbus knows per connection, from your notes. ",
    h("b", {}, "List with Test"), " opens a saved connection as Columbus would (nothing is saved, your assistant sees none of it) and says which datasets its lists allow. Hidden fields never appear; refused tables are counted, never named.")));
  let st;
  let notes;
  try {
    [st, notes] = await Promise.all([loadState(), invoke("notes_list")]);
  } catch (e) {
    body.append(el("p", "error-text", String(e)));
    return;
  }
  const conns = st.connections.filter((c) => c.id);
  if (!conns.length) {
    body.append(emptyState("No connections yet", "Add a GeoPackage, a folder of rasters, or a database first.", h("a", { class: "btn primary", href: "#/connections" }, "Go to Connections")));
    return;
  }
  for (const c of conns) {
    const mine = notes.notes.filter((n) => n.connection === c.id || n.path.startsWith(`datasets/${c.id}/`));
    const result = h("p", { class: "small wrap-any", role: "status", hidden: true });
    const listBtn = button("List with Test", "sm", async () => {
      listBtn.disabled = true;
      result.hidden = false;
      result.className = "small muted";
      result.textContent = "Opening…";
      try {
        const r = await invoke("settings_test_connection", { form: { existing: true, id: c.id, kind: c.type, path: c.path, host: c.host, port: c.port, database: c.database, dsn: c.dsn, user: c.user, sslmode: c.sslmode, search_path: c.search_path, mode: c.mode, allow: c.datasets.allow, deny: c.datasets.deny, row_filter: c.row_filter, description: c.description, password: null,
          recursive: !!c.recursive, crs: c.crs, x: c.x, y: c.y, url: c.url, auth: oauthPortal(c) ? "oauth" : "none", portal: oauthPortal(c), client_id: c.auth && c.auth.client_id, page_size: c.page_size } });
        result.textContent = r.message;
        result.className = r.ok ? "small" : "small error-text";
      } catch (e) {
        result.textContent = String(e);
        result.className = "small error-text";
      } finally { listBtn.disabled = false; }
    });
    const tbody = h("tbody");
    for (const n of mine) {
      tbody.append(h("tr", {}, h("td", { class: "code" }, n.title || n.id || n.path),
        h("td", {}, h("span", { class: `badge ${n.status === "reviewed" ? "pass" : n.status === "draft" ? "mute" : "info"}` }, n.status || "note")),
        h("td", { class: "nowrap muted" }, n.last_confirmed || "")));
    }
    body.append(h("section", { class: "card" },
      h("div", { class: "hd" }, h("h2", {}, c.id), h("span", { class: "muted small" }, `${KIND_NAMES[c.type] || c.type} · ${plural(mine.length, "dataset")} with notes`), h("span", { class: "more" }, listBtn)),
      h("div", { class: "bd stack" }, result,
        mine.length ? h("table", { class: "t" }, h("thead", {}, h("tr", {}, h("th", {}, "Dataset"), h("th", {}, "Notes"), h("th", {}, "Last confirmed"))), tbody)
          : h("p", { class: "muted small" }, "No notes yet for this connection. Use Draft notes on the Databases and files tab, or List with Test to see what it allows."))));
  }
}

// ---------- Project databases ----------
async function projectDbs(body) {
  let r;
  try { r = await invoke("project_databases"); } catch (e) { body.append(el("p", "error-text", String(e))); return; }
  if (!r.files.length) {
    body.append(emptyState("No project databases yet",
      `Each project will get a local database the assistant can save working tables into (scratch space, never a source of record), exported to GeoPackage for ArcGIS Pro or QGIS. Large tables are not part of this version yet. When they are, they are listed here from ${r.dir}.`));
    return;
  }
  const tbody = h("tbody");
  for (const f of r.files) tbody.append(h("tr", {}, h("td", { class: "code" }, f.path), h("td", { class: "num nowrap" }, size(f.bytes)), h("td", { class: "nowrap muted" }, when(f.modified))));
  body.append(h("p", { class: "small muted wrap-any" }, `In ${r.dir}`),
    h("section", { class: "card" }, h("table", { class: "t" }, h("thead", {}, h("tr", {}, h("th", {}, "File"), h("th", { class: "num" }, "Size"), h("th", {}, "Changed"))), tbody)));
}

export { store };
