import { useEffect, useState } from "react";
import type { Approval } from "./api";

const RISK: Record<string, string> = {
  export: "Export",
  download: "Download",
  script: "Script",
  edit: "Edit",
  notes: "Notes",
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
  /** Absent in a window that may only look. */
  onDecide?: (d: "allow_once" | "always_this_session" | "decline") => void;
}) {
  const { a } = props;
  const d = a.detail || {};
  const once = a.offer_always === false || a.risk === "script" || a.risk === "edit";
  const [open, setOpen] = useState(false);
  // A card that has just appeared cannot be clicked by accident.
  const [armed, setArmed] = useState(false);
  useEffect(() => {
    const t = window.setTimeout(() => setArmed(true), 1000);
    return () => window.clearTimeout(t);
  }, [a.id]);
  const off = props.busy || !armed;
  const rows: [string, string][] = [];
  if (d.destination) rows.push(["Destination", d.destination]);
  if (Array.isArray(d.files) && d.files.length)
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
        {Array.isArray(d.changes) &&
          d.changes.map((n, i) => (
            <div key={i}>
              <p className="approval-sub">{String(n.id)}:</p>
              <pre className="code">
                {(Array.isArray(n.changes) ? n.changes : [])
                  .flatMap((line) => String(line).split("\n"))
                  .map((part, j) => (
                    <span key={j} className="ln">
                      {part === "" ? " " : part}
                    </span>
                  ))}
              </pre>
            </div>
          ))}
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
        {props.onDecide ? (
          <>
            <div className="approval-actions">
              <button type="button" className="primary" disabled={off} onClick={() => props.onDecide?.("allow_once")}>
                Allow once
              </button>
              {!once && (
                <button type="button" disabled={off} onClick={() => props.onDecide?.("always_this_session")}>
                  Allow for this session
                </button>
              )}
              <button type="button" className="danger" disabled={off} onClick={() => props.onDecide?.("decline")}>
                Decline
              </button>
            </div>
            <p className="hint">
              {once
                ? a.risk === "edit"
                  ? "Edits are approved one change set at a time."
                  : a.risk === "script"
                    ? "Scripts are approved one run at a time."
                    : "This is asked every time."
                : a.always_covers || "Allow for this session allows this exact request again until the assistant's session ends."}
            </p>
          </>
        ) : null}
      </div>
    </article>
  );
}
