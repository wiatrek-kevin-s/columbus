// Pages the design has but Columbus does not have a backend for yet. Each
// says honestly what it will do and that it is coming; none shows made-up
// data.
import { h, pageHead, coming } from "../core.js";

function page(main, title, lede, ...content) {
  const p = h("div", { class: "page" }, pageHead(title, lede), h("div", { class: "scroll" }, h("div", { class: "pad stack" }, ...content)));
  main.append(p);
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

