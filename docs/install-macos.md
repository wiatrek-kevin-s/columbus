# Installing Columbus on a Mac

Columbus is a governed GIS server for AI agents. It runs on your computer, beside the AI client you already use (Claude desktop, or any MCP client), and enforces what the agent may read, write, run and send.

This build is for Macs with Apple Silicon (M1 and later) on macOS 13 or later. The Columbus app (step 5) is optional. For Windows, see [install.md](install.md).

## What works on a Mac

Everything works as on Windows: describing, querying and processing vector and raster data; PostGIS, Oracle, SQL Server, GeoPackage and file connections; verification checks; exports to approved folders; the context pack; the open geospatial registry and its downloads; approvals; the audit log; remote (HTTP) MCP servers.

Engine plugins, `run_python` scripts and local MCP servers run inside the macOS sandbox, which limits what they can read, write and reach (see "Confinement levels" in the README). Before the first one starts, Columbus checks that the sandbox works on this Mac, and refuses them if not; `whoami` says which.

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

This creates `engine/venv` with GDAL, PROJ, GeoPandas and rasterio (about 300 MB). At the end it prints the engine's Python path; keep it for step 4.

## 3. Tell Columbus what it may use

**The easy way: the Columbus app.** Install the app (step 5), open it and press **Settings**. There you add connections (Browse for a GeoPackage or a folder of rasters, or fill in a database's host, login and password, which goes into your Keychain), set each one to read only or read and write, and turn edits and scripts on or off. Anything that gives your assistant more access asks for Touch ID or your Mac password first; your assistant cannot change these settings itself. **Test** checks that Columbus can open a connection before you save it. Changes reach your assistant on its next request; there is no need to restart Claude desktop.

**Or by hand,** as below.

Columbus reads the organization's settings from `/Library/Application Support/Columbus` and your own from `~/Library/Application Support/Columbus`. On your own Mac you create the organization folder once, owned by you:

```sh
sudo mkdir -p "/Library/Application Support/Columbus"
sudo chown "$USER" "/Library/Application Support/Columbus"
cp ~/Columbus/examples/policy.yaml "/Library/Application Support/Columbus/"
mkdir -p ~/Library/Application\ Support/Columbus/connections
```

- `policy.yaml` holds what is allowed at all: connection types, limits, hidden fields, export folders, download hosts. With no file, Columbus allows only local GeoPackage and SQLite files, read-only.
- `~/Library/Application Support/Columbus/connections/` holds one `.yaml` file per database or folder. See `~/Columbus/examples/connections`. Passwords never go in these files:

  ```sh
  ~/Columbus/columbus-mcp secret set roads-prod
  ```

  This stores the password in your login Keychain. Each new Columbus version is a new program to macOS, so the first time a new version reads a password, macOS asks whether `columbus-mcp` may use it: choose **Always Allow**.
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

The app shows each Columbus session running on your Mac: its layers on a map, its tables, the checks and where each result came from, and requests waiting for your decision. Download `columbus-app-<version>-macos-arm64.dmg` from the release, open it and drag Columbus to Applications. It is not signed with an Apple Developer ID yet, so clear the download mark once before opening it:

```sh
xattr -dr com.apple.quarantine /Applications/Columbus.app
```

Open Columbus while your assistant is working (ask it to call `whoami` if no session is listed), and press **Open** beside the session. Requests that need your decision show in the window, with **Review in your browser**: you decide them on the page Columbus opens in your browser, never in the app, so no other program can approve them for you. The basemap is off until you turn it on (map tiles come from OpenFreeMap, which then sees the area in view).


### Chat in the app

The app can also chat with Claude itself, using an Anthropic API key (billed by Anthropic per use, separate from a Claude subscription; create one at console.anthropic.com). Press **Chat**, open **Settings**, paste the key and press **Save key**: it goes into your computer's credential store. The chat starts its own Columbus session from the server in `~/Columbus` (change it in Settings if yours is elsewhere), so the same policy, connections and checks apply. When something needs your approval, the chat shows the request with **Allow once**, **Allow for this session** and **Decline**. For a Python script, the request shows the code the AI wrote in its own box, apart from what Columbus says about the run: read it before allowing. Closing the chat window ends its session. **Show map** opens the session's window with its layers.

## 6. Skills

The `skills` folder holds GIS skills. Copy them where your agent loads skills, for example `~/.claude/skills` for Claude Code.

## Check it works

- `~/Columbus/columbus-mcp --version` prints the version.
- `~/Columbus/columbus-mcp audit verify "/Library/Application Support/Columbus/audit/<month>.jsonl"` checks the audit log's hash chain. (Without the organization folder from step 3, the log is in `~/Library/Application Support/Columbus/audit` instead, and `whoami` says so.)
- In Claude, `whoami` reports `engines` as ready once the engine is set up.
