import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { api, currentScope, signIn, SignedOut, webApi, type Approval, type Check, type FeatureCollection, type OutputCard, type Overview, type WebSource, type WebSourceInfo } from "./api";
import { ApprovalCard } from "./ApprovalCard";
import { MapView, type MapLayer, type WebDraw, type WebState } from "./MapView";
import { Table } from "./Table";
import { NO_WEB_PREFS, ordered, WebLayers, type WebPrefs } from "./WebLayers";

// Distinct, colour-blind-aware hues for layers, in the order they appear.
const COLORS = ["#1767c2", "#c2185b", "#2e7d32", "#ef6c00", "#6a1b9a", "#00838f", "#8d6e63", "#546e7a"];
const POLL_MS = 2000;
// How often the list of web layers is read again (one added, edited or
// removed in the Columbus app shows in every chat).
const WEB_POLL_MS = 5000;

type Phase = "starting" | "ready" | "no-link" | "used" | "ended";

// Shown inside the Columbus app's output panel (the server lets only the
// app's own pages frame this one). A framed page never decides approvals,
// whatever its link: the app shows them and opens the browser page.
const FRAMED = (() => {
  try {
    return window.self !== window.top;
  } catch {
    return true;
  }
})();
const APP_ORIGINS = ["tauri://localhost", "http://tauri.localhost", "https://tauri.localhost"];
type EmbedView = "map" | "table" | "split";

function size(b: number): string {
  if (b < 1024) return `${b} B`;
  if (b < 1024 * 1024) return `${(b / 1024).toFixed(0)} KB`;
  return `${(b / 1024 / 1024).toFixed(1)} MB`;
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
        Provenance {id}
        {prov.created ? `, ${new Date(String(prov.created)).toLocaleString()}` : ""}
      </p>
    </div>
  );
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
  const [embedView, setEmbedView] = useState<EmbedView>("split");
  const basemapSet = useRef(false);
  // The app's layout and preferences: plain values, only from the app's
  // own page that frames this one.
  useEffect(() => {
    if (!FRAMED) return;
    const on = (e: MessageEvent) => {
      if (e.source !== window.parent || !APP_ORIGINS.includes(e.origin)) return;
      const d = e.data as { type?: unknown; view?: unknown; basemap?: unknown; theme?: unknown } | null;
      if (d && d.type === "columbus-web-layers") {
        webReload.current();
        return;
      }
      if (!d || d.type !== "columbus-embed") return;
      if (d.view === "map" || d.view === "table" || d.view === "split") setEmbedView(d.view);
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
  const colors = useRef(new Map<string, string>());

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
    for (const o of outputs) {
      if (!colors.current.has(o.handle)) colors.current.set(o.handle, COLORS[colors.current.size % COLORS.length]);
    }
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

  const mapLayers: MapLayer[] = useMemo(
    () =>
      onMap
        .map((h) => {
          const d = layers[h];
          const card = outputs.find((o) => o.handle === h);
          if (!d || "error" in d || !card) return null;
          return { handle: h, name: card.name, color: colors.current.get(h) || COLORS[0], data: d };
        })
        .filter((x): x is MapLayer => x !== null)
        .reverse(),
    [onMap, layers, outputs],
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

  useEffect(() => {
    document.title = pending.length ? `(${pending.length}) Columbus: approval needed` : "Columbus";
  }, [pending.length]);
  useEffect(() => {
    if (!notice) return;
    const t = window.setTimeout(() => setNotice(null), 6000);
    return () => window.clearTimeout(t);
  }, [notice]);

  if (phase !== "ready") {
    const text: Record<Exclude<Phase, "ready">, string> = {
      starting: "Connecting to this session…",
      "no-link": "Open this window from the Columbus app, or from the link Columbus opens for you. Each link works once.",
      used: "This link has already been used or has expired: for safety, each link Columbus opens works once. Open the session again from the Columbus app.",
      ended: "This window is no longer connected to its session. The session may have ended, or the window sat idle for 15 minutes. Open it again from the Columbus app.",
    };
    return (
      <main className="signed-out">
        <h1>Columbus</h1>
        <p>{text[phase]}</p>
      </main>
    );
  }

  return (
    <div className={FRAMED ? `app embedded v-${embedView}` : "app"}>
      <header className="top">
        <h1>Columbus</h1>
        <div className="session">
          {overview ? (
            <>
              <span>{overview.user}</span>
              <span className="muted">Session {overview.session}</span>
            </>
          ) : (
            <span className="muted">Loading the session…</span>
          )}
        </div>
        <div className="conns" aria-label="Connections this session may use">
          {overview?.connections.map((c) => (
            <span key={c.id} className="conn" title={c.description || c.type}>
              {c.id}
            </span>
          ))}
        </div>
        {overview?.basemap_allowed && (
          <label className="switch" title="Street basemap from OpenFreeMap. When on, the area in view is sent to that site to fetch map tiles.">
            <input
              type="checkbox"
              checked={basemap && !webPrefs.base}
              onChange={(e) => {
                setBasemap(e.target.checked);
                if (e.target.checked) setWebPrefs((p) => ({ ...p, base: null }));
              }}
            />
            Basemap
          </label>
        )}
      </header>

      <div className="body">
        <aside className="outputs" aria-label="Outputs from this session">
          <h2>Outputs</h2>
          {outputs.length === 0 && (
            <p className="empty">
              Nothing yet. Layers your assistant makes in this session appear here and on the map as they are made.
            </p>
          )}
          <ul>
            {outputs.map((o) => {
              const err = layers[o.handle] && "error" in layers[o.handle] ? (layers[o.handle] as { error: string }).error : null;
              const fc = layers[o.handle] && !("error" in layers[o.handle]) ? (layers[o.handle] as FeatureCollection) : null;
              return (
                <li key={o.handle} className={o.handle === active ? "out active" : "out"}>
                  <div className="out-head">
                    {o.kind === "layer" && o.viewable ? (
                      <input
                        type="checkbox"
                        aria-label={`Show ${o.name} on the map`}
                        checked={onMap.includes(o.handle)}
                        onChange={() => toggle(o.handle)}
                        style={{ accentColor: colors.current.get(o.handle) }}
                      />
                    ) : (
                      <span className="out-kind">{o.kind}</span>
                    )}
                    <button type="button" className="out-name" onClick={() => setActive(o.handle)}>
                      {o.name}
                    </button>
                  </div>
                  <div className="out-meta">
                    {o.rows != null && <span>{o.rows.toLocaleString()} rows</span>}
                    <span>{size(o.bytes)}</span>
                    <CheckBadges c={o.checks} />
                    {o.truncated && <span className="badge warn">Cut off at a limit</span>}
                  </div>
                  {fc?.truncated && (
                    <p className="note">
                      The map shows the first {fc.shown.toLocaleString()} of {fc.total.toLocaleString()} features.
                    </p>
                  )}
                  {err && <p className="error">Not on the map: {err}</p>}
                  {o.handle === active && (
                    <>
                      {onMap.includes(o.handle) && (
                        <button type="button" className="link" onClick={() => setFitTo(o.handle + "#" + Date.now())}>
                          Zoom to layer
                        </button>
                      )}
                      <Details card={o} />
                    </>
                  )}
                </li>
              );
            })}
          </ul>
        </aside>

        <main className="work">
          {pending.length > 0 && !FRAMED && (
            <section className="approvals" aria-live="assertive" aria-label="Requests waiting for your decision">
              {!canDecide && (
                <div className="review">
                  <p>
                    {pending.length === 1 ? "A request is" : `${pending.length} requests are`} waiting for your decision.
                    For your safety, requests are decided in the browser page Columbus opens, not in this window.
                  </p>
                  <button type="button" className="primary" disabled={reviewing} onClick={review}>
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
          <MapView
            layers={mapLayers}
            basemap={basemap && !!overview?.basemap_allowed && !webBase}
            fitTo={fitTo}
            selected={selected}
            zoomTo={zoomTo}
            zoomToSelection={zoomToSelection}
            onZoomToSelection={setZoomToSelection}
            tools={
              <>
                {FRAMED && overview?.basemap_allowed && (
                  <label className="switch" title="Street basemap from OpenFreeMap. When on, the area in view is sent to that site to fetch map tiles.">
                    <input
                      type="checkbox"
                      checked={basemap && !webPrefs.base}
                      onChange={(e) => {
                        setBasemap(e.target.checked);
                        if (e.target.checked) setWebPrefs((p) => ({ ...p, base: null }));
                      }}
                    />
                    Basemap
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
                />
              </>
            }
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
            />
          </section>
        </main>
      </div>
      {notice && (
        <div className="toast" role="status">
          {notice}
        </div>
      )}
    </div>
  );
}
