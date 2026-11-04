# Entry templates

These fit the context pack format, version 1. Fields marked *required* are what `pack lint` checks. Everything in the body is free Markdown.

## SOP (procedure)

```markdown
---
id: <slug, e.g. annual-route-certification>           # required
kind: sop                                              # required
title: <the words people search for>                   # required
summary: <what it does, on what; at most two sentences>   # required
when_to_use: <the task, schedule or event that triggers it>            # required for sop
owner: <team or person who vouches for it>             # required
last_confirmed: YYYY-MM-DD                             # required; sign-off date
status: draft                                          # required; reviewed after sign-off
source: { file: sources/<original file name>, section: "<number and heading>" }
links: [<dataset ids>, <lrs id>, <glossary>, <role id>]
---

**Inputs:** <datasets by entry id and filter, e.g. roadway-inventory with STATUS = 'A'>
**Output:** <what is produced, its format, and who receives it>
**Frequency:** <as the original says>

## Steps

1. <one action, with the threshold and unit in the step> (orig. 2.1)
2. <...> (orig. 2.2)
   - If <condition>: <what changes>. (orig. 2.2, note)
3. <...>

## Exceptions

- <each exception from the original, word for word where it is a rule>

## Do not

- <each prohibition, word for word>

## Questions for the owner

- <anything the original left unclear; remove this section once answered>
```

Keep "Exceptions" and "Do not" as separate sections only if the original states them apart from the steps. A condition that governs one step goes in that step.

## Shared entries a conversion often creates

### Glossary term

Add a row to the pack's existing `glossary` entry rather than creating a new entry per term.

```markdown
| DFO | Distance from origin, in miles, along a route |
```

### Dataset

```markdown
---
id: roadway-inventory
kind: dataset
title: Roadway inventory
summary: One row per roadway segment on the county system, with functional class and lanes, located by route and measure.
connection: roads-prod            # required for a dataset: a Columbus connection id
physical: ROADS.ROADWAY_INVENTORY # required for a dataset: the table as the connection names it
geometry: polyline (M)
crs: EPSG:3081
keys: [ROUTE_ID, FROM_MEAS, TO_MEAS]
fields:
  FUNC_CLASS: { alias: Functional class, type: smallint, domain: functional-class }
  STATUS:     { alias: Status, type: char(1), note: "A active, R retired" }
updated: nightly from the LRS edit environment
owner: GIS Data Management
last_confirmed: 2026-10-15
status: reviewed
links: [state-routes, functional-class]
---

## Gotchas

- <things every task on this table needs to know>
```

A field's `domain` must be the id of a `domain` entry, or lint fails with `field FUNC_CLASS uses unknown domain "..."`.

### Domain (YAML file under `domains/`)

```yaml
id: functional-class
kind: domain
title: Functional class
summary: Coded values for a roadway segment's functional class.
type: coded
values:
  1: Interstate
  2: Other freeway and expressway
owner: GIS Data Management
last_confirmed: 2026-10-15
status: reviewed
```

An Excel data-validation list or lookup sheet in the original usually becomes a domain.

### Role

```markdown
---
id: gis-analyst
kind: role
title: GIS analyst
summary: Builds maps and analyses from county data and runs the monthly LRS quality check.
when_to_use: Planning recurring analyst work, or deciding who runs an LRS check.
owner: GIS Data Management
last_confirmed: 2026-09-30
status: reviewed
links: [monthly-lrs-qc]
---

Recurring tasks:

1. Monthly LRS quality check (monthly-lrs-qc).
```

## Folders

| Entry | Folder |
| --- | --- |
| SOP | `sops/` |
| Dataset | `datasets/<id>/README.md` |
| Domain | `domains/<id>.yaml` |
| LRS | `lrs/` |
| Role | `org/roles/` |
| Original documents | `sources/` (not entries; lint and search skip it) |
