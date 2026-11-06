// Activity: the requests waiting for the person across sessions (view
// only: requests from other AI apps are decided in that app or on the
// browser page the session opens), the audit log (read only, with the
// chain check) and the exports it records.
import { h, el, invoke, store, button, pageHead, timeLeft, when, ago, plural, clientName, monthName, emptyState, toast } from "../core.js";
import { openBrowser, waitingCount, describe, otherInbox } from "./chats.js";

export async function render(main, rest) {
  const sub = rest[0] || "approvals";
  const page = h("div", { class: "page" });
  main.append(page);
  if (sub === "audit") return audit(page);
  if (sub === "exports") return exportsPage(page);
  return approvals(page);
}

const KIND = { edit: "Edits", script: "Scripts", export: "Exports and sending", download: "Downloads", fetch: "Downloads", notes: "Notes", tool: "Tools", send: "Exports and sending" };

function approvals(page) {
  page.append(pageHead("Approvals", "Requests from your assistants that wait for your decision, from every session on this computer.",
    [button("Refresh", "", () => store.refresh && store.refresh.inbox())]));
  const body = h("div", { class: "pad stack", id: "approvals" });
  page.append(h("div", { class: "scroll" }, body));
  const draw = () => {
    body.replaceChildren(h("div", { class: "notice" }, h("div", {},
      "Requests from this app's own chat are answered in the chat. Requests from other AI apps are decided in that app's own prompt, or on the browser page Columbus opens: ",
      h("b", {}, "Decide in browser"), " opens it for that session. This app only shows them; deciding them here comes after a security review.")));
    const own = waitingCount();
    if (own) {
      body.append(h("section", { class: "group" }, h("div", { class: "hd" }, h("span", { class: "dot run" }), h("h3", {}, "Your chat in this app"), h("span", { class: "badge info" }, plural(own, "request")),
        h("a", { class: "btn sm primary right", href: "#/chats/new" }, "Answer in the chat"))));
    }
    // The app's own chat's requests are answered in the chat (above).
    const groups = otherInbox();
    let total = 0;
    for (const s of groups) {
      const pending = s.pending || [];
      if (!pending.length && !s.error) continue;
      total += pending.length;
      const sess = store.sessions.find((x) => x.session === s.session);
      const who = clientName(sess && sess.client);
      const g = h("section", { class: "group", "aria-label": `${who} session` });
      g.append(h("div", { class: "hd" }, h("h3", {}, `${who} session`), h("span", { class: "muted small" }, `started ${when(s.started)}${s.last_used ? ` · used ${ago(s.last_used)}` : ""}`),
        pending.length ? h("span", { class: "badge warn" }, plural(pending.length, "request")) : null,
        h("span", { class: "right row" }, h("a", { class: "btn sm", href: `#/chats/s/${encodeURIComponent(s.session)}` }, "Open session"),
          pending.length ? button("Decide in browser", "sm primary", () => openBrowser(s.session)) : null)));
      if (s.error) g.append(h("p", { class: "pad small error-text" }, s.error));
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
          ul.append(h("li", {}, h("span", { class: `risk ${a.risk}` }, a.risk),
            h("div", { class: "text" }, h("div", { class: "wrap-any" }, a.summary), h("span", { class: "tiny muted" }, `${a.tool}${a.lines ? ` · ${a.lines} lines of code` : ""} · asked ${ago(a.created)}`)),
            h("span", { class: "tiny muted nowrap" }, timeLeft(a.expires))));
        }
        g.append(ul);
      }
      body.append(g);
    }
    if (!total && !own) {
      body.append(emptyState("Nothing is waiting", "Requests appear here as your assistants make them. Each one expires after 10 minutes if no one decides; your assistant is then told."));
    }
    body.append(h("p", { class: "small muted" }, "Approvals are bound to the exact request, are used once, and expire after 10 minutes; your assistant never approves its own request."));
  };
  draw();
  if (store.refresh) store.refresh.inbox();
  const unsub = store.on((k) => { if (k === "inbox" || k === "waiting" || k === "sessions") draw(); });
  const tick = setInterval(draw, 15000);
  return () => { unsub(); clearInterval(tick); };
}

function audit(page) {
  page.append(pageHead("Audit log", "Every call your assistants made through Columbus, and every decision: one hash-chained line each, kept on this computer. Read only.",
    [button("Verify this month", "", () => verify(), { id: "audit-verify" })]));
  const q = { month: "", client: "", tool: "", decision: "", text: "", offset: 0, limit: 100 };
  const month = h("select", { id: "audit-month", "aria-label": "Month" });
  const client = h("select", { id: "audit-client", "aria-label": "App" });
  const tool = h("select", { id: "audit-tool", "aria-label": "Tool" });
  const decision = h("select", { id: "audit-decision", "aria-label": "Decision" });
  const text = h("input", { type: "search", id: "audit-text", placeholder: "Search lines", "aria-label": "Search lines" });
  const filters = h("div", { class: "filters" },
    h("label", {}, "Month", month), h("label", {}, "App", client), h("label", {}, "Tool", tool), h("label", {}, "Decision", decision), h("label", { class: "grow" }, "Text", text));
  const result = h("p", { id: "audit-verify-result", role: "status", hidden: true });
  const tbody = h("tbody", { id: "audit-rows" });
  const count = h("span", { id: "audit-count", class: "small muted" });
  const prev = button("Newer", "sm", () => { q.offset = Math.max(0, q.offset - q.limit); load(); });
  const next = button("Older", "sm", () => { q.offset += q.limit; load(); });
  const where = h("p", { class: "small muted wrap-any" });
  const body = h("div", { class: "pad stack" }, filters, result,
    h("section", { class: "card" }, h("table", { class: "t" }, h("thead", {}, h("tr", {}, h("th", {}, "Time"), h("th", {}, "App"), h("th", {}, "Tool"), h("th", {}, "What"), h("th", {}, "Decision"), h("th", {}, "Line"))), tbody)),
    h("div", { class: "pager" }, count, prev, next), where);
  page.append(h("div", { class: "scroll" }, body));
  const opt = (sel, values, cur, all) => {
    sel.replaceChildren(h("option", { value: "" }, all), ...values.map((v) => { const o = h("option", { value: v }, sel === client ? `${clientName(v)}${clientName(v) !== v ? ` (${v})` : ""}` : v); o.selected = v === cur; return o; }));
  };
  const load = async () => {
    try {
      const r = await invoke("audit_page", { query: { ...q, month: q.month || null } });
      q.month = r.month || "";
      month.replaceChildren(...(r.months.length ? r.months : [{ month: "" }]).map((m) => { const o = h("option", { value: m.month }, m.month ? monthName(m.month) : "No log yet"); o.selected = m.month === q.month; return o; }));
      opt(client, r.clients, q.client, "All apps");
      opt(tool, r.tools, q.tool, "All tools");
      opt(decision, r.decisions, q.decision, "All decisions");
      tbody.replaceChildren();
      for (const l of r.lines) {
        const what = [l.connection && l.dataset ? `${l.connection}:${l.dataset}` : l.connection || l.dataset, l.rows != null ? `${Number(l.rows).toLocaleString()} rows` : null, l.reason].filter(Boolean).join(" · ");
        const dec = String(l.decision || "");
        tbody.append(h("tr", {},
          h("td", { class: "nowrap" }, when(l.ts)),
          h("td", { class: "nowrap" }, clientName(l.client)),
          h("td", { class: "code nowrap" }, String(l.tool || "")),
          h("td", { class: "wrap-any" }, what),
          h("td", {}, el("span", `badge ${dec === "allowed" || dec === "approved" ? "pass" : dec === "denied" || dec === "error" || dec === "declined" ? "fail" : "warn"}`, dec.replace(/_/g, " "))),
          h("td", { class: "num muted" }, String(l.line))));
      }
      if (!r.lines.length) tbody.append(h("tr", {}, h("td", { colspan: "6", class: "muted" }, r.total ? "No line matches." : "The audit log has no lines yet.")));
      const from = r.matching ? r.offset + 1 : 0;
      count.textContent = `${from.toLocaleString()}–${(r.offset + r.lines.length).toLocaleString()} of ${r.matching.toLocaleString()}${r.matching !== r.total ? ` matching (${r.total.toLocaleString()} lines)` : ""}, newest first`;
      prev.disabled = r.offset === 0;
      next.disabled = r.offset + r.lines.length >= r.matching;
      where.textContent = `In ${r.dir}. Lines record a hash of each request, never the rows.`;
    } catch (e) {
      tbody.replaceChildren(h("tr", {}, h("td", { colspan: "6", class: "error-text" }, String(e))));
    }
  };
  const verify = async () => {
    if (!q.month) return toast("There is no month to check.");
    result.hidden = false;
    result.className = "notice";
    result.textContent = "Checking…";
    try {
      const r = await invoke("audit_verify", { month: q.month });
      result.className = r.ok ? "notice ok" : "notice fail";
      result.textContent = r.ok ? `The chain of ${monthName(q.month)} holds: ${Number(r.lines).toLocaleString()} lines, none removed, reordered or changed.`
        : `The chain of ${monthName(q.month)} is broken${r.line ? ` at line ${r.line}` : ""}: ${r.message}. Tell your IT team.`;
    } catch (e) { result.className = "notice fail"; result.textContent = String(e); }
  };
  const reset = (k) => (ev) => { q[k] = ev.target.value; q.offset = 0; load(); };
  month.addEventListener("change", (ev) => { q.month = ev.target.value; q.offset = 0; q.client = q.tool = q.decision = ""; result.hidden = true; load(); });
  client.addEventListener("change", reset("client"));
  tool.addEventListener("change", reset("tool"));
  decision.addEventListener("change", reset("decision"));
  let t = 0;
  text.addEventListener("input", (ev) => { clearTimeout(t); t = setTimeout(() => reset("text")(ev), 250); });
  load();
}

async function exportsPage(page) {
  page.append(pageHead("Exports", "Every file your assistants exported, from the audit log: when, from which app, and its provenance record."));
  const body = h("div", { class: "pad stack" });
  page.append(h("div", { class: "scroll" }, body));
  let r;
  try { r = await invoke("exports_list"); } catch (e) { body.append(el("p", "error-text", String(e))); return; }
  if (!r.length) {
    body.append(emptyState("No exports yet", "When your assistant exports a file to one of your organization's export folders (after the checks pass, and your approval where needed), it is listed here."));
    return;
  }
  const tbody = h("tbody");
  for (const l of r) {
    tbody.append(h("tr", {}, h("td", { class: "nowrap" }, when(l.ts)), h("td", {}, clientName(l.client)),
      h("td", { class: "wrap-any" }, [l.dataset, l.reason].filter(Boolean).join(" · ") || describe("export_output", {})),
      h("td", { class: "code" }, l.prov || ""), h("td", {}, (l.labels || []).map((x) => el("span", "chip label", x)))));
  }
  body.append(h("section", { class: "card" }, h("table", { class: "t" }, h("thead", {}, h("tr", {}, h("th", {}, "Time"), h("th", {}, "App"), h("th", {}, "What"), h("th", {}, "Provenance"), h("th", {}, "Data from"))), tbody)),
    h("p", { class: "small muted" }, "Checks and the full provenance of each file are in its session's Files tab while the session runs, and in the provenance file written beside the export."));
}
