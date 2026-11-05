// Talking to the Columbus server that served this page.
//
// The page is opened on /#t=<launch token>. The token is single-use: it is
// traded once for a session token, which is kept only in this module's
// memory (never a cookie or browser storage) and sent as a Bearer header.
// The launch token is removed from the address bar straight away.

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
  detail?: {
    destination?: string;
    files?: { name: string; rows?: number | null }[];
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
}

export interface Rows {
  columns: string[];
  rows: unknown[][];
  total: number;
  offset: number;
}

export class SignedOut extends Error {}

let token: string | null = null;

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
  token = (await r.json()).token;
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

export const api = {
  state: () => call<{ session: string; version: number; pending: number }>("/api/state"),
  overview: () => call<Overview>("/api/overview"),
  outputs: () => call<{ outputs: OutputCard[]; version: number }>("/api/outputs"),
  layer: (handle: string) => call<FeatureCollection>(`/api/layer?handle=${q(handle)}`),
  rows: (handle: string, offset: number, limit: number) =>
    call<Rows>(`/api/rows?handle=${q(handle)}&offset=${offset}&limit=${limit}`),
  provenance: (id: string) => call<Record<string, unknown>>(`/api/provenance/${encodeURIComponent(id)}`),
  approvals: () => call<{ session: string; pending: Approval[]; recent: Approval[] }>("/api/approvals"),
  decide: (id: string, decision: "allow_once" | "always_this_session" | "decline") =>
    call<{ id: string; state: string }>(`/api/approvals/${encodeURIComponent(id)}`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ decision }),
    }),
};
