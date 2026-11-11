# Installing Columbus on Windows

Columbus is a governed GIS server for AI agents. It runs on your computer, beside the AI client you already use (Claude desktop, GitHub Copilot in VS Code, or any MCP client), and enforces what the agent may read, write, run and send.

These steps are for Columbus 0.15.0 on Windows 10 and 11 (64-bit). The server (step 1 to 4) is all you need; the Columbus app (step 5) is optional. For a Mac, see [install-macos.md](install-macos.md).

**Updating from 0.14?** Install the new `.mcpb` and app as in steps 4 and 5. The engine's packages did not change, so step 2 need not be run again. Nothing loosens by itself: working freely on copies, toolboxes, project files and sync are off until you choose them in the app (step 3) or your organization writes them in `policy.yaml`. The app's window is new: a sidebar, the chat, and a panel on the right. (From 0.13 or earlier, also run the engine setup, step 2, once more.)

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

**The easy way: the Columbus app.** Install the app (step 5), open it and choose **Data** in the sidebar on the left, then **Add data**. There you add connections (Browse for a GeoPackage, a file geodatabase (.gdb), a folder of shapefiles, a folder of rasters, or a **Folder of files** (CSV, Excel `.xlsx`, GeoJSON, GeoParquet, KML, FlatGeobuf); paste the address of an **ArcGIS service** (ArcGIS Online, Portal or ArcGIS Server); or fill in a database's host, login and password, which goes into your Credential Manager) and set each one to read only or read and write; **Settings › Organization rules** turns edits, scripts and the other switches on or off. Anything that gives your assistant more access asks for Windows Hello first; your assistant cannot change these settings itself. **Test** checks that Columbus can open a connection before you save it. Each connection has a page of its own (click its row under **Data**) that says its rules in plain words. There, **Draft notes** writes notes about the connection's tables from the database itself (fields, keys, relationships, codes) so your assistant knows your data; you or your assistant fill in what it means, and you mark each note reviewed under **Documentation › My notes**. Changes reach your assistant on its next request; there is no need to restart Claude desktop.

**Or by hand,** as below.

Columbus reads the organization's settings from `%ProgramData%\Columbus` and your connections from `%APPDATA%\Columbus\connections`. In an organization, IT manages these files. On your own machine you set them up yourself.

- `policy.yaml` holds what is allowed at all: connection types, limits, hidden fields, export folders, download hosts. With no file, Columbus allows only local GeoPackage, SQLite, shapefile, file geodatabase and folder-of-files connections, read-only, with no scripts, no edits and no Python environments. The app does not create this file. On your own PC, create it once from the example, in an ordinary PowerShell window (not one run as administrator, so that the file is yours to change) opened in the Columbus folder:

  ```powershell
  New-Item -ItemType Directory -Force "$env:ProgramData\Columbus" | Out-Null
  Copy-Item examples\policy.yaml "$env:ProgramData\Columbus\policy.yaml"
  ```

  Then open the copy in Notepad and make it yours: set your own export folders (`exports`), and take out the example lines you don't need (`hosts`, `hide_fields`, `flows`, the layer under `map: sources`).
- `%APPDATA%\Columbus\connections\*.yaml` has one file per database or folder. See `examples\connections`. Passwords never go in these files:

  ```powershell
  columbus-mcp secret set roads-prod
  ```

  This stores the password in Windows Credential Manager.
- Folders of files use `type: files` and ArcGIS services `type: arcgis`; see `examples\connections\crash-files.yaml` and `examples\connections\roads-agol.yaml`. Policy must list `files` and `arcgis` in `connection_types`, and an ArcGIS service's host (and its portal's, for signed-in layers) in `hosts`.
- `context\` holds your organization's context pack, which is optional. See `docs\context-pack.md` and `examples\context\contoso-county`.
- To install the open geospatial registry, run `columbus-mcp registry update`.

**How your assistant works (new in 0.15).** The app asks once, on its first-run screen and at the top of **Settings**: **Work freely on copies** (recommended) or **Ask me before each script**. With the first, scripts and installed tools run without asking you each time, in the operating system's sandbox, on copies of your data: no network, and no way to your connections, your passwords or your other files. Columbus still asks before an export to a folder that isn't pre-approved, a download from a host not on the list, any edit to a source, and code that needs the network or must run outside the sandbox (a trusted environment, arcpy). Choosing it asks for Windows Hello once and writes seven settings to `policy.yaml`: `allow_scripts: true`, `scripts.approval: none`, `environments.allow: true`, `allow_personal_tools: true`, `allow_toolboxes: true`, `allow_project_files: true` and `allow_sync: true`. **Ask me before each script** sets `scripts.approval: each` and leaves the others as they are; scripts, personal tools, toolboxes, project files and sync each have a switch of their own under **Settings › Organization rules**. On a PC with no `policy.yaml` the app says to create it first (above); where your organization manages the file, the app shows what is in force and offers no choice.

The policy keys that are new in 0.15, all off or empty until someone sets them (`examples\policy.yaml` shows each):

| Key | What it allows |
| --- | --- |
| `scripts.approval` | When a script that stays in the sandbox asks you first: `each` run (the default), once per `session` for the same script, or `none`. A script that needs the network, or runs in a trusted environment, asks every time whatever this says |
| `allow_toolboxes` | Toolboxes may be installed and used (step 8). `toolboxes: { catalogue: false }` stops Columbus's catalogue from downloading any |
| `allow_project_files` | Your assistant may read and write text files in a project's own folder, and run scripts kept there |
| `allow_sync` | A project's files may sync with a Git server when you press **Sync** (step 9). `sync: { hosts: [...] }` names the only servers allowed |

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

The app is one window with three parts: a sidebar on the left, the chat (or the page of the place you chose) in the middle, and a panel on the right that opens with the chat's map and table when your assistant makes something. The sidebar has **New chat** and **Search**, then seven places: **Chats**, **Projects**, **Data** (what your assistant can reach), **Documentation** (your notes and your organization's context pack), **Workflows**, **Tools** (toolboxes, your tools, and the skills) and **Activity** (requests waiting, then the audit log and exports). Under them are your recent chats, and at the bottom **Settings**. The app also shows each Columbus session running on your computer: its layers on a map, its tables, the checks and where each result came from, and requests waiting for your decision. Install `columbus-app-<version>-windows-x64-setup.exe` (per user, no administrator rights) or the `.msi` from the release. It is not code-signed yet, so Windows SmartScreen asks first: **More info**, then **Run anyway**.

Open Columbus while your assistant is working (ask it to call `whoami` if no session is listed). Under **Chats**, a session from Claude desktop is in the list with your own chats. Open it to see what its assistant did, with its map in the panel on the right; its files are under **Files this chat made** in the **⋯** menu. Requests that need your decision are listed in **Activity**, under **Waiting for you**, and above the session's activity, with **Decide in browser**: you decide them in your AI app's own prompt or on the page Columbus opens in your browser, never in the app, so no other program can approve them for you. The street background is off until you turn it on (map tiles come from OpenFreeMap, which then sees the area in view); the **Street background** switch in **Settings** sets whether it starts on.

The app opens on a question in the middle of the window: a new chat, with things to try. The first time, in place of the question, it says **Start with your data**, with **Add data** to add a first connection (read only, unless you choose otherwise). Two checks are under it: connect an AI for the app's own chat, and the GIS engine. Each can be skipped, and **Skip setup** skips them all. Under the checks it asks how your assistant should work (step 3).

**Classic windows**, at the foot of **Settings**, opens the previous session list and settings window, kept for now in case you prefer them.

### Chat in the app

The app can also chat with Claude itself, using an Anthropic API key (billed by Anthropic per use, separate from a Claude subscription; create one at console.anthropic.com). Open **Settings › Assistant** (or use the check on the first-run screen), paste the key and press **Save key**: it goes into your computer's credential store, and the app can never read it back. Then press **New chat**. The chat starts its own Columbus session from the server that comes with the app (or, in a build without one, the one in `C:\\Columbus`), so the same policy, connections and checks apply. The GIS engine it uses can be chosen in **Settings › Assistant**; choosing another asks you to confirm first. Its steps fold into one line per turn (open it to see each step, with **Provenance** for results that made data). The last result of a turn is a card with **Table**, which opens the table on the map, and **How it was made**, which shows its checks, inputs and files; both open in the panel on the right. That panel opens with the chat's map the first time your assistant makes a result, and **Map**, at the top of the chat, opens and closes it. **Fill the window with the map**, in the chat's **⋯** menu, puts the chat away so the map has the window, and **Show the chat again**, in the panel's head, brings it back. When something needs your approval, the chat shows a card (**Columbus needs your OK**) with **Allow once**, **Allow for this chat** where Columbus offers it, and **Decline**. Under **Work freely on copies** a script that stays in the sandbox runs without a card. Under **Ask me before each script**, and for any script that needs the network or runs outside the sandbox, the card shows the code the AI wrote in its own box, apart from what Columbus says about the run: read it before allowing. **New chat** ends the chat and its session; your chats are kept on this computer, listed under **Chats** and found with **Ctrl+K** (⌘K on a Mac), but an ended chat cannot be continued. Closing the app's window ends the chat.

## 6. ArcGIS sign-in (optional)

Public ArcGIS layers need no sign-in. For layers shared only inside your organization, your ArcGIS administrator registers an OAuth app for Columbus once and gives you its **client id** (Columbus ships none; the steps are in [manual-checks.md](manual-checks.md), H-1). Put it in the connection: in the app, **Data**, the ArcGIS service (**Add data** for a new one, or **Change the rules** on its page), **Sign-in: ArcGIS OAuth**, with the **Portal** (for example `https://www.arcgis.com` or `https://gis.example.org/portal`) and the **Client id**; or in the connection file:

```yaml
auth: { type: oauth, portal: https://www.arcgis.com, client_id: <your organization's client id> }
```

Then sign in once: press **Sign in** on the connection's page in the app (Windows Hello asks first), or run `columbus-mcp arcgis signin <connection id>` in a terminal. Your browser opens ArcGIS's sign-in page; after you sign in, it comes back to Columbus. If it doesn't, run `columbus-mcp arcgis signin <connection id> --paste-code` and paste the code ArcGIS shows into the terminal (never into a chat). The sign-in is kept in Windows Credential Manager, never in a file, and your assistant never sees it. When it lapses (ArcGIS ends sign-ins after about two weeks, as your organization sets), Columbus says "Sign in again: …"; a weekly workflow renews it by itself. `columbus-mcp arcgis signout <connection id>` removes it.

## 7. Python environments (optional)

Scripts run in the engine's own Python unless you register another Python, an **environment**: an ArcGIS Pro clone, a venv of your own, or one Columbus creates for you. Organization policy must turn this on (`environments: { allow: true }`, see `examples\policy.yaml`; **Work freely on copies** sets it); it is off by default.

1. Register one, at the keyboard: in the app, **Settings › Python environments › Add environment** (Windows Hello confirms), or in a terminal:

   ```powershell
   columbus-mcp env add pro --python "C:\Users\<you>\AppData\Local\ESRI\conda\envs\arcgispro-py3-clone\python.exe"
   ```

   Columbus checks the interpreter (Python version, packages, whether arcpy imports and its licence) and asks you to type the id to confirm. Your assistant can't register or change one.
2. **Governed** (the default) environments run scripts in the same sandbox as the engine's: no network, extracts of your data only. Use them for any library the engine lacks.
3. **Trusted** environments (**experimental**) run scripts unconfined, as you, every run approved, whatever you chose in step 3. They need `environments: { allow_trusted: true }` in policy and `--trust trusted` when you register. arcpy is expected to need one (its licence check reads your profile); use one only if your organization accepts that Columbus can't enforce your connection rules inside it. Running arcpy under your Pro licence is your organization's call.
4. If you change an environment (for example `pip install` into it), Columbus stops using it until you confirm it again: **Settings › Python environments › Confirm again**, or `columbus-mcp env confirm <id>`. `columbus-mcp env list` shows each one and its state.
5. Your assistant can ask for a new environment with exact package versions (`propose_environment`); you approve it in **Settings › Python environments** (or `columbus-mcp env approve <id>`), and Columbus creates it with its own pinned uv, wheels only, from `environments.index_hosts`.

`whoami` lists your environments, with arcpy and its licence in plain words ("ArcGIS Pro licence: Advanced", or "not initialized: open ArcGIS Pro and sign in …").

## 8. Toolboxes (optional)

New in 0.15. A toolbox is a program that brings many tools. With toolboxes turned on (`allow_toolboxes`; **Work freely on copies** sets it), you can install GeoLibre: about a thousand open-source terrain, hydrology, raster and vector tools. In the app: **Tools › Add tools**, then **Install** under **GeoLibre tools** (Windows Hello asks first). Or in a terminal:

```powershell
columbus-mcp toolbox install geolibre
```

Columbus shows what it is (version 1.8.1, MIT licence, a download of about 18 MB for Windows), downloads it from its publisher's release page on GitHub, checks the download against a SHA-256 pinned in Columbus before it unpacks anything, and lists the tools in the sandbox. Columbus never ships the program: it is fetched on your PC, when you ask. There is no build for Windows on ARM. `columbus-mcp toolbox list` shows what is installed, and `columbus-mcp toolbox remove geolibre` removes it. Any other command-line program can be added with a small description file ([Toolboxes](reference.md#toolboxes) in the reference shows how). Your assistant then finds a tool by what it does and runs it on copies, in the sandbox, without asking.

The real GeoLibre program was never run while Columbus 0.15 was built: Columbus's side was written from the program's source and tested with a stand-in. Your first install is its first real run ([manual-checks.md](manual-checks.md), H-3).

## 9. Sync a project with GitHub or Gitea (optional)

New in 0.15. With sync turned on (`allow_sync`; **Work freely on copies** sets it), a project's files folder can sync with a repository on github.com, a GitHub Enterprise Server, a Gitea or a Forgejo. Columbus talks to the server's web API, so no Git is needed on the PC, and nothing from a repository can run on it.

1. **Connect the server**, once: in the app, **Settings › Git servers › Connect a server**. Choose the kind and, unless it is github.com, give its address (https only). **Open the page for a new token** opens the server's own page for making one. On GitHub, choose the repositories and give the token **Contents: Read and write** (and **Administration: Read and write** if Columbus should create repositories for you); on a GitHub Enterprise Server, a classic token with the `repo` scope; on Gitea and Forgejo (**Settings › Applications**), the scopes `read:user` and `write:repository`. Paste the token into the **Token** field (never into a chat). Columbus asks the server whose token it is, then keeps it in Windows Credential Manager, never in a file.
2. **Link the project:** on the project's page, **Link this project**, then one of your repositories, another one written as `owner/name`, or a new private one. Linking sends none of your files.
3. **Check for changes** only reads. **Sync** gets the server's changes, then sends yours as one change, with your note.

A file changed on both sides is kept twice, and you choose: **Keep mine**, **Keep theirs** or **Keep both**. Data, keys and big files never sync (GeoPackages, shapefiles, file geodatabases, GeoTIFFs, GeoJSON, CSV and Excel files, databases, `.env`, `.sde`, files over 5 MB, the `outputs` folder; a table saved as `.txt` or `.json` is not recognized as data), a file your assistant wrote in a chat that had read one of your sources is held back until you choose **Send it anyway**, every other file in the folder is sent (but for links, a `.git` folder, a Python environment or its cache, and names another system cannot hold) and is listed before it goes, and text that looks like a password or a token stops the sync before anything is sent. This is not a Git clone: one branch, no history on the PC, and nothing syncs by itself. Sync was tested against stand-in servers, never a real GitHub or Gitea: try it first on a repository that does not matter ([manual-checks.md](manual-checks.md), H-4).

## 10. Skills

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
- Toolboxes: QGIS is not a toolbox yet, and workflows cannot use toolbox tools yet. GeoLibre has no build for Windows on ARM.
- Sync follows one branch of one repository, keeps no history on the PC and never runs by itself.
