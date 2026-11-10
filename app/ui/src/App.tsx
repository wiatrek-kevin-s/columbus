import { useCallback, useEffect, useMemo, useRef, useState, type CSSProperties, type ReactNode } from "react";
import { api, currentScope, signIn, SignedOut, webApi, type Approval, type Check, type FeatureCollection, type OutputCard, type Overview, type WebSource, type WebSourceInfo } from "./api";
import { ApprovalCard } from "./ApprovalCard";
import { layout, NO_INSET, parseInset, type Inset } from "./layout";
import { MapView, type MapLayer, type WebDraw, type WebState } from "./MapView";
import { Table } from "./Table";
import { LAYER_TONES, useTheme } from "./theme";
import { NO_WEB_PREFS, ordered, WebLayers, type WebPrefs } from "./WebLayers";

const POLL_MS = 2000;
// How often the list of web layers is read again (one added, edited or
// removed in the Columbus app shows in every chat).
const WEB_POLL_MS = 5000;

type Phase = "starting" | "ready" | "no-link" | "used" | "ended";

// Shown inside the Columbus app, which lays its own panel over this page
// (the server lets only the app's own pages frame this one). A framed page
// never decides approvals, whatever its link: the app shows them and opens
// the browser page.
const FRAMED = (() => {
  try {
    return window.self !== window.top;
  } catch {
    return true;
  }
})();
const APP_ORIGINS = ["tauri://localhost", "http://tauri.localhost", "https://tauri.localhost"];

function size(b: number): string {
  if (b < 1024) return `${b} B`;
  if (b < 1024 * 1024) return `${(b / 1024).toFixed(0)} KB`;
  return `${(b / 1024 / 1024).toFixed(1)} MB`;
}

/** The Columbus mark: a compass, its north half in the "hit" colour. */
function Mark() {
  return (
    <svg className="mark" viewBox="0 0 24 24" aria-hidden="true">
      <circle cx="12" cy="12" r="10" fill="none" strokeWidth="1.6" />
      <path className="north" d="M12 5 14.4 12H9.6z" />
      <path className="south" d="M12 19 9.6 12h4.8z" />
    </svg>
  );
}

type Shape = "point" | "line" | "area" | "any";

/** How a layer's swatch is drawn: as its features are, once they are
 * here. */
function shapeOf(fc: FeatureCollection | null): Shape {
  const kind = fc?.geometry_type || fc?.features.find((f) => f.geometry)?.geometry?.type || "";
  if (/point/i.test(kind)) return "point";
  if (/line/i.test(kind)) return "line";
  if (/polygon/i.test(kind)) return "area";
  return "any";
}

/** A layer's mark in the colour it has on the map now; hollow while the
 * layer is off the map. */
function Swatch(props: { shape: Shape; color: string | null }) {
  return (
    <span
      className={`swatch ${props.shape}${props.color ? "" : " off"}`}
      style={props.color ? ({ "--c": props.color } as CSSProperties) : undefined}
      aria-hidden="true"
    />
  );
}

function CheckBadges(props: { c: OutputCard["checks"] }) {
  const c = props.c;
  if (!c) return null;
  if (!c.fail && !c.warn) return <span className="badge pass">Checks passed</span>;
  return (
    <>
      {c.fail > 0 && <span className="badge fail">{c.fail} failed</span>}
      {c.warn > 0 && <span className="badge warn">{c.warn} to review</span>}
    </>
  );
}

function Details(props: { card: OutputCard }) {
  const [prov, setProv] = useState<Record<string, unknown> | null>(null);
  const [error, setError] = useState<string | null>(null);
  const id = props.card.provenance;
  useEffect(() => {
    if (!id) return;
    api.provenance(id).then(setProv, (e) => setError(String(e.message || e)));
  }, [id]);
  if (!id) return <p className="muted">No provenance record for this file.</p>;
  if (error) return <p className="error">Could not read the provenance record: {error}</p>;
  if (!prov) return <p className="muted">Reading the provenance record…</p>;
  const checks = (Array.isArray(prov.checks) ? prov.checks : []) as Check[];
  const order = { fail: 0, warn: 1, info: 2 } as const;
  checks.sort((a, b) => order[a.severity] - order[b.severity]);
  const inputs = (Array.isArray(prov.inputs) ? prov.inputs : []) as Record<string, unknown>[];
  return (
    <div className="details">
      {checks.length > 0 && (
        <ul className="checks">
          {checks.map((c, i) => (
            <li key={i} className={`check ${c.severity}`}>
              <span className="check-sev">{c.severity === "fail" ? "Failed" : c.severity === "warn" ? "Review" : "Note"}</span>
              <span>
                {c.message}
                {c.fix && <span className="fix"> Fix: {c.fix}</span>}
              </span>
            </li>
          ))}
        </ul>
      )}
      {inputs.length > 0 && (
        <>
          <p className="sub">Made from</p>
          <ul className="inputs">
            {inputs.map((x, i) => (
              <li key={i}>{String(x.dataset ?? x.handle ?? x.path ?? JSON.stringify(x))}{x.connection ? ` (${String(x.connection)})` : ""}</li>
            ))}
          </ul>
        </>
      )}
      <p className="muted">
        Provenance <span className="mono">{id}</span>
        {prov.created ? `, ${new Date(String(prov.created)).toLocaleString()}` : ""}
      </p>
    </div>
  );
}

/** One output in the list: a toggle that shows it on the map (drawn as its
 * swatch), its name (click makes it the active one), its row count. The
 * active one opens: rows, size, checks, Table and Zoom to layer, and the
 * provenance behind Details. */
function OutputRow(props: {
  o: OutputCard;
  active: boolean;
  onMap: boolean;
  /** The colour it has on the map now. */
  color: string;
  layer: FeatureCollection | { error: string } | undefined;
  tableOpen: boolean;
  onToggle: () => void;
  onActive: () => void;
  onTable: () => void;
  onZoom: () => void;
}) {
  const { o } = props;
  const [details, setDetails] = useState(false);
  const err = props.layer && "error" in props.layer ? props.layer.error : null;
  const fc = props.layer && !("error" in props.layer) ? props.layer : null;
  const notes = (
    <>
      {fc?.truncated && (
        <p className="note">
          The map shows the first {fc.shown.toLocaleString()} of {fc.total.toLocaleString()} features.
        </p>
      )}
      {err && <p className="error">Not on the map: {err}</p>}
    </>
  );
  const cut = o.truncated && <span className="badge warn">Cut off at a limit</span>;
  // At rest only what needs the person's eye shows: a mark for checks that
  // failed or want a look.
  const flag = o.checks && (o.checks.fail > 0 || o.checks.warn > 0) ? (o.checks.fail > 0 ? "fail" : "warn") : null;
  const flagText = o.checks ? [o.checks.fail > 0 && `${o.checks.fail} failed`, o.checks.warn > 0 && `${o.checks.warn} to review`].filter(Boolean).join(", ") : "";
  return (
    <li className={props.active ? "out active" : "out"}>
      <div className="out-head">
        {o.kind === "layer" && o.viewable ? (
          <label className="out-show" title={`Show ${o.name} on the map`}>
            <input type="checkbox" aria-label={`Show ${o.name} on the map`} checked={props.onMap} onChange={props.onToggle} />
            <Swatch shape={shapeOf(fc)} color={props.onMap ? props.color : null} />
          </label>
        ) : (
          <span className="out-kind">{o.kind}</span>
        )}
        <button type="button" className="out-name" aria-current={props.active ? "true" : undefined} onClick={props.onActive}>
          {o.name}
        </button>
        {!props.active && flag && <span className={`dot ${flag}`} role="img" aria-label={flagText} title={flagText} />}
        {o.rows != null && (
          <span className="out-count" title={`${o.rows.toLocaleString()} rows`}>
            {o.rows.toLocaleString()}
          </span>
        )}
      </div>
      {props.active ? (
        <div className="out-open">
          <div className="out-meta">
            {o.rows != null && <span>{o.rows.toLocaleString()} rows</span>}
            <span>{size(o.bytes)}</span>
            <CheckBadges c={o.checks} />
            {cut}
          </div>
          {notes}
          <div className="out-acts">
            {o.viewable && (
              <button type="button" className="btn sm" aria-pressed={props.tableOpen} onClick={props.onTable}>
                <svg className="ic" viewBox="0 0 16 16" aria-hidden="true">
                  <rect x="2.5" y="3" width="11" height="10" rx="2" />
                  <path d="M2.5 6.5h11M2.5 9.75h11M6.5 6.5V13" />
                </svg>
                Table
              </button>
            )}
            {props.onMap && (
              <button type="button" className="btn sm" onClick={props.onZoom}>
                Zoom to layer
              </button>
            )}
          </div>
          <details className="out-more" onToggle={(e) => setDetails(e.currentTarget.open)}>
            <summary>Details</summary>
            {details && <Details card={o} />}
          </details>
        </div>
      ) : (
        (cut || fc?.truncated || err) && (
          <div className="out-flags">
            {cut}
            {notes}
          </div>
        )
      )}
    </li>
  );
}

/** The window's size: the page fills it, so this is the page's too. */
function useWindowSize(): { width: number; height: number } {
  const [s, setS] = useState(() => ({ width: window.innerWidth, height: window.innerHeight }));
  useEffect(() => {
    const on = () =>
      setS((old) =>
        old.width === window.innerWidth && old.height === window.innerHeight ? old : { width: window.innerWidth, height: window.innerHeight },
      );
    window.addEventListener("resize", on);
    on();
    return () => window.removeEventListener("resize", on);
  }, []);
  return s;
}

export function App() {
  const [phase, setPhase] = useState<Phase>("starting");
  const [overview, setOverview] = useState<Overview | null>(null);
  const [outputs, setOutputs] = useState<OutputCard[]>([]);
  const [version, setVersion] = useState(-1);
  const [pending, setPending] = useState<Approval[]>([]);
  const [busy, setBusy] = useState<Record<string, boolean>>({});
  const [onMap, setOnMap] = useState<string[]>([]);
  const [layers, setLayers] = useState<Record<string, FeatureCollection | { error: string }>>({});
  const [active, setActive] = useState<string | null>(null);
  const [fitTo, setFitTo] = useState<string | null>(null);
  const [selected, setSelected] = useState<{ handle: string; feature: number } | null>(null);
  const [zoomTo, setZoomTo] = useState<{ handle: string; feature: number; n: number } | null>(null);
  // Zoom to selection: on unless turned off, remembered per chat (this
  // window's session).
  const [zoomToSelection, setZoomToSelectionState] = useState(true);
  const zoomKey = overview ? `columbus.zoomToSelection.${overview.session}` : null;
  useEffect(() => {
    if (!zoomKey) return;
    try {
      setZoomToSelectionState(localStorage.getItem(zoomKey) !== "off");
    } catch {
      /* not remembered: stays on */
    }
  }, [zoomKey]);
  const setZoomToSelection = (on: boolean) => {
    setZoomToSelectionState(on);
    try {
      if (zoomKey) localStorage.setItem(zoomKey, on ? "on" : "off");
    } catch {
      /* the choice is not remembered */
    }
  };
  const [basemap, setBasemapState] = useState(() => {
    try {
      return localStorage.getItem("columbus.basemap") === "on";
    } catch {
      return false;
    }
  });
  const setBasemap = (on: boolean) => {
    setBasemapState(on);
    try {
      localStorage.setItem("columbus.basemap", on ? "on" : "off");
    } catch {
      /* the choice is not remembered */
    }
  };
  // The map alone until the person opens the table (or the app asks for
  // it).
  const [tableOpen, setTableOpen] = useState(false);
  const [measuring, setMeasuring] = useState(false);
  // How much of this page the app's own panel covers (framed only).
  const [inset, setInset] = useState<Inset>(NO_INSET);
  const theme = useTheme();
  const win = useWindowSize();
  const basemapSet = useRef(false);
  // The app's layout and preferences: plain values, only from the app's
  // own page that frames this one.
  useEffect(() => {
    if (!FRAMED) return;
    const on = (e: MessageEvent) => {
      if (e.source !== window.parent || !APP_ORIGINS.includes(e.origin)) return;
      const d = e.data as { type?: unknown; view?: unknown; basemap?: unknown; theme?: unknown; inset?: unknown } | null;
      if (d && d.type === "columbus-web-layers") {
        webReload.current();
        return;
      }
      if (!d || d.type !== "columbus-embed") return;
      if (d.view === "map") setTableOpen(false);
      else if (d.view === "table" || d.view === "split") setTableOpen(true);
      // What the app's panel covers, in CSS pixels: checked numbers only.
      const covered = parseInset(d.inset);
      if (covered) setInset(covered);
      if (typeof d.basemap === "boolean" && !basemapSet.current) {
        basemapSet.current = true;
        setBasemapState(d.basemap);
      }
      if (d.theme === "light" || d.theme === "dark") document.documentElement.dataset.theme = d.theme;
      else if (d.theme === "system") delete document.documentElement.dataset.theme;
    };
    window.addEventListener("message", on);
    // Nothing secret: only that the page is ready for the app's choices.
    window.parent.postMessage({ type: "columbus-embed-ready" }, "*");
    return () => window.removeEventListener("message", on);
  }, []);
  // ---------- Web map layers (T-25) ----------
  const [webList, setWebList] = useState<WebSource[] | null>(null);
  const [webEnabled, setWebEnabled] = useState(true);
  const [webListError, setWebListError] = useState<string | null>(null);
  const [webInfo, setWebInfo] = useState<Record<string, WebSourceInfo | { error: string }>>({});
  const [webState, setWebState] = useState<Record<string, WebState>>({});
  const [webPrefs, setWebPrefsState] = useState<WebPrefs>(NO_WEB_PREFS);
  const [webNotice, setWebNotice] = useState(false);
  const [webFit, setWebFit] = useState<{ id: string; n: number } | null>(null);
  const webReload = useRef<() => void>(() => {});
  // Per chat: this window's session, as Zoom to selection.
  const webKey = overview ? `columbus.web.${overview.session}` : null;
  useEffect(() => {
    if (!webKey) return;
    try {
      const v = JSON.parse(localStorage.getItem(webKey) || "null");
      if (v && typeof v === "object") setWebPrefsState({ ...NO_WEB_PREFS, ...v });
    } catch {
      /* not remembered: all off */
    }
  }, [webKey]);
  const setWebPrefs = (f: (p: WebPrefs) => WebPrefs) => {
    setWebPrefsState((p) => {
      const next = f(p);
      try {
        if (webKey) localStorage.setItem(webKey, JSON.stringify(next));
      } catch {
        /* the choice is not remembered */
      }
      return next;
    });
  };
  // The privacy notice, the first time any web layer is turned on in this
  // chat (R-21).
  const firstOn = () => {
    if (!webPrefs.noticed) {
      setWebNotice(true);
      return true;
    }
    return false;
  };
  const revOf = (id: string) => webList?.find((x) => x.id === id)?.rev || "0";

  const canDecide = currentScope() === "approve" && !FRAMED;
  const [reviewing, setReviewing] = useState(false);
  const [notice, setNotice] = useState<string | null>(null);
  const seen = useRef(new Set<string>());
  // Each output's place in the order of appearance: its resting colour.
  const rank = useRef(new Map<string, number>());

  const signedOut = useCallback((e: unknown) => {
    if (e instanceof SignedOut) setPhase((p) => (p === "ready" ? "ended" : p));
    else setNotice(String((e as Error).message || e));
  }, []);

  // Sign in from the link Columbus opened, and again if a new one is pasted.
  useEffect(() => {
    const go = () =>
      signIn().then((r) => {
        if (r === "signed-in") setPhase("ready");
        else if (r === "used") setPhase("used");
        else setPhase((p) => (p === "starting" ? "no-link" : p));
      });
    go();
    window.addEventListener("hashchange", go);
    return () => window.removeEventListener("hashchange", go);
  }, []);

  useEffect(() => {
    if (phase === "ready") api.overview().then(setOverview, signedOut);
  }, [phase, signedOut]);

  // The web layers the person can turn on: read now, every few seconds, when
  // the window gets the focus, and when the app says one changed.
  useEffect(() => {
    if (phase !== "ready") return;
    let live = true;
    const load = () =>
      webApi.sources().then(
        (r) => {
          if (!live) return;
          setWebEnabled(r.enabled);
          setWebListError(null);
          setWebList((old) => (old && JSON.stringify(old) === JSON.stringify(r.sources) ? old : r.sources));
        },
        (e) => {
          if (!live) return;
          if (e instanceof SignedOut) return signedOut(e);
          setWebListError(String((e as Error).message || e));
        },
      );
    webReload.current = load;
    load();
    const t = window.setInterval(load, WEB_POLL_MS);
    window.addEventListener("focus", load);
    return () => {
      live = false;
      window.clearInterval(t);
      window.removeEventListener("focus", load);
      webReload.current = () => {};
    };
  }, [phase, signedOut]);

  // A layer removed in the app is off here too.
  useEffect(() => {
    if (!webList) return;
    const ids = new Set(webList.map((x) => x.id));
    if (webPrefs.on.some((id) => !ids.has(id)) || (webPrefs.base && !ids.has(webPrefs.base))) {
      setWebPrefs((p) => ({ ...p, on: p.on.filter((id) => ids.has(id)), base: p.base && ids.has(p.base) ? p.base : null }));
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [webList, webPrefs.on, webPrefs.base]);

  // What the map needs for each layer turned on (or chosen as basemap),
  // read again when the layer is edited.
  useEffect(() => {
    if (!webList) return;
    for (const id of [...webPrefs.on, ...(webPrefs.base ? [webPrefs.base] : [])]) {
      const src = webList.find((x) => x.id === id);
      if (!src) continue;
      const key = `${id}~${src.rev || "0"}`;
      if (webInfo[key]) continue;
      setWebInfo((w) => ({ ...w, [key]: { error: "" } }));
      webApi.source(id).then(
        (info) => {
          setWebInfo((w) => ({ ...w, [key]: info }));
          setWebState((st) => ({ ...st, [id]: { ...st[id], error: null } }));
        },
        (e) => {
          if (e instanceof SignedOut) return signedOut(e);
          const msg = String((e as Error).message || e);
          setWebInfo((w) => ({ ...w, [key]: { error: msg } }));
          setWebState((st) => ({ ...st, [id]: { ...st[id], error: msg, drawn: false } }));
        },
      );
    }
  }, [webList, webPrefs.on, webPrefs.base, webInfo, signedOut]);

  const webDraw = (id: string): WebDraw | null => {
    const rev = revOf(id);
    const info = webInfo[`${id}~${rev}`];
    if (!info || "error" in info) return null;
    return { id, rev, info, opacity: webPrefs.opacity[id] ?? 1 };
  };
  const webLayers: WebDraw[] = useMemo(
    () =>
      ordered(webList || [], webPrefs.order)
        .filter((x) => webPrefs.on.includes(x.id) && x.id !== webPrefs.base)
        .map((x) => webDraw(x.id))
        .filter((x): x is WebDraw => x !== null)
        .reverse(),
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [webList, webPrefs, webInfo],
  );
  const webBase = useMemo(
    () => {
      if (!webPrefs.base) return null;
      const d = webDraw(webPrefs.base);
      return d && d.info.raster ? d : null;
    },
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [webList, webPrefs.base, webInfo],
  );

  // Poll for changes: new outputs, and requests waiting for the person.
  useEffect(() => {
    if (phase !== "ready") return;
    let live = true;
    let last = -1;
    let lastPending = -1;
    const tick = async () => {
      try {
        const s = await api.state();
        if (s.version !== last) {
          last = s.version;
          const o = await api.outputs();
          if (!live) return;
          setOutputs(o.outputs);
          setVersion(o.version);
        }
        if (s.pending !== lastPending || s.pending > 0) {
          lastPending = s.pending;
          const a = await api.approvals();
          if (live) setPending(a.pending);
        }
      } catch (e) {
        signedOut(e);
      }
    };
    tick();
    const t = window.setInterval(tick, POLL_MS);
    return () => {
      live = false;
      window.clearInterval(t);
    };
  }, [phase, signedOut]);

  // A new layer goes on the map and gets the focus; the person can take it off.
  useEffect(() => {
    const fresh = outputs.filter((o) => o.viewable && o.kind === "layer" && !seen.current.has(o.handle));
    for (const o of outputs) seen.current.add(o.handle);
    if (fresh.length) {
      const newest = fresh[0];
      // A workflow run opened in this chat: all of its layers draw at once.
      const fromRun = fresh.filter((o) => (o.made_by || "").startsWith("workflow ")).map((o) => o.handle);
      setOnMap((m) => [...new Set([...m, ...fromRun, newest.handle])]);
      setActive(newest.handle);
      setFitTo(newest.handle);
    }
    // Drop layers whose files are gone.
    const present = new Set(outputs.map((o) => o.handle));
    setOnMap((m) => m.filter((h) => present.has(h)));
  }, [outputs]);

  // Load what is on the map.
  useEffect(() => {
    for (const h of onMap) {
      if (layers[h]) continue;
      setLayers((l) => ({ ...l, [h]: { type: "FeatureCollection", features: [], total: 0, shown: 0, truncated: false, geometry: true } }));
      api.layer(h).then(
        (fc) => setLayers((l) => ({ ...l, [h]: fc })),
        (e) => {
          if (e instanceof SignedOut) return signedOut(e);
          setLayers((l) => ({ ...l, [h]: { error: String(e.message || e) } }));
        },
      );
    }
  }, [onMap, layers, signedOut]);

  // Colour belongs to the data. The active output is drawn in the "hit"
  // colour; every other output rests in a quiet colour that stays its own,
  // by order of appearance.
  for (const o of outputs) if (!rank.current.has(o.handle)) rank.current.set(o.handle, rank.current.size);
  const tones = LAYER_TONES[theme];
  const colorOf = (handle: string) => (handle === active ? tones.hit : tones.rest[(rank.current.get(handle) ?? 0) % tones.rest.length]);

  // What the map draws, bottom first: the list's order, its top row on
  // top.
  const mapLayers: MapLayer[] = useMemo(
    () =>
      outputs
        .filter((o) => onMap.includes(o.handle))
        .map((o) => {
          const d = layers[o.handle];
          if (!d || "error" in d) return null;
          return { handle: o.handle, name: o.name, color: colorOf(o.handle), active: o.handle === active, data: d };
        })
        .filter((x): x is MapLayer => x !== null)
        .reverse(),
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [onMap, layers, outputs, active, theme],
  );

  const decide = async (id: string, d: "allow_once" | "always_this_session" | "decline") => {
    setBusy((b) => ({ ...b, [id]: true }));
    try {
      await api.decide(id, d);
      setPending((p) => p.filter((a) => a.id !== id));
      setNotice(d === "decline" ? "Declined. Your assistant has been told." : "Allowed. Your assistant can go ahead.");
    } catch (e) {
      signedOut(e);
    } finally {
      setBusy((b) => ({ ...b, [id]: false }));
    }
  };

  const review = async () => {
    setReviewing(true);
    try {
      await api.review();
      setNotice("Columbus opened the request in your browser. Decide there.");
    } catch (e) {
      signedOut(e);
    } finally {
      window.setTimeout(() => setReviewing(false), 3000);
    }
  };

  const toggle = (h: string) => setOnMap((m) => (m.includes(h) ? m.filter((x) => x !== h) : [...m, h]));
  const activeCard = outputs.find((o) => o.handle === active) || null;
  const activeLayer = active ? layers[active] : undefined;

  useEffect(() => {
    document.title = pending.length ? `(${pending.length}) Columbus: approval needed` : "Columbus";
  }, [pending.length]);
  useEffect(() => {
    if (!notice) return;
    const t = window.setTimeout(() => setNotice(null), 6000);
    return () => window.clearTimeout(t);
  }, [notice]);

  // Where the sheets go, and what they leave of the map. The page's own
  // panel is wider while a request can be decided in it.
  const deciding = canDecide && pending.length > 0;
  const lay = layout({
    width: win.width,
    height: win.height,
    // Framed: what the app's panel covers. Otherwise the page's own panel,
    // once there is a session to show in it.
    covered: FRAMED ? inset : phase === "ready" ? null : NO_INSET,
    tableOpen: phase === "ready" && tableOpen,
    deciding,
  });
  const px = (n: number) => `${n}px`;
  const place = {
    "--il": px(lay.inset.left),
    "--it": px(lay.inset.top),
    "--ir": px(lay.inset.right),
    "--ib": px(lay.inset.bottom),
    "--pl": px(lay.pad.left),
    "--pt": px(lay.pad.top),
    "--pr": px(lay.pad.right),
    "--pb": px(lay.pad.bottom),
    "--table": px(lay.table),
    ...(lay.panel ? { "--panel-w": px(lay.panel.width), "--panel-h": px(lay.panel.height) } : {}),
  } as CSSProperties;

  if (phase !== "ready") {
    const text: Record<Exclude<Phase, "ready">, string> = {
      starting: "Connecting to this session…",
      "no-link": "Open this window from the Columbus app, or from the link Columbus opens for you. Each link works once.",
      used: "This link has already been used or has expired: for safety, each link Columbus opens works once. Open the session again from the Columbus app.",
      ended: "This window is no longer connected to its session. The session may have ended, or the window sat idle for 15 minutes. Open it again from the Columbus app.",
    };
    return (
      <main className="signed-out" style={place}>
        <div className="sheet">
          <div className="brand">
            <Mark />
            <h1>Columbus</h1>
          </div>
          <p>{text[phase]}</p>
        </div>
      </main>
    );
  }

  // The session's outputs: the legend when framed, a part of the page's
  // own panel otherwise.
  const list = (
    <aside className="outputs" aria-label="Outputs from this session">
      <h2>Outputs</h2>
      {outputs.length === 0 && (
        <p className="empty">
          Nothing yet. Layers your assistant makes in this session appear here and on the map as they are made.
        </p>
      )}
      <ul>
        {outputs.map((o) => (
          <OutputRow
            key={o.handle}
            o={o}
            active={o.handle === active}
            onMap={onMap.includes(o.handle)}
            color={colorOf(o.handle)}
            layer={layers[o.handle]}
            tableOpen={tableOpen}
            onToggle={() => toggle(o.handle)}
            onActive={() => setActive(o.handle)}
            onTable={() => setTableOpen((t) => !t)}
            onZoom={() => setFitTo(o.handle + "#" + Date.now())}
          />
        ))}
      </ul>
    </aside>
  );

  // Under the outputs: the street background, web layers, measure.
  const streets = basemap && !webPrefs.base;
  const foot = (
    <div className="foot">
      {overview?.basemap_allowed && (
        <label className="switch" title="Street basemap from OpenFreeMap. When on, the area in view is sent to that site to fetch map tiles.">
          <input
            type="checkbox"
            checked={streets}
            onChange={(e) => {
              setBasemap(e.target.checked);
              if (e.target.checked) setWebPrefs((p) => ({ ...p, base: null }));
            }}
          />
          <svg className="ic" viewBox="0 0 16 16" aria-hidden="true">
            <path d="M8 2.5 2.5 5.5 8 8.5l5.5-3L8 2.5zM2.5 8.5 8 11.5l5.5-3" />
          </svg>
          <span className="grow">Street background</span>
          <span className="switch-state" aria-hidden="true">
            {streets ? "On" : "Off"}
          </span>
        </label>
      )}
      <WebLayers
        enabled={webEnabled}
        list={webList}
        listError={webListError}
        prefs={webPrefs}
        state={webState}
        streetsAllowed={!!overview?.basemap_allowed}
        streetsOn={basemap}
        notice={webNotice}
        framed={FRAMED}
        onToggle={(id, on) => {
          const notice = on && firstOn();
          setWebPrefs((p) => ({
            ...p,
            noticed: p.noticed || notice,
            on: on ? [...new Set([...p.on, id])] : p.on.filter((x) => x !== id),
            order: p.order.includes(id) ? p.order : [...ordered(webList || [], p.order).map((x) => x.id)],
          }));
        }}
        onOpacity={(id, v) => setWebPrefs((p) => ({ ...p, opacity: { ...p.opacity, [id]: v } }))}
        onMove={(id, by) =>
          setWebPrefs((p) => {
            const ids = ordered(webList || [], p.order).map((x) => x.id);
            const i = ids.indexOf(id);
            const j = i + by;
            if (i < 0 || j < 0 || j >= ids.length) return p;
            [ids[i], ids[j]] = [ids[j], ids[i]];
            return { ...p, order: ids };
          })
        }
        onZoom={(id) => setWebFit({ id, n: Date.now() })}
        onBasemap={(choice) => {
          if (choice === "streets" || choice === "none") {
            setBasemap(choice === "streets");
            setWebPrefs((p) => ({ ...p, base: null }));
          } else {
            const notice = firstOn();
            setBasemap(false);
            setWebPrefs((p) => ({ ...p, noticed: p.noticed || notice, base: choice.web }));
          }
        }}
        onDismissNotice={() => setWebNotice(false)}
        onAdd={() => window.parent.postMessage({ type: "columbus-add-web-layer" }, "*")}
        beside={
          <button type="button" className="btn sm" aria-pressed={measuring} onClick={() => setMeasuring((m) => !m)}>
            Measure
          </button>
        }
      />
    </div>
  );

  let toast: ReactNode = null;
  if (notice)
    toast = (
      <div className="toast" role="status">
        {notice}
      </div>
    );

  return (
    <div className={FRAMED ? "app embedded" : "app"} style={place}>
      {FRAMED ? (
        // Top right, clear of the app's own panel.
        <div className="legend">
          {list}
          {foot}
        </div>
      ) : (
        <div className="panel">
          <header className="top">
            <div className="brand">
              <Mark />
              <h1>Columbus</h1>
            </div>
            <div className="session">
              {overview ? (
                <>
                  <span>{overview.user}</span>
                  <span className="muted">
                    Session <span className="mono">{overview.session}</span>
                  </span>
                </>
              ) : (
                <span className="muted">Loading the session…</span>
              )}
            </div>
            {!!overview?.connections.length && (
              <div className="conns" aria-label="Connections this session may use">
                {overview.connections.map((c) => (
                  <span key={c.id} className="conn" title={c.description || c.type}>
                    {c.id}
                  </span>
                ))}
              </div>
            )}
          </header>
          {pending.length > 0 && (
            // Approvals are never shown in a frame: this panel is not there.
            <section className="approvals" aria-live="assertive" aria-label="Requests waiting for your decision">
              {!canDecide && (
                <div className="review">
                  <p>
                    {pending.length === 1 ? "A request is" : `${pending.length} requests are`} waiting for your decision.
                    For your safety, requests are decided in the browser page Columbus opens, not in this window.
                  </p>
                  <button type="button" className="btn primary" disabled={reviewing} onClick={review}>
                    Review in your browser
                  </button>
                </div>
              )}
              {pending.map((a) => (
                <ApprovalCard
                  key={a.id}
                  a={a}
                  busy={!!busy[a.id]}
                  onDecide={canDecide ? (d) => decide(a.id, d) : undefined}
                />
              ))}
            </section>
          )}
          {list}
          {foot}
        </div>
      )}

      <main className="work">
        <MapView
          layers={mapLayers}
          theme={theme}
          basemap={basemap && !!overview?.basemap_allowed && !webBase}
          padding={lay.pad}
          fitTo={fitTo}
          selected={selected}
          zoomTo={zoomTo}
          measuring={measuring}
          onMeasuring={setMeasuring}
          notice={toast}
          onBasemapFailed={() => {
            setBasemap(false);
            setNotice("The basemap could not be loaded (offline, or blocked by your network). Your layers still show.");
          }}
          onIdentify={(h, f) => {
            setActive(h);
            setSelected({ handle: h, feature: f });
          }}
          webLayers={webLayers}
          webBase={webBase}
          webFit={webFit}
          onWebState={(id, st) => setWebState((all) => ({ ...all, [id]: { ...all[id], ...st } }))}
        />
        {tableOpen && (
          <section className="table" aria-label="Attribute table">
            <Table
              output={activeCard}
              version={version}
              selected={selected && selected.handle === active ? selected.feature : null}
              onSelect={(f) => {
                if (!active) return;
                setSelected({ handle: active, feature: f });
                if (zoomToSelection) setZoomTo({ handle: active, feature: f, n: Date.now() });
              }}
              swatch={
                activeCard?.kind === "layer" && activeCard.viewable ? (
                  <Swatch
                    shape={shapeOf(activeLayer && !("error" in activeLayer) ? activeLayer : null)}
                    color={onMap.includes(activeCard.handle) ? tones.hit : null}
                  />
                ) : null
              }
              zoomToSelection={zoomToSelection}
              onZoomToSelection={setZoomToSelection}
              onClose={() => setTableOpen(false)}
            />
          </section>
        )}
      </main>
    </div>
  );
}
