import { useEffect, useRef, useState, type MutableRefObject, type ReactNode } from "react";
import * as maplibregl from "maplibre-gl";
import type {
  CircleLayerSpecification,
  ExpressionSpecification,
  FillLayerSpecification,
  GeoJSONSource,
  LineLayerSpecification,
  StyleSpecification,
} from "maplibre-gl";
// MapLibre finds its worker next to its own file, which a bundle does not
// have; give it the built copy, served from this server like the rest.
import workerUrl from "maplibre-gl/dist/maplibre-gl-worker.mjs?url";

maplibregl.setWorkerUrl(workerUrl);
import { authorization, isWebLayerUrl, webApi, WebLayerError, type FeatureCollection, type WebSourceInfo } from "./api";
import { BASEMAP_URL, blankStyle, isStyle, restyle } from "./basemap";
import type { Inset } from "./layout";
import { formatLength, lineMetres } from "./measure";
import { LAYER_TONES, type Theme } from "./theme";
import { normalExtent, type Extent } from "./view";

export interface MapLayer {
  handle: string;
  name: string;
  /** The colour it has on the map now: the "hit" colour while it is the
   * active output, its own quiet colour otherwise. */
  color: string;
  active: boolean;
  data: FeatureCollection;
}

/** A web layer on the map (T-25): drawn through this server only. */
export interface WebDraw {
  id: string;
  rev: string;
  info: WebSourceInfo;
  opacity: number;
}

/** What the map learnt about a web layer, for its row in the list. */
export interface WebState {
  error?: string | null;
  drawn?: boolean;
  total?: number;
  shown?: number;
  exceeded?: boolean;
}

// Web layers' sources: `w~<id>~<rev>` (on the map) and `wb~<id>~<rev>`
// (as the basemap). Their layers carry the same id, plus -fill, -line and
// -point for feature layers.
const webSid = (d: WebDraw, base: boolean) => `${base ? "wb" : "w"}~${d.id}~${d.rev}`;
const isWeb = (sid: string | undefined) => !!sid && (sid.startsWith("w~") || sid.startsWith("wb~"));
const webIdOf = (sid: string) => sid.split("~")[1] || "";
const FEATURE_PARTS = ["-fill", "-line", "-point"];
// Web layers keep a colour of their own, apart from the session's.
const WEB_COLOR = "#7b3fa0";

/** A feature's attributes as a small table (text only). */
function attributeTable(props: Record<string, unknown>): HTMLElement {
  const t = document.createElement("table");
  t.className = "web-attrs";
  for (const [k, v] of Object.entries(props)) {
    const tr = t.insertRow();
    const th = document.createElement("th");
    th.textContent = k;
    tr.append(th);
    tr.insertCell().textContent = v == null ? "" : String(v);
  }
  return t;
}

// The street style as OpenFreeMap sent it, kept while the page lives: a
// change of theme, or switching it on again, asks for nothing more. A
// failure is not kept, so the next try asks again.
let streets: Promise<StyleSpecification> | null = null;
function loadStreets(): Promise<StyleSpecification> {
  if (streets) return streets;
  const stop = new AbortController();
  const timer = window.setTimeout(() => stop.abort(), 20000);
  const asked = fetch(BASEMAP_URL, { credentials: "omit", signal: stop.signal })
    .then((r) => {
      if (!r.ok) throw new Error(`openfreemap answered ${r.status}`);
      return r.json() as Promise<unknown>;
    })
    .then((style) => {
      if (!isStyle(style)) throw new Error("openfreemap did not send a map style");
      return style;
    })
    .finally(() => window.clearTimeout(timer));
  asked.catch(() => {
    if (streets === asked) streets = null;
  });
  streets = asked;
  return asked;
}

// Map ids by order of appearance: handles can differ only in characters
// an id cannot hold.
const ids = new Map<string, string>();
const id = (handle: string) => {
  let v = ids.get(handle);
  if (!v) {
    v = `l-${ids.size + 1}`;
    ids.set(handle, v);
  }
  return v;
};

type LngLat = [number, number];

// Every [longitude, latitude] pair in a GeoJSON geometry.
function eachPosition(g: GeoJSON.Geometry | null | undefined, out: LngLat[]): LngLat[] {
  if (!g) return out;
  if (g.type === "GeometryCollection") {
    for (const x of g.geometries) eachPosition(x, out);
    return out;
  }
  const walk = (c: unknown): void => {
    if (Array.isArray(c) && typeof c[0] === "number") out.push([c[0] as number, c[1] as number]);
    else if (Array.isArray(c)) c.forEach(walk);
  };
  walk(g.coordinates);
  return out;
}

/** Where the map looks: the part of it nothing covers (its padding left
 * out), as west, south, east, north, and its zoom. Null while the map has
 * no size (its window or panel is not on show). */
export interface Seen {
  extent: Extent;
  zoom: number;
}
function partInView(m: maplibregl.Map): Seen | null {
  const c = m.getContainer();
  const p = m.getPadding();
  const x0 = p.left || 0;
  const y0 = p.top || 0;
  const x1 = c.clientWidth - (p.right || 0);
  const y1 = c.clientHeight - (p.bottom || 0);
  if (!(x1 - x0 >= 8 && y1 - y0 >= 8)) return null;
  // All four corners: a turned map's view is not a box of its own.
  const at = ([x, y]: [number, number]) => m.unproject([x, y]);
  const corners = [at([x0, y0]), at([x1, y0]), at([x1, y1]), at([x0, y1])];
  const lngs = corners.map((q) => q.lng);
  const lats = corners.map((q) => q.lat);
  const extent = normalExtent(Math.min(...lngs), Math.min(...lats), Math.max(...lngs), Math.max(...lats));
  return extent ? { extent, zoom: m.getZoom() } : null;
}

const MEASURE = "measure";
const EMPTY: GeoJSON.FeatureCollection = { type: "FeatureCollection", features: [] };

// The clicked row or shape.
const SELECTED: ExpressionSpecification = ["boolean", ["feature-state", "selected"], false];

/** How one of the session's layers is drawn. The active output: stronger
 * fills, thicker lines, larger points with a ring. The selected feature:
 * larger again, as a highlight. */
function looks(l: MapLayer, ring: string) {
  const a = l.active;
  const fill: FillLayerSpecification["paint"] = {
    "fill-color": l.color,
    "fill-opacity": ["case", SELECTED, a ? 0.6 : 0.46, a ? 0.32 : 0.14],
  };
  const line: LineLayerSpecification["paint"] = {
    "line-color": l.color,
    "line-width": ["case", SELECTED, a ? 4.5 : 3.5, a ? 2.4 : 1.4],
  };
  const point: CircleLayerSpecification["paint"] = {
    "circle-color": l.color,
    "circle-radius": ["case", SELECTED, a ? 8 : 7, a ? 5 : 3.5],
    "circle-stroke-color": ring,
    "circle-stroke-width": ["case", SELECTED, 2.5, a ? 1.5 : 0.75],
  };
  return { "-fill": fill, "-line": line, "-point": point } as Record<string, Record<string, unknown>>;
}

/** The session's layers on a MapLibre map that fills the page, with
 * identify on click, the table's selected row highlighted (and zoomed to,
 * when asked), and a geodesic measure tool. */
export function MapView(props: {
  /** The session's layers on the map, bottom first. */
  layers: MapLayer[];
  theme: Theme;
  /** The street background, when the person turned it on. */
  basemap: boolean;
  /** How much of the map is covered at each edge (the app's panel or the
   * page's own, the table sheet): every "zoom to" centres in the rest. */
  padding: Inset;
  fitTo: string | null;
  selected: { handle: string; feature: number } | null;
  /** A table row to zoom to; `n` changes for each click. */
  zoomTo: { handle: string; feature: number; n: number } | null;
  measuring: boolean;
  onMeasuring: (on: boolean) => void;
  /** What the page has to say, shown with the measure result at the foot
   * of the part of the map in view. */
  notice?: ReactNode;
  onIdentify: (handle: string, feature: number) => void;
  onBasemapFailed: () => void;
  /** Web layers turned on, bottom first. */
  webLayers?: WebDraw[];
  /** A raster web layer chosen as the basemap. */
  webBase?: WebDraw | null;
  /** A web layer to zoom to; `n` changes for each click. */
  webFit?: { id: string; n: number } | null;
  onWebState?: (id: string, s: WebState) => void;
  /** An extent to zoom to (west, south, east, north), once per `key`. */
  viewFit?: { extent: Extent; key: string } | null;
  /** A zoom asked for with `fitTo` or `viewFit` has ended (the map has
   * stopped): its `fitTo`, or its `key`. */
  onFitted?: (key: string) => void;
  /** The map moved (it has stopped). */
  onMoved?: () => void;
  /** Filled in by the map: where it looks now (see `Seen`). */
  seenNow?: MutableRefObject<(() => Seen | null) | null>;
}) {
  const box = useRef<HTMLDivElement>(null);
  const map = useRef<maplibregl.Map | null>(null);
  const [ready, setReady] = useState(0);
  const bound = useRef(new Set<string>());
  const failed = useRef(false);
  // Whether the current style has loaded, so sources and layers can be
  // added. Not `isStyleLoaded()`: with a basemap that stays false while its
  // icons and tiles load, long after the style itself is ready.
  const styled = useRef(false);
  // The style the map holds or is loading: `blank:<theme>` or
  // `streets:<theme>`.
  const styleKey = useRef(`blank:${props.theme}`);
  // Changes of style asked for, and the one the map's style answers. Until
  // they agree, `data-layers` stays at 0.
  const asked = useRef(0);
  const held = useRef(0);
  const onBasemapFailed = useRef(props.onBasemapFailed);
  onBasemapFailed.current = props.onBasemapFailed;
  const onIdentify = useRef(props.onIdentify);
  onIdentify.current = props.onIdentify;
  const onWebState = useRef(props.onWebState);
  onWebState.current = props.onWebState;
  const onMeasuring = useRef(props.onMeasuring);
  onMeasuring.current = props.onMeasuring;
  const onFitted = useRef(props.onFitted);
  onFitted.current = props.onFitted;
  const onMoved = useRef(props.onMoved);
  onMoved.current = props.onMoved;
  const padding = useRef(props.padding);
  padding.current = props.padding;
  const layersNow = useRef(props.layers);
  layersNow.current = props.layers;
  // The last "zoom to" (a layer, a row, a web layer), to do again when a
  // sheet opens or closes: what was asked for stays fitted in the part of
  // the map in view. Forgotten once the person moves the map themselves.
  const again = useRef<(() => void) | null>(null);
  const fit = (m: maplibregl.Map, bounds: [[number, number], [number, number]], options: maplibregl.FitBoundsOptions) => {
    again.current = () => m.fitBounds(bounds, options);
    again.current();
  };
  // The same, and word when the map has stopped there. Asked after the
  // zoom starts, not before: starting one ends the move under way, and
  // that end is not this zoom's. A zoom that is a jump (reduced motion) has
  // ended by the time it returns.
  const fitThen = (m: maplibregl.Map, bounds: [[number, number], [number, number]], options: maplibregl.FitBoundsOptions, key: string) => {
    fit(m, bounds, options);
    const done = () => onFitted.current?.(key);
    if (m.isMoving()) m.once("moveend", done);
    else done();
  };
  // The padding the map is to take, and whether it has had one yet.
  const padWanted = useRef<Inset | null>(null);
  const padSet = useRef(false);
  // The data each of the session's sources was last given.
  const given = useRef(new Map<string, FeatureCollection>());
  // Feature layers drawn now, and their requests in flight.
  const webFeatures = useRef<{ sid: string; d: WebDraw }[]>([]);
  const inFlight = useRef(new Map<string, AbortController>());
  const drawnOnce = useRef(new Set<string>());
  const failedOnce = useRef(new Set<string>());
  const refreshTimer = useRef(0);
  // Measure: one line at a time. While measuring, clicks add points
  // instead of identifying; a double-click finishes the line, the next
  // click starts a new one, and Escape clears it.
  const [points, setPoints] = useState<LngLat[]>([]);
  const [finished, setFinished] = useState(false);
  const measuringRef = useRef(false);
  measuringRef.current = props.measuring;
  const finishedRef = useRef(false);
  finishedRef.current = finished;

  // How many of the session's layers the map holds, for the window's own
  // tests: 0 from a change of style until the map shows it.
  const count = () => {
    const m = map.current;
    if (!m || !box.current || held.current !== asked.current) return;
    box.current.dataset.layers = String(layersNow.current.filter((l) => m.getSource(id(l.handle))).length);
  };

  // One map; its style changes with the basemap switch and the theme.
  useEffect(() => {
    if (!box.current) return;
    const m = new maplibregl.Map({
      container: box.current,
      style: blankStyle(props.theme),
      center: [-98.5, 39.5],
      zoom: 3,
      // In view at the foot of the map, not folded away: the map is the
      // whole page now.
      attributionControl: { compact: false },
      // Web layers come from this server only (D-14), with the window's
      // session token in a header, never in the address. Nothing else gets
      // it.
      transformRequest: (url: string) => {
        if (!isWebLayerUrl(url)) return { url };
        const a = authorization();
        return a ? { url, headers: { Authorization: a } } : { url };
      },
    });
    // Bottom right, above the attribution; the scale bottom left.
    m.addControl(new maplibregl.NavigationControl({ showCompass: false }), "bottom-right");
    m.addControl(new maplibregl.ScaleControl({ unit: "imperial" }), "bottom-left");
    m.on("style.load", () => {
      // A new style holds none of the session's or the web layers' sources
      // yet.
      given.current.clear();
      drawnOnce.current.clear();
      failedOnce.current.clear();
      styled.current = true;
      setReady((n) => n + 1);
    });
    // The person moved the map (a drag, the wheel, the keyboard, the zoom
    // buttons: moves that carry their event): the view is theirs now.
    m.on("movestart", (e: maplibregl.MapLibreEvent<unknown>) => {
      if (e.originalEvent) again.current = null;
    });
    // Where the map is looking, for the window's own tests.
    m.on("moveend", () => {
      const b = m.getBounds();
      if (box.current) box.current.dataset.view = [b.getWest(), b.getSouth(), b.getEast(), b.getNorth()].join(",");
      scheduleFeatures();
      onMoved.current?.();
    });
    if (props.seenNow) props.seenNow.current = () => partInView(m);
    // A web layer's first tile: it draws.
    m.on("sourcedata", (e: maplibregl.MapSourceDataEvent) => {
      if (!isWeb(e.sourceId) || !e.tile || drawnOnce.current.has(e.sourceId)) return;
      drawnOnce.current.add(e.sourceId);
      failedOnce.current.delete(e.sourceId);
      onWebState.current?.(webIdOf(e.sourceId), { drawn: true, error: null });
    });
    m.on("click", (e: maplibregl.MapMouseEvent) => {
      if (!measuringRef.current) return;
      const p: LngLat = [e.lngLat.lng, e.lngLat.lat];
      if (finishedRef.current) {
        setFinished(false);
        setPoints([p]);
        return;
      }
      setPoints((ps) => {
        // The two clicks of a double-click land on the last point: skip them.
        const last = ps[ps.length - 1];
        if (last) {
          const a = m.project(last);
          if (Math.hypot(a.x - e.point.x, a.y - e.point.y) < 4) return ps;
        }
        return [...ps, p];
      });
    });
    m.on("dblclick", (e: maplibregl.MapMouseEvent) => {
      if (!measuringRef.current) return;
      e.preventDefault();
      setFinished(true);
    });
    m.on("error", (e: maplibregl.ErrorEvent & { sourceId?: string }) => {
      // A web layer's tile failed: say why on its row (R-14 wording from
      // this server), once; the map and the other layers carry on.
      if (isWeb(e.sourceId)) {
        const sid = e.sourceId as string;
        if (failedOnce.current.has(sid)) return;
        failedOnce.current.add(sid);
        const err = e.error as Error & { body?: Blob; status?: number };
        const say = (msg: string) => onWebState.current?.(webIdOf(sid), { error: msg });
        if (err?.body && typeof err.body.text === "function") {
          err.body
            .text()
            .then((t) => say(JSON.parse(t).error || err.message))
            .catch(() => say(err.message || "Some of this layer's tiles could not be loaded."));
        } else say(err?.message || "Some of this layer's tiles could not be loaded.");
        return;
      }
      // Offline, or the basemap host blocked: carry on with the paper
      // alone, so the session's own layers still show.
      if (failed.current || !styleKey.current.startsWith("streets:")) return;
      const fromBasemap = String(e.error?.message || "").includes("openfreemap");
      if (fromBasemap || !m.isStyleLoaded()) {
        failed.current = true;
        styled.current = false;
        const theme: Theme = styleKey.current.endsWith(":dark") ? "dark" : "light";
        styleKey.current = `blank:${theme}`;
        m.setStyle(blankStyle(theme), { diff: false });
        onBasemapFailed.current();
      }
    });
    map.current = m;
    return () => {
      for (const c of inFlight.current.values()) c.abort();
      window.clearTimeout(refreshTimer.current);
      m.remove();
      map.current = null;
      if (props.seenNow) props.seenNow.current = null;
      // Nothing of this map is left for another (React makes two in
      // development).
      again.current = null;
      padSet.current = false;
      styled.current = false;
      styleKey.current = `blank:${props.theme}`;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // The map's padding: what covers its edges (the app's panel or the page's
  // own, the table sheet). When it changes, the map makes room: what it was
  // last asked to zoom to is fitted again into the part now in view; and if
  // the person has moved the map since, the middle of their view stays the
  // middle of that part, as when a table opens under a map in a GIS.
  const pad = props.padding;
  useEffect(() => {
    const m = map.current;
    if (!m) return;
    padWanted.current = { left: pad.left, top: pad.top, right: pad.right, bottom: pad.bottom };
    const apply = () => {
      const p = padWanted.current;
      if (!p || map.current !== m) return;
      const refit = again.current;
      // Their own move under way (a drag): after it.
      if (!refit && m.isMoving()) {
        m.once("moveend", apply);
        return;
      }
      padWanted.current = null;
      if (!padSet.current) {
        padSet.current = true;
        m.jumpTo({ padding: p });
      } else if (refit) {
        // Without a jump: first the new padding with everything drawn left
        // where it is, then the fit, from there.
        const c = m.getContainer();
        const middle: [number, number] = [
          p.left + (c.clientWidth - p.left - p.right) / 2,
          p.top + (c.clientHeight - p.top - p.bottom) / 2,
        ];
        m.jumpTo({ padding: p, center: m.unproject(middle) });
        refit();
      } else m.easeTo({ padding: p, duration: 300 });
    };
    apply();
  }, [pad.left, pad.top, pad.right, pad.bottom]);

  // The style: the paper alone, or the street background in the page's
  // colours. The street style is fetched here and recoloured before the map
  // gets it; if it cannot be had, the paper stays and the page is told.
  const wanted = `${props.basemap ? "streets" : "blank"}:${props.theme}`;
  useEffect(() => {
    const m = map.current;
    if (!m || (wanted === styleKey.current && asked.current === held.current)) return;
    const turn = ++asked.current;
    if (box.current) box.current.dataset.layers = "0";
    const show = (style: StyleSpecification) => {
      held.current = turn;
      failed.current = false;
      if (wanted === styleKey.current) {
        // The map already holds it (a street style that never arrived).
        if (styled.current) count();
        return;
      }
      styleKey.current = wanted;
      styled.current = false;
      // From scratch, not as a difference: the session's layers are put
      // back when it has loaded.
      m.setStyle(style, { diff: false });
    };
    if (!props.basemap) {
      show(blankStyle(props.theme));
      return;
    }
    loadStreets().then(
      (style) => {
        if (turn === asked.current && map.current === m) show(restyle(style, props.theme));
      },
      () => {
        if (turn !== asked.current || map.current !== m) return;
        failed.current = true;
        onBasemapFailed.current();
      },
    );
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [wanted]);

  // Keep the map's sources and layers in step with the list, each in the
  // colour it has now.
  useEffect(() => {
    const m = map.current;
    if (!m || !styled.current) return;
    const ring = LAYER_TONES[props.theme].ring;
    // By name, as `looks` lists them (the same value again changes nothing).
    const repaint = m.setPaintProperty.bind(m) as (layer: string, name: string, value: unknown) => unknown;
    const want = new Set(props.layers.map((l) => id(l.handle)));
    for (const s of Object.keys(m.getStyle().sources || {})) {
      if (s.startsWith("l-") && !want.has(s)) {
        for (const suffix of FEATURE_PARTS) {
          if (m.getLayer(s + suffix)) m.removeLayer(s + suffix);
        }
        m.removeSource(s);
        given.current.delete(s);
      }
    }
    for (const l of props.layers) {
      const sid = id(l.handle);
      const look = looks(l, ring);
      const src = m.getSource(sid) as GeoJSONSource | undefined;
      if (src) {
        if (given.current.get(sid) !== l.data) {
          src.setData(l.data as unknown as GeoJSON.FeatureCollection);
          given.current.set(sid, l.data);
        }
        for (const suffix of FEATURE_PARTS) {
          for (const [k, v] of Object.entries(look[suffix])) repaint(sid + suffix, k, v);
        }
        continue;
      }
      m.addSource(sid, { type: "geojson", data: l.data as unknown as GeoJSON.FeatureCollection });
      given.current.set(sid, l.data);
      m.addLayer({
        id: `${sid}-fill`,
        source: sid,
        type: "fill",
        filter: ["in", ["geometry-type"], ["literal", ["Polygon", "MultiPolygon"]]],
        paint: look["-fill"] as FillLayerSpecification["paint"],
      });
      m.addLayer({
        id: `${sid}-line`,
        source: sid,
        type: "line",
        filter: ["in", ["geometry-type"], ["literal", ["Polygon", "MultiPolygon", "LineString", "MultiLineString"]]],
        layout: { "line-cap": "round", "line-join": "round" },
        paint: look["-line"] as LineLayerSpecification["paint"],
      });
      m.addLayer({
        id: `${sid}-point`,
        source: sid,
        type: "circle",
        filter: ["in", ["geometry-type"], ["literal", ["Point", "MultiPoint"]]],
        paint: look["-point"] as CircleLayerSpecification["paint"],
      });
      for (const suffix of FEATURE_PARTS) {
        // Handlers belong to the map, not the style: bind once per layer.
        if (bound.current.has(sid + suffix)) continue;
        bound.current.add(sid + suffix);
        m.on("click", sid + suffix, (e: maplibregl.MapLayerMouseEvent) => {
          if (measuringRef.current) return;
          const f = e.features?.[0];
          if (f && typeof f.id === "number") onIdentify.current(l.handle, f.id);
        });
        m.on("mouseenter", sid + suffix, () => {
          if (!measuringRef.current) m.getCanvas().style.cursor = "pointer";
        });
        m.on("mouseleave", sid + suffix, () => {
          if (!measuringRef.current) m.getCanvas().style.cursor = "";
        });
      }
    }
    // Fills under lines under points, whichever output they belong to, in
    // the list's order with the active output on top of its kind.
    const stack = [...props.layers].sort((a, b) => Number(a.active) - Number(b.active));
    const order = FEATURE_PARTS.flatMap((suffix) => stack.map((l) => id(l.handle) + suffix));
    const have = m.getLayersOrder().filter((x) => x.startsWith("l-"));
    if (order.length !== have.length || order.some((x, i) => x !== have[i])) {
      for (const layer of order) if (m.getLayer(layer)) m.moveLayer(layer);
    }
    count();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [props.layers, props.theme, ready]);

  // Feature layers: the view's features, at most the server's limit, again
  // after each move (a moment after it stops).
  function scheduleFeatures() {
    window.clearTimeout(refreshTimer.current);
    refreshTimer.current = window.setTimeout(loadFeatures, 250);
  }
  function loadFeatures() {
    const m = map.current;
    if (!m) return;
    const b = m.getBounds();
    let bbox: [number, number, number, number] = [
      Math.max(-180, b.getWest()),
      Math.max(-85.06, b.getSouth()),
      Math.min(180, b.getEast()),
      Math.min(85.06, b.getNorth()),
    ];
    if (!(bbox[0] < bbox[2] && bbox[1] < bbox[3])) bbox = [-180, -85.06, 180, 85.06];
    for (const { sid, d } of webFeatures.current) {
      inFlight.current.get(sid)?.abort();
      const ctrl = new AbortController();
      inFlight.current.set(sid, ctrl);
      webApi.features(d.id, bbox, ctrl.signal).then(
        (fc) => {
          if (ctrl.signal.aborted) return;
          inFlight.current.delete(sid);
          const src = m.getSource(sid) as GeoJSONSource | undefined;
          if (!src) return;
          src.setData(fc as unknown as GeoJSON.FeatureCollection);
          onWebState.current?.(d.id, { error: null, drawn: true, total: fc.total, shown: fc.features.length, exceeded: fc.exceeded });
        },
        (e) => {
          if (ctrl.signal.aborted || (e as Error).name === "AbortError") return;
          inFlight.current.delete(sid);
          const src = m.getSource(sid) as GeoJSONSource | undefined;
          src?.setData(EMPTY);
          onWebState.current?.(d.id, { error: e instanceof WebLayerError ? e.message : String((e as Error).message || e), shown: 0 });
        },
      );
    }
  }

  // Keep the web layers on the map in step with the list: the basemap one
  // at the bottom, the others above it in order, the session's own layers
  // and the measured line above them all.
  useEffect(() => {
    const m = map.current;
    if (!m || !styled.current) return;
    const want = new Map<string, { d: WebDraw; base: boolean }>();
    if (props.webBase) want.set(webSid(props.webBase, true), { d: props.webBase, base: true });
    for (const d of props.webLayers || []) want.set(webSid(d, false), { d, base: false });
    for (const l of [...(m.getStyle().layers || [])]) {
      const src = (l as { source?: string }).source;
      if (isWeb(src) && !want.has(src as string)) m.removeLayer(l.id);
    }
    for (const s of Object.keys(m.getStyle().sources || {})) {
      if (isWeb(s) && !want.has(s)) {
        inFlight.current.get(s)?.abort();
        inFlight.current.delete(s);
        drawnOnce.current.delete(s);
        failedOnce.current.delete(s);
        m.removeSource(s);
      }
    }
    const features: { sid: string; d: WebDraw }[] = [];
    let added = false;
    for (const [sid, { d, base }] of want) {
      const info = d.info;
      const ex = info.extent;
      const bounds = ex && ex[0] < ex[2] && ex[1] < ex[3] ? (ex as [number, number, number, number]) : undefined;
      if (info.raster && info.tiles) {
        if (!m.getSource(sid)) {
          m.addSource(sid, {
            type: "raster",
            tiles: [location.origin + info.tiles],
            tileSize: info.tile_size || 256,
            minzoom: info.min_zoom ?? 0,
            maxzoom: info.max_zoom ?? 22,
            ...(bounds ? { bounds } : {}),
            ...(info.attribution ? { attribution: info.attribution } : {}),
          });
          m.addLayer({ id: sid, type: "raster", source: sid });
        }
        m.setPaintProperty(sid, "raster-opacity", base ? 1 : d.opacity);
      } else if (info.features) {
        if (!m.getSource(sid)) {
          m.addSource(sid, { type: "geojson", data: EMPTY, ...(info.attribution ? { attribution: info.attribution } : {}) });
          m.addLayer({
            id: `${sid}-fill`,
            source: sid,
            type: "fill",
            filter: ["in", ["geometry-type"], ["literal", ["Polygon", "MultiPolygon"]]],
            paint: { "fill-color": WEB_COLOR },
          });
          m.addLayer({
            id: `${sid}-line`,
            source: sid,
            type: "line",
            filter: ["in", ["geometry-type"], ["literal", ["Polygon", "MultiPolygon", "LineString", "MultiLineString"]]],
            paint: { "line-color": WEB_COLOR, "line-width": 1.2 },
          });
          m.addLayer({
            id: `${sid}-point`,
            source: sid,
            type: "circle",
            filter: ["in", ["geometry-type"], ["literal", ["Point", "MultiPoint"]]],
            paint: { "circle-color": WEB_COLOR, "circle-radius": 3.5, "circle-stroke-color": "#ffffff", "circle-stroke-width": 0.8 },
          });
          added = true;
          // A feature's attributes on click (hidden fields were removed by
          // the server).
          for (const part of FEATURE_PARTS) {
            if (bound.current.has(sid + part)) continue;
            bound.current.add(sid + part);
            m.on("click", sid + part, (e: maplibregl.MapLayerMouseEvent) => {
              if (measuringRef.current) return;
              const f = e.features?.[0];
              if (!f) return;
              // Clear of whatever covers the map's edges.
              const p = padding.current;
              new maplibregl.Popup({
                maxWidth: "320px",
                padding: { left: p.left + 8, top: p.top + 8, right: p.right + 8, bottom: p.bottom + 8 },
              })
                .setLngLat(e.lngLat)
                .setDOMContent(attributeTable((f.properties || {}) as Record<string, unknown>))
                .addTo(m);
            });
          }
        }
        m.setPaintProperty(`${sid}-fill`, "fill-opacity", d.opacity * 0.35);
        m.setPaintProperty(`${sid}-line`, "line-opacity", d.opacity);
        m.setPaintProperty(`${sid}-point`, "circle-opacity", d.opacity);
        m.setPaintProperty(`${sid}-point`, "circle-stroke-opacity", d.opacity);
        features.push({ sid, d });
      }
    }
    // Order: the basemap layer just above the background; the others, bottom
    // first, below the session's own layers.
    const layerIds = (sid: string, d: WebDraw) => (d.info.raster ? [sid] : FEATURE_PARTS.map((p) => sid + p));
    if (props.webBase) {
      const sid = webSid(props.webBase, true);
      const above = (m.getStyle().layers || []).find((l) => l.id !== "background" && l.id !== sid);
      for (const lid of layerIds(sid, props.webBase)) if (m.getLayer(lid)) m.moveLayer(lid, above?.id);
    }
    const firstOwn = (m.getStyle().layers || []).find((l) => l.id.startsWith("l-") || l.id.startsWith(MEASURE))?.id;
    for (const d of props.webLayers || []) {
      for (const lid of layerIds(webSid(d, false), d)) if (m.getLayer(lid)) m.moveLayer(lid, firstOwn);
    }
    webFeatures.current = features;
    if (added) loadFeatures();
    // What the map holds, bottom first, with each layer's opacity: for the
    // window's own tests.
    if (box.current) {
      const held = (m.getStyle().layers || [])
        .filter((l) => isWeb((l as { source?: string }).source) && !l.id.endsWith("-fill") && !l.id.endsWith("-line"))
        .map((l) => {
          const sid = (l as { source: string }).source;
          const op = l.type === "raster" ? m.getPaintProperty(l.id, "raster-opacity") : m.getPaintProperty(l.id, "circle-opacity");
          return { id: webIdOf(sid), base: sid.startsWith("wb~"), opacity: typeof op === "number" ? op : 1 };
        });
      box.current.dataset.web = JSON.stringify(held);
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [props.webLayers, props.webBase, ready, props.layers]);

  // Zoom to a web layer's extent, once per click.
  const webFitted = useRef<number | null>(null);
  useEffect(() => {
    const m = map.current;
    const f = props.webFit;
    if (!m || !f || webFitted.current === f.n) return;
    webFitted.current = f.n;
    const d = [...(props.webLayers || []), ...(props.webBase ? [props.webBase] : [])].find((x) => x.id === f.id);
    const ex = d?.info.extent;
    if (!ex) return;
    fit(
      m,
      [
        [Math.max(-180, ex[0]), Math.max(-85, ex[1])],
        [Math.min(180, ex[2]), Math.min(85, ex[3])],
      ],
      { padding: 30, maxZoom: 18, duration: 500 },
    );
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [props.webFit]);

  // Zoom to a layer when asked, once per request (`handle` or
  // `handle#<anything>` to ask again), as soon as its data is here.
  const fitted = useRef<string | null>(null);
  useEffect(() => {
    const m = map.current;
    if (!props.fitTo || fitted.current === props.fitTo) return;
    const target = props.fitTo.split("#")[0];
    const l = props.layers.find((x) => x.handle === target);
    const b = l?.data.bbox;
    if (m && b) {
      fitted.current = props.fitTo;
      fitThen(
        m,
        [
          [b[0], b[1]],
          [b[2], b[3]],
        ],
        { padding: 40, maxZoom: 16, duration: 600 },
        props.fitTo,
      );
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [props.fitTo, props.layers]);

  // Zoom to an extent when asked (the assistant's `map_view`), once per
  // key. Written after the zoom to a layer on purpose: when both are asked
  // for together, this one is the later and stands.
  const viewFitted = useRef<string | null>(null);
  useEffect(() => {
    const m = map.current;
    const f = props.viewFit;
    if (!m || !f || viewFitted.current === f.key) return;
    viewFitted.current = f.key;
    const [w, s, e, n] = f.extent;
    // Within the map's own world; an extent wholly past its edge is
    // nothing to zoom to.
    const south = Math.min(85, Math.max(-85, s));
    const north = Math.min(85, Math.max(-85, n));
    if (!(south < north)) return;
    fitThen(
      m,
      [
        [w, south],
        [e, north],
      ],
      { padding: 24, maxZoom: 19, duration: 500 },
      f.key,
    );
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [props.viewFit]);

  // Highlight the selected feature.
  const last = useRef<{ source: string; id: number } | null>(null);
  useEffect(() => {
    const m = map.current;
    if (!m || !styled.current) return;
    if (last.current && m.getSource(last.current.source)) {
      m.setFeatureState(last.current, { selected: false });
    }
    last.current = null;
    const s = props.selected;
    if (s && m.getSource(id(s.handle))) {
      const ref = { source: id(s.handle), id: s.feature };
      m.setFeatureState(ref, { selected: true });
      last.current = ref;
    }
  }, [props.selected, ready, props.layers]);

  // Zoom to the table's selected row (REQ-28), once per click. A row the
  // map does not hold (its layer off the map or cut off at its limit) or
  // without geometry leaves the view as it is.
  const zoomed = useRef<number | null>(null);
  useEffect(() => {
    const m = map.current;
    const z = props.zoomTo;
    if (!m || !z || zoomed.current === z.n) return;
    zoomed.current = z.n;
    const l = props.layers.find((x) => x.handle === z.handle);
    if (!l) return;
    const f = l.data.features.find((x) => x.id === z.feature);
    const pts = eachPosition(f?.geometry, []);
    if (!pts.length) return;
    let [w, s, e, n] = [pts[0][0], pts[0][1], pts[0][0], pts[0][1]];
    for (const [x, y] of pts) {
      w = Math.min(w, x);
      s = Math.min(s, y);
      e = Math.max(e, x);
      n = Math.max(n, y);
    }
    fit(
      m,
      [
        [w, s],
        [e, n],
      ],
      { padding: 60, maxZoom: 18, duration: 500 },
    );
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [props.zoomTo]);

  // The measured line, drawn over the session's layers in the page's ink.
  useEffect(() => {
    const m = map.current;
    if (!m || !styled.current) return;
    const tones = LAYER_TONES[props.theme];
    const data: GeoJSON.FeatureCollection = points.length
      ? {
          type: "FeatureCollection",
          features: [
            ...(points.length > 1
              ? [{ type: "Feature" as const, properties: {}, geometry: { type: "LineString" as const, coordinates: points } }]
              : []),
            ...points.map((p) => ({ type: "Feature" as const, properties: {}, geometry: { type: "Point" as const, coordinates: p } })),
          ],
        }
      : EMPTY;
    const src = m.getSource(MEASURE) as GeoJSONSource | undefined;
    if (src) {
      src.setData(data);
      m.setPaintProperty(`${MEASURE}-line`, "line-color", tones.ink);
      m.setPaintProperty(`${MEASURE}-point`, "circle-color", tones.panel);
      m.setPaintProperty(`${MEASURE}-point`, "circle-stroke-color", tones.ink);
    } else {
      m.addSource(MEASURE, { type: "geojson", data });
      m.addLayer({
        id: `${MEASURE}-line`,
        source: MEASURE,
        type: "line",
        filter: ["==", ["geometry-type"], "LineString"],
        paint: { "line-color": tones.ink, "line-width": 2.5, "line-dasharray": [2, 1.5] },
      });
      m.addLayer({
        id: `${MEASURE}-point`,
        source: MEASURE,
        type: "circle",
        filter: ["==", ["geometry-type"], "Point"],
        paint: { "circle-color": tones.panel, "circle-radius": 4, "circle-stroke-color": tones.ink, "circle-stroke-width": 2 },
      });
    }
    // Keep it on top of layers added since.
    m.moveLayer(`${MEASURE}-line`);
    m.moveLayer(`${MEASURE}-point`);
  }, [points, ready, props.layers, props.theme]);

  // Each measurement starts clean; ending one clears its line.
  useEffect(() => {
    setPoints((ps) => (ps.length ? [] : ps));
    setFinished(false);
  }, [props.measuring]);

  // Measuring: a crosshair, no double-click zoom, and Escape clears.
  useEffect(() => {
    const m = map.current;
    if (!m) return;
    if (props.measuring) {
      m.doubleClickZoom.disable();
      m.getCanvas().style.cursor = "crosshair";
    } else {
      m.doubleClickZoom.enable();
      m.getCanvas().style.cursor = "";
    }
    if (!props.measuring && !points.length) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "Escape") return;
      onMeasuring.current(false);
      setPoints([]);
      setFinished(false);
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [props.measuring, points.length]);

  const length = points.length > 1 ? formatLength(lineMetres(points)) : null;

  return (
    <div className="map-wrap">
      <div className="map" ref={box} role="region" aria-label="Map of the session's layers" />
      <div className="map-foot">
        {props.notice}
        {(props.measuring || length) && (
          <span className="measure-result" role="status">
            {length ? (
              <>
                <strong>{length.feet}</strong> ({length.metres})
              </>
            ) : (
              <span className="muted">Click points on the map; double-click to finish; Esc clears.</span>
            )}
          </span>
        )}
      </div>
    </div>
  );
}
