//! Example engine plugin: geometry statistics for a dataset.
//!
//! Its manifest is `geometry-stats.plugin.yaml` next to this file. Build
//! with `cargo build --release -p columbus-plugin --example geometry_stats`
//! and put the executable and the manifest (renamed `plugin.yaml`) in a
//! plugin folder.
//!
//! Tools:
//! * `bbox`: overall bounding box, geometry counts per type, empty and
//!   invalid geometries, written as a `metric,value` table.
//! * `wait`: sleeps for `seconds`, reporting progress; for testing
//!   cancellation.

use std::collections::BTreeMap;
use std::thread;
use std::time::{Duration, Instant};

use columbus_plugin::{write_csv, Context, Plugin, Severity, ToolError};
use serde_json::{json, Value};

fn main() {
    Plugin::new("columbus.example-rust", env!("CARGO_PKG_VERSION"))
        .tool("bbox", &["layer"], &["stats"], bbox)
        .tool("wait", &["seconds"], &[], wait)
        .serve();
}

fn bbox(ctx: &mut Context) -> Result<Value, ToolError> {
    let layer = ctx.dataset("layer")?;
    if layer.geometry_column().is_none() {
        return Err(ToolError::invalid("layer has no geometry column"));
    }
    let total = layer.row_count();

    let mut rows = 0u64;
    let mut geometries = 0u64;
    let mut null = 0u64;
    let mut empty = 0u64;
    let mut invalid = 0u64;
    let mut undecodable = 0u64;
    let mut by_type: BTreeMap<&'static str, u64> = BTreeMap::new();
    let mut extent: Option<[f64; 4]> = None;

    for row in layer.rows()? {
        if rows.is_multiple_of(1000) {
            let pct = if total > 0 {
                Some(100.0 * rows as f64 / total as f64)
            } else {
                None
            };
            ctx.progress(pct, format!("Read {rows} of {total} rows"))?;
        }
        rows += 1;
        let geometry = match row?.geometry() {
            None => {
                null += 1;
                continue;
            }
            Some(Err(_)) => {
                undecodable += 1;
                continue;
            }
            Some(Ok(g)) => g,
        };
        geometries += 1;
        *by_type
            .entry(geometry.geometry_type().as_str())
            .or_default() += 1;
        if !geometry.is_valid_basic() {
            invalid += 1;
        }
        match geometry.bbox() {
            None => empty += 1,
            Some(b) => {
                extent = Some(match extent {
                    None => b,
                    Some(e) => [
                        e[0].min(b[0]),
                        e[1].min(b[1]),
                        e[2].max(b[2]),
                        e[3].max(b[3]),
                    ],
                });
            }
        }
    }
    ctx.progress(Some(100.0), format!("Read {rows} rows"))?;

    let mut table: Vec<[String; 2]> = vec![
        ["rows".into(), rows.to_string()],
        ["geometries".into(), geometries.to_string()],
        ["null".into(), null.to_string()],
        ["empty".into(), empty.to_string()],
        ["invalid".into(), invalid.to_string()],
        ["undecodable".into(), undecodable.to_string()],
    ];
    let names = ["min_x", "min_y", "max_x", "max_y"];
    for (name, v) in names.iter().zip(extent.unwrap_or([f64::NAN; 4])) {
        let text = if v.is_nan() {
            String::new()
        } else {
            v.to_string()
        };
        table.push([(*name).into(), text]);
    }
    for (ty, n) in &by_type {
        table.push([format!("type.{ty}"), n.to_string()]);
    }
    let written = write_csv(ctx.output_path("stats")?, &["metric", "value"], table)?;
    ctx.set_output_count("stats", "rows", written);

    if invalid > 0 {
        ctx.warn(
            "geometry.invalid",
            Severity::Warn,
            format!("{invalid} of {geometries} geometries failed basic validity checks"),
        )
        .detail(json!({"count": invalid}))
        .fix("Repair the geometries (close rings, drop degenerate lines) in the source dataset.");
    }
    if undecodable > 0 {
        ctx.warn(
            "geometry.undecodable",
            Severity::Warn,
            format!("{undecodable} geometry values could not be decoded"),
        )
        .detail(json!({"count": undecodable}));
    }

    Ok(json!({"rows": rows, "bbox": extent}))
}

fn wait(ctx: &mut Context) -> Result<Value, ToolError> {
    let seconds = ctx.param_f64("seconds")?;
    if !(0.0..=3600.0).contains(&seconds) {
        return Err(ToolError::invalid("seconds must be between 0 and 3600"));
    }
    let total = Duration::from_secs_f64(seconds);
    let start = Instant::now();
    loop {
        let elapsed = start.elapsed();
        let pct = if total.is_zero() {
            100.0
        } else {
            100.0 * elapsed.as_secs_f64() / seconds
        };
        ctx.progress(
            Some(pct.min(100.0)),
            format!("Waited {:.1} of {seconds} s", elapsed.as_secs_f64()),
        )?;
        if elapsed >= total {
            break;
        }
        thread::sleep(Duration::from_millis(50).min(total - elapsed));
    }
    Ok(json!({"waited_s": start.elapsed().as_secs_f64()}))
}
