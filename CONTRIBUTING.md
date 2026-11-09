# Contributing

Thanks for your interest in Columbus.

## Issues are the best way to help

- **Something broken?** Open a [bug report](https://github.com/wiatrek-kevin-s/columbus/issues/new?template=bug_report.yml).
- **An idea?** Open a [feature request](https://github.com/wiatrek-kevin-s/columbus/issues/new?template=feature_request.yml).
- **A security problem?** Please don't open an issue: see [SECURITY.md](SECURITY.md).

Never paste passwords, tokens or your organization's data into an issue.

## How Columbus is made

Columbus is built mostly by Claude, an AI model made by Anthropic, directed by its owner, a GIS analyst. Pull requests are welcome, but they are read and tested the same way as the AI's own work, so a clear issue first is often quicker. Every change must keep the rules in [docs/reference.md](docs/reference.md) true, and none may loosen a security rule.

## Build and test

You need Rust (stable), Python 3.12 and, for the app, Node.js 22. From the repository folder:

```sh
python -m pip install --require-hashes -r workers/python/requirements-gis.txt   # the GIS engine
cargo build --workspace
cargo test --workspace
python -m unittest discover -s workers/python/tests
```

The map page is in `app/ui` (`npm ci && npm run build`) and the desktop app in `app/desktop` (Tauri 2). The [technical reference](docs/reference.md#build-and-run) has the details, and [How Columbus is built](docs/architecture.md) shows where each part lives. CI runs everything on Windows, macOS and Linux, with PostGIS, Oracle and SQL Server.

By contributing, you agree that your contribution is licensed under the [Apache License 2.0](LICENSE).
