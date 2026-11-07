// Registry: the open geospatial registry, searched through Columbus's own
// server (the same checked index, checks and audit an assistant gets).
// Entries are community-written: every word of them is shown as text, links
// included, never as markup.
import { h, el, invoke, button, pageHead, showError, emptyState, toast } from "../core.js";

const go = (hash) => { location.hash = hash; };
const rHash = (id) => `#/registry/${encodeURIComponent(id)}`;
const NOTICE = "Registry entries are community-written. Check the source, licence and access with the publisher before you rely on one.";

let lastQuery = { query: "", kind: "", theme: "" };

function licence(l) {
  if (!l) return "licence not recorded";
  return String(l).replace(/^LicenseRef-/, "").replace(/-/g, " ");
}

function reviewBadge(r) {
  const state = typeof r === "object" && r ? r.state : r;
  return state === "seeded"
    ? el("span", "badge warn", "not yet reviewed by a person")
    : state ? el("span", "badge pass", "reviewed") : null;
}

function checksList(checks) {
  const items = (checks || []).filter((c) => c.id !== "registry.no_match");
  if (!items.length) return null;
  return h("ul", { class: "small muted", id: "rg-checks" }, ...items.map((c) => el("li", null, c.message)));
}

export async function render(main, rest) {
  const [id] = (rest || []).map((x) => decodeURIComponent(x));
  const page = h("div", { class: "page" });
  main.append(page);
  const body = h("div", { class: "pad stack" });
  const err = h("p", { id: "rg-error", class: "error-text", role: "alert", hidden: true });
  if (id) return entryView(id);
  return searchView();

  function notInstalled(e) {
    if (!/not installed/i.test(String(e))) return null;
    const btn = button("Install the registry index", "primary", async () => {
      btn.disabled = true;
      btn.textContent = "Installing…";
      try {
        const msg = await invoke("registry_update");
        toast(msg || "Installed.");
        go("#/registry");
        location.reload();
      } catch (x) {
        showError(err, x);
        btn.disabled = false;
        btn.textContent = "Install the registry index";
      }
    }, { id: "rg-install" });
    return emptyState("The registry index isn't installed yet",
      "It's a file of about a few megabytes, downloaded from the registry's published release and checked against its manifest before it's used. Your assistant uses the same index.", btn);
  }

  // ---- Search ----------------------------------------------------------
  function searchView() {
    page.append(pageHead("Registry", "Open data sources and GIS tools: who publishes them, what they cover, their licence, and how to get or install them."));
    page.append(h("div", { class: "scroll" }, body));
    const q = h("input", { type: "search", id: "rg-q", placeholder: "Words to look for: lidar texas, flood zones, geocoding library", class: "grow", value: lastQuery.query });
    const kind = h("select", { id: "rg-kind", "aria-label": "Kind" },
      h("option", { value: "" }, "Data and tools"), h("option", { value: "data" }, "Data sources"), h("option", { value: "tool" }, "Tools"));
    kind.value = lastQuery.kind;
    const theme = h("input", { type: "text", id: "rg-theme", placeholder: "Theme (optional): elevation, hydrography…", value: lastQuery.theme });
    const go1 = button("Search", "primary", () => search(), { id: "rg-search" });
    const results = h("div", { id: "rg-results", class: "stack" });
    body.append(h("form", { class: "row wrap", onsubmit: (e) => { e.preventDefault(); search(); } }, q, kind, theme, go1),
      h("p", { class: "small muted" }, NOTICE), err, results);
    q.focus();
    if (lastQuery.query || lastQuery.kind || lastQuery.theme) search();

    async function search() {
      showError(err, null);
      lastQuery = { query: q.value.trim(), kind: kind.value, theme: theme.value.trim() };
      const args = { limit: 30 };
      if (lastQuery.query) args.query = lastQuery.query;
      if (lastQuery.kind) args.kind = lastQuery.kind;
      if (lastQuery.theme) args.theme = lastQuery.theme;
      results.replaceChildren(el("p", "muted small", "Searching…"));
      go1.disabled = true;
      try {
        const r = await invoke("registry_query", { tool: "registry_search", args });
        const cards = r.result?.cards || [];
        results.replaceChildren();
        const c = checksList(r.checks);
        if (c) results.append(c);
        if (!cards.length) { results.append(emptyState("No entries matched", "Try fewer or broader words, or another kind or theme.")); return; }
        const ul = h("ul", { class: "list", id: "rg-list" });
        for (const card of cards) {
          const li = el("li", "tool clickable");
          li.dataset.id = card.id;
          li.addEventListener("click", (ev) => { if (!ev.target.closest("a")) go(rHash(card.id)); });
          const where = [card.kind === "data" ? "Data" : "Tool", card.region || card.coverage?.region, card.category, licence(card.license)].filter(Boolean).join(" · ");
          li.append(h("div", { class: "text" },
            h("b", {}, h("a", { href: rHash(card.id) }, card.name || card.id)),
            h("span", { class: "where" }, where),
            card.summary ? h("span", { class: "muted small" }, card.summary) : null),
          h("div", { class: "acts" }, reviewBadge(card.review), card.status && card.status !== "active" ? el("span", "badge fail", card.status) : null));
          ul.append(li);
        }
        results.append(h("section", { class: "card" }, h("div", { class: "hd" }, h("h2", {}, `${cards.length} entr${cards.length === 1 ? "y" : "ies"}`)), h("div", { class: "bd flush" }, ul)));
      } catch (e) {
        results.replaceChildren();
        const ni = notInstalled(e);
        if (ni) results.append(ni);
        else showError(err, e);
      } finally {
        go1.disabled = false;
      }
    }
  }

  // ---- One entry -------------------------------------------------------
  async function entryView(eid) {
    page.append(pageHead("Registry entry"));
    page.append(h("div", { class: "scroll" }, body));
    body.append(h("p", { class: "small" }, h("a", { href: "#/registry", id: "rg-back" }, "← Search")), err);
    let r;
    try {
      r = await invoke("registry_query", { tool: "registry_get", args: { id: eid } });
    } catch (e) {
      const ni = notInstalled(e);
      if (ni) body.append(ni);
      else showError(err, e);
      return;
    }
    const e = r.result?.entry;
    if (!e) { body.append(emptyState("Not found", "The registry has no entry with this id.")); return; }
    document.getElementById("page-title").textContent = e.name || e.id;
    const rows = [];
    const add = (label, value) => { if (value != null && value !== "" && !(Array.isArray(value) && !value.length)) rows.push([label, value]); };
    add("Kind", e.kind === "data" ? "Data source" : "Tool");
    add("Publisher", e.publisher);
    add("Coverage", e.coverage ? [e.coverage.region, e.coverage.bbox ? `extent ${e.coverage.bbox.join(", ")}` : null].filter(Boolean).join(" · ") : null);
    add("Themes", (e.themes || []).join(", "));
    add("Category", [e.category, e.subcategory].filter(Boolean).join(" › "));
    add("Licence", [licence(e.license), e.license_original && e.license_original !== licence(e.license) ? `(${e.license_original})` : null].filter(Boolean).join(" "));
    add("Updated", e.update_cadence || e.cadence);
    add("Status", e.status);
    add("Web page", e.url);
    const dl = h("dl", { class: "facts", id: "rg-facts" });
    for (const [k, v] of rows) dl.append(h("dt", {}, k), h("dd", { class: "wrap-any" }, String(v)));
    const access = (e.access || []).map((a) => h("li", { class: "wrap-any" }, [a.type, a.auth && a.auth !== "none" ? `sign-in: ${a.auth}` : null].filter(Boolean).join(" · "), a.url ? h("div", {}, h("code", {}, String(a.url))) : null));
    const install = e.install ? h("pre", { class: "box small wrap-any" }, typeof e.install === "string" ? e.install : JSON.stringify(e.install, null, 2)) : null;
    const ask = button("Ask my assistant about it", "primary", () => {
      try { sessionStorage.setItem("columbus.prefill", `Tell me about the registry entry ${e.id} (${e.name}): what it covers, its licence, and how I'd use it in Columbus.`); } catch { /* no prefill */ }
      go("#/chats/fresh");
    }, { id: "rg-ask" });
    body.append(h("section", { class: "card" }, h("div", { class: "bd stack" },
      h("div", { class: "row wrap" }, reviewBadge(e.review), e.status && e.status !== "active" ? el("span", "badge fail", e.status) : null),
      e.summary ? h("p", {}, e.summary) : null, dl,
      access.length ? h("div", {}, h("h3", {}, "How to get it"), h("ul", { class: "small" }, ...access)) : null,
      install ? h("div", {}, h("h3", {}, "Install"), install) : null,
      checksList(r.checks),
      h("p", { class: "small muted" }, NOTICE, " Links are shown as text: copy one into your browser if you want to visit it."),
      h("div", { class: "row" }, ask))));
  }
}
