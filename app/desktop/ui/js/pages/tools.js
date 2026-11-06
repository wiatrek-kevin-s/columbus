// Tools: the person's own tools (ported from the Settings window), each
// read in full before it is installed, which is confirmed at the keyboard
// in the app's own code; and the Skills library, read only.
import { $, h, el, invoke, store, button, pageHead, showError, when, numbered, emptyState, coming } from "../core.js";
import { switchRow } from "./settings.js";

let tools = null;
let reviewing = null;

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

export async function render(main) {
  const page = h("div", { class: "page" });
  main.append(page);
  page.append(pageHead("Tools", "Tools your assistant made from scripts, for it (and any assistant you use with Columbus) to reuse. You read each one and install it here."));
  const body = h("div", { class: "pad stack" });
  page.append(h("div", { class: "scroll" }, body));
  const notice = h("p", { id: "restart", class: "notice ok", role: "status", hidden: true });
  const err = h("p", { id: "tools-error", class: "error-text", role: "alert", hidden: true });
  const off = h("p", { id: "tools-off", class: "notice warn", role: "status", hidden: true }, "Personal tools are turned off in your organization's settings, so none of these are used, and none can be installed.");
  const waiting = h("ul", { id: "tools-waiting", class: "list", "aria-live": "polite" });
  const installed = h("ul", { id: "tools-installed", class: "list", "aria-live": "polite" });
  const sw = h("div", {});
  body.append(notice, err, off,
    h("p", { class: "small muted" }, "An installed tool runs whenever it is needed, without asking, in the same sandbox as scripts: no network; each run starts afresh, reads only the layers it is given for that run (and the Python installation) and writes only its own folder, which is deleted when the run ends. Ask your assistant, for example: ", h("i", {}, "add the script we just made as a tool"), "."),
    h("section", { class: "card" }, h("div", { class: "hd" }, h("h2", {}, "Waiting for you")), h("div", { class: "bd flush" }, waiting, h("p", { id: "no-waiting", class: "pad muted small", hidden: true }, "Nothing is waiting."))),
    h("section", { class: "card" }, h("div", { class: "hd" }, h("h2", {}, "Installed")), h("div", { class: "bd flush" }, installed, h("p", { id: "no-installed", class: "pad muted small", hidden: true }, "No tools installed yet."))),
    h("section", { class: "card" }, h("div", { class: "hd" }, h("h2", {}, "Your organization's switch")), h("div", { class: "bd" }, sw)),
    h("p", { class: "small muted wrap-any" }, "Stored in ", h("code", { id: "tools-dir" })),
    h("p", { class: "small muted" }, "Built-in tools, engine plugins and connected servers are listed by your assistant (list_providers); a page for them comes later."));

  const draw = () => {
    $("tools-dir").textContent = tools.dir;
    off.hidden = tools.allowed;
    waiting.replaceChildren();
    $("no-waiting").hidden = tools.waiting.length > 0;
    for (const t of tools.waiting) {
      const li = el("li", t.problem ? "tool bad" : "tool");
      const text = el("div", "text");
      text.append(el("b", null, t.title), el("span", "where", `${t.tool} · version ${t.version || "?"}`));
      if (t.description) text.append(el("span", null, t.description));
      const info = testText(t);
      if (info) text.append(el("span", "where", info));
      if (t.problem) text.append(el("span", "where error-text", t.problem));
      const b = button("Review", "primary sm", () => openReview(t), { title: "Read its code and manifest, then install or decline it" });
      b.disabled = !!t.problem;
      const d = button("Decline", "danger sm", () => decline(t.id));
      li.append(text, h("div", { class: "acts" }, b, d));
      waiting.append(li);
    }
    installed.replaceChildren();
    $("no-installed").hidden = tools.installed.length > 0;
    for (const t of tools.installed) {
      const li = el("li", "tool");
      const text = el("div", "text");
      text.append(el("b", null, t.title), el("span", "where", `${t.tool} · version ${t.version || "?"}${t.installed ? ` · installed ${when(t.installed)}` : ""}`));
      if (t.description) text.append(el("span", null, t.description));
      const state = t.state === "ready" ? (tools.allowed ? "In use" : "Off (organization)")
        : t.state === "changed" ? "Off: its files changed"
          : t.state === "invalid" ? "Off: ask your assistant to propose it again" : "Off: not installed";
      text.append(h("div", { class: "chips" }, el("span", t.state === "ready" && tools.allowed ? "chip ro" : "chip off", state)));
      if (t.problem) text.append(el("span", "where error-text", `Not used: ${t.problem}. Remove it, and ask your assistant to propose it again.`));
      const v = button("Code", "sm", () => openInstalled(t));
      const r = button("Remove", "danger sm", () => removeTool(t.id), { title: "Remove it: your assistant cannot use it from its next request" });
      li.append(text, h("div", { class: "acts" }, v, r));
      installed.append(li);
    }
  };
  const refresh = async () => {
    try {
      tools = await invoke("settings_tools");
      store.set("tools", tools);
      draw();
      showError(err, null);
    } catch (e) { showError(err, e); }
  };
  const drawSwitch = async () => {
    try {
      const st = await invoke("settings_state");
      sw.replaceChildren(switchRow("allow_personal_tools", st, async () => { await drawSwitch(); await refresh(); }));
    } catch (e) { sw.replaceChildren(el("p", "error-text", String(e))); }
  };
  async function decline(id) {
    try {
      await invoke("settings_tool_decline", { id });
      const r = $("tool-review");
      if (r && r.open) r.close();
      await refresh();
    } catch (e) { showError($("tool-review")?.open ? $("review-error") : err, e); }
  }
  async function removeTool(id) {
    try { await invoke("settings_tool_remove", { id }); await refresh(); } catch (e) { showError(err, e); }
  }
  function reviewDialog() {
    let d = $("tool-review");
    if (d) return d;
    d = h("dialog", { id: "tool-review", "aria-labelledby": "review-h" }, h("div", { class: "dlg" },
      h("h2", { id: "review-h" }, "Tool"),
      h("p", { id: "review-name", class: "muted small wrap-any" }),
      h("p", { id: "review-desc" }), h("p", { id: "review-not-for" }), h("p", { id: "review-test", class: "small" }),
      h("p", { id: "review-replaces", class: "notice", hidden: true }),
      h("ul", { id: "review-warnings", class: "small error-text" }),
      h("p", { class: "small" }, "Read the code before you install it: once installed, your assistant may run it on the layers it gives it whenever it needs, without asking. The last part of the code is added by Columbus to connect the tool to it."),
      h("h3", {}, "Manifest ", h("span", { id: "review-manifest-lines", class: "muted small" })), h("div", { id: "review-manifest-box" }),
      h("h3", {}, "Code ", h("span", { id: "review-code-lines", class: "muted small" })), h("div", { id: "review-code-box" }),
      h("p", { id: "review-error", class: "error-text", role: "alert", hidden: true }),
      h("div", { class: "actions" },
        h("button", { type: "button", id: "review-decline", class: "btn danger" }, "Decline"), h("span", { class: "spacer" }),
        h("button", { type: "button", id: "review-close", class: "btn" }, "Close"),
        h("button", { type: "button", id: "review-install", class: "btn primary" }, "Install"))));
    document.body.append(d);
    $("review-close").addEventListener("click", () => d.close());
    return d;
  }
  function fill(t, manifest, code) {
    $("review-h").textContent = t.title;
    $("review-name").textContent = `${t.tool} · version ${t.version || "?"} · ${t.hash}`;
    $("review-desc").textContent = t.description || "";
    $("review-not-for").textContent = t.not_for ? `Not for: ${t.not_for}` : "";
    const m = numbered(manifest || "");
    m.pre.id = "review-manifest";
    $("review-manifest-box").replaceChildren(m.pre);
    $("review-manifest-lines").textContent = `${m.count} line${m.count === 1 ? "" : "s"}`;
    const c = numbered(code || "");
    c.pre.id = "review-code";
    $("review-code-box").replaceChildren(c.pre);
    $("review-code-lines").textContent = `${c.count} line${c.count === 1 ? "" : "s"}`;
    showError($("review-error"), null);
  }
  function openReview(t) {
    const d = reviewDialog();
    reviewing = t;
    fill(t, t.manifest_text, t.code);
    $("review-test").textContent = testText(t);
    $("review-replaces").hidden = t.replaces == null;
    $("review-replaces").textContent = t.replaces != null ? "This replaces the installed tool of the same name." : "";
    const warn = $("review-warnings");
    warn.replaceChildren(...((t.proposal && t.proposal.warnings) || []).map((w) => el("li", null, w)));
    const inst = $("review-install");
    inst.hidden = false;
    $("review-decline").hidden = false;
    inst.disabled = !tools.allowed;
    inst.title = tools.allowed ? `Install it (asks for ${store.info ? store.info.confirm_with : "confirmation"})` : "Personal tools are turned off in your organization's settings";
    inst.onclick = install;
    $("review-decline").onclick = () => reviewing && decline(reviewing.id);
    d.showModal();
    $("review-code").focus();
  }
  async function openInstalled(t) {
    const d = reviewDialog();
    reviewing = null;
    try {
      const f = await invoke("settings_tool_files", { id: t.id });
      fill(t, f.manifest_text, f.code);
    } catch (e) {
      fill(t, "", "");
      showError($("review-error"), e);
    }
    $("review-test").textContent = t.installed ? `Installed ${when(t.installed)}` : "";
    $("review-replaces").hidden = true;
    $("review-warnings").replaceChildren();
    $("review-install").hidden = true;
    $("review-decline").hidden = true;
    d.showModal();
  }
  async function install() {
    if (!reviewing) return;
    const btn = $("review-install");
    btn.disabled = true;
    showError($("review-error"), null);
    try {
      await invoke("settings_tool_install", { id: reviewing.id, hash: reviewing.hash });
      $("tool-review").close();
      await refresh();
      notice.textContent = `Installed ${reviewing.title}. Your assistant can use it from its next request (as ${reviewing.tool}); there is no need to restart anything.`;
      notice.hidden = false;
    } catch (e) {
      showError($("review-error"), e);
    } finally {
      btn.disabled = !tools || !tools.allowed;
    }
  }
  await Promise.all([refresh(), drawSwitch()]);
  const onFocus = () => refresh();
  window.addEventListener("focus", onFocus);
  return () => window.removeEventListener("focus", onFocus);
}

export async function renderSkills(main) {
  const page = h("div", { class: "page" });
  main.append(page);
  page.append(pageHead("Skills", "Advice your assistant reads for GIS work: when to use which tool, and how. Skills never grant access to anything."));
  const body = h("div", { class: "pad stack" });
  page.append(h("div", { class: "scroll" }, body));
  let r;
  try { r = await invoke("skills_list"); } catch (e) { body.append(el("p", "error-text", String(e))); return; }
  body.append(h("div", { class: "notice" }, h("div", {}, r.from === "installed"
    ? `The skills installed beside Columbus's server, in ${r.dir}. Your assistant gets them from Columbus itself.`
    : "The skills that ship with this version of Columbus. (No installed copy was found beside the server.)")));
  if (!r.skills.length) { body.append(emptyState("No skills found", "Columbus's skills ship with the server.")); return; }
  const ul = h("ul", { class: "list" });
  for (const s of r.skills) ul.append(h("li", {}, h("div", { class: "text" }, h("b", { class: "mono" }, String(s.name)), s.description ? h("span", { class: "where" }, String(s.description)) : null), h("span", { class: "chip ro" }, "Built in")));
  body.append(h("section", { class: "card" }, h("div", { class: "hd" }, h("h2", {}, "Built in"), h("span", { class: "muted small" }, `${r.skills.length} skills · read only`)), h("div", { class: "bd flush" }, ul)));
  body.append(coming("skills", "Organization and imported skills",
    "Skills IT installs for your organization (district rules, for example) and skills you import from a file or GitHub, each with an on/off switch and marked \"not checked\" until their tool names match this server.", [], "Coming in a later stage"));
}
