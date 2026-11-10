// Web map layers (T-25, REQ-33), opened from the foot of the outputs list:
// the saved web layers (Data › Map layers in the Columbus app, or
// the organization's), each off until the person turns it on in this chat,
// with opacity, order, zoom to extent and choose as basemap, all remembered
// per chat. The assistant never sees any of this.
import { useState, type ReactNode } from "react";
import type { WebSource } from "./api";
import type { WebState } from "./MapView";

export const PRIVACY_NOTICE = "Web layers' services see which area you're viewing";

/** This chat's choices, kept in the window's storage under its session. */
export interface WebPrefs {
  /** Turned on, by id. */
  on: string[];
  /** 0..1 by id; 1 when not set. */
  opacity: Record<string, number>;
  /** The list's order, top first (the top one draws above the others). */
  order: string[];
  /** A raster web layer chosen as the basemap, or null. */
  base: string | null;
  /** Whether the privacy notice has been shown in this chat. */
  noticed: boolean;
}

export const NO_WEB_PREFS: WebPrefs = { on: [], opacity: {}, order: [], base: null, noticed: false };

/** The sources in the list's order, top first; new ones go to the bottom. */
export function ordered(list: WebSource[], order: string[]): WebSource[] {
  const at = new Map(order.map((id, i) => [id, i]));
  return [...list].sort((a, b) => (at.get(a.id) ?? 1e9) - (at.get(b.id) ?? 1e9));
}

const KIND: Record<WebSource["kind"], string> = {
  xyz: "Tiles",
  wmts: "WMTS",
  wms: "WMS",
  "arcgis-map": "ArcGIS map",
  "arcgis-image": "ArcGIS image",
  "arcgis-features": "ArcGIS features",
};

export function WebLayers(props: {
  enabled: boolean;
  list: WebSource[] | null;
  listError: string | null;
  prefs: WebPrefs;
  state: Record<string, WebState>;
  streetsAllowed: boolean;
  streetsOn: boolean;
  notice: boolean;
  framed: boolean;
  onToggle: (id: string, on: boolean) => void;
  onOpacity: (id: string, v: number) => void;
  onMove: (id: string, by: -1 | 1) => void;
  onZoom: (id: string) => void;
  onBasemap: (choice: "streets" | "none" | { web: string }) => void;
  onDismissNotice: () => void;
  onAdd: () => void;
  /** What shares the button's row (Measure). */
  beside?: ReactNode;
}) {
  const [open, setOpen] = useState(false);
  const [addHint, setAddHint] = useState(false);
  const list = ordered(props.list || [], props.prefs.order);
  const rasters = list.filter((s) => s.raster);
  const value = props.prefs.base ? `web:${props.prefs.base}` : props.streetsOn && props.streetsAllowed ? "streets" : "none";
  const onCount = props.prefs.on.filter((id) => list.some((s) => s.id === id)).length;
  return (
    <div className="web-layers">
      <div className="web-bar">
        <button type="button" className="btn sm" aria-expanded={open} aria-controls="web-panel" onClick={() => setOpen(!open)}>
          Web layers{onCount ? ` (${onCount})` : ""}
        </button>
        {props.beside}
      </div>
      {open && (
        <section id="web-panel" className="web-panel" aria-label="Web layers">
          {props.notice && (
            <div className="web-notice" role="status">
              <span>{PRIVACY_NOTICE}.</span>
              <button type="button" className="btn sm" onClick={props.onDismissNotice}>
                OK
              </button>
            </div>
          )}
          <label className="web-base">
            Basemap
            <select
              aria-label="Basemap"
              value={value}
              onChange={(e) => {
                const v = e.target.value;
                props.onBasemap(v === "streets" || v === "none" ? v : { web: v.slice(4) });
              }}
            >
              {props.streetsAllowed && <option value="streets">Streets</option>}
              <option value="none">None</option>
              {rasters.map((s) => (
                <option key={s.id} value={`web:${s.id}`}>
                  {s.title}
                </option>
              ))}
            </select>
          </label>
          {!props.enabled && <p className="muted">Your organization has turned web layers off.</p>}
          {props.listError && <p className="error">{props.listError}</p>}
          {props.enabled && list.length === 0 && !props.listError && (
            <p className="muted">No web layers yet. Add imagery or another map service by its address.</p>
          )}
          <ul className="web-list">
            {list.map((s, i) => {
              const on = props.prefs.on.includes(s.id);
              const st = props.state[s.id] || {};
              const op = props.prefs.opacity[s.id] ?? 1;
              const isBase = props.prefs.base === s.id;
              return (
                <li key={s.id} className={on ? "web-row on" : "web-row"} data-id={s.id} data-drawn={st.drawn ? "1" : "0"}>
                  <div className="web-head">
                    <input
                      type="checkbox"
                      aria-label={`Show ${s.title}`}
                      checked={on}
                      onChange={(e) => props.onToggle(s.id, e.target.checked)}
                    />
                    <span className="web-title">{s.title}</span>
                    <span className="web-kind">
                      {KIND[s.kind] || s.kind}
                      {s.origin === "policy" ? " · organization" : ""}
                      {isBase ? " · basemap" : ""}
                    </span>
                  </div>
                  {on && (
                    <div className="web-tools">
                      <label>
                        Opacity
                        <input
                          type="range"
                          min={0}
                          max={100}
                          step={5}
                          aria-label={`Opacity of ${s.title}`}
                          value={Math.round(op * 100)}
                          onChange={(e) => props.onOpacity(s.id, Number(e.target.value) / 100)}
                        />
                      </label>
                      <button type="button" className="btn sm" aria-label={`Move ${s.title} up`} disabled={i === 0} onClick={() => props.onMove(s.id, -1)}>
                        Up
                      </button>
                      <button
                        type="button"
                        className="btn sm"
                        aria-label={`Move ${s.title} down`}
                        disabled={i === list.length - 1}
                        onClick={() => props.onMove(s.id, 1)}
                      >
                        Down
                      </button>
                      <button type="button" className="link" onClick={() => props.onZoom(s.id)}>
                        Zoom to layer
                      </button>
                    </div>
                  )}
                  {(on || isBase) && st.exceeded && (
                    <p className="note" data-total={st.total}>
                      Showing {(st.shown ?? 0).toLocaleString()} of {(st.total ?? 0).toLocaleString()} features. Zoom in to see all.
                    </p>
                  )}
                  {(on || isBase) && st.error && <p className="error">{st.error}</p>}
                </li>
              );
            })}
          </ul>
          <div className="web-add">
            <button type="button" className="btn sm" onClick={() => (props.framed ? props.onAdd() : setAddHint(true))}>
              Add web layer
            </button>
            {addHint && (
              <p className="muted">
                Add web layers in the Columbus app, under Data › Map layers, by pasting the service's address.
              </p>
            )}
          </div>
        </section>
      )}
    </div>
  );
}
