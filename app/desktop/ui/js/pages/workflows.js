// Workflows: jobs your assistant proposed (propose_workflow), each read in
// plain words and in full before you approve it, which is confirmed at the
// keyboard in the app's own code; their schedules; Run now; and every run's
// report. Your workflows are yours alone: nothing here is shared.
import { $, h, el, icon, invoke, store, button, pageHead, placeHead, sec, menuButton, showError, when, shortWhen, plural, numbered, emptyState } from "../core.js";
import { switchRow } from "./settings.js";
import { shareOut, shareIn, importedText } from "./share.js";
import { markdown } from "./chats.js";

let state = null;
let reviewing = null;
let onFocus = null;

const STATUS = {
  succeeded: ["Succeeded", "pass"],
  succeeded_with_warnings: ["Succeeded, to review", "warn"],
  needs_you: ["Needs you", "warn"],
  needs_reapproval: ["Needs re-approval", "warn"],
  off: ["Off", "mute"],
  skipped: ["Skipped (already running)", "mute"],
  stopped_at_limit: ["Stopped at its time limit", "fail"],
  failed: ["Failed", "fail"],
  running: ["Running", "mute"],
};

function statusBadge(s) {
  const [t, c] = STATUS[s] || [s || "?", "mute"];
  return el("span", `badge ${c}`, t);
}

function stateText(w) {
  if (w.state === "ready") return state.allowed ? null : "Off: your organization's settings turn workflows off";
  if (w.state === "changed") return "Off: its file changed since you approved it. Ask your assistant to propose it again.";
  if (w.state === "tools_changed") return "Off: a personal tool it uses changed since you approved it. Ask your assistant to propose it again.";
  if (w.state === "invalid") return `Off: ${w.problem || "its file breaks a rule"}`;
  return "Off: not approved";
}

// When a workflow runs, in a few words: "weekdays at 06:00".
function scheduleWhen(w) {
  if (!w.at) return "runs when you start it";
  const days = w.days.length === 7 ? "every day" : (w.days.join(",") === "mon,tue,wed,thu,fri" ? "weekdays" : w.days.join(", "));
  return `${days} at ${w.at}`;
}

function scheduleText(w) {
  if (!w.at) return "Runs when you start it";
  const entry = { on: "", off: " · paused", differs: " · schedule needs repair", none: "" }[w.schedule_entry] ?? "";
  return `${scheduleWhen(w)}${entry}`;
}

// A run's state as a dot: passed, to look at, failed, or nothing to say.
function statusDot(s) {
  const c = (STATUS[s] || ["", "mute"])[1];
  return h("span", { class: `dot ${c === "pass" ? "ok" : c === "warn" ? "warn" : c === "fail" ? "bad" : ""}`, title: (STATUS[s] || [s || ""])[0] });
}

const RUNS_SHOWN = 20;
const NEEDS = ["failed", "needs_you", "needs_reapproval", "stopped_at_limit"];

const go = (hash) => { location.hash = hash; };
const wfHash = (id, run) => `#/workflows/${encodeURIComponent(id)}${run ? `/${encodeURIComponent(run)}` : ""}`;

// Three levels, so many workflows with many runs stay tidy: the list (one
// row each, its last run), one workflow (its settings and its runs), and
// one run's report on its own page.
export async function render(main, rest) {
  const [wid, rid] = (rest || []).map((x) => decodeURIComponent(x));
  onFocus = null;
  const cleanup = [];
  const page = h("div", { class: "page" });
  main.append(page);
  const body = h("div", { class: "pad stack" });
  const notice = h("p", { id: "wf-notice", class: "notice ok", role: "status", hidden: true });
  const err = h("p", { id: "wf-error", class: "error-text", role: "alert", hidden: true });
  const say = (t) => { notice.textContent = t; notice.hidden = false; };
  const refresh = async () => {
    try {
      state = await invoke("workflows_state");
      store.set("workflows", state);
      showError(err, null);
      return true;
    } catch (e) { showError(err, e); return false; }
  };

  async function runNow(w, btn) {
    btn.disabled = true;
    const was = btn.textContent;
    btn.textContent = "Running…";
    showError(err, null);
    try {
      const r = await invoke("workflow_run_now", { id: w.id });
      await refresh();
      go(wfHash(w.id, r.run));
    } catch (e) {
      showError(err, e);
      btn.textContent = was;
      btn.disabled = false;
    }
  }
  const ready = (w) => w.state === "ready" && state.allowed;

  await refresh();
  if (wid && rid) await reportView();
  else if (wid) await detailView();
  else await listView();

  // ---- The list: what waits for you, then everything that can run -------
  async function listView() {
    const search = h("input", { type: "search", placeholder: "Search workflows", "aria-label": "Search workflows" });
    page.append(placeHead("Workflows", search,
      [button("Open a shared one", "", () => shareIn(err, () => refreshList()), { id: "wf-open-shared", title: "A workflow or tool a coworker shared: you review it here before anything runs" })]));
    page.append(h("div", { class: "scroll" }, body),
      h("div", { class: "footline" }, h("span", { class: "grow" }, "To make one, ask for it in a chat: ", h("i", {}, "“Repeat this every Monday.”"))));
    const off = h("p", { id: "wf-off", class: "notice warn", role: "status", hidden: true }, "Workflows are turned off in your organization's settings, so none run, and none can be approved. The switch is under How workflows work, below.");
    const noSched = h("p", { id: "wf-nosched", class: "notice", role: "status", hidden: true }, "Schedules are not available on this system yet. Run now works.");
    const macNote = h("p", { id: "wf-macnote", class: "small muted", hidden: true }, "On a Mac, a scheduled run cannot ask for access to Documents, Desktop, Downloads, iCloud Drive, network or removable drives: keep a workflow's data and export folders elsewhere (your Columbus folder, for example), or give columbus-mcp Full Disk Access in System Settings › Privacy & Security.");
    const waiting = h("ul", { id: "wf-waiting", class: "list", "aria-live": "polite" });
    const waitingN = h("span", { class: "n", id: "wf-waiting-h" });
    const waitingCard = h("section", { class: "card", id: "wf-waiting-card" },
      h("div", { class: "sec" }, h("h2", {}, "Can't run until you read it"), waitingN), h("div", { class: "bd flush" }, waiting));
    const installed = h("ul", { id: "wf-installed", class: "list", "aria-live": "polite" });
    const noneEx = h("i", {}, "make what we just did a workflow that runs every weekday at 6");
    const noneEnd = document.createTextNode(".");
    const none = h("p", { id: "wf-no-installed", class: "muted small quiet", hidden: true }, "Nothing yet. Ask your assistant, for example: ", noneEx, noneEnd);
    const sw = h("div", {});
    const more = h("ul", { class: "list compact", id: "wf-more-list" });
    const moreRow = (href, title, sub) => {
      const li = h("li", { class: "clickable" }, h("div", { class: "text" }, h("b", {}, h("a", { href }, title))), h("span", { class: "when" }, sub), icon("forward"));
      li.addEventListener("click", (e) => { if (e.target.tagName !== "A") go(href); });
      return li;
    };
    const toolsSub = h("span", {}, "steps your assistant can reuse");
    more.append(moreRow("#/tools", "Tools", toolsSub));
    const moreCard = h("section", { class: "card" }, sec("More"), h("div", { class: "bd flush" }, more));
    const about = h("details", { class: "card", id: "wf-about" }, h("summary", { class: "hd" }, h("h2", {}, "How workflows work, and your organization's switch")),
      h("div", { class: "bd stack" },
        h("p", { class: "small muted" }, "A workflow reads only the connections it lists, runs only the steps it lists (reading and processing tools, and personal tools you installed), and never waits for you: a step that needs an approval stops the run with \"needs you\". Runs happen while you are signed in to this computer; one missed while it slept runs once when it wakes. Share one with a coworker from its page: they open the file here, choose their own connections and approve it themselves. Your workflows are yours alone."),
        sw, h("p", { class: "small muted wrap-any" }, "Stored in ", h("code", { id: "wf-dir" }))));
    body.append(notice, err, off, noSched, macNote, waitingCard, h("section", { class: "card" }, installed, none), moreCard, about, emailCard());

    const draw = () => {
      const q = search.value.trim().toLowerCase();
      const hit = (name) => !q || String(name).toLowerCase().includes(q);
      $("wf-dir").textContent = state.dir;
      off.hidden = state.allowed;
      noSched.hidden = !!state.scheduler;
      macNote.hidden = state.scheduler !== "launchd";
      if (!state.allowed) about.open = true;
      waiting.replaceChildren();
      const tools = store.tools || { waiting: [], installed: [] };
      for (const w of state.waiting.filter((x) => hit(x.name))) {
        const li = el("li", w.problem ? "tool bad" : "tool");
        const text = el("div", "text");
        text.append(el("b", null, w.name), el("span", "where", `${w.replaces != null ? "A change to your workflow" : "New"}${w.proposal && w.proposal.proposed ? ` · proposed ${when(w.proposal.proposed)}` : ""} · ${w.at ? `would run ${scheduleText({ ...w, schedule_entry: "none" })}` : "would run when you start it"}`));
        if (w.purpose) text.append(el("span", "where", w.purpose));
        if (w.proposal && w.proposal.imported) text.append(el("span", "where", importedText(w.proposal, false)));
        if (w.problem) text.append(el("span", "where error-text", w.problem));
        const b = button("Review", "sm", () => openReview(w, refreshList), { title: "Read what it does, then approve or decline it" });
        b.disabled = !!w.problem;
        li.append(text, h("div", { class: "acts" }, b, button("Decline", "ghost sm", () => decline(w.id, refreshList))));
        waiting.append(li);
      }
      // A tool waiting to be read is read on the Tools page.
      for (const t of tools.waiting.filter((x) => hit(x.title))) {
        waiting.append(h("li", { class: "tool", "data-tool": t.id }, h("div", { class: "text" }, h("b", {}, t.title), el("span", "where", "A tool your assistant made from a script")),
          h("div", { class: "acts" }, h("a", { class: "btn sm", href: "#/tools" }, "Review"))));
      }
      const waits = state.waiting.length + tools.waiting.length;
      waitingCard.hidden = !waiting.children.length;
      waitingN.textContent = waits ? String(waits) : "";
      installed.replaceChildren();
      const row = (w) => {
        const li = el("li", "tool clickable");
        li.dataset.id = w.id;
        li.addEventListener("click", (ev) => { if (!ev.target.closest("button, a")) go(wfHash(w.id)); });
        const text = el("div", "text");
        const line = h("span", { class: "where" }, scheduleText(w), " · ");
        if (w.last_run) line.append("last run ", (STATUS[w.last_run.status] || [w.last_run.status || "?"])[0].toLowerCase(), w.last_run.started ? `, ${shortWhen(w.last_run.started)}` : "");
        else line.append("not run yet");
        text.append(h("b", {}, h("a", { href: wfHash(w.id) }, w.name)), line);
        const why = stateText(w);
        if (why) text.append(el("span", "where off", why));
        li.append(text, w.last_run ? statusDot(w.last_run.status) : icon("forward"));
        return li;
      };
      const group = (label, rows) => {
        if (!rows.length) return;
        installed.append(h("li", { class: "label" }, h("span", {}, label), h("span", { class: "n" }, String(rows.length))), ...rows);
      };
      const mine = state.installed.filter((w) => hit(w.name));
      group("Runs by itself", mine.filter((w) => w.at).map(row));
      group("Runs when you ask", [...mine.filter((w) => !w.at).map(row),
        ...tools.installed.filter((t) => hit(t.title)).map((t) => {
          const li = h("li", { class: "tool clickable", "data-tool": t.id }, h("div", { class: "text" }, h("b", {}, h("a", { href: "#/tools" }, t.title)),
            el("span", "where", `A step your assistant can reuse${t.state === "ready" && tools.allowed ? "" : " · off"}`)), icon("forward"));
          li.addEventListener("click", (ev) => { if (!ev.target.closest("a")) go("#/tools"); });
          return li;
        })]);
      none.hidden = installed.children.length > 0;
      // A search with no match says only that; the example is for an empty list.
      none.firstChild.textContent = q ? "Nothing matches." : "Nothing yet. Ask your assistant, for example: ";
      noneEx.hidden = !!q;
      noneEnd.textContent = q ? "" : ".";
      toolsSub.textContent = tools.installed.length ? `${plural(tools.installed.length, "tool")} installed` : "steps your assistant can reuse";
      moreCard.hidden = about.hidden = !!q;
    };
    async function refreshList() { if (await refresh()) draw(); }
    const drawSwitch = async () => {
      try {
        const st = await invoke("settings_state");
        sw.replaceChildren(switchRow("allow_workflows", st, async () => { await drawSwitch(); await refreshList(); }),
          switchRow("allow_shared_tools", st, async () => { await drawSwitch(); }));
      } catch (e) { sw.replaceChildren(el("p", "error-text", String(e))); }
    };
    search.addEventListener("input", () => { if (state) draw(); });
    const unsub = store.on((k) => { if (k === "tools" && state) draw(); });
    cleanup.push(unsub);
    if (state) draw();
    await drawSwitch();
    onFocus = refreshList;
  }

  // ---- One workflow: when it runs, what it does, and its runs ----------
  async function detailView() {
    const w = state && state.installed.find((x) => x.id === wid);
    const back = { href: "#/workflows", id: "wf-back", label: "All workflows" };
    if (!w) {
      page.append(pageHead("Workflow", null, [], back), h("div", { class: "scroll" }, body));
      body.append(notice, err, emptyState("Not found", "This workflow isn't installed (it may have been removed). Its past runs' reports are kept."));
      return;
    }
    const run = button("Run now", "sm", () => runNow(w, run), { id: "wf-run-now", title: "Run it once now, as its schedule would" });
    run.prepend(icon("play"));
    run.disabled = !ready(w);
    const menu = menuButton("More", () => [
      w.at && state.scheduler && w.schedule_entry === "on" ? { label: "Pause its schedule", id: "wf-pause", onclick: () => setSchedule(false) } : null,
      w.at && state.scheduler && w.schedule_entry !== "on" ? { label: w.schedule_entry === "differs" ? "Repair its schedule" : "Resume its schedule", id: "wf-resume", disabled: !ready(w), onclick: () => setSchedule(true) } : null,
      { label: "Share…", id: "wf-share", disabled: !ready(w), title: "Save it, with the tools it uses, as a file to send a coworker; they choose their own connections and approve it themselves. Its schedule and runs stay here.", onclick: () => shareOut("workflow", w.id, err) },
      { label: "Remove", id: "wf-remove", danger: true, title: "It stops running; its past runs' reports are kept", onclick: removeWf },
    ], { class: "iconbtn wf-more" });
    page.append(pageHead(w.name, null, [run, menu], back), h("div", { class: "scroll" }, body));
    const why = stateText(w);
    const sched = scheduleWhen(w);
    // Whether its schedule is in force on this computer, in a line.
    const entry = !w.at ? null
      : !state.scheduler ? "Schedules are not available on this system yet. Run now works."
        : w.schedule_entry === "off" ? "Its schedule is paused: it runs only when you start it."
          : w.schedule_entry === "differs" ? "Its schedule on this computer needs repair (More › Repair its schedule)." : null;
    body.append(notice, err, ...[
      why ? el("p", "notice warn", why) : null,
      h("h2", { class: "big" }, `${sched[0].toUpperCase()}${sched.slice(1)}.`),
      entry ? h("p", { class: "small muted", id: "wf-entry" }, entry) : null,
      w.purpose ? h("p", {}, w.purpose) : null,
      h("p", { class: "small muted wrap-any" }, [w.installed ? `Approved ${when(w.installed)}` : null, `id ${w.id}`].filter(Boolean).join(" · ")),
    ].filter(Boolean));
    const runsN = h("span", { class: "n", id: "wf-runs-h" });
    const runsBd = h("div", { class: "bd flush" });
    body.append(h("section", { class: "card", id: "wf-runs" }, h("div", { class: "sec" }, h("h2", {}, "Runs"), runsN), runsBd));
    let runs = [];
    try { runs = await invoke("workflow_runs", { id: w.id }); } catch (e) { runsBd.append(el("p", "pad error-text", String(e))); return; }
    if (!runs.length) { runsBd.append(h("p", { class: "pad muted small" }, "No runs yet. Run it now, or wait for its schedule.")); return; }
    runsN.textContent = `${runs.length}${runs.length >= 100 ? "+" : ""}`;
    const ul = h("ul", { class: "list", id: "wf-run-list" });
    let shown = 0;
    const more = button("Show older runs", "sm", () => drawMore(), { id: "wf-more" });
    const drawMore = () => {
      for (const r of runs.slice(shown, shown + RUNS_SHOWN)) {
        const li = el("li", "tool clickable");
        li.addEventListener("click", () => go(wfHash(w.id, r.run)));
        li.append(statusDot(r.status), h("div", { class: "text" },
          h("b", {}, h("a", { href: wfHash(w.id, r.run) }, r.started ? when(r.started) : r.run)),
          h("span", { class: "where" }, `${(STATUS[r.status] || [r.status || "?"])[0]} · ${r.scheduled ? "On its schedule" : "Started by you"}${r.reason ? ` · ${r.reason}` : ""}`)),
          icon("forward"));
        ul.append(li);
      }
      shown += RUNS_SHOWN;
      more.hidden = shown >= runs.length;
    };
    runsBd.append(ul, h("div", { class: "pad" }, more));
    drawMore();
    async function setSchedule(on) {
      try { await invoke("workflow_set_schedule", { id: w.id, on }); await refresh(); location.reload(); } catch (e) { showError(err, e); }
    }
    async function removeWf() {
      try {
        await invoke("workflow_remove", { id: w.id });
        await refresh();
        go("#/workflows");
      } catch (e) { showError(err, e); }
    }
  }

  // ---- One run's report, on its own page -------------------------------
  async function reportView() {
    const w = state && state.installed.find((x) => x.id === wid);
    const name = w ? w.name : wid;
    const acts = h("div", { class: "row" });
    page.append(pageHead(`${name}: run report`, null, [acts], { href: wfHash(wid), id: "wf-back", label: name }));
    page.append(h("div", { class: "scroll" }, body));
    body.append(notice, err);
    const box = h("div", { class: "report stack", id: "wf-report" });
    body.append(box);
    try {
      const r = await invoke("workflow_run_report", { id: wid, run: rid });
      const rec = r.record || {};
      box.append(h("p", { class: "small" }, statusBadge(rec.status), " ", rec.started ? when(rec.started) : rid, rec.scheduled ? " · on its schedule" : " · started by you"));
      const m = h("div", { class: "msg ai" });
      markdown(m, r.report || "(no report)");
      box.append(m);
      if (rec.workspace) box.append(h("p", { class: "small muted wrap-any" }, "This run's files: ", h("code", {}, String(rec.workspace))));
      // REQ-27: review the run's results with the assistant, on the map.
      const layers = Array.isArray(rec.outputs) ? rec.outputs.length : 0;
      if (layers && rec.finished) {
        acts.append(button("Open in a chat", "sm primary", () => { location.hash = `#/chats/run/${encodeURIComponent(wid)}/${encodeURIComponent(rid)}/${encodeURIComponent(name)}`; },
          { id: "wf-open-chat", title: "A new chat with this run's layers on its map and in its table, and its report for your assistant. The run itself is not changed." }));
        box.append(h("p", { class: "small muted" }, `${layers} layer${layers === 1 ? "" : "s"}: open them in a chat to review them with your assistant on the map, then export what you approve.`));
      }
    } catch (e) { box.append(el("p", "error-text", String(e))); }
  }

  async function decline(id, after) {
    try {
      await invoke("workflow_decline", { id });
      const r = $("wf-review");
      if (r && r.open) r.close();
      await after();
    } catch (e) { showError($("wf-review")?.open ? $("wf-review-error") : err, e); }
  }

  function reviewDialog() {
    let d = $("wf-review");
    if (d) return d;
    d = h("dialog", { id: "wf-review", "aria-labelledby": "wf-review-h" }, h("div", { class: "dlg" },
      h("h2", { id: "wf-review-h" }, "Workflow"),
      h("p", { id: "wf-review-id", class: "muted small wrap-any" }),
      h("p", { id: "wf-review-purpose" }),
      h("p", { id: "wf-review-imported", class: "notice", hidden: true }),
      h("h3", {}, "What it does"),
      h("ul", { id: "wf-review-plain", class: "small" }),
      h("p", { id: "wf-review-replaces", class: "notice", hidden: true }),
      h("p", { class: "small muted" }, "Read it before it can run. Once approved it runs by itself, on its schedule, without asking you. A change to it, or to a personal tool it uses, turns it off until you approve it again."),
      h("h3", {}, "The workflow file ", h("span", { id: "wf-review-lines", class: "muted small" })), h("div", { id: "wf-review-box" }),
      h("p", { id: "wf-review-error", class: "error-text", role: "alert", hidden: true }),
      h("div", { class: "actions" },
        h("button", { type: "button", id: "wf-review-decline", class: "btn danger" }, "Decline"), h("span", { class: "spacer" }),
        h("button", { type: "button", id: "wf-review-close", class: "btn" }, "Not now"),
        h("button", { type: "button", id: "wf-review-install", class: "btn primary" }, "I've read it. Approve it."))));
    document.body.append(d);
    $("wf-review-close").addEventListener("click", () => d.close());
    return d;
  }
  function openReview(w, after) {
    const d = reviewDialog();
    reviewing = w;
    $("wf-review-h").textContent = w.name;
    $("wf-review-id").textContent = `${w.id} · ${w.fingerprint}`;
    $("wf-review-purpose").textContent = w.purpose || "";
    const imp = importedText(w.proposal, false);
    $("wf-review-imported").textContent = imp ? `${imp} Every connection it reads is one you chose when you opened it.` : "";
    $("wf-review-imported").hidden = !imp;
    $("wf-review-plain").replaceChildren(...(w.plain || []).map((t) => el("li", null, t)));
    const f = numbered(w.text || "");
    f.pre.id = "wf-review-file";
    $("wf-review-box").replaceChildren(f.pre);
    $("wf-review-lines").textContent = `${f.count} line${f.count === 1 ? "" : "s"}`;
    $("wf-review-replaces").hidden = w.replaces == null;
    $("wf-review-replaces").textContent = w.replaces != null ? "This replaces your workflow of the same id." : "";
    showError($("wf-review-error"), null);
    const inst = $("wf-review-install");
    inst.disabled = !state.allowed;
    inst.title = state.allowed ? `Approve it (asks for ${store.info ? store.info.confirm_with : "confirmation"})` : "Workflows are turned off in your organization's settings";
    inst.onclick = () => install(after);
    $("wf-review-decline").onclick = () => reviewing && decline(reviewing.id, after);
    d.showModal();
    $("wf-review-file").focus();
  }
  async function install(after) {
    if (!reviewing) return;
    const btn = $("wf-review-install");
    btn.disabled = true;
    showError($("wf-review-error"), null);
    try {
      await invoke("workflow_install", { id: reviewing.id, fingerprint: reviewing.fingerprint });
      $("wf-review").close();
      await after();
      say(`Approved ${reviewing.name}. ${reviewing.at ? "It runs on its schedule from now on; " : ""}Run now runs it once and shows its report.`);
    } catch (e) {
      showError($("wf-review-error"), e);
    } finally {
      btn.disabled = !state || !state.allowed;
    }
  }
  let focusFn = () => onFocus && onFocus();
  window.addEventListener("focus", focusFn);
  return () => { window.removeEventListener("focus", focusFn); for (const fn of cleanup) fn(); };
}

// Email for workflows' reports: as the organization set it up, and the mail
// server's password (kept in the credential store, never shown again).
function emailCard() {
  const bd = h("div", { class: "bd stack", id: "wf-email" });
  const card = h("details", { class: "card" }, h("summary", { class: "hd" }, h("h2", {}, "Email")), bd);
  invoke("email_settings").then((e) => {
    if (!e.on) {
      bd.append(h("p", { class: "small muted" }, "Your organization hasn't set up email for workflows (the policy's email section), so a workflow can't send its report by email. Exports to folders work as before."));
      return;
    }
    bd.append(h("p", { class: "small" }, `Workflows can email their report through ${e.host}:${e.port} (${e.security === "tls" ? "TLS" : "STARTTLS"}), from ${e.from}, to addresses in ${e.allowed_domains.join(", ")} only. ${e.attachments ? `A report can carry the files the run exported, up to ${e.max_attachment_mb} MB.` : "Reports go without attachments."} Your assistant adds email to a workflow when it proposes one; you see the addresses when you approve it.`));
    if (e.username) {
      const pw = h("input", { type: "password", autocomplete: "off", "aria-label": "Mail server password", id: "wf-email-pw" });
      const err = h("p", { class: "error-text small", role: "alert", hidden: true });
      const save = button("Save password", "sm", async () => {
        showError(err, null);
        try { await invoke("email_password_set", { password: pw.value }); pw.value = ""; save.textContent = "Saved"; } catch (x) { showError(err, x); }
      });
      bd.append(h("label", { class: "field" }, `Password for ${e.username} on the mail server (kept in your credential store; leave empty and save to forget it)`, pw), h("div", { class: "row" }, save), err);
    }
  }).catch(() => {});
  return card;
}
