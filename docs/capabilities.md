# What Columbus can do

What Columbus 0.15.0 does, area by area. Items marked *experimental* work but have not yet been checked on a real ArcGIS Pro computer ([manual checks](manual-checks.md), H-2). The [technical reference](reference.md) has every rule in detail; [Not in this release](reference.md#not-in-this-release) lists what it doesn't do.

| Area | Highlights |
| --- | --- |
| **Data** | GeoPackage, SQLite, shapefiles, file geodatabases, raster folders, folders of CSV, Excel, GeoJSON, GeoParquet, KML and FlatGeobuf files, ArcGIS Online/Portal layers (public or signed in); PostGIS, Oracle and SQL Server (read-only, or read-write when allowed); datasets allowed by name or pattern; row filters and hidden fields applied in the server; limits on rows and time. |
| **Analysis** | `query_features`, checked SQL (`sql_select`), vector operations (reproject, buffer, simplify, make valid, clip, dissolve, intersect, attribute and spatial joins), raster operations, verification checks; results are workspace layers passed by handle, never pasted into the chat. |
| **Looking** | The map beside each chat, in a panel on the right, with its table as a sheet on the map; `map_snapshot` lets the assistant look at its layers as a picture (geometry only, no basemap, nothing downloaded) to spot slivers, gaps and layers that don't line up. With `map_view` the assistant reads what your map shows (the area, the layers that are on, the selected feature) and points it at a result; a snapshot can be of that same view. |
| **Big tables** | Raised limits (up to 20 million rows per query, by your organization), results streamed to disk, and project databases (DuckDB) to load several tables into and join with SQL. |
| **The boundary** | Connections and passwords stay with Columbus. Reading goes through Columbus, under each source's rules, and lands as a copy in the chat's workspace. Scripts and installed tools get only copies, in the operating system's sandbox: no network, their inputs to read, their own run folder to write. Where the sandbox cannot be set up, the code does not run. |
| **Approvals** | Columbus asks when something would cross the boundary: an export to a folder that isn't pre-approved, a download from a host not on the list, an edit to a source, code that needs the network or must run outside the sandbox (trusted environments, arcpy), sending data to a connected server, and anything new being installed. You answer in your AI app's own prompt, in the Columbus app's chat, or on a local approvals page. Bound to the exact request, single-use, expiring. Whether a script that stays in the sandbox also asks is your choice: **Ask me before each script**, or **Work freely on copies**. Scripts are off altogether until the policy turns them on. |
| **Toolboxes** | Programs that bring many tools. **Tools › Add tools** installs GeoLibre (about a thousand open-source terrain, hydrology, raster and vector tools), downloaded at your request and checked against a hash pinned in Columbus; any other command-line program can be added with a small description file. The assistant finds, reads and runs their tools, on copies, in the sandbox, with no approval. |
| **Your tools** | Save a script as a tool (`propose_tool`): checked, test-run in the sandbox, read and installed by you, then reusable by any assistant. |
| **Projects** | A piece of work with its own instructions, connections and data groups, and a folder of files (scripts, notes). The assistant can read and write text files there and run a script from there; you can put your own in. The folder can sync with a repository on GitHub, GitHub Enterprise Server, Gitea or Forgejo when you press **Sync**: no Git needed on the PC. |
| **Workflows** | Save a job as a workflow (`propose_workflow`): reading and checking steps, the connections it may read, a schedule (launchd on macOS, Task Scheduler on Windows), results to an export folder, an optional AI review. Never waits for you: anything that needs approval stops it with "needs you". |
| **Connected servers** | Other MCP servers (Drive, notes, vendor APIs) behind Columbus, reviewed and pinned by manifest, confined, with data-flow rules on what may be sent where. |
| **Context** | Your organization's context pack (datasets, domains, LRS, SOPs, delivery specs) and your own notes, searchable by the assistant. |
| **Audit** | One hash-chained line per call, including refusals; `columbus-mcp audit verify`; the app's **Activity** page (History). |
| **Platforms** | macOS 15 or later (Apple Silicon) and Windows 10/11. The server also builds and runs on Linux. |

## New in 0.15.0

| Area | Highlights |
| --- | --- |
| **The window** | A sidebar, the chat, and a panel on the right that opens with what the assistant made: the map and its table, the chat's files, how a result was made, a note |
| **Work freely on copies** | One choice, in setup or Settings, in place of approving each script: scripts and installed tools run in the sandbox without asking. Exports to folders that aren't pre-approved, downloads from hosts not listed, edits, and code that needs the network or leaves the sandbox still ask. Off until you choose it |
| **Toolboxes** | GeoLibre from **Tools › Add tools** or `columbus-mcp toolbox install geolibre`; any command-line program with a description file. *The real GeoLibre program was never run while this was built ([manual checks](manual-checks.md), H-3).* Not yet: QGIS as a toolbox, toolbox tools in workflows |
| **The map** | `map_view`: the assistant reads what your map shows and changes it (layers on and off, the active layer, zoom); `map_snapshot` of that view |
| **Project files** | `project_files`: the assistant lists, reads and writes text files in its project's folder, and `run_python` runs a script kept there |
| **Sync** | **Settings › Git servers**, then **Link this project**, **Check for changes** and **Sync** on a project's page. A file changed on both sides is kept twice; data, secrets and big files stay out. *Tested against stand-in servers only ([manual checks](manual-checks.md), H-4); no server can be connected on Linux yet.* |

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
