Columbus {{VERSION}}, a test release. Click a link to download that file.

**New in this version:**
- **Documents:**
  - **Import** a Word, Excel, PDF, CSV, Markdown or text file as a draft note (an SOP, a reference note or a glossary). You preview exactly what your assistant will see first; the original is kept beside the note.
  - **Proposals:** notes whose database changed since they were written, notes due for review, and changes your assistant **suggested** for you to **Accept** or **Dismiss** when you get to them (it can suggest changes to notes you've reviewed, which it can't change itself).
- **Share tools and workflows with coworkers:**
  - **Share…** on an installed tool, or on an approved workflow's page, saves a `.columbus` file, **signed with your own sharing key**. A workflow's file brings the tools it uses; its schedule, runs, data, connection details, passwords and email recipients never travel.
  - Your coworker opens it with **Open a shared file…**, sees who signed it (by the key's fingerprint, until they trust your key under your name), chooses their own connections, and approves each part with their own Touch ID or Windows Hello. Nothing is installed or run by opening the file.
  - **Team library** (Tools page): pick a folder your team shares; Columbus lists the shared files in it and whether each is new, the same as yours, or an update.
  - **Tools for everyone:** IT can install a shared tool for everyone on a computer (`columbus-mcp tools org-install FILE`).
  - Off by default: turn on **Allow shared tools and workflows** on the Tools page.
- **Map snapshots can label features** with a field you or your assistant choose.
- **Workflows can email their report**, once your organization sets up a mail server in the policy (Workflows page, Email).
- **Workflows list every connection they read,** including ones named inside a step's inputs.

**After updating:** run the engine setup once more (Mac: `sh engine/setup-engine.sh` in your Columbus folder; Windows: `engine\setup-engine.ps1`). It adds what PDF import and map labels need.

**In 0.12.0:** Esri shapefiles and file geodatabases.

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
