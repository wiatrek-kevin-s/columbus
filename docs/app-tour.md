# The Columbus app, page by page

What each page of the Columbus desktop app (0.14.0) is for. For how it works underneath, see the [technical reference](reference.md#companion-window-and-desktop-app); to install it, the [Mac](install-macos.md) or [Windows](install.md) guide.

The Columbus app is one window with a menu down the side.

| Page | What it is for |
| --- | --- |
| **Home** | What needs you right now: requests waiting for a decision, notes in draft, tools and workflows to review, recent chats. The first time, a short setup checklist (connect an AI, set up the GIS engine, add a connection). |
| **Chats** | Your conversations in the app, saved and searchable (**Ctrl+K** / **⌘K**), and the sessions other AI apps (Claude desktop) run through Columbus, with what they did and their map. A chat opens with the map and table beside it; choose its model and thinking level below the message box. |
| **Projects** | A piece of work with its own instructions for the assistant, the connections its chats may use, a default export folder and its project databases. |
| **Documents** | Notes Columbus drafted from your databases (fields, codes, relationships), which you fill in and **Mark reviewed**, and your organization's context pack, read only. |
| **Data groups** | Named sets of datasets across connections, each with an optional row filter; attached to a project, its chats see only those rows. |
| **Skills** | The GIS skills your assistant reads: when to use which tool, and how. Skills are advice; they never grant access. |
| **Connections** | Add, test and edit GeoPackages, SQLite files, shapefiles, file geodatabases, raster folders, folders of files (CSV, Excel, GeoJSON, GeoParquet, KML, FlatGeobuf), ArcGIS Online and Portal services (with **Sign in**) and databases (PostGIS, Oracle, SQL Server), each with its mode (read-only or read-write), allowed datasets, row filter and limits. Anything that gives the assistant more access asks for Touch ID or Windows Hello. **Draft notes** writes a note about a connection's datasets for you to review. **Map layers** adds imagery and other web services to your map (display only; the assistant never sees them). |
| **Registry** | The open geospatial registry: open data by place and theme, and GIS tools, searched through Columbus. |
| **Tools** | Tools your assistant made from its scripts. You read each one, code and all, and install it; installed tools run without asking, in the same sandbox as scripts. |
| **Workflows** | Jobs that run by themselves on a schedule: proposed by your assistant, approved by you, with **Run now**, **Pause**, and every run's report. Optionally an AI review of the results against your criteria. |
| **Activity** | The **approvals inbox** across every session, the **audit log** (filter by app, tool and decision; verify the chain), and **exports**. |
| **Settings** | The AI for chats (API key, model, thinking level), the Columbus server and GIS engine in use, your Python environments, theme, privacy, keyboard, and your organization's settings in plain sentences. |

## From the command line

The same server runs without the app. A few commands people use most:

| Command | What it does |
| --- | --- |
| `columbus-mcp --version` | Prints the server's version |
| `columbus-mcp secret set <id>` | Stores a connection's database password in the operating system's credential store |
| `columbus-mcp arcgis signin <id>` | Signs in to an ArcGIS connection (`--paste-code` to paste a code instead); `arcgis signout <id>` removes it |
| `columbus-mcp env add <id> --python <path>` | Registers a Python environment (`--trust trusted` for a trusted one, experimental); `env list`, `env confirm <id>`, `env remove <id>` |
| `columbus-mcp env proposals`, `env approve <id>` | Environments your assistant asked for, and approving one |
| `columbus-mcp workflow list`, `workflow run <id>` | Your workflows, and running one now |
| `columbus-mcp workflow open <id> <run>` | Opens a finished run's results in a new session for another AI app |
| `columbus-mcp map-source list` | Your web map layers |
| `columbus-mcp audit verify <file>` | Checks an audit log's hash chain |

Every command is in the [technical reference](reference.md).
