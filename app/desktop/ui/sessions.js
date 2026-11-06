// The classic session list. Everything shown is set as text, never as markup.
const { invoke } = window.__TAURI__.core;
const list = document.getElementById("sessions");
const empty = document.getElementById("empty");
const error = document.getElementById("error");
let shown = "";

function when(iso) {
  const d = new Date(iso);
  return isNaN(+d) ? "" : `Started ${d.toLocaleString([], { dateStyle: "medium", timeStyle: "short" })}`;
}

// "Used 2 minutes ago", or "Not used yet" for a session whose assistant has
// not called Columbus (Claude desktop often starts a second, idle one).
function used(iso) {
  if (!iso) return "Not used yet";
  const d = new Date(iso);
  if (isNaN(+d)) return "";
  const min = Math.floor((Date.now() - +d) / 60000);
  if (min < 1) return "Used just now";
  if (min < 60) return `Used ${min} minute${min === 1 ? "" : "s"} ago`;
  const h = Math.floor(min / 60);
  if (h < 24) return `Used ${h} hour${h === 1 ? "" : "s"} ago`;
  return `Last used ${d.toLocaleString([], { dateStyle: "medium", timeStyle: "short" })}`;
}

function el(tag, cls, text) {
  const e = document.createElement(tag);
  if (cls) e.className = cls;
  if (text != null) e.textContent = text;
  return e;
}

async function open(port, button) {
  button.disabled = true;
  error.hidden = true;
  try {
    await invoke("open_session", { port });
  } catch (e) {
    error.textContent = String(e);
    error.hidden = false;
  } finally {
    setTimeout(() => (button.disabled = false), 1500);
  }
}

async function refresh() {
  let sessions = [];
  try {
    sessions = await invoke("sessions");
  } catch (e) {
    error.textContent = String(e);
    error.hidden = false;
  }
  // Again each minute too, so "used 2 minutes ago" stays true.
  const key = JSON.stringify(sessions) + Math.floor(Date.now() / 60000);
  if (key === shown) return;
  shown = key;
  list.replaceChildren();
  empty.hidden = sessions.length > 0;
  for (const s of sessions) {
    const li = el("li", "session");
    const text = el("div", "text");
    text.append(el("span", "name", s.user ? `${s.user}'s session` : "Session"));
    text.append(el("span", s.last_used ? "used" : "used idle", used(s.last_used)));
    text.append(el("span", "when", when(s.started)));
    text.append(el("span", "id", `${s.session}${s.server ? `, ${s.server}` : ""}`));
    const b = el("button", "primary", "Open");
    b.type = "button";
    b.addEventListener("click", () => open(s.port, b));
    li.append(text, b);
    list.append(li);
  }
}

document.getElementById("settings").addEventListener("click", () => invoke("open_settings"));

refresh();
setInterval(refresh, 3000);
