import { useEffect, useState, type ReactNode } from "react";
import { api, type OutputCard, type Rows } from "./api";

const PAGE = 100;

function cell(v: unknown): string {
  if (v == null) return "";
  if (typeof v === "object") {
    const g = v as { type?: string };
    return g.type ? g.type : JSON.stringify(v);
  }
  return String(v);
}

/** The active output's attribute table, a page at a time: a sheet over the
 * map, with its own head (what it shows, which rows, the pager, Zoom to
 * selection, close). Clicking a row selects that feature on the map. */
export function Table(props: {
  output: OutputCard | null;
  version: number;
  selected: number | null;
  onSelect: (feature: number) => void;
  /** The output's swatch, as in the list. */
  swatch?: ReactNode;
  zoomToSelection: boolean;
  onZoomToSelection: (on: boolean) => void;
  onClose: () => void;
}) {
  const [page, setPage] = useState(0);
  // The rows read last, and the output they belong to.
  const [read, setRead] = useState<{ handle: string; rows: Rows } | null>(null);
  const [error, setError] = useState<string | null>(null);
  const handle = props.output?.viewable ? props.output.handle : null;

  useEffect(() => setPage(0), [handle]);
  useEffect(() => {
    if (props.selected != null) setPage(Math.floor(props.selected / PAGE));
  }, [props.selected]);
  useEffect(() => {
    let live = true;
    setError(null);
    if (!handle) {
      setRead(null);
      return;
    }
    api
      .rows(handle, page * PAGE, PAGE)
      .then((r) => live && setRead({ handle, rows: r }))
      .catch((e) => live && setError(String(e.message || e)));
    return () => {
      live = false;
    };
  }, [handle, page, props.version]);

  const data = read && read.handle === handle ? read.rows : null;
  const last = data ? Math.max(0, Math.ceil(data.total / PAGE) - 1) : 0;
  // A column of numbers reads from the right. One of measures or
  // coordinates (any number in it with a fraction) is set in the
  // fixed-width face, so its digits line up.
  const numbers = data
    ? data.columns.map((_, j) => data.rows.some((r) => typeof r[j] === "number") && data.rows.every((r) => r[j] == null || typeof r[j] === "number"))
    : [];
  const fractions = data ? data.columns.map((_, j) => data.rows.some((r) => typeof r[j] === "number" && !Number.isInteger(r[j]))) : [];

  let body: ReactNode;
  if (!props.output) body = <div className="table-empty">Choose an output to see its rows.</div>;
  else if (!props.output.viewable)
    body = <div className="table-empty">Columbus shows rows only for layers and tables its own tools made.</div>;
  else if (error) body = <div className="table-empty error">Could not read the rows: {error}</div>;
  else if (!data) body = <div className="table-empty">Reading rows…</div>;
  else
    body = (
      <div className="table-scroll" tabIndex={0} aria-label={`Rows of ${props.output.name}`}>
        <table>
          <thead>
            <tr>
              <th scope="col" className="num">
                #
              </th>
              {data.columns.map((c, j) => (
                <th key={c} scope="col" className={numbers[j] ? "num" : undefined}>
                  {c}
                </th>
              ))}
            </tr>
          </thead>
          <tbody>
            {data.rows.map((r, i) => {
              const n = data.offset + i;
              return (
                <tr key={n} className={n === props.selected ? "selected" : undefined} onClick={() => props.onSelect(n)}>
                  <td className="num">{n + 1}</td>
                  {r.map((v, j) => (
                    <td key={j} className={typeof v === "number" ? (fractions[j] ? "num mono" : "num") : undefined}>
                      {cell(v)}
                    </td>
                  ))}
                </tr>
              );
            })}
          </tbody>
        </table>
      </div>
    );

  return (
    <div className="table-wrap">
      <div className="table-bar">
        {props.output && props.swatch}
        <h2 className="table-name">{props.output ? props.output.name : "Table"}</h2>
        {data && (
          <span className="table-count">
            Rows {data.total ? data.offset + 1 : 0}–{data.offset + data.rows.length} of {data.total.toLocaleString()}
          </span>
        )}
        <span className="grow" />
        <label className="tick" title="Clicking a row in the table zooms the map to that feature">
          <input type="checkbox" checked={props.zoomToSelection} onChange={(e) => props.onZoomToSelection(e.target.checked)} />
          Zoom to selection
        </label>
        {data && (
          <span className="pager">
            <button type="button" className="btn sm" disabled={page === 0} onClick={() => setPage(page - 1)}>
              Previous
            </button>
            <button type="button" className="btn sm" disabled={page >= last} onClick={() => setPage(page + 1)}>
              Next
            </button>
          </span>
        )}
        <button type="button" className="iconbtn" aria-label="Close the table" onClick={props.onClose}>
          <svg viewBox="0 0 16 16" aria-hidden="true">
            <path d="M4 4l8 8M12 4l-8 8" />
          </svg>
        </button>
      </div>
      {body}
    </div>
  );
}
