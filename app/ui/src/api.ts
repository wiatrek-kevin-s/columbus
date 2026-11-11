// Talking to the Columbus server that served this page.
//
// The page is opened on /#t=<launch token>. The token is single-use: it is
// traded once for a session token, which is kept only in this module's
// memory (never a cookie or browser storage) and sent as a Bearer header.
// The launch token is removed from the address bar straight away.

import type { ViewReport } from "./view";

export type Severity = "fail" | "warn" | "info";

export interface Check {
  id: string;
  severity: Severity;
  message: string;
  detail?: unknown;
  fix?: string;
}

export interface OutputCard {
  handle: string;
  name: string;
  folder: string;
  kind: "layer" | "table" | "raster" | "file";
  bytes: number;
  modified: string | null;
  made_by: string | null;
  rows: number | null;
  truncated: boolean;
  provenance: string | null;
  checks: { fail: number; warn: number; info: number } | null;
  labels: string[];
  viewable: boolean;
}

export interface Overview {
  session: string;
  user: string;
  server: string;
  workspace: string;
  policy: {
    connection_types: string[];
    scripts_allowed: boolean;
    edits_allowed: boolean;
    rows_returned_per_call: number | null;
  };
  connections: { id: string; type: string; description: string | null }[];
  export_folders: { id: string; path: string; granted: boolean }[];
  context_pack: string | null;
  basemap_allowed: boolean;
  notes: string[];
}

export interface Approval {
  id: string;
  tool: string;
  risk: string;
  summary: string;
  request: unknown;
  expires: string;
  state: string;
  offer_always?: boolean;
  always_covers?: string;
  detail?: {
    /** An edit lists its changes as lines; notes come one by one, each
     * with its own lines. */
    changes?: (string | { id: string; changes: string[] })[];
    destination?: string;
    /** Exports name the files they write (paths); scripts and edits give a
     * name and its rows. */
    files?: (string | { name: string; rows?: number | null })[];
    labels?: string[];
    flow_rule?: string;
    arguments?: Record<string, unknown>;
    code?: string;
    lines?: number;
  };
}

export interface FeatureCollection {
  type: "FeatureCollection";
  features: GeoJSON.Feature[];
  total: number;
  shown: number;
  truncated: boolean;
  geometry: boolean;
  geometry_type?: string | null;
  bbox?: [number, number, number, number] | null;
  source_crs?: string;
  /** Set by the page while the layer is being read; never sent by the server. */
  loading?: boolean;
}

export interface Rows {
  columns: string[];
  rows: unknown[][];
  total: number;
  offset: number;
}

export class SignedOut extends Error {}

/** What this window may do: a window the Columbus app opened can only
 * look; deciding happens in the browser page Columbus opens for it. */
export type Scope = "approve" | "view";

let token: string | null = null;
let scope: Scope = "view";

export const currentScope = () => scope;

/** Trade the launch token in the URL for a session, if there is one. */
export async function signIn(): Promise<"signed-in" | "no-link" | "used"> {
  const m = /[#&]t=([0-9a-f]{64})/.exec(location.hash);
  if (!m) return token ? "signed-in" : "no-link";
  history.replaceState(null, "", location.pathname);
  const r = await fetch("/api/session", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ launch: m[1] }),
    cache: "no-store",
  });
  if (!r.ok) return "used";
  const body = await r.json();
  token = body.token;
  scope = body.scope === "approve" ? "approve" : "view";
  return "signed-in";
}

async function call<T>(path: string, init: RequestInit = {}): Promise<T> {
  if (!token) throw new SignedOut("not signed in");
  const r = await fetch(path, {
    ...init,
    cache: "no-store",
    headers: { ...(init.headers || {}), Authorization: `Bearer ${token}` },
  });
  if (r.status === 401 || r.status === 403) {
    token = null;
    throw new SignedOut("signed out");
  }
  const body = await r.json().catch(() => ({}));
  if (!r.ok) throw new Error(body.error || `${r.status}`);
  return body as T;
}

const q = (handle: string) => encodeURIComponent(handle);

// ---------- Web map layers (T-25; the server side is T-24) ----------

/** A web layer the person can turn on: an id and a title, never an address. */
export interface WebSource {
  id: string;
  title: string;
  kind: "xyz" | "wmts" | "wms" | "arcgis-map" | "arcgis-image" | "arcgis-features";
  raster: boolean;
  attribution: string | null;
  secured: boolean;
  origin: "saved" | "policy";
  /** Changes when the layer is edited (address, layer or sign-in). */
  rev?: string;
}

/** What the map needs to draw one web layer. */
export interface WebSourceInfo {
  id: string;
  title: string;
  kind: WebSource["kind"];
  raster: boolean;
  attribution: string | null;
  secured: boolean;
  extent: [number, number, number, number] | null;
  min_zoom?: number | null;
  max_zoom?: number | null;
  tiles?: string;
  tile_size?: number;
  features?: string;
  max_features?: number;
  fields?: string[];
  geometry_type?: string | null;
}

export interface WebFeatures {
  type: "FeatureCollection";
  features: GeoJSON.Feature[];
  total: number;
  exceeded: boolean;
}

/** A web layer's own failure (R-14 wording from the server): the session
 * is still signed in. */
export class WebLayerError extends Error {
  constructor(
    message: string,
    public status: number,
  ) {
    super(message);
  }
}

/** The header the map sends with its own requests to this server's web
 * layer addresses (MapLibre's transformRequest): never in a URL. */
export function authorization(): string | null {
  return token ? `Bearer ${token}` : null;
}

/** Only this server's web-layer addresses get the session token. */
export function isWebLayerUrl(url: string): boolean {
  return url.startsWith(`${location.origin}/api/web/`);
}

async function web<T>(path: string, signal?: AbortSignal): Promise<T> {
  if (!token) throw new SignedOut("not signed in");
  const r = await fetch(path, { cache: "no-store", signal, headers: { Authorization: `Bearer ${token}` } });
  const body = await r.json().catch(() => ({}));
  // The session's own refusal (its token gone) signs the window out; a
  // layer's (needs sign-in, host refused, service down) does not.
  if ((r.status === 401 && body.error === "not signed in") || (r.status === 403 && body.error === "wrong host")) {
    token = null;
    throw new SignedOut("signed out");
  }
  if (!r.ok) throw new WebLayerError(body.error || `the server answered ${r.status}`, r.status);
  return body as T;
}

export const webApi = {
  sources: () => web<{ enabled: boolean; sources: WebSource[]; notes: string[] }>("/api/web/sources"),
  source: (id: string) => web<WebSourceInfo>(`/api/web/source/${q(id)}`),
  features: (id: string, bbox: [number, number, number, number], signal?: AbortSignal) =>
    web<WebFeatures>(`/api/web/features/${q(id)}?bbox=${bbox.map((n) => n.toFixed(6)).join(",")}`, signal),
};

export const api = {
  state: () => call<{ session: string; version: number; pending: number; scope: Scope }>("/api/state"),
  review: () => call<{ opened: boolean }>("/api/review", { method: "POST" }),
  overview: () => call<Overview>("/api/overview"),
  outputs: () => call<{ outputs: OutputCard[]; version: number }>("/api/outputs"),
  layer: (handle: string) => call<FeatureCollection>(`/api/layer?handle=${q(handle)}`),
  rows: (handle: string, offset: number, limit: number) =>
    call<Rows>(`/api/rows?handle=${q(handle)}&offset=${offset}&limit=${limit}`),
  provenance: (id: string) => call<Record<string, unknown>>(`/api/provenance/${encodeURIComponent(id)}`),
  /** What the map shows, for the assistant's `map_view`; the answer carries
   * the change the assistant asked for, if one waits (checked by
   * `parseCommand`). Given up after ten seconds, so that one report that
   * never answers does not hold up the next. */
  view: (report: ViewReport) => {
    const stop = new AbortController();
    const timer = window.setTimeout(() => stop.abort(), 10_000);
    return call<{ command: unknown }>("/api/view", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(report),
      signal: stop.signal,
    }).finally(() => window.clearTimeout(timer));
  },
  approvals: () => call<{ session: string; pending: Approval[]; recent: Approval[] }>("/api/approvals"),
  decide: (id: string, decision: "allow_once" | "always_this_session" | "decline") =>
    call<{ id: string; state: string }>(`/api/approvals/${encodeURIComponent(id)}`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ decision }),
    }),
};
