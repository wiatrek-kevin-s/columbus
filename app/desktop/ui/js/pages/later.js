// Pages the design has but Columbus does not have a backend for yet. Each
// says honestly what it will do and that it is coming; none shows made-up
// data.
import { h, store, pageHead, coming, ago, plural, clientName } from "../core.js";
import { liveSession } from "./chats.js";

function page(main, title, lede, ...content) {
  const p = h("div", { class: "page" }, pageHead(title, lede), h("div", { class: "scroll" }, h("div", { class: "pad stack" }, ...content)));
  main.append(p);
}

export function projects(main) {
  const list = h("ul", { class: "list" });
  for (const c of store.chats) list.append(h("li", { class: "clickable", onclick: () => { location.hash = `#/chats/c/${encodeURIComponent(c.id)}`; } }, h("div", { class: "text" }, h("b", {}, c.title), h("span", { class: "where" }, `Chat in this app · ${ago(c.updated)}`))));
  for (const s of store.sessions.filter((x) => x.session !== liveSession())) list.append(h("li", { class: "clickable", onclick: () => { location.hash = `#/chats/s/${encodeURIComponent(s.session)}`; } }, h("div", { class: "text" }, h("b", {}, `${clientName(s.client)} session`), h("span", { class: "where" }, "Running now"))));
  const n = store.chats.length + store.sessions.filter((x) => x.session !== liveSession()).length;
  page(main, "Projects", "A project is a piece of work: its instructions, chats, documents and the data its chats may use.",
    h("section", { class: "card" }, h("div", { class: "hd" }, h("h2", {}, "Unfiled"), h("span", { class: "muted small" }, plural(n, "chat"))),
      h("div", { class: "bd flush" }, n ? list : h("p", { class: "pad muted" }, "No chats yet."))),
    coming("projects", "Projects are coming",
      "Each project will have its instructions (given to every chat in it), its chats and documents, and context controls: which connections, data groups, notes and skills its chats may use, enforced by Columbus's server. It will also own a project database and a default export folder.",
      ["Until then every chat is Unfiled.", "Your personal projects start private to you; sharing comes later."], "Coming in stage 3"));
}

export function dataGroups(main) {
  page(main, "Data groups", "Named sets of datasets across connections, with their filters.",
    coming("datagroups", "Data groups are coming",
      "Build a named set of datasets across your connections (for example the District 14 roadway core), attach it to a project, or mention it in a chat (@d14-roadway-core) instead of listing tables. A group can only narrow what each connection allows; Columbus's server enforces it.",
      ["Your groups start private to you; sharing comes later.", "Your organization's groups will come from its context pack, read only."], "Coming in stage 3"));
}

export function registry(main) {
  page(main, "Registry", "Open data and geoprocessing tools, compared across engines.",
    coming("registry", "The registry page is coming",
      "Search open data by place and theme with coverage on a map, and fetch what is in view into a chat's workspace; look up any tool by what it does or by any engine's name for it (dissolve, v.dissolve, ST_Union) and see its equivalents across ArcGIS Pro, QGIS, GRASS, PostGIS and Python, with what is installed here.",
      ["Your assistant can already search the registry today (registry_search, search_tools)."], "Coming in stage 3"));
}

