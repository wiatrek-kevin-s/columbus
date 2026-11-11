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
- **Your approval.** Exports, edits, scripts and downloads wait for your decision, bound to the exact request, single-use and expiring, unless you or your organization approved them ahead of time (a pre-approved folder, a tool you installed, a workflow you approved).
- **Confinement.** Scripts and tools run in the operating system's sandbox (AppContainer on Windows, Seatbelt on macOS, Landlock and seccomp on Linux), or not at all; so do local connected servers, unless your organization allows an unconfined pilot. The one exception for scripts is a *trusted* Python environment (experimental), which your organization must turn on and which runs as you, each run approved.
- **Toolboxes are pinned and confined.** A toolbox (a program with many GIS tools, such as GeoLibre) is someone else's code. Columbus runs it only in the same sandbox as scripts (no network, copies of its inputs, its own run folder), and only while its file has the SHA-256 recorded when you installed it. A download from Columbus's catalogue is checked against a hash pinned in Columbus before it is unpacked.
- **Secrets stay out of files.** Database passwords and ArcGIS sign-ins are kept in the operating system's credential store, never in a file, a log or anything the assistant sees.
- **A record.** Every call, including refusals, is a line in a hash-chained audit log on your computer, and every output has a provenance record.
- **Nothing phoned home.** No telemetry and no account. Columbus contacts only what you set up (your databases and services, the AI service you chose, the basemap if you turn it on).

The [technical reference](docs/reference.md) says exactly how each rule is enforced.

## What it does not promise

- **No professional audit.** Columbus is built mostly by an AI model. It is tested automatically on Windows, macOS and Linux and reviewed by AI, but no human security firm has audited it.
- **The AI service sees what the rules allow.** Rows the assistant reads go to the AI service you chose, under that service's terms.
- **A toolbox's results are its own.** The hash shows the program is the file its publisher released (or the one you added), not that its results are right. Check them as you would any analysis.
- **Trusted environments are outside its control.** A script in one can do anything you can.
- **Your computer is trusted.** Columbus can't protect against malware or another person already on your computer.
- **The assistant can be wrong.** Checks catch many mistakes, not all; review results before you rely on them.
