A test release of the Columbus GIS server, for Windows (x64) and macOS (Apple Silicon).

| File | For |
| --- | --- |
| `columbus-<version>-windows-x64.zip` and `.mcpb` | Windows 10 and 11: see [docs/install.md](https://github.com/wiatrek-kevin-s/columbus/blob/main/docs/install.md) |
| `columbus-<version>-macos-arm64.zip` and `.mcpb` | Macs with M1 or later, macOS 13 or later: see [docs/install-macos.md](https://github.com/wiatrek-kevin-s/columbus/blob/main/docs/install-macos.md) |

Unpack the zip and set up the GIS engine once; the `.mcpb` adds Columbus to Claude desktop as an extension.

On macOS, engine plugins, `run_python` scripts and local MCP servers are refused: Columbus has no Mac sandbox for them yet. Everything else works as on Windows.

The programs are not code-signed yet, so Windows SmartScreen and macOS Gatekeeper warn about them; the install guides say what to do.
