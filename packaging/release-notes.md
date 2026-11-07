Columbus {{VERSION}}, a test release. Click a link to download that file.

**New in this version:**
- **Projects:** a project has its own instructions for your assistant, the connections its chats may use, a default export folder, and data groups. Start a chat in a project and Columbus's server keeps it to the project's connections. The Projects page lists each project's chats and its project databases. The Chats page groups chats by project.
- **Data groups:** a named set of datasets across your connections, each with an optional filter on which rows count (for example, the roadway inventory, and the centerline without retired segments). Attach groups to a project and its chats can read only those datasets and only those rows. Your connection's own row filter always applies too. Your organization's groups, from its context pack, show read only.
- **Registry page:** search open data sources and GIS tools by words, kind and theme, and open an entry for its publisher, coverage, licence and how to get it. **Ask my assistant about it** starts a chat about it. If the registry index isn't installed yet, the page installs it.
- **Fewer password prompts on a Mac:** the app reads each item it keeps in your Keychain once per launch, not every time a page asks. When macOS asks, choose **Always Allow** so it stops asking until the next update.

**Safety fixes** (found by three rounds of independent security review):
- A row filter now only ever applies to its own table's columns. A filter that names a column the table doesn't have is refused instead of matching something else in the query.
- Free SQL can't use the names Columbus gives its own parts of a query.
- For filtered datasets, the table's stored extent and row estimate are no longer shown, since they count rows you can't read.
- Edits on PostgreSQL work with a `%` in a row filter.

**In 0.10.1:** a clear key-saved state, New chat always new, the workflow approvals badge, and the tidier Workflows page.

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
