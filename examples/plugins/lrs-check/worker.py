"""LRS event checker: an example Columbus engine plugin.

Events need a route id and from/to measures; routes need a route id and an
end measure (a begin measure defaults to 0). Column names are matched
case-insensitively against common names (ROUTE_ID / RTE_ID, FROM_MEAS /
BEGIN_MEAS, TO_MEAS / END_MEAS, …).
"""

from collections import defaultdict

from columbus_plugin import Plugin, Severity, ToolError, write_csv, write_geojson

plugin = Plugin("contoso.lrs-check", "1.2.0")

ROUTE = ("route_id", "rte_id", "routeid", "route")
FROM = ("from_meas", "from_measure", "begin_meas", "bmp", "frm_meas")
TO = ("to_meas", "to_measure", "end_meas", "emp")
ROUTE_END = ("to_meas", "end_meas", "max_meas", "length_mi", "route_length", "emp")
ROUTE_BEGIN = ("from_meas", "begin_meas", "min_meas", "bmp")


def pick(columns, names, what, required=True):
    lower = {c.lower(): c for c in columns}
    for n in names:
        if n in lower:
            return lower[n]
    if required:
        raise ToolError(f"no {what} column; expected one of {', '.join(names)}")
    return None


def first_point(geom):
    if geom is None or geom.is_empty:
        return None
    x, y = next(geom.points())
    return {"type": "Point", "coordinates": [x, y]}


@plugin.tool("check_events", params=["events", "routes", "tolerance"], outputs=["findings", "report"])
def check_events(ctx):
    tol = float(ctx.param("tolerance", 0.001))
    routes = ctx.dataset("routes")
    events = ctx.dataset("events")

    r_id = pick(routes.columns, ROUTE, "route id")
    r_end = pick(routes.columns, ROUTE_END, "route end measure")
    r_begin = pick(routes.columns, ROUTE_BEGIN, "route begin measure", required=False)
    extent = {}
    for row in routes.rows():
        extent[str(row[r_id])] = (float(row[r_begin]) if r_begin and row[r_begin] is not None else 0.0, float(row[r_end]))

    e_id = pick(events.columns, ROUTE, "route id")
    e_from = pick(events.columns, FROM, "from measure")
    e_to = pick(events.columns, TO, "to measure")

    findings = []  # (check, severity, route, from, to, message, geometry)
    by_route = defaultdict(list)
    total = events.row_count or 1
    for i, row in enumerate(events.rows()):
        if i % 500 == 0:
            ctx.progress(100.0 * i / total, f"Checked {i} of {total} events")
        rid, f, t = str(row[e_id]), row[e_from], row[e_to]
        geom = first_point(row.geometry()) if events.geometry_column else None
        if f is None or t is None:
            findings.append(("lrs.missing_measure", Severity.FAIL, rid, f, t, "missing from or to measure", geom))
            continue
        f, t = float(f), float(t)
        if f > t + tol:
            findings.append(("lrs.reversed", Severity.FAIL, rid, f, t, "from measure after to measure", geom))
        if rid not in extent:
            findings.append(("lrs.unknown_route", Severity.FAIL, rid, f, t, "route not in the routes dataset", geom))
        else:
            lo, hi = extent[rid]
            if min(f, t) < lo - tol or max(f, t) > hi + tol:
                findings.append(("lrs.out_of_range", Severity.FAIL, rid, f, t, f"outside route measures {lo:g}–{hi:g}", geom))
        by_route[rid].append((min(f, t), max(f, t), geom))

    for rid, spans in by_route.items():
        ctx.check_cancelled()
        spans.sort(key=lambda s: (s[0], s[1]))
        reach = spans[0][1]
        for f, t, geom in spans[1:]:
            if f < reach - tol:
                findings.append(("lrs.overlap", Severity.WARN, rid, f, t, f"overlaps the previous event by {reach - f:g}", geom))
            elif f > reach + tol:
                findings.append(("lrs.gap", Severity.WARN, rid, reach, f, f"gap of {f - reach:g} before this event", geom))
            reach = max(reach, t)

    findings.sort(key=lambda x: (x[2], x[3] if x[3] is not None else -1))
    n = write_geojson(
        ctx.output_path("findings"),
        (
            {"type": "Feature", "geometry": g, "properties": {"check": c, "route_id": r, "from_meas": f, "to_meas": t, "message": m}}
            for c, _, r, f, t, m, g in findings
        ),
    )
    ctx.set_output("findings", features=n)
    rows = write_csv(ctx.output_path("report"), ["check", "severity", "route_id", "from_meas", "to_meas", "message"],
                     ([c, s, r, f, t, m] for c, s, r, f, t, m, _ in findings))
    ctx.set_output("report", rows=rows)

    counts = defaultdict(int)
    severity = {}
    for c, s, *_ in findings:
        counts[c] += 1
        severity[c] = s
    for c in sorted(counts):
        ctx.warn(c, severity[c], f"{counts[c]} event(s): {c.split('.', 1)[1].replace('_', ' ')}", detail={"count": counts[c]})
    return {"events_checked": events.row_count, "routes": len(extent), "findings": len(findings)}


if __name__ == "__main__":
    plugin.serve()
