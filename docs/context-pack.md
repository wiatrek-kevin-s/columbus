# GIS context pack format, version 1

A context pack is a folder of plain Markdown and YAML that describes one organization's GIS world: its datasets, codes, linear referencing, delivery specs, procedures, roles and the data groups people work with. Columbus reads it, gives agents what they need on demand, and attaches the right entry when an agent describes a dataset. The format is open; each organization's pack is private (usually a private git repository or a network folder).

A fictional sample, [Contoso County](../examples/context/contoso-county), shows every kind of entry. Check a pack with:

```powershell
columbus-mcp pack lint C:\path\to\pack
```

## Layout

```text
contoso-gis/
  pack.yaml                       # name, format version, owners, default CRS, always-on rules
  glossary.md                     # acronyms and terms of art
  org/organization.md             # who owns which data
  org/roles/gis-analyst.md        # a role and its recurring tasks
  datasets/roadway-inventory/README.md
  domains/functional-class.yaml   # coded value and range domains
  lrs/state-routes.md             # networks, route ids, measure units, calibration
  data-groups/live-grid-linework.yaml
  delivery/district-deliverable.yaml
  sops/monthly-lrs-qc.md          # procedures
  checks/  scripts/  sources/     # not entries: organization checks, approved scripts, originals
```

Folders other than `checks`, `scripts` and `sources` may hold entries at any depth. A YAML file's `kind` may be left out when its folder says it (`domains/` is `domain`, `data-groups/` is `data-group`, and so on).

## pack.yaml

| Field | Required | Meaning |
| --- | --- | --- |
| `name` | yes | The pack's name |
| `format_version` | no | `1` |
| `owners` | no | Who maintains the pack |
| `default_crs` | no | The organization's working CRS |
| `rules` | no | A handful of always-on rules, returned by `whoami` at the start of every session ("deliverables in EPSG:3081") |
| `review_interval_months` | no | Entries not confirmed for this long are flagged `stale` in search results (default 12) |

## Entries

A Markdown entry has YAML front matter between `---` lines; the front matter is the **card** an agent sees in search results, and the body is read only when the agent opens the entry. A YAML entry is all card.

| Field | Required | Meaning |
| --- | --- | --- |
| `id` | yes | Stable slug (lower-case letters, digits, `-`), unique in the pack |
| `kind` | yes | `dataset`, `domain`, `lrs`, `data-group`, `delivery`, `sop`, `role`, `glossary`, `note` |
| `title`, `summary` | yes | One-line title; summary of at most two sentences |
| `when_to_use` | SOPs and roles | The tasks or questions this answers |
| `owner`, `last_confirmed` | yes (not data groups) | Who vouches for it, and when they last did (`YYYY-MM-DD`) |
| `status` | yes (not data groups) | `draft`, `reviewed` or `retired` |
| `source` | when converted | The original file and section |
| `links` | no | Other entries by id |

**Size rule:** split an entry only when a task usually needs one part without the other. Long, intricate procedures stay whole. Move repeated material (definitions, dataset descriptions) into shared entries and link to them; never trim detail.

### Dataset

Adds `connection` (a Columbus connection id) and `physical` (the table as the connection names it), and optionally `geometry`, `crs`, `keys`, `updated`, and `fields`, each with `alias`, `type`, `domain` (a domain entry's id) and `note`. `describe_dataset` attaches the entry whose `connection` and `physical` match; on a file connection without schemas, `ROADS.ROADWAY_INVENTORY` matches the layer `roadway_inventory`.

### Domain

Adds `type` (`coded` or `range`) and `values` (code: label) or `min`/`max`.

### Data group

The datasets a recurring job needs, how they join and the documents that explain them. `datasets` lists dataset ids, or `{id, filter}` to narrow one; `assembly` describes joins; `documents` lists entry ids; `mode` is `read`. A data group can only narrow what the connections allow: `describe_data_group` marks which of its datasets the session can read.

### Delivery

Adds the spec a deliverable must meet: `crs`, `formats`, `fields: {rename: {OLD: NEW}}` and required `metadata` items. `export_output` with `delivery: <id>` applies the CRS and renames and refuses (as a `fail`, overridable only by a person) a format the spec does not list or missing metadata.

## What lint checks

Front matter present and valid; required fields per kind; ids unique and well-formed; `kind` and `status` known; `summary` at most two sentences; `last_confirmed` a real date not in the future; `links`, data-group datasets and documents, and field domains all resolve.

## Not yet

Columbus does not yet enforce domains in verification, draft entries from a geodatabase (`draft_context_from_geodatabase`), or propose updates when a live schema drifts from its entry. Those come with the edit and export tools.
