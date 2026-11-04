# Columbus

Columbus makes a GIS analyst, paired with whatever AI their organization already licenses, faster and more trustworthy at real work on real data. This repository holds the core of it: **`columbus-mcp`, a governed GIS MCP server** that any MCP agent (Copilot, Claude, Codex, Gemini CLI, Cursor) can use, and that enforces in its own code what each agent may read, write, run and download.

> **Status: early, phase 1.** The governance core and the first read-only tools work end to end on GeoPackage and SQLite. Database engines (Oracle, SQL Server, PostGIS, enterprise geodatabases through ArcPy), geoprocessing, verification checks, approvals UI and the desktop app come next. See [Roadmap](#roadmap).

## Principles in code

- **Rules live in the server, not the prompt.** The agent can ask for anything; the server decides. It has no tool that reads or changes policy or connections.
- **Read-only by default.** Edits stay off until organization policy turns them on; until then every connection is read-only whatever it says.
- **Access is an intersection.** Organization policy ∩ connection ∩ data group ∩ project. Each layer can only narrow the one above.
- **Show your work.** Every result carries `result`, `checks` and `provenance`; every call writes one line to a hash-chained audit log, including refusals.

## What works today

| Tool | Does |
| --- | --- |
| `whoami` | Permissions in force, connections and their modes, workspace, engines. Call first. |
| `search_tools`, `describe_tool` | Find the right tool by describing the job |
| `list_connections`, `describe_connection` | Connections, effective mode, allowed datasets, row filter, limits. Never secrets. |
| `list_datasets` | Allowed tables with geometry type, SRS, extent and fields (hidden fields removed) |
| `sql_select` | One checked, read-only `SELECT` (details below) |
| `approval_status` | Check an approval the agent is waiting on; it cannot approve anything |

**How `sql_select` is checked.** The SQL must parse, in the connection's dialect (Oracle, PostgreSQL, SQL Server, SQLite, DuckDB), as exactly one `SELECT`. Every table, including those in subqueries, must be on the connection's allow list and off its deny list; quoting does not get around this. `WITH`, `SELECT INTO`, `FOR UPDATE`, table functions and functions that reach files, the network or server internals (`pg_*`, `dbms_*`, `xp_*`, `load_extension`, `read_csv`…) are refused. The server then rewrites every table reference into a filtered derived table, so the connection's row filter and data-group filters always apply, and adds a row limit with one sentinel row so truncation is detected and reported rather than returned as a complete result. The worker opens the source read-only as well. Anything that does not parse is refused, never passed through.

**Results.** At most 500 rows go back to the AI provider per call (organization policy can lower this). Larger results are written to the session workspace and the path returned. Fields matching the policy's `hide_fields` are stripped before anything leaves the server.

## Layout

```text
crates/shared     policy layers, query checking, approvals, sandbox, audit (no I/O to agents)
crates/gateway    columbus-mcp: MCP server (rmcp), tools, worker supervision
workers/python    engine worker: line-delimited JSON-RPC over stdio
examples          a sample organization policy and connections
```

## Build and run

Requires Rust (stable, 1.85+) and Python 3.12+.

```powershell
cargo build --release
.\target\release\columbus-mcp.exe --version
```

Add it to an MCP client. In VS Code with Copilot (`.vscode/mcp.json`):

```json
{
  "servers": {
    "columbus": {
      "command": "C:\\path\\to\\columbus-mcp.exe",
      "env": { "COLUMBUS_PYTHON": "C:\\path\\to\\python.exe" }
    }
  }
}
```

Claude Desktop's `claude_desktop_config.json` takes the same entry under `mcpServers` instead of `servers`.

### Where it looks

| What | Windows default | Override |
| --- | --- | --- |
| Organization policy | `%ProgramData%\Columbus\policy.yaml` | `COLUMBUS_POLICY` |
| Connections (one YAML each) | `%APPDATA%\Columbus\connections\` | `COLUMBUS_CONNECTIONS` |
| Session workspaces | `%LOCALAPPDATA%\Columbus\workspaces\<session>` | `COLUMBUS_WORKSPACES` |
| Audit log | `%ProgramData%\Columbus\audit\YYYY-MM.jsonl` | `COLUMBUS_AUDIT_DIR` |
| Engine worker | `workers\python\columbus_worker.py` beside the executable | `COLUMBUS_WORKER` |
| Python | `python.exe` on PATH | `COLUMBUS_PYTHON` |

On Linux and macOS the defaults are `/etc/columbus` and the XDG folders. Without a policy file the server runs with safe defaults: local GeoPackage and SQLite only, read-only, no scripts, no edits. See [`examples/`](examples) for a policy and connections.

If the audit folder is not writable (a per-user install without the installer's permissions), the server falls back to a folder under the user's profile and says so in `whoami`. A call that cannot be audited does not run.

### Check the audit log

```powershell
columbus-mcp audit verify "%ProgramData%\Columbus\audit\2026-11.jsonl"
```

Each line records user, client, tool, connection, tables, row count, decision and a hash of the exact request; never row data. Editing, removing or reordering a line breaks the chain at that line.

## Tests

```powershell
cargo test --workspace                                   # unit tests + an end-to-end MCP session
python -m unittest discover -s workers/python/tests      # worker
```

The end-to-end test starts the real binary as an MCP client would, with a policy, a GeoPackage connection and the Python worker, and checks over the wire that row filters apply, hidden fields never leave, denied tables and writes are refused, truncation is reported and the audit chain verifies.

## Roadmap

Phase 1 (this repository, in progress):

- [x] Four-layer permission model, query checking, sandbox, approvals store, hash-chained audit log
- [x] MCP server over stdio with structured output; Python worker; GeoPackage/SQLite engine
- [ ] `query_features` (structured queries) and `describe_dataset`, `sample_rows`
- [ ] Database engines: PostGIS (`psycopg`), SQL Server (ODBC 18), Oracle (`python-oracledb` thin), each in read-only sessions
- [ ] Secrets from Windows Credential Manager; streamable HTTP on 127.0.0.1 with a per-session token
- [ ] Approval flow: in-client elicitation plus the local approvals page
- [ ] `vector_op`, `raster_op`, `pointcloud_pipeline` through GDAL/PDAL in a pixi environment
- [ ] Verification checks (design section 4) attached to every result
- [ ] ArcPy worker for enterprise geodatabases; signed `.mcpb` bundle and MSI

Companion repositories: [`geo-registry`](https://github.com/wiatrek-kevin-s/geo-registry) (open catalogue of GIS tools and data sources). Planned: `gis-context-pack` (format for an organization's datasets, rules and SOPs) and `columbus-skills` (agent skills for sound GIS practice).

## Honest limits

The server's checks are a second line of defence. The guarantee nothing bypasses is the database's own users, roles and grants: give Columbus a read-only database login. Anyone with direct credentials and other tools can go around Columbus.

## Licence

Apache-2.0. See [LICENSE](LICENSE).
