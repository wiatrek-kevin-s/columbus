Columbus {{VERSION}}, a test release. Click a link to download that file.

## What's new in 0.15.0

Columbus now looks like the AI apps you know, lets your assistant work freely on copies of your data, and takes tools you install in place of tools someone has to build. No rule loosens by itself: every new switch is off until you choose it.

- **A new window.** A sidebar (New chat, Search, your chats, Projects, Data, Documentation, Workflows, Tools, Activity, Settings), the chat in the middle, and a panel on the right that opens when your assistant makes something: the map and its table, the chat's files, how a result was made. The panel resizes, can fill the window, and closes.
- **One boundary, in place of approving each script.** Your connections and passwords stay with Columbus. Reading goes through Columbus, under each source's rules, and lands as a copy. On copies your assistant works freely: choose **Work freely on copies** on the first-run screen or in **Settings**, and scripts and installed tools run without asking, in your operating system's sandbox (no network, only their inputs and their own run folder). Columbus still asks before an export to a folder that isn't pre-approved, a download from a host not on your list, any edit to a source, and code that needs the network or must run outside the sandbox (trusted environments, arcpy). The other choice is **Ask me before each script**.
- **Toolboxes: install tools, don't build them.** **Tools › Add tools** installs GeoLibre: about a thousand open-source terrain, hydrology, raster and vector tools (MIT licence, version 1.8.1, a download of 16 to 19 MB). Columbus never ships it: it is downloaded when you ask, and checked against a SHA-256 pinned in Columbus. Your assistant finds, reads and runs any of its tools, each run in the same sandbox as scripts, on copies, with no approval. Any other command-line program can be added with a small description file, from a folder of its own: its tools can read that folder, and the app says which before you confirm. From a terminal: `columbus-mcp toolbox install geolibre`.
- **Your assistant and your map.** It can read what your map shows (the area, the layers that are on, the feature you selected), point the map at a result, and take its snapshot of that same view.
- **Project files.** Each project has a folder for scripts and notes. Your assistant can read and write text files there and run a script from there; you can put your own in. It writes scripts and notes, not data (results leave a chat only through an export), and Columbus does not hand it a file that holds a password or a key. The project's page lists them.
- **Sync with GitHub and Gitea.** **Settings › Git servers** connects github.com, a GitHub Enterprise Server, a Gitea or a Forgejo with a token, kept in your computer's credential store. On a project's page: **Link this project**, **Check for changes**, **Sync**. It works through the server's web API, so no Git is needed on your PC and nothing from a repository can run on it. A file changed on both sides is kept twice (keep mine, keep theirs, keep both). The page lists what will go and what will not before anything does. Data files, password and key files, archives, programs and files over 5 MB stay on your computer; text that looks like a password or a token stops the sync; and a file your assistant wrote after reading one of your sources is held back until you choose **Send it anyway** for it.

**Needs macOS 15 or later on a Mac** (Apple Silicon). Windows 10 and 11 (64-bit) as before.

### What changes if you come from 0.14

- **Nothing loosens by itself.** The new policy keys (`scripts.approval`, `allow_toolboxes`, `allow_project_files`, `allow_sync`) are off until you choose **Work freely on copies** in the app, or your organization writes them in `policy.yaml`. Until then scripts are as in 0.14: off, or approved one by one. `scripts.session_approval: true` keeps its meaning.
- **Choosing is yours, once.** **Work freely on copies** asks for Touch ID or Windows Hello and writes seven settings to `policy.yaml`: `allow_scripts`, `scripts.approval: none`, `environments.allow`, `allow_personal_tools`, `allow_toolboxes`, `allow_project_files` and `allow_sync`. **Ask me before each script** sets `scripts.approval` back to `each` and leaves the others as they are; scripts, personal tools, toolboxes, project files and sync each have a switch of their own under **Settings › Organization rules**.
- **The app does not create `policy.yaml`.** On a computer without one, the app says to create it first; the [Windows](https://github.com/wiatrek-kevin-s/columbus/blob/main/docs/install.md) and [Mac](https://github.com/wiatrek-kevin-s/columbus/blob/main/docs/install-macos.md) guides show how (step 3). Where your organization manages the file, the app shows what is in force and offers no choice.
- **The window is new; your things are where they were:** chats, connections, notes, workflows and tools. The app opens on a question, not a Home page. The earlier session list and settings window are still under **Classic windows**, at the foot of **Settings**.

### Not in this release

- QGIS as a toolbox, and toolbox tools as steps of a workflow.
- In sync: GitHub's device sign-in (a token works), merging line by line, more than the one linked branch, and syncing by itself. A synced folder is not a Git clone.
- Sync on Linux: no server can be connected there yet. GeoLibre on Windows on ARM: its publisher builds none.
- Still open from 0.14: a personal tool can't import an environment's other packages beyond the tool rules' module list (CR-1), so arcpy works in scripts you approve, not in personal tools or workflows; and a documentation website (CR-3).

### What we could and couldn't confirm

- **The real GeoLibre program was never run while this was built** (the build machine may not run downloaded programs). Columbus's side follows the program's source at version 1.8.1 and is tested against a stand-in. Your first install is its first real run (H-3).
- **Sync has never met a real GitHub or Gitea.** It was built and tested against stand-in servers written from their published API descriptions. Try it first on a repository that does not matter (H-4).
- **arcpy and trusted environments are still experimental,** as in 0.14: not yet tried on a real ArcGIS Pro computer (H-2). They run outside the sandbox, where code could reach your connections, so Columbus asks before every such run.
- **The app's pages are driven by tests on Linux only.** On Windows and macOS the tests build the app and run the server's own tests. Two short looks on Windows and on a Mac are in the manual checks.
- **ArcGIS sign-in route (A-2): still unconfirmed** (H-1). Register both redirect URLs, as in 0.14.

### Optional checks you can do (not needed to use this release)

- **H-1:** register your organization's ArcGIS app and try a signed-in layer.
- **H-2:** run one arcpy script on a Windows PC with ArcGIS Pro.
- **H-3:** install GeoLibre and have your assistant run one of its tools.
- **H-4:** sync a project with your GitHub or your organization's Gitea.
- **Two short looks on Windows and on a Mac:** the map panel, and the choice with Windows Hello or Touch ID.

All are step by step in [docs/manual-checks.md](https://github.com/wiatrek-kevin-s/columbus/blob/main/docs/manual-checks.md).

### Upgrading from 0.14

1. Install the new extension and app (below).
2. The engine's packages did not change, so the engine setup need not be run again. (From 0.13 or earlier, run it once more, from the new server folder: `sh engine/setup-engine.sh` on a Mac, `engine\setup-engine.ps1` on Windows.)
3. Nothing else to change. To use what is new, open **Settings** in the app and choose how your assistant works.

Licences of software Columbus carries are in `THIRD-PARTY-NOTICES.md` in the server folder. GeoLibre is not part of Columbus; its own licence and notices are kept beside it when you install it.

**In 0.14.0:** folders of files and ArcGIS Online and Portal layers as connections, your own Python environments, file geodatabase and GeoTIFF exports, measures kept end to end, web map layers. **In 0.13.0:** importing documents as notes, sharing tools and workflows with coworkers, labelled map snapshots, workflow email.

## Mac (Apple Silicon, macOS 15 or later)

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
| `columbus-app-…` | The Columbus app: a chat with Claude using your own Anthropic API key, with the map, tables and checks beside it; your data, projects, workflows and tools; and the sessions other AI apps run, with the requests waiting for your decision |
| `.zip` | The server as a folder, with the GIS engine setup script, examples, skills and guides |

Nothing is code-signed yet, which is why macOS and Windows warn; the steps above say what to do.
