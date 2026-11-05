import { useEffect, useRef, useState } from "react";
import * as maplibregl from "maplibre-gl";
import type { ExpressionSpecification, GeoJSONSource, StyleSpecification } from "maplibre-gl";
import "maplibre-gl/dist/maplibre-gl.css";
// MapLibre finds its worker next to its own file, which a bundle does not
// have; give it the built copy, served from this server like the rest.
import workerUrl from "maplibre-gl/dist/maplibre-gl-worker.mjs?url";

maplibregl.setWorkerUrl(workerUrl);
import type { FeatureCollection } from "./api";

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

/** The session's layers on a MapLibre map, with identify on click and the
 * table's selected row highlighted. */
export function MapView(props: {
  layers: MapLayer[];
  basemap: boolean;
  fitTo: string | null;
  selected: { handle: string; feature: number } | null;
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
          const f = e.features?.[0];
          if (f && typeof f.id === "number") onIdentify.current(l.handle, f.id);
        });
        m.on("mouseenter", sid + suffix, () => (m.getCanvas().style.cursor = "pointer"));
        m.on("mouseleave", sid + suffix, () => (m.getCanvas().style.cursor = ""));
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

  return <div className="map" ref={box} role="region" aria-label="Map of the session's layers" />;
}
