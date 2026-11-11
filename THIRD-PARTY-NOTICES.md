# Third-party notices

Columbus is licensed under the Apache License 2.0 ([LICENSE](LICENSE)). It is built from, and its releases carry or install, software by others under their own licences. This file gives the notices that software asks to be passed on, and says how to find the licence of every other dependency.

## Bundled or installed by Columbus itself

### uv 0.13.7 (Astral Software Inc.)

Licence: MIT OR Apache-2.0 (you may use it under either). Columbus's own copy of uv is installed into the GIS engine from PyPI, pinned by version and SHA-256 (`workers/python/requirements-uv.txt`), and used only to create Python environments the person approves. Source: https://github.com/astral-sh/uv

```text
MIT License

Copyright (c) 2025 Astral Software Inc.

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```

The Apache-2.0 option's text is the same Apache License 2.0 as Columbus's own [LICENSE](LICENSE).

### roxmltree 0.21.2 (Yevhenii Reizner)

Licence: MIT OR Apache-2.0. A Rust crate compiled into `columbus-mcp`, used to read WMS and WMTS capabilities documents for web map layers. Source: https://github.com/RazrFalcon/roxmltree

```text
The MIT License (MIT)

Copyright (c) 2018 Yevhenii Reizner

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```

## Installed at your request, never bundled

### GeoLibre runner 1.8.1 (opengeos)

Licence: MIT. Columbus does not carry this program in its repository or its installers. When you choose Tools › Add tools › GeoLibre (or run `columbus-mcp toolbox install geolibre`), Columbus downloads the publisher's own release for your system from https://github.com/opengeos/geolibre-rust/releases/tag/v1.8.1, checks it against a SHA-256 pinned in Columbus, and keeps the publisher's `LICENSE` and `THIRD-PARTY-NOTICES.txt` beside the program (in your toolboxes folder, `geolibre/bin/`). Most of its tools come from Whitebox (`whitebox_next_gen`, MIT OR Apache-2.0). Versions before 1.8.1 contained code under the AGPL and are never offered. Source: https://github.com/opengeos/geolibre-rust

## Every other dependency

Columbus does not copy other projects' code into this repository. Their licences travel with them, and can be listed as follows:

- **Rust crates** compiled into `columbus-mcp`, `columbus-confine`, `columbus-workflow` and the desktop app: listed with versions in `Cargo.lock` and `app/desktop/src-tauri/Cargo.lock`. Each crate's licence is in its `Cargo.toml` (`license = …`) and on crates.io. To list them all: `cargo install cargo-license` (or `cargo-about`), then `cargo license` in the repository and in `app/desktop/src-tauri`.
- **Python packages** of the GIS engine (GDAL, PROJ and GEOS inside the pyogrio, rasterio and shapely wheels; geopandas, pyproj, pandas, DuckDB and its spatial extension, the database drivers): listed with exact versions and hashes in `workers/python/requirements-*.txt`. Each wheel installs its licence files in its `.dist-info` folder; `engine/venv` holds them after setup. To list them: `pip install pip-licenses` into the engine, then `pip-licenses --with-urls`.
- **npm packages** of the map page and the app's interface (MapLibre GL JS and others): listed in `app/ui/package-lock.json` and `app/desktop/package-lock.json`; the built map page carries their licence comments. To list them: `npx license-checker --production` in `app/ui`.
- **Optional software you install yourself** (Microsoft ODBC Driver 18 for SQL Server, ArcGIS Pro and arcpy, Python itself) is under its own licence; Columbus neither ships nor redistributes it.

New dependencies are accepted only under MIT, Apache-2.0, BSD, ISC, Zlib, Unicode-3.0 or unmodified MPL-2.0 (build brief, security baseline item 5).
