// Tools › Toolboxes (build brief 3, D-17, T-29): programs that bring many
// tools at once. The person installs one here, which is confirmed at the
// keyboard in the app's own code; from then on the assistant finds its
// tools and runs them in the sandbox, on copies. The search below reads
// the same list the assistant is offered.
import { $, h, el, invoke, listen, button, icon, showError, openDialog, openSide, closeSide, when } from "../core.js";

// The install that is running, if any: it goes on when the dialog closes.
let busy = null;

const count = (n) => Number(n || 0).toLocaleString();
const tools = (n) => `${count(n)} ${n === 1 ? "tool" : "tools"}`;
const mb = (b) => (b / 1024 / 1024).toFixed(1);

function stepText(p) {
  if (p.step === "download") return p.total ? `Downloading: ${mb(p.bytes || 0)} of ${mb(p.total)} MB` : "Downloading…";
  if (p.step === "check") return "Checking the download against the hash pinned in Columbus…";
  if (p.step === "unpack") return "Unpacking the program…";
  if (p.step === "list") return "Listing its tools in the sandbox…";
  return "";
}

/** The Toolboxes card for the top of Tools: `{ card, refresh, close }`. */
export function toolboxSection() {
  let st = null;
  let shownTool = null;
  let timer = 0;
  let asked = 0;
  let stopListening = null;
  let closed = false;

  const error = h("p", { id: "tbx-error", class: "error-text", role: "alert", hidden: true });
  const notice = h("p", { id: "tbx-notice", class: "notice ok", role: "status", hidden: true });
  const off = h("div", { id: "tbx-off", class: "notice warn", role: "status", hidden: true });
  const n = h("span", { class: "n" });
  const add = button("Add tools", "sm", () => openAdd(), { id: "tbx-add", title: "Install a toolbox from Columbus's catalogue, or add a program that is on this computer" });
  const list = h("ul", { id: "toolboxes", class: "list", "aria-live": "polite" });
  const none = h("p", { id: "no-toolboxes", class: "muted small quiet", hidden: true }, "No toolboxes yet. Add tools to give your assistant many more at once.");
  const q = h("input", { type: "search", id: "tbx-q", placeholder: "Find a tool: slope, buffer, clip…", "aria-label": "Find a tool in your toolboxes", autocomplete: "off", spellcheck: "false" });
  const hits = h("ul", { id: "tbx-tools", class: "list", "aria-live": "polite" });
  const more = h("p", { id: "tbx-count", class: "muted small quiet", role: "status" });
  const find = h("div", { id: "tbx-find", class: "stack", hidden: true }, h("label", { class: "searchbox" }, icon("search"), q), h("div", { class: "bd flush" }, hits), more);
  const where = h("p", { class: "small muted wrap-any" }, "A toolbox is a program that brings many tools. Your assistant finds them by what they do and runs them in the sandbox, on copies of your data, without asking: no network, and nothing outside the run's own folder is written. Stored in ", h("code", { id: "tbx-dir" }), ".");
  const card = h("section", { class: "card", id: "toolbox-card" },
    h("div", { class: "sec" }, h("h2", {}, "Toolboxes"), n, h("span", { class: "more" }, add)),
    h("div", { class: "bd stack" }, error, notice, off),
    h("div", { class: "bd flush" }, list), none,
    h("div", { class: "bd stack" }, find, where));

  const say = (text) => { notice.textContent = text; notice.hidden = !text; };

  const draw = () => {
    $("tbx-dir").textContent = st.dir;
    off.hidden = st.allowed;
    off.replaceChildren(h("div", {}, h("b", {}, "Toolboxes are turned off. "),
      st.managed
        ? "Your organization's settings do not allow them (allow_toolboxes), so none are used and none can be added. Ask whoever manages Columbus for you to turn them on."
        : "None are used and none can be added. To turn them on, choose Work freely on copies in Settings, or set allow_toolboxes in your organization settings file.",
      st.managed ? null : " ", st.managed ? null : h("a", { href: "#/settings", id: "tbx-settings" }, "Open Settings")));
    // Turned off: nothing is offered.
    add.hidden = !st.allowed;
    const ready = st.installed.filter((t) => t.state === "ready" && t.tools);
    find.hidden = !st.allowed || !ready.length;
    n.textContent = st.installed.length ? String(st.installed.length) : "";
    none.hidden = st.installed.length > 0 || !st.allowed;
    list.replaceChildren();
    for (const t of st.installed) {
      const ok = t.state === "ready";
      const li = el("li", ok ? "tool" : "tool bad");
      li.dataset.id = t.id;
      const text = el("div", "text");
      const facts = [t.id, t.version ? `version ${t.version}` : null, t.tools != null ? tools(t.tools) : null].filter(Boolean).join(" · ");
      text.append(el("b", null, t.name), el("span", "where", facts));
      if (t.cannot_run) text.append(el("span", "where", `${count(t.cannot_run)} of them need data Columbus cannot give yet.`));
      const from = [t.source ? `From ${t.source}` : null, t.licence ? `licence: ${t.licence}` : null, t.installed ? `installed ${when(t.installed)}` : null].filter(Boolean).join(" · ");
      if (from) text.append(el("span", "where", from));
      const state = !ok ? (t.state === "changed" ? "Off: its program changed" : t.state === "missing" ? "Off: its program is missing" : "Off: it cannot be read")
        : !st.allowed ? "Off (organization)" : t.tools == null ? "Its tools are listed the first time your assistant uses it" : "In use";
      text.append(el("span", ok && st.allowed ? "where" : "where off", state));
      if (t.reason) text.append(el("span", "where error-text tbx-reason", t.reason));
      const r = button("Remove", "ghost sm", () => remove(t, r), { title: `Remove ${t.name}: your assistant cannot use its tools from its next request` });
      li.append(text, h("div", { class: "acts" }, r));
      list.append(li);
    }
  };

  const refresh = async () => {
    try {
      st = await invoke("toolboxes_state");
      if (closed) return;
      draw();
      showError(error, null);
      if (!find.hidden) search(); else { hits.replaceChildren(); more.textContent = ""; }
    } catch (e) { showError(error, e); }
  };

  async function remove(t, btn) {
    btn.disabled = true;
    say("");
    try {
      const text = await invoke("toolbox_remove", { id: t.id });
      if (shownTool && shownTool.startsWith(`${t.id}.`)) closeSide();
      say(text);
    } catch (e) { showError(error, e); }
    await refresh();
  }

  // ---------- finding a tool ----------
  async function search() {
    const mine = ++asked;
    const words = q.value.trim();
    let r;
    try { r = await invoke("toolbox_search", { query: words }); } catch (e) { showError(error, e); return; }
    if (mine !== asked || closed) return;
    hits.replaceChildren();
    for (const t of r.hits) {
      const li = h("li", { class: "clickable", tabindex: "0", title: "Read what it does and its parameters" },
        h("div", { class: "text" },
          h("div", { class: "row wrap" }, h("b", {}, t.title), h("span", { class: "where mono" }, t.name)),
          t.summary ? h("span", {}, t.summary) : null,
          h("span", { class: "where" }, [t.toolbox, t.category].filter(Boolean).join(" · "))),
        t.cannot_run ? h("span", { class: "badge warn" }, "Cannot run yet") : null);
      li.dataset.tool = t.name;
      if (shownTool === t.name) li.classList.add("sel");
      const go = () => openTool(t, li);
      li.addEventListener("click", go);
      li.addEventListener("keydown", (e) => { if (e.key === "Enter" || e.key === " ") { e.preventDefault(); go(); } });
      hits.append(li);
    }
    more.textContent = !r.total ? (words ? "No tool matches those words. Try what it does, in a word or two." : "")
      : r.total > r.hits.length ? `Showing ${count(r.hits.length)} of ${tools(r.total)}${words ? ": add a word to narrow it." : ". Type what you need to find one."}`
        : `${tools(r.total)}${words ? " found" : ""}.`;
  }
  q.addEventListener("input", () => { clearTimeout(timer); timer = setTimeout(search, 180); });

  // A tool's description opens in the panel on the right.
  async function openTool(t, li) {
    const bd = h("div", { class: "stack", id: "tbx-tool" });
    for (const x of hits.children) x.classList.toggle("sel", x === li);
    shownTool = t.name;
    openSide(t.title, bd, t.cannot_run ? [h("span", { class: "badge warn" }, "Cannot run yet")] : [],
      () => { if (shownTool === t.name) { shownTool = null; li.classList.remove("sel"); } });
    let d;
    try { d = await invoke("toolbox_tool", { name: t.name }); } catch (e) { bd.append(el("p", "error-text", String(e))); return; }
    if (shownTool !== t.name) return;
    if (d.summary) bd.append(el("p", null, d.summary));
    if (d.description && d.description !== d.summary) bd.append(el("p", "small", d.description));
    if (d.cannot_run) bd.append(h("div", { class: "notice warn" }, h("div", {}, h("b", {}, "Columbus cannot run this one yet. "), d.cannot_run)));
    const dl = h("dl", { class: "kv" });
    for (const [k, v] of [["Your assistant calls it", d.name], ["Toolbox", `${d.toolbox_name}${d.version ? ` ${d.version}` : ""}`], ["Category", d.category]]) if (v) dl.append(h("dt", {}, k), h("dd", { class: k === "Your assistant calls it" ? "mono" : null }, String(v)));
    bd.append(dl);
    bd.append(el("h3", null, d.params.length ? "Parameters" : "It takes no parameters"));
    if (d.params.length) {
      const ul = h("ul", { class: "list", id: "tbx-params" });
      for (const p of d.params) {
        ul.append(h("li", {}, h("div", { class: "text" },
          h("div", { class: "row wrap" }, h("b", { class: "mono" }, p.name), h("span", { class: `badge ${p.role === "input" ? "info" : p.role === "output" ? "pass" : "mute"}` }, p.role)),
          h("span", { class: "where" }, p.facts),
          p.description ? h("span", {}, p.description) : null)));
      }
      bd.append(ul);
    }
    if (d.notes && d.notes.length) bd.append(h("p", { class: "small muted" }, d.notes.join(" ")));
    bd.append(h("p", { class: "small muted" }, "Your assistant gives each input as a layer, raster or file of the chat's workspace (the tool gets a copy), and each output comes back to the workspace as a new result. What the tool prints is shown to your assistant as text it must not obey."));
  }

  // ---------- adding ----------
  function openAdd() {
    if (!st || !st.allowed) return;
    const body = h("div", { class: "stack", id: "tbx-add-body" });
    const close = button("Close", "", () => d.close(), { id: "tbx-add-close" });
    const d = openDialog("Add tools", body, [h("span", { class: "spacer" }), close], "Add tools");
    d.id = "tbx-dialog";
    const terminal = (command) => [
      h("p", { class: "small" }, "Adding tools is confirmed at the keyboard, and this system has no Touch ID or Windows Hello for that. Use a terminal instead:"),
      h("pre", { class: "box tbx-terminal", tabindex: "0", "aria-label": "The command to type in a terminal" }, command)];

    for (const e of st.catalogue) {
      const facts = h("dl", { class: "kv" });
      for (const [k, v] of [["Version", e.version], ["Size", e.size ? `${e.size} to download (${e.system})` : null], ["Licence", e.licence], ["Publisher", e.publisher], ["Address", e.url || e.homepage]]) if (v) facts.append(h("dt", {}, k), h("dd", {}, String(v)));
      const part = h("section", { class: "stack tbx-entry", "aria-label": e.name }, h("h3", {}, e.name), h("p", { class: "small" }, e.about), facts);
      part.dataset.id = e.id;
      const step = h("p", { class: "small tbx-step", role: "status", hidden: true });
      const bar = h("progress", { class: "dl", max: "1", value: "0", "aria-label": `Downloading ${e.name}`, hidden: true });
      const err = h("p", { class: "error-text tbx-install-error", role: "alert", hidden: true });
      if (!e.available) {
        part.append(h("p", { class: "notice warn" }, `${e.name} has no build for this system (${e.system}), so it cannot be installed here.`));
      } else if (!st.catalogue_allowed) {
        part.append(h("p", { class: "notice warn" }, "Your organization's settings do not let Columbus download toolboxes (toolboxes.catalogue). You can still add a program that is already on this computer."));
      } else if (!st.can_confirm) {
        part.append(...terminal(`columbus-mcp toolbox install ${e.id}`));
      } else {
        const go = button(e.installed ? "Install again" : "Install", "primary", () => install(e, go, part), { class: "btn primary tbx-install", title: `Download and install it (asks for ${st.confirm_with})` });
        part.append(h("p", { class: "small muted" }, "Columbus checks the download against a hash pinned in this version before it unpacks anything, then lists the tools in the sandbox."),
          bar, step, err, h("div", { class: "row" }, go));
        if (busy && busy.id === e.id) { go.disabled = true; showStep(part, busy.last); }
      }
      body.append(part);
    }

    const own = h("section", { class: "stack", id: "tbx-own", "aria-label": "A program on this computer" }, h("h3", {}, "A program on this computer"),
      h("p", { class: "small" }, "A command-line program you already have, with a description file that lists its tools and their parameters (a tools.json; the reference shows how to write one). Its tools run in the sandbox like every toolbox tool."));
    if (!st.can_confirm) {
      own.append(...terminal("columbus-mcp toolbox add PROGRAM --id SHORT-NAME --kind described --tools TOOLS.JSON"));
    } else {
      const inp = (name, attrs = {}) => h("input", { name, autocomplete: "off", spellcheck: "false", ...attrs });
      const pickProgram = h("button", { type: "button", class: "btn" }, "Choose file…");
      const pickTools = h("button", { type: "button", class: "btn" }, "Choose file…");
      const err = h("p", { id: "tbx-form-error", class: "error-text", role: "alert", hidden: true });
      const save = h("button", { type: "submit", id: "tbx-save", class: "btn primary" }, "Add");
      const form = h("form", { id: "tbx-form", class: "stack" },
        h("div", { class: "grid2" },
          h("label", { class: "field wide" }, "Program", h("span", { class: "pick" }, inp("tbx_program", { required: true }), pickProgram), h("small", {}, "The program's file, with its full path.")),
          h("label", { class: "field wide" }, "Description file", h("span", { class: "pick" }, inp("tbx_tools", { required: true }), pickTools), h("small", {}, "The tools.json that describes it.")),
          h("label", { class: "field" }, "Short name", inp("tbx_id", { required: true, maxlength: "40", placeholder: "e.g. county-lines" }), h("small", {}, "Lowercase letters, digits and -. Its tools are named after it: county-lines.<tool>.")),
          h("label", { class: "field" }, "Name", inp("tbx_name", { placeholder: "e.g. County line tools" }), h("small", {}, "Shown here and to your assistant."))),
        err, h("div", { class: "row" }, save));
      const pick = (kind, field) => async () => {
        try { const p = await invoke("toolbox_pick", { kind }); if (p) field.value = p; } catch (x) { showError(err, x); }
      };
      pickProgram.addEventListener("click", pick("program", form.tbx_program));
      pickTools.addEventListener("click", pick("description", form.tbx_tools));
      form.addEventListener("submit", async (ev) => {
        ev.preventDefault();
        save.disabled = true;
        save.textContent = "Adding…";
        showError(err, null);
        try {
          const done = await invoke("toolbox_add", { id: form.tbx_id.value, name: form.tbx_name.value, program: form.tbx_program.value, tools: form.tbx_tools.value });
          d.close();
          say(done.message || `Added ${done.name}.`);
          await refresh();
        } catch (x) {
          showError(err, x);
        } finally {
          save.disabled = false;
          save.textContent = "Add";
        }
      });
      own.append(form);
    }
    body.append(own);
    const first = body.querySelector("button.tbx-install:not(:disabled)") || close;
    first.focus();
  }

  function showStep(part, p) {
    if (!part || !p) return;
    const step = part.querySelector(".tbx-step");
    const bar = part.querySelector("progress");
    if (!step || !bar) return;
    const text = stepText(p);
    if (text) { step.textContent = text; step.hidden = false; }
    bar.hidden = p.step !== "download" || !p.total;
    if (!bar.hidden) { bar.max = p.total; bar.value = Math.min(p.bytes || 0, p.total); }
  }

  async function install(e, btn, part) {
    if (busy) return;
    const err = part.querySelector(".tbx-install-error");
    const step = part.querySelector(".tbx-step");
    busy = { id: e.id, last: null };
    btn.disabled = true;
    showError(err, null);
    say("");
    step.className = "small tbx-step";
    step.textContent = `Waiting for you to confirm (${st.confirm_with})…`;
    step.hidden = false;
    try {
      const done = await invoke("toolbox_install", { id: e.id });
      const words = `${tools(done.tools - (done.cannot_run || 0))} ready.`;
      // Said in the dialog when it is still open, and on the page either way.
      if (part.isConnected) {
        part.querySelector("progress").hidden = true;
        step.className = "notice ok tbx-step";
        step.textContent = words;
        btn.textContent = "Install again";
      }
      say(done.message || `Installed ${done.name}: ${words}`);
    } catch (x) {
      if (part.isConnected) { step.hidden = true; part.querySelector("progress").hidden = true; showError(err, x); } else showError(error, x);
    } finally {
      busy = null;
      btn.disabled = false;
    }
    await refresh();
  }

  listen("toolbox-progress", (ev) => {
    const p = ev.payload || {};
    if (!busy || p.id !== busy.id || p.step === "done") return;
    busy.last = p;
    const d = $("tbx-dialog");
    if (d) showStep([...d.querySelectorAll(".tbx-entry")].find((x) => x.dataset.id === p.id), p);
  }).then((un) => { if (closed) un(); else stopListening = un; }).catch(() => {});

  return {
    card,
    refresh,
    close() {
      closed = true;
      clearTimeout(timer);
      if (stopListening) stopListening();
      if (shownTool) closeSide();
    },
  };
}
