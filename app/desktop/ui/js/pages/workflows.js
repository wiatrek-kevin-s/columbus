// Workflows: jobs your assistant proposed (propose_workflow), each read in
// plain words and in full before you approve it, which is confirmed at the
// keyboard in the app's own code; their schedules; Run now; and every run's
// report. Your workflows are yours alone: nothing here is shared.
import { $, h, el, invoke, store, button, pageHead, showError, when, numbered, emptyState } from "../core.js";
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

function scheduleText(w) {
  if (!w.at) return "Runs when you start it";
  const days = w.days.length === 7 ? "every day" : (w.days.join(",") === "mon,tue,wed,thu,fri" ? "weekdays" : w.days.join(", "));
  const entry = { on: "", off: " · paused", differs: " · schedule needs repair", none: "" }[w.schedule_entry] ?? "";
  return `${days} at ${w.at}${entry}`;
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

  // ---- The list: what waits for you, and one row per workflow ----------
  async function listView() {
    page.append(pageHead("Workflows", "Jobs that run by themselves, without you waiting: your assistant proposes one, you read it and approve it here. They are yours alone.",
      [button("Open a shared file…", "", () => shareIn(err, () => refreshList()), { id: "wf-open-shared", title: "A workflow a coworker shared: you review it here before anything runs" })]));
    page.append(h("div", { class: "scroll" }, body));
    const off = h("p", { id: "wf-off", class: "notice warn", role: "status", hidden: true }, "Workflows are turned off in your organization's settings, so none run, and none can be approved. The switch is at the bottom of this page.");
    const noSched = h("p", { id: "wf-nosched", class: "notice", role: "status", hidden: true }, "Schedules are not available on this system yet. Run now works.");
    const macNote = h("p", { id: "wf-macnote", class: "small muted", hidden: true }, "On a Mac, a scheduled run cannot ask for access to Documents, Desktop, Downloads, iCloud Drive, network or removable drives: keep a workflow's data and export folders elsewhere (your Columbus folder, for example), or give columbus-mcp Full Disk Access in System Settings › Privacy & Security.");
    const waiting = h("ul", { id: "wf-waiting", class: "list", "aria-live": "polite" });
    const waitingCard = h("section", { class: "card", id: "wf-waiting-card" },
      h("div", { class: "hd" }, h("h2", { id: "wf-waiting-h" }, "Waiting for you")), h("div", { class: "bd flush" }, waiting));
    const installed = h("ul", { id: "wf-installed", class: "list", "aria-live": "polite" });
    const sw = h("div", {});
    body.append(notice, err, off, noSched, macNote, waitingCard,
      h("section", { class: "card" }, h("div", { class: "hd" }, h("h2", {}, "Your workflows")),
        h("div", { class: "bd flush" }, installed, h("p", { id: "wf-no-installed", class: "pad muted small", hidden: true },
          "No workflows yet. Ask your assistant, for example: ", h("i", {}, "make what we just did a workflow that runs every weekday at 6"), "."))),
      h("details", { class: "card", id: "wf-about" }, h("summary", { class: "hd" }, h("h2", {}, "How workflows work, and your organization's switch")),
        h("div", { class: "bd stack" },
          h("p", { class: "small muted" }, "A workflow reads only the connections it lists, runs only the steps it lists (reading and processing tools, and personal tools you installed), and never waits for you: a step that needs an approval stops the run with \"needs you\". Runs happen while you are signed in to this computer; one missed while it slept runs once when it wakes. Share one with a coworker from its page: they open the file here, choose their own connections and approve it themselves."),
          sw, h("p", { class: "small muted wrap-any" }, "Stored in ", h("code", { id: "wf-dir" })))));

    const draw = () => {
      $("wf-dir").textContent = state.dir;
      off.hidden = state.allowed;
      noSched.hidden = !!state.scheduler;
      macNote.hidden = state.scheduler !== "launchd";
      $("wf-about").open = !state.allowed;
      waiting.replaceChildren();
      waitingCard.hidden = !state.waiting.length;
      $("wf-waiting-h").textContent = `Waiting for you (${state.waiting.length})`;
      for (const w of state.waiting) {
        const li = el("li", w.problem ? "tool bad" : "tool");
        const text = el("div", "text");
        text.append(el("b", null, w.name), el("span", "where", `${w.replaces != null ? "A change to your workflow" : "New"}${w.proposal && w.proposal.proposed ? ` · proposed ${when(w.proposal.proposed)}` : ""} · ${w.at ? `would run ${scheduleText({ ...w, schedule_entry: "none" })}` : "would run when you start it"}`));
        if (w.purpose) text.append(el("span", null, w.purpose));
        if (w.proposal && w.proposal.imported) text.append(el("span", "where", importedText(w.proposal, false)));
        if (w.problem) text.append(el("span", "where error-text", w.problem));
        const b = button("Review", "primary sm", () => openReview(w, refreshList), { title: "Read what it does, then approve or decline it" });
        b.disabled = !!w.problem;
        li.append(text, h("div", { class: "acts" }, b, button("Decline", "danger sm", () => decline(w.id, refreshList))));
        waiting.append(li);
      }
      installed.replaceChildren();
      $("wf-no-installed").hidden = state.installed.length > 0;
      for (const w of state.installed) {
        const li = el("li", "tool clickable");
        li.dataset.id = w.id;
        li.addEventListener("click", (ev) => { if (!ev.target.closest("button")) go(wfHash(w.id)); });
        const text = el("div", "text");
        const line = h("span", { class: "where" }, scheduleText(w), " · ");
        if (w.last_run) line.append("last run ", statusBadge(w.last_run.status), w.last_run.started ? ` ${when(w.last_run.started)}` : "");
        else line.append("not run yet");
        text.append(h("b", {}, h("a", { href: wfHash(w.id) }, w.name)), line);
        const why = stateText(w);
        if (why) text.append(h("div", { class: "chips" }, el("span", "chip off", why)));
        const run = button("Run now", "primary sm", () => runNow(w, run), { title: "Run it once now, as its schedule would" });
        run.disabled = !ready(w);
        li.append(text, h("div", { class: "acts" }, run));
        installed.append(li);
      }
    };
    async function refreshList() { if (await refresh()) draw(); }
    const drawSwitch = async () => {
      try {
        const st = await invoke("settings_state");
        sw.replaceChildren(switchRow("allow_workflows", st, async () => { await drawSwitch(); await refreshList(); }),
          switchRow("allow_shared_tools", st, async () => { await drawSwitch(); }));
      } catch (e) { sw.replaceChildren(el("p", "error-text", String(e))); }
    };
    if (state) draw();
    await drawSwitch();
    onFocus = refreshList;
  }

  // ---- One workflow: what it does, and its runs ------------------------
  async function detailView() {
    const w = state && state.installed.find((x) => x.id === wid);
    page.append(pageHead(w ? w.name : "Workflow", w && w.purpose ? w.purpose : null));
    page.append(h("div", { class: "scroll" }, body));
    body.append(h("p", { class: "small" }, h("a", { href: "#/workflows", id: "wf-back" }, "← All workflows")), notice, err);
    if (!w) { body.append(emptyState("Not found", "This workflow isn't installed (it may have been removed). Its past runs' reports are kept.")); return; }
    const facts = h("ul", { class: "small" },
      h("li", {}, scheduleText(w)),
      w.installed ? h("li", {}, `Approved ${when(w.installed)}`) : null,
      h("li", { class: "muted" }, `Id: ${w.id}`));
    const why = stateText(w);
    const acts = h("div", { class: "row wrap" });
    const run = button("Run now", "primary", () => runNow(w, run), { id: "wf-run-now", title: "Run it once now, as its schedule would" });
    run.disabled = !ready(w);
    acts.append(run);
    if (w.at && state.scheduler) {
      if (w.schedule_entry === "on") acts.append(button("Pause schedule", "", () => setSchedule(false), { title: "Stop it running by itself until you resume it" }));
      else {
        const r = button(w.schedule_entry === "differs" ? "Repair schedule" : "Resume schedule", "", () => setSchedule(true));
        r.disabled = !ready(w);
        acts.append(r);
      }
    }
    const sh = button("Share…", "", () => shareOut("workflow", w.id, err), { id: "wf-share", title: "Save it, with the tools it uses, as a file to send a coworker; they choose their own connections and approve it themselves. Its schedule and runs stay here." });
    sh.disabled = !ready(w);
    acts.append(sh, h("span", { class: "spacer" }), button("Remove", "danger", removeWf, { id: "wf-remove", title: "It stops running; its past runs' reports are kept" }));
    body.append(h("section", { class: "card" }, h("div", { class: "bd stack" },
      why ? el("p", "notice warn", why) : null, facts, acts)));
    const runsBd = h("div", { class: "bd flush" });
    body.append(h("section", { class: "card", id: "wf-runs" }, h("div", { class: "hd" }, h("h2", { id: "wf-runs-h" }, "Runs")), runsBd));
    let runs = [];
    try { runs = await invoke("workflow_runs", { id: w.id }); } catch (e) { runsBd.append(el("p", "pad error-text", String(e))); return; }
    if (!runs.length) { runsBd.append(h("div", { class: "pad" }, emptyState("No runs yet", "Run it now, or wait for its schedule."))); return; }
    $("wf-runs-h").textContent = `Runs (${runs.length}${runs.length >= 100 ? "+" : ""}, newest first)`;
    const ul = h("ul", { class: "list", id: "wf-run-list" });
    let shown = 0;
    const more = button("Show older runs", "sm", () => drawMore(), { id: "wf-more" });
    const drawMore = () => {
      for (const r of runs.slice(shown, shown + RUNS_SHOWN)) {
        const li = el("li", "tool clickable");
        li.addEventListener("click", () => go(wfHash(w.id, r.run)));
        li.append(h("div", { class: "text" },
          h("b", {}, h("a", { href: wfHash(w.id, r.run) }, r.started ? when(r.started) : r.run)),
          h("span", { class: "where" }, `${r.scheduled ? "On its schedule" : "Started by you"}${r.reason ? ` · ${r.reason}` : ""}`)),
          h("div", { class: "acts" }, statusBadge(r.status)));
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
    page.append(pageHead(`${name}: run report`, null));
    page.append(h("div", { class: "scroll" }, body));
    body.append(h("p", { class: "small" }, h("a", { href: wfHash(wid), id: "wf-back" }, `← ${name}`), " · ", h("a", { href: "#/workflows" }, "All workflows")), notice, err);
    const box = h("div", { class: "report", id: "wf-report" });
    body.append(h("section", { class: "card" }, h("div", { class: "bd stack" }, box)));
    try {
      const r = await invoke("workflow_run_report", { id: wid, run: rid });
      const rec = r.record || {};
      box.append(h("p", {}, statusBadge(rec.status), " ", rec.started ? when(rec.started) : rid, rec.scheduled ? " · on its schedule" : " · started by you"));
      const m = h("div", { class: "msg ai" });
      markdown(m, r.report || "(no report)");
      box.append(m);
      if (rec.workspace) box.append(h("p", { class: "small muted wrap-any" }, "This run's files: ", h("code", {}, String(rec.workspace))));
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
      h("p", { class: "small" }, "Once approved it runs by itself, on its schedule, without asking you: read it before you approve it. A change to it, or to a personal tool it uses, turns it off until you approve it again."),
      h("h3", {}, "The workflow file ", h("span", { id: "wf-review-lines", class: "muted small" })), h("div", { id: "wf-review-box" }),
      h("p", { id: "wf-review-error", class: "error-text", role: "alert", hidden: true }),
      h("div", { class: "actions" },
        h("button", { type: "button", id: "wf-review-decline", class: "btn danger" }, "Decline"), h("span", { class: "spacer" }),
        h("button", { type: "button", id: "wf-review-close", class: "btn" }, "Close"),
        h("button", { type: "button", id: "wf-review-install", class: "btn primary" }, "Approve"))));
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
  return () => window.removeEventListener("focus", focusFn);
}
