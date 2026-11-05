import { useState } from "react";
import type { Approval } from "./api";

const RISK: Record<string, string> = {
  export: "Export",
  download: "Download",
  script: "Script",
  edit: "Edit",
  long_job: "Long job",
  policy: "Policy",
};

function shown(v: unknown): string {
  const t = typeof v === "string" ? v : JSON.stringify(v);
  return t.length > 200 ? `${t.slice(0, 200)}…` : t;
}

function time(iso: string): string {
  const d = new Date(iso);
  return isNaN(+d) ? iso : d.toLocaleTimeString([], { hour: "numeric", minute: "2-digit" });
}

/** One request waiting for the person. Everything from the request is
 * rendered as text, never as markup. */
export function ApprovalCard(props: {
  a: Approval;
  busy: boolean;
  onDecide: (d: "allow_once" | "always_this_session" | "decline") => void;
}) {
  const { a } = props;
  const d = a.detail || {};
  const once = a.risk === "script";
  const [open, setOpen] = useState(false);
  const rows: [string, string][] = [];
  if (d.destination) rows.push(["Destination", d.destination]);
  if (Array.isArray(d.files))
    rows.push(["Files", d.files.map((f) => (f.rows != null ? `${f.name} (${f.rows} rows)` : f.name)).join(", ")]);
  if (Array.isArray(d.labels) && d.labels.length) rows.push(["Data from", d.labels.join(", ")]);
  if (d.flow_rule) rows.push(["Flow rule", d.flow_rule]);
  if (d.arguments && typeof d.arguments === "object")
    for (const [k, v] of Object.entries(d.arguments)) rows.push([k, shown(v)]);

  return (
    <article className={`approval risk-${a.risk}`} aria-label={`Approval needed: ${a.summary}`}>
      <div className="approval-band" aria-hidden="true">
        {RISK[a.risk] || a.risk}
      </div>
      <div className="approval-body">
        <p className="approval-summary">{a.summary}</p>
        <p className="approval-meta">
          Asked by {a.tool}. Waits for you until {time(a.expires)}.
        </p>
        {rows.length > 0 && (
          <dl className="approval-facts">
            {rows.map(([k, v]) => (
              <div key={k}>
                <dt>{k}</dt>
                <dd>{v}</dd>
              </div>
            ))}
          </dl>
        )}
        {typeof d.code === "string" && (
          <>
            <p className="approval-sub">The code that will run{d.lines ? ` (${d.lines} lines)` : ""}:</p>
            <pre className="code">
              {d.code.split("\n").map((line, i) => (
                <span key={i} className="ln">
                  {line === "" ? " " : line}
                </span>
              ))}
            </pre>
          </>
        )}
        <button type="button" className="link" onClick={() => setOpen(!open)} aria-expanded={open}>
          {open ? "Hide the exact request" : "Show the exact request"}
        </button>
        {open && <pre className="request">{JSON.stringify(a.request, null, 2)}</pre>}
        <div className="approval-actions">
          <button type="button" className="primary" disabled={props.busy} onClick={() => props.onDecide("allow_once")}>
            Allow once
          </button>
          {!once && (
            <button type="button" disabled={props.busy} onClick={() => props.onDecide("always_this_session")}>
              Allow for this session
            </button>
          )}
          <button type="button" className="danger" disabled={props.busy} onClick={() => props.onDecide("decline")}>
            Decline
          </button>
        </div>
        <p className="hint">
          {once
            ? "Scripts are approved one run at a time."
            : "Allow for this session allows this exact request again until the assistant's session ends."}
        </p>
      </div>
    </article>
  );
}
