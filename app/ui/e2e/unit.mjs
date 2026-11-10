// Checks of the map page's pure parts, without a browser:
//
// - `restyle` (src/basemap.ts): the street background recoloured to the
//   page's paper and ink, against a hand-made style with the usual
//   OpenMapTiles layers (as OpenFreeMap's positron has them). The real style
//   is on the internet; this is the stand-in for it.
// - `parseInset`, `namedOutput` and `layout` (src/layout.ts): the app's
//   inset, the result it names with a request for the table, and where the
//   sheets go.
// - The colours the map is given in code agree with the page's tokens in
//   src/styles.css.
//
//     node app/ui/e2e/unit.mjs        (or: npm run test:unit, in app/ui)
//
// Needs Node 22.18 or newer: it reads the TypeScript sources as they are.
// `map.py` runs it first, so CI's companion job does too.

import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import { validateStyleMin } from "@maplibre/maplibre-gl-style-spec";

if (!process.features.typescript) {
  console.error("unit.mjs needs Node 22.18 or newer (it reads the .ts sources directly)");
  process.exit(1);
}

const { BASEMAP_URL, MAP_TONES, blankStyle, isStyle, restyle } = await import("../src/basemap.ts");
const { GAP, NO_INSET, layout, namedOutput, parseInset } = await import("../src/layout.ts");
const { LAYER_TONES } = await import("../src/theme.ts");

const OFM = "https://tiles.openfreemap.org";
const stops = (a, b) => ({ base: 1.4, stops: [[8, a], [20, b]] });
const road = (id, classes, paint, extra = {}) => ({
  id,
  type: "line",
  source: "openmaptiles",
  "source-layer": "transportation",
  filter: ["all", ["==", "$type", "LineString"], ["in", "class", ...classes]],
  layout: { "line-cap": "round", "line-join": "round" },
  paint,
  ...extra,
});

/** A style in the manner of positron: OpenMapTiles layers, light greys. */
function positron() {
  return {
    version: 8,
    name: "Positron (hand-made for the tests)",
    metadata: { "maputnik:renderer": "mlgljs" },
    center: [10.184, -1.12],
    zoom: 0.87,
    sources: { openmaptiles: { type: "vector", url: `${OFM}/planet` } },
    sprite: `${OFM}/sprites/ofm_f384/ofm`,
    glyphs: `${OFM}/fonts/{fontstack}/{range}.pbf`,
    layers: [
      { id: "background", type: "background", paint: { "background-color": "rgb(242,243,240)" } },
      { id: "park", type: "fill", source: "openmaptiles", "source-layer": "park", filter: ["==", "$type", "Polygon"], paint: { "fill-color": "rgb(230,233,229)" } },
      { id: "water", type: "fill", source: "openmaptiles", "source-layer": "water", filter: ["all", ["==", "$type", "Polygon"], ["!=", "brunnel", "tunnel"]], paint: { "fill-antialias": true, "fill-color": "rgb(194,200,202)" } },
      { id: "landcover_ice_shelf", type: "fill", source: "openmaptiles", "source-layer": "landcover", maxzoom: 8, filter: ["all", ["==", "$type", "Polygon"], ["==", "subclass", "ice_shelf"]], paint: { "fill-color": "hsl(0,0%,98%)", "fill-opacity": 0.7 } },
      { id: "landuse_residential", type: "fill", source: "openmaptiles", "source-layer": "landuse", maxzoom: 16, filter: ["all", ["==", "$type", "Polygon"], ["==", "class", "residential"]], paint: { "fill-color": "rgb(234,234,230)", "fill-opacity": { base: 0.6, stops: [[8, 0.8], [9, 0.6]] } } },
      { id: "landcover_wood", type: "fill", source: "openmaptiles", "source-layer": "landcover", minzoom: 10, filter: ["all", ["==", "$type", "Polygon"], ["==", "class", "wood"]], paint: { "fill-color": "rgb(220,224,220)", "fill-opacity": { base: 1, stops: [[8, 0], [12, 1]] }, "fill-pattern": "wood" } },
      { id: "waterway", type: "line", source: "openmaptiles", "source-layer": "waterway", filter: ["==", "$type", "LineString"], paint: { "line-color": "hsl(195,17%,78%)", "line-width": stops(1, 6) } },
      { id: "water_name", type: "symbol", source: "openmaptiles", "source-layer": "water_name", filter: ["==", "$type", "LineString"], layout: { "symbol-placement": "line", "text-field": "{name:latin}\n{name:nonlatin}", "text-font": ["Noto Sans Italic"], "text-size": 12 }, paint: { "text-color": "rgb(157,169,177)", "text-halo-blur": 1, "text-halo-color": "rgb(242,243,240)", "text-halo-width": 1 } },
      { id: "building", type: "fill", source: "openmaptiles", "source-layer": "building", minzoom: 12, paint: { "fill-antialias": true, "fill-color": "rgb(234,234,229)", "fill-outline-color": "rgb(219,219,218)" } },
      { id: "building-3d", type: "fill-extrusion", source: "openmaptiles", "source-layer": "building", minzoom: 15, paint: { "fill-extrusion-color": "rgb(234,234,229)", "fill-extrusion-height": ["get", "render_height"] } },
      { id: "aeroway-area", type: "fill", source: "openmaptiles", "source-layer": "aeroway", minzoom: 4, filter: ["all", ["==", "$type", "Polygon"], ["in", "class", "runway", "taxiway"]], paint: { "fill-color": "rgba(255,255,255,1)", "fill-opacity": 0.7 } },
      { id: "aeroway-runway", type: "line", source: "openmaptiles", "source-layer": "aeroway", minzoom: 4, filter: ["all", ["in", "class", "runway"], ["==", "$type", "LineString"]], paint: { "line-color": "rgba(255,255,255,1)", "line-width": stops(1, 30) } },
      { id: "highway_area", type: "fill", source: "openmaptiles", "source-layer": "transportation", filter: ["==", "$type", "Polygon"], paint: { "fill-color": "hsla(0,0%,89%,0.56)", "fill-outline-color": "#cfcdca" } },
      road("tunnel_motorway_casing", ["motorway"], { "line-color": "rgb(213,213,213)", "line-width": stops(1, 23), "line-dasharray": [0.5, 0.25] }),
      road("tunnel_motorway_inner", ["motorway"], { "line-color": "rgb(234,234,234)", "line-width": stops(0, 20) }),
      road("highway_path", ["path"], { "line-color": "rgb(234,234,234)", "line-opacity": 0.9, "line-width": stops(1, 10), "line-dasharray": [1.5, 1.5] }),
      road("highway_minor", ["minor", "service", "track"], { "line-color": "hsl(0,0%,88%)", "line-opacity": 0.9, "line-width": stops(0.5, 13) }),
      road("highway_major_casing", ["primary", "secondary", "tertiary", "trunk"], { "line-color": "rgb(213,213,213)", "line-width": stops(3, 23) }),
      road("highway_major_inner", ["primary", "secondary", "tertiary", "trunk"], { "line-color": "#fff", "line-width": stops(2, 20) }),
      road("highway_major_subtle", ["primary", "secondary", "tertiary", "trunk"], { "line-color": "hsla(0,0%,85%,0.69)", "line-width": 2 }, { maxzoom: 11 }),
      road("highway_motorway_casing", ["motorway"], { "line-color": "rgb(213,213,213)", "line-width": stops(3, 23) }),
      road("highway_motorway_inner", ["motorway"], { "line-color": { base: 1, stops: [[5.8, "hsla(0,0%,85%,0.53)"], [6, "#fff"]] }, "line-width": stops(1, 20) }),
      road("railway_transit", ["transit"], { "line-color": "#dddddd", "line-width": 3 }),
      road("railway_transit_dashline", ["transit"], { "line-color": "#fafafa", "line-dasharray": [3, 3], "line-width": 2 }),
      road("railway", ["rail"], { "line-color": "#dddddd", "line-width": stops(1, 7) }),
      road("railway_dashline", ["rail"], { "line-color": "#fafafa", "line-dasharray": [3, 3], "line-width": stops(1, 6) }),
      // The same roads, said the newer way and with no hint in the id.
      { id: "roads-a", type: "line", source: "openmaptiles", "source-layer": "transportation", filter: ["match", ["get", "class"], ["minor", "service"], true, false], paint: { "line-color": "#eee" } },
      { id: "roads-b", type: "line", source: "openmaptiles", "source-layer": "transportation", filter: ["all", ["==", ["get", "class"], "rail"], ["!=", ["get", "brunnel"], "tunnel"]], paint: { "line-color": "#ddd" } },
      { id: "roads-c", type: "line", source: "openmaptiles", "source-layer": "transportation", filter: ["all", ["!", ["in", ["get", "class"], ["literal", ["rail", "transit"]]]], ["in", ["get", "class"], ["literal", ["motorway", "trunk"]]]], paint: { "line-color": "#fff" } },
      { id: "roads-d", type: "line", source: "openmaptiles", "source-layer": "transportation", filter: ["all", ["!in", "class", "rail", "transit"], ["in", "class", "pier"]], paint: { "line-pattern": "pier", "line-width": 4 } },
      { id: "highway_name_other", type: "symbol", source: "openmaptiles", "source-layer": "transportation_name", filter: ["all", ["!=", "class", "motorway"], ["==", "$type", "LineString"]], layout: { "symbol-placement": "line", "text-field": "{name:latin} {name:nonlatin}", "text-font": ["Noto Sans Regular"], "text-size": 10 }, paint: { "text-color": "#bbb", "text-halo-blur": 1, "text-halo-color": "#fff", "text-halo-width": 2 } },
      { id: "highway_shield", type: "symbol", source: "openmaptiles", "source-layer": "transportation_name", filter: ["==", "class", "motorway"], layout: { "icon-image": "road_{ref_length}", "icon-text-fit": "both", "text-field": "{ref}", "text-font": ["Noto Sans Bold"], "text-size": 10 }, paint: { "text-color": "#333" } },
      { id: "poi_icons", type: "symbol", source: "openmaptiles", "source-layer": "poi", layout: { "icon-image": "{class}_11" } },
      { id: "boundary_state", type: "line", source: "openmaptiles", "source-layer": "boundary", filter: ["==", "admin_level", 4], paint: { "line-color": "rgb(230,204,207)", "line-dasharray": [2, 2], "line-width": stops(1, 4) } },
      { id: "boundary_country", type: "line", source: "openmaptiles", "source-layer": "boundary", filter: ["==", "admin_level", 2], paint: { "line-color": "rgb(230,204,207)", "line-width": stops(1.1, 2) } },
      { id: "place_suburb", type: "symbol", source: "openmaptiles", "source-layer": "place", maxzoom: 15, filter: ["all", ["==", "$type", "Point"], ["==", "class", "suburb"]], layout: { "text-field": "{name:latin}\n{name:nonlatin}", "text-font": ["Noto Sans Regular"], "text-letter-spacing": 0.1, "text-size": 10, "text-transform": "uppercase" }, paint: { "text-color": "rgb(117,129,145)", "text-halo-blur": 1, "text-halo-color": "rgb(242,243,240)", "text-halo-width": 1 } },
      { id: "place_city", type: "symbol", source: "openmaptiles", "source-layer": "place", maxzoom: 14, filter: ["all", ["==", "$type", "Point"], ["==", "class", "city"]], layout: { "icon-image": { base: 1, stops: [[0, "circle-11"], [8, ""]] }, "text-field": "{name:latin}\n{name:nonlatin}", "text-font": ["Noto Sans Bold"], "text-size": 14 }, paint: { "text-color": "#000", "text-halo-color": "rgb(242,243,240)", "text-halo-width": 0.8 } },
      { id: "place_country", type: "symbol", source: "openmaptiles", "source-layer": "place", filter: ["==", "class", "country"], layout: { "text-field": "{name:latin}", "text-font": ["Noto Sans Bold"], "text-size": 13 } },
      // What the page leaves alone.
      { id: "hills", type: "hillshade", source: "dem", paint: { "hillshade-shadow-color": "#444" } },
      { id: "satellite", type: "raster", source: "sat", paint: { "raster-opacity": 0.5 } },
      { id: "dots", type: "circle", source: "geo", paint: { "circle-color": "#f00" } },
      { id: "other-fill", type: "fill", source: "openmaptiles", "source-layer": "something_else", paint: { "fill-color": "#123456" } },
    ],
  };
}
// Sources for the layers above that are not OpenMapTiles.
const withSources = (s) => ({ ...s, sources: { ...s.sources, dem: { type: "raster-dem", url: `${OFM}/dem` }, sat: { type: "raster", url: `${OFM}/sat` }, geo: { type: "geojson", data: { type: "FeatureCollection", features: [] } } } });

const byId = (style, id) => style.layers.find((l) => l.id === id);
const paint = (style, id) => byId(style, id).paint || {};

for (const theme of ["light", "dark"]) {
  const t = MAP_TONES[theme];
  test(`restyle, ${theme}: each kind of layer takes the page's colour`, () => {
    const out = restyle(withSources(positron()), theme);
    assert.equal(paint(out, "background")["background-color"], t.paper);
    assert.equal(paint(out, "water")["fill-color"], t.water);
    assert.equal(paint(out, "waterway")["line-color"], t.water);
    for (const id of ["park", "landcover_ice_shelf", "landuse_residential", "landcover_wood", "aeroway-area"]) {
      assert.equal(paint(out, id)["fill-color"], t.land, id);
    }
    assert.equal(paint(out, "building")["fill-color"], t.building);
    assert.equal(paint(out, "building-3d")["fill-extrusion-color"], t.building);
    assert.equal(paint(out, "highway_area")["fill-color"], t.minor);
    // Roads: the main ones, the small ones, and the line under them.
    for (const id of ["tunnel_motorway_inner", "highway_major_inner", "highway_major_subtle", "highway_motorway_inner", "aeroway-runway", "roads-c", "roads-d"]) {
      assert.equal(paint(out, id)["line-color"], t.road, id);
    }
    for (const id of ["highway_path", "highway_minor", "roads-a"]) assert.equal(paint(out, id)["line-color"], t.minor, id);
    for (const id of ["tunnel_motorway_casing", "highway_major_casing", "highway_motorway_casing"]) {
      if (t.casing) {
        assert.equal(paint(out, id)["line-color"], t.casing, id);
        assert.notEqual(byId(out, id).layout.visibility, "none", id);
      } else assert.equal(byId(out, id).layout.visibility, "none", id);
    }
    // Rails are not roads, whatever the id says.
    for (const id of ["railway_transit", "railway_transit_dashline", "railway", "railway_dashline", "roads-b"]) {
      assert.equal(paint(out, id)["line-color"], t.rail, id);
    }
    for (const id of ["boundary_state", "boundary_country"]) assert.equal(paint(out, id)["line-color"], t.boundary, id);
    // Labels: muted, with a paper halo wide enough to read on.
    for (const id of ["water_name", "highway_name_other", "place_suburb", "place_city", "place_country"]) {
      const p = paint(out, id);
      assert.equal(p["text-color"], t.label, id);
      assert.equal(p["text-halo-color"], t.halo, id);
      assert.ok(p["text-halo-width"] >= 1, `${id}: halo ${p["text-halo-width"]}`);
    }
    assert.equal(paint(out, "highway_name_other")["text-halo-width"], 2, "a halo already wide enough is kept");
  });

  test(`restyle, ${theme}: nothing but colour changes`, () => {
    const before = withSources(positron());
    const out = restyle(before, theme);
    assert.deepEqual(out.layers.map((l) => l.id), before.layers.map((l) => l.id), "the same layers, in order");
    for (const key of ["version", "name", "metadata", "center", "zoom", "sources", "sprite", "glyphs"]) assert.deepEqual(out[key], before[key], key);
    for (const [i, l] of before.layers.entries()) {
      const o = out.layers[i];
      for (const key of ["type", "source", "source-layer", "filter", "minzoom", "maxzoom"]) assert.deepEqual(o[key], l[key], `${l.id}: ${key}`);
      // Widths, opacities, dashes and text stay as the style drew them.
      for (const [k, v] of Object.entries(l.paint || {})) {
        if (/color|pattern|gradient|halo-width/.test(k)) continue;
        assert.deepEqual(o.paint[k], v, `${l.id}: ${k}`);
      }
      for (const [k, v] of Object.entries(l.layout || {})) assert.deepEqual(o.layout[k], v, `${l.id}: ${k}`);
    }
    // Pictures in place of a colour, and outlines in the old colour, go.
    assert.equal(paint(out, "landcover_wood")["fill-pattern"], undefined);
    assert.equal(paint(out, "building")["fill-outline-color"], undefined);
    assert.equal(paint(out, "roads-d")["line-pattern"], undefined);
    // What is not the basemap's land, water, roads or labels is untouched.
    for (const id of ["hills", "satellite", "dots", "other-fill", "poi_icons"]) assert.deepEqual(byId(out, id), byId(before, id), id);
    // Text set in its icon (a road shield) keeps the colours drawn for it.
    assert.deepEqual(byId(out, "highway_shield"), byId(before, "highway_shield"));
  });

  test(`restyle, ${theme}: pure, repeatable, and still a valid style`, () => {
    const before = withSources(positron());
    const copy = structuredClone(before);
    const out = restyle(before, theme);
    assert.deepEqual(before, copy, "the style given is not changed");
    assert.notEqual(out, before);
    assert.deepEqual(restyle(out, theme), out, "a second pass changes nothing");
    assert.deepEqual(restyle(before, theme), out, "the same answer each time");
    assert.deepEqual(validateStyleMin(before).map((e) => e.message), [], "the hand-made style is valid");
    assert.deepEqual(validateStyleMin(out).map((e) => e.message), [], "and so is the restyled one");
  });
}

test("restyle: light and dark differ, and dark draws no casing", () => {
  const light = restyle(positron(), "light");
  const dark = restyle(positron(), "dark");
  assert.notDeepEqual(light, dark);
  assert.equal(MAP_TONES.dark.casing, null);
  assert.equal(byId(dark, "highway_major_casing").layout.visibility, "none");
  assert.equal(byId(light, "highway_major_casing").layout.visibility, undefined);
});

test("restyle: any surprise hands the style back as it came", () => {
  const same = (v, theme = "light") => assert.equal(restyle(v, theme), v);
  for (const v of [null, undefined, 0, 7, "", "https://tiles.openfreemap.org/styles/positron", true, [], [1, 2]]) same(v);
  same({});
  same({ version: 8, sources: {} });
  same({ version: 8, sources: {}, layers: "none" });
  same({ version: 8, sources: {}, layers: { background: {} } });
  same({ version: 8, sources: {}, layers: [null] });
  same({ version: 8, sources: {}, layers: ["background"] });
  same({ version: 8, sources: {}, layers: [{ id: "a" }] });
  same({ version: 8, sources: {}, layers: [{ id: "a", type: 5 }] });
  same({ version: 8, sources: {}, layers: [{ id: "a", type: "background", paint: "red" }] });
  same({ version: 8, sources: {}, layers: [{ id: "a", type: "background", paint: ["red"] }] });
  same({ version: 8, sources: {}, layers: [{ id: "a", type: "symbol", layout: 3 }] });
  // Even on a layer the page would not colour.
  same({ version: 8, sources: {}, layers: [{ id: "a", type: "raster", source: "r", paint: "x" }] });
  same({ version: 8, sources: {}, layers: [{ id: "a", type: "line", "source-layer": "contours", paint: 5 }] });
  same({ version: 8, sources: {}, layers: [{ id: "a", type: "circle", layout: [] }] });
  same({ version: 8, sources: {}, layers: [{ id: "a", type: "line", "source-layer": "transportation", filter: ["==", "class", "motorway"], layout: "x" }] }, "dark");
  // One bad layer among good ones: the whole style comes back untouched.
  const mixed = positron();
  mixed.layers.push({ id: "odd", type: "fill", "source-layer": "water", paint: 4 });
  same(mixed);
  // A theme the page does not have.
  for (const theme of ["", "sepia", "LIGHT", "toString", "__proto__", null, undefined, 1, {}]) {
    const style = positron();
    assert.equal(restyle(style, theme), style, `theme ${String(theme)}`);
  }
  // Something that cannot be copied as JSON.
  const loop = positron();
  loop.self = loop;
  same(loop);
  const big = positron();
  big.layers[0].paint["background-opacity"] = 10n;
  same(big);
});

test("restyle: a style with nothing it knows passes through unchanged in content", () => {
  const plain = { version: 8, sources: {}, layers: [{ id: "x", type: "raster", source: "r" }, { id: "y", type: "line", source: "g", "source-layer": "contours", paint: { "line-color": "#abc" } }] };
  assert.deepEqual(restyle(plain, "dark"), plain);
  assert.deepEqual(restyle({ version: 8, sources: {}, layers: [] }, "light"), { version: 8, sources: {}, layers: [] });
});

test("restyle: a layer with no paint or filter is still coloured", () => {
  const bare = {
    version: 8,
    sources: {},
    layers: [
      { id: "bg", type: "background" },
      { id: "w", type: "fill", "source-layer": "water" },
      { id: "r", type: "line", "source-layer": "transportation" },
      { id: "r_casing", type: "line", "source-layer": "transportation" },
      { id: "railway", type: "line", "source-layer": "transportation" },
      { id: "label", type: "symbol", "source-layer": "place", layout: { "text-field": "{name}" } },
      { id: "no-text", type: "symbol", "source-layer": "place" },
    ],
  };
  const out = restyle(bare, "dark");
  const t = MAP_TONES.dark;
  assert.equal(paint(out, "bg")["background-color"], t.paper);
  assert.equal(paint(out, "w")["fill-color"], t.water);
  assert.equal(paint(out, "r")["line-color"], t.road);
  assert.equal(byId(out, "r_casing").layout.visibility, "none");
  assert.equal(paint(out, "railway")["line-color"], t.rail);
  assert.deepEqual(paint(out, "label"), { "text-color": t.label, "text-halo-color": t.halo, "text-halo-width": 1.2 });
  assert.deepEqual(byId(out, "no-text"), byId(bare, "no-text"));
  assert.equal(paint(restyle(bare, "light"), "r_casing")["line-color"], MAP_TONES.light.casing);
});

test("the blank map is the paper alone, and a style is told from what is not one", () => {
  for (const theme of ["light", "dark"]) {
    const s = blankStyle(theme);
    assert.deepEqual(validateStyleMin(s).map((e) => e.message), []);
    assert.deepEqual(s.layers, [{ id: "background", type: "background", paint: { "background-color": MAP_TONES[theme].paper } }]);
    assert.ok(isStyle(s));
  }
  assert.ok(isStyle(positron()));
  for (const v of [null, "x", [], {}, { version: 7, sources: {}, layers: [] }, { version: 8, layers: [] }, { version: 8, sources: {}, layers: {} }, { version: 8, sources: [], layers: [] }]) {
    assert.equal(isStyle(v), false, JSON.stringify(v));
  }
  assert.equal(BASEMAP_URL, "https://tiles.openfreemap.org/styles/positron", "the one internet host the page's policy allows");
});

test("parseInset: finite numbers from 0 to 4000, a side left out is 0", () => {
  assert.deepEqual(parseInset({ left: 488, top: 0, right: 0, bottom: 0 }), { left: 488, top: 0, right: 0, bottom: 0 });
  assert.deepEqual(parseInset({ left: 488 }), { left: 488, top: 0, right: 0, bottom: 0 });
  assert.deepEqual(parseInset({}), NO_INSET);
  assert.deepEqual(parseInset({ left: 0.5, top: 4000, right: 12.25, bottom: 1 }), { left: 0.5, top: 4000, right: 12.25, bottom: 1 });
  assert.deepEqual(parseInset({ left: 10, color: "red" }), { left: 10, top: 0, right: 0, bottom: 0 }, "other keys are not looked at");
  assert.notEqual(parseInset({}), NO_INSET, "a fresh object each time");
});

test("parseInset: anything else is refused whole", () => {
  for (const v of [undefined, null, 0, 488, "488", true, [], [488, 0, 0, 0], () => 1]) assert.equal(parseInset(v), null, String(v));
  for (const bad of [-1, -0.001, 4000.5, 1e9, NaN, Infinity, -Infinity, "488", "", null, true, [488], { valueOf: () => 5 }, 5n]) {
    for (const side of ["left", "top", "right", "bottom"]) {
      assert.equal(parseInset({ left: 1, top: 2, right: 3, bottom: 4, [side]: bad }), null, `${side}: ${String(bad)}`);
    }
  }
});

test("namedOutput: only the handle of one of the session's own outputs with rows to show", () => {
  const outputs = [
    { handle: "ws://s/results/roads.gpkg", viewable: true },
    { handle: "ws://s/results/elevation.tif", viewable: false },
  ];
  assert.equal(namedOutput("ws://s/results/roads.gpkg", outputs), "ws://s/results/roads.gpkg");
  // A result with no rows to show, one the page does not have, one from
  // another session: none of them is named.
  assert.equal(namedOutput("ws://s/results/elevation.tif", outputs), null);
  assert.equal(namedOutput("ws://s/results/other.gpkg", outputs), null);
  assert.equal(namedOutput("ws://t/results/roads.gpkg", outputs), null);
  // Not a short string: refused before it is compared with anything.
  for (const bad of [undefined, null, 7, true, "", {}, ["ws://s/results/roads.gpkg"], { handle: "ws://s/results/roads.gpkg" }]) {
    assert.equal(namedOutput(bad, outputs), null, JSON.stringify(bad));
  }
  const long = "ws://s/results/" + "a".repeat(200);
  assert.equal(namedOutput(long, [{ handle: long, viewable: true }]), null, "longer than a handle the app passes on");
  assert.equal(namedOutput("ws://s/results/roads.gpkg", []), null);
});

test("layout, framed: the app's inset, the table from it, the map padded by both", () => {
  const inset = { left: 488, top: 0, right: 0, bottom: 0 };
  const closed = layout({ width: 1440, height: 900, covered: inset, tableOpen: false });
  assert.deepEqual(closed, { inset, panel: null, table: 0, pad: inset });
  const open = layout({ width: 1440, height: 900, covered: inset, tableOpen: true });
  assert.equal(open.table, 360, "40% of the height");
  assert.deepEqual(open.pad, { left: 488, top: 0, right: 0, bottom: 360 + GAP });
  assert.equal(layout({ width: 1440, height: 500, covered: inset, tableOpen: true }).table, 240, "at least 240 px");
  // No inset sent: nothing is covered.
  assert.deepEqual(layout({ width: 1024, height: 680, covered: NO_INSET, tableOpen: false }).pad, NO_INSET);
  // An inset from every side.
  const all = layout({ width: 1440, height: 900, covered: { left: 488, top: 40, right: 20, bottom: 30 }, tableOpen: true });
  assert.deepEqual(all.pad, { left: 488, top: 40, right: 20, bottom: 30 + 360 + GAP });
});

test("layout: some of the map always stays in view", () => {
  // An inset wider than the page is shrunk, not obeyed.
  const wide = layout({ width: 800, height: 600, covered: { left: 4000, top: 0, right: 4000, bottom: 0 }, tableOpen: false });
  assert.ok(wide.inset.left + wide.inset.right <= 800 - 120, JSON.stringify(wide.inset));
  assert.equal(wide.inset.left, wide.inset.right);
  const tall = layout({ width: 800, height: 600, covered: { left: 0, top: 500, right: 0, bottom: 500 }, tableOpen: true });
  assert.ok(tall.pad.top + tall.pad.bottom <= 600 - 120, JSON.stringify(tall.pad));
  for (const [width, height] of [[1440, 900], [1024, 680], [760, 500], [600, 400], [320, 240], [0, 0]]) {
    for (const covered of [null, NO_INSET, { left: 488, top: 0, right: 0, bottom: 0 }, { left: 3000, top: 3000, right: 3000, bottom: 3000 }]) {
      for (const tableOpen of [false, true]) {
        const l = layout({ width, height, covered, tableOpen, deciding: true });
        for (const v of [...Object.values(l.inset), ...Object.values(l.pad), l.table]) assert.ok(Number.isFinite(v) && v >= 0, `${width}x${height}: ${v}`);
        if (width >= 240 && height >= 240) {
          assert.ok(width - l.pad.left - l.pad.right >= 120, `${width}x${height}: ${JSON.stringify(l.pad)}`);
          assert.ok(height - l.pad.top - l.pad.bottom >= 120, `${width}x${height}: ${JSON.stringify(l.pad)}`);
        }
        assert.equal(tableOpen, l.table > 0);
      }
    }
  }
});

test("layout, on its own: the page's panel beside the map, wider while deciding, above it when narrow", () => {
  const page = layout({ width: 1360, height: 860, covered: null, tableOpen: false });
  assert.deepEqual(page.panel, { width: 400, height: 860 - 2 * GAP });
  assert.deepEqual(page.inset, { left: GAP + 400 + GAP, top: 0, right: 0, bottom: 0 });
  assert.deepEqual(page.pad, page.inset);
  const deciding = layout({ width: 1360, height: 860, covered: null, tableOpen: false, deciding: true });
  assert.equal(deciding.panel.width, 600);
  assert.equal(deciding.inset.left, GAP + 600 + GAP);
  assert.equal(layout({ width: 900, height: 700, covered: null, tableOpen: false, deciding: true }).panel.width, 508, "the map keeps a fair share");
  assert.equal(layout({ width: 800, height: 700, covered: null, tableOpen: false, deciding: true }).panel.width, 408);
  assert.equal(layout({ width: 770, height: 700, covered: null, tableOpen: false, deciding: true }).panel.width, 400, "never narrower than usual");
  const open = layout({ width: 1360, height: 860, covered: null, tableOpen: true });
  assert.equal(open.table, 344);
  assert.deepEqual(open.pad, { left: 432, top: 0, right: 0, bottom: 344 + GAP });
  const narrow = layout({ width: 700, height: 800, covered: null, tableOpen: false });
  assert.deepEqual(narrow.panel, { width: 700 - 2 * GAP, height: 368 });
  assert.deepEqual(narrow.inset, { left: 0, top: GAP + 368 + GAP, right: 0, bottom: 0 });
  // Not connected yet: no panel, nothing covered.
  assert.deepEqual(layout({ width: 1360, height: 860, covered: NO_INSET, tableOpen: false }), { inset: NO_INSET, panel: null, table: 0, pad: NO_INSET });
});

test("the map's colours in code are the page's tokens in styles.css", () => {
  const css = readFileSync(new URL("../src/styles.css", import.meta.url), "utf8");
  const block = (start) => {
    const at = css.indexOf(start);
    assert.ok(at >= 0, `styles.css has ${start}`);
    return css.slice(at, css.indexOf("}", at));
  };
  const token = (text, name) => {
    const m = new RegExp(`--${name}:\\s*(#[0-9a-fA-F]{6})\\s*;`).exec(text);
    assert.ok(m, `--${name}`);
    return m[1].toLowerCase();
  };
  const light = block(":root {");
  const darkSystem = block(':root:not([data-theme="light"]) {');
  const darkChosen = block(':root[data-theme="dark"] {');
  // The agreed tokens, exactly (the same as the app window's, in app/desktop/ui/app.css).
  const want = {
    light: { paper: "#efede8", panel: "#ffffff", ink: "#17181a", ink2: "#3f4044", mute: "#66676b", faint: "#a6a7aa", line: "#edece9", line2: "#dfdeda", fill: "#f6f5f3", fill2: "#eeedea", hit: "#ef4b23", wait: "#f0ae1b", "wait-ink": "#7a5200", "wait-bg": "#fff6dc", ok: "#18794e", "ok-bg": "#e7f3ec", bad: "#be3319", "bad-bg": "#fbe9e5" },
    dark: { paper: "#121315", panel: "#1c1d20", ink: "#f3f2ef", ink2: "#cfceca", mute: "#9c9c9a", faint: "#66676a", line: "#2a2b2f", line2: "#393a3f", fill: "#25262a", fill2: "#2e2f34", hit: "#ff5b34", wait: "#f2b01e", "wait-ink": "#f5c55a", "wait-bg": "#2c2513", ok: "#4cc38a", "ok-bg": "#16291f", bad: "#ff7b63", "bad-bg": "#321a15" },
  };
  for (const [name, value] of Object.entries(want.light)) assert.equal(token(light, name), value, `light --${name}`);
  for (const [name, value] of Object.entries(want.dark)) {
    assert.equal(token(darkSystem, name), value, `dark (system) --${name}`);
    assert.equal(token(darkChosen, name), value, `dark (chosen) --${name}`);
  }
  // The app window's own stylesheet carries the same ones: the two are one
  // look, and a colour changed in one file alone would split them.
  const app = readFileSync(new URL("../../desktop/ui/app.css", import.meta.url), "utf8");
  const appBlock = (start) => {
    const at = app.indexOf(start);
    assert.ok(at >= 0, `app.css has ${start}`);
    return app.slice(at, app.indexOf("}", at));
  };
  const appLight = appBlock(":root {");
  const appDarkSystem = appBlock(':root:not([data-theme="light"]) {');
  const appDarkChosen = appBlock(':root[data-theme="dark"] {');
  for (const [name, value] of Object.entries(want.light)) assert.equal(token(appLight, name), value, `app.css light --${name}`);
  for (const [name, value] of Object.entries(want.dark)) {
    assert.equal(token(appDarkSystem, name), value, `app.css dark (system) --${name}`);
    assert.equal(token(appDarkChosen, name), value, `app.css dark (chosen) --${name}`);
  }
  // The edge of a field (3:1 on the panel) is the same in both, too.
  for (const [text, where] of [[light, "styles.css light"], [appLight, "app.css light"]]) assert.equal(token(text, "field"), "#85868a", where);
  for (const [text, where] of [[darkSystem, "styles.css dark"], [darkChosen, "styles.css dark (chosen)"], [appDarkSystem, "app.css dark"], [appDarkChosen, "app.css dark (chosen)"]]) {
    assert.equal(token(text, "field"), "#77787b", where);
  }
  for (const theme of ["light", "dark"]) {
    const w = want[theme];
    assert.equal(MAP_TONES[theme].paper, w.paper);
    assert.equal(MAP_TONES[theme].halo, w.paper);
    assert.equal(MAP_TONES[theme].label, w.mute);
    assert.equal(MAP_TONES[theme].boundary, w.line2);
    assert.equal(LAYER_TONES[theme].hit, w.hit);
    assert.equal(LAYER_TONES[theme].ink, w.ink);
    assert.equal(LAYER_TONES[theme].panel, w.panel);
    assert.equal(LAYER_TONES[theme].rest.length, 6);
    assert.ok(!LAYER_TONES[theme].rest.includes(w.hit), "no resting colour is the hit colour");
    assert.equal(new Set(LAYER_TONES[theme].rest).size, 6);
  }
  assert.deepEqual(LAYER_TONES.light.rest, ["#1b1c1f", "#5b6b8c", "#7a6a4f", "#4f7a6a", "#8c5b7a", "#6b6c70"]);
  assert.equal(MAP_TONES.light.water, "#cadce8");
  assert.equal(MAP_TONES.dark.water, "#0b1017");
  assert.equal(MAP_TONES.light.road, "#ffffff");
  assert.equal(MAP_TONES.light.casing, "#dfdbd3");
  assert.equal(MAP_TONES.dark.road, "#2b2e33");
  assert.equal(MAP_TONES.dark.minor, "#1c1e21");
});
