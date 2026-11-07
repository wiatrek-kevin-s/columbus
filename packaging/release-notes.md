Columbus {{VERSION}}, a test release. Click a link to download that file.

**What's fixed in this version** (from testing 0.10.0):
- **API key:** Settings now shows a clear **✓ Key saved** with the last four characters of your key, and a message when you save or remove it.
- **New chat** on the Chats page and on Home always starts a new chat. Before, it reopened the chat you already had open.
- **Workflows:** the **Workflows** button shows a count when a workflow is waiting for your approval. The page is tidier: one row per workflow with its last run, each workflow's own page with its runs (newest first, 20 at a time), and each run's report on its own page. **Run now** takes you straight to the report.
- **AI review in workflows** now streams its answer, so a long review at a high thinking level isn't cut off by a network proxy.
- **Run reports** say what was already exported when a run is stopped at its time limit.

**Safety fixes** (found by an independent security review):
- **Windows:** every file Columbus opens or creates in a folder that a connected server, plugin or script can change is checked to be exactly where it was asked for. A folder swapped for a shortcut (a junction) is refused, as are hard links. This works on substituted drives and network shares too.
- **Project databases** are created by walking from your workspace folder, never from a folder another program could change.

**In 0.10.0:** workflows (scheduled jobs you approve, with an optional AI review), map snapshots your assistant can look at, streamed chat replies, and the new README.

**Since 0.9.0** (in 0.9.1, if you installed it): the app's chat brings its own Columbus server; choose the model and thinking level for each chat; tables in replies show as tables.

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
