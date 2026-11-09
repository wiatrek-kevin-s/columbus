//! Web map layers' sources (build brief part 2, Part 3; F-13, F-14, D-14).
//!
//! A web layer is a display-only layer drawn on the map from a service:
//! XYZ tiles, WMTS, WMS, ArcGIS MapServer and ImageServer, and ArcGIS
//! feature layers. Each is saved once by the person, as
//! `map-sources/<id>.yaml` beside the `connections` folder (F-13), or
//! supplied by the organization in policy `map.sources` (F-14).
//!
//! The gateway proxies every request (D-14); this module holds only the
//! formats, where they live, and the non-secret record of the hosts the
//! person approved when they saved a source (`map-sources/.hosts.json`,
//! `{"<id>": {"url", "hosts"}}`): hosts a service's capabilities named
//! (WMTS ResourceURL, WMS OnlineResource), shown to the person when they
//! saved it. F-13 gains no field for them. No MCP tool writes this folder,
//! and confined scripts can't write outside their run.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The record of approved capability hosts, beside the source files.
pub const HOSTS_FILE: &str = ".hosts.json";
/// Most saved sources read.
pub const MAX_SOURCES: usize = 200;

/// F-13 `kind`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum MapKind {
    #[serde(rename = "xyz")]
    Xyz,
    #[serde(rename = "wmts")]
    Wmts,
    #[serde(rename = "wms")]
    Wms,
    #[serde(rename = "arcgis-map")]
    ArcgisMap,
    #[serde(rename = "arcgis-image")]
    ArcgisImage,
    #[serde(rename = "arcgis-features")]
    ArcgisFeatures,
}

impl MapKind {
    pub fn as_str(self) -> &'static str {
        match self {
            MapKind::Xyz => "xyz",
            MapKind::Wmts => "wmts",
            MapKind::Wms => "wms",
            MapKind::ArcgisMap => "arcgis-map",
            MapKind::ArcgisImage => "arcgis-image",
            MapKind::ArcgisFeatures => "arcgis-features",
        }
    }

    pub fn parse(s: &str) -> Option<MapKind> {
        Some(match s.trim() {
            "xyz" => MapKind::Xyz,
            "wmts" => MapKind::Wmts,
            "wms" => MapKind::Wms,
            "arcgis-map" => MapKind::ArcgisMap,
            "arcgis-image" => MapKind::ArcgisImage,
            "arcgis-features" => MapKind::ArcgisFeatures,
            _ => return None,
        })
    }

    /// Drawn from images (tiles or exports); else features as vectors.
    pub fn raster(self) -> bool {
        self != MapKind::ArcgisFeatures
    }

    pub fn arcgis(self) -> bool {
        matches!(
            self,
            MapKind::ArcgisMap | MapKind::ArcgisImage | MapKind::ArcgisFeatures
        )
    }
}

/// F-13: one web map layer's source.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MapSource {
    pub id: String,
    pub title: String,
    pub kind: MapKind,
    /// https. XYZ: a template with `{z}`, `{x}` and `{y}`; WMTS: the
    /// capabilities document; WMS: the service endpoint; ArcGIS: the
    /// MapServer, ImageServer or FeatureServer (or one of its layers).
    pub url: String,
    /// WMS or WMTS layer name, or ArcGIS layer id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub layer: Option<String>,
    /// An `arcgis` connection whose sign-in secures this service.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth_from: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attribution: Option<String>,
}

/// F-14: policy `map`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MapPolicy {
    /// Web layers at all (default true).
    #[serde(default = "yes")]
    pub allow_web_layers: bool,
    /// Hosts web layers may reach (`*` wildcards). Empty: each source's own
    /// host, once the person saves that source at the keyboard.
    #[serde(default)]
    pub hosts: Vec<String>,
    /// Sources the organization supplies (F-13 entries).
    #[serde(default)]
    pub sources: Vec<MapSource>,
}

fn yes() -> bool {
    true
}

impl Default for MapPolicy {
    fn default() -> Self {
        MapPolicy {
            allow_web_layers: true,
            hosts: vec![],
            sources: vec![],
        }
    }
}

impl MapPolicy {
    /// Whether `host` is on `map.hosts` (only meaningful when it is not
    /// empty).
    pub fn host_listed(&self, host: &str) -> bool {
        crate::policy::DatasetRules {
            allow: self.hosts.clone(),
            deny: vec![],
        }
        .permits(host)
        .unwrap_or(false)
    }
}

/// Letters, digits, `-` and `_`, starting with a letter or digit, at most
/// 64 characters (also a folder name under the tile cache).
pub fn id_ok(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id.chars().next().is_some_and(|c| c.is_ascii_alphanumeric())
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

impl MapSource {
    /// The checks that need no network: required fields, the id, the
    /// placeholders an XYZ template needs, a layer where the kind needs
    /// one, and `auth_from` only for ArcGIS kinds. The URL's scheme and
    /// host are checked by the gateway, with the HTTP client's own parser.
    pub fn check(&self) -> Result<(), String> {
        if !id_ok(&self.id) {
            return Err(format!(
                "web layer id {:?} must be letters, digits, - and _ (at most 64), starting with a letter or digit",
                self.id
            ));
        }
        let id = &self.id;
        if self.title.trim().is_empty() || self.title.len() > 200 {
            return Err(format!(
                "web layer {id} needs a title (at most 200 characters)"
            ));
        }
        if self.title.chars().any(char::is_control) {
            return Err(format!("web layer {id}'s title has control characters"));
        }
        let url = self.url.trim();
        if url.is_empty() {
            return Err(format!("web layer {id} needs a url"));
        }
        if !(url.starts_with("https://") || url.starts_with("http://")) {
            return Err(format!("web layer {id}'s url must start with https://"));
        }
        if url.chars().any(|c| c.is_whitespace() || c.is_control()) {
            return Err(format!(
                "web layer {id}'s url has spaces or control characters"
            ));
        }
        let layer = self
            .layer
            .as_deref()
            .map(str::trim)
            .filter(|l| !l.is_empty());
        match self.kind {
            MapKind::Xyz => {
                for p in ["{z}", "{x}", "{y}"] {
                    if !url.contains(p) {
                        return Err(format!(
                            "web layer {id} is XYZ tiles: its url must hold {{z}}, {{x}} and {{y}}"
                        ));
                    }
                }
                let rest = url.replace("{z}", "").replace("{x}", "").replace("{y}", "");
                if rest.contains('{') || rest.contains('}') {
                    return Err(format!(
                        "web layer {id}'s url may hold only the placeholders {{z}}, {{x}} and {{y}}"
                    ));
                }
            }
            MapKind::Wms | MapKind::Wmts => {
                if layer.is_none() {
                    return Err(format!(
                        "web layer {id} is {}: choose one of its layers (layer)",
                        self.kind.as_str().to_uppercase()
                    ));
                }
            }
            MapKind::ArcgisFeatures => {
                let last = url
                    .split(['?', '#'])
                    .next()
                    .unwrap_or("")
                    .trim_end_matches('/')
                    .rsplit('/')
                    .next()
                    .unwrap_or("");
                let numbered = !last.is_empty() && last.bytes().all(|b| b.is_ascii_digit());
                if !numbered && layer.is_none() {
                    return Err(format!(
                        "web layer {id} is an ArcGIS feature layer: choose one of the service's layers (layer)"
                    ));
                }
            }
            MapKind::ArcgisMap | MapKind::ArcgisImage => {}
        }
        if let Some(l) = layer {
            if l.len() > 200 || l.chars().any(char::is_control) {
                return Err(format!("web layer {id}'s layer is not a layer name"));
            }
            if self.kind.arcgis() && !l.bytes().all(|b| b.is_ascii_digit()) {
                return Err(format!(
                    "web layer {id}'s layer must be the ArcGIS layer's number"
                ));
            }
        }
        if let Some(a) = &self.auth_from {
            if !self.kind.arcgis() {
                return Err(format!(
                    "web layer {id}: auth_from (an arcgis connection's sign-in) works only with ArcGIS services"
                ));
            }
            if a.trim().is_empty() {
                return Err(format!("web layer {id}'s auth_from is empty"));
            }
        }
        if let Some(a) = &self.attribution {
            if a.len() > 500 || a.chars().any(|c| c.is_control() && c != '\n') {
                return Err(format!(
                    "web layer {id}'s attribution is too long or has control characters"
                ));
            }
        }
        Ok(())
    }

    /// What, if changed, makes the source a different one: its cached
    /// tiles are then dropped.
    pub fn fingerprint(&self) -> String {
        use sha2::Digest;
        let text = serde_json::json!([
            self.kind.as_str(),
            self.url.trim(),
            self.layer.as_deref().map(str::trim),
            self.auth_from,
        ])
        .to_string();
        hex::encode(sha2::Sha256::digest(text.as_bytes()))
    }
}

/// The saved sources' folder: `COLUMBUS_MAP_SOURCES`, else `map-sources`
/// beside the connections folder.
pub fn map_sources_dir() -> PathBuf {
    if let Some(d) = crate::paths::env_path("COLUMBUS_MAP_SOURCES") {
        return d;
    }
    beside(&crate::paths::connections_dir())
}

/// `map-sources` beside a connections folder.
pub fn beside(connections: &Path) -> PathBuf {
    connections
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| connections.to_path_buf())
        .join("map-sources")
}

/// The file of saved source `id`.
pub fn file(dir: &Path, id: &str) -> PathBuf {
    dir.join(format!("{id}.yaml"))
}

/// Parse one saved source's text; its id must match the file name.
pub fn parse(text: &str, file_id: &str) -> Result<MapSource, String> {
    let s: MapSource = serde_yaml::from_str(text).map_err(|e| e.to_string())?;
    if s.id != file_id {
        return Err(format!(
            "its id is {:?} but the file is named {file_id}.yaml",
            s.id
        ));
    }
    s.check()?;
    Ok(s)
}

/// Read one saved source; None when there is no such file.
pub fn load_one(dir: &Path, id: &str) -> Result<Option<MapSource>, String> {
    if !id_ok(id) {
        return Ok(None);
    }
    let p = file(dir, id);
    match std::fs::symlink_metadata(&p) {
        Err(_) => return Ok(None),
        Ok(m) if !m.is_file() => return Err(format!("{} is not a plain file", p.display())),
        Ok(_) => {}
    }
    let text = std::fs::read_to_string(&p).map_err(|e| format!("{}: {e}", p.display()))?;
    parse(&text, id)
        .map(Some)
        .map_err(|e| format!("{}: {e}", p.display()))
}

/// Every saved source, by id, and the files that could not be read (one
/// bad file never hides the others).
pub fn load_dir(dir: &Path) -> (Vec<MapSource>, Vec<String>) {
    let mut out = Vec::new();
    let mut problems = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return (out, problems);
    };
    let mut names: Vec<String> = entries
        .flatten()
        .filter_map(|e| e.file_name().to_str().map(String::from))
        .filter(|n| n.ends_with(".yaml") && !n.starts_with('.'))
        .collect();
    names.sort();
    for n in names.into_iter().take(MAX_SOURCES) {
        let id = n.trim_end_matches(".yaml");
        match load_one(dir, id) {
            Ok(Some(s)) => out.push(s),
            Ok(None) => problems.push(format!("{n}: not a usable web layer file name")),
            Err(e) => problems.push(e),
        }
    }
    (out, problems)
}

/// One source's approved capability hosts.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct HostsRecord {
    /// The url they were read from; a changed url voids the record.
    pub url: String,
    pub hosts: Vec<String>,
}

pub fn read_hosts(dir: &Path) -> BTreeMap<String, HostsRecord> {
    std::fs::read(dir.join(HOSTS_FILE))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

/// The hosts approved for `source`, if its record is for its current url.
pub fn approved_hosts(dir: &Path, source: &MapSource) -> Vec<String> {
    read_hosts(dir)
        .remove(&source.id)
        .filter(|r| r.url == source.url.trim())
        .map(|r| r.hosts)
        .unwrap_or_default()
}

fn write_hosts(dir: &Path, all: &BTreeMap<String, HostsRecord>) -> Result<(), String> {
    let p = dir.join(HOSTS_FILE);
    if all.is_empty() {
        return match std::fs::remove_file(&p) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                Err(format!("{}: {e}", p.display()))
            }
            _ => Ok(()),
        };
    }
    let bytes = serde_json::to_vec_pretty(all).map_err(|e| e.to_string())?;
    crate::tools::write_atomic(&p, &bytes)
}

/// Record (or, with no hosts, clear) the capability hosts approved for
/// `source` when it was saved.
pub fn record_hosts(dir: &Path, source: &MapSource, hosts: &[String]) -> Result<(), String> {
    let mut all = read_hosts(dir);
    if hosts.is_empty() {
        all.remove(&source.id);
    } else {
        all.insert(
            source.id.clone(),
            HostsRecord {
                url: source.url.trim().to_string(),
                hosts: hosts.to_vec(),
            },
        );
    }
    write_hosts(dir, &all)
}

/// Forget `id`'s record.
pub fn forget_hosts(dir: &Path, id: &str) -> Result<(), String> {
    let mut all = read_hosts(dir);
    if all.remove(id).is_none() {
        return Ok(());
    }
    write_hosts(dir, &all)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn src(yaml: &str) -> Result<MapSource, String> {
        let s: MapSource = serde_yaml::from_str(yaml).map_err(|e| e.to_string())?;
        s.check().map(|_| s)
    }

    #[test]
    fn f13_example_reads() {
        let s = src("{id: county-imagery, title: County imagery 2025, kind: wmts, url: https://imagery.example.gov/wmts/1.0.0/WMTSCapabilities.xml, layer: ortho2025}").unwrap();
        assert_eq!(s.kind, MapKind::Wmts);
        assert_eq!(s.layer.as_deref(), Some("ortho2025"));
    }

    #[test]
    fn f13_refuses_unknown_fields_and_bad_values() {
        assert!(src(
            "{id: a, title: A, kind: xyz, url: 'https://t.example/{z}/{x}/{y}.png', hosts: [x]}"
        )
        .is_err());
        assert!(
            src("{id: a, title: A, kind: tiles, url: 'https://t.example/{z}/{x}/{y}.png'}")
                .is_err()
        );
        assert!(src("{id: a, title: A, kind: xyz, url: 'https://t.example/{z}/{x}.png'}").is_err());
        assert!(
            src("{id: a, title: A, kind: xyz, url: 'https://{s}.t.example/{z}/{x}/{y}.png'}")
                .is_err()
        );
        assert!(src("{id: a, title: A, kind: wms, url: 'https://w.example/wms'}").is_err());
        assert!(src(
            "{id: a, title: A, kind: wms, url: 'https://w.example/wms', layer: x, auth_from: c}"
        )
        .is_err());
        assert!(src("{id: a, title: A, kind: arcgis-features, url: 'https://s.example/arcgis/rest/services/P/FeatureServer'}").is_err());
        assert!(src("{id: a, title: A, kind: arcgis-features, url: 'https://s.example/arcgis/rest/services/P/FeatureServer/0'}").is_ok());
        assert!(src("{id: a, title: A, kind: arcgis-features, url: 'https://s.example/arcgis/rest/services/P/FeatureServer', layer: roads}").is_err());
        assert!(src("{id: ../x, title: A, kind: arcgis-map, url: 'https://s.example/arcgis/rest/services/P/MapServer'}").is_err());
        assert!(src("{id: a, title: '', kind: arcgis-map, url: 'https://s.example/arcgis/rest/services/P/MapServer'}").is_err());
    }

    #[test]
    fn f14_defaults_and_hosts() {
        let p: MapPolicy = serde_yaml::from_str("{}").unwrap();
        assert!(p.allow_web_layers && p.hosts.is_empty() && p.sources.is_empty());
        let p: MapPolicy = serde_yaml::from_str(
            "{allow_web_layers: true, hosts: [\"*.arcgis.com\", \"imagery.example.gov\"]}",
        )
        .unwrap();
        assert!(p.host_listed("services3.arcgis.com"));
        assert!(p.host_listed("IMAGERY.example.gov"));
        assert!(!p.host_listed("imagery.example.gov.evil.example"));
        assert!(serde_yaml::from_str::<MapPolicy>("{host: [x]}").is_err());
    }

    #[test]
    fn hosts_record_is_tied_to_the_url() {
        let dir = tempfile::tempdir().unwrap();
        let mut s =
            src("{id: a, title: A, kind: wmts, url: 'https://w.example/caps.xml', layer: l}")
                .unwrap();
        record_hosts(dir.path(), &s, &["cdn.example".into()]).unwrap();
        assert_eq!(
            approved_hosts(dir.path(), &s),
            vec!["cdn.example".to_string()]
        );
        s.url = "https://other.example/caps.xml".into();
        assert!(approved_hosts(dir.path(), &s).is_empty());
        forget_hosts(dir.path(), "a").unwrap();
        assert!(!dir.path().join(HOSTS_FILE).exists());
    }
}
