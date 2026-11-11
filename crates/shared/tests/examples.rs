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
        "approval:",
        "allow_toolboxes:",
        "toolboxes:",
        "catalogue:",
        "allow_project_files:",
        "allow_sync:",
        "sync:",
        "assistant:",
        "map:",
        "allow_web_layers:",
        "sources:",
    ] {
        assert!(text.contains(key), "examples/policy.yaml lacks {key}");
    }
    assert!(!p.environments.allow && !p.environments.allow_trusted);
    assert!(!p.environments.allow_network && !p.scripts.session_approval);
    // F-15: written out at their defaults too.
    assert_eq!(
        p.scripts.approval,
        Some(columbus_shared::policy::ScriptApproval::Each)
    );
    assert!(!p.allow_toolboxes && p.toolboxes.catalogue);
    assert!(!p.allow_project_files && !p.allow_sync && p.sync.hosts.is_empty());
    // F-19: written out at its default (any AI service the person sets up).
    assert!(p.assistant.hosts.is_empty());
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

/// The toolbox example (F-16): its description file passes the same checks
/// `columbus-mcp toolbox add` runs, and its `toolbox.yaml` has the keys
/// Columbus writes and no other.
#[test]
fn example_toolbox_reads_as_columbus_reads_one() {
    use columbus_shared::toolboxes::{self as tbx, Kind, Role};
    let dir = examples().join("toolboxes").join("linetools");
    let tools = tbx::parse_described(&std::fs::read(dir.join("tools.json")).unwrap())
        .expect("examples/toolboxes/linetools/tools.json checks");
    let ids: Vec<&str> = tools.iter().map(|t| t.id.as_str()).collect();
    assert_eq!(ids, ["densify", "length_report"]);
    let densify = &tools[0];
    assert_eq!(densify.param("lines").unwrap().role, Role::Input);
    assert_eq!(densify.param("densified").unwrap().output_ext(), "gpkg");
    assert_eq!(tools[1].param("report").unwrap().output_ext(), "csv");
    // Its command line, with the values a run would give it.
    let line = tbx::command_line(
        Kind::Described,
        densify,
        &[
            ("lines".to_string(), "in/lines.gpkg".to_string()),
            ("densified".to_string(), "out/densified.gpkg".to_string()),
            ("spacing".to_string(), "25".to_string()),
        ],
    );
    assert_eq!(
        line,
        [
            "densify",
            "--in=in/lines.gpkg",
            "--out=out/densified.gpkg",
            "--spacing=25"
        ]
    );
    // toolbox.yaml: every key known (unknown ones are refused). Its program
    // is a Windows path, so the path check is left to the system it is for.
    let text = std::fs::read_to_string(dir.join("toolbox.yaml")).unwrap();
    let tb: tbx::Toolbox = serde_yaml::from_str(&text).expect("toolbox.yaml reads");
    assert!(tbx::id_ok(&tb.id) && tb.id == "linetools");
    assert_eq!((tb.kind, tb.version.as_str()), (Kind::Described, "2.1"));
    assert!(tb.sha256.len() == 64 && tb.tools_sha256.is_some_and(|h| h.len() == 64));
}
