import { useEffect, useState } from "react";
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

/** The selected output's attribute table, a page at a time. Clicking a
 * row selects that feature on the map. */
export function Table(props: {
  output: OutputCard | null;
  version: number;
  selected: number | null;
  onSelect: (feature: number) => void;
}) {
  const [page, setPage] = useState(0);
  const [data, setData] = useState<Rows | null>(null);
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
      setData(null);
      return;
    }
    api
      .rows(handle, page * PAGE, PAGE)
      .then((r) => live && setData(r))
      .catch((e) => live && setError(String(e.message || e)));
    return () => {
      live = false;
    };
  }, [handle, page, props.version]);

  if (!props.output) return <div className="table-empty">Choose an output to see its rows.</div>;
  if (!props.output.viewable)
    return <div className="table-empty">Columbus shows rows only for layers and tables its own tools made.</div>;
  if (error) return <div className="table-empty error">Could not read the rows: {error}</div>;
  if (!data) return <div className="table-empty">Reading rows…</div>;

  const last = Math.max(0, Math.ceil(data.total / PAGE) - 1);
  return (
    <div className="table-wrap">
      <div className="table-bar">
        <span>
          Rows {data.total ? data.offset + 1 : 0}–{data.offset + data.rows.length} of {data.total.toLocaleString()}
        </span>
        <span className="pager">
          <button type="button" disabled={page === 0} onClick={() => setPage(page - 1)}>
            Previous
          </button>
          <button type="button" disabled={page >= last} onClick={() => setPage(page + 1)}>
            Next
          </button>
        </span>
      </div>
      <div className="table-scroll" tabIndex={0} aria-label={`Rows of ${props.output.name}`}>
        <table>
          <thead>
            <tr>
              <th scope="col" className="num">
                #
              </th>
              {data.columns.map((c) => (
                <th key={c} scope="col">
                  {c}
                </th>
              ))}
            </tr>
          </thead>
          <tbody>
            {data.rows.map((r, i) => {
              const n = data.offset + i;
              return (
                <tr
                  key={n}
                  className={n === props.selected ? "selected" : undefined}
                  onClick={() => props.onSelect(n)}
                >
                  <td className="num">{n + 1}</td>
                  {r.map((v, j) => (
                    <td key={j} className={typeof v === "number" ? "num" : undefined}>
                      {cell(v)}
                    </td>
                  ))}
                </tr>
              );
            })}
          </tbody>
        </table>
      </div>
    </div>
  );
}
