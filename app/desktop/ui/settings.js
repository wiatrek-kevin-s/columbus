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
  for (const name of ["allow_edits", "allow_scripts", "allow_personal_tools"]) {
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
  await refreshTools();
}

// ---- The person's own tools ----
const review = $("tool-review");
let tools = null;
let reviewing = null; // the waiting tool open in the review dialog

function when(iso) {
  if (!iso) return "";
  const d = new Date(iso);
  return isNaN(d) ? iso : d.toLocaleString();
}

function testText(t) {
  const p = t.proposal || {};
  const test = p.test || {};
  const parts = [];
  if (p.proposed) parts.push(`Proposed ${when(p.proposed)}`);
  const outs = (test.outputs || []).map((o) => `${o.kind}${o.rows != null ? ` (${o.rows} rows)` : ""}`);
  if (outs.length) parts.push(`test run made ${outs.join(", ")}`);
  const checks = test.checks || [];
  const bad = checks.filter((c) => c.severity === "fail").map((c) => c.id);
  const warn = checks.filter((c) => c.severity === "warn").map((c) => c.id);
  if (bad.length) parts.push(`failed checks: ${bad.join(", ")}`);
  if (warn.length) parts.push(`warnings: ${warn.join(", ")}`);
  const labels = [...new Set((test.inputs || []).flatMap((i) => i.labels || []))];
  if (labels.length) parts.push(`tested on data from ${labels.map((l) => l.replace(/^connection:/, "")).join(", ")}`);
  return parts.join("; ");
}

function renderTools() {
  $("tools-dir").textContent = tools.dir;
  $("tools-off").hidden = tools.allowed;
  const waiting = $("tools-waiting");
  waiting.replaceChildren();
  $("no-waiting").hidden = tools.waiting.length > 0;
  for (const t of tools.waiting) {
    const li = el("li", t.problem ? "tool bad" : "tool");
    const text = el("div", "text");
    text.append(el("b", null, t.title));
    text.append(el("span", "where", `${t.tool} · version ${t.version || "?"}`));
    if (t.description) text.append(el("span", null, t.description));
    const info = testText(t);
    if (info) text.append(el("span", "where", info));
    if (t.problem) text.append(el("span", "where error-text", t.problem));
    const b = el("button", "primary", "Review");
    b.type = "button";
    b.title = "Read its code and manifest, then install or decline it";
    b.disabled = !!t.problem;
    b.addEventListener("click", () => openReview(t));
    const d = el("button", "danger", "Decline");
    d.type = "button";
    d.addEventListener("click", () => decline(t.id));
    li.append(text, b, d);
    waiting.append(li);
  }
  const installed = $("tools-installed");
  installed.replaceChildren();
  $("no-installed").hidden = tools.installed.length > 0;
  for (const t of tools.installed) {
    const li = el("li", "tool");
    const text = el("div", "text");
    text.append(el("b", null, t.title));
    text.append(el("span", "where", `${t.tool} · version ${t.version || "?"}${t.installed ? ` · installed ${when(t.installed)}` : ""}`));
    if (t.description) text.append(el("span", null, t.description));
    const chips = el("div", "chips");
    const state = t.state === "ready" ? (tools.allowed ? "In use" : "Off (organization)") : t.state === "changed" ? "Off: its files changed" : "Off: not installed";
    chips.append(el("span", t.state === "ready" && tools.allowed ? "chip" : "chip off", state));
    text.append(chips);
    if (t.problem) text.append(el("span", "where error-text", t.problem));
    const v = el("button", null, "Code");
    v.type = "button";
    v.addEventListener("click", () => openInstalled(t));
    const r = el("button", "danger", "Remove");
    r.type = "button";
    r.title = "Remove it: your assistant cannot use it from its next request";
    r.addEventListener("click", () => removeTool(t.id));
    li.append(text, v, r);
    installed.append(li);
  }
}

async function refreshTools() {
  try {
    tools = await invoke("settings_tools");
    renderTools();
    showError("tools-error", null);
  } catch (e) {
    showError("tools-error", e);
  }
}

// Numbered lines, one per line as Python reads them (never scrolled
// sideways), as on the approval card: what a reader counts is what runs.
function numbered(id, countId, text) {
  const pre = $(id);
  pre.replaceChildren();
  const lines = text === "" ? [] : text.replace(/\n$/, "").split("\n");
  for (const line of lines) pre.append(el("span", "ln", line === "" ? " " : line));
  $(countId).textContent = `${lines.length} line${lines.length === 1 ? "" : "s"}`;
}

function fillReview(t, manifest, code) {
  $("review-h").textContent = t.title;
  $("review-name").textContent = `${t.tool} · version ${t.version || "?"} · ${t.hash}`;
  $("review-desc").textContent = t.description || "";
  $("review-not-for").textContent = t.not_for ? `Not for: ${t.not_for}` : "";
  numbered("review-manifest", "review-manifest-lines", manifest || "");
  numbered("review-code", "review-code-lines", code || "");
  showError("review-error", null);
}

function openReview(t) {
  reviewing = t;
  fillReview(t, t.manifest_text, t.code);
  $("review-test").textContent = testText(t);
  const rep = $("review-replaces");
  rep.hidden = t.replaces == null;
  rep.textContent = t.replaces != null ? "This replaces the installed tool of the same name." : "";
  const warn = $("review-warnings");
  warn.replaceChildren();
  for (const w of (t.proposal && t.proposal.warnings) || []) warn.append(el("li", null, w));
  $("review-install").hidden = false;
  $("review-decline").hidden = false;
  $("review-install").disabled = !tools.allowed;
  $("review-install").title = tools.allowed ? `Install it (asks for ${state ? state.confirm_with : "confirmation"})` : "Personal tools are turned off in your organization's settings";
  review.showModal();
  $("review-code").focus();
}

async function openInstalled(t) {
  reviewing = null;
  try {
    const f = await invoke("settings_tool_files", { id: t.id });
    fillReview(t, f.manifest_text, f.code);
  } catch (e) {
    fillReview(t, "", "");
    showError("review-error", e);
  }
  $("review-test").textContent = t.installed ? `Installed ${when(t.installed)}` : "";
  $("review-replaces").hidden = true;
  $("review-warnings").replaceChildren();
  $("review-install").hidden = true;
  $("review-decline").hidden = true;
  review.showModal();
}

async function install() {
  if (!reviewing) return;
  const btn = $("review-install");
  btn.disabled = true;
  showError("review-error", null);
  try {
    await invoke("settings_tool_install", { id: reviewing.id, hash: reviewing.hash });
    review.close();
    await refreshTools();
    $("restart").textContent = `Installed ${reviewing.title}. Your assistant can use it from its next request (as ${reviewing.tool}); there is no need to restart anything.`;
    $("restart").hidden = false;
  } catch (e) {
    showError("review-error", e);
  } finally {
    btn.disabled = !tools || !tools.allowed;
  }
}

async function decline(id) {
  try {
    await invoke("settings_tool_decline", { id });
    if (review.open) review.close();
    await refreshTools();
  } catch (e) {
    showError(review.open ? "review-error" : "tools-error", e);
  }
}

async function removeTool(id) {
  try {
    await invoke("settings_tool_remove", { id });
    await refreshTools();
  } catch (e) {
    showError("tools-error", e);
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
$("allow_personal_tools").addEventListener("change", toggle);
$("review-install").addEventListener("click", install);
$("review-decline").addEventListener("click", () => reviewing && decline(reviewing.id));
$("review-close").addEventListener("click", () => review.close());
// New proposals arrive while the window is open.
window.addEventListener("focus", refreshTools);
refresh();
