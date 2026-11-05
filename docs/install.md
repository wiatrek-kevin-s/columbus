# Installing Columbus on Windows

Columbus is a governed GIS server for AI agents. It runs on your computer, beside the AI client you already use (Claude desktop, GitHub Copilot in VS Code, or any MCP client), and enforces what the agent may read, write, run and send.

This release is the server alone. The Columbus desktop app (chat, map and approvals in one window) comes later. For a Mac, see [install-macos.md](install-macos.md).

## 1. Unpack

Download `columbus-<version>-windows-x64.zip` from the release and unpack it to a folder of your own, for example `C:\Columbus`. In an organization, IT usually installs it under `C:\Program Files\Columbus`.

## 2. Set up the GIS engine (once)

Columbus's GIS engine needs Python 3.12. If you don't have it, install it from python.org or with `winget install Python.Python.3.12`. Then, in the Columbus folder:

```powershell
powershell -ExecutionPolicy Bypass -File engine\setup-engine.ps1
```

This creates `engine\venv` with GDAL, PROJ, GeoPandas and rasterio (about 300 MB). At the end it prints the engine's Python path; keep it for step 4. On a machine without internet access, run it with `-Wheels <folder>` pointing at a folder of downloaded wheels.

## 3. Tell Columbus what it may use

**The easy way: the Columbus app.** Install the app (step 5), open it and press **Settings**. There you add connections (Browse for a GeoPackage or a folder of rasters, or fill in a database's host, login and password, which goes into your Credential Manager), set each one to read only or read and write, and turn edits and scripts on or off. Anything that gives your assistant more access asks for Windows Hello first; your assistant cannot change these settings itself. After a change, quit and reopen Claude desktop.

**Or by hand,** as below.

Columbus reads the organization's settings from `%ProgramData%\Columbus` and your connections from `%APPDATA%\Columbus\connections`. In an organization, IT manages these files. On your own machine you set them up yourself.

- `policy.yaml` holds what is allowed at all: connection types, limits, hidden fields, export folders, download hosts. Start from `examples\policy.yaml`. With no file, Columbus allows only local GeoPackage and SQLite files, read-only, with no scripts and no edits.
- `%APPDATA%\Columbus\connections\*.yaml` has one file per database or folder. See `examples\connections`. Passwords never go in these files:

  ```powershell
  columbus-mcp secret set roads-prod
  ```

  This stores the password in Windows Credential Manager.
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

The app shows each Columbus session running on your computer: its layers on a map, its tables, the checks and where each result came from, and requests waiting for your decision. Install `columbus-app-<version>-windows-x64-setup.exe` (per user, no administrator rights) or the `.msi` from the release. It is not code-signed yet, so Windows SmartScreen asks first: **More info**, then **Run anyway**.

Open Columbus while your assistant is working and press **Open** beside the session. Requests that need your decision show in the window, with **Review in your browser**: you decide them on the page Columbus opens in your browser, never in the app. The basemap is off until you turn it on (map tiles come from OpenFreeMap, which then sees the area in view).


### Chat in the app

The app can also chat with Claude itself, using an Anthropic API key (billed by Anthropic per use, separate from a Claude subscription; create one at console.anthropic.com). Press **Chat**, open **Settings**, paste the key and press **Save key**: it goes into your computer's credential store. The chat starts its own Columbus session from the server in `C:\\Columbus` (change it in Settings if yours is elsewhere), so the same policy, connections and checks apply. When something needs your approval, the chat shows the request with **Allow once**, **Allow for this session** and **Decline**. For a Python script, the request shows the code the AI wrote in its own box, apart from what Columbus says about the run: read it before allowing. Closing the chat window ends its session. **Show map** opens the session's window with its layers.

## 6. Skills

The `skills` folder holds GIS skills: CRS choice, joins, QA, spatial SQL, rasters, lidar, imagery, deliverables, SOP conversion and more. Copy them where your agent loads skills, for example `%USERPROFILE%\.claude\skills` for Claude Code.

## Check it works

- `columbus-mcp --version` prints the version.
- `columbus-mcp audit verify %ProgramData%\Columbus\audit\<month>.jsonl` checks the audit log's hash chain.
- In the AI client, `whoami` reports `engines` as ready once the engine is set up.

## Known limits in this release

- Oracle (`oracle`, `sde-oracle`) and SQL Server (`sqlserver`, `mssql`, `sde-sqlserver`, `azure-sql`) connections read through `engine\setup-engine.ps1`'s database drivers: python-oracledb in thin mode (no Oracle Client) and pyodbc, which also needs Microsoft ODBC Driver 18 for SQL Server installed. SQL Server has no read-only session: Columbus rolls back every query's transaction, but give it a login with SELECT only. Esri `ST_Geometry` columns on Oracle are reported as unsupported geometry, not read.
- On Windows, confinement level C is not available yet. Servers and plugins run at A or B.
- No point-cloud processing, no edits to geodatabases, no ArcPy.
