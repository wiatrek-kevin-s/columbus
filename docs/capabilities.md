# What Columbus can do

What Columbus 0.14.0 does, area by area. Items marked *experimental* work but have not yet been checked on a real ArcGIS Pro computer ([manual checks](manual-checks.md), H-2). The [technical reference](reference.md) has every rule in detail; [Not in this release](reference.md#not-in-this-release) lists what it doesn't do.

| Area | Highlights |
| --- | --- |
| **Data** | GeoPackage, SQLite, shapefiles, file geodatabases, raster folders, folders of CSV, Excel, GeoJSON, GeoParquet, KML and FlatGeobuf files, ArcGIS Online/Portal layers (public or signed in); PostGIS, Oracle and SQL Server (read-only, or read-write when allowed); datasets allowed by name or pattern; row filters and hidden fields applied in the server; limits on rows and time. |
| **Analysis** | `query_features`, checked SQL (`sql_select`), vector operations (reproject, buffer, simplify, make valid, clip, dissolve, intersect, attribute and spatial joins), raster operations, verification checks; results are workspace layers passed by handle, never pasted into the chat. |
| **Looking** | The map beside each chat, in a panel on the right, with its table as a sheet on the map; `map_snapshot` lets the assistant look at its layers as a picture (geometry only, no basemap, nothing downloaded) to spot slivers, gaps and layers that don't line up. |
| **Big tables** | Raised limits (up to 20 million rows per query, by your organization), results streamed to disk, and project databases (DuckDB) to load several tables into and join with SQL. |
| **Approvals** | Exports, edits, scripts, downloads and sending data to connected servers wait for you: in your AI app's own prompt, in the Columbus app's chat, or on a local approvals page. Bound to the exact request, single-use, expiring. |
| **Your tools** | Save a script as a tool (`propose_tool`): checked, test-run in the sandbox, read and installed by you, then reusable by any assistant. |
| **Workflows** | Save a job as a workflow (`propose_workflow`): reading and checking steps, the connections it may read, a schedule (launchd on macOS, Task Scheduler on Windows), results to an export folder, an optional AI review. Never waits for you: anything that needs approval stops it with "needs you". |
| **Connected servers** | Other MCP servers (Drive, notes, vendor APIs) behind Columbus, reviewed and pinned by manifest, confined, with data-flow rules on what may be sent where. |
| **Context** | Your organization's context pack (datasets, domains, LRS, SOPs, delivery specs) and your own notes, searchable by the assistant. |
| **Audit** | One hash-chained line per call, including refusals; `columbus-mcp audit verify`; the app's **Activity** page (History). |
| **Platforms** | macOS 15 or later (Apple Silicon) and Windows 10/11. The server also builds and runs on Linux. |

## New in 0.14.0

| Area | Highlights |
| --- | --- |
| **Folders of files** | CSV, Excel, GeoJSON, GeoParquet, KML and FlatGeobuf files as read-only connections, with every rule a GeoPackage has |
| **ArcGIS Online and Portal** | Feature and map services as read-only connections: layers and tables as datasets, row filters and hidden fields, paging without lost rows, Z and M kept; sign-in with your organization's own ArcGIS app (PKCE), tokens kept by the operating system and never seen by the assistant |
| **Python environments** | Your own Python for scripts: registered at the keyboard, governed (sandboxed) by default; environments the assistant asks for, built with Columbus's pinned uv after you approve |
| **arcpy and trusted environments** | *Experimental.* Where policy allows, scripts run unconfined, as you, every run approved; arcpy licence state in plain words |
| **Exports** | New file geodatabases (`gdb`) and Cloud-Optimized GeoTIFFs (`tif`), from chats and workflows |
| **Measures** | M (and Z) values kept through queries, operations and exports, for linear referencing |
| **Reviewing results** | Open a workflow run in a chat; zoom to the selected row; measure on the map; `vector_op` `filter` |
| **Web map layers** | Imagery and other services on your map, display only, through Columbus |
| **Speed** | Bounding-box queries use each source's spatial index |
