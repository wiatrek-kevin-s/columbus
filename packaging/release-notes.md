Columbus {{VERSION}}, a test release. Click a link to download that file.

**Fixed in this version:**
- **The app's chat now brings its own Columbus server.** Before, it started the server in your Columbus folder, which the update steps never replaced, so it could be many versions old: layer names with spaces were refused and the map beside the chat did not load. The app now carries the server of its own version and uses your Columbus folder only for the GIS engine. **Settings › Models and accounts** shows which server and engine a chat uses. If your Columbus folder is somewhere other than your home folder (or is `C:\Columbus` on Windows), Settings shows a **Use …** button for its engine: click it, then **Save settings**.
- **Choose the model and thinking level for each chat**, below the message box: Claude Sonnet 5.5, Opus 5.5, Fable 5.1 or Haiku 5.5, and thinking from Low to Max. Settings sets what a new chat starts with; a chat can switch between messages.
- **Tables in replies** show as tables.
- On Windows, a personal tool's run folder is emptied reliably after it runs.

**New in 0.9.0: a redesigned Columbus app.** One main window replaces the separate windows, with a menu down the side: Home, Chats, Projects, Documents, Skills, Connections, Tools, Activity and Settings.
- **Home** shows what needs you: requests waiting, notes in draft, tools to review and your recent chats, with a setup checklist the first time.
- **Chats** open with the map and table beside them (one click puts the map first). Tool steps fold into short lines with check badges; chats are saved and searchable with **Ctrl+K**. Sessions from Claude desktop show up too, with what they did and their map.
- **Connections, your notes** (now with **Mark reviewed**), **personal tools, settings**, an **approvals inbox** across every session (decide in the browser page, as before) and an **audit log viewer** are all pages of the main window. The old session list and settings windows are still under **Classic windows…** for now.
- Workflows, the registry and data groups have their pages, saying plainly that they are coming.
- A new compass icon.

**Also new: big tables and project databases.** Your organization can raise the per-query limits (up to 20 million rows), large results are saved to disk as they are read, and your assistant can build a local project database (DuckDB) to load several tables into, add keys and indexes, and join them with SQL, writing only to that one file, never to your databases. Your data rules, hidden fields and labels follow the data in, and a size cap protects your disk.

**Fixed:** saving a script as a tool now works in Claude (0.8.0 refused every proposal), and Columbus hands assistants its own guides (`columbus_guide`), including how to design a tool.

Since 0.8.0: your own tools (switch on **Allow personal tools** in Settings).

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
