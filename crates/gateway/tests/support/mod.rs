//! Shared by the integration tests: finding a Python that has what a test
//! needs, and failing instead of skipping when CI has installed the GIS
//! engine (REQ-22).

use std::process::Command;

/// Set to `1` in every CI job that installs the GIS engine. A test that
/// would skip because no Python has the engine then fails, naming this
/// variable.
const REQUIRE_ENGINE: &str = "COLUMBUS_TEST_REQUIRE_ENGINE";

/// The GIS engine's own modules. A probe that imports one of these, and no
/// database driver, is an engine probe.
const ENGINE_MODULES: [&str; 7] = [
    "geopandas",
    "pyogrio",
    "shapely",
    "pyproj",
    "rasterio",
    "pyarrow",
    "duckdb",
];

/// Database drivers: tests that need them are gated by their own
/// `COLUMBUS_TEST_*_HOST` variable and fail there when the driver is
/// missing, so a probe naming one never counts as an engine probe (the
/// Rust job installs the engine but not the drivers).
const DB_DRIVERS: [&str; 3] = ["psycopg", "oracledb", "pyodbc"];

/// The first of `COLUMBUS_PYTHON`, `python3` and `python` that runs the
/// Python statement `probe` successfully. When none does and the probe
/// imports the GIS engine, the test skips (the caller prints why), unless
/// `COLUMBUS_TEST_REQUIRE_ENGINE=1`, which makes it fail here.
pub fn python_for(probe: &str) -> Option<String> {
    let found = std::env::var("COLUMBUS_PYTHON")
        .ok()
        .filter(|p| !p.is_empty())
        .into_iter()
        .chain(["python3".to_string(), "python".to_string()])
        .find(|p| {
            Command::new(p)
                .args(["-c", probe])
                .output()
                .is_ok_and(|o| o.status.success())
        });
    let engine = ENGINE_MODULES.iter().any(|m| probe.contains(m))
        && !DB_DRIVERS.iter().any(|d| probe.contains(d));
    if found.is_none() && engine && std::env::var(REQUIRE_ENGINE).is_ok_and(|v| v == "1") {
        panic!(
            "{REQUIRE_ENGINE}=1, so this test must not skip, but the GIS engine is not installed: \
             no Python among COLUMBUS_PYTHON, python3 and python can run `{probe}`. Install \
             workers/python/requirements-gis.txt (see docs/install.md), or unset {REQUIRE_ENGINE} \
             to let engine tests skip"
        );
    }
    found
}
