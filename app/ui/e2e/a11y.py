"""A check shared by the map page's scripts (Playwright) and the main
window's (WebDriver, through app/desktop/e2e/guard.py): the controls on show
that a screen reader could not name, or that Tab cannot reach.

A name is an aria-label, a label, the control's own text or its title; a
placeholder alone is not one. It looks in the dialog that is open, else in
the whole page. The 0.14 audit left accessibility (B10) to the rebuilt
window; this keeps what was checked then from slipping.
"""

UNNAMED = """
const seen = (e) => { const r = e.getBoundingClientRect(); const cs = getComputedStyle(e); return r.width > 0 && r.height > 0 && cs.visibility !== 'hidden' && cs.display !== 'none'; };
const name = (e) => {
  const said = e.getAttribute('aria-label'); if (said && said.trim()) return said.trim();
  const by = e.getAttribute('aria-labelledby');
  if (by) { const t = by.split(/\\s+/).map((i) => (document.getElementById(i) || {}).textContent || '').join(' ').trim(); if (t) return t; }
  if (e.matches('input, select, textarea')) {
    if (e.id) { const l = document.querySelector('label[for="' + CSS.escape(e.id) + '"]'); if (l && l.textContent.trim()) return l.textContent.trim(); }
    const around = e.closest('label'); if (around && around.textContent.trim()) return around.textContent.trim();
    return e.title || (e.type === 'submit' || e.type === 'button' ? e.value : '');
  }
  return (e.textContent || '').trim() || e.title || '';
};
const open = [...document.querySelectorAll('dialog[open]')];
const root = open.length ? open[open.length - 1] : document.body;
const out = [];
for (const e of root.querySelectorAll('button, a[href], input:not([type=hidden]), select, textarea, summary, [role=button], [tabindex]:not([tabindex="-1"])')) {
  if (!seen(e) || e.closest('[aria-hidden=true]')) continue;
  const what = e.tagName.toLowerCase() + (e.id ? '#' + e.id : '') + (typeof e.className === 'string' && e.className.trim() ? '.' + e.className.trim().split(/\\s+/).join('.') : '');
  if (!name(e)) out.push(what + ': no name');
  else if (e.matches('button, a[href], input, select, textarea') && e.tabIndex < 0 && !e.disabled) out.push(what + ': not reachable with Tab');
}
return out;
"""


def unnamed(page):
    """The same, for a Playwright page."""
    return page.evaluate("() => {" + UNNAMED + "}")
