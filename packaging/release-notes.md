Columbus {{VERSION}}, a test release. Click a link to download that file.

**New in this version: workflows.** Jobs that run by themselves on a schedule, without you waiting: for example, read a dataset, check it and put the results in your export folder every weekday at 06:00.
- **Ask your assistant** ("make what we just did a workflow that runs every weekday at 6"). It proposes one; you find it under **Workflows**, read what it does in plain words and in full, and approve it with Touch ID or Windows Hello. Nothing runs before that.
- **It does only what it lists:** it reads only the connections it names, its steps are reading and checking tools or personal tools you installed (never scripts or edits), and it never waits for you: a step that would need your approval stops the run with "needs you". A changed workflow, or a changed tool it uses, is off until you approve it again.
- **Run now**, **Pause**, and every run's **report** (steps, row counts, checks, what was exported). Home and the menu's foot show workflows that need you.
- **An optional AI review:** after the steps, a workflow can send a capped number of its result rows (only the fields you choose) with your criteria to Claude, using the API key you saved in the app, and keep its notes in the report, marked as the AI's. The reviewing AI has no tools: it cannot read anything else or change anything. On a Mac, the first **Run now** of such a workflow asks once for access to the key: choose **Always Allow**. If the review can't finish (no key, the service is busy, an incomplete answer), the run still delivers its results and says plainly that the AI review did not run; it never reads as "nothing found". An organization can turn reviews off (`deny_tools: [workflow_review]`) or keep a connection's data from the AI with a flow rule (to `ai:anthropic`; a rule for every outside service, `server:*`, covers it too).
- Turn them on with **Allow workflows** (Workflows page or Settings › Organization). Scheduled runs happen while you are signed in; one missed while the computer slept runs once when it wakes. Yours alone; sharing with your team comes later.
- On a Mac, keep a workflow's data and export folders out of Documents, Desktop, Downloads and iCloud Drive (a scheduled run cannot ask for access there), or give `columbus-mcp` Full Disk Access.

**Also new: your assistant can look at the map.** A new tool, `map_snapshot`, draws the session's layers as a picture the assistant sees, so it can spot slivers, gaps, layers that don't line up or features in the wrong place. It is drawn on your computer from geometry only (no attribute values, labels or basemap; nothing is downloaded), and the chat shows you what it saw. Your organization can turn it off.

**Also:** the app's chat streams replies, so long answers at a high thinking level are not cut off; a new README with a tour of the app.

**Smaller fixes:** chat search finds Greek words typed in capitals; the app's own chat is never listed among other AI apps' sessions.

**Also since 0.9.0** (in 0.9.1, if you installed it): the app's chat brings its own Columbus server (so it is never older than the app); choose the model and thinking level for each chat; tables in replies show as tables. If your Columbus folder is somewhere other than your home folder, Settings › Models and accounts shows a **Use …** button for its engine: click it, then **Save settings**.

**Since 0.8.1 (in 0.9.0):** the redesigned Columbus app (one main window: Home, Chats, Documents, Connections, Tools, Workflows, Activity, Settings), and big tables with project databases.

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
