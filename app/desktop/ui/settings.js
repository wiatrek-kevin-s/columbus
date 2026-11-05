// The settings window. Everything shown is set as text, never as markup.
const { invoke } = window.__TAURI__.core;
const $ = (id) => document.getElementById(id);
const dialog = $("editor");
const form = $("form");
let state = null;
let editing = null; // the connection being edited, or null when adding

const KIND_NAMES = {
  gpkg: "GeoPackage", sqlite: "SQLite", rasters: "Rasters folder",
  postgis: "PostGIS", sqlserver: "SQL Server", oracle: "Oracle",
};

function el(tag, cls, text) {
  const e = document.createElement(tag);
  if (cls) e.className = cls;
  if (text != null) e.textContent = text;
  return e;
}

function showError(target, err) {
  const box = $(target);
  box.textContent = err ? String(err) : "";
  box.hidden = !err;
}

function saved() {
  $("restart").textContent = state && state.restart_needed
    ? "Saved. The Columbus running in your AI app is an older version: quit your AI app (in Claude desktop: Cmd+Q or Quit) and open it again for your assistant to use the change."
    : "Saved. Your assistant uses the change from its next request; there is no need to restart anything. If you narrowed what a connection allows, files your assistant already made from it are removed, and anything waiting for your approval is asked again.";
  $("restart").hidden = false;
  showError("error", null);
}

function showTest(result) {
  const box = $("test-result");
  if (!result) {
    box.hidden = true;
    box.textContent = "";
    return;
  }
  box.textContent = result.message;
  box.className = result.ok ? "test-ok" : "test-bad";
  box.hidden = false;
}

function where(c) {
  if (c.path) return c.path;
  const host = [c.host, c.port].filter(Boolean).join(":");
  return [c.user && `${c.user}@`, host, c.database && `/${c.database}`, c.dsn && ` (${c.dsn})`].filter(Boolean).join("");
}

function render() {
  $("confirm-note").textContent = state.confirm_with === "not available on this system"
    ? "On this system, changes that give it more access cannot be made here (there is no Touch ID or Windows Hello to confirm them)."
    : `Changes that give it more access ask for ${state.confirm_with} first.`;
  $("conn-dir").textContent = state.connections_dir;
  const other = state.other_folders || [];
  $("other-folders").hidden = other.length === 0;
  $("other-folders").textContent = other.length
    ? `A running Columbus session reads its connections from ${other.join(", ")}, not from this folder, so changes here do not reach it. Clear the Connections folder setting of the Columbus extension in Claude desktop to use this one.`
    : "";
  const list = $("connections");
  list.replaceChildren();
  $("no-conn").hidden = state.connections.length > 0;
  for (const c of state.connections) {
    const li = el("li", c.problem ? "conn bad" : "conn");
    const text = el("div", "text");
    if (c.problem) {
      text.append(el("b", null, "A connection file Columbus cannot read"));
      text.append(el("span", "where", `${c.file}: ${c.problem}`));
      li.append(text);
      list.append(li);
      continue;
    }
    text.append(el("b", null, c.id));
    text.append(el("span", "where", `${KIND_NAMES[c.type] || c.type} · ${where(c)}`));
    const chips = el("div", "chips");
    chips.append(el("span", c.mode === "read-write" ? "chip rw" : "chip", c.mode === "read-write" ? "Read and write" : "Read only"));
    const allow = (c.datasets && c.datasets.allow) || [];
    chips.append(el("span", "chip", allow.length === 1 && allow[0] === "*" ? "All datasets" : `Datasets: ${allow.join(", ")}`));
    if (c.datasets && c.datasets.deny && c.datasets.deny.length) chips.append(el("span", "chip", `Refused: ${c.datasets.deny.join(", ")}`));
    if (c.row_filter) chips.append(el("span", "chip", `Rows: ${c.row_filter}`));
    if (c.has_password) chips.append(el("span", "chip", "Password stored"));
    text.append(chips);
    if (c.description) text.append(el("span", "where", c.description));
    const status = el("span", "where draft-status");
    status.setAttribute("role", "status");
    text.append(status);
    const d = el("button", null, "Draft notes");
    d.type = "button";
    d.title = "Write notes about this connection's datasets from the database itself (fields, keys, relationships, codes), for you to fill in";
    d.addEventListener("click", () => draftNotes(c.id, d, status));
    const b = el("button", null, "Edit");
    b.type = "button";
    b.addEventListener("click", () => openEditor(c));
    li.append(text, d, b);
    list.append(li);
  }
  const p = state.policy;
  $("policy-path").textContent = p.exists ? p.path : `${p.path} (not created yet: Columbus's defaults apply)`;
  $("org-managed").hidden = p.writable;
  for (const name of ["allow_edits", "allow_scripts"]) {
    const box = $(name);
    box.checked = !!p[name];
    box.disabled = !p.writable;
  }
  showError("policy-problem", p.problem);
}

async function refresh() {
  try {
    state = await invoke("settings_state");
    render();
  } catch (e) {
    showError("error", e);
  }
}

function setKind(kind) {
  for (const label of form.querySelectorAll("[data-for]")) {
    label.hidden = !label.dataset.for.split(" ").includes(kind);
  }
  $("browse").textContent = kind === "rasters" ? "Choose folder…" : "Choose file…";
}

function openEditor(c) {
  editing = c || null;
  form.reset();
  showError("form-error", null);
  showTest(null);
  $("editor-h").textContent = c ? `Edit ${c.id}` : "Add connection";
  form.conn_id.readOnly = !!c;
  $("delete").hidden = !c;
  const v = c || { type: "gpkg", mode: "read", datasets: { allow: ["*"], deny: [] } };
  form.conn_id.value = v.id || "";
  form.kind.value = v.type || "gpkg";
  form.kind.disabled = !!c;
  for (const f of ["path", "host", "database", "dsn", "user", "sslmode", "search_path", "row_filter", "description"]) {
    form[f].value = v[f] || "";
  }
  form.port.value = v.port || "";
  form.mode.value = v.mode || "read";
  form.allow.value = ((v.datasets && v.datasets.allow) || ["*"]).join(", ");
  form.deny.value = ((v.datasets && v.datasets.deny) || []).join(", ");
  $("pw-note").textContent = c && c.has_password
    ? "A password is stored. Leave this empty to keep it."
    : "Kept in your computer's credential store, never in a file.";
  setKind(form.kind.value);
  dialog.showModal();
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
    if (ch === "," && depth === 0) {
      out.push(cur);
      cur = "";
    } else cur += ch;
  }
  out.push(cur);
  return out.map((x) => x.trim()).filter(Boolean);
}

function formValue() {
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
  };
}

async function save(ev) {
  ev.preventDefault();
  const btn = $("save");
  btn.disabled = true;
  showError("form-error", null);
  try {
    await invoke("settings_save_connection", { form: formValue() });
    dialog.close();
    // The notice depends on the sessions running now.
    await refresh();
    saved();
  } catch (e) {
    showError("form-error", e);
  } finally {
    btn.disabled = false;
    form.password.value = "";
  }
}

async function draftNotes(id, btn, status) {
  btn.disabled = true;
  btn.textContent = "Drafting…";
  status.textContent = "";
  status.classList.remove("bad");
  try {
    const r = await invoke("settings_draft_notes", { id });
    status.textContent = r.message;
    status.classList.toggle("bad", !r.ok);
  } catch (e) {
    status.textContent = String(e);
    status.classList.add("bad");
  } finally {
    btn.disabled = false;
    btn.textContent = "Draft notes";
  }
}

async function test() {
  const btn = $("test");
  btn.disabled = true;
  btn.textContent = "Testing…";
  showError("form-error", null);
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
  const id = editing.id;
  try {
    await invoke("settings_delete_connection", { id, forgetPassword: true });
    dialog.close();
    await refresh();
    saved();
  } catch (e) {
    showError("form-error", e);
  }
}

async function toggle(ev) {
  const box = ev.target;
  const want = box.checked;
  box.disabled = true;
  try {
    await invoke("settings_set_switch", { name: box.id, on: want });
    await refresh();
    saved();
  } catch (e) {
    showError("error", e);
    await refresh();
  }
}

$("add").addEventListener("click", () => openEditor(null));
$("cancel").addEventListener("click", () => dialog.close());
$("test").addEventListener("click", test);
$("delete").addEventListener("click", remove);
form.addEventListener("submit", save);
form.kind.addEventListener("change", () => setKind(form.kind.value));
$("browse").addEventListener("click", async () => {
  try {
    const p = await invoke("settings_pick", { kind: form.kind.value });
    if (p) form.path.value = p;
  } catch (e) {
    showError("form-error", e);
  }
});
$("allow_edits").addEventListener("change", toggle);
$("allow_scripts").addEventListener("change", toggle);
refresh();
