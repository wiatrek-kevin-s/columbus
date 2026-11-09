import { useEffect, useRef, useState, type ReactNode } from "react";
import * as maplibregl from "maplibre-gl";
import type { ExpressionSpecification, GeoJSONSource, StyleSpecification } from "maplibre-gl";
import "maplibre-gl/dist/maplibre-gl.css";
// MapLibre finds its worker next to its own file, which a bundle does not
// have; give it the built copy, served from this server like the rest.
import workerUrl from "maplibre-gl/dist/maplibre-gl-worker.mjs?url";

maplibregl.setWorkerUrl(workerUrl);
import type { FeatureCollection } from "./api";
import { formatLength, lineMetres } from "./measure";

export interface MapLayer {
  handle: string;
  name: string;
  color: string;
  data: FeatureCollection;
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
    });
    m.addControl(new maplibregl.NavigationControl({ showCompass: false }), "top-right");
    m.addControl(new maplibregl.ScaleControl({ unit: "imperial" }), "bottom-left");
    m.on("style.load", () => {
      styled.current = true;
      setReady((n) => n + 1);
    });
    // Where the map is looking, for the window's own tests.
    m.on("moveend", () => {
      const b = m.getBounds();
      if (box.current) box.current.dataset.view = [b.getWest(), b.getSouth(), b.getEast(), b.getNorth()].join(",");
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
    m.on("error", (e: maplibregl.ErrorEvent) => {
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
