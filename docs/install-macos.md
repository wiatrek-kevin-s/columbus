# Installing Columbus on a Mac

Columbus is a governed GIS server for AI agents. It runs on your computer, beside the AI client you already use (Claude desktop, or any MCP client), and enforces what the agent may read, write, run and send.

This build is for Macs with Apple Silicon (M1 and later) on macOS 15 or later (the engine's pinned packages, such as rasterio 1.5, publish Apple Silicon wheels for macOS 15 and later only). The Columbus app (step 5) is optional. These steps are for Columbus 0.14.0. For Windows, see [install.md](install.md).

**Updating from 0.13?** Install the new `.mcpb` and app as in steps 4 and 5, then run the engine setup (step 2) once more: it reinstalls every engine package from the hash-checked lists. The new policy switches (Python environments, session approval for scripts) are off until your organization turns them on, and folder-of-files caches rebuild once by themselves on first use.

## What works on a Mac

Everything works as on Windows: describing, querying and processing vector and raster data; PostGIS, Oracle, SQL Server, GeoPackage, shapefile, file geodatabase, folder-of-files (CSV, Excel, GeoJSON, GeoParquet, KML, FlatGeobuf) and ArcGIS Online/Portal connections; Python environments; verification checks; exports to approved folders; the context pack; the open geospatial registry and its downloads; approvals; the audit log; remote (HTTP) MCP servers.

Engine plugins, `run_python` scripts and local MCP servers run inside the macOS sandbox, which limits what they can read, write and reach (see "Confinement levels" in the [technical reference](reference.md#confinement-levels)). Before the first one starts, Columbus checks that the sandbox works on this Mac, and refuses them if not; `whoami` says which.

## 1. Download and unpack

Download `columbus-<version>-macos-arm64.zip` from the release page in your browser (while the repository is private, you must be signed in to GitHub; `curl` in Terminal gets a "Not Found" page instead). Safari unpacks it in Downloads, into a folder named after the file but cut at the first dot, such as `columbus-0`. Then, in Terminal (Applications → Utilities → Terminal):

```sh
mkdir -p ~/Columbus
cp -R ~/Downloads/columbus-0/ ~/Columbus/        # the folder Safari made
xattr -dr com.apple.quarantine ~/Columbus
~/Columbus/columbus-mcp --version
```

If the zip was not unpacked for you, use `unzip -q ~/Downloads/columbus-<version>-macos-arm64.zip -d ~/Columbus` in place of the `cp` line.

The `xattr` line matters: the program is not signed with an Apple Developer ID yet, and macOS blocks unsigned programs downloaded in a browser ("cannot be opened because Apple cannot check it for malicious software") until that mark is cleared.

## 2. Set up the GIS engine (once)

The engine needs Python 3.12: install it from [python.org](https://www.python.org/downloads/macos/) or with `brew install python@3.12`. Then:

```sh
cd ~/Columbus
conda deactivate 2>/dev/null   # only matters if Anaconda shows (base) in your prompt
sh engine/setup-engine.sh
```

This creates `engine/venv` with GDAL, PROJ, GeoPandas and rasterio (about 300 MB). At the end it prints the engine's Python path; keep it for step 4. Every package is pinned by exact version and SHA-256 (`workers/python/requirements-*.txt`) and installed with `--require-hashes`, so a file that differs from the pinned one is refused.

## 3. Tell Columbus what it may use

**The easy way: the Columbus app.** Install the app (step 5), open it and choose **Data** in the sidebar on the left, then **Add data**. There you add connections (Browse for a GeoPackage, a file geodatabase (.gdb), a folder of shapefiles, a folder of rasters or a **Folder of files** (CSV, Excel `.xlsx`, GeoJSON, GeoParquet, KML, FlatGeobuf); paste the address of an **ArcGIS service**; or fill in a database's host, login and password, which goes into your Keychain) and set each one to read only or read and write; **Settings › Organization rules** turns edits and scripts on or off. Anything that gives your assistant more access asks for Touch ID or your Mac password first; your assistant cannot change these settings itself. **Test** checks that Columbus can open a connection before you save it. Each connection has a page of its own (click its row under **Data**) that says its rules in plain words. There, **Draft notes** writes notes about the connection's tables from the database itself (fields, keys, relationships, codes) so your assistant knows your data; you or your assistant fill in what it means, and you mark each note reviewed under **Documentation › My notes**. Changes reach your assistant on its next request; there is no need to restart Claude desktop.

**Or by hand,** as below.

Columbus reads the organization's settings from `/Library/Application Support/Columbus` and your own from `~/Library/Application Support/Columbus`. On your own Mac you create the organization folder once, owned by you:

```sh
sudo mkdir -p "/Library/Application Support/Columbus"
sudo chown "$USER" "/Library/Application Support/Columbus"
cp ~/Columbus/examples/policy.yaml "/Library/Application Support/Columbus/"
mkdir -p ~/Library/Application\ Support/Columbus/connections
```

- `policy.yaml` holds what is allowed at all: connection types, limits, hidden fields, export folders, download hosts. With no file, Columbus allows only local GeoPackage, SQLite, shapefile, file geodatabase and folder-of-files connections, read-only, with no scripts, no edits and no Python environments.
- `~/Library/Application Support/Columbus/connections/` holds one `.yaml` file per database or folder. See `~/Columbus/examples/connections`. Passwords never go in these files:

  ```sh
  ~/Columbus/columbus-mcp secret set roads-prod
  ```

  This stores the password in your login Keychain. Each new Columbus version is a new program to macOS, so the first time a new version reads a password, macOS asks whether `columbus-mcp` may use it: choose **Always Allow**.
- Folders of files use `type: files` and ArcGIS services `type: arcgis`; see `~/Columbus/examples/connections/crash-files.yaml` and `roads-agol.yaml`. Policy must list `files` and `arcgis` in `connection_types`, and an ArcGIS service's host (and its portal's, for signed-in layers) in `hosts`.
- The context pack, which is optional, goes in `/Library/Application Support/Columbus/context`. See `docs/context-pack.md`.
- To install the open geospatial registry: `~/Columbus/columbus-mcp registry update`.

SQL Server connections also need unixODBC and Microsoft ODBC Driver 18 (`brew install unixodbc`, then [Microsoft's instructions](https://learn.microsoft.com/sql/connect/odbc/linux-mac/install-microsoft-odbc-driver-sql-server-macos)). Everything else works without them.

## 4. Connect Claude desktop

**The extension:** download `columbus-<version>-macos-arm64.mcpb` from the release and open it; Claude desktop installs it as an extension. Paste the Python path from step 2 into **Engine Python** (required). The folder fields can stay empty from version 0.1.4; in earlier versions, fill them in (Connections folder: `~/Library/Application Support/Columbus/connections`), because an empty field reached the server as the text `${user_config.connections}`. In a file picker, Cmd+Shift+G lets you paste a path.

**Or the config file:** in Claude desktop, Settings → Developer → Edit Config, and add (with your own user name):

```json
{
  "mcpServers": {
    "columbus": { "command": "/Users/<you>/Columbus/columbus-mcp", "args": ["serve"] }
  }
}
```

Restart Claude desktop, then ask it to call `whoami`. It shows what Columbus allows in this session, where approvals will appear, and that the engines are ready.

## 5. The Columbus app (optional)

The app is one window with three parts: a sidebar on the left, the chat (or the page of the place you chose) in the middle, and a panel on the right that opens with the chat's map and table when your assistant makes something. The sidebar has **New chat** and **Search**, then seven places: **Chats**, **Projects**, **Data** (what your assistant can reach), **Documentation** (your notes and your organization's context pack), **Workflows**, **Tools** (your tools, and the skills) and **Activity** (requests waiting, then the audit log and exports). Under them are your recent chats, and at the bottom **Settings**. The app also shows each Columbus session running on your Mac: its layers on a map, its tables, the checks and where each result came from, and requests waiting for your decision. Download `columbus-app-<version>-macos-arm64.dmg` from the release, open it and drag Columbus to Applications. It is not signed with an Apple Developer ID yet, so clear the download mark once before opening it:

```sh
xattr -dr com.apple.quarantine /Applications/Columbus.app
```

Open Columbus while your assistant is working (ask it to call `whoami` if no session is listed). Under **Chats**, a session from Claude desktop is in the list with your own chats. Open it to see what its assistant did, with its map in the panel on the right; its files are under **Files this chat made** in the **⋯** menu. Requests that need your decision are listed in **Activity**, under **Waiting for you**, and above the session's activity, with **Decide in browser**: you decide them in your AI app's own prompt or on the page Columbus opens in your browser, never in the app, so no other program can approve them for you. The street background is off until you turn it on (map tiles come from OpenFreeMap, which then sees the area in view); the **Street background** switch in **Settings** sets whether it starts on.

The app opens on a question in the middle of the window: a new chat, with things to try. The first time, in place of the question, it says **Start with your data**, with **Add data** to add a first connection (read only, unless you choose otherwise). Two checks are under it: connect an AI for the app's own chat, and the GIS engine. Each can be skipped, and **Skip setup** skips them all.

**Classic windows**, at the foot of **Settings**, opens the previous session list and settings window, kept for now in case you prefer them.

### Chat in the app

The app can also chat with Claude itself, using an Anthropic API key (billed by Anthropic per use, separate from a Claude subscription; create one at console.anthropic.com). Open **Settings › Assistant** (or use the check on the first-run screen), paste the key and press **Save key**: it goes into your computer's credential store, and the app can never read it back. Then press **New chat**. The chat starts its own Columbus session from the server that comes with the app (or, in a build without one, the one in `~/Columbus`), so the same policy, connections and checks apply. The GIS engine it uses can be chosen in **Settings › Assistant**; choosing another asks you to confirm first. Its steps fold into one line per turn (open it to see each step, with **Provenance** for results that made data). The last result of a turn is a card with **Table**, which opens the table on the map, and **How it was made**, which shows its checks, inputs and files; both open in the panel on the right. That panel opens with the chat's map the first time your assistant makes a result, and **Map**, at the top of the chat, opens and closes it. **Fill the window with the map**, in the chat's **⋯** menu, puts the chat away so the map has the window, and **Show the chat again**, in the panel's head, brings it back. When something needs your approval, the chat shows a card (**Columbus needs your OK**) with **Allow once**, **Allow for this chat** where Columbus offers it, and **Decline**. For a Python script, the card shows the code the AI wrote in its own box, apart from what Columbus says about the run: read it before allowing. **New chat** ends the chat and its session; your chats are kept on this computer, listed under **Chats** and found with **Ctrl+K** (⌘K on a Mac), but an ended chat cannot be continued. Closing the app's window ends the chat.

## 6. ArcGIS sign-in (optional)

Public ArcGIS layers need no sign-in. For layers shared only inside your organization, your ArcGIS administrator registers an OAuth app for Columbus once and gives you its **client id** (Columbus ships none; the steps are in [manual-checks.md](manual-checks.md), H-1). Put it in the connection: in the app, **Data**, the ArcGIS service (**Add data** for a new one, or **Change the rules** on its page), **Sign-in: ArcGIS OAuth**, with the **Portal** (for example `https://www.arcgis.com`) and the **Client id**; or in the connection file:

```yaml
auth: { type: oauth, portal: https://www.arcgis.com, client_id: <your organization's client id> }
```

Then sign in once: press **Sign in** on the connection's page in the app (Touch ID or your password asks first), or run `~/Columbus/columbus-mcp arcgis signin <connection id>` in Terminal. Your browser opens ArcGIS's sign-in page and comes back to Columbus. If it doesn't, add `--paste-code` and paste the code ArcGIS shows into Terminal (never into a chat). The sign-in is kept in your Keychain, never in a file, and your assistant never sees it. When it lapses (after about two weeks, as your organization sets), Columbus says "Sign in again: …"; a weekly workflow renews it by itself. `arcgis signout <connection id>` removes it.

## 7. Python environments (optional)

Scripts run in the engine's own Python unless you register another Python, an **environment**: a venv or conda environment of your own, or one Columbus creates for you. Organization policy must turn this on (`environments: { allow: true }`, see `examples/policy.yaml`); it is off by default.

1. Register one, at the keyboard: in the app, **Settings › Python environments › Add environment** (Touch ID confirms), or in Terminal: `~/Columbus/columbus-mcp env add geo --python /path/to/venv/bin/python3`. Columbus checks the interpreter and asks you to type the id to confirm. Your assistant can't register or change one.
2. **Governed** (the default) environments run scripts in the same sandbox as the engine's: no network, extracts of your data only.
3. **Trusted** environments (**experimental**) run scripts unconfined, as you, every run approved, only where policy says `allow_trusted: true`. arcpy needs Windows and ArcGIS Pro, so on a Mac you won't need one for it.
4. If you change an environment (a package installed), Columbus stops using it until you confirm it again (**Confirm again**, or `env confirm <id>`). `env list` shows each one and its state.
5. Your assistant can ask for a new environment with exact package versions (`propose_environment`); you approve it in **Settings › Python environments** (or `env approve <id>`), and Columbus creates it with its own pinned uv, wheels only.

## 8. Skills

The `skills` folder holds GIS skills. Copy them where your agent loads skills, for example `~/.claude/skills` for Claude Code.

## Check it works

- `~/Columbus/columbus-mcp --version` prints the version.
- `~/Columbus/columbus-mcp audit verify "/Library/Application Support/Columbus/audit/<month>.jsonl"` checks the audit log's hash chain. (Without the organization folder from step 3, the log is in `~/Library/Application Support/Columbus/audit` instead, and `whoami` says so.)
- In Claude, `whoami` reports `engines` as ready once the engine is set up.
