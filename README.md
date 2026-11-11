<p align="center">
  <img src="docs/images/logo.svg" alt="Columbus logo" width="88">
</p>

<h1 align="center">Columbus</h1>

<p align="center">
  <b>Let AI work on your organization's GIS data, under your rules, on your computer.</b>
</p>

<p align="center">
  <a href="https://github.com/wiatrek-kevin-s/columbus/actions/workflows/ci.yml"><img alt="CI status" src="https://github.com/wiatrek-kevin-s/columbus/actions/workflows/ci.yml/badge.svg"></a>
  <a href="https://github.com/wiatrek-kevin-s/columbus/releases"><img alt="Latest release" src="https://img.shields.io/github/v/release/wiatrek-kevin-s/columbus?include_prereleases&label=release"></a>
  <img alt="Platforms: macOS, Windows, Linux server" src="https://img.shields.io/badge/platforms-macOS%20%7C%20Windows%20%7C%20Linux%20server-blue">
  <a href="LICENSE"><img alt="License: Apache 2.0" src="https://img.shields.io/badge/license-Apache--2.0-green"></a>
</p>

<p align="center">
  <a href="https://github.com/wiatrek-kevin-s/columbus/releases/latest"><b>Download</b></a> ·
  <a href="#get-started"><b>Get started</b></a> ·
  <a href="docs/reference.md"><b>Docs</b></a>
</p>

<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="docs/images/hero-dark.png">
    <img src="docs/images/hero-light.png" width="900" alt="The Columbus app, in three parts. On the left, a sidebar: New chat, Search, the places (Chats, Projects, Data, Documentation, Workflows, Tools, Activity), the recent chats and Settings. In the middle, the chat about which fatal crashes from 2020 to 2023 happened within 150 feet of IH 35 in central Austin. The answer reads: 36 of the 122 fatal crashes were within 150 feet of IH 35, taking 39 lives, with a table of crashes and deaths by year. Under the message box, the chat's project, its AI service, its model and its thinking level. On the right, a panel named Map and table: the fatal crashes of central Austin as points, with the IH 35 corridor and the 36 counted crashes highlighted, and a legend of the layers the assistant made, the active one marked Checks passed.">
  </picture>
  <br><sub>A real question about public Austin crash data, answered in the app. Each layer is checked, and the map shows what was counted.</sub>
</p>

> [!WARNING]
> **Columbus is a test release, built mostly by an AI.** Try it on data you can afford to have read, and have your organization review it before using it on production or sensitive data ([disclaimer](#disclaimer)).

## What it does

- **Reaches all your data.** Oracle and SQL Server geodatabases, file geodatabases and shapefiles, ArcGIS Online and Portal layers, CSV and Excel, GeoPackage, PostGIS and rasters. You add each one once, in the app.
- **Keeps one boundary, in code.** Your connections and passwords stay with Columbus. It reads for the AI under each source's rules (read-only unless your organization allows edits; row filters and hidden fields applied first), and what is read lands as a copy on your computer. On copies the AI works freely: once you choose **Work freely on copies**, its scripts and tools run in a sandbox with no network, without asking each time. Columbus still asks before an export to a folder that isn't pre-approved, before any edit to a source, and before code that needs the network or must leave the sandbox. Every step is logged on your computer.
- **Does real GIS work.** Queries, joins, buffers and overlays, raster analysis, your own Python and ArcGIS Pro's arcpy (experimental). Every result is checked (row counts, coordinate systems, valid shapes), and repeated jobs become scheduled workflows.
- **Uses tools you install, not ones you build.** Add GeoLibre from the Tools page: about a thousand open-source terrain, hydrology, raster and vector tools. The AI finds the right one and runs it on copies, in the same sandbox. Any other command-line program can be added with a small description file.
- **Works with the AI you choose.** The app's own chat uses Anthropic, OpenAI, xAI, Google Gemini or OpenRouter with your own key, a model on your own computer (Ollama, LM Studio), or another service you add by its address. Or keep the AI app you already use: Claude desktop, GitHub Copilot, Cursor and others can use Columbus too. The rules are the same whichever you use.

## See it work

<p align="center">
  <picture>
    <source type="image/webp" srcset="docs/images/demo.webp">
    <img src="docs/images/demo.gif" width="900" alt="A short animation of the Columbus app. It opens on the finished answer: a table of fatal crashes by year in the chat, and beside it, in the panel on the right, a map of the IH 35 corridor with the counted crashes highlighted. Then it replays how it got there: the analyst types the question; the assistant reads two layers, which appear on the map as the panel opens; the checks pass and it answers with a table; asked to export, Columbus shows a request with exactly what will be written and where; the analyst allows it once and the export is done.">
  </picture>
  <br><sub>Ask, read, map, check. The export waits until you allow it.</sub>
</p>

## How it works

<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="docs/images/how-it-works-dark.svg">
    <img src="docs/images/how-it-works-light.svg" width="900" alt="Diagram: your AI app (Columbus's own chat, Claude, Copilot or another AI app) asks Columbus. Columbus, on your computer, applies the rules, the approvals (exports, edits and anything leaving the sandbox wait for your yes), and the checks and log. It reads your data (Oracle, SQL Server, file geodatabases, shapefiles, ArcGIS Online and Portal, CSV, Excel, GeoPackage, rasters, PostGIS). Copies of what was read go to the sandbox, where Columbus runs scripts in your Python and installed tools: no network, no passwords.">
  </picture>
</p>

Your AI app never touches your data. It asks Columbus for each step, and Columbus decides in its own code what may be read or done. What is read lands as a copy, and scripts and installed tools get only copies, inside your operating system's sandbox. Columbus runs on your computer, with no account and no tracking. Your data goes only to the AI service you chose, and only the rows your rules allow. (AI apps reach Columbus through MCP, their standard way to use tools; the [technical reference](docs/reference.md) has the details.)

## Get started

**You need:** Windows 10 or 11, or a Mac with Apple Silicon and macOS 15 or later; Python 3.12 (the setup script installs the rest); an AI of your choice (a key for an AI service, a model on your own computer, or an AI app such as Claude desktop or GitHub Copilot); ArcGIS Pro only if you want arcpy.

1. **Download** the [latest release](https://github.com/wiatrek-kevin-s/columbus/releases/latest): the Columbus app, the Claude desktop extension and the server folder.
2. **Set up Columbus's Python** once, from the server folder: `sh engine/setup-engine.sh` on a Mac, `engine\setup-engine.ps1` on Windows. It installs the GIS tools Columbus uses.
3. **Connect an AI.** In the app, choose your AI service under **Settings › Assistant** and save its key (a model on your own computer, through Ollama, needs none). Or use an AI app you already have: install the extension in Claude desktop, or copy the entry for GitHub Copilot, Cursor and others from that same page.
4. **Add your data and ask.** In the app, **Data › Add data** (a GeoPackage or a file geodatabase is the quickest start). Then ask, for example: *Check the parcels layer for invalid geometries and show me where they are.*
5. **Choose how the AI works.** On the first-run screen or in **Settings**: **Work freely on copies** (recommended) or **Ask me before each script**. On your own computer you create Columbus's settings file once first; the guides show how.

Step by step: [Windows](docs/install.md) · [Mac](docs/install-macos.md). The app is not code-signed yet, so Windows and macOS warn the first time; the guides say what to do.

## A short tour

<table>
  <tr>
    <td width="50%"><picture><source media="(prefers-color-scheme: dark)" srcset="docs/images/tour-connections-dark.png"><img src="docs/images/tour-connections-light.png" alt="The Data page, with Data chosen in the sidebar on the left: what the assistant can reach, one row each for an Oracle enterprise geodatabase, a file geodatabase, a folder of crash records, a GeoPackage of streets and an ArcGIS Online layer, each read only with the rule that matters; under them, where results can go."></picture><br><b>Data.</b> What the AI may reach, in plain words.</td>
    <td width="50%"><picture><source media="(prefers-color-scheme: dark)" srcset="docs/images/tour-approval-dark.png"><img src="docs/images/tour-approval-light.png" alt="A request in the chat that says Columbus needs your OK: an export, with the layer, the folder and the file it will write, and Allow once and Decline buttons."></picture><br><b>Approvals.</b> What an export will write, and where.</td>
  </tr>
  <tr>
    <td><picture><source media="(prefers-color-scheme: dark)" srcset="docs/images/tour-workflow-run-dark.png"><img src="docs/images/tour-workflow-run-light.png" alt="Under Workflows, the report of a weekly crash check's last run, on its own page: steps, row counts, checks and the exported files."></picture><br><b>Workflows.</b> Repeated jobs, with a report each run.</td>
    <td><picture><source media="(prefers-color-scheme: dark)" srcset="docs/images/tour-checks-dark.png"><img src="docs/images/tour-checks-light.png" alt="The chat with a result's How it was made open in the panel on the right: its checks, the datasets it was made from and its full record."></picture><br><b>How it was made.</b> Each result's checks, and what it was made from.</td>
  </tr>
  <tr>
    <td><picture><source media="(prefers-color-scheme: dark)" srcset="docs/images/tour-toolboxes-dark.png"><img src="docs/images/tour-toolboxes-light.png" alt="The Add tools dialog, over the Tools page. GeoLibre tools: about a thousand open-source terrain, hydrology, raster and vector tools in one program, with its version, the size of the download, its licence, its publisher and the address it is downloaded from, a line saying Columbus checks the download against a pinned hash, and an Install button. Under it, a form to add a program that is already on this computer, with its description file."></picture><br><b>Toolboxes.</b> Tools you install instead of building.</td>
    <td><picture><source media="(prefers-color-scheme: dark)" srcset="docs/images/tour-project-dark.png"><img src="docs/images/tour-project-light.png" alt="The page of a project named IH 35 safety review, with a button for a new chat in the project. Its Sync section says the project's files can sync with a repository on GitHub or the organization's own Git server, that nothing is sent until Sync is pressed, and that no Git server is connected yet. Under it, the project's chat, and its three files in two folders, a note and two scripts, with a button that opens the folder."></picture><br><b>Projects.</b> Files that can sync with GitHub or Gitea.</td>
  </tr>
</table>

More pages in the [app tour](docs/app-tour.md). Every picture is taken from the app by a script ([`screenshots.py`](app/desktop/e2e/screenshots.py)) on [public data](docs/images/DATA-CREDITS.md).

## What's in 0.15

- **A window like the AI apps you know:** a sidebar, the chat, and a panel on the right with the map, the table and how each result was made.
- **Work freely on copies:** one choice replaces approving each script. Exports to folders you haven't approved, edits, and code that leaves the sandbox still ask.
- **Toolboxes:** install GeoLibre, about a thousand open-source tools, from the Tools page; or add any command-line program with a small description file.
- **The map, both ways:** the AI can read what your map shows, and point it at a result.
- **Projects with files, synced with GitHub or Gitea:** each project has a folder for scripts and notes, which the AI can write and run from. Link it to a repository and press **Sync**; no Git is needed on your PC.
- **Any AI in the app's chat:** choose Anthropic, OpenAI, xAI, Google Gemini, OpenRouter or a model on your own computer (Ollama, LM Studio), or add another service by its address. The app also shows how to connect other AI apps to Columbus, with the entry to copy.

Said plainly: the GeoLibre program itself was never run while this was built, so your first install is its first real run. Sync was tested against stand-in servers, never a real GitHub or Gitea. In the app's own chat, no AI service was contacted while this was built: every one was tested against stand-ins. OpenAI, xAI, Gemini and the other services new in 0.15 were built from each one's published description, so your first chat with one of them is its first meeting with the real thing. arcpy is still *experimental*, not yet tried on a real ArcGIS Pro computer; it has to run outside the sandbox, where code could reach your connections, so Columbus asks before every such run. And 0.15 may not be published yet: if the newest version on the [releases page](https://github.com/wiatrek-kevin-s/columbus/releases) is still 0.14, it has none of the above. What changes for people coming from 0.14 is in the [release notes](packaging/release-notes.md).

## Learn more

[Technical reference](docs/reference.md) (every rule, and how it is enforced) · [What it can do](docs/capabilities.md) · [How it is built](docs/architecture.md) · [Optional manual checks](docs/manual-checks.md) · [GIS skills](skills/README.md) · [Example settings](examples) · [Security policy](SECURITY.md) · [Contributing](CONTRIBUTING.md)

## Disclaimer

Columbus is built mostly by Claude, an AI model made by Anthropic, for a GIS analyst who directs the work. Each change is checked by automated tests on macOS, Windows and Linux and by AI reviews; no professional has audited it. It comes as is, without warranty. Review it before any production use, and check what the AI produces.

## License

[Apache License 2.0](LICENSE). The sample data in the pictures is public US government data ([credits](docs/images/DATA-CREDITS.md)).
