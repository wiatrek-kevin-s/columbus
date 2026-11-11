// Settings › Git servers: the servers a project may sync with. GitHub, a
// GitHub Enterprise Server, a Gitea or a Forgejo is connected by pasting a
// token here. The token goes to the app's own code, which asks the server
// whose it is and keeps it in the computer's credential store; this page
// can give one and remove a server, and can never read one back. Whether
// sync is allowed at all, and with which hosts, is the organization's
// (allow_sync, sync.hosts), checked in the app's own code.
import { h, el, invoke, showError, toast, button, sec, openDialog, askConfirm } from "../core.js";

/** The part's address under Settings, and its name. */
export const PART = ["git", "Git servers"];
export const LEDE = "The servers your projects can sync with: GitHub, or your organization's own Gitea, Forgejo or GitHub Enterprise Server. You connect one with a token; your assistant cannot.";

const KINDS = [
  ["github", "GitHub (github.com)"],
  ["github-enterprise", "GitHub Enterprise Server"],
  ["gitea", "Gitea"],
  ["forgejo", "Forgejo"],
];
const KIND_NAME = { github: "GitHub", gitea: "Gitea or Forgejo" };
// What the token must be allowed to do, by kind, in the server's own words.
const TOKEN_HELP = {
  github: "Opens GitHub's page for a new token, set to read and write the contents of the repositories you choose there. For Columbus to create a repository too, also give it Administration: read and write.",
  "github-enterprise": "Opens your server's page for tokens. The token must be allowed to read and write the contents of your repositories (for a classic token, the repo scope).",
  gitea: "Opens Settings › Applications on your server. Make a token there with the scopes read:user and write:repository.",
  forgejo: "Opens Settings › Applications on your server. Make a token there with the scopes read:user and write:repository.",
};

/** A line for the first view of Settings: what is connected. */
export function overviewLine() {
  const line = h("span", { id: "set-git-line" }, "…");
  invoke("git_servers_state").then((st) => {
    if (!st.allowed) { line.textContent = "Off in your organization's settings."; return; }
    const n = st.servers.length;
    line.textContent = n ? st.servers.map((s) => `${s.user} on ${s.host}`).join(", ") : "None connected yet.";
  }).catch(() => { line.textContent = "GitHub, or your organization's Gitea."; });
  return line;
}

/** The part itself. `saved` is called when something was changed, so that
 * the page under the sheet is brought up to date when the sheet closes. */
export async function part(body, saved = () => {}) {
  const error = h("p", { id: "git-error", class: "error-text", role: "alert", hidden: true });
  const notice = h("p", { id: "git-notice", class: "notice ok", role: "status", hidden: true });
  const off = h("div", { id: "git-off", class: "notice warn", hidden: true });
  const noStore = h("div", { id: "git-nostore", class: "notice warn", hidden: true });
  const list = h("ul", { id: "git-servers", class: "list", "aria-live": "polite" });
  const none = h("p", { id: "git-none", class: "muted small quiet pad", hidden: true }, "No server is connected yet.");
  const connect = button("Connect a server", "sm", () => openConnect(), { id: "git-connect" });
  const card = h("section", { class: "card", id: "git-card" }, sec("Connected", null, h("span", { class: "more" }, connect)), h("div", { class: "bd flush" }, list, none));
  const foot = h("p", { id: "git-foot", class: "small muted wrap-any" });
  body.append(error, notice, off, noStore, card, foot);
  let st = null;
  const say = (text) => { notice.textContent = text; notice.hidden = false; showError(error, null); };

  const draw = () => {
    off.hidden = st.allowed;
    off.replaceChildren(h("div", {}, h("b", {}, st.why_not || "Sync is turned off."), " No server can be connected, and no project syncs. The switch for it, Allow sync, is in ",
      h("a", { href: "#/settings/org", id: "git-off-switch" }, "Settings › Organization rules"), "."));
    // With sync off, nothing is offered.
    card.hidden = !st.allowed;
    foot.hidden = !st.allowed;
    noStore.hidden = !st.allowed || !st.no_store;
    noStore.replaceChildren(h("div", {}, st.no_store || ""));
    connect.disabled = !st.allowed || !!st.no_store;
    if (st.problem) showError(error, st.problem);
    list.replaceChildren();
    none.hidden = st.servers.length > 0;
    for (const s of st.servers) {
      const li = el("li", s.problem ? "conn bad" : "conn");
      li.dataset.id = s.id;
      const text = el("div", "text");
      text.append(h("div", { class: "row wrap" }, el("b", null, s.host), el("span", "where", `${KIND_NAME[s.kind] || s.kind} · ${s.url}`)));
      text.append(el("span", "where", `Connected as ${s.user}. The token is in your computer's credential store.`));
      if (s.certificate) text.append(el("span", "where wrap-any", `Certificate file: ${s.certificate}`));
      if (s.problem) text.append(el("span", "where error-text", s.problem));
      const remove = button("Remove", "sm danger", async () => {
        const ok = await askConfirm(`Remove ${s.host}?`, "Its token is deleted from this computer. Projects linked to it keep their files and their link; they sync again when you connect it again.", "Remove server", true);
        if (!ok) return;
        remove.disabled = true;
        try {
          await invoke("git_server_remove", { id: s.id });
          saved();
          say(`Removed ${s.host}. Its token was deleted from this computer.`);
        } catch (e) { showError(error, e); }
        await refresh();
      }, { "aria-label": `Remove ${s.host}` });
      li.append(text, h("div", { class: "acts" }, remove));
      list.append(li);
    }
    foot.replaceChildren("The list of servers, without tokens, is kept in ", h("code", {}, st.file), ". ",
      st.hosts.length ? `Your organization allows these servers only: ${st.hosts.join(", ")} (sync.hosts).` : "");
  };
  const refresh = async () => {
    try { st = await invoke("git_servers_state"); draw(); } catch (e) { showError(error, e); }
  };

  const openConnect = () => {
    const kind = h("select", { id: "git-kind", name: "kind" }, ...KINDS.map(([v, t]) => h("option", { value: v }, t)));
    const url = h("input", { id: "git-url", name: "url", type: "text", autocomplete: "off", spellcheck: "false", placeholder: "https://git.example.org" });
    const token = h("input", { id: "git-token", name: "token", type: "password", autocomplete: "off", spellcheck: "false" });
    const cert = h("input", { id: "git-cert", name: "certificate", type: "text", autocomplete: "off", spellcheck: "false", placeholder: "Empty: not needed" });
    const browse = h("button", { type: "button", class: "btn", id: "git-cert-pick" }, "Choose file…");
    const tokenPage = h("button", { type: "button", class: "btn sm", id: "git-token-page" }, "Open the page for a new token");
    const tokenHelp = h("small", { id: "git-token-help" });
    const opened = h("small", { id: "git-token-opened", class: "wrap-any", hidden: true });
    const err = h("p", { id: "git-form-error", class: "error-text", role: "alert", hidden: true });
    const urlField = h("label", { class: "field wide", for: "git-url" }, "Server address", url, h("small", {}, "Where you open it in your browser, for example https://git.example.org"));
    const certField = h("label", { class: "field wide", for: "git-cert" }, "Your organization's certificate file (optional)", h("span", { class: "pick" }, cert, browse),
      h("small", {}, "Only if the server uses a certificate of your organization's own that this computer does not know (a .pem file from your IT team). The server's certificate is always checked."));
    const form = h("form", { id: "git-form", class: "stack" },
      h("div", { class: "grid2" },
        h("label", { class: "field wide", for: "git-kind" }, "Server", kind),
        urlField,
        h("div", { class: "field wide" }, h("div", { class: "row wrap" }, tokenPage), tokenHelp, opened),
        h("label", { class: "field wide", for: "git-token" }, "Token", token,
          h("small", {}, "Paste the token the server gave you. It is kept in your computer's credential store, never in a file, and is sent only to that server. This window can set or remove it, but never read it back.")),
        certField),
      err);
    const save = h("button", { type: "submit", id: "git-save", class: "btn primary", form: "git-form" }, "Connect");
    const cancel = h("button", { type: "button", class: "btn" }, "Cancel");
    const d = openDialog("Connect a Git server", form, [h("span", { class: "spacer" }), cancel, save], "Connect a Git server");
    d.id = "git-dialog";
    const byKind = () => {
      const github = kind.value === "github";
      urlField.hidden = github;
      certField.hidden = github;
      tokenHelp.textContent = TOKEN_HELP[kind.value];
      opened.hidden = true;
    };
    kind.addEventListener("change", byKind);
    byKind();
    cancel.addEventListener("click", () => d.close());
    tokenPage.addEventListener("click", async () => {
      showError(err, null);
      try {
        const page = await invoke("git_server_token_page", { kind: kind.value, url: url.value || null });
        opened.textContent = `Opened in your browser: ${page}`;
        opened.hidden = false;
      } catch (e) { showError(err, e); }
    });
    browse.addEventListener("click", async () => {
      try { const p = await invoke("git_server_pick_certificate"); if (p) cert.value = p; } catch (e) { showError(err, e); }
    });
    form.addEventListener("submit", async (ev) => {
      ev.preventDefault();
      save.disabled = true;
      save.textContent = "Checking…";
      showError(err, null);
      try {
        const s = await invoke("git_server_connect", { form: { kind: kind.value, url: url.value || null, token: token.value, certificate: cert.value || null } });
        // The token is not kept on the page a moment longer than needed.
        token.value = "";
        d.close();
        saved();
        say(`Connected to ${s.host} as ${s.user}.`);
        toast(`Connected to ${s.host} as ${s.user}.`);
        await refresh();
      } catch (e) {
        showError(err, e);
      } finally {
        save.disabled = false;
        save.textContent = "Connect";
      }
    });
    kind.focus();
  };
  await refresh();
}
