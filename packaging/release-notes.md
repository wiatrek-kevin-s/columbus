Columbus {{VERSION}}, a test release. Click a link to download that file.

**Fixed in this version: saving a script as a tool works in Claude.** In 0.8.0, Claude desktop sent one part of a tool proposal (its test run) as text, and Columbus refused every proposal. Columbus now describes that part so every assistant sends it correctly, and accepts it either way. Columbus also now hands assistants its own guides (including how to design a tool), so they no longer need the skills folder installed. And the app has a new compass icon.

**New in 0.8.0: your own tools.** Ask your assistant to "save the script we just made as a tool" and it packages it as one of your own Columbus tools, with a clear description, typed inputs, and the units and checks other assistants need to use it well. You approve one test run (you see the full code), then install it in the Columbus app under **Settings → Tools** with Touch ID or Windows Hello. After that, any assistant can find it and run it on your layers without asking each time. Switch on **Allow personal tools** in Settings first (scripts must be allowed too). Every run is sandboxed: no network, it reads only the layers it's given and writes only its own results, and settings passed in later (like a field name) can never become code.

## Mac (Apple Silicon: M1 or later)

**Already set up Columbus before? Update with these two files:**

1. **Claude extension:** [columbus-{{VERSION}}-macos-arm64.mcpb]({{BASE}}/columbus-{{VERSION}}-macos-arm64.mcpb). Double-click it in your Downloads folder. Claude desktop asks to install or update the extension: accept, check that **Engine Python** still says `/Users/<you>/Columbus/engine/venv/bin/python3`, then quit Claude with **Cmd+Q** and open it again.
2. **Columbus app (optional):** [columbus-app-{{VERSION}}-macos-arm64.dmg]({{BASE}}/columbus-app-{{VERSION}}-macos-arm64.dmg). Double-click it and drag **Columbus** onto **Applications**. Then, once, in Terminal: `xattr -dr com.apple.quarantine /Applications/Columbus.app`

**New to Columbus?** Also download the server folder, [columbus-{{VERSION}}-macos-arm64.zip]({{BASE}}/columbus-{{VERSION}}-macos-arm64.zip), and follow the [Mac install guide](https://github.com/wiatrek-kevin-s/columbus/blob/main/docs/install-macos.md) from step 1. You only need the folder once, to set up the GIS engine.

## Windows (10 and 11, 64-bit)

**Already set up Columbus before? Update with these two files:**

1. **Claude extension:** [columbus-{{VERSION}}-windows-x64.mcpb]({{BASE}}/columbus-{{VERSION}}-windows-x64.mcpb). Double-click it, accept the update in Claude desktop, check **Engine Python**, then quit Claude completely (not just its window) and open it again.
2. **Columbus app (optional):** [columbus-app-{{VERSION}}-windows-x64-setup.exe]({{BASE}}/columbus-app-{{VERSION}}-windows-x64-setup.exe). Run it; it installs for you alone, no administrator needed. Windows SmartScreen warns because it is not signed yet: **More info**, then **Run anyway**. For IT: [the .msi]({{BASE}}/columbus-app-{{VERSION}}-windows-x64.msi).

**New to Columbus?** Also download the server folder, [columbus-{{VERSION}}-windows-x64.zip]({{BASE}}/columbus-{{VERSION}}-windows-x64.zip), and follow the [Windows install guide](https://github.com/wiatrek-kevin-s/columbus/blob/main/docs/install.md) from step 1.

## What the files are

| File | What it is |
| --- | --- |
| `.mcpb` | The Columbus server as a Claude desktop extension: Claude starts it for you |
| `columbus-app-…` | The Columbus app: each running session's layers on a map, its tables, checks, and the requests waiting for your decision; and a chat with Claude using your own Anthropic API key |
| `.zip` | The server as a folder, with the GIS engine setup script, examples, skills and guides |

Nothing is code-signed yet, which is why macOS and Windows warn; the steps above say what to do.
