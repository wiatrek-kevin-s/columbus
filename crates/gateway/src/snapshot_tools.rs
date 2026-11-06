//! `map_snapshot`: a picture of the session's layers for the assistant to
//! look at, drawn on this computer from the layers alone.
//!
//! What reaches the AI is the same data it could already read, as a
//! picture: geometry only. No attribute value, label or field name is
//! drawn, no basemap or other tile is fetched (nothing leaves the computer
//! to make it), and only layers Columbus wrote in this session are drawn.
//! An organization can turn it off with `deny_tools: [map_snapshot]`.

use std::time::Duration;

use base64::Engine as _;
use columbus_shared::audit::AuditRecord;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ContentBlock as Content};
use rmcp::service::RequestContext;
use rmcp::{tool, tool_router, RoleServer};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::server::{new_prov_id, Gateway};

/// Largest picture returned, in bytes: within the AI service's limit for
/// one image once it is base64 (5 MB).
const MAX_PNG: u64 = 3_600_000;

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SnapshotArgs {
    /// 1 to 8 workspace layer handles (ws://…) from this session, drawn in
    /// this order (the first underneath).
    pub layers: Vec<String>,
    /// The area to draw, [minx, miny, maxx, maxy] in the first layer's CRS;
    /// leave out for all of the layers.
    #[serde(default)]
    pub bbox: Option<[f64; 4]>,
    /// Largest width and height in pixels (default 1000 × 700, at most
    /// 1600 × 1200); the picture keeps the area's shape within them.
    #[serde(default)]
    pub width: Option<u32>,
    #[serde(default)]
    pub height: Option<u32>,
}

#[tool_router(router = snapshot_router, vis = "pub(crate)")]
impl Gateway {
    #[tool(
        description = "Look at layers on a map: draws 1 to 8 of this session's workspace layers (ws:// handles) as a PNG picture you can see, each layer its own colour (polygons lightly filled with an outline, lines and points solid), on a plain background with no basemap. Use it to check what numbers cannot show: slivers, gaps and overlaps, features in the wrong place (a CRS mix-up), a layer that does not line up with another, the shape of a selection. Geometry only: no attribute values or labels are drawn. The answer lists each layer's colour and feature count, the CRS and extent drawn. The picture is saved in the workspace (handle).",
        annotations(read_only_hint = true, open_world_hint = false),
        output_schema = rmcp::handler::server::common::schema_for_output::<crate::server::Output<Value>>()
    )]
    async fn map_snapshot(
        &self,
        Parameters(args): Parameters<SnapshotArgs>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, String> {
        let request = json!({"layers": args.layers, "bbox": args.bbox, "width": args.width, "height": args.height});
        let mut rec = self.gate(&ctx, "map_snapshot", &request)?;
        if args.layers.is_empty() || args.layers.len() > 8 {
            return Err(self.deny(&ctx, rec, "give 1 to 8 layers (ws:// handles)"));
        }
        let mut paths = Vec::new();
        for h in &args.layers {
            match self.own_output(h) {
                Ok((p, _)) => paths.push(p),
                Err(e) => return Err(self.deny(&ctx, rec, e)),
            }
        }
        let prov = new_prov_id();
        let out = match self
            .state
            .sandbox
            .resolve(&format!("snapshots/map-{prov}.png"))
        {
            Ok(p) => p,
            Err(e) => return Err(self.deny(&ctx, rec, e)),
        };
        if let Some(d) = out.parent() {
            if let Err(e) = std::fs::create_dir_all(d) {
                return Err(self.deny(
                    &ctx,
                    rec,
                    format!("could not make the snapshots folder: {e}"),
                ));
            }
        }
        let reply = self
            .state
            .worker
            .core(
                "map_snapshot",
                json!({"paths": paths, "out_path": out, "bbox": args.bbox,
                    "width": args.width.unwrap_or(1000), "height": args.height.unwrap_or(700)}),
                Duration::from_secs(120),
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
        // Exactly the file asked for, a plain file, of a size to send.
        let ok = reply.get("path").and_then(Value::as_str) == Some(&*out.to_string_lossy())
            && std::fs::symlink_metadata(&out)
                .is_ok_and(|m| m.file_type().is_file() && m.len() > 0 && m.len() <= MAX_PNG);
        let bytes = match ok.then(|| std::fs::read(&out)).transpose() {
            Ok(Some(b)) if b.starts_with(b"\x89PNG\r\n\x1a\n") => b,
            _ => return Err(self.deny(&ctx, rec, "the engine made no picture")),
        };
        let labels = self.state.flow.labels_for(&paths);
        self.state.flow.record_file(&out, &labels);
        let features: u64 = reply["layers"]
            .as_array()
            .map(|a| a.iter().filter_map(|l| l["features"].as_u64()).sum())
            .unwrap_or(0);
        rec.rows = Some(features);
        rec.labels = labels.iter().cloned().collect();
        self.audit(&ctx, rec)?;
        let layers: Vec<Value> = args
            .layers
            .iter()
            .zip(reply["layers"].as_array().cloned().unwrap_or_default())
            .map(|(h, l)| json!({"handle": h, "color": l["color"], "features": l["features"], "geometry_types": l["geometry_types"]}))
            .collect();
        let handle =
            crate::providers::handle_for(&self.state.session, self.state.sandbox.root(), &out);
        let result = json!({
            "handle": handle,
            "width": reply["width"],
            "height": reply["height"],
            "crs": reply["crs"],
            "extent": reply["extent"],
            "wrapped_antimeridian": reply["wrapped_antimeridian"],
            "layers": layers,
            "note": "Geometry only, no basemap: colours are per layer, as listed. A geographic CRS is drawn with longitude scaled for latitude; across the 180th meridian it is drawn in one piece (longitudes past 180).",
        });
        let structured = json!({"result": result, "checks": [], "provenance": Value::Null});
        let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
        let mut r = CallToolResult::success(vec![
            Content::text(structured.to_string()),
            Content::image(b64, "image/png"),
        ]);
        r.structured_content = Some(structured);
        Ok(r)
    }
}
