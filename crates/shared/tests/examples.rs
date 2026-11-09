//! The files under `examples/` load with the same parsers Columbus uses, so
//! the examples the docs point to stay in step with the formats (T-16).

use columbus_shared::environments::{Environment, Trust};
use columbus_shared::map_sources;
use columbus_shared::policy::{Connection, OrgPolicy};
use std::path::{Path, PathBuf};

fn examples() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples")
}

#[test]
fn example_policy_has_every_new_key() {
    let path = examples().join("policy.yaml");
    let p = OrgPolicy::load(&path).expect("examples/policy.yaml loads");
    for t in ["files", "arcgis"] {
        assert!(p.connection_types.iter().any(|c| c == t), "{t}");
    }
    // F-7: every key is written out (at its default), and parses.
    let text = std::fs::read_to_string(&path).unwrap();
    for key in [
        "environments:",
        "allow:",
        "allow_trusted:",
        "allow_network:",
        "index_hosts:",
        "scripts:",
        "session_approval:",
        "map:",
        "allow_web_layers:",
        "sources:",
    ] {
        assert!(text.contains(key), "examples/policy.yaml lacks {key}");
    }
    assert!(!p.environments.allow && !p.environments.allow_trusted);
    assert!(!p.environments.allow_network && !p.scripts.session_approval);
    assert_eq!(
        p.environments.index_hosts,
        ["pypi.org", "files.pythonhosted.org"]
    );
    assert!(p.map.allow_web_layers && p.map.hosts.is_empty());
    assert_eq!(p.map.sources.len(), 1);
    for s in &p.map.sources {
        s.check().unwrap();
    }
}

#[test]
fn example_connections_load() {
    let all = Connection::load_dir(&examples().join("connections")).expect("connections load");
    let kind = |id: &str| all.iter().find(|c| c.id == id).map(|c| c.kind.clone());
    assert_eq!(kind("crash-files").as_deref(), Some("files"));
    assert_eq!(kind("roads-agol").as_deref(), Some("arcgis"));
    assert_eq!(kind("row-polygons").as_deref(), Some("arcgis"));
}

#[test]
fn example_map_sources_load() {
    let (sources, problems) = map_sources::load_dir(&examples().join("map-sources"));
    assert!(problems.is_empty(), "{problems:?}");
    let mut ids: Vec<_> = sources.iter().map(|s| s.id.as_str()).collect();
    ids.sort();
    assert_eq!(ids, ["county-imagery", "row-service"]);
}

#[test]
fn example_environment_has_the_f4_shape() {
    // Parsed with the registration's own type (unknown keys refused); the
    // Windows path keeps it from loading as a registration elsewhere.
    let text = std::fs::read_to_string(examples().join("environments/pro.yaml")).unwrap();
    let env: Environment = serde_yaml::from_str(&text).expect("F-4 fields");
    assert_eq!(env.id, "pro");
    assert_eq!(env.trust, Trust::Governed);
}
