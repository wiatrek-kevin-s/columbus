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
    <img src="docs/images/hero-light.png" width="900" alt="The Columbus app. On the left, a chat: the analyst asks which fatal crashes from 2020 to 2023 happened within 150 feet of IH 35 in central Austin; the assistant reads the crash records and the street centerlines, draws a corridor, keeps the crashes inside it and answers with a table by year. On the right, a street map of central Austin with the IH 35 corridor and the counted crashes highlighted, and each layer marked Checks passed.">
  </picture>
  <br><sub>A question about real public data for Austin, Texas, answered in the Columbus app. Every layer is checked, and the map shows what was counted.</sub>
</p>

> [!WARNING]
> **Columbus is a test release, built mostly by an AI.** Try it on data you can afford to have read, and have your organization review it before using it on production or sensitive data ([disclaimer](#disclaimer)).

## What it does

- **Reaches all your data.** Oracle and SQL Server geodatabases, file geodatabases and shapefiles, ArcGIS Online and Portal layers, CSV and Excel, GeoPackage, PostGIS and rasters. You add each one once, in the app.
- **Keeps your rules, in code.** Every connection is read-only unless your organization allows edits. Row filters and hidden fields apply before the AI sees anything. Exports, edits and scripts wait for your yes, unless you approved them ahead of time (a pre-approved folder, a tool you installed). Every step is logged on your computer.
- **Does real GIS work.** Queries, joins, buffers and overlays, raster analysis, your own Python, and ArcGIS Pro's arcpy (experimental). Every result comes back with checks (row counts, coordinate systems, valid shapes), and jobs you repeat become scheduled workflows.

## See it work

<p align="center">
  <picture>
    <source type="image/webp" srcset="docs/images/demo.webp">
    <img src="docs/images/demo.gif" width="900" alt="A short animation of the Columbus app: the analyst types a question; the assistant reads two layers, which appear on the map; the checks pass and it answers with a table; asked to export, Columbus shows a request with exactly what will be written and where; the analyst allows it once and the export is done.">
  </picture>
  <br><sub>Ask, read, map, check. The export waits until you allow it.</sub>
</p>

## How it works

<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="docs/images/how-it-works-dark.svg">
    <img src="docs/images/how-it-works-light.svg" width="900" alt="Diagram: your AI app (Columbus's own chat, Claude, Copilot or any app that speaks MCP) asks Columbus. Columbus, on your computer, applies the rules, the approvals, and the checks and log. Then it reads your data (Oracle, SQL Server, file geodatabases, shapefiles, ArcGIS Online and Portal, CSV, Excel, GeoPackage, rasters, PostGIS) or runs your Python (the GIS engine, your environments, ArcGIS Pro's arcpy).">
  </picture>
</p>

Your AI app never touches your data directly. It asks Columbus, through MCP (the standard way AI apps call tools), and Columbus decides in its own code what it may read and do. Columbus runs on your computer, with no account and no telemetry. Your data goes only to the AI service you chose, and only the rows your rules allow.

## Get started

1. **Download** the [latest release](https://github.com/wiatrek-kevin-s/columbus/releases/latest): the Columbus app, the Claude extension (`.mcpb`) and the server folder (`.zip`).
2. **Set up the GIS engine** once, from the server folder: `sh engine/setup-engine.sh` on a Mac, `engine\setup-engine.ps1` on Windows.
3. **Connect an AI.** Install the extension in Claude desktop, or add your Anthropic API key in the app under **Settings › Models and accounts**.
4. **Add a connection and ask.** In the app, **Connections › Add connection** (a GeoPackage or a file geodatabase is the quickest start). Then ask, for example: *Check the parcels layer for invalid geometries and show me where they are.*

Step by step: [Windows 10 and 11](docs/install.md) · [Mac](docs/install-macos.md) (Apple Silicon, macOS 15 or later). The app is not code-signed yet, so Windows and macOS warn the first time; the guides say what to do.

## A short tour

<table>
  <tr>
    <td width="50%"><picture><source media="(prefers-color-scheme: dark)" srcset="docs/images/tour-connections-dark.png"><img src="docs/images/tour-connections-light.png" alt="The Connections page listing an Oracle enterprise geodatabase, a file geodatabase, a folder of crash records, a GeoPackage of streets and an ArcGIS Online layer, each read only, with what the assistant may see in plain words."></picture><br><b>Connections.</b> Each source, and in plain words what the AI may see.</td>
    <td width="50%"><picture><source media="(prefers-color-scheme: dark)" srcset="docs/images/tour-approval-dark.png"><img src="docs/images/tour-approval-light.png" alt="An export request in the chat, showing the layer, the folder and the file it will write, with Allow once and Deny buttons."></picture><br><b>Approvals.</b> An export shows exactly what it will write, and where.</td>
  </tr>
  <tr>
    <td><picture><source media="(prefers-color-scheme: dark)" srcset="docs/images/tour-workflow-run-dark.png"><img src="docs/images/tour-workflow-run-light.png" alt="The Workflows page with a weekly crash check and the report of its last run: steps, row counts, checks and the exported files."></picture><br><b>Workflows.</b> A repeated job, run on a schedule, with a report each time.</td>
    <td><picture><source media="(prefers-color-scheme: dark)" srcset="docs/images/tour-checks-dark.png"><img src="docs/images/tour-checks-light.png" alt="The map in front, with a layer's details open: its checks, the datasets it was made from and its provenance record."></picture><br><b>Checks and provenance.</b> Each result, what it was made from, and how.</td>
  </tr>
</table>

More pages in the [app tour](docs/app-tour.md). Every picture is taken from the app by a script ([`screenshots.py`](app/desktop/e2e/screenshots.py)) on [public data](docs/images/DATA-CREDITS.md).

## What's in 0.14

- **Folders of files:** CSV, Excel, GeoJSON, GeoParquet, KML and FlatGeobuf, with the same rules as a GeoPackage.
- **ArcGIS Online and Portal layers**, public or signed in with your organization's own ArcGIS app. The AI never sees the sign-in.
- **Your own Python environments** for scripts, sandboxed by default.
- **arcpy in trusted environments** (*experimental*: not yet checked on a real ArcGIS Pro computer).
- **File geodatabase and GeoTIFF exports**, and measures (M values) kept end to end for linear referencing.
- **Web map layers** (imagery and other services) on your map, for display only.
- **Reviewing results:** open a workflow run in a chat, zoom to a row, measure on the map.

Files and upgrade steps: [release notes](https://github.com/wiatrek-kevin-s/columbus/releases) · Everything Columbus does: [capabilities](docs/capabilities.md).

## Learn more

- [Technical reference](docs/reference.md): every rule, and how it is enforced.
- [How Columbus is built](docs/architecture.md) and [what it can do](docs/capabilities.md).
- [Install on Windows](docs/install.md) · [Install on a Mac](docs/install-macos.md) · [Optional manual checks](docs/manual-checks.md).
- [GIS skills](skills/README.md) for assistants · [example settings](examples).
- [Security policy](SECURITY.md) · [Contributing](CONTRIBUTING.md).

## Disclaimer

Columbus is built mostly by Claude, an AI model made by Anthropic, directed by its owner, a GIS analyst rather than a software engineer. Its security rests on automated tests on macOS, Windows and Linux and on AI reviews of each change, not on a professional human audit. It is offered as test releases, as is, without warranty. Don't rely on it to protect sensitive data, or use it on production systems, until your organization has reviewed it. AI assistants make mistakes: check their results, which is what Columbus's checks, approvals and provenance are for.

## License

[Apache License 2.0](LICENSE). The sample data in the pictures is public US government data ([credits](docs/images/DATA-CREDITS.md)).
