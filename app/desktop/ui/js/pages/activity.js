// Activity: what waits for the person (requests across sessions, view
// only: requests from other AI apps are decided in that app or on the
// browser page the session opens; and tools and workflows to read before
// they can run), then the history (the audit log, read only, with the chain
// check) and the exports it records.
import { h, el, icon, iconButton, invoke, store, button, redraw, sec, pageHead, timeLeft, ago, clock, dayName, plural, clientName, monthName, toast, size } from "../core.js";
import { openBrowser, waitingTitles, describe, otherInbox } from "./chats.js";

const KIND = { edit: "Edits", script: "Scripts", export: "Exports and sending", download: "Downloads", fetch: "Downloads", notes: "Notes", tool: "Tools", send: "Exports and sending" };

// A decision as the log has it, in plain words: [words, tone].
function decided(dec) {
  return {
    allowed: ["Allowed by rule", ""], denied: ["Refused", "bad"], error: ["Failed", "bad"], approval_required: ["Asked you", "warn"],
    allow: ["You allowed it", ""], allow_once: ["You allowed it", ""], approved: ["You allowed it", ""], always_this_session: ["You allowed it for the session", ""],
    decline: ["You declined", "bad"], declined: ["You declined", "bad"], expired: ["Not answered in time", "warn"], running: ["Running", ""], exporting: ["Exporting", ""],
  }[dec] || [String(dec || "").replace(/_/g, " "), ""];
}

export async function render(main, rest) {
  const sub = rest[0] || "";
  const page = h("div", { class: "page" });
  main.append(page);
  const body = h("div", { class: "pad stack" });
  const scroll = h("div", { class: "scroll" }, body);
  const waiting = h("div", { class: "stack", id: "approvals" });
  body.append(waiting);
  const foot = h("div", { class: "footline" });
  page.append(pageHead("Activity", "What waits for you, then everything your assistants did, as it was written down."), scroll, foot);
  const stopWaiting = approvals(waiting);
  const hist = history(body, foot, sub === "exports" ? "exports" : "all");
  if (sub === "audit" || sub === "exports") setTimeout(() => hist.head.scrollIntoView({ block: "start" }), 0);
  // The history follows what happens: read again when a request comes or
  // is decided (and a moment later, once its line is written), and now and
  // then while the page is open.
  const key = () => JSON.stringify([store.inbox.map((x) => (x.pending || []).map((a) => a.id)), waitingTitles().length]);
  let last = key();
  let later = 0;
  const unsub = store.on((k) => {
    if (k !== "inbox" && k !== "waiting") return;
    const now = key();
    if (now === last) return;
    last = now;
    hist.reload();
    clearTimeout(later);
    later = setTimeout(() => hist.reload(), 2000);
  });
  const tick = setInterval(() => hist.reload(), 30000);
  return () => { stopWaiting(); unsub(); clearInterval(tick); clearTimeout(later); };
}

function approvals(body) {
  // Drawn on every poll; only what changed is redrawn (core.js redraw), so
  // a poll never swaps a button out from under the person's click.
  const draw = () => {
    const out = [];
    const cards = [];
    // The app's own chat's requests are answered in the chat.
    for (const title of waitingTitles()) {
      cards.push(h("section", { class: "group waiting" }, h("div", { class: "hd" }, h("h3", {}, title)),
        h("p", { class: "what" }, "From your chat in this app."),
        h("div", { class: "acts" }, h("a", { class: "btn primary", href: "#/chats/new" }, "Answer in the chat"))));
    }
    const groups = otherInbox();
    let total = 0;
    for (const s of groups) {
      const pending = s.pending || [];
      if (!pending.length && !s.error) continue;
      total += pending.length;
      const sess = store.sessions.find((x) => x.session === s.session);
      const who = clientName(sess && sess.client);
      const g = h("section", { class: pending.length ? "group waiting" : "group", "aria-label": `${who} session` });
      g.append(h("div", { class: "hd" }, h("h3", {}, pending.length ? `${who} is asking: ${plural(pending.length, "request")}` : `${who} session`)));
      if (s.error) g.append(h("p", { class: "what error-text" }, s.error));
      const byKind = new Map();
      for (const a of pending) {
        const k = KIND[a.risk] || a.risk;
        if (!byKind.has(k)) byKind.set(k, []);
        byKind.get(k).push(a);
      }
      for (const [k, items] of byKind) {
        g.append(h("div", { class: "kindhead" }, `${k} · ${items.length}`));
        const ul = h("ul", { class: "items" });
        for (const a of items) {
          ul.append(h("li", {}, h("div", { class: "text" }, h("div", { class: "wrap-any" }, a.summary),
            h("span", { class: "tiny muted" }, `${a.tool}${a.lines ? ` · ${a.lines} lines of code` : ""} · asked ${ago(a.created)} · ${timeLeft(a.expires)}`))));
        }
        g.append(ul);
      }
      g.append(h("div", { class: "acts" },
        pending.length ? button("Decide in browser", "", () => openBrowser(s.session), { "data-session": s.session }) : null,
        h("a", { class: "btn ghost", href: `#/chats/s/${encodeURIComponent(s.session)}` }, "Open session")));
      if (pending.length) g.append(h("p", { class: "note" }, "Requests from other apps are decided in that app or in your browser, not here."));
      cards.push(g);
    }
    // Things to read before they can run: proposed by an assistant, or
    // opened from a shared file. Each is read and approved on its own page.
    const reviews = [];
    for (const w of (store.workflows ? store.workflows.waiting : [])) reviews.push(["workflow", w.name, "#/workflows", w.id]);
    for (const t of (store.tools ? store.tools.waiting : [])) reviews.push(["tool", t.title, "#/tools", t.id]);
    for (const [kind, name, href, id] of reviews) {
      cards.push(h("section", { class: "group", "data-id": `${kind}:${id}` }, h("div", { class: "hd" }, h("h3", {}, `Read a ${kind} before it can run`)),
        h("p", { class: "what" }, String(name)), h("div", { class: "acts" }, h("a", { class: "btn", href }, "Review"))));
    }
    const n = waitingTitles().length + total + reviews.length;
    out.push(sec("Waiting for you", n || null));
    if (cards.length) out.push(...cards);
    else out.push(h("p", { class: "muted small quiet" }, "Nothing is waiting. Requests appear here as your assistants make them; each expires after 10 minutes if no one decides."));
    redraw(body, out);
  };
  draw();
  if (store.refresh) store.refresh.inbox();
  const unsub = store.on((k) => { if (k === "inbox" || k === "waiting" || k === "sessions" || k === "tools" || k === "workflows") draw(); });
  const tick = setInterval(draw, 15000);
  return () => { unsub(); clearInterval(tick); };
}

// The audit log, newest first, and the exports it records. `view` is "all"
// or "exports" to start with.
function history(body, foot, view) {
  const q = { month: "", client: "", tool: "", decision: "", text: "", offset: 0, limit: 100 };
  const month = h("select", { id: "audit-month", "aria-label": "Month" });
  const client = h("select", { id: "audit-client", "aria-label": "App" });
  const tool = h("select", { id: "audit-tool", "aria-label": "Tool" });
  const decision = h("select", { id: "audit-decision", "aria-label": "Decision" });
  const text = h("input", { type: "search", id: "audit-text", placeholder: "Search lines", "aria-label": "Search lines" });
  const filters = h("div", { class: "filters", id: "audit-filters", hidden: true },
    h("label", {}, "Month", month), h("label", {}, "App", client), h("label", {}, "Tool", tool), h("label", {}, "Decision", decision), h("label", { class: "wide" }, "Text", text));
  const seg = h("div", { class: "seg", role: "group", "aria-label": "Show" });
  const VIEWS = [["all", "All"], ["decisions", "Decisions"], ["refused", "Refused"], ["exports", "Exports"]];
  for (const [k, t] of VIEWS) seg.append(h("button", { type: "button", "data-view": k, onclick: () => setView(k) }, t));
  const filterBtn = iconButton("filter", "Filter by month, app, tool, decision or text", () => {
    filters.hidden = !filters.hidden;
    filterBtn.setAttribute("aria-pressed", String(!filters.hidden));
  }, { id: "audit-filter", "aria-pressed": "false" });
  const head = h("div", { class: "sec histhead" }, h("h2", {}, "History"), h("span", { class: "more row" }, seg, filterBtn));
  const result = h("p", { id: "audit-verify-result", role: "status", hidden: true });
  const rows = h("ul", { id: "audit-rows", class: "auditlist" });
  const count = h("span", { id: "audit-count", class: "small muted" });
  const prev = button("Newer", "sm", () => { q.offset = Math.max(0, q.offset - q.limit); load(); });
  const next = button("Older", "sm", () => { q.offset += q.limit; load(); });
  const pager = h("div", { class: "pager" }, count, prev, next);
  const where = h("p", { class: "small muted wrap-any" });
  const skippedNote = h("p", { class: "notice warn", role: "status", hidden: true });
  const exportsBox = h("div", { class: "stack", id: "exports", hidden: true });
  const log = h("div", { class: "stack", id: "audit" }, filters, result, skippedNote, rows, pager, where);
  body.append(h("section", { class: "card" }, head, log, exportsBox));
  const verifyBtn = button("Verify this month", "sm", () => verify(), { id: "audit-verify", title: "Each line carries a hash of the one before it, so a removed, reordered or changed line shows. Verify checks every line of the month." });
  const footText = h("span", { class: "grow" }, "");
  foot.replaceChildren(icon("shield"), footText, verifyBtn);
  let total = 0;

  const opt = (sel, values, cur, all) => {
    sel.replaceChildren(h("option", { value: "" }, all), ...values.map((v) => { const o = h("option", { value: v }, sel === client ? `${clientName(v)}${clientName(v) !== v ? ` (${v})` : ""}` : v); o.selected = v === cur; return o; }));
  };
  const current = () => (view === "exports" ? "exports" : q.tool === "approval_decision" && !q.decision ? "decisions" : q.decision === "denied" && !q.tool ? "refused" : !q.tool && !q.decision ? "all" : "");
  const drawSeg = () => { for (const b of seg.children) b.setAttribute("aria-pressed", String(b.dataset.view === current())); };
  const setView = (k) => {
    view = k === "exports" ? "exports" : "all";
    exportsBox.hidden = view !== "exports";
    log.hidden = view === "exports";
    filterBtn.hidden = view === "exports";
    if (view === "exports") { drawSeg(); loadExports(); return; }
    q.tool = k === "decisions" ? "approval_decision" : "";
    q.decision = k === "refused" ? "denied" : "";
    q.offset = 0;
    load();
  };
  const load = async () => {
    drawSeg();
    try {
      const r = await invoke("audit_page", { query: { ...q, month: q.month || null } });
      q.month = r.month || "";
      month.replaceChildren(...(r.months.length ? r.months : [{ month: "" }]).map((m) => { const o = h("option", { value: m.month }, m.month ? monthName(m.month) : "No log yet"); o.selected = m.month === q.month; return o; }));
      opt(client, r.clients, q.client, "All apps");
      opt(tool, r.tools, q.tool, "All tools");
      opt(decision, r.decisions, q.decision, "All decisions");
      rows.replaceChildren();
      let day = null;
      for (const l of r.lines) {
        const d = dayName(l.ts);
        if (d !== day) { day = d; rows.append(h("li", { class: "day" }, d)); }
        const what = [l.connection && l.dataset ? `${l.connection}:${l.dataset}` : l.connection || l.dataset, l.rows != null ? `${Number(l.rows).toLocaleString()} rows` : null, l.reason].filter(Boolean).join(" · ");
        const dec = String(l.decision || "");
        const [words, tone] = decided(dec);
        // In plain words first; then the line as the log has it.
        rows.append(h("li", {},
          h("span", { class: "t" }, clock(l.ts)),
          h("span", { class: "w" }, describe(String(l.tool || ""), { dataset: l.dataset, connection: l.connection })),
          h("span", { class: tone ? `d ${tone}` : "d", title: `decision: ${dec}` }, words),
          h("span", { class: "s", title: `${l.tool || ""} · ${clientName(l.client)}${what ? ` · ${what}` : ""} · line ${l.line}` },
            h("span", { class: "mono" }, String(l.tool || "")), ` · ${clientName(l.client)}${what ? ` · ${what}` : ""} · line ${l.line}`)));
      }
      if (!r.lines.length) rows.append(h("li", { class: "none" }, h("span", { class: "s muted" }, r.total ? "No line matches." : "Nothing yet. Every call your assistants make through Columbus, and every decision, is written here.")));
      const from = r.matching ? r.offset + 1 : 0;
      count.textContent = `${from.toLocaleString()}–${(r.offset + r.lines.length).toLocaleString()} of ${r.matching.toLocaleString()}${r.matching !== r.total ? ` matching (${r.total.toLocaleString()} lines)` : ""}, newest first`;
      prev.disabled = r.offset === 0;
      next.disabled = r.offset + r.lines.length >= r.matching;
      pager.hidden = r.matching <= r.lines.length && r.offset === 0;
      where.textContent = `Kept in ${r.dir}, on this computer. Lines record a hash of each request, never the rows.${r.skipped_lines ? ` This month is too large to read whole here: its first ${Number(r.skipped_lines).toLocaleString()} lines (${size(r.skipped_bytes)}) are not shown. Verify still checks every line.` : ""}`;
      skippedNote.hidden = !r.skipped_lines;
      skippedNote.textContent = r.skipped_lines ? `Showing the newest lines of a large month: the first ${Number(r.skipped_lines).toLocaleString()} lines (${size(r.skipped_bytes)}) are not read here.` : "";
      total = r.total;
      footText.textContent = q.month ? `${plural(r.total, "line")} in ${monthName(q.month)}.` : "Nothing written yet.";
      drawSeg();
    } catch (e) {
      rows.replaceChildren(h("li", { class: "none" }, h("span", { class: "s error-text" }, String(e))));
    }
  };
  const verify = async () => {
    if (!q.month) return toast("There is no month to check.");
    result.hidden = false;
    result.className = "notice";
    result.textContent = "Checking…";
    if (view === "exports") setView("all");
    try {
      const r = await invoke("audit_verify", { month: q.month });
      result.className = r.ok ? "notice ok" : "notice fail";
      result.textContent = r.ok ? `The chain of ${monthName(q.month)} holds: ${Number(r.lines).toLocaleString()} lines, none removed, reordered or changed.`
        : `The chain of ${monthName(q.month)} is broken${r.line ? ` at line ${r.line}` : ""}: ${r.message}. Tell your IT team.`;
      footText.textContent = r.ok ? `${plural(Number(r.lines), "line")} in ${monthName(q.month)}. None altered.` : `${plural(total, "line")} in ${monthName(q.month)}. The chain is broken.`;
    } catch (e) { result.className = "notice fail"; result.textContent = String(e); }
  };
  let exportsLoaded = false;
  const loadExports = async () => {
    if (exportsLoaded) return;
    exportsLoaded = true;
    let r;
    try { r = await invoke("exports_list"); } catch (e) { exportsBox.replaceChildren(el("p", "error-text", String(e))); exportsLoaded = false; return; }
    if (!r.length) {
      exportsBox.replaceChildren(h("p", { class: "muted small quiet" }, "No exports yet. When your assistant exports a file to one of your organization's export folders (after the checks pass, and your approval where needed), it is listed here."));
      return;
    }
    const list = h("ul", { class: "auditlist", id: "export-rows" });
    let day = null;
    for (const l of r) {
      const d = dayName(l.ts);
      if (d !== day) { day = d; list.append(h("li", { class: "day" }, d)); }
      list.append(h("li", {},
        h("span", { class: "t" }, clock(l.ts)),
        h("span", { class: "w" }, [l.dataset, l.reason].filter(Boolean).join(" · ") || describe("export_output", {})),
        h("span", { class: "d" }, clientName(l.client)),
        h("span", { class: "s" }, l.prov ? h("span", { class: "mono" }, l.prov) : null, l.prov && (l.labels || []).length ? " · data from " : null, ...(l.labels || []).map((x) => el("span", "chip label", x)))));
    }
    exportsBox.replaceChildren(list,
      h("p", { class: "small muted" }, "Checks and the full provenance of each file are in its session's files while the session runs, and in the provenance file written beside the export."));
  };
  const reset = (k) => (ev) => { q[k] = ev.target.value; q.offset = 0; load(); };
  month.addEventListener("change", (ev) => { q.month = ev.target.value; q.offset = 0; q.client = q.tool = q.decision = ""; result.hidden = true; load(); });
  client.addEventListener("change", reset("client"));
  tool.addEventListener("change", reset("tool"));
  decision.addEventListener("change", reset("decision"));
  let t = 0;
  text.addEventListener("input", (ev) => { clearTimeout(t); t = setTimeout(() => reset("text")(ev), 250); });
  // The month is read even when Exports shows first, for the foot's count.
  load();
  if (view === "exports") setView("exports");
  // Newer lines, when the person is looking at the newest page.
  const reload = () => {
    // Not under the person's hands: a choice being made in a filter stays.
    if (q.offset !== 0 || !body.isConnected || filters.contains(document.activeElement)) return;
    if (view === "exports") { exportsLoaded = false; loadExports(); } else load();
  };
  return { head, reload };
}
