# Build brief, part 3: the open workbench (0.15)

Version 1.0 · as of 2026-11-10 · written by the builder from the owner's instruction of 2026-11-10 and three research reports of that day · adds to `docs/build-brief.md` (part 1) and `docs/build-brief-2.md` (part 2); IDs continue from them.

Parts 1 and 2 made Columbus reach more data and made one weekly job work. This part changes how Columbus is used: a window like the AI apps people already know, an assistant that works freely on local copies, tools that are installed rather than built, and projects that sync with a Git server. The same rules apply: one source per fact, and cite IDs.

## The owner's instruction (2026-11-10)

> ok I have a new UI design I want to implement instead. I want to basically "mimic" or "copy" the design that apps such as "Claude" and "Chatgpt" use. Similar to those apps, which have tables/outputs/etc from the AI pop up on the right hand side, I want that as well (just to include mapping). I also want you to look at how the app itself is used by the user; i.e. is the approach we have right? Is sandboxing appropriate? We are already sending data to the AI itself, is really the only thing we need to ensure is that the AI can't access the database connections itself; i.e. that it has to request data and make essentially a copy of it locally, and then from there it can and should be able to do anything with it right? I am afraid that the approach we currently have is overkill, in that now columbus itself is required to maintain the use of each tool in the "sandbox". But i also don't understand or know what other companies use, and what is a good approach to use - hence where you come in. I really just need for an AI to be able to connect to my databases, not be able to edit it but can read it directly, acess my python env so that it can trigger scripts, run scripts, write scripts, etc - and access my documentation. That being said, i want it to have access to a map as well, i.e. reading and seeing a map, and have access to as many open source tools as possible to interact with said spatial data. I used rust gis as the example, since it has thousands of tools built in for MCP. I dont want to build or design all these spatial tools, i just want to be able to install them and have the AI use them. Documentation, workflows, and projects are still important to me to keep. Outside of that; take these requirements and implement/redesign the entire application however you want to best fit these deliverables as well as just taking the best approaches that you think should be used. Make sure you also update the readme and github page once your done. I don't want to be involved in it at all; just notify me when your done. Also - another requirement is that i can connect both github and gitea to the application; this way i can sync projects with a repo within the app; i.e. if a user has a company gitea they use they can push/pull/track projects locally.

This instruction reopens decisions that parts 1 and 2 froze, and hands them to the builder ("redesign the entire application however you want"). The reopened ones are named under each decision below. Everything not named stays as it was.

## What the research found (2026-11-10)

Three reports, read from vendor documentation and source on the day. The full texts are in the build session; the findings that decide things:

1. **Others use a sandbox in place of asking.** Claude Code, Codex, Cursor and VS Code run code without a prompt when it runs inside an operating-system sandbox, and ask only at its edge (a new network host, a write outside the working folder, running outside the sandbox). Anthropic measured that people accept 93% of prompts, and that sandboxing removed 84% of them. The edge is files and network together. Secrets stay outside the sandbox with a broker that uses them. Third-party tool servers are the soft spot: most apps run them with the person's full access.
2. **"Rust GIS" is RustGIS (opengeos, Apache-2.0).** Its MCP server has 12 tools. The "over 1,000" tools sit behind three of them (search, describe, run) and come from a separate MIT program, the GeoLibre runner (1,066 tools, most of them Whitebox tools), one 15 to 18 MB file per system. The runner lists every tool and its parameters as JSON (`geolibre manifests`) and runs one with `geolibre <tool> --key=value`. QGIS (`qgis_process --json`, about 2,000 algorithms on one install) and GDAL (`gdal --json-usage`) describe themselves the same way. No MCP server with thousands of GIS tools exists; the GIS MCP servers found have 12 to 125 tools, and several need the network, a running desktop app, or carry GPL or AGPL licences.
3. **Git from a desktop app.** The pure-Rust Git library (gix 0.90) cannot push. libgit2 (GPL-2.0 with a linking exception) and a bundled Git (GPL-2.0) are outside the licence list (security baseline item 5). An installed Git is missing on many analysts' PCs. Both GitHub and Gitea (and Forgejo) can do everything a sync needs through their web APIs, with a token.

## Decisions

### D-15: the window is a sidebar, a chat, and a panel on the right

Like Claude and ChatGPT. Left: a sidebar with New chat, Search, the places (Chats, Projects, Data, Documentation, Workflows, Tools, Activity), recent chats, and Settings; it folds to a rail of icons. Middle: the chat, or a place's page. Right: a panel that opens when the assistant makes something: the map and its table, the files of the chat, how a result was made, a note. The panel can be resized, can fill the window, and closes.

*Reopens:* the window of the UI-review project (one panel over a full-window map, merged 2026-11-10), and the D-2 non-goal "screen redesign".

### D-16: one boundary, in place of approving each script

The owner asked whether sandboxing is appropriate and whether the approach is overkill. The answer: **keep the sandbox, drop the asking.** The sandbox is what makes "do anything with the copy" safe; asking before every script, and wrapping every tool by hand, were the overkill.

The boundary, always true, loosened by no setting:

- **B1. Connections stay with Columbus.** Passwords live in the operating system's credential store. The assistant, its scripts and its tools never get a password, a connection file, the credential store, or a way to open a network connection.
- **B2. Reading goes through Columbus**, under each source's rules (read-only or read-write, which rows and fields, limits). What is read lands as a copy in the chat's workspace.
- **B3. Inside the workspace the assistant works freely.** Scripts in the person's Python environment and installed tools run without asking, in the sandbox: no network; they read the copies they are given and the Python or tool installation; they write only their own run folder. A script can spoil a copy, never a source.
- **B4. Columbus asks only when something would cross the boundary:** a copy leaving the workspace (an export to a folder that isn't granted, a web fetch to a host not on the list); a change to a real source (edits); code that wants the network or must run outside the sandbox (level B, trusted environments, arcpy); something new being installed (a toolbox, a personal tool, a workflow, a connected server); a project going to a Git server (the person presses Sync).
- **B5. An organization can keep it stricter** in its policy file: ask for every script, no scripts, no toolboxes, no sync, named hosts only.

In code: a new policy key `scripts.approval` (`each`, `session` or `none`; F-15). With `none`, a `run_python` in a governed, confined, level A environment runs without an approval. Level B and trusted runs ask every time, as now. The code's defaults do not change (everything off until the policy says so): the app's setup, and Settings, offer **Work freely on copies (recommended)**, which writes the open settings after the person confirms at the keyboard, the same way the existing switches do. `scripts.session_approval: true` keeps its meaning (`session`).

*Reopens:* F-7 (adds `scripts.approval`), REQ-17 and R-15 (a script approval per run), the "what can never be approved for later" rule for scripts only (edits keep it), AF-6. *Unchanged:* D-3 (the person's Python is governed and confined by default; trusted only under `environments.allow_trusted`, approved every run), D-13 (arcpy stays experimental), every rule about edits and exports.

### D-17: tools are installed as toolboxes, not built

A **toolbox** is a command-line program that describes its own tools. Columbus reads the description, lists each tool to the assistant through the tools it already has (`search_tools`, `describe_tool`, `run_tool`), and runs a tool in the sandbox at level A: input copies in, results out as workspace layers with provenance, no network. Nobody writes a tool by hand, and Columbus maintains one adapter per toolbox kind, not one wrapper per tool.

- **GeoLibre** (kind `geolibre`): the runner behind RustGIS's thousand tools, installed from the Tools page with one click. Columbus downloads the release for the system from the publisher, checks it against a hash pinned in Columbus (version 1.8.1 or later: earlier ones contained AGPL code), and never bundles it.
- **QGIS** (kind `qgis`, experimental): the person's own QGIS, found, never shipped (it is GPL).
- **Any other program** (kind `described`): a small description file names its tools and parameters.
- **Connected MCP servers** stay (D-6, the broker). They are shown on the Tools page.

*Reopens:* AF-13 (the registry lists tools nothing runs) and AF-16 (the RustGIS pilot). The `rustgis-pilot` branch stays as it is; nothing is deleted.

### D-18: the assistant can read and steer the map

`map_snapshot` already draws layers for the assistant. New: the assistant can read what the person's map shows (extent, layers on, the active layer, the selected feature) and change it (turn layers on, zoom to a layer or an extent), and can take its snapshot of that same view. Web map layers stay out of the assistant's reach (D-14, REQ-34 unchanged).

### D-19: a project has files

Each project gets a folder of files (`projects/<id>/files/`): scripts, notes, small things worth keeping. The assistant can list, read and write text files there and run a script from there; the person can put their own scripts in it. This is what "write scripts, trigger scripts" needs, and what syncs (D-20). Data still leaves the workspace only through export.

*Reopens:* AF-14 (save as script), and the rule that a project's folder holds only `project.yaml`.

### D-20: projects sync with GitHub and Gitea through their web APIs

No Git program and no Git library: Columbus talks to the server's API with a token the person enters in the app (kept in the credential store). So it works on a PC without Git, and nothing from a repository can ever run on the PC (no hooks, no Git configuration, no submodules). Works with github.com, GitHub Enterprise Server, Gitea and Forgejo.

- The person presses **Sync**: Columbus gets the server's changes, then sends theirs as one commit. **Check for changes** only reads. Nothing is sent automatically.
- A file changed on both sides is never merged or overwritten: the server's version keeps the name, the person's is kept beside it as a copy, and the app lists it under "Needs a look" (keep mine, keep theirs, keep both).
- Data, secrets and big files stay out: an ignore list (geodata formats, `.env`, keys, `.sde`, outputs), a 5 MB cap, and a stop if outgoing text looks like a token or a private key.
- The project's shared settings (name, instructions) travel as a file; a change that arrives is shown to the person before the assistant uses it.

*Left out:* GitHub's device sign-in (it needs the owner to register an app with GitHub; a token works today), merging line by line, branches other than the one linked.

### D-21: version 0.15.0, nothing published

The version becomes 0.15.0 when the last area merges. No release is published: that needs the owner's word (the never-without-approval list is unchanged).

## Requirements

| ID | The app must | Decision |
|---|---|---|
| REQ-35 | Show a sidebar, the chat in the middle, and a panel on the right that opens with what the assistant made (map, table, files, how it was made); the panel resizes, fills the window, and closes | D-15 |
| REQ-36 | Run a script in a governed level A environment without asking when the policy says `scripts.approval: none`; still ask for level B and trusted runs; never let a script or tool reach a connection, a password, the credential store, the person's other files, or the network | D-16 |
| REQ-37 | Offer the choice "Work freely on copies" in setup and Settings, confirmed at the keyboard, and show which mode is on | D-16 |
| REQ-38 | Install a toolbox (download, hash check, list its tools) from the Tools page and from the command line; list installed toolboxes and their tools; remove one | D-17 |
| REQ-39 | Let the assistant find, describe and run any tool of an installed toolbox through `search_tools`, `describe_tool`, `run_tool`; each run confined at level A, inputs as copies, outputs as workspace layers with provenance and the inputs' labels | D-17 |
| REQ-40 | Let the assistant read the person's map view and change it, and snapshot that view | D-18 |
| REQ-41 | Give each project a files folder the assistant can list, read, write (text) and run scripts from | D-19 |
| REQ-42 | Connect GitHub, GitHub Enterprise Server, Gitea or Forgejo with a token entered in the app, kept in the credential store, sent only to that host | D-20 |
| REQ-43 | Link a project to a repository (pick one or create a private one), show what changed here and on the server, sync on the person's click, keep both versions on a conflict, and show history | D-20 |
| REQ-44 | Keep Documentation, Workflows and Projects working as before | owner |
| REQ-45 | Update the README, the docs and the pictures to the new app | owner |

## Rules and formats added

One-way doors from here on, like F-1 to F-14.

- **F-15, policy keys.** `scripts.approval: each | session | none` (absent: `session` if `scripts.session_approval` is true, else `each`; stated, it wins over the older key). Three switches beside the existing `allow_*` ones: `allow_toolboxes: false` (toolboxes may be installed and used), `allow_project_files: false` (the assistant may read and write a project's files), `allow_sync: false` (projects may sync). Their options: `toolboxes.catalogue: true` (the built-in catalogue may download), `sync.hosts: []` (when not empty, only these hosts). As with every switch, absent means off. The app's "Work freely on copies" turns on `allow_scripts`, `scripts.approval: none`, `environments.allow`, `allow_personal_tools`, `allow_toolboxes`, `allow_project_files` and `allow_sync` together; turning it off sets `scripts.approval` back to `each` and leaves the other switches, which each have their own place in the app.
- **F-16, a toolbox.** `toolboxes/<id>/toolbox.yaml` beside the person's tools: `id` (the prefix of its tools: `geolibre.slope`), `name`, `kind` (`geolibre`, `qgis`, `described`), `program` (absolute path), `sha256` (of the program; checked before every run, and a changed program does not run until installed again), `version`, `source`, `licence`, `installed`. For `described`, a `tools.json` beside it lists each tool: `id`, `summary`, `args` (the command line, with `{name}` placeholders) and `params` (each `name`, `role` of `input`, `output` or `value`, a `kind` or `type`, `required`, `description`). The list of a toolbox's tools is cached beside it and rebuilt when the program's hash changes.
- **F-17, a project's files.** `projects/<id>/files/`, plain files in folders. `columbus-project.yaml` at its top carries the shared settings (name, instructions) when the project is linked. Paths that are absolute, contain `..`, `.git` or `.columbus`, use Windows reserved names, or differ only by case are refused; links are never followed.
- **F-18, Git servers and links.** `git-servers.yaml` in the app's settings: `id`, `kind` (`github`, `gitea`), `url`, `user`; the token is in the credential store only. A project's link (server, owner, repository, branch) and the last synced state are kept in the app's data, never in the files that sync.
- **R-22.** A toolbox tool, a script and a level A connected server all run with the same confinement: no network, their own run folder to write, the inputs they are given to read.
- **R-23.** What a toolbox tool prints goes to the assistant marked as untrusted text and cut to the same size as a script's output.
- **R-24.** Sync never forces: if the server moved while a sync ran, the sync is run again.

## Assumptions and fallbacks

| ID | Assumption | If false |
|---|---|---|
| A-7 | The GeoLibre runner's output matches its source at v1.8.1 (read on 2026-11-10; the build session could not run a downloaded program) | The adapter ignores what it doesn't know and reports a tool it cannot describe; a `described` toolbox works without it. Owner check H-3 |
| A-8 | The sandbox can run a toolbox's program at level A on Windows, macOS and Linux | The Tools page says on which system it cannot, and the toolbox is not offered there |
| A-9 | Gitea 1.20 or later (the endpoint that changes many files in one commit) | Older servers are refused with a message naming the version |
| A-10 | `qgis_process --json` output as documented | QGIS stays marked experimental; left out if it cannot be confirmed from QGIS's source |

## Tasks

T-26 to T-36 in `docs/tasks.json` (areas S9 to S12).

| Area | Tasks | Milestone |
|---|---|---|
| S9 | T-26 | The new window |
| S10 | T-27, T-28, T-29, T-30, T-31 | One boundary, toolboxes, the map, project files |
| S11 | T-32, T-33 | Sync with GitHub and Gitea |
| S12 | T-34, T-35, T-36 | Docs and pictures, version, review, hand-over |

## Optional checks for the owner

- **H-3.** On your PC: Tools › Add tools › GeoLibre › Install. Then ask in a chat for a slope or a hillshade of a raster you have, or a buffer of a layer. (The build could not run the real runner; this is its first real run.)
- **H-4.** Settings › Git servers: connect your GitHub or your company's Gitea with a token. Open a project, link it to a new private repository, press Sync, and look at the repository on the server.
- **H-5.** arcpy: still experimental, as in 0.14 (H-1).

## Honest limits to state in the docs

- What the assistant reads is sent to the AI service. The boundary does not change that.
- Text hidden in data, documents or a tool's output can still mislead the assistant. It cannot use that to read beyond a source's rules, to change a source, or to send anything out without the person.
- The sandbox is the operating system's (Windows AppContainer, macOS sandbox-exec, Linux Landlock and seccomp). Where it cannot be set up, Columbus refuses to run the code.
- A synced project folder is not a Git clone. Developers can clone the same repository with Git elsewhere.
- What the assistant writes into a project's files may quote data it has read. The person sees every changed file before a sync sends it.
