Columbus {{VERSION}}, a test release. Click a link to download that file.

## What's new in 0.14.0

Columbus now reaches much more of your organization's data, without loosening any rule.

- **Folders of files.** Add a folder (or one file) of CSV, Excel (`.xlsx`), GeoJSON, GeoParquet, KML or FlatGeobuf files as a connection, and query, analyse and export it like a GeoPackage. Points come from `lon`/`lat`, `x`/`y` or WKT columns; codes with leading zeros stay text; row filters and hidden fields apply as everywhere else.
- **ArcGIS Online and Portal layers.** Add a feature or map service as a connection: list, describe, sample and query its layers under your row filters and hidden fields, with no row lost to paging. Public layers work at once; layers shared only in your organization need a one-time **Sign in** (your organization registers its own ArcGIS app first: see [manual-checks.md](https://github.com/wiatrek-kevin-s/columbus/blob/main/docs/manual-checks.md)). Your assistant never sees the sign-in.
- **Your own Python.** Register a Python environment (a venv, an ArcGIS Pro clone) under **Settings › Environments**, and scripts can run in it. Governed environments (the default) run in the same sandbox as before. Your assistant can ask for a new environment with exact package versions, and Columbus builds it after you approve.
- **Experimental: arcpy and trusted environments.** Where your organization allows it, a *trusted* environment runs scripts outside the sandbox, as you, each run approved; that is what arcpy is expected to need. `whoami` reports arcpy and your Pro licence in plain words. Both are marked **experimental** until checked on a real ArcGIS Pro computer (optional check H-2).
- **File geodatabase and GeoTIFF exports.** `export_output` writes a new `.gdb` (64-bit whole numbers kept exact, flagged as needing ArcGIS Pro 3.2 or later) and raster results as Cloud-Optimized GeoTIFFs. Workflows can deliver both.
- **Your weekly ROW job end to end.** Line geometries with M values (route measures) now survive every step. Workflows read files and ArcGIS layers and keep an ArcGIS sign-in alive across weekly runs. **Open in a chat** puts a run's results on the map for review; clicking a table row zooms to it; **Measure** gives feet and metres; `vector_op` `filter` removes rows ("leave out route X") without a script.
- **Web map layers.** Add imagery and other services (XYZ, WMTS, WMS, ArcGIS map, image and feature services) to your map under **Connections › Map layers** or **Add web layer**. They are display only: every request goes through Columbus, and your assistant never sees them.
- **Faster and safer underneath.** Bounding-box queries use each database's spatial index; every engine package is installed by exact version and hash; tests that need the engine can no longer be skipped silently.

**Needs macOS 15 or later on a Mac** (Apple Silicon). Windows 10 and 11 (64-bit) as before.

### Not in this release

- A personal tool can't yet import an environment's other packages (whitebox, arcpy) beyond the tool rules' module list; scripts can (CR-1, sent to Blueprint for a decision). So arcpy works in scripts you approve, not in personal tools or workflows.
- A documentation website (CR-3, a later round).
- Screen redesign: a separate review of the app's screens comes next.

### What we could and couldn't confirm

- **ArcGIS sign-in route (A-2): unconfirmed.** Esri's documentation couldn't be checked, so Columbus supports both ways back from the browser: straight to Columbus (`http://127.0.0.1`), or a code you paste into the terminal (`--paste-code`). Register both redirect URLs; H-1 tells you which your portal accepts.
- **Engine updates (A-4): Dependabot can't update the hash-pinned engine lists.** A weekly workflow regenerates them and opens a pull request instead; crates, npm packages and GitHub Actions are updated by Dependabot.

### Optional checks you can do (not needed to use this release)

- **H-1:** register your organization's ArcGIS app and try a signed-in layer.
- **H-2:** run one arcpy script on a Windows PC with ArcGIS Pro.

Both are step by step in [docs/manual-checks.md](https://github.com/wiatrek-kevin-s/columbus/blob/main/docs/manual-checks.md).

### Upgrading from 0.13

1. Install the new extension and app (below).
2. **Run the engine setup once more** (Mac: `sh engine/setup-engine.sh` in your Columbus folder; Windows: `engine\setup-engine.ps1`), from the new server folder. It reinstalls every package with hash checks and adds Columbus's own uv.
3. Nothing else to change: the new policy keys (`environments`, `scripts.session_approval`, `map`) are off or empty by default, and `file` in `connection_types` is read as `files`. Folder-of-files caches rebuild once by themselves on first use.

Licences of software Columbus carries are in `THIRD-PARTY-NOTICES.md` in the server folder.

**In 0.13.0:** importing documents as notes, sharing tools and workflows with coworkers, labelled map snapshots, workflow email.

## Mac (Apple Silicon, macOS 15 or later)

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
