// Tools: the person's own tools (ported from the Settings window), each
// read in full before it is installed, which is confirmed at the keyboard
// in the app's own code; and the Skills library, read only.
import { $, h, el, invoke, store, button, pageHead, sec, showError, when, numbered, emptyState } from "../core.js";
import { switchRow } from "./settings.js";
import { shareOut, shareIn, importedText, teamLibrary } from "./share.js";
import { toolboxSection } from "./toolboxes.js";

let tools = null;
let reviewing = null;
// Reads of the list begun so far (see `refresh`).
let reads = 0;

// Tools and skills are pages inside Workflows.
const BACK = { href: "#/tools", label: "Tools" };

function testText(t) {
  const p = t.proposal || {};
  const test = p.test || {};
  const parts = [];
  if (p.imported) return importedText(p);
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
  page.append(pageHead("Tools", "Toolboxes that bring many tools at once, and steps your assistant made from scripts, for it to reuse. You install each one here.",
    [h("a", { class: "btn sm", href: "#/skills", id: "to-skills", title: "Advice your assistant reads for GIS work" }, "Skills"),
      button("Open a shared file…", "sm", () => shareIn($("tools-error"), () => refresh()), { id: "tools-open-shared", title: "A tool or workflow a coworker shared: you review it here before anything is installed" })]));
  const body = h("div", { class: "pad stack" });
  page.append(h("div", { class: "scroll" }, body));
  const notice = h("p", { id: "restart", class: "notice ok", role: "status", hidden: true });
  const err = h("p", { id: "tools-error", class: "error-text", role: "alert", hidden: true });
  const off = h("p", { id: "tools-off", class: "notice warn", role: "status", hidden: true }, "Personal tools are turned off in your organization's settings, so none of these are used, and none can be installed.");
  const waiting = h("ul", { id: "tools-waiting", class: "list", "aria-live": "polite" });
  const installed = h("ul", { id: "tools-installed", class: "list", "aria-live": "polite" });
  const sw = h("div", {});
  const orgList = h("div", { id: "org-tools", class: "bd stack" });
  const orgCard = h("section", { class: "card", hidden: true }, sec("From your organization"), orgList);
  const teamBody = h("div", { class: "bd stack" });
  const teamCard = h("section", { class: "card", id: "team-library" }, sec("Team library"), teamBody);
  const waitingN = h("span", { class: "n" });
  const installedN = h("span", { class: "n" });
  const about = h("details", { class: "card", id: "tools-about" }, h("summary", { class: "hd" }, h("h2", {}, "How tools run, and your organization's switch")),
    h("div", { class: "bd stack" },
      h("p", { class: "small muted" }, "An installed tool runs whenever it is needed, without asking, in the same sandbox as scripts: no network; each run starts afresh, reads only the layers it is given for that run (and the Python installation) and writes only its own folder, which is deleted when the run ends. Ask your assistant, for example: ", h("i", {}, "add the script we just made as a tool"), "."),
      sw,
      h("p", { class: "small muted wrap-any" }, "Stored in ", h("code", { id: "tools-dir" }), ". Built-in tools, engine plugins and connected servers are listed by your assistant (list_providers).")));
  teamLibrary(teamBody, err, () => refresh());
  invoke("org_tools_list").then((o) => {
    const list = o.tools || [];
    if (!list.length) return;
    orgCard.hidden = false;
    orgList.replaceChildren(
      h("p", { class: "small muted" }, `Installed for everyone on this computer by whoever administers it. They run like your own tools (confined, no network) and need no approval.${o.on ? "" : " Turned off in your organization's settings, so none of them are used."}`),
      ...list.map((t) => h("div", { class: "tool" }, h("div", { class: "text" }, h("b", {}, t.title || t.id),
        h("span", { class: "where" }, `${t.tool} · version ${t.version || "?"} · ${t.state === "ready" ? "ready" : "not approved"}`),
        t.description ? h("span", {}, t.description) : null))));
  }).catch(() => {});
  // Toolboxes first (their own module): programs that bring many tools.
  const boxes = toolboxSection();
  body.append(boxes.card, notice, err, off,
    h("section", { class: "card" }, h("div", { class: "sec" }, h("h2", {}, "Waiting for you"), waitingN), h("div", { class: "bd flush" }, waiting), h("p", { id: "no-waiting", class: "muted small quiet", hidden: true }, "Nothing is waiting.")),
    h("section", { class: "card" }, h("div", { class: "sec" }, h("h2", {}, "Installed"), installedN), h("div", { class: "bd flush" }, installed), h("p", { id: "no-installed", class: "muted small quiet", hidden: true }, "No tools installed yet.")),
    orgCard,
    teamCard,
    about);

  const draw = () => {
    $("tools-dir").textContent = tools.dir;
    off.hidden = tools.allowed;
    if (!tools.allowed) about.open = true;
    waitingN.textContent = tools.waiting.length ? String(tools.waiting.length) : "";
    installedN.textContent = tools.installed.length ? String(tools.installed.length) : "";
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
      const b = button("Review", "sm", () => openReview(t), { title: "Read its code and manifest, then install or decline it" });
      b.disabled = !!t.problem;
      const d = button("Decline", "ghost sm", () => decline(t.id));
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
      text.append(el("span", t.state === "ready" && tools.allowed ? "where" : "where off", state));
      if (t.problem) text.append(el("span", "where error-text", `Not used: ${t.problem}. Remove it, and ask your assistant to propose it again.`));
      const v = button("Code", "sm", () => openInstalled(t));
      const sh = button("Share…", "sm", () => shareOut("tool", t.id, err), { title: "Save it as a file to send a coworker; they review and install it themselves" });
      sh.disabled = t.state !== "ready";
      const r = button("Remove", "ghost sm", () => removeTool(t.id), { title: "Remove it: your assistant cannot use it from its next request" });
      li.append(text, h("div", { class: "acts" }, v, sh, r));
      installed.append(li);
    }
  };
  // Only the read begun last is kept and drawn: one begun before a tool was
  // installed, declined or removed can end after the one begun since (the
  // list is also read whenever the window gets the focus), and would put
  // back what is gone. Counted for the whole module, as `tools` is.
  const refresh = async () => {
    const mine = ++reads;
    try {
      const now = await invoke("settings_tools");
      if (mine !== reads) return;
      tools = now;
      store.set("tools", tools);
      draw();
      showError(err, null);
    } catch (e) { if (mine === reads) showError(err, e); }
  };
  // The same for the switches: only the read begun last is drawn.
  let switchReads = 0;
  const drawSwitch = async () => {
    const mine = ++switchReads;
    try {
      const st = await invoke("settings_state");
      if (mine !== switchReads) return;
      sw.replaceChildren(switchRow("allow_personal_tools", st, async () => { await drawSwitch(); await refresh(); }),
        switchRow("allow_shared_tools", st, async () => { await drawSwitch(); }));
    } catch (e) { if (mine === switchReads) sw.replaceChildren(el("p", "error-text", String(e))); }
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
        h("button", { type: "button", id: "review-close", class: "btn" }, "Not now"),
        h("button", { type: "button", id: "review-install", class: "btn primary" }, "I've read it. Install it."))));
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
    $("review-close").textContent = "Not now";
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
    $("review-close").textContent = "Close";
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
  await Promise.all([refresh(), drawSwitch(), boxes.refresh()]);
  const onFocus = () => { refresh(); boxes.refresh(); };
  window.addEventListener("focus", onFocus);
  return () => { window.removeEventListener("focus", onFocus); boxes.close(); };
}

export async function renderSkills(main) {
  const page = h("div", { class: "page" });
  main.append(page);
  page.append(pageHead("Skills", "Advice your assistant reads for GIS work: when to use which tool, and how. Skills never grant access to anything.", [], BACK));
  const body = h("div", { class: "pad stack" });
  page.append(h("div", { class: "scroll" }, body));
  let r;
  try { r = await invoke("skills_list"); } catch (e) { body.append(el("p", "error-text", String(e))); return; }
  const from = r.from === "installed"
    ? `Installed beside Columbus's server, in ${r.dir}. Your assistant gets them from Columbus itself.`
    : "These ship with this version of Columbus. (No installed copy was found beside the server.)";
  if (!r.skills.length) { body.append(emptyState("No skills found", "Columbus's skills ship with the server.")); return; }
  const ul = h("ul", { class: "list" });
  // A skill's description is long: two lines, and all of it on a click.
  for (const s of r.skills) {
    const li = h("li", { class: "clickable clamp", tabindex: "0", title: "Show all of it" }, h("div", { class: "text" }, h("b", { class: "mono" }, String(s.name)), s.description ? h("span", { class: "where" }, String(s.description)) : null));
    const toggle = () => li.classList.toggle("clamp");
    li.addEventListener("click", toggle);
    li.addEventListener("keydown", (e) => { if (e.key === "Enter" || e.key === " ") { e.preventDefault(); toggle(); } });
    ul.append(li);
  }
  body.append(h("section", { class: "card" }, sec("Built in, read only", r.skills.length), h("div", { class: "bd flush" }, ul)), h("p", { class: "small muted wrap-any" }, from));
}
