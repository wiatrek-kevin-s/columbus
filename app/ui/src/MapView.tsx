import { useEffect, useRef, useState, type ReactNode } from "react";
import * as maplibregl from "maplibre-gl";
import type { ExpressionSpecification, GeoJSONSource, StyleSpecification } from "maplibre-gl";
import "maplibre-gl/dist/maplibre-gl.css";
// MapLibre finds its worker next to its own file, which a bundle does not
// have; give it the built copy, served from this server like the rest.
import workerUrl from "maplibre-gl/dist/maplibre-gl-worker.mjs?url";

maplibregl.setWorkerUrl(workerUrl);
import { authorization, isWebLayerUrl, webApi, WebLayerError, type FeatureCollection, type WebSourceInfo } from "./api";
import { formatLength, lineMetres } from "./measure";

export interface MapLayer {
  handle: string;
  name: string;
  color: string;
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

// OpenFreeMap: free vector basemaps, no key. The page's policy allows this
// one host and nothing else on the internet; without it the map is blank.
const BASEMAP = "https://tiles.openfreemap.org/styles/positron";
const BLANK: StyleSpecification = {
  version: 8,
  sources: {},
  layers: [{ id: "background", type: "background", paint: { "background-color": "#eef2f4" } }],
};

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

const MEASURE = "measure";
const EMPTY: GeoJSON.FeatureCollection = { type: "FeatureCollection", features: [] };

/** The session's layers on a MapLibre map, with identify on click, the
 * table's selected row highlighted (and zoomed to, when asked), and a
 * geodesic measure tool. */
export function MapView(props: {
  layers: MapLayer[];
  basemap: boolean;
  fitTo: string | null;
  selected: { handle: string; feature: number } | null;
  /** A table row to zoom to; `n` changes for each click. */
  zoomTo: { handle: string; feature: number; n: number } | null;
  zoomToSelection: boolean;
  onZoomToSelection: (on: boolean) => void;
  /** Controls the page puts first in the map's bar. */
  tools?: ReactNode;
  onIdentify: (handle: string, feature: number) => void;
  onBasemapFailed: () => void;
  /** Web layers turned on, bottom first. */
  webLayers?: WebDraw[];
  /** A raster web layer chosen as the basemap. */
  webBase?: WebDraw | null;
  /** A web layer to zoom to; `n` changes for each click. */
  webFit?: { id: string; n: number } | null;
  onWebState?: (id: string, s: WebState) => void;
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
  const onBasemapFailed = useRef(props.onBasemapFailed);
  onBasemapFailed.current = props.onBasemapFailed;
  const onIdentify = useRef(props.onIdentify);
  onIdentify.current = props.onIdentify;
  const onWebState = useRef(props.onWebState);
  onWebState.current = props.onWebState;
  // Feature layers drawn now, and their requests in flight.
  const webFeatures = useRef<{ sid: string; d: WebDraw }[]>([]);
  const inFlight = useRef(new Map<string, AbortController>());
  const drawnOnce = useRef(new Set<string>());
  const failedOnce = useRef(new Set<string>());
  const refreshTimer = useRef(0);
  // Measure: one line at a time. While measuring, clicks add points
  // instead of identifying; a double-click finishes the line, the next
  // click starts a new one, and Escape clears it.
  const [measuring, setMeasuring] = useState(false);
  const [points, setPoints] = useState<LngLat[]>([]);
  const [finished, setFinished] = useState(false);
  const measuringRef = useRef(false);
  measuringRef.current = measuring;
  const finishedRef = useRef(false);
  finishedRef.current = finished;

  // One map; its style changes with the basemap switch.
  useEffect(() => {
    if (!box.current) return;
    const m = new maplibregl.Map({
      container: box.current,
      style: props.basemap ? BASEMAP : BLANK,
      center: [-98.5, 39.5],
      zoom: 3,
      attributionControl: { compact: true },
      // Web layers come from this server only (D-14), with the window's
      // session token in a header, never in the address. Nothing else gets
      // it.
      transformRequest: (url: string) => {
        if (!isWebLayerUrl(url)) return { url };
        const a = authorization();
        return a ? { url, headers: { Authorization: a } } : { url };
      },
    });
    m.addControl(new maplibregl.NavigationControl({ showCompass: false }), "top-right");
    m.addControl(new maplibregl.ScaleControl({ unit: "imperial" }), "bottom-left");
    m.on("style.load", () => {
      // A new style holds none of the web layers' sources yet.
      drawnOnce.current.clear();
      failedOnce.current.clear();
      styled.current = true;
      setReady((n) => n + 1);
    });
    // Where the map is looking, for the window's own tests.
    m.on("moveend", () => {
      const b = m.getBounds();
      if (box.current) box.current.dataset.view = [b.getWest(), b.getSouth(), b.getEast(), b.getNorth()].join(",");
      scheduleFeatures();
    });
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
      // Offline, or the basemap host blocked: carry on with a plain
      // background, so the session's own layers still show.
      if (failed.current) return;
      const fromBasemap = String(e.error?.message || "").includes("openfreemap");
      if (fromBasemap || !m.isStyleLoaded()) {
        failed.current = true;
        styled.current = false;
        m.setStyle(BLANK);
        onBasemapFailed.current();
      }
    });
    map.current = m;
    return () => {
      for (const c of inFlight.current.values()) c.abort();
      window.clearTimeout(refreshTimer.current);
      m.remove();
      map.current = null;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const first = useRef(true);
  useEffect(() => {
    if (first.current) {
      first.current = false;
      return;
    }
    failed.current = false;
    styled.current = false;
    if (box.current) box.current.dataset.layers = "0";
    map.current?.setStyle(props.basemap ? BASEMAP : BLANK);
  }, [props.basemap]);

  // Keep the map's sources and layers in step with the list.
  useEffect(() => {
    const m = map.current;
    if (!m || !styled.current) return;
    const want = new Set(props.layers.map((l) => id(l.handle)));
    for (const s of Object.keys(m.getStyle().sources || {})) {
      if (s.startsWith("l-") && !want.has(s)) {
        for (const suffix of ["-fill", "-line", "-point"]) {
          if (m.getLayer(s + suffix)) m.removeLayer(s + suffix);
        }
        m.removeSource(s);
      }
    }
    for (const l of props.layers) {
      const sid = id(l.handle);
      const src = m.getSource(sid) as GeoJSONSource | undefined;
      if (src) {
        src.setData(l.data as unknown as GeoJSON.FeatureCollection);
        continue;
      }
      m.addSource(sid, { type: "geojson", data: l.data as unknown as GeoJSON.FeatureCollection });
      const hi: ExpressionSpecification = ["boolean", ["feature-state", "selected"], false];
      m.addLayer({
        id: `${sid}-fill`,
        source: sid,
        type: "fill",
        filter: ["in", ["geometry-type"], ["literal", ["Polygon", "MultiPolygon"]]],
        paint: { "fill-color": l.color, "fill-opacity": ["case", hi, 0.55, 0.22] },
      });
      m.addLayer({
        id: `${sid}-line`,
        source: sid,
        type: "line",
        filter: ["in", ["geometry-type"], ["literal", ["Polygon", "MultiPolygon", "LineString", "MultiLineString"]]],
        paint: { "line-color": l.color, "line-width": ["case", hi, 3.5, 1.4] },
      });
      m.addLayer({
        id: `${sid}-point`,
        source: sid,
        type: "circle",
        filter: ["in", ["geometry-type"], ["literal", ["Point", "MultiPoint"]]],
        paint: {
          "circle-color": l.color,
          "circle-radius": ["case", hi, 7, 4],
          "circle-stroke-color": "#ffffff",
          "circle-stroke-width": 1,
        },
      });
      for (const suffix of ["-fill", "-line", "-point"]) {
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
    // How many of the session's layers the map holds, for the window's own
    // tests.
    if (box.current) box.current.dataset.layers = String(props.layers.filter((l) => m.getSource(id(l.handle))).length);
  }, [props.layers, ready]);

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
              new maplibregl.Popup({ maxWidth: "320px" })
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
    m.fitBounds(
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
      m.fitBounds(
        [
          [b[0], b[1]],
          [b[2], b[3]],
        ],
        { padding: 40, maxZoom: 16, duration: 600 },
      );
    }
  }, [props.fitTo, props.layers]);

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
    m.fitBounds(
      [
        [w, s],
        [e, n],
      ],
      { padding: 60, maxZoom: 18, duration: 500 },
    );
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [props.zoomTo]);

  // The measured line, drawn over the session's layers.
  useEffect(() => {
    const m = map.current;
    if (!m || !styled.current) return;
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
    if (src) src.setData(data);
    else {
      m.addSource(MEASURE, { type: "geojson", data });
      m.addLayer({
        id: `${MEASURE}-line`,
        source: MEASURE,
        type: "line",
        filter: ["==", ["geometry-type"], "LineString"],
        paint: { "line-color": "#17324d", "line-width": 2.5, "line-dasharray": [2, 1.5] },
      });
      m.addLayer({
        id: `${MEASURE}-point`,
        source: MEASURE,
        type: "circle",
        filter: ["==", ["geometry-type"], "Point"],
        paint: { "circle-color": "#ffffff", "circle-radius": 4, "circle-stroke-color": "#17324d", "circle-stroke-width": 2 },
      });
    }
    // Keep it on top of layers added since.
    m.moveLayer(`${MEASURE}-line`);
    m.moveLayer(`${MEASURE}-point`);
  }, [points, ready, props.layers]);

  // Measuring: a crosshair, no double-click zoom, and Escape clears.
  useEffect(() => {
    const m = map.current;
    if (!m) return;
    if (measuring) {
      m.doubleClickZoom.disable();
      m.getCanvas().style.cursor = "crosshair";
    } else {
      m.doubleClickZoom.enable();
      m.getCanvas().style.cursor = "";
    }
    if (!measuring && !points.length) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "Escape") return;
      setMeasuring(false);
      setPoints([]);
      setFinished(false);
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [measuring, points.length]);

  const toggleMeasure = () => {
    setMeasuring(!measuring);
    setPoints([]);
    setFinished(false);
  };
  const length = points.length > 1 ? formatLength(lineMetres(points)) : null;

  return (
    <div className="map-wrap">
      <div className="map" ref={box} role="region" aria-label="Map of the session's layers" />
      <div className="map-bar">
        {props.tools}
        <label className="switch" title="Clicking a row in the table zooms the map to that feature">
          <input
            type="checkbox"
            checked={props.zoomToSelection}
            onChange={(e) => props.onZoomToSelection(e.target.checked)}
          />
          Zoom to selection
        </label>
        <button type="button" aria-pressed={measuring} onClick={toggleMeasure}>
          Measure
        </button>
        {(measuring || length) && (
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
