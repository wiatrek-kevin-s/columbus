# Security policy

Columbus decides what an AI assistant may read, change, run and send on your computer, so a security problem in it matters. Thank you for reporting one.

## Reporting a problem

Please report it **privately**: on this repository's **Security** tab, choose **Report a vulnerability** ([direct link](https://github.com/wiatrek-kevin-s/columbus/security/advisories/new)). Don't open a public issue for it.

Say which version you used (`columbus-mcp --version`), your system (Windows, macOS), what you did and what happened. A small example is the most helpful thing you can send. Never send real passwords, tokens or your organization's data.

You'll get an answer within two weeks. Columbus is maintained by one person with AI help, so fixes go into the next test release; the release notes credit you unless you'd rather not be named.

## Supported versions

Only the newest release gets fixes. Columbus is in test releases (0.x): update to the latest before reporting.

## What Columbus promises

- **Rules in code.** What an assistant may read and do is decided by Columbus's own code from your organization's policy and your connections, never by the assistant's instructions. Connections are read-only unless policy allows edits; row filters and hidden fields are applied before any data reaches the assistant.
- **One boundary.** Your connections and passwords stay with Columbus: your assistant, its scripts and its tools never get a password, a connection file, the credential store or a way onto the network. Reading goes through Columbus, under each source's rules, and what is read lands as a copy in the chat's workspace. Inside the workspace your assistant can work on those copies; a script can spoil a copy, never a source.
- **Your approval at the boundary.** Columbus asks you when something would cross it: an export to a folder that isn't pre-approved, a download from a host not on the list, an edit to a source (each change set), a script that needs the network or must run outside the sandbox, and anything new being installed (a tool, a workflow, an environment, a connected server). Each approval is bound to the exact request, single-use and expiring. Whether you are also asked before each script that stays in the sandbox is your organization's choice (`scripts.approval`): every run unless the policy says otherwise; with **Work freely on copies** in the app, which you confirm at the keyboard, such scripts run without asking.
- **Confinement.** Scripts and tools run in the operating system's sandbox (AppContainer on Windows, Seatbelt on macOS, Landlock and seccomp on Linux), or not at all: no network, their inputs and the Python installation to read, their own run folder to write. Tests run real scripts that try to reach the connections, the policy, other chats' files, your home folder and the network, and check that each attempt fails. Local connected servers are confined too, unless your organization allows an unconfined pilot. The one exception for scripts is a *trusted* Python environment (experimental), which your organization must turn on and which runs as you, each run approved.
- **Toolboxes are pinned and confined.** A toolbox (a program with many GIS tools, such as GeoLibre) is someone else's code. Columbus runs it only in the same sandbox as scripts (no network, copies of its inputs, its own run folder), and only while its file has the SHA-256 recorded when you installed it. A download from Columbus's catalogue is checked against a hash pinned in Columbus before it is unpacked.
- **The map is only shown.** Your assistant can read what your map shows and point it at a result (`map_view`). That changes what is on your screen, never data, a source or a file, and your organization can turn it off (`deny_tools: [map_view]`). While the street background or a web layer is on, moving the map fetches tiles for the new area, so that service sees an area your assistant chose.
- **Sync is yours to press.** A project's files go to a Git server only when you press **Sync**, to the repository you linked, and your assistant has no part in it. The token is sent only to that server, whose certificate is always checked. Data, keys and files over 5 MB stay out, and text that looks like a password or a token stops the sync before anything is sent.
- **Secrets stay out of files.** Database passwords, ArcGIS sign-ins and Git server tokens are kept in the operating system's credential store, never in a file, a log or anything the assistant sees.
- **A record.** Every call, including refusals, is a line in a hash-chained audit log on your computer, and every output has a provenance record.
- **Nothing phoned home.** No telemetry and no account. Columbus contacts only what you set up (your databases and services, the AI service you chose, the basemap if you turn it on).

The [technical reference](docs/reference.md) says exactly how each rule is enforced.

## What it does not promise

- **No professional audit.** Columbus is built mostly by an AI model. It is tested automatically on Windows, macOS and Linux and reviewed by AI, but no human security firm has audited it.
- **The AI service sees what the rules allow.** Rows the assistant reads go to the AI service you chose, under that service's terms.
- **A toolbox's results are its own.** The hash shows the program is the file its publisher released (or the one you added), not that its results are right. Check them as you would any analysis.
- **Trusted environments are outside its control.** A script in one can do anything you can.
- **What your assistant writes may quote what it read.** A script or a note it writes into a project's files can hold values from your data. You see every changed file before a sync sends it.
- **Two parts are not yet tried on the real thing.** The real GeoLibre program was never run while Columbus was built (its first real run is your first install), and sync was tested against stand-in servers, never a real GitHub or Gitea.
- **Hidden text can mislead the assistant.** Text hidden in data, documents or a tool's output can still steer it. It cannot use that to read beyond a source's rules, to change a source, or to send anything out without you.
- **The sandbox is the operating system's.** Its known limits on each system are listed in the reference; where it cannot be set up, Columbus refuses to run the code.
- **Your computer is trusted.** Columbus can't protect against malware or another person already on your computer.
- **The assistant can be wrong.** Checks catch many mistakes, not all; review results before you rely on them.
