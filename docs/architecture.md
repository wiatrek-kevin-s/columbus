# How Columbus is built

The parts of Columbus 0.14.0 and where they live. The [technical reference](reference.md#layout) has the folder layout and every rule.

```text
        AI app (Columbus's own chat, Claude, Copilot, any MCP client)
                              │ MCP
                              ▼
   Columbus gateway (Rust): policy ∩ connection ∩ data group ∩ project,
   approvals, audit, checks, provenance; ArcGIS requests and sign-in;
   web map layer proxy; environments registry; columbus-confine sandbox
              │                                   │
              ▼                                   ▼
   GIS engine (Python): reads and          Your Python environments
   writes data, GIS operations,            (governed: sandboxed; trusted:
   file caches, exports                    unconfined, experimental)
              │
              ▼
   GeoPackage · files · shapefiles · file geodatabases · rasters ·
   PostGIS · Oracle · SQL Server · ArcGIS REST services
```

| Part | Folder | Job |
| --- | --- | --- |
| **Gateway** (`columbus-mcp`) | [`crates/gateway`](../crates/gateway) | The MCP server: tools, policy, approvals, audit, provenance, the approvals page, the workflow runner |
| **Shared rules** | [`crates/shared`](../crates/shared) | Policy, access, SQL checking, approvals, audit chain, personal tools and workflows on disk |
| **Broker and flows** | [`crates/broker`](../crates/broker), [`crates/flow`](../crates/flow) | Connected MCP servers, their manifests, and data-flow rules |
| **Sandbox** | [`crates/confine`](../crates/confine) | Confinement for scripts, tools and connected servers (Seatbelt on macOS, AppContainer and job objects on Windows, Landlock and seccomp on Linux) |
| **Chat** | [`crates/chat`](../crates/chat) | The app's own chat with the Anthropic API |
| **GIS engine** | [`workers/python`](../workers/python) | geopandas, pyogrio, shapely, pyproj, rasterio and DuckDB, as a worker the gateway drives |
| **Desktop app** | [`app/desktop`](../app/desktop) | Tauri 2 (Rust) with a plain HTML/JS interface |
| **Map page** | [`app/ui`](../app/ui) | MapLibre map and table, served by the gateway, shown in the panel on the right of each chat in the app, or on its own in the browser |
| **Skills** | [`skills`](../skills) | GIS skills for assistants, checked against the server's tool names |

The rules live in the gateway; the app adds no rules of its own. The [technical reference](reference.md) has the details: what is enforced and how, confinement levels, connected servers, the context pack, tests and honest limits.
