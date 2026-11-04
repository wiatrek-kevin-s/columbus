//! SDK for Columbus engine plugins written in Rust.
//!
//! A plugin is a folder holding a `plugin.yaml` and an executable built on
//! this crate. The gateway starts the executable as a worker process and
//! speaks the worker protocol with it: line-delimited JSON-RPC 2.0 over
//! stdin/stdout (see `docs/worker-protocol.md`). The SDK handles the
//! protocol, so a plugin only writes tools:
//!
//! ```no_run
//! use columbus_plugin::{write_csv, Context, Plugin, Severity, ToolError};
//! use serde_json::json;
//!
//! fn main() {
//!     Plugin::new("contoso.null-check", env!("CARGO_PKG_VERSION"))
//!         .tool("null_geometry", &["layer"], &["report"], null_geometry)
//!         .serve();
//! }
//!
//! fn null_geometry(ctx: &mut Context) -> Result<serde_json::Value, ToolError> {
//!     let layer = ctx.dataset("layer")?;
//!     let total = layer.row_count();
//!     let mut nulls = 0u64;
//!     for (i, row) in layer.rows()?.enumerate() {
//!         if i % 1000 == 0 {
//!             // Also the cancellation point.
//!             ctx.progress(Some(100.0 * i as f64 / total.max(1) as f64), "checking")?;
//!         }
//!         if row?.geometry().is_none() {
//!             nulls += 1;
//!         }
//!     }
//!     if nulls > 0 {
//!         ctx.warn("geometry.null", Severity::Warn, format!("{nulls} rows have no geometry"));
//!     }
//!     let rows = write_csv(
//!         ctx.output_path("report")?,
//!         &["metric", "value"],
//!         [["null_geometry".to_string(), nulls.to_string()]],
//!     )?;
//!     ctx.set_output_count("report", "rows", rows);
//!     Ok(json!({"rows": total, "null_geometry": nulls}))
//! }
//! ```
//!
//! Plugins receive datasets as read-only [`Extract`]s the gateway produced
//! through its governed read path, and paths for their outputs. They never
//! receive credentials. Print nothing to stdout: it carries the protocol.
//! Use [`Context::log`] or stderr for diagnostics.

mod extract;
mod geometry;
mod output;
mod plugin;

pub use extract::{Cell, Extract, ExtractError, Row, Rows};
pub use geometry::{Coord, GeomError, Geometry, GeometryFormat, GeometryType};
pub use output::{csv_field, feature, write_csv, write_geojson};
pub use plugin::{codes, Context, Plugin, Severity, ToolError, Warning, PROTOCOL_VERSION};
