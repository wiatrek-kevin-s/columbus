// The session list. Everything shown is set as text, never as markup.
const { invoke } = window.__TAURI__.core;
const list = document.getElementById("sessions");
const empty = document.getElementById("empty");
const error = document.getElementById("error");
let shown = "";

function when(iso) {
  const d = new Date(iso);
  return isNaN(+d) ? "" : `Started ${d.toLocaleString([], { dateStyle: "medium", timeStyle: "short" })}`;
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
  const key = JSON.stringify(sessions);
  if (key === shown) return;
  shown = key;
  list.replaceChildren();
  empty.hidden = sessions.length > 0;
  for (const s of sessions) {
    const li = el("li", "session");
    const text = el("div", "text");
    text.append(el("span", "name", s.user ? `${s.user}'s session` : "Session"));
    text.append(el("span", "when", when(s.started)));
    text.append(el("span", "id", `${s.session}${s.server ? `, ${s.server}` : ""}`));
    const b = el("button", "primary", "Open");
    b.type = "button";
    b.addEventListener("click", () => open(s.port, b));
    li.append(text, b);
    list.append(li);
  }
}

refresh();
setInterval(refresh, 3000);
