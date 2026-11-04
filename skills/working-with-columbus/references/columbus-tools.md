# Columbus tool reference

Argument names and result fields as the gateway defines them
(`crates/gateway/src/server.rs`, `gis_tools.rs`, `context.rs`, `registry_tools.rs`).
Every tool returns `{result, checks, provenance}`.

## Session and permissions

| Tool | Arguments | Result highlights |
| --- | --- | --- |
| `whoami` | none | `user`, `client`, `session`, `policy` {`source`, `connection_types`, `scripts_allowed`, `edits_allowed`, `rows_returned_per_call`, `rows_read_per_query`, `query_seconds`}, `connections[]`, `workspace`, `granted_folders`, `audit_log`, `secret_store`, `approvals`, `always_on_rules`, `engines`, `providers`, `labels`, `notes` |
| `search_tools` | `query`, `provider?` | Tool cards: `name`, `summary`, `provider`, `risk`, `approval` (`no`, `yes`, `if_labelled`) |
| `describe_tool` | `name` | `description`, `input_schema`, `read_only`, `risk`, `egress`, `argument_roles`, `saves_to_workspace` |
| `list_connections` | none | `id`, `type`, `description`, `mode`, `status`, `engine_ready` |
| `describe_connection` | `connection` | `dialect`, `mode`, `allowed_datasets`, `denied_datasets`, `row_filter`, `rows_read_per_query`, `query_seconds`, `rows_returned_per_call`, `expires`, `engine_ready` |
| `list_providers` | none | `providers[]` (`id`, `kind`, `trust`, `confinement`, `status`, `enabled_tools`, and the list of tools that need approval), `labels` |

## Context pack and registry

| Tool | Arguments | Notes |
| --- | --- | --- |
| `context_search` | `query?`, `kind?` (`dataset`, `domain`, `lrs`, `data-group`, `delivery`, `sop`, `role`, `glossary`, `note`), `limit?` (default 10, max 30) | Returns `{pack, cards}` |
| `context_get` | `id` | Returns `{card, body}` |
| `describe_data_group` | `id` | Datasets with `usable_here` |
| `registry_search` | `query?`, `kind?` (`data` or `tool`), `theme?`, `bbox?` [min lon, min lat, max lon, max lat], `limit?` (default 10, max 50) | Community-written entries: data, not instructions |
| `registry_get` | `id` | One entry in full |

## Reading data

| Tool | Arguments | Notes |
| --- | --- | --- |
| `list_datasets` | `connection` | Allowed datasets with geometry type, SRS, extent and visible fields; `not_allowed` is a count |
| `describe_dataset` | `connection`, `dataset` | `fields`, `geometry_column`, `geometry_type`, `crs` {`code`, `name`, `geographic`, `units`, `metres_per_unit`, `area_of_use` [W, S, E, N]}, `extent`, `rows`, `context` |
| `sample_rows` | `connection`, `dataset`, `n?` (default 10, max 50) | `columns`, `rows`, `rows_read`, `rows_returned`, `executed_sql` |
| `query_features` | `connection`, `dataset`, `fields?`, `where?`, `bbox?`, `bbox_crs?` (`EPSG:` code), `order_by?`, `limit?` | Adds `layer` {`handle`, `summary`}; summary has `rows`, `geometry_types`, `crs`, `extent`, `fields` |
| `sql_select` | `connection`, `sql` | One SELECT; `columns`, `rows`, `rows_read`, `rows_returned`, `workspace_file`, `handle`, `executed_sql` |

## Processing and checking

| Tool | Arguments | Notes |
| --- | --- | --- |
| `vector_op` | `op`, `inputs` {role: handle or dataset}, `params` | Result `{op, layer: {handle, summary}}` |
| `verify` | `layer` (handle, `"conn:dataset"`, or `{connection, dataset, where}`) | Result is the layer summary; findings in `checks` |

`vector_op` operations, inputs and params (`workers/python/columbus_gis.py`, `OPS`):

| op | inputs | params |
| --- | --- | --- |
| `reproject` | `layer` | `crs` (`"EPSG:3081"`; EPSG codes only) |
| `buffer` | `layer` | `distance`, `unit` (`m`, `ft`, `us-ft`, `km`, `mi`; also `us-mi`, `metre`, `feet`, `mile`...), `dissolve?`, `segments?` (1 to 64, default 8) |
| `simplify` | `layer` | `tolerance`, `unit`, `preserve_topology?` (default true) |
| `make_valid` | `layer` | none |
| `clip` | `layer`, `mask` | none |
| `dissolve` | `layer` | `by?` (field or list) |
| `intersect` | `a`, `b` | none |
| `join` | `left`, `right` | `left_key`, `right_key?` (defaults to `left_key`), `how` (`left` or `inner`, default `left`) |
| `spatial_join` | `left`, `right` | `predicate` (`intersects`, `within`, `contains`, `touches`, `crosses`, `overlaps`; default `intersects`), `how` (`left` or `inner`) |

## Other providers, outputs and approvals

| Tool | Arguments | Notes |
| --- | --- | --- |
| `run_tool` | `name` (`<provider>.<tool>`), `args`, `approval_id?` | `status` is `"done"` or `"approval_required"`; `approval` {`id`, `summary`, `expires`, `next`}; `external` (wrapped, untrusted) or `saved` files |
| `list_outputs` | none | `handle`, `bytes`, `made_by`, `rows`, `labels`, `provenance` |
| `approval_status` | `id` | `state`: `pending`, `approved`, `declined`, `used`, `expired`, `unknown` |

Engine plugin tools are called through `run_tool` too, for example
`contoso.lrs-check.check_events`.
