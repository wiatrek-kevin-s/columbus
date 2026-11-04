# columbus-plugin

The Rust SDK for Columbus engine plugins. A plugin is a folder holding a
`plugin.yaml` and an executable built on this crate. The gateway starts the
executable as a worker and speaks the
[worker protocol](../../docs/worker-protocol.md) with it: line-delimited
JSON-RPC 2.0 over stdin/stdout. The SDK handles the protocol (`describe`,
`run`, `cancel`, `progress`, `log`), so a plugin only writes tools.

```rust
use columbus_plugin::{write_csv, Context, Plugin, Severity, ToolError};
use serde_json::json;

fn main() {
    Plugin::new("contoso.null-check", env!("CARGO_PKG_VERSION"))
        .tool("null_geometry", &["layer"], &["report"], null_geometry) // name, params, outputs, handler
        .serve(); // until stdin closes
}

fn null_geometry(ctx: &mut Context) -> Result<serde_json::Value, ToolError> {
    let layer = ctx.dataset("layer")?; // a read-only extract
    let mut nulls = 0u64;
    for (i, row) in layer.rows()?.enumerate() {
        if i % 1000 == 0 {
            ctx.progress(None, format!("{i} rows"))?; // also the cancellation point
        }
        if row?.geometry().is_none() {
            nulls += 1;
        }
    }
    if nulls > 0 {
        ctx.warn("geometry.null", Severity::Warn, format!("{nulls} rows have no geometry"));
    }
    let n = write_csv(ctx.output_path("report")?, &["metric", "value"],
                      [["null_geometry".to_string(), nulls.to_string()]])?;
    ctx.set_output_count("report", "rows", n);
    Ok(json!({"null_geometry": nulls})) // the run's summary
}
```

## What the SDK gives a tool

* **`Context`**: `param_f64`, `param_i64`, `param_str` and `param_bool`
  read parameters. `dataset` opens an input extract, `input_file` gives a
  workspace file's path and `output_path` gives the path an output must be
  written to (write nowhere else). `set_output_count` reports `rows` or
  `features`. `warn` adds a warning (with optional `detail` and `fix`).
  `progress` reports progress; it returns `Err(Cancelled)` or
  `Err(Timeout)` once the run must stop, so call it with `?` in long loops.
  `log` sends diagnostics. Never print to stdout.
* **`ToolError`**: `InvalidParams` (-32602), `Unsupported` (-32001),
  `Engine` (-32002), `Timeout` (-32003) or `Cancelled` (-32004).
  `ExtractError`, `GeomError` and `io::Error` convert with `?`.
* **`Extract`**: the SQLite extract opened read-only. `meta`, `columns`,
  `row_count`, `geometry_column`, `srid`, `labels`, and `rows()` (streamed
  in batches). Rows have `get_f64`, `get_i64`, `get_str` and `get_blob`,
  plus `geometry()` and `properties()`.
* **`Geometry`**: a light reader for GeoPackage binary, EWKB and ISO WKB
  (all seven types, Z/M, both byte orders). `geometry_type`, `bbox`,
  `num_points`, `is_valid_basic` and `to_geojson`. It is not a geometry
  library.
* **Writers**: `write_csv` (RFC 4180) and `write_geojson` (a streamed
  FeatureCollection, with `feature` to build each feature).

The worker reads stdin on one thread and runs each tool on another, so
`cancel` is handled while a tool works. It runs one tool at a time; a
second `run` gets a "busy" error.

## Example

[`examples/geometry_stats.rs`](examples/geometry_stats.rs) is a complete
plugin (`columbus.example-rust`) with a `bbox` tool (extent, counts per
type, empty and invalid geometries) and a `wait` tool (for testing
cancellation). Its manifest is
[`examples/geometry-stats.plugin.yaml`](examples/geometry-stats.plugin.yaml).

```sh
cargo build --release -p columbus-plugin --example geometry_stats
# plugin folder: target/release/examples/geometry_stats[.exe] + the manifest, renamed plugin.yaml
```

`cargo test -p columbus-plugin` runs the unit tests and drives the built
example through the protocol, as the gateway does.
