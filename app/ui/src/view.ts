// What the map shows, said to the Columbus server for the assistant
// (`map_view`), and the changes the assistant asks the map to make.
//
// The page says only what `ViewReport` lists: where the map looks, which of
// the session's own layers are on, the active output, the selected feature
// (its layer and its row, never its values: the server reads those itself),
// whether the table is open and whether the street background is on. Web
// layers are no part of it: nothing here takes their ids, titles, order or
// opacity, or which one is the basemap (D-14). The server checks every part
// again and refuses a report with anything else in it.
//
// Pure, with no imports at run time, so `e2e/unit.mjs` can check it without
// a browser.

/** West, south, east, north in degrees (EPSG:4326). East passes 180 when
 * the view crosses the 180th meridian. */
export type Extent = [number, number, number, number];

export type CommandPart = "show" | "hide" | "active" | "zoom_to";

export interface ViewReport {
  /** The page is in front of the person, and its map has a size. */
  showing: boolean;
  /** Milliseconds since the person last used this page. */
  idle_ms: number;
  /** The last change (its number) this page has finished applying. */
  applied: number;
  /** The parts of that change the page could not do. */
  not_done: CommandPart[];
  view?: {
    extent: Extent;
    zoom: number;
    /** The session's layers that are on, the list's top one first. */
    layers: string[];
    active: string | null;
    selected: { layer: string; feature: number } | null;
    table: boolean;
    streets: boolean;
  };
}

/** A change the assistant asked for, as the server hands it over. */
export interface ViewCommand {
  seq: number;
  show: string[];
  hide: string[];
  active: string | null;
  zoom_to: string | Extent | null;
}

/** One of the session's outputs, as far as this module needs it. */
export interface Listed {
  handle: string;
  kind: string;
  viewable: boolean;
}

/** Most layers a report names (the server takes no more). */
export const MAX_LAYERS = 300;
/** Most layers one change turns on, or off. */
const MAX_NAMED = 50;
// The longest handle the server takes (as the app's own limit, `layout.ts`).
const MAX_HANDLE = 200;

const isHandle = (v: unknown): v is string => typeof v === "string" && v.length > 0 && v.length <= MAX_HANDLE;

// About a centimetre: enough for any zoom, and short to send.
const round = (v: number) => Math.round(v * 1e7) / 1e7;

/** A box of longitudes and latitudes as the server takes an extent: west
 * from -180 to 180, east greater than west by at most 360 (so past 180
 * when the box crosses the 180th meridian), latitudes within the poles.
 * Null when it is no box at all (a map without a size). */
export function normalExtent(west: number, south: number, east: number, north: number): Extent | null {
  if (![west, south, east, north].every(Number.isFinite)) return null;
  let w = west;
  let e = east;
  if (e - w >= 360) {
    // Zoomed out past a whole world: all of it.
    w = -180;
    e = 180;
  } else {
    const turns = Math.floor((w + 180) / 360);
    w -= 360 * turns;
    e -= 360 * turns;
  }
  const out: Extent = [round(w), round(Math.max(-90, south)), round(e), round(Math.min(90, north))];
  if (!(out[0] >= -180 && out[0] <= 180 && out[2] > out[0] && out[2] <= out[0] + 360 && out[3] > out[1])) return null;
  return out;
}

/** What the page tells the server. Only the session's own outputs are
 * named, by their handles. */
export function viewReport(i: {
  /** Where the map looks; null while it is not on show. */
  seen: { extent: Extent; zoom: number } | null;
  idleMs: number;
  applied: number;
  notDone: readonly CommandPart[];
  /** The session's outputs, in the list's order (its top row first). */
  outputs: readonly Listed[];
  onMap: readonly string[];
  /** A layer that could not be drawn (its data did not load). */
  failed: (handle: string) => boolean;
  active: string | null;
  selected: { handle: string; feature: number } | null;
  tableOpen: boolean;
  streets: boolean;
}): ViewReport {
  const head = {
    idle_ms: Math.max(0, Math.round(i.idleMs)) || 0,
    applied: Math.max(0, Math.round(i.applied)) || 0,
    not_done: [...i.notDone],
  };
  if (!i.seen || !Number.isFinite(i.seen.zoom)) return { showing: false, ...head };
  const listed = (h: string) => i.outputs.some((o) => o.handle === h && o.viewable && isHandle(h));
  const s = i.selected;
  return {
    showing: true,
    ...head,
    view: {
      extent: i.seen.extent,
      zoom: Math.round(Math.min(30, Math.max(0, i.seen.zoom)) * 100) / 100,
      layers: i.outputs
        .filter((o) => o.kind === "layer" && o.viewable && isHandle(o.handle) && i.onMap.includes(o.handle) && !i.failed(o.handle))
        .slice(0, MAX_LAYERS)
        .map((o) => o.handle),
      active: i.active && listed(i.active) ? i.active : null,
      selected: s && listed(s.handle) && Number.isSafeInteger(s.feature) && s.feature >= 0 ? { layer: s.handle, feature: s.feature } : null,
      table: !!i.tableOpen,
      streets: !!i.streets,
    },
  };
}

/** The server's change, checked: a number, lists of handles, a handle or
 * an extent. Anything else is no change (null), and the map stays as it
 * is. Whether the handles are this session's own is checked when the
 * change is applied (`planCommand`). */
export function parseCommand(v: unknown): ViewCommand | null {
  if (!v || typeof v !== "object" || Array.isArray(v)) return null;
  const c = v as Record<string, unknown>;
  if (typeof c.seq !== "number" || !Number.isSafeInteger(c.seq) || c.seq < 1) return null;
  const handles = (x: unknown): string[] | null => {
    if (x == null) return [];
    return Array.isArray(x) && x.length <= MAX_NAMED && x.every(isHandle) ? [...new Set(x as string[])] : null;
  };
  const show = handles(c.show);
  const hide = handles(c.hide);
  if (!show || !hide) return null;
  if (c.active != null && !isHandle(c.active)) return null;
  let zoom: string | Extent | null = null;
  const z = c.zoom_to;
  if (isHandle(z)) zoom = z;
  else if (Array.isArray(z) && z.length === 4 && z.every((n) => typeof n === "number" && Number.isFinite(n))) {
    const [w, s, e, n] = z as number[];
    if (!(w >= -180 && w <= 180 && e > w && e <= w + 360 && s >= -90 && n <= 90 && n > s)) return null;
    zoom = [w, s, e, n];
  } else if (z != null) return null;
  return { seq: c.seq, show, hide, active: (c.active as string | null | undefined) ?? null, zoom_to: zoom };
}

/** How a change applies to this page: only the session's own outputs it
 * lists can be named. `ready` is false while one named is not listed yet
 * (a result made a moment ago); the parts that still name an unknown one
 * when the change is applied are left out and said in `notDone`. */
export function planCommand(
  cmd: ViewCommand,
  outputs: readonly Listed[],
): {
  ready: boolean;
  show: string[];
  hide: string[];
  active: string | null;
  zoomLayer: string | null;
  zoomExtent: Extent | null;
  notDone: CommandPart[];
} {
  const isLayer = (h: string) => outputs.some((o) => o.handle === h && o.viewable && o.kind === "layer");
  const isOutput = (h: string) => outputs.some((o) => o.handle === h && o.viewable);
  const wantLayer = typeof cmd.zoom_to === "string" ? cmd.zoom_to : null;
  const notDone: CommandPart[] = [];
  if (!cmd.show.every(isLayer)) notDone.push("show");
  if (!cmd.hide.every(isLayer)) notDone.push("hide");
  if (cmd.active && !isOutput(cmd.active)) notDone.push("active");
  const zoomLayer = wantLayer && isLayer(wantLayer) ? wantLayer : null;
  if (wantLayer && !zoomLayer) notDone.push("zoom_to");
  // Zooming to a layer shows it; a layer both shown and hidden stays on.
  const show = [...new Set([...cmd.show.filter(isLayer), ...(zoomLayer ? [zoomLayer] : [])])];
  return {
    ready: notDone.length === 0,
    show,
    hide: cmd.hide.filter((h) => isLayer(h) && !show.includes(h)),
    active: cmd.active && isOutput(cmd.active) ? cmd.active : null,
    zoomLayer,
    zoomExtent: Array.isArray(cmd.zoom_to) ? cmd.zoom_to : null,
    notDone,
  };
}
