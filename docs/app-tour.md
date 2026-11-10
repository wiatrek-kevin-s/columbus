# The Columbus app, page by page

What each page of the Columbus desktop app (0.14.0) is for. For how it works underneath, see the [technical reference](reference.md#companion-window-and-desktop-app); to install it, the [Mac](install-macos.md) or [Windows](install.md) guide.

The Columbus app is one window. The map fills it, and one panel floats on the map, at the left. Everything else is in that panel.

The app opens on a question: a new chat, its panel in the middle of the window (*What do you want to know?*), with things to try, your recent chats, and a line when something waits for you. Once you ask, the panel sits at the left and the chat's map shows behind it. The first time, in place of the question, it says **Start with your data** (**Add data**), with two checks under it: connect an AI for the chat (an Anthropic API key), and the GIS engine. Each can be skipped, and **Skip setup** skips them all.

The panel's top bar has four places: **Chats**, **Data**, **Workflows** and **Activity** (which shows a count when something waits for you). Beside them are **New chat** (+), **Search** (**Ctrl+K**, or **⌘K** on a Mac: chats by title and content, sessions, every page, and your connections, workflows and projects by name) and **Settings**.

| Where | What it is for |
| --- | --- |
| **Chats** | The list of your chats, saved on this computer: a search box, **New chat**, and the chats grouped by day (Today, Yesterday, This week, Earlier). Sessions that other AI apps (Claude desktop) run through Columbus are in the same list, and open to show what they did and their map. The footer chooses which project's chats show, and links to **Projects**. |
| **A chat** | The conversation in the panel, with the chat's map behind it. The assistant's steps fold into one line ("3 steps · …") that opens to show each step. The last result of a turn is a card with **Table** (opens that result's rows on the map) and **How it was made** (its checks, inputs and files, in a sheet beside the panel). A request for your approval is a card in the chat (**Columbus needs your OK**), with **Allow once**, **Allow for this chat** where Columbus offers it, and **Decline**. Under the message box: the chat's project (you can choose it until the first message), the model and the thinking level. The **⋯** menu has **Files this chat made**, **Show only the map** (a small pill brings the panel back, and the panel comes back by itself when Columbus needs your OK) and **Open the map in its own window**. |
| **Chats › Projects** | A piece of work with its own instructions for the assistant, the connections its chats may use, a default export folder and its project databases. |
| **Data** | What your assistant can reach, one row per connection: what it is, read only or read and write, and the rule that matters. Under it, where results can go (your organization's export folders). **Add data** opens the connection editor for GeoPackages, SQLite files, shapefiles, file geodatabases, raster folders, folders of files (CSV, Excel, GeoJSON, GeoParquet, KML, FlatGeobuf), ArcGIS Online and Portal services, and databases (PostGIS, Oracle, SQL Server), with its allowed datasets and row filter; **Test** it before you **Save**. Anything that gives the assistant more access asks for Touch ID or Windows Hello. |
| **A connection's page** | Click a connection's row under **Data**. Its rules in plain words ("Your assistant can read it, and change nothing."): read only or read and write, which datasets, which rows, sign-in. **Change the rules** edits them, **Test** opens it as Columbus would, **Draft notes** writes a note about its datasets for you to review, and **Sign in** signs in to ArcGIS. Its datasets with notes are listed there. |
| **Data › Notes** | The line at the bottom of **Data** opens it. **My notes**: notes Columbus drafted from your databases (fields, codes, relationships), which you fill in and **Mark reviewed**. **Organization's pack**: your organization's context pack, read only. **Proposals**: notes that no longer match their database or are due for review, and changes your assistant suggested. **Import**: a document becomes a draft note. |
| **Data › Datasets** | The datasets Columbus knows for each connection, from your notes. |
| **Data › Data groups** | Named sets of datasets across connections, each with an optional row filter; attached to a project, its chats see only those rows. |
| **Data › Map layers** | Imagery and other web services for your map (display only; the assistant never sees them). |
| **Data › Project databases** | The project database files in your workspaces folder. |
| **Data › Registry** | The open geospatial registry: open data by place and theme, and GIS tools, searched through Columbus. |
| **Workflows** | Jobs that run by themselves on a schedule: proposed by your assistant, approved by you. The page lists what can't run until you read it (proposed workflows and tools, with **Review** and **Decline**), what runs by itself (scheduled workflows) and what runs when you ask (other workflows, and installed tools). **Open a shared one** opens a file a coworker shared. A workflow's own page says when it runs, and has **Run now**, a **⋯** menu (pause or resume its schedule, **Share…**, **Remove**) and its runs. A run's report is its own page, with **Open in a chat**. Optionally an AI review of the results against your criteria. |
| **Workflows › Tools** | Tools your assistant made from its scripts. You read each one, code and all, and install it; installed tools run without asking, in the same sandbox as scripts. |
| **Workflows › Skills** | The GIS skills your assistant reads: when to use which tool, and how. Skills are advice; they never grant access. |
| **Activity** | One page. On top, **Waiting for you**: requests from the app's own chat (**Answer in the chat**), requests from other AI apps (listed only, with **Decide in browser**), and workflows and tools to read. Under it, **History**: the audit log, newest first, in plain words with the line as written under it; quick views (**All**, **Decisions**, **Refused**, **Exports**); a filter for month, app, tool, decision and text; and **Verify this month**, which checks the chain. |
| **Settings** | One sheet over the page that is open. Its first view: the AI for chats (API key, model and thinking level for new chats), Python (where scripts run; other environments), the map (**Street background**; appearance: light, dark or match the system) and this computer (where things are kept; your organization's rules). Parts that open inside it: **Assistant** (key, model, GIS engine), **Python environments**, **Privacy and files**, **Keyboard shortcuts** and **Organization rules** (the policy in force, in plain sentences, and its switches). The foot has the version and **Classic windows** (the earlier session list and settings window, still kept). |

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

## Pictures

Taken from the app by [`screenshots.py`](../app/desktop/e2e/screenshots.py), on [public data for central Austin](images/DATA-CREDITS.md).

<table>
  <tr>
    <td width="50%"><img src="images/start.png" alt="The start: a new chat in a panel in the middle of the window, asking What do you want to know?, with questions to try and a line for what waits for you"><br><b>The start</b>: the question first.</td>
    <td width="50%"><img src="images/chat.png" alt="A chat in the panel on the left, after an export the analyst allowed: the assistant's steps in one line and its answer, with the chat's map filling the window behind the panel"><br><b>A chat</b>: steps and answer in the panel, the map behind.</td>
  </tr>
  <tr>
    <td><img src="images/map-only.png" alt="The same chat with the panel put away: the map alone, with a small pill at the top left that brings the panel back"><br><b>Show only the map</b>: the panel put away, for reviewing.</td>
    <td><img src="images/tool-review.png" alt="A tool the assistant proposed, with its code, before it is installed"><br><b>Workflows › Tools</b>: read line by line before installing.</td>
  </tr>
  <tr>
    <td><img src="images/workflow-review.png" alt="A proposed workflow, in plain words and in full, before it is approved"><br><b>Workflows</b>: a proposal, read before it is approved.</td>
    <td><img src="images/activity.png" alt="Activity: what waits for you on top; under it History, every call and decision in plain words, newest first, with a button that verifies the month's chain"><br><b>Activity</b>: what waits for you, then every call and every decision.</td>
  </tr>
  <tr>
    <td><img src="images/settings.png" alt="The Settings sheet over the open page: the assistant (API key, model and thinking level for new chats), Python, the map and this computer"><br><b>Settings</b>: one sheet, over the page that is open.</td>
    <td></td>
  </tr>
</table>
