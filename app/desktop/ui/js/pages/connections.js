// Data: what the assistant can reach (the person's connections, ported
// from the Settings window), what each lets it see, and where results can
// go; one connection's own page with its rules in plain words, Test, Draft
// notes and Sign in; the datasets known per connection; project databases
// and map layers. Changes that give the assistant more access are confirmed
// at the keyboard in the app's own code (Touch ID, the account password or
// Windows Hello); nothing here can skip that.
import { $, h, el, icon, invoke, store, button, pageHead, placeHead, sec, showError, size, when, plural, emptyState, coming, toast } from "../core.js";
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

// The same, in a line, for the foot of a connection's page.
function confirmShort() {
  if (!state) return "Loosening a rule asks you to confirm first.";
  return state.confirm_with === "not available on this system"
    ? "Here, loosening a rule is done in the settings files: there is no Touch ID or Windows Hello to confirm it."
    : `Loosening a rule asks for ${state.confirm_with} first. Your assistant cannot change these.`;
}

const BACK = { href: "#/data", label: "Data" };
const FILE_KINDS = ["gpkg", "sqlite", "rasters", "shapefile", "filegdb", "files"];
const connHash = (id) => `#/data/c/${encodeURIComponent(id)}`;

/** What a connection is drawn with: a file, a service or a database. */
function glyph(c) {
  return h("span", { class: "glyph" }, icon(c.type === "arcgis" ? "registry" : FILE_KINDS.includes(c.type) ? "files" : "connections"));
}

/** A connection's rules as short facts (the words its chips carried). */
function facts(c) {
  const out = [c.mode === "read-write" ? "Read and write" : "Read only"];
  const allow = (c.datasets && c.datasets.allow) || [];
  out.push(allow.length === 1 && allow[0] === "*" ? "All datasets" : `Datasets: ${allow.join(", ")}`);
  if (c.datasets && c.datasets.deny && c.datasets.deny.length) out.push(`Refused: ${c.datasets.deny.join(", ")}`);
  if (c.row_filter) out.push(`Rows: ${c.row_filter}`);
  if (c.has_password) out.push("Password stored");
  if (c.expires) out.push(`Expires ${when(c.expires)}`);
  if (c.recursive) out.push("With subfolders");
  if (c.crs) out.push(c.crs);
  if (c.type === "arcgis") out.push(oauthPortal(c) ? `Signs in at ${oauthPortal(c)}` : "Public (no sign-in)");
  return out;
}

/** One line for a connection's row: what it is, and the rule that matters. */
function brief(c) {
  const allow = (c.datasets && c.datasets.allow) || [];
  const every = allow.length === 1 && allow[0] === "*";
  return [KIND_NAMES[c.type] || c.type, c.mode === "read-write" ? "read and write" : "read only",
    c.row_filter ? `rows where ${c.row_filter}` : null, every ? null : `only ${allow.join(", ")}`].filter(Boolean).join(" · ");
}

/** A saved connection as the Test command takes it (nothing is saved). */
function testForm(c) {
  return { existing: true, id: c.id, kind: c.type, path: c.path, host: c.host, port: c.port, database: c.database, dsn: c.dsn, user: c.user, sslmode: c.sslmode,
    search_path: c.search_path, mode: c.mode, allow: c.datasets.allow, deny: c.datasets.deny, row_filter: c.row_filter, description: c.description, password: null,
    recursive: !!c.recursive, crs: c.crs, x: c.x, y: c.y, url: c.url, auth: oauthPortal(c) ? "oauth" : "none", portal: oauthPortal(c), client_id: c.auth && c.auth.client_id, page_size: c.page_size };
}

export async function render(main, rest) {
  const tab = rest[0] || "databases";
  const page = h("div", { class: "page" });
  main.append(page);
  const body = h("div", { class: "pad stack" });
  const scroll = h("div", { class: "scroll" }, body);
  if (tab === "c" && rest[1]) return source(page, scroll, body, rest[1]);
  if (tab === "maplayers") {
    page.append(pageHead("Map layers", null,
      [button("Add web layer", "sm", () => maplayers.openWebLayerEditor(null), { id: "add-web-layer" })], BACK), scroll);
    return maplayers.render(body);
  }
  if (tab === "datasets") {
    page.append(pageHead("Datasets", null, [], BACK), scroll);
    return datasets(body);
  }
  if (tab === "projectdbs") {
    page.append(pageHead("Project databases", null, [], BACK), scroll);
    return projectDbs(body);
  }
  if (tab === "accounts") {
    page.append(pageHead("Accounts and services", null, [], BACK), scroll);
    body.append(coming("connections", "Accounts and services",
      "ArcGIS Online and Enterprise, GitHub, Microsoft 365 (SharePoint, OneDrive) and Google Drive, each signed in through your browser with its token kept in your computer's credential store, and each a connected server under Columbus's rules.",
      ["Today, connected servers are set up by your organization (server manifests).", "AI accounts will be under Settings › Assistant."], "Coming in a later stage"));
    return;
  }
  const search = h("input", { type: "search", placeholder: "Search sources", "aria-label": "Search sources" });
  const notes = h("a", { class: "footline", href: "#/documents", id: "data-notes" }, icon("doc"), h("span", { class: "grow" }, "Notes on what your data means"), icon("forward"));
  page.append(placeHead("Data", search, [button("Add data", "primary", () => openEditor(null), { id: "add" })]), scroll, notes);
  return databases(body, search, notes);
}

// The words shown after a save, here and on a connection's own page.
function savedText() {
  return state && state.restart_needed
    ? "Saved. The Columbus running in your AI app is an older version: quit your AI app (in Claude desktop: Cmd+Q or Quit) and open it again for your assistant to use the change."
    : "Saved. Your assistant uses the change from its next request; there is no need to restart anything. If you narrowed what a connection allows, files your assistant already made from it are removed, and anything waiting for your approval is asked again.";
}

async function databases(body, search, notesLink) {
  const notice = h("p", { id: "restart", class: "notice ok", role: "status", hidden: true });
  const error = h("p", { id: "conn-error", class: "error-text", role: "alert", hidden: true });
  const list = h("ul", { id: "connections", class: "list", "aria-live": "polite" });
  const none = h("p", { id: "no-conn", class: "muted small", hidden: true }, "Nothing yet. Add a GeoPackage, a file geodatabase, a folder of shapefiles, rasters or other files (CSV, Excel, GeoJSON, KML), an ArcGIS service, or a database.");
  const other = h("p", { id: "other-folders", class: "notice warn", hidden: true });
  const count = h("span", { class: "n" });
  const out = h("ul", { class: "list", id: "export-folders" });
  const outCount = h("span", { class: "n" });
  const outCard = h("section", { class: "card", hidden: true }, h("div", { class: "sec" }, h("h2", {}, "Where results can go"), outCount), h("div", { class: "bd flush" }, out));
  const more = h("ul", { class: "list compact", id: "data-more" });
  const link = (href, title, sub) => {
    const li = h("li", { class: "clickable", "data-href": href }, h("div", { class: "text" }, h("b", {}, h("a", { href }, title))), h("span", { class: "when" }, sub), icon("forward"));
    li.addEventListener("click", (e) => { if (e.target.tagName !== "A") location.hash = href; });
    return li;
  };
  more.append(
    link("#/data/datasets", "Datasets", "what each source has notes for"),
    link("#/datagroups", "Data groups", "named sets, to keep a project to"),
    link("#/data/maplayers", "Map layers", "imagery and web maps"),
    link("#/data/projectdbs", "Project databases", "working tables"),
    link("#/registry", "Registry", "open data sources and GIS tools"),
    link("#/data/accounts", "Accounts and services", "coming"));
  const moreCard = h("section", { class: "card" }, sec("More"), h("div", { class: "bd flush" }, more));
  body.append(notice, error, other,
    h("section", { class: "card" }, h("div", { class: "sec" }, h("h2", {}, "What your assistant can reach"), count), h("div", { class: "bd flush" }, list), none), outCard, moreCard,
    h("p", { class: "small muted wrap-any" }, h("span", { id: "confirm-note" }, confirmNote()), " Kept in ", h("code", { id: "conn-dir" }), "; passwords in your computer's credential store, never in a file."));

  const draw = () => {
    const q = search.value.trim().toLowerCase();
    $("confirm-note").textContent = confirmNote();
    $("conn-dir").textContent = state.connections_dir;
    const o = state.other_folders || [];
    other.hidden = o.length === 0;
    other.textContent = o.length ? `A running Columbus session reads its connections from ${o.join(", ")}, not from this folder, so changes here do not reach it. Clear the Connections folder setting of the Columbus extension in Claude desktop to use this one.` : "";
    list.replaceChildren();
    none.hidden = state.connections.length > 0;
    count.textContent = state.connections.length ? String(state.connections.length) : "";
    for (const c of state.connections) {
      const li = el("li", c.problem ? "conn bad" : "conn clickable");
      const text = el("div", "text");
      if (c.problem) {
        if (q) continue;
        text.append(el("b", null, "A connection file Columbus cannot read"), el("span", "where", `${c.file}: ${c.problem}`));
        li.append(h("span", { class: "glyph" }, icon("files")), text);
        list.append(li);
        continue;
      }
      if (q && ![c.id, KIND_NAMES[c.type] || c.type, where(c), c.description || ""].some((x) => String(x).toLowerCase().includes(q))) continue;
      li.dataset.id = c.id;
      li.title = `${where(c)}\n${facts(c).join(" · ")}`;
      text.append(h("b", {}, h("a", { href: connHash(c.id) }, c.id)), el("span", "where clip", brief(c)));
      li.addEventListener("click", (e) => { if (e.target.tagName !== "A") location.hash = connHash(c.id); });
      li.append(...[glyph(c), text, c.mode === "read-write" ? el("span", "badge warn", "Edits") : null, icon("forward")].filter(Boolean));
      list.append(li);
    }
    if (q && !list.children.length && state.connections.length) list.append(h("li", {}, el("span", "where", "No source matches.")));
    outCard.hidden = !!q || !out.children.length;
    moreCard.hidden = !!q;
  };
  const drawOut = () => {
    const folders = (store.projects && store.projects.export_folders) || [];
    outCount.textContent = folders.length ? String(folders.length) : "";
    out.replaceChildren(...folders.map((f) => {
      const li = h("li", {}, h("span", { class: "glyph" }, icon("out")),
        h("div", { class: "text" }, h("b", {}, f.id), h("span", { class: "where" }, f.granted ? "Saves without asking" : "Asks you each time")), h("span", { class: "when mono clip" }, f.path));
      li.title = f.path;
      return li;
    }));
    outCard.hidden = !folders.length || !!search.value.trim();
  };
  afterSave = async () => {
    await refresh();
    notice.textContent = savedText();
    notice.hidden = false;
    showError(error, null);
  };
  const refresh = async () => {
    try { await loadState(); draw(); } catch (e) { showError(error, e); }
  };
  search.addEventListener("input", () => { if (state) draw(); });
  await refresh();
  drawOut();
  if (store.refresh) store.refresh.projects().then(drawOut);
  invoke("notes_list").then((n) => {
    const drafts = n.notes.filter((x) => x.status === "draft").length;
    notesLink.querySelector(".grow").textContent = n.notes.length
      ? `${plural(n.notes.length, "note")} on what your data means.${drafts ? ` ${drafts === 1 ? "1 is a draft" : `${drafts} are drafts`}.` : ""}`
      : "Notes on what your data means. None yet.";
  }).catch(() => {});
  return () => { afterSave = null; };
}

// ---------- One connection: its rules in plain words, and what to do with it ----------
async function source(page, scroll, body, id) {
  const error = h("p", { id: "conn-error", class: "error-text", role: "alert", hidden: true });
  const notice = h("p", { id: "restart", class: "notice ok", role: "status", hidden: true });
  const box = h("div", { class: "stack", id: "source" });
  // Its datasets with notes, drawn apart: they change when notes are drafted.
  const notesBox = h("div", { class: "stack" });
  const head = h("div", {});
  const foot = h("div", { class: "footline" }, icon("lock"), h("span", { class: "grow", id: "confirm-note" }, confirmShort()));
  page.append(head, scroll, foot);
  body.append(notice, error, box, notesBox);
  let notes = null;
  const loadNotes = () => invoke("notes_list").then((n) => { notes = n.notes; if (state) drawNotes(); }).catch(() => {});
  const drawNotes = () => {
    const c = state.connections.find((x) => x.id === id);
    notesBox.replaceChildren();
    if (!c) return;
    // The datasets it has notes for; a note opens under Notes.
    const mine = notes ? notes.filter((n) => n.connection === c.id || n.path.startsWith(`datasets/${c.id}/`)) : [];
    if (mine.length) {
      const ul = h("ul", { class: "list", id: "source-datasets" });
      for (const n of mine) {
        const li = h("li", { class: "clickable", tabindex: "0" }, h("div", { class: "text" }, h("b", { class: "mono" }, n.title || n.id || n.path)),
          h("span", { class: `badge ${n.status === "reviewed" ? "mute" : "warn"}` }, n.status === "reviewed" ? "Note reviewed" : "Draft note"));
        const open = () => { try { sessionStorage.setItem("columbus.note", n.path); } catch { /* none */ } location.hash = "#/documents/notes"; };
        li.addEventListener("click", open);
        li.addEventListener("keydown", (e) => { if (e.key === "Enter") open(); });
        ul.append(li);
      }
      notesBox.append(h("section", { class: "card" }, sec("Datasets with notes", mine.length), h("div", { class: "bd flush" }, ul)));
    } else if (c.type !== "arcgis") {
      notesBox.append(h("p", { class: "small muted" }, h("b", {}, "Draft notes"), " writes what Columbus finds in the data (fields, keys, relationships, the codes short fields hold) into your notes folder, so your assistant knows the data. Review them under ", h("a", { href: "#/documents/notes" }, "Notes"), "."));
    }
  };
  const draw = () => {
    const c = state.connections.find((x) => x.id === id);
    head.replaceChildren(pageHead(c ? c.id : "Connection", null, [], BACK));
    $("confirm-note").textContent = confirmShort();
    $("confirm-note").title = confirmNote();
    box.replaceChildren();
    if (!c) { box.append(emptyState("Not found", "This connection isn't here any more (it may have been removed).")); return; }
    const rw = c.mode === "read-write";
    const allow = (c.datasets && c.datasets.allow) || [];
    const deny = (c.datasets && c.datasets.deny) || [];
    const every = allow.length === 1 && allow[0] === "*";
    const rule = (name, ...words) => h("li", {}, icon(name), h("span", {}, ...words));
    const status = el("span", "small draft-status");
    status.setAttribute("role", "status");
    const result = h("p", { id: "source-test", role: "status", hidden: true });
    // Test opens it as Columbus would: nothing is saved, the assistant sees none of it.
    const testBtn = button("Test", "", async () => {
      testBtn.disabled = true;
      testBtn.textContent = "Testing…";
      result.hidden = true;
      try {
        const r = await invoke("settings_test_connection", { form: testForm(c) });
        result.textContent = r.message;
        result.className = r.ok ? "test-ok" : "test-bad";
      } catch (e) {
        result.textContent = String(e);
        result.className = "test-bad";
      } finally {
        result.hidden = false;
        testBtn.disabled = false;
        testBtn.textContent = "Test";
      }
    }, { id: "source-test-btn", title: "Open it as Columbus would and say what it finds. Nothing is saved, and your assistant sees none of it." });
    const acts = h("div", { class: "row wrap" }, button("Change the rules", "", () => openEditor(c), { id: "source-edit" }), testBtn);
    // Drafted notes don't apply to ArcGIS services.
    if (c.type !== "arcgis") {
      const d = button("Draft notes", "", async () => { await draftNotes(c.id, d, status); loadNotes(); }, { title: "Write notes about this connection's datasets from the database itself (fields, keys, relationships, codes), for you to fill in" });
      acts.append(d);
    }
    if (c.type === "arcgis" && oauthPortal(c)) {
      const s = button("Sign in", "", () => signIn(c.id, s, status), { title: `Sign in to ArcGIS at ${oauthPortal(c)} in your browser` });
      acts.append(s);
    }
    const rules = h("ul", { class: "rules" },
      rule(rw ? "pen" : "eye", h("b", {}, rw ? "Read and write." : "Read only."), rw ? " Each change set is shown to you first." : " No edits and no deletes."),
      rule("layers", every ? "Every dataset" : ["Only ", h("b", {}, allow.join(", "))], deny.length ? [", never ", h("b", {}, deny.join(", "))] : null, "."),
      rule("filter", c.row_filter ? ["Only rows where ", h("b", { class: "mono" }, c.row_filter), "."] : "Every row."));
    if (c.type === "arcgis") rules.append(rule("user", oauthPortal(c) ? `Signs in at ${oauthPortal(c)}.` : "Public (no sign-in)."));
    if (c.has_password) rules.append(rule("key", "Password stored in your computer's credential store."));
    if (c.recursive) rules.append(rule("files", "With subfolders."));
    if (c.crs) rules.append(rule("registry", `Coordinates in ${c.crs}${c.x && c.y ? ` (columns ${c.x}, ${c.y})` : ""}.`));
    if (c.expires) rules.append(rule("clock", `Expires ${when(c.expires)}.`));
    box.append(...[
      h("p", { class: "small muted wrap-any" }, `${KIND_NAMES[c.type] || c.type} · ${where(c)}`),
      h("h2", { class: "big" }, rw ? "Your assistant can read it, and propose edits you approve." : `Your assistant can read ${every && !deny.length && !c.row_filter ? "it" : "part of it"}, and change nothing.`),
      c.description ? h("p", { class: "small" }, c.description) : null,
      rules, acts, status, result].filter(Boolean));
    drawNotes();
  };
  afterSave = async () => {
    try { await loadState(); } catch (e) { showError(error, e); return; }
    if (!state.connections.some((x) => x.id === id)) { location.hash = "#/data"; return; }
    draw();
    notice.textContent = savedText();
    notice.hidden = false;
    showError(error, null);
  };
  try { await loadState(); draw(); } catch (e) { showError(error, e); }
  loadNotes();
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
    body.append(emptyState("No connections yet", "Add a GeoPackage, a folder of rasters, or a database first.", h("a", { class: "btn primary", href: "#/data" }, "Go to Data")));
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
        const r = await invoke("settings_test_connection", { form: testForm(c) });
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
          : h("p", { class: "muted small" }, "No notes yet for this connection. Use Draft notes on the connection's own page, or List with Test to see what it allows."))));
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
