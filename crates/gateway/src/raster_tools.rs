//! Raster tools (design section 3, "Tool catalogue"): `rasters` connections,
//! a folder of GeoTIFF files, and `raster_op` (clip, reproject, slope,
//! aspect, hillshade, zonal statistics and band statistics) through GDAL.
//!
//! A raster dataset is a file's path relative to the connection's folder,
//! with forward slashes (`dem/travis_1m.tif`). The gateway resolves it: the
//! name must be plain (no `..`, no absolute path, no `\` or `:`), the file
//! must be the one that name reaches inside the folder after following
//! every link (so a symlink or junction cannot lead elsewhere, or to a file
//! the allow list does not name), and its first bytes must say TIFF. GDAL
//! chooses a driver from a file's content, so a ".tif" holding VRT XML
//! could read any file; the worker checks the bytes again, opens with the
//! GTiff driver only and keeps VRT switched off.
//!
//! Outputs are Cloud-Optimized GeoTIFFs in the workspace's `results/`,
//! recorded like vector_op's, with a provenance record and the data labels
//! of everything they were made from.

use std::collections::BTreeSet;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono::Local;
use columbus_shared::audit::AuditRecord;
use columbus_shared::policy::{normalize_identifier, Connection, EffectiveAccess, DEFAULT_LIMITS};
use rmcp::handler::server::wrapper::Parameters;
use rmcp::service::RequestContext;
use rmcp::{tool, tool_router, Json, RoleServer};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::gis_tools::{note_reads, LayerOut, Prov};
use crate::providers::{OutputMeta, OutputSource, HANDLE_SCHEME};
use crate::server::{checks_from, new_prov_id, Check, Gateway, Output, Severity};

pub(crate) const RASTERS: &str = "rasters";
/// The most pixels a raster_op output may have.
pub(crate) const MAX_PIXELS: u64 = 400_000_000;
/// The read limit in rows, counted as pixels: `limits.rows` x 1,000.
pub(crate) const PIXELS_PER_ROW: u64 = 1_000;
/// Classic TIFF and BigTIFF, little- and big-endian.
const TIFF_MAGIC: [&[u8; 4]; 4] = [b"II*\0", b"MM\0*", b"II+\0", b"MM\0+"];
const LIST_MAX: usize = 10_000;
const DEPTH_MAX: usize = 16;
const DEVICE_NAMES: &[&str] = &[
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];
/// Input names that are vector layers (read with the GIS tools' rules).
const VECTOR_INPUTS: &[&str] = &["mask", "zones"];

pub(crate) fn is_rasters(kind: &str) -> bool {
    kind.eq_ignore_ascii_case(RASTERS)
}

/// The refusal for SQL and vector tools on a rasters connection.
pub(crate) fn not_tabular(conn: &Connection) -> String {
    format!(
        "{} is a rasters connection (a folder of GeoTIFF files): describe its files with describe_dataset and process them with raster_op",
        conn.id
    )
}

fn has_raster_ext(name: &str) -> bool {
    let low = name.to_ascii_lowercase();
    low.ends_with(".tif") || low.ends_with(".tiff")
}

/// Whether a dataset name is one a rasters connection can serve, before
/// anything touches the file system.
pub(crate) fn name_ok(name: &str) -> Result<(), String> {
    let refuse = |why: &str| Err(format!("{name:?} is not a raster dataset name: {why}"));
    if name.is_empty() || name.len() > 512 {
        return refuse("it must be 1 to 512 characters");
    }
    if name.starts_with('/') || name.contains('\\') || name.contains(':') {
        return refuse("it must be a path relative to the connection's folder, with forward slashes, and no drive, \\ or :");
    }
    if name
        .chars()
        .any(|c| c.is_control() || "\"[]`*?<>|".contains(c))
    {
        return refuse("it contains a character file names here may not use");
    }
    // The allow and deny lists compare names after this normalization, so
    // a name it would change could be checked as a different file.
    if normalize_identifier(name) != name {
        return refuse("it has spaces next to a dot");
    }
    for part in name.split('/') {
        if part.is_empty() || part == "." || part == ".." {
            return refuse("it may not contain empty, . or .. parts");
        }
        if part.trim() != part || part.ends_with('.') {
            return refuse("its parts may not start or end with a space, or end with a dot");
        }
        let stem = part.split('.').next().unwrap_or("").to_ascii_uppercase();
        if DEVICE_NAMES.contains(&stem.as_str()) {
            return refuse("it names a device");
        }
    }
    if !has_raster_ext(name) {
        return refuse("only GeoTIFF files (.tif, .tiff) are served");
    }
    Ok(())
}

fn folder(conn: &Connection) -> Result<PathBuf, String> {
    let path = conn
        .path
        .as_deref()
        .ok_or_else(|| format!("{} has no path (the folder of GeoTIFFs)", conn.id))?;
    match std::fs::canonicalize(path) {
        Ok(p) if p.is_dir() => Ok(p),
        _ => Err(format!("{}'s folder does not exist", conn.id)),
    }
}

/// True when the file starts with a TIFF or BigTIFF header.
pub(crate) fn is_tiff(path: &Path) -> bool {
    let mut head = [0u8; 4];
    std::fs::File::open(path)
        .and_then(|mut f| f.read_exact(&mut head))
        .is_ok()
        && TIFF_MAGIC.iter().any(|m| **m == head)
}

/// The file a raster dataset name reaches in `folder`, or a refusal.
pub(crate) fn raster_path(folder: &Path, name: &str) -> Result<PathBuf, String> {
    name_ok(name)?;
    let mut p = folder.to_path_buf();
    for part in name.split('/') {
        p.push(part);
    }
    let real = std::fs::canonicalize(&p)
        .map_err(|_| format!("no raster {name:?} on this connection (list_datasets lists them)"))?;
    let rel = real
        .strip_prefix(folder)
        .map_err(|_| format!("{name:?} leads outside the connection's folder"))?;
    let rel: Vec<String> = rel
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    if rel.join("/") != name {
        return Err(format!(
            "{name:?} goes through a link or is spelled differently from the file's own name; use the exact name list_datasets shows"
        ));
    }
    if !real.is_file() {
        return Err(format!("{name:?} is not a file"));
    }
    if !is_tiff(&real) {
        return Err(format!(
            "{name:?} is named like a GeoTIFF but its content is not TIFF, so it is not opened"
        ));
    }
    Ok(real)
}

/// The GeoTIFFs in a rasters connection's folder that its allow and deny
/// lists permit, and how many there are in all. Links are not followed.
pub(crate) fn list_rasters(
    conn: &Connection,
    access: &EffectiveAccess,
) -> Result<(Vec<Value>, usize), String> {
    let root = folder(conn)?;
    let mut found: Vec<(String, u64)> = Vec::new();
    let mut todo = vec![(root.clone(), 0usize)];
    while let Some((dir, depth)) = todo.pop() {
        if found.len() >= LIST_MAX {
            break;
        }
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in entries.flatten() {
            let Ok(ft) = e.file_type() else { continue };
            if ft.is_symlink() {
                continue;
            }
            let path = e.path();
            if ft.is_dir() {
                if depth < DEPTH_MAX {
                    todo.push((path, depth + 1));
                }
                continue;
            }
            let Ok(rel) = path.strip_prefix(&root) else {
                continue;
            };
            let parts: Option<Vec<&str>> =
                rel.components().map(|c| c.as_os_str().to_str()).collect();
            let Some(parts) = parts else { continue };
            let name = parts.join("/");
            if ft.is_file() && name_ok(&name).is_ok() {
                found.push((name, e.metadata().map(|m| m.len()).unwrap_or(0)));
                if found.len() >= LIST_MAX {
                    break;
                }
            }
        }
    }
    found.sort();
    let total = found.len();
    let datasets = found
        .into_iter()
        .filter(|(n, _)| access.dataset_allowed(n))
        .map(|(n, bytes)| json!({"name": n, "type": "raster", "bytes": bytes}))
        .collect();
    Ok((datasets, total))
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct RasterOpArgs {
    /// One of: clip, reproject, slope, aspect, hillshade, zonal_stats, info.
    pub op: String,
    /// Input rasters and layers by role. Every op takes `raster`: a raster
    /// as "<connection>:<file>" from a rasters connection (e.g.
    /// "dem:travis/dem_1m.tif") or a ws:// handle of a raster an earlier
    /// raster_op wrote. clip may take `mask` (a polygon layer);
    /// zonal_stats takes `zones` (a layer). Layers are ws:// handles or
    /// vector datasets as "<connection>:<dataset>".
    pub inputs: serde_json::Map<String, Value>,
    /// clip: {bbox?: [minx, miny, maxx, maxy], bbox_crs (with bbox),
    /// all_touched?}. reproject: {crs: "EPSG:…", resampling (required:
    /// nearest|bilinear|cubic|average|mode), resolution? (target units)}.
    /// slope: {z_unit (required: m|ft|us-ft), units?: degrees|percent,
    /// band?}. aspect: {z_unit, band?}. hillshade: {z_unit, azimuth? (315),
    /// altitude? (45), band?}. zonal_stats: {stats?: subset of count, mean,
    /// min, max, sum, std (default count, mean, min, max), band? (1),
    /// all_touched? (false)}. info: none.
    #[serde(default)]
    pub params: serde_json::Map<String, Value>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct RasterOpOut {
    pub op: String,
    /// The output: a Cloud-Optimized GeoTIFF, or for zonal_stats a vector
    /// layer of the zones with the statistics. None for info.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub layer: Option<LayerOut>,
    /// info: per-band statistics and how they were computed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub info: Option<Value>,
}

/// A raster input, resolved.
struct RasterIn {
    path: PathBuf,
    record: Value,
    labels: BTreeSet<String>,
    /// The connection's read limit (rows), for the size guard.
    rows_limit: Option<u64>,
    read: Option<(String, String, u64)>,
}

impl Gateway {
    /// describe_dataset for a rasters connection. The caller has checked
    /// the dataset is allowed.
    pub(crate) async fn raster_describe(
        &self,
        conn: &Connection,
        dataset: &str,
    ) -> Result<Value, String> {
        let path = raster_path(&folder(conn)?, dataset)?;
        let mut info = self
            .state
            .worker
            .core(
                "describe_raster",
                json!({"path": path}),
                Duration::from_secs(60),
            )
            .await
            .map_err(|e| e.to_string())?;
        if let Some(o) = info.as_object_mut() {
            o.insert("name".into(), json!(dataset));
            o.insert(
                "bytes".into(),
                json!(std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0)),
            );
        }
        Ok(info)
    }

    async fn raster_input(&self, v: &Value, prov: &str) -> Result<RasterIn, String> {
        if let Some(h) = v.as_str().filter(|s| s.starts_with(HANDLE_SCHEME)) {
            let path =
                crate::providers::resolve_handle(&self.state.session, &self.state.sandbox, h)?;
            let meta = self.state.outputs.get(&path);
            match meta.as_ref().map(|m| m.source) {
                Some(OutputSource::Engine | OutputSource::Plugin) => {}
                Some(OutputSource::Provider) => {
                    return Err(format!(
                        "{h} came from a connected server, and raster_op only opens rasters Columbus or an approved engine plugin wrote. Load the data through a rasters connection instead"
                    ))
                }
                None => {
                    return Err(format!(
                        "{h} was not written by a Columbus tool in this session, so raster_op will not open it"
                    ))
                }
            }
            if !self.state.outputs.unchanged(&path) {
                return Err(format!(
                    "{h} changed after Columbus wrote it, so raster_op will not open it"
                ));
            }
            if !has_raster_ext(&path.to_string_lossy()) {
                return Err(format!(
                    "{h} is not a raster; layers go in mask (clip) or zones (zonal_stats)"
                ));
            }
            let labels = self.state.flow.file_labels(&path);
            return Ok(RasterIn {
                record: json!({"input": "raster", "handle": h, "labels": labels,
                    "made_by": meta.as_ref().map(|m| m.made_by.clone()),
                    "provenance": meta.and_then(|m| m.provenance)}),
                path,
                labels,
                rows_limit: None,
                read: None,
            });
        }
        let (conn_id, dataset) = match v {
            Value::String(s) => s
                .split_once(':')
                .map(|(c, d)| (c.trim().to_string(), d.to_string())),
            Value::Object(m) if !m.contains_key("where") => Some((
                m.get("connection")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                m.get("dataset")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
            )),
            _ => None,
        }
        .ok_or("the raster input must be \"<connection>:<file>\" from a rasters connection, or a ws:// handle of a raster")?;
        let (access, conn) = self.access(&conn_id).map_err(|e| e.to_string())?;
        if !is_rasters(&conn.kind) {
            return Err(format!(
                "{conn_id} is a {} connection; the raster input comes from a rasters connection or a ws:// raster",
                conn.kind
            ));
        }
        if !access.dataset_allowed(&dataset) {
            return Err(format!(
                "dataset {dataset:?} is not in this connection's allowed datasets"
            ));
        }
        refuse_filters(&access, &dataset)?;
        let path = raster_path(&folder(conn)?, &dataset)?;
        self.note_read(&conn.id, None, None, Some(prov), "raster_op");
        let meta = std::fs::metadata(&path).ok();
        Ok(RasterIn {
            record: json!({"input": "raster", "connection": conn.id, "dataset": dataset,
                "bytes": meta.as_ref().map(|m| m.len()),
                "modified": meta.and_then(|m| m.modified().ok()).map(|t| chrono::DateTime::<Local>::from(t).to_rfc3339())}),
            path,
            labels: [columbus_flow::connection_label(&conn.id)].into(),
            rows_limit: access.limits.rows,
            read: Some((conn.id.clone(), dataset, 0)),
        })
    }
}

/// A row filter (the connection's or a data group's) cannot be applied to
/// a raster; serving the whole file would ignore it, so refuse instead.
pub(crate) fn refuse_filters(
    access: &columbus_shared::policy::EffectiveAccess,
    dataset: &str,
) -> Result<(), String> {
    if !access.filters_for(dataset).is_empty() {
        return Err(format!(
            "{dataset} has a row filter, which cannot be applied to a raster; a rasters connection takes no row_filter or data-group filter (ask IT to remove it or narrow the allowed files instead)"
        ));
    }
    Ok(())
}

/// Most pixels an output may have: 400 million, or the read limit in rows
/// x 1,000 when that is smaller.
pub(crate) fn pixel_limit(rows_limits: impl IntoIterator<Item = Option<u64>>) -> u64 {
    rows_limits
        .into_iter()
        .flatten()
        .map(|r| r.saturating_mul(PIXELS_PER_ROW))
        .fold(MAX_PIXELS, u64::min)
}

#[tool_router(router = raster_router, vis = "pub(crate)")]
impl Gateway {
    #[tool(
        description = "Process rasters inside the workspace: clip (to a polygon layer or a bbox), reproject (resampling must be chosen), slope, aspect and hillshade of a DEM (z_unit must be stated; refused in degrees or Web Mercator: reproject first), zonal_stats (statistics per zone as a vector layer) and info (band statistics). Inputs are files of a rasters connection as \"<connection>:<file>\" or ws:// handles of earlier raster results; masks and zones are layers. Outputs are Cloud-Optimized GeoTIFFs with a handle. Outputs over 400 million pixels (or the read limit x 1,000) are refused: clip first. Checks report nodata, CRS, resampling of categories, zones outside the raster or with too few cells. No approval needed: nothing leaves the workspace.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            open_world_hint = false
        )
    )]
    async fn raster_op(
        &self,
        Parameters(args): Parameters<RasterOpArgs>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<Json<Output<RasterOpOut>>, String> {
        let request = json!({"op": args.op, "inputs": args.inputs, "params": args.params});
        let mut rec = self.gate(&ctx, "raster_op", &request)?;
        if args.op.is_empty() || !args.op.chars().all(|c| c.is_ascii_lowercase() || c == '_') {
            return Err(self.deny(&ctx, rec, "op must be one of the listed operations"));
        }
        let prov = new_prov_id();
        let started = Local::now();
        let mut paths = serde_json::Map::new();
        let mut inputs = Vec::new();
        let mut layer_files = Vec::new();
        let mut all_files: Vec<PathBuf> = Vec::new();
        let mut labels = BTreeSet::new();
        let mut warnings = Vec::new();
        let mut reads = Vec::new();
        let mut rows_limits = vec![DEFAULT_LIMITS.narrow(self.state.policy.limits).rows];
        for (name, v) in &args.inputs {
            let got = if name == "raster" {
                self.raster_input(v, &prov).await.map(|r| {
                    labels.extend(r.labels);
                    rows_limits.push(r.rows_limit);
                    reads.extend(r.read);
                    (r.path, r.record)
                })
            } else if VECTOR_INPUTS.contains(&name.as_str()) {
                self.input_file(name, v, &prov).await.map(|i| {
                    layer_files.push(i.path.clone());
                    warnings.extend(i.warning);
                    reads.extend(i.read);
                    (i.path, i.record)
                })
            } else {
                Err(format!(
                    "{name:?} is not an input of raster_op: inputs are raster, mask (clip) and zones (zonal_stats)"
                ))
            };
            match got {
                Ok((path, record)) => {
                    paths.insert(name.clone(), json!(path));
                    all_files.push(path);
                    inputs.push(record);
                }
                Err(e) => {
                    note_reads(&mut rec, &reads);
                    return Err(self.deny(&ctx, rec, e));
                }
            }
        }
        note_reads(&mut rec, &reads);
        labels.extend(self.state.flow.labels_for(&layer_files));
        let max_pixels = pixel_limit(rows_limits);
        let stem = match self
            .state
            .sandbox
            .resolve(&format!("results/{}-{prov}", args.op))
        {
            Ok(s) => s,
            Err(e) => return Err(self.deny(&ctx, rec, e)),
        };
        let timeout = self.state.policy.limits.seconds.unwrap_or(120).max(1);
        let reply = self
            .state
            .worker
            .core(
                "raster_op",
                json!({"op": args.op, "inputs": paths, "params": args.params,
                       "output_stem": stem, "max_pixels": max_pixels}),
                Duration::from_secs(timeout + 10),
            )
            .await;
        let reply = match reply {
            Ok(r) => r,
            Err(e) => {
                let msg = e.to_string();
                self.audit(
                    &ctx,
                    AuditRecord {
                        decision: "error".into(),
                        reason: Some(msg.clone()),
                        ..rec
                    },
                )?;
                return Err(msg);
            }
        };
        let summary = reply.get("summary").cloned().unwrap_or(Value::Null);
        let path = match reply.get("output_path").and_then(Value::as_str) {
            None if args.op == "info" => None,
            Some(p)
                if args.op != "info"
                    && Path::new(p).starts_with(self.state.sandbox.root())
                    && p.starts_with(&*stem.to_string_lossy())
                    && Path::new(p).is_file() =>
            {
                Some(PathBuf::from(p))
            }
            _ => {
                return Err(self.deny(&ctx, rec, "the engine returned no output in the workspace"))
            }
        };
        let rows = summary.get("rows").and_then(Value::as_u64);
        let mut checks = warnings;
        checks.extend(checks_from(reply.get("checks")));
        let inherit = self
            .state
            .outputs
            .inherit(&all_files)
            .with_step_fails(&checks, &prov);
        if let Some(p) = &path {
            self.state.flow.record_file(p, &labels);
            self.state.outputs.record(
                p,
                OutputMeta {
                    made_by: format!("raster_op {}", args.op),
                    provenance: Some(prov.clone()),
                    rows,
                    source: OutputSource::Engine,
                    truncated: inherit.truncated
                        || checks.iter().any(|c| c.id == "input.truncated"),
                    inherited: inherit.fails,
                    ..Default::default()
                },
            );
        }

        if !labels.is_empty() {
            checks.push(Check {
                id: "flow.labels".into(),
                severity: Severity::Info,
                message: format!(
                    "The output carries data labels: {}",
                    labels.iter().cloned().collect::<Vec<_>>().join(", ")
                ),
                detail: None,
                fix: None,
            });
        }
        let handle = path.as_ref().and_then(|p| self.ws_handle(p));
        self.provenance(
            &ctx,
            Prov {
                id: &prov,
                started,
                inputs: json!(inputs),
                steps: json!([{"tool": "raster_op", "op": args.op, "params": args.params,
                               "max_pixels": max_pixels, "engine": reply.get("engine")}]),
                labels: labels.iter().cloned().collect(),
                checks: &checks,
                outputs: handle.iter().cloned().collect(),
            },
        );
        rec.prov = Some(prov.clone());
        if rows.is_some() {
            rec.rows = rows;
        }
        rec.labels = labels.into_iter().collect();
        self.audit(&ctx, rec)?;
        Ok(Json(Output {
            result: RasterOpOut {
                op: args.op,
                layer: handle.map(|handle| LayerOut { handle, summary }),
                info: reply.get("info").cloned(),
            },
            checks,
            provenance: Some(prov),
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tiff(path: &Path) {
        std::fs::write(path, b"II*\0rest of a tiff").unwrap();
    }

    #[test]
    fn names_are_plain_relative_paths() {
        for ok in [
            "dem.tif",
            "dem/travis_1m.tif",
            "a b/c-d_e.TIFF",
            "2026/x.v2.tif",
        ] {
            assert!(name_ok(ok).is_ok(), "{ok}");
        }
        for bad in [
            "",
            "../x.tif",
            "dem/../../x.tif",
            "./x.tif",
            "/etc/x.tif",
            "C:/x.tif",
            "dem\\x.tif",
            "x.tif:stream",
            "dem//x.tif",
            "x.png",
            "x.vrt",
            "x.tif.",
            " x.tif",
            "dem /x.tif",
            "x .tif",
            "\"x\".tif",
            "con.tif",
            "dem/nul.tif",
            "/vsicurl/x.tif",
            "x\n.tif",
        ] {
            assert!(name_ok(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn resolution_stays_inside_the_folder() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("rasters");
        std::fs::create_dir_all(root.join("dem")).unwrap();
        tiff(&root.join("dem").join("a.tif"));
        let outside = dir.path().join("secret.tif");
        tiff(&outside);
        let root = std::fs::canonicalize(&root).unwrap();
        assert!(raster_path(&root, "dem/a.tif").is_ok());
        assert!(raster_path(&root, "dem/missing.tif").is_err());
        assert!(raster_path(&root, "../secret.tif").is_err());
        // Content, not the name, decides: VRT XML in a ".tif" is refused.
        std::fs::write(
            root.join("fake.tif"),
            "<VRTDataset><SourceFilename>/etc/passwd</SourceFilename></VRTDataset>",
        )
        .unwrap();
        let e = raster_path(&root, "fake.tif").unwrap_err();
        assert!(e.contains("not TIFF"), "{e}");
        assert!(is_tiff(&root.join("dem/a.tif")));
        // Every TIFF header form is accepted.
        for m in TIFF_MAGIC {
            std::fs::write(root.join("m.tif"), m).unwrap();
            assert!(raster_path(&root, "m.tif").is_ok());
        }
    }

    #[cfg(unix)]
    #[test]
    fn links_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("rasters");
        std::fs::create_dir_all(root.join("public")).unwrap();
        std::fs::create_dir_all(root.join("secret")).unwrap();
        tiff(&root.join("secret").join("b.tif"));
        let outside = tempfile::tempdir().unwrap();
        tiff(&outside.path().join("c.tif"));
        std::os::unix::fs::symlink(outside.path(), root.join("out")).unwrap();
        std::os::unix::fs::symlink(root.join("secret/b.tif"), root.join("public/b.tif")).unwrap();
        let root = std::fs::canonicalize(&root).unwrap();
        // Out of the folder, and within it to a file the allow list may not name.
        assert!(raster_path(&root, "out/c.tif").is_err());
        let e = raster_path(&root, "public/b.tif").unwrap_err();
        assert!(e.contains("link"), "{e}");
        assert!(raster_path(&root, "secret/b.tif").is_ok());
        // Letter case must match the file's own name.
        assert!(raster_path(&root, "Secret/b.tif").is_err());
    }

    #[test]
    fn pixel_limit_takes_the_smallest() {
        assert_eq!(pixel_limit([None]), MAX_PIXELS);
        assert_eq!(pixel_limit([Some(200_000)]), 200_000_000);
        assert_eq!(pixel_limit([Some(1_000_000), Some(1_000)]), 1_000_000);
        assert_eq!(pixel_limit([Some(u64::MAX)]), MAX_PIXELS);
    }
}
