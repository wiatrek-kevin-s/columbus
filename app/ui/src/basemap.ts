// The street background: OpenFreeMap's positron style, recoloured to the
// page's paper and ink so that the session's data stays the only colour.
//
// `restyle` is pure (the style it is given is never changed) and careful:
// it reads a style by each layer's `type` and `source-layer` (the
// OpenMapTiles names), not by exact layer ids, changes colours only, and on
// any surprise hands the style back as it came. It has no imports at run
// time, so `e2e/unit.mjs` can check it without a browser.

import type { StyleSpecification } from "maplibre-gl";

export type Theme = "light" | "dark";

// OpenFreeMap: free vector basemaps, no key. The page's policy allows this
// one host and nothing else on the internet; without it the map is blank.
export const BASEMAP_URL = "https://tiles.openfreemap.org/styles/positron";

export interface MapTones {
  /** The map's background, and the land. */
  paper: string;
  /** Land use and land cover: a whisper off the paper. */
  land: string;
  building: string;
  water: string;
  road: string;
  /** Streets, service roads, tracks and paths. */
  minor: string;
  /** The line under a road; null draws none. */
  casing: string | null;
  rail: string;
  label: string;
  halo: string;
  boundary: string;
}

/** The map's own colours. `paper`, `label` (mute) and `boundary` (line2)
 * are the page's tokens in styles.css; `e2e/unit.mjs` checks they agree. */
export const MAP_TONES: Record<Theme, MapTones> = {
  light: {
    paper: "#efede8",
    land: "#e9e6e0",
    building: "#e6e3dc",
    water: "#cadce8",
    road: "#ffffff",
    minor: "#ffffff",
    casing: "#dfdbd3",
    rail: "#d8d4cc",
    label: "#66676b",
    halo: "#efede8",
    boundary: "#dfdeda",
  },
  dark: {
    paper: "#121315",
    land: "#17181b",
    building: "#1a1b1e",
    water: "#0b1017",
    road: "#2b2e33",
    minor: "#1c1e21",
    casing: null,
    rail: "#24262a",
    label: "#9c9c9a",
    halo: "#121315",
    boundary: "#393a3f",
  },
};

/** The map with no basemap: the paper alone. */
export function blankStyle(theme: Theme): StyleSpecification {
  return {
    version: 8,
    sources: {},
    layers: [{ id: "background", type: "background", paint: { "background-color": MAP_TONES[theme].paper } }],
  };
}

type Json = Record<string, unknown>;

const isObject = (v: unknown): v is Json => !!v && typeof v === "object" && !Array.isArray(v);

// OpenMapTiles source layers drawn as land: quiet fills, a whisper off the
// paper.
const LAND = new Set(["landuse", "landcover", "park", "aeroway", "globallandcover"]);
// Classes of `transportation` that are not roads: drawn as a quiet line.
const RAILS = new Set(["rail", "transit", "cable_car", "gondola", "ferry"]);
const MINOR = new Set(["minor", "service", "track", "path", "pedestrian", "raceway", "busway", "bus_guideway"]);

/** A layer's `paint` or `layout`. Anything but an object (or nothing) is a
 * surprise. */
function part(layer: Json, key: "paint" | "layout", make: boolean): Json {
  const v = layer[key];
  if (v === undefined) {
    const made: Json = {};
    if (make) layer[key] = made;
    return made;
  }
  if (!isObject(v)) throw new Error(`a layer's ${key} is not an object`);
  return v;
}

function fill(layer: Json, color: string): void {
  const p = part(layer, "paint", true);
  p["fill-color"] = color;
  // No outline in the old colour (without one the edge takes the fill's),
  // and no picture in place of the colour.
  delete p["fill-outline-color"];
  delete p["fill-pattern"];
}

function line(layer: Json, color: string): void {
  const p = part(layer, "paint", true);
  p["line-color"] = color;
  delete p["line-pattern"];
}

/** The property a filter tests: `"class"` or `["get", "class"]`. */
function keyOf(v: unknown): string | null {
  if (typeof v === "string") return v;
  if (Array.isArray(v) && v.length === 2 && v[0] === "get" && typeof v[1] === "string") return v[1];
  return null;
}

/** The classes a filter asks for (not the ones it rules out), for the
 * usual forms: `["==", "class", "rail"]`, `["in", "class", "minor",
 * "service"]`, the same with `["get", "class"]` and `["literal", [...]]`,
 * `["match", ["get", "class"], [...], true, false]`, under `all` and
 * `any`. A form it does not know adds nothing. */
function classes(filter: unknown, out: Set<string>): Set<string> {
  if (!Array.isArray(filter) || typeof filter[0] !== "string") return out;
  const [op, ...rest] = filter as [string, ...unknown[]];
  if (op === "all" || op === "any") {
    for (const f of rest) classes(f, out);
    return out;
  }
  if (op !== "==" && op !== "in" && op !== "match") return out;
  if (keyOf(rest[0]) !== "class") return out;
  const add = (v: unknown): void => {
    if (typeof v === "string") out.add(v);
    else if (Array.isArray(v)) (v[0] === "literal" ? [v[1]] : v).forEach(add);
  };
  if (op === "match") {
    // Pairs of label and result, then the fallback: the labels whose
    // result is `true`.
    for (let i = 1; i + 1 < rest.length; i += 2) if (rest[i + 1] === true) add(rest[i]);
  } else rest.slice(1).forEach(add);
  return out;
}

function road(layer: Json, t: MapTones): void {
  const id = typeof layer.id === "string" ? layer.id.toLowerCase() : "";
  const asked = classes(layer.filter, new Set<string>());
  // Without classes in the filter, the usual words in the layer's id are
  // the hint.
  const rail = asked.size ? [...asked].some((c) => RAILS.has(c)) : /rail|transit|tram|ferry/.test(id);
  if (rail) return line(layer, t.rail);
  if (/cas(e|ing)|outline/.test(id)) {
    if (t.casing) return line(layer, t.casing);
    part(layer, "layout", true).visibility = "none";
    return;
  }
  const minor = asked.size ? [...asked].every((c) => MINOR.has(c)) : /minor|service|track|path|pedestrian|foot|cycle|steps/.test(id);
  line(layer, minor ? t.minor : t.road);
}

function label(layer: Json, t: MapTones): void {
  const layout = part(layer, "layout", false);
  // Icons alone stay as they are; so does text set inside its icon (a road
  // shield), which is drawn for the icon's own colours.
  if (layout["text-field"] === undefined) return;
  const fit = layout["icon-text-fit"];
  if (fit !== undefined && fit !== "none") return;
  const p = part(layer, "paint", true);
  p["text-color"] = t.label;
  p["text-halo-color"] = t.halo;
  const w = p["text-halo-width"];
  if (w === undefined || (typeof w === "number" && w < 1)) p["text-halo-width"] = 1.2;
}

function recolour(layer: Json, t: MapTones): void {
  const from = typeof layer["source-layer"] === "string" ? (layer["source-layer"] as string) : "";
  switch (layer.type) {
    case "background": {
      const p = part(layer, "paint", true);
      p["background-color"] = t.paper;
      delete p["background-pattern"];
      break;
    }
    case "fill":
      if (from === "water") fill(layer, t.water);
      else if (from === "land") fill(layer, t.paper);
      else if (from === "building") fill(layer, t.building);
      else if (from === "transportation") fill(layer, t.minor);
      else if (LAND.has(from)) fill(layer, t.land);
      break;
    case "fill-extrusion":
      if (from === "building") {
        const p = part(layer, "paint", true);
        p["fill-extrusion-color"] = t.building;
        delete p["fill-extrusion-pattern"];
      }
      break;
    case "line":
      if (from === "water" || from === "waterway") line(layer, t.water);
      else if (from === "boundary") line(layer, t.boundary);
      else if (from === "transportation" || from === "aeroway") road(layer, t);
      else if (from === "building") line(layer, t.building);
      else if (LAND.has(from)) line(layer, t.land);
      break;
    case "symbol":
      label(layer, t);
      break;
    default:
    // Rasters, hillshade, circles and the rest: as they came.
  }
}

/** The style in the page's colours for the theme: background and land to
 * paper, water and waterways pale (dark: near black), roads white on a
 * soft casing (dark: grey, no casing), buildings and land use a whisper
 * off the paper, labels muted with a paper halo, boundaries a quiet line.
 * Anything it does not expect: the style, as it came. */
export function restyle<T>(style: T, theme: Theme): T {
  try {
    if (theme !== "light" && theme !== "dark") return style;
    const t = MAP_TONES[theme];
    if (!isObject(style) || !Array.isArray(style.layers)) return style;
    const out = JSON.parse(JSON.stringify(style)) as Json;
    for (const layer of out.layers as unknown[]) {
      if (!isObject(layer) || typeof layer.type !== "string") return style;
      // Looked at for every layer, coloured or not: a `paint` or `layout`
      // that is not an object is a style this does not understand.
      part(layer, "paint", false);
      part(layer, "layout", false);
      recolour(layer, t);
    }
    return out as T;
  } catch {
    return style;
  }
}

/** Whether what a server sent looks like a map style at all. */
export function isStyle(v: unknown): v is StyleSpecification {
  return isObject(v) && v.version === 8 && isObject(v.sources) && Array.isArray(v.layers);
}
