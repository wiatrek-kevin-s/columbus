// Workflows: jobs your assistant proposed (propose_workflow), each read in
// plain words and in full before you approve it, which is confirmed at the
// keyboard in the app's own code; their schedules; Run now; and every run's
// report. Your workflows are yours alone: nothing here is shared.
import { $, h, el, invoke, store, button, pageHead, showError, when, numbered, emptyState } from "../core.js";
import { switchRow } from "./settings.js";
import { markdown } from "./chats.js";

let state = null;
let reviewing = null;

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

export async function render(main, rest) {
  const page = h("div", { class: "page" });
  main.append(page);
  page.append(pageHead("Workflows", "Jobs that run by themselves on a schedule, without you waiting: your assistant proposes one, you read it and approve it here. They are yours alone."));
  const body = h("div", { class: "pad stack" });
  page.append(h("div", { class: "scroll" }, body));
  const notice = h("p", { id: "wf-notice", class: "notice ok", role: "status", hidden: true });
  const err = h("p", { id: "wf-error", class: "error-text", role: "alert", hidden: true });
  const off = h("p", { id: "wf-off", class: "notice warn", role: "status", hidden: true }, "Workflows are turned off in your organization's settings, so none run, and none can be approved.");
  const noSched = h("p", { id: "wf-nosched", class: "notice", role: "status", hidden: true }, "Schedules are not available on this system yet. Run now works.");
  const waiting = h("ul", { id: "wf-waiting", class: "list", "aria-live": "polite" });
  const installed = h("ul", { id: "wf-installed", class: "list", "aria-live": "polite" });
  const runsBox = h("section", { class: "card", id: "wf-runs", hidden: true });
  const sw = h("div", {});
  body.append(notice, err, off, noSched,
    h("p", { class: "small muted" }, "A workflow reads only the connections it lists, runs only the steps it lists (reading and processing tools, and personal tools you installed), and never waits for you: a step that needs an approval stops the run with \"needs you\". Runs happen while you are signed in to this computer; one missed while it slept runs once when it wakes. Ask your assistant, for example: ", h("i", {}, "make what we just did a workflow that runs every weekday at 6"), "."),
    h("section", { class: "card" }, h("div", { class: "hd" }, h("h2", {}, "Waiting for you")), h("div", { class: "bd flush" }, waiting, h("p", { id: "wf-no-waiting", class: "pad muted small", hidden: true }, "Nothing is waiting."))),
    h("section", { class: "card" }, h("div", { class: "hd" }, h("h2", {}, "Your workflows")), h("div", { class: "bd flush" }, installed, h("p", { id: "wf-no-installed", class: "pad muted small", hidden: true }, "No workflows yet."))),
    runsBox,
    h("section", { class: "card" }, h("div", { class: "hd" }, h("h2", {}, "Your organization's switch")), h("div", { class: "bd" }, sw)),
    h("p", { class: "small muted wrap-any" }, "Stored in ", h("code", { id: "wf-dir" })),
    h("p", { class: "small muted" }, "Sharing a workflow with your team, and an AI review step in a run, come later."));

  const draw = () => {
    $("wf-dir").textContent = state.dir;
    off.hidden = state.allowed;
    noSched.hidden = !!state.scheduler;
    waiting.replaceChildren();
    $("wf-no-waiting").hidden = state.waiting.length > 0;
    for (const w of state.waiting) {
      const li = el("li", w.problem ? "tool bad" : "tool");
      const text = el("div", "text");
      text.append(el("b", null, w.name), el("span", "where", `${w.id}${w.proposal && w.proposal.proposed ? ` · proposed ${when(w.proposal.proposed)}` : ""}`));
      if (w.purpose) text.append(el("span", null, w.purpose));
      text.append(el("span", "where", w.at ? `Would run ${scheduleText({ ...w, schedule_entry: "none" })}` : "Would run when you start it"));
      if (w.problem) text.append(el("span", "where error-text", w.problem));
      const b = button("Review", "primary sm", () => openReview(w), { title: "Read what it does, then approve or decline it" });
      b.disabled = !!w.problem;
      li.append(text, h("div", { class: "acts" }, b, button("Decline", "danger sm", () => decline(w.id))));
      waiting.append(li);
    }
    installed.replaceChildren();
    $("wf-no-installed").hidden = state.installed.length > 0;
    for (const w of state.installed) {
      const li = el("li", "tool");
      li.dataset.id = w.id;
      const text = el("div", "text");
      text.append(el("b", null, w.name), el("span", "where", `${w.id} · ${scheduleText(w)}${w.installed ? ` · approved ${when(w.installed)}` : ""}`));
      if (w.purpose) text.append(el("span", null, w.purpose));
      const chips = h("div", { class: "chips" });
      const why = stateText(w);
      if (why) chips.append(el("span", "chip off", why));
      if (w.last_run) chips.append(el("span", "where", "Last run: "), statusBadge(w.last_run.status), el("span", "where", w.last_run.started ? ` ${when(w.last_run.started)}` : ""));
      else chips.append(el("span", "where", "Not run yet"));
      text.append(chips);
      const acts = h("div", { class: "acts" });
      const ready = w.state === "ready" && state.allowed;
      const run = button("Run now", "primary sm", () => runNow(w, run), { title: "Run it once now, as its schedule would" });
      run.disabled = !ready;
      acts.append(run);
      if (w.at && state.scheduler) {
        if (w.schedule_entry === "on") acts.append(button("Pause", "sm", () => setSchedule(w.id, false), { title: "Stop it running by itself until you resume it" }));
        else {
          const r = button(w.schedule_entry === "differs" ? "Repair" : "Resume", "sm", () => setSchedule(w.id, true));
          r.disabled = !ready;
          acts.append(r);
        }
      }
      acts.append(button("Runs", "sm", () => showRuns(w)), button("Remove", "danger sm", () => removeWf(w)));
      li.append(text, acts);
      installed.append(li);
    }
  };
  const refresh = async () => {
    try {
      state = await invoke("workflows_state");
      store.set("workflows", state);
      draw();
      showError(err, null);
    } catch (e) { showError(err, e); }
  };
  const drawSwitch = async () => {
    try {
      const st = await invoke("settings_state");
      sw.replaceChildren(switchRow("allow_workflows", st, async () => { await drawSwitch(); await refresh(); }));
    } catch (e) { sw.replaceChildren(el("p", "error-text", String(e))); }
  };
  const say = (t) => { notice.textContent = t; notice.hidden = false; };
  async function decline(id) {
    try {
      await invoke("workflow_decline", { id });
      const r = $("wf-review");
      if (r && r.open) r.close();
      await refresh();
    } catch (e) { showError($("wf-review")?.open ? $("wf-review-error") : err, e); }
  }
  async function removeWf(w) {
    try {
      await invoke("workflow_remove", { id: w.id });
      say(`Removed ${w.name}: it no longer runs. Its past runs' reports are kept.`);
      await refresh();
    } catch (e) { showError(err, e); }
  }
  async function setSchedule(id, on) {
    try {
      await invoke("workflow_set_schedule", { id, on });
      await refresh();
    } catch (e) { showError(err, e); }
  }
  async function runNow(w, btn) {
    btn.disabled = true;
    const was = btn.textContent;
    btn.textContent = "Running…";
    showError(err, null);
    try {
      const r = await invoke("workflow_run_now", { id: w.id });
      say(`${w.name}: ${(STATUS[r.status] || [r.status])[0]}.`);
      await refresh();
      await showRuns(w, r.run);
    } catch (e) {
      showError(err, e);
    } finally {
      btn.textContent = was;
      btn.disabled = false;
    }
  }
  async function showRuns(w, open) {
    runsBox.hidden = false;
    runsBox.replaceChildren(h("div", { class: "hd" }, h("h2", {}, `Runs of ${w.name}`)));
    const bd = h("div", { class: "bd stack" });
    runsBox.append(bd);
    let runs = [];
    try { runs = await invoke("workflow_runs", { id: w.id }); } catch (e) { bd.append(el("p", "error-text", String(e))); return; }
    if (!runs.length) { bd.append(emptyState("No runs yet", "Run it now, or wait for its schedule.")); return; }
    const ul = h("ul", { class: "list", id: "wf-run-list" });
    const report = h("div", { class: "report", id: "wf-report" });
    for (const r of runs) {
      const li = h("li", { class: "tool" }, h("div", { class: "text" },
        h("b", {}, r.started ? when(r.started) : r.run),
        h("span", { class: "where" }, `${r.scheduled ? "On its schedule" : "Started by you"}${r.reason ? ` · ${r.reason}` : ""}`)),
        h("div", { class: "acts" }, statusBadge(r.status), button("Report", "sm", () => openReport(w.id, r.run, report))));
      ul.append(li);
    }
    bd.append(ul, report);
    await openReport(w.id, open || runs[0].run, report);
    runsBox.scrollIntoView({ block: "start" });
  }
  async function openReport(id, run, box) {
    box.replaceChildren();
    try {
      const r = await invoke("workflow_run_report", { id, run });
      const m = h("div", { class: "msg ai" });
      markdown(m, r.report || "(no report)");
      box.append(m);
      if (r.record && r.record.workspace) box.append(h("p", { class: "small muted wrap-any" }, "This run's files: ", h("code", {}, String(r.record.workspace))));
    } catch (e) { box.append(el("p", "error-text", String(e))); }
  }
  function reviewDialog() {
    let d = $("wf-review");
    if (d) return d;
    d = h("dialog", { id: "wf-review", "aria-labelledby": "wf-review-h" }, h("div", { class: "dlg" },
      h("h2", { id: "wf-review-h" }, "Workflow"),
      h("p", { id: "wf-review-id", class: "muted small wrap-any" }),
      h("p", { id: "wf-review-purpose" }),
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
  function openReview(w) {
    const d = reviewDialog();
    reviewing = w;
    $("wf-review-h").textContent = w.name;
    $("wf-review-id").textContent = `${w.id} · ${w.fingerprint}`;
    $("wf-review-purpose").textContent = w.purpose || "";
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
    inst.onclick = install;
    $("wf-review-decline").onclick = () => reviewing && decline(reviewing.id);
    d.showModal();
    $("wf-review-file").focus();
  }
  async function install() {
    if (!reviewing) return;
    const btn = $("wf-review-install");
    btn.disabled = true;
    showError($("wf-review-error"), null);
    try {
      await invoke("workflow_install", { id: reviewing.id, fingerprint: reviewing.fingerprint });
      $("wf-review").close();
      await refresh();
      say(`Approved ${reviewing.name}. ${reviewing.at ? "It runs on its schedule from now on; " : ""}Run now runs it once and shows its report.`);
    } catch (e) {
      showError($("wf-review-error"), e);
    } finally {
      btn.disabled = !state || !state.allowed;
    }
  }
  await Promise.all([refresh(), drawSwitch()]);
  if (rest && rest[0]) {
    const w = state && state.installed.find((x) => x.id === rest[0]);
    if (w) await showRuns(w);
  }
  const onFocus = () => refresh();
  window.addEventListener("focus", onFocus);
  return () => window.removeEventListener("focus", onFocus);
}
