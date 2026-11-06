# RustGIS as a connected server (pilot)

[RustGIS](https://github.com/opengeos/RustGIS) has an MCP server, `rustgis-cli mcp`, that reaches more than 1,000 GeoLibre and Whitebox geoprocessing tools (terrain, hydrology, LiDAR and more) through three tools: `search_tools`, `describe_tool` and `run_tool`. This pilot connects it to Columbus as a governed connected server, so an assistant can use those tools under Columbus's rules.

## What is in place

- **A manifest**, [`examples/servers/rustgis.yaml`](../examples/servers/rustgis.yaml). It turns on the reading and geoprocessing tools and keeps `run_command` (opening, saving, plugin actions) off. It runs RustGIS confined at level B: it can read and write only its own folder, with no network.
- **Files in.** The manifest gives `run_tool` the role `"*": path`. Any `ws://` handle in its parameters, however nested, is copied into RustGIS's folder once the call is allowed. RustGIS gets that path and never sees the workspace.
- **Files back.** In a parameter, `out://slope.tif` asks RustGIS to write that file in its own folder. After the call, Columbus copies it into the workspace, carrying the data labels of what RustGIS was given. It does this only if the file is a plain file (never a link) and at most 512 MB, and it records the file's hash and provenance. The assistant gets back a `ws://` handle.
- **Review and pinning as for any server.** `columbus-mcp servers review rustgis`, then `approve`. A RustGIS update that changes a tool's definition switches that tool off until it is approved again.

Tests: `crates/gateway/tests/server_files.rs` uses a stand-in server with a `run_tool` like RustGIS's. It covers nested handles going in, an asked-for file coming back, a refused file name, a handle from another session, and a link in place of the file.

## What is not settled

1. **RustGIS's real parameters.** RustGIS's documentation does not give `run_tool`'s argument schema. We have not yet confirmed with a running copy that its tools take input and output file paths in `params`, as opposed to layer names in its in-memory project. If they take layer names, inputs need loading first, which is a `run_command` (off here), or the `--data` flag at start.
2. **Using what comes back.** Files from a connected server are untrusted, so Columbus's GIS tools, exports and personal tools do not open them today. For this pilot that means a RustGIS result can be listed and read as rows (`read_features`) but not exported or processed further. Opening them would need a decision. One option: a reviewer-set manifest switch for confined servers, so their files are opened like an engine plugin's. That widens what reaches GDAL, so it needs a security review first.
3. **Confinement on each platform.** Level B on macOS and Windows has to be checked against how RustGIS starts (it is a desktop app with a CLI).
4. **Checks.** RustGIS outputs have no Columbus checks yet. If item 2 is settled, `verify` can run on them, or they can be checked as they come back.

## Next

1. Install RustGIS on a Mac and run `columbus-mcp servers review rustgis` to see its real tool definitions.
2. Try one terrain tool (slope on a sample DEM) end to end.
3. Decide item 2.

If the tools prove useful, the next step is an engine plugin built with the Rust SDK (route B in the exploration notes). Its tools would get Columbus checks and provenance like built-in ones.
