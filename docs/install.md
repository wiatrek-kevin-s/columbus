# Installing Columbus on Windows

Columbus is a governed GIS server for AI agents. It runs on your computer, beside the AI client you already use (Claude desktop, GitHub Copilot in VS Code, or any MCP client), and enforces what the agent may read, write, run and send.

These steps are for Columbus 0.14.0 on Windows 10 and 11 (64-bit). The server (step 1 to 4) is all you need; the Columbus app (step 5) is optional. For a Mac, see [install-macos.md](install-macos.md).

**Updating from 0.13?** Install the new `.mcpb` and app as in steps 4 and 5, then run the engine setup (step 2) once more: it reinstalls every engine package from the hash-checked lists. The new policy switches (Python environments, session approval for scripts) are off until your organization turns them on, and folder-of-files caches rebuild once by themselves on first use. Nothing else changes in your settings.

## 1. Unpack

Download `columbus-<version>-windows-x64.zip` from the release and unpack it to a folder of your own, for example `C:\Columbus`. In an organization, IT usually installs it under `C:\Program Files\Columbus`.

## 2. Set up the GIS engine (once)

Columbus's GIS engine needs Python 3.12. If you don't have it, install it from python.org or with `winget install Python.Python.3.12`. Then, in the Columbus folder:

```powershell
powershell -ExecutionPolicy Bypass -File engine\setup-engine.ps1
```

This creates `engine\venv` with GDAL, PROJ, GeoPandas and rasterio (about 300 MB). At the end it prints the engine's Python path; keep it for step 4. Every package is pinned by exact version and SHA-256 (`workers\python\requirements-gis.txt`, `-db.txt`, `-projectdb.txt` and `-uv.txt`, covering CPython 3.12 and 3.13 on Windows x64, macOS arm64 and Linux x64) and installed with `--require-hashes`, so a file that differs from the pinned one is refused. On a machine without internet access, run it with `-Wheels <folder>` pointing at a folder of downloaded wheels. That folder must hold exactly the pinned files, for example from these on a connected Windows machine with Python 3.12: `pip download --require-hashes -r workers\python\requirements-gis.txt -r workers\python\requirements-db.txt -d <folder>` and `pip download --require-hashes --no-deps --only-binary :all: -r workers\python\requirements-projectdb.txt -r workers\python\requirements-uv.txt -d <folder>` (DuckDB and its spatial extension 1.5.5, `pytz`, which DuckDB needs to return timestamps with a time zone, and Columbus's uv); any other file is refused.

To change a pinned package (for maintainers), edit its `workers/python/requirements-*.in` source and run `python packaging/engine/compile-requirements.py` (add `--upgrade` to move every package to the newest version its range allows); it regenerates the `.txt` files with `uv pip compile --universal --generate-hashes` and checks every system has wheels. A weekly workflow (Engine requirements) does the `--upgrade` run and opens a pull request.

## 3. Tell Columbus what it may use

**The easy way: the Columbus app.** Install the app (step 5), open it and choose **Data** in the bar at the top of the panel, then **Add data**. There you add connections (Browse for a GeoPackage, a file geodatabase (.gdb), a folder of shapefiles, a folder of rasters, or a **Folder of files** (CSV, Excel `.xlsx`, GeoJSON, GeoParquet, KML, FlatGeobuf); paste the address of an **ArcGIS service** (ArcGIS Online, Portal or ArcGIS Server); or fill in a database's host, login and password, which goes into your Credential Manager) and set each one to read only or read and write; **Settings › Organization rules** turns edits and scripts on or off. Anything that gives your assistant more access asks for Windows Hello first; your assistant cannot change these settings itself. **Test** checks that Columbus can open a connection before you save it. Each connection has a page of its own (click its row under **Data**) that says its rules in plain words. There, **Draft notes** writes notes about the connection's tables from the database itself (fields, keys, relationships, codes) so your assistant knows your data; you or your assistant fill in what it means, and you mark each note reviewed under **Data › Notes › My notes**. Changes reach your assistant on its next request; there is no need to restart Claude desktop.

**Or by hand,** as below.

Columbus reads the organization's settings from `%ProgramData%\Columbus` and your connections from `%APPDATA%\Columbus\connections`. In an organization, IT manages these files. On your own machine you set them up yourself.

- `policy.yaml` holds what is allowed at all: connection types, limits, hidden fields, export folders, download hosts. Start from `examples\policy.yaml`. With no file, Columbus allows only local GeoPackage, SQLite, shapefile, file geodatabase and folder-of-files connections, read-only, with no scripts, no edits and no Python environments.
- `%APPDATA%\Columbus\connections\*.yaml` has one file per database or folder. See `examples\connections`. Passwords never go in these files:

  ```powershell
  columbus-mcp secret set roads-prod
  ```

  This stores the password in Windows Credential Manager.
- Folders of files use `type: files` and ArcGIS services `type: arcgis`; see `examples\connections\crash-files.yaml` and `examples\connections\roads-agol.yaml`. Policy must list `files` and `arcgis` in `connection_types`, and an ArcGIS service's host (and its portal's, for signed-in layers) in `hosts`.
- `context\` holds your organization's context pack, which is optional. See `docs\context-pack.md` and `examples\context\contoso-county`.
- To install the open geospatial registry, run `columbus-mcp registry update`.

## 4. Connect your AI client

**Claude desktop:** install `columbus-<version>-windows-x64.mcpb` from the release as an extension (Claude desktop opens `.mcpb` files). When asked, leave the folder fields empty to use the folders above, and paste the Python path that `setup-engine.ps1` printed at the end into **Engine Python** (the extension runs from its own folder, so it does not find the engine by itself).

**Any MCP client (stdio):** add a server entry like this one, using your own path:

```json
{
  "mcpServers": {
    "columbus": { "command": "C:\\Columbus\\columbus-mcp.exe", "args": ["serve"] }
  }
}
```

Then ask the agent to call `whoami`. It shows what Columbus allows in this session, where approvals will appear, and the organization's always-on rules.

## 5. The Columbus app (optional)

The app is one window. The map fills it, and one panel floats on the map, at the left; everything else is in that panel. The bar at the top of the panel has four places: **Chats**, **Data** (what your assistant can reach, and your notes), **Workflows** (with your tools and skills) and **Activity** (requests waiting, then the audit log and exports). Beside them are **New chat**, **Search** and **Settings**. The app also shows each Columbus session running on your computer: its layers on a map, its tables, the checks and where each result came from, and requests waiting for your decision. Install `columbus-app-<version>-windows-x64-setup.exe` (per user, no administrator rights) or the `.msi` from the release. It is not code-signed yet, so Windows SmartScreen asks first: **More info**, then **Run anyway**.

Open Columbus while your assistant is working (ask it to call `whoami` if no session is listed). Under **Chats**, a session from Claude desktop is in the list with your own chats. Open it to see what its assistant did, with its map behind the panel; its files are under **Files this chat made** in the **⋯** menu. Requests that need your decision are listed in **Activity**, under **Waiting for you**, and above the session's activity, with **Decide in browser**: you decide them in your AI app's own prompt or on the page Columbus opens in your browser, never in the app, so no other program can approve them for you. The street background is off until you turn it on (map tiles come from OpenFreeMap, which then sees the area in view); the **Street background** switch in **Settings** sets whether it starts on.

The app opens on a question: a new chat, with things to try and your recent chats. The first time, in place of the question, it says **Start with your data**, with **Add data** to add a first connection (read only, unless you choose otherwise). Two checks are under it: connect an AI for the app's own chat, and the GIS engine. Each can be skipped, and **Skip setup** skips them all.

**Classic windows**, at the foot of **Settings**, opens the previous session list and settings window, kept for now in case you prefer them.

### Chat in the app

The app can also chat with Claude itself, using an Anthropic API key (billed by Anthropic per use, separate from a Claude subscription; create one at console.anthropic.com). Open **Settings › Assistant** (or use the check on the first-run screen), paste the key and press **Save key**: it goes into your computer's credential store, and the app can never read it back. Then press **New chat**. The chat starts its own Columbus session from the server that comes with the app (or, in a build without one, the one in `C:\\Columbus`), so the same policy, connections and checks apply. The GIS engine it uses can be chosen in **Settings › Assistant**; choosing another asks you to confirm first. Its steps fold into one line per turn (open it to see each step, with **Provenance** for results that made data). The last result of a turn is a card with **Table**, which opens the table on the map, and **How it was made**, which shows its checks, inputs and files in a sheet beside the panel. The chat's map fills the window behind the panel; **Show only the map**, in the chat's **⋯** menu, puts the panel away, and a small pill brings it back. When something needs your approval, the chat shows a card (**Columbus needs your OK**) with **Allow once**, **Allow for this chat** where Columbus offers it, and **Decline**. For a Python script, the card shows the code the AI wrote in its own box, apart from what Columbus says about the run: read it before allowing. **New chat** ends the chat and its session; your chats are kept on this computer, listed under **Chats** and found with **Ctrl+K** (⌘K on a Mac), but an ended chat cannot be continued. Closing the app's window ends the chat.

## 6. ArcGIS sign-in (optional)

Public ArcGIS layers need no sign-in. For layers shared only inside your organization, your ArcGIS administrator registers an OAuth app for Columbus once and gives you its **client id** (Columbus ships none; the steps are in [manual-checks.md](manual-checks.md), H-1). Put it in the connection: in the app, **Data**, the ArcGIS service (**Add data** for a new one, or **Change the rules** on its page), **Sign-in: ArcGIS OAuth**, with the **Portal** (for example `https://www.arcgis.com` or `https://gis.example.org/portal`) and the **Client id**; or in the connection file:

```yaml
auth: { type: oauth, portal: https://www.arcgis.com, client_id: <your organization's client id> }
```

Then sign in once: press **Sign in** on the connection's page in the app (Windows Hello asks first), or run `columbus-mcp arcgis signin <connection id>` in a terminal. Your browser opens ArcGIS's sign-in page; after you sign in, it comes back to Columbus. If it doesn't, run `columbus-mcp arcgis signin <connection id> --paste-code` and paste the code ArcGIS shows into the terminal (never into a chat). The sign-in is kept in Windows Credential Manager, never in a file, and your assistant never sees it. When it lapses (ArcGIS ends sign-ins after about two weeks, as your organization sets), Columbus says "Sign in again: …"; a weekly workflow renews it by itself. `columbus-mcp arcgis signout <connection id>` removes it.

## 7. Python environments (optional)

Scripts run in the engine's own Python unless you register another Python, an **environment**: an ArcGIS Pro clone, a venv of your own, or one Columbus creates for you. Organization policy must turn this on (`environments: { allow: true }`, see `examples\policy.yaml`); it is off by default.

1. Register one, at the keyboard: in the app, **Settings › Python environments › Add environment** (Windows Hello confirms), or in a terminal:

   ```powershell
   columbus-mcp env add pro --python "C:\Users\<you>\AppData\Local\ESRI\conda\envs\arcgispro-py3-clone\python.exe"
   ```

   Columbus checks the interpreter (Python version, packages, whether arcpy imports and its licence) and asks you to type the id to confirm. Your assistant can't register or change one.
2. **Governed** (the default) environments run scripts in the same sandbox as the engine's: no network, extracts of your data only. Use them for any library the engine lacks.
3. **Trusted** environments (**experimental**) run scripts unconfined, as you, every run approved. They need `environments: { allow_trusted: true }` in policy and `--trust trusted` when you register. arcpy is expected to need one (its licence check reads your profile); use one only if your organization accepts that Columbus can't enforce your connection rules inside it. Running arcpy under your Pro licence is your organization's call.
4. If you change an environment (for example `pip install` into it), Columbus stops using it until you confirm it again: **Settings › Python environments › Confirm again**, or `columbus-mcp env confirm <id>`. `columbus-mcp env list` shows each one and its state.
5. Your assistant can ask for a new environment with exact package versions (`propose_environment`); you approve it in **Settings › Python environments** (or `columbus-mcp env approve <id>`), and Columbus creates it with its own pinned uv, wheels only, from `environments.index_hosts`.

`whoami` lists your environments, with arcpy and its licence in plain words ("ArcGIS Pro licence: Advanced", or "not initialized: open ArcGIS Pro and sign in …").

## 8. Skills

The `skills` folder holds GIS skills: CRS choice, joins, QA, spatial SQL, rasters, lidar, imagery, deliverables, SOP conversion and more. Copy them where your agent loads skills, for example `%USERPROFILE%\.claude\skills` for Claude Code.

## Check it works

- `columbus-mcp --version` prints the version.
- `columbus-mcp audit verify %ProgramData%\Columbus\audit\<month>.jsonl` checks the audit log's hash chain.
- In the AI client, `whoami` reports `engines` as ready once the engine is set up.

## Known limits in this release

- Oracle (`oracle`, `sde-oracle`) and SQL Server (`sqlserver`, `mssql`, `sde-sqlserver`, `azure-sql`) connections read through `engine\setup-engine.ps1`'s database drivers: python-oracledb in thin mode (no Oracle Client) and pyodbc, which also needs Microsoft ODBC Driver 18 for SQL Server installed. SQL Server has no read-only session: Columbus rolls back every query's transaction, but give it a login with SELECT only. Esri `ST_Geometry` columns on Oracle are reported as unsupported geometry, not read.
- On Windows, confinement level C is not available yet. Servers and plugins run at A or B.
- No point-cloud processing and no edits to geodatabases.
- arcpy runs only in a trusted Python environment you register (experimental, above); not yet tested on a real ArcGIS Pro machine (see [manual-checks.md](manual-checks.md), H-2). Personal tools and workflows can't use arcpy yet.
