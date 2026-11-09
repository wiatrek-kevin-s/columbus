// Connections › Map layers (T-25; build brief part 2, Part 3): web map
// layers the person adds by pasting a service's address. Columbus reads
// the service's capabilities and offers its layers; each saved layer is
// listed in every chat's Web layers (off until turned on there) and here,
// with Test, Edit and Remove. The assistant never sees them. Saving asks
// the person to confirm at the keyboard (in the app's own code) when the
// organization lists no map hosts, since the service will see which area
// the map shows.
import { $, h, el, invoke, button, showError, toast } from "../core.js";

const KINDS = {
  "": "Tell from the address",
  xyz: "XYZ tiles ({z}/{x}/{y})",
  wmts: "WMTS",
  wms: "WMS",
  "arcgis-map": "ArcGIS MapServer",
  "arcgis-image": "ArcGIS ImageServer",
  "arcgis-features": "ArcGIS feature layer",
};
const KIND_SHORT = { xyz: "XYZ tiles", wmts: "WMTS", wms: "WMS", "arcgis-map": "ArcGIS map", "arcgis-image": "ArcGIS image", "arcgis-features": "ArcGIS features" };

// Told after a layer is saved or removed: the chats' maps read their list again.
const listeners = new Set();
export function onChange(fn) {
  listeners.add(fn);
  return () => listeners.delete(fn);
}
function changed() {
  for (const fn of listeners) {
    try { fn(); } catch (e) { console.error(e); }
  }
}

let editing = null; // the saved layer being edited, or null
let found = null; // the last map_layer_inspect answer
let redraw = null; // the tab's list, when shown

function hostOf(url) {
  try { return new URL(url.replace(/[{}]/g, "")).hostname; } catch { return ""; }
}

/** The Map layers tab. */
export async function render(body) {
  const error = h("p", { id: "wl-list-error", class: "error-text", role: "alert", hidden: true });
  const note = h("div", { class: "notice" }, h("div", { id: "wl-note" },
    "Imagery and other map services, drawn on your map from the address you paste. Each layer starts off in every chat; turning one on shows that its service sees which area you are viewing. Your assistant never sees web layers."));
  const list = h("ul", { id: "web-layers", class: "list", "aria-live": "polite" });
  const none = h("div", { id: "no-web-layers", class: "pad", hidden: true },
    h("p", { class: "muted" }, "No web layers yet. Add one by pasting a WMTS, WMS, XYZ tile, ArcGIS MapServer, ImageServer or feature layer address."));
  body.append(error, note, h("section", { class: "card" }, h("div", { class: "hd" }, h("h2", {}, "Map layers")), h("div", { class: "bd flush" }, list, none)),
    h("p", { class: "small muted" }, "Services that need a user name and password are not supported. A secured ArcGIS service signs in through one of your ArcGIS connections."));
  const draw = async () => {
    let r;
    try {
      r = await invoke("map_layers_list");
    } catch (e) {
      showError(error, e);
      return;
    }
    showError(error, null);
    if (!r.allow_web_layers) $("wl-note").textContent = "Your organization has turned web layers off.";
    else if (r.hosts && r.hosts.length) $("wl-note").textContent = `Your organization allows web layers from ${r.hosts.join(", ")} only. Each layer starts off in every chat; your assistant never sees web layers.`;
    list.replaceChildren();
    none.hidden = r.sources.length > 0;
    for (const s of r.sources) {
      const li = el("li", "conn");
      li.dataset.id = s.id;
      const text = el("div", "text");
      text.append(h("div", { class: "row wrap" }, el("b", null, s.title), el("span", "where", `${s.id} · ${KIND_SHORT[s.kind] || s.kind} · ${hostOf(s.url)}`)));
      const chips = el("div", "chips");
      if (s.layer) chips.append(el("span", "chip", `Layer ${s.layer}`));
      if (s.auth_from) chips.append(el("span", "chip", `Signs in through ${s.auth_from}`));
      if (s.origin === "policy") chips.append(el("span", "chip", "From your organization"));
      for (const a of s.approved_hosts || []) chips.append(el("span", "chip", `Also ${a}`));
      text.append(chips);
      const status = el("span", "where wl-status");
      status.setAttribute("role", "status");
      text.append(status);
      const acts = h("div", { class: "acts" });
      const t = button("Test", "sm", () => testOne(s.id, t, status), { title: "Ask the service for a tile or its features, as the map would" });
      acts.append(t);
      if (s.origin === "saved") {
        acts.append(button("Edit", "sm", () => openWebLayerEditor(s)));
        acts.append(button("Remove", "sm danger", () => removeOne(s, status)));
      }
      li.append(text, acts);
      list.append(li);
    }
  };
  redraw = draw;
  await draw();
  return () => { redraw = null; };
}

async function testOne(id, btn, status) {
  btn.disabled = true;
  btn.textContent = "Testing…";
  status.textContent = "";
  status.classList.remove("error-text");
  try {
    const r = await invoke("map_layer_test", { id });
    status.textContent = r.ok ? `Reachable: ${String(r.message || "").replace(/^reachable: /, "")}` : r.message;
    status.classList.toggle("error-text", !r.ok);
  } catch (e) {
    status.textContent = String(e);
    status.classList.add("error-text");
  } finally {
    btn.disabled = false;
    btn.textContent = "Test";
  }
}

async function removeOne(s, status) {
  try {
    await invoke("map_layer_remove", { id: s.id });
    toast(`Removed ${s.title}. It is gone from every chat's map.`);
    changed();
    if (redraw) await redraw();
  } catch (e) {
    status.textContent = String(e);
    status.classList.add("error-text");
  }
}

// ---------- The editor (one dialog for the whole window) ----------

function field(label, input, opts = {}) {
  const l = h("label", { class: `field${opts.wide ? " wide" : ""}`, id: opts.id || null }, label, input);
  if (opts.help) l.append(h("small", {}, opts.help));
  return l;
}

function buildEditor() {
  const inp = (name, attrs = {}) => h("input", { name, autocomplete: "off", ...attrs });
  const kind = h("select", { name: "wl_kind" }, ...Object.entries(KINDS).map(([v, t]) => h("option", { value: v }, t)));
  const auth = h("select", { name: "wl_auth" }, h("option", { value: "" }, "None (a public service)"));
  const layer = h("select", { name: "wl_layer" });
  const read = h("button", { type: "button", id: "wl-read", class: "btn" }, "Read layers");
  const form = h("form", { id: "wl-form", method: "dialog", class: "dlg" },
    h("h2", { id: "wl-editor-h" }, "Add web layer"),
    h("div", { class: "grid2" },
      field("Address", h("span", { class: "pick" }, inp("wl_url", { required: true, placeholder: "https://…/WMTSCapabilities.xml, …/MapServer, …/{z}/{x}/{y}.png" }), read),
        { wide: true, help: "Paste the service's address. Columbus reads what the service offers; nothing is saved until you press Save." }),
      field("Kind", kind),
      field("Sign in through", auth, { id: "wl-auth-field", help: "For a secured ArcGIS service: one of your ArcGIS connections." }),
      field("Layer", layer, { wide: true, id: "wl-layer-field" }),
      field("Title", inp("wl_title", { required: true, maxlength: "200" }), { help: "As the layer list shows it." }),
      field("Name", inp("wl_id", { required: true, maxlength: "64" }), { help: "Letters, digits, - and _." }),
      field("Attribution", inp("wl_attribution"), { wide: true, help: "Credit shown on the map, if the service asks for one." })),
    h("p", { id: "wl-error", class: "error-text", role: "alert", hidden: true }),
    h("p", { id: "wl-found", role: "status", class: "small", hidden: true }),
    h("p", { id: "wl-hint", class: "small muted" }),
    h("div", { class: "actions" },
      h("span", { class: "spacer" }),
      h("button", { type: "button", id: "wl-cancel", class: "btn" }, "Cancel"),
      h("button", { type: "submit", id: "wl-save", class: "btn primary" }, "Save")));
  const dlg = h("dialog", { id: "wl-editor", "aria-labelledby": "wl-editor-h" }, form);
  document.body.append(dlg);
  read.addEventListener("click", () => readLayers());
  kind.addEventListener("change", () => { found = null; showAuth(); });
  form.wl_url.addEventListener("change", () => { found = null; });
  layer.addEventListener("change", () => {
    const o = layer.selectedOptions[0];
    if (o && o.dataset.title && !form.wl_title.dataset.typed) form.wl_title.value = o.dataset.title;
    suggestId();
  });
  form.wl_title.addEventListener("input", () => { form.wl_title.dataset.typed = "1"; suggestId(); });
  form.wl_id.addEventListener("input", () => { form.wl_id.dataset.typed = "1"; });
  $("wl-cancel").addEventListener("click", () => dlg.close());
  form.addEventListener("submit", save);
  return dlg;
}

function suggestId() {
  const form = $("wl-form");
  if (editing || form.wl_id.dataset.typed) return;
  form.wl_id.value = form.wl_title.value.toLowerCase().normalize("NFKD").replace(/[^a-z0-9]+/g, "-").replace(/^-+|-+$/g, "").slice(0, 64);
}

function showAuth() {
  const form = $("wl-form");
  const k = (found && found.kind) || form.wl_kind.value;
  $("wl-auth-field").hidden = !(k === "" || k.startsWith("arcgis"));
}

/** Read the pasted address's capabilities and offer its layers. */
async function readLayers(keep) {
  const form = $("wl-form");
  const btn = $("wl-read");
  btn.disabled = true;
  btn.textContent = "Reading…";
  showError($("wl-error"), null);
  const box = $("wl-found");
  box.hidden = true;
  try {
    found = await invoke("map_layer_inspect", { url: form.wl_url.value, kind: form.wl_kind.value || null, authFrom: form.wl_auth.value || null });
    form.wl_kind.value = found.kind;
    const sel = form.wl_layer;
    sel.replaceChildren();
    const layers = found.layers || [];
    // A map service may be drawn whole (its cached tiles, when it has them).
    if (found.kind === "arcgis-map" || (found.kind === "arcgis-features" && !layers.length)) sel.append(h("option", { value: "", "data-title": "" }, found.kind === "arcgis-map" ? "All layers (as published)" : "The layer in the address"));
    for (const l of layers) {
      const extra = (l.refused_hosts || []).length ? ` (tiles from ${l.refused_hosts.join(", ")}: not allowed)` : (l.hosts || []).length ? ` (tiles from ${l.hosts.join(", ")})` : "";
      sel.append(h("option", { value: l.layer, "data-title": l.title || l.layer }, `${l.title || l.layer} · ${l.layer}${extra}`));
    }
    $("wl-layer-field").hidden = sel.options.length === 0;
    if (keep != null && [...sel.options].some((o) => o.value === keep)) sel.value = keep;
    else if (sel.options.length) sel.selectedIndex = found.kind === "arcgis-map" ? 0 : 0;
    if (!form.wl_title.value && sel.selectedOptions[0]) form.wl_title.value = sel.selectedOptions[0].dataset.title || "";
    suggestId();
    box.textContent = `${KIND_SHORT[found.kind] || found.kind} at ${found.host}: ${layers.length ? `${layers.length} layer${layers.length === 1 ? "" : "s"} offered` : "ready to save"}.`;
    box.className = "small test-ok";
    box.hidden = false;
    $("wl-hint").textContent = found.confirm
      ? "Saving asks you to confirm first: the service (and any tile host it names) will see which area your map shows when the layer is on."
      : "Your organization lists the hosts web layers may come from.";
    showAuth();
  } catch (e) {
    found = null;
    showError($("wl-error"), e);
  } finally {
    btn.disabled = false;
    btn.textContent = "Read layers";
  }
}

/** Add (src null) or edit a saved web layer. `onSaved` runs after a save. */
export async function openWebLayerEditor(src, onSaved) {
  const dlg = $("wl-editor") || buildEditor();
  const form = $("wl-form");
  editing = src || null;
  found = null;
  dlg.onSaved = onSaved || null;
  form.reset();
  delete form.wl_title.dataset.typed;
  delete form.wl_id.dataset.typed;
  showError($("wl-error"), null);
  $("wl-found").hidden = true;
  $("wl-hint").textContent = "";
  $("wl-editor-h").textContent = src ? `Edit ${src.title}` : "Add web layer";
  form.wl_layer.replaceChildren();
  $("wl-layer-field").hidden = true;
  // Secured ArcGIS services sign in through an ArcGIS connection.
  const auth = form.wl_auth;
  auth.replaceChildren(h("option", { value: "" }, "None (a public service)"));
  try {
    const st = await invoke("settings_state");
    for (const c of st.connections) if (c.type === "arcgis" && c.id) auth.append(h("option", { value: c.id }, c.id));
  } catch { /* none offered */ }
  form.wl_id.readOnly = !!src;
  if (src) {
    form.wl_url.value = src.url || "";
    form.wl_kind.value = src.kind || "";
    form.wl_title.value = src.title || "";
    form.wl_title.dataset.typed = "1";
    form.wl_id.value = src.id;
    form.wl_attribution.value = src.attribution || "";
    auth.value = src.auth_from || "";
  }
  showAuth();
  dlg.showModal();
  form.wl_url.focus();
  if (src) await readLayers(src.layer || "");
}

async function save(ev) {
  ev.preventDefault();
  const form = $("wl-form");
  const btn = $("wl-save");
  btn.disabled = true;
  showError($("wl-error"), null);
  try {
    if (!found) await readLayers(form.wl_layer.value || (editing && editing.layer) || null);
    if (!found) return;
    const r = await invoke("map_layer_save", { form: {
      existing: !!editing,
      id: form.wl_id.value.trim(),
      title: form.wl_title.value.trim(),
      kind: form.wl_kind.value || null,
      url: form.wl_url.value.trim(),
      layer: $("wl-layer-field").hidden ? null : form.wl_layer.value || null,
      auth_from: form.wl_auth.value || null,
      attribution: form.wl_attribution.value.trim() || null,
    } });
    const dlg = $("wl-editor");
    dlg.close();
    toast(editing ? "Saved. Every chat's map uses the change." : "Saved. It is listed under Web layers on every chat's map; it starts off there.");
    changed();
    if (redraw) await redraw();
    if (dlg.onSaved) dlg.onSaved(r);
  } catch (e) {
    showError($("wl-error"), e);
  } finally {
    btn.disabled = false;
  }
}
