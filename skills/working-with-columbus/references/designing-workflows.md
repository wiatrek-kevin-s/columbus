# Designing a workflow for `propose_workflow`

A workflow is a job the person wants done again and again without sitting through it: read a
dataset, check it, export what was found, every weekday at 06:00. You write it as one
`workflow.yaml`, Columbus checks it, and it waits in the Columbus app under **Workflows**. The
person reads it in plain words and approves it with Touch ID or Windows Hello. Only they can;
nothing runs before that, and a changed workflow (or a changed personal tool it uses) is off
until they approve it again.

A run is an ordinary Columbus session with three differences: it reads only the connections
the workflow lists, it calls only the workflow's steps, and **it never waits for a person**.
Any step that would need an approval (an export to a folder that isn't granted, a dataset a
flow rule holds back) stops the run with "needs you". Design for that: a good workflow needs no
approval at all.

## When to propose one

- The person asks for it: "run this every morning", "make this a workflow", "check this every
  Monday and put the results in the outbox".
- You have just done the same steps by hand and they say they will need them again.

Do the steps by hand in the session first, so you know every argument works and what each
result looks like. Then write the workflow from those calls.

## The format

```yaml
id: morning-tract-check          # 3-40 lowercase letters, digits, single hyphens
name: Morning tract check        # one line, at most 80 characters, no double quotes
purpose: Check the tracts layer for invalid geometry every weekday and export it.
schedule:                        # leave out to run only when the person starts it
  days: weekdays                 # daily, weekdays, or a list: [mon, wed, fri]
  at: "06:00"                    # HH:MM, 24-hour, the computer's time
scope:
  connections: [tiger]           # every connection a step reads; nothing else is read
  run_minutes: 20                # the whole run stops after this (default 30, at most 240)
steps:
  - id: tracts                   # a lowercase letter, then letters, digits and _
    tool: query_features
    args: { connection: tiger, dataset: tracts, where: "STATEFP = '48'" }
  - id: checked
    tool: verify
    args: { layer: "${tracts}" }
outputs:
  folder: { export: outbox, steps: [tracts], format: gpkg }
  email: { to: [ana@agency.gov], when: problems }   # only if whoami.workflow_email allows
  keep_runs: 60                  # runs kept (default 100)
```

`email` sends the run's report (`when: always`, the default, or `problems`: only when a run
doesn't simply succeed). Only addresses in the domains `whoami` lists under `workflow_email`;
when that is null, the organization hasn't set up email: leave it out. `attach: true` adds the
files the run exported, if the organization allows attachments. Ask the person whom to email;
never guess an address.

### Steps

Each step is one call, with the same arguments you would give it in a chat, on any of the
person's connection types in `scope.connections`: databases, GeoPackages, `files` folders and
`arcgis` services alike. A step on a signed-in `arcgis` connection keeps its sign-in alive by
itself; if the sign-in has lapsed, the step stops with "needs you: sign in again (columbus-mcp
arcgis signin <id>)". A step's `tool` is one of:

- `list_datasets`, `describe_dataset`, `sample_rows`
- `query_features`, `sql_select` (not on `arcgis` services, which take no SQL)
- `vector_op`, `raster_op`, `verify`, `read_output`
- an installed personal tool, by its full name `personal.<name>.run`, with `args` as you
  would pass to `run_tool`'s `args`

Never a step: `run_python` (a script is approved one run at a time), edits, connected
servers, downloads, and `export_output` (exports are made by `outputs.folder`, below). A
personal tool is the way to put your own code in a workflow: propose it with `propose_tool`
first; once the person has installed it, the workflow can use it.

A personal tool step runs in the tool's own environment (its `environment`, or the engine's
Python). Add `environment: <id>` to the step to run it in another registered environment:
governed, at level A (whoami lists them); never a trusted one. If that environment is missing,
changed or no longer allowed when the workflow runs, the step stops with "needs you" and the run
goes on with the steps that don't use its result.

### References between steps

A whole text value `"${step}"` is the layer (or file) that step made: its workspace handle.
`"${step.a.b}"` is a field of that step's result, e.g. `"${tracts.layer.summary.rows}"`. A
reference is always the whole value (never inside other text), and only to an earlier step.

### Checks

Every step's checks go in the run's report. A step with a `fail` check stops the run there
(the report says which check); add `continue_on_fail: true` to a step whose failures are what
the workflow is meant to find. Warnings never stop it; the run ends "succeeded, with findings
to review".

### An AI review (optional)

```yaml
review:
  reads: [checked]               # the steps whose layers it reads
  fields: [SEG_ID, GSC, CN]      # only these fields (leave out for all)
  max_rows: 200                  # rows sent to the model, at most (default 100, at most 500)
  model: claude-haiku-5-5        # claude-sonnet-5-5 (default), -opus-5-5, -fable-5-1, -haiku-5-5
  effort: low                    # low, medium, high, xhigh, max
  criteria: |
    Flag a segment if its GSC is rural but it lies inside the urbanized area.
```

After the steps, Columbus reads those rows itself (under the person's rules: row filters,
hidden fields and the per-call row limit apply) and sends them with the criteria to Anthropic,
using the API key the person saved in the Columbus app. The model has **no tools**: it cannot
read anything else or change anything. Its notes (a summary and the flagged rows) go in the
report, marked as the AI's. The AI's flags never change the run's status: they are notes for
the person to read, not checks. If it cannot run (no key, the service is busy, an answer that
was cut off or not in the form asked for), the run still delivers its results, marked "AI
review did not run"; it never reads as "nothing flagged". The organization can turn reviews off
(`deny_tools: [workflow_review]`, or a `tools` allowlist without it) or keep a connection's data
from the AI with a flow rule (to `ai:anthropic`, or to every outside service, `server:*`); a
workflow with a review is then refused when you propose it. Write criteria a
careful colleague could apply from the rows alone, and ask only for the fields the criteria
need.

### Outputs

`outputs.folder` exports the layers of the steps it lists to one of the policy's export
folders (whoami lists them), named `<workflow>-<step>-<date>`, with a provenance file beside
each. `format` is `gpkg` (the default), `gdb` (a new file geodatabase per layer, named
`<workflow>_<step>_<date>.gdb`, since a feature class name has no `-`), `geojson`, `fgb`, `shp`,
`csv`, or `tif` for raster results. Use a folder whose `granted` is true: an export elsewhere needs the person's approval, so
a run would stop with "needs you" and export nothing. Every run also keeps a report (steps,
row counts, checks, deliveries, provenance ids), which the person reads in the app.

## Rules

- **One job per workflow.** A clear name and a purpose that says what it finds and for whom.
- **Name every connection in `scope.connections`.** A step that reads another one is refused
  when you propose it, and again when it runs.
- **Narrow reads.** Use `where` and `fields`: a scheduled run should read what it needs, not
  the whole table, and the person's read limits apply.
- **No secrets, no paths.** Arguments are dataset names, fields, filters, numbers and handles
  from earlier steps.
- **Schedule only what they asked for.** If they didn't name a time, leave `schedule` out and
  say they can add one; they can always run it with **Run now**.

## After proposing

`propose_workflow` answers `waiting_for_person` with a plain account of what the workflow
does. Tell the person it is waiting under **Workflows** in the Columbus app, what it will do
in a sentence or two, and that they can run it once with **Run now** and read the report
before letting the schedule take over. You cannot approve, run or schedule it yourself.

Runs happen while the person is signed in to the computer; a computer that was asleep runs a
missed job once when it wakes.
