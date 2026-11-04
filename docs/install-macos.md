# Installing Columbus on a Mac

Columbus is a governed GIS server for AI agents. It runs on your computer, beside the AI client you already use (Claude desktop, or any MCP client), and enforces what the agent may read, write, run and send.

This build is for Macs with Apple Silicon (M1 and later) on macOS 13 or later. It is the server alone: the Columbus desktop app comes later. For Windows, see [install.md](install.md).

## What works on a Mac, and what doesn't yet

Works as on Windows: describing, querying and processing vector and raster data; PostGIS, Oracle, SQL Server, GeoPackage and file connections; verification checks; exports to approved folders; the context pack; the open geospatial registry and its downloads; approvals; the audit log; remote (HTTP) MCP servers.

Refused on a Mac for now: engine plugins, `run_python` scripts, and local MCP servers. On Windows and Linux these run inside a sandbox that limits what they can touch. Columbus has no Mac sandbox yet, so it refuses them rather than run them unprotected. `whoami` says so.

## 1. Download and unpack

In Terminal (Applications → Utilities → Terminal), with the version from the release page in place of `<version>`:

```sh
mkdir -p ~/Columbus && cd ~/Columbus
curl -LO https://github.com/wiatrek-kevin-s/columbus/releases/download/v<version>/columbus-<version>-macos-arm64.zip
unzip -q columbus-<version>-macos-arm64.zip && rm columbus-<version>-macos-arm64.zip
./columbus-mcp --version
```

Downloading with `curl` matters. The program is not signed with an Apple Developer ID yet, and macOS blocks unsigned programs downloaded in a browser ("cannot be opened because Apple cannot check it for malicious software"). If you did download it in a browser, clear the download mark once:

```sh
xattr -dr com.apple.quarantine ~/Columbus
```

## 2. Set up the GIS engine (once)

The engine needs Python 3.12: install it from [python.org](https://www.python.org/downloads/macos/) or with `brew install python@3.12`. Then:

```sh
cd ~/Columbus
sh engine/setup-engine.sh
```

This creates `engine/venv` with GDAL, PROJ, GeoPandas and rasterio (about 300 MB). At the end it prints the engine's Python path; keep it for step 4.

## 3. Tell Columbus what it may use

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

**The extension:** download `columbus-<version>-macos-arm64.mcpb` from the release and open it; Claude desktop installs it as an extension. Leave the folder fields empty, and paste the Python path from step 2 into **Engine Python**.

**Or the config file:** in Claude desktop, Settings → Developer → Edit Config, and add (with your own user name):

```json
{
  "mcpServers": {
    "columbus": { "command": "/Users/<you>/Columbus/columbus-mcp", "args": ["serve"] }
  }
}
```

Restart Claude desktop, then ask it to call `whoami`. It shows what Columbus allows in this session, where approvals will appear, and that the engines are ready.

## 5. Skills

The `skills` folder holds GIS skills. Copy them where your agent loads skills, for example `~/.claude/skills` for Claude Code.

## Check it works

- `~/Columbus/columbus-mcp --version` prints the version.
- `~/Columbus/columbus-mcp audit verify "/Library/Application Support/Columbus/audit/<month>.jsonl"` checks the audit log's hash chain. (Without the organization folder from step 3, the log is in `~/Library/Application Support/Columbus/audit` instead, and `whoami` says so.)
- In Claude, `whoami` reports `engines` as ready once the engine is set up.
