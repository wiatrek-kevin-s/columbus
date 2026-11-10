// Where the page's sheets go, and how much of the map they cover.
//
// The map fills the page; sheets float on it. Something always covers part
// of the map: the Columbus app's own panel when the page is framed (the app
// says how much, as an `inset`), or the page's own panel otherwise; and the
// table sheet while it is open. The map is told the same numbers as its
// padding, so every "zoom to" centres in the part still in view.
//
// Pure, with no imports at run time, so `e2e/unit.mjs` can check it without
// a browser.

/** CSS pixels covered at each edge of the page. */
export interface Inset {
  left: number;
  top: number;
  right: number;
  bottom: number;
}

export const NO_INSET: Inset = { left: 0, top: 0, right: 0, bottom: 0 };

/** The space around every sheet. */
export const GAP = 16;

const SIDES = ["left", "top", "right", "bottom"] as const;
const MAX_INSET = 4000;
// The longest handle the app passes on (the chat keeps plain ids of at most
// this length).
const MAX_HANDLE = 200;
// The page's own panel; wider while a request can be decided in it, so the
// code and the facts read without wrapping every few words.
const PANEL = 400;
const PANEL_DECIDING = 600;
// A window narrower than this puts the panel above the map.
const STACK_BELOW = 760;
// How much of the map stays in view each way, whatever covers the rest.
const MAP_MIN = 120;

/** The app's `inset`: finite numbers from 0 to 4000, a side left out
 * counting as 0. Anything else is refused whole (null), and the page keeps
 * what it had. */
export function parseInset(v: unknown): Inset | null {
  if (!v || typeof v !== "object" || Array.isArray(v)) return null;
  const given = v as Record<string, unknown>;
  const out: Inset = { ...NO_INSET };
  for (const side of SIDES) {
    const n = given[side];
    if (n === undefined) continue;
    if (typeof n !== "number" || !Number.isFinite(n) || n < 0 || n > MAX_INSET) return null;
    out[side] = n;
  }
  return out;
}

/** The result the app names when it asks for the table (`output`): taken
 * only when it is a short string equal to the handle of one of this
 * session's own outputs that has rows to show. Anything else names nothing
 * (null), and the page keeps the result it had. */
export function namedOutput(v: unknown, outputs: readonly { handle: string; viewable: boolean }[]): string | null {
  if (typeof v !== "string" || v.length === 0 || v.length > MAX_HANDLE) return null;
  const o = outputs.find((x) => x.handle === v);
  return o && o.viewable ? o.handle : null;
}

const clamp = (n: number, lo: number, hi: number) => Math.min(Math.max(n, lo), Math.max(lo, hi));

/** The inset, shrunk where it would leave less than `MAP_MIN` of the page
 * in view. */
function within(i: Inset, width: number, height: number): Inset {
  const fit = (a: number, b: number, room: number): [number, number] => {
    const most = Math.max(0, room - MAP_MIN);
    if (a + b <= most) return [a, b];
    const k = most / (a + b);
    return [Math.floor(a * k), Math.floor(b * k)];
  };
  const [left, right] = fit(i.left, i.right, width);
  const [top, bottom] = fit(i.top, i.bottom, height);
  return { left, top, right, bottom };
}

export interface Layout {
  /** What covers the page's edges: the app's panel, or the page's own. */
  inset: Inset;
  /** The page's own panel (never when framed), from the page's top left
   * corner plus `GAP`. */
  panel: { width: number; height: number } | null;
  /** The table sheet's height; 0 while it is closed. */
  table: number;
  /** The map's padding: the inset, and the table sheet while it is open. */
  pad: Inset;
}

export function layout(o: {
  width: number;
  height: number;
  /** What already covers the page: the app's inset when it frames the
   * page. Null: nothing does, and the page shows its own panel. */
  covered: Inset | null;
  tableOpen: boolean;
  /** A request can be decided in this window now. */
  deciding?: boolean;
}): Layout {
  const w = Math.max(0, o.width);
  const h = Math.max(0, o.height);
  let inset: Inset;
  let panel: Layout["panel"] = null;
  if (o.covered) {
    inset = within(o.covered, w, h);
  } else if (w >= STACK_BELOW) {
    // Beside the map, full height. Wider while deciding, as far as that
    // leaves the map a fair share.
    const want = o.deciding ? clamp(w - 2 * GAP - 360, PANEL, PANEL_DECIDING) : PANEL;
    const width = Math.min(want, w - 2 * GAP);
    panel = { width, height: Math.max(0, h - 2 * GAP) };
    inset = within({ ...NO_INSET, left: GAP + width + GAP }, w, h);
  } else {
    // Above the map, on a narrow window.
    const height = Math.round(clamp(h * 0.46, 160, h - 2 * GAP - MAP_MIN));
    panel = { width: Math.max(0, w - 2 * GAP), height };
    inset = within({ ...NO_INSET, top: GAP + height + GAP }, w, h);
  }
  // About 40% of the page, at least 240 px, and never so tall that the map
  // above it is lost.
  const free = h - inset.top - inset.bottom;
  const table = o.tableOpen ? Math.round(clamp(Math.max(240, h * 0.4), 120, free - 2 * GAP - MAP_MIN)) : 0;
  const pad = within({ ...inset, bottom: inset.bottom + (table ? table + GAP : 0) }, w, h);
  return { inset, panel, table, pad };
}
