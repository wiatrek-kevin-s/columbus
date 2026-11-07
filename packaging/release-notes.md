Columbus {{VERSION}}, a test release. Click a link to download that file.

**New in this version:**
- **Esri shapefiles and file geodatabases:** two new kinds of connection, added on the Connections page with **Choose folder…**: a folder of shapefiles (each shapefile is a dataset), and a `.gdb` file geodatabase (each feature class or table is a dataset). No ArcGIS is needed: Columbus reads them with the GIS engine it already installs. They follow the same rules as your other connections (allowed datasets, hidden fields, row filters, data groups, the SQL checks), and are read only for now.
- **One edit to your policy file:** if your `policy.yaml` lists `connection_types`, add `shapefile` and `filegdb` to that line. If you have no policy file, both are allowed already.
- **Limits:** a shapefile whose name has a dot (like `parcels.2024.shp`) isn't offered, so rename it without dots. A row filter can't use a shapefile date field, because shapefile dates are compared as text. Compressed geodatabases, rasters inside a geodatabase and annotation aren't read yet.
- **Faster reading:** the engine now installs `pyarrow` so large shapefile and geodatabase reads stream. An engine set up before this version still works, just more slowly. To update it, run the engine setup again.

**In 0.11.0:** Projects, Data groups, the Registry page, project databases on the Projects page, filter security fixes, and fewer Keychain prompts on a Mac.

## Mac (Apple Silicon: M1 or later)

**Already set up Columbus before? Update with these two files:**

1. **Claude extension:** [columbus-{{VERSION}}-macos-arm64.mcpb]({{BASE}}/columbus-{{VERSION}}-macos-arm64.mcpb). Double-click it in your Downloads folder. Claude desktop asks to install or update the extension: accept, check that **Engine Python** still says `/Users/<you>/Columbus/engine/venv/bin/python3`, then quit Claude with **Cmd+Q** and open it again.
2. **Columbus app (optional):** [columbus-app-{{VERSION}}-macos-arm64.dmg]({{BASE}}/columbus-app-{{VERSION}}-macos-arm64.dmg). Double-click it and drag **Columbus** onto **Applications**, replacing the old one. Then, in Terminal: `xattr -dr com.apple.quarantine /Applications/Columbus.app` (again after each update: it also clears the server inside the app).

**New to Columbus?** Also download the server folder, [columbus-{{VERSION}}-macos-arm64.zip]({{BASE}}/columbus-{{VERSION}}-macos-arm64.zip), and follow the [Mac install guide](https://github.com/wiatrek-kevin-s/columbus/blob/main/docs/install-macos.md) from step 1. You only need the folder once, to set up the GIS engine.

## Windows (10 and 11, 64-bit)

**Already set up Columbus before? Update with these two files:**

1. **Claude extension:** [columbus-{{VERSION}}-windows-x64.mcpb]({{BASE}}/columbus-{{VERSION}}-windows-x64.mcpb). Double-click it, accept the update in Claude desktop, check **Engine Python**, then quit Claude completely (not just its window) and open it again.
2. **Columbus app (optional):** [columbus-app-{{VERSION}}-windows-x64-setup.exe]({{BASE}}/columbus-app-{{VERSION}}-windows-x64-setup.exe). Run it; it installs for you alone, no administrator needed, and replaces the old version. Windows SmartScreen warns because it is not signed yet: **More info**, then **Run anyway**. For IT: [the .msi]({{BASE}}/columbus-app-{{VERSION}}-windows-x64.msi).

**New to Columbus?** Also download the server folder, [columbus-{{VERSION}}-windows-x64.zip]({{BASE}}/columbus-{{VERSION}}-windows-x64.zip), and follow the [Windows install guide](https://github.com/wiatrek-kevin-s/columbus/blob/main/docs/install.md) from step 1.

## What the files are

| File | What it is |
| --- | --- |
| `.mcpb` | The Columbus server as a Claude desktop extension: Claude starts it for you |
| `columbus-app-…` | The Columbus app: each running session's layers on a map, its tables, checks, and the requests waiting for your decision; and a chat with Claude using your own Anthropic API key |
| `.zip` | The server as a folder, with the GIS engine setup script, examples, skills and guides |

Nothing is code-signed yet, which is why macOS and Windows warn; the steps above say what to do.
