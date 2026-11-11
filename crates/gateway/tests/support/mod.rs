//! Shared by the integration tests: finding a Python that has what a test
//! needs, and failing instead of skipping when CI has installed the GIS
//! engine (REQ-22); and asking whether this machine can set the sandbox
//! up, failing instead of skipping where CI says tests must not skip.

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

/// Whether tests must not skip: `COLUMBUS_TEST_REQUIRE_ENGINE=1`, set in
/// every CI job that runs the gateway's tests, on every system.
#[allow(dead_code)]
pub fn must_not_skip() -> bool {
    std::env::var(REQUIRE_ENGINE).is_ok_and(|v| v == "1")
}

/// Whether this machine can set the sandbox up at level A
/// (`<launcher> --check A`). A test that proves something about the
/// sandbox skips where it cannot, unless tests must not skip: there, a
/// machine that cannot set the sandbox up fails the test, on Windows, macOS
/// and Linux alike. A proof of the sandbox must never pass by not running
/// (final review of part 3, R-104).
#[allow(dead_code)]
pub fn can_confine(launcher: &std::path::Path) -> bool {
    sandbox_or_fail(launcher, must_not_skip())
}

/// The same, told whether tests must not skip.
#[allow(dead_code)]
pub fn sandbox_or_fail(launcher: &std::path::Path, must_not_skip: bool) -> bool {
    let out = Command::new(launcher).args(["--check", "A"]).output();
    let ok = out.as_ref().is_ok_and(|o| o.status.success());
    if !ok && must_not_skip {
        let said = match &out {
            Ok(o) => String::from_utf8_lossy(&o.stderr).trim().to_string(),
            Err(e) => e.to_string(),
        };
        panic!(
            "{REQUIRE_ENGINE}=1, so this test must not skip, but this machine cannot set up the \
             sandbox at level A: `{} --check A` failed ({said}). The sandbox is AppContainer on \
             Windows, sandbox-exec on macOS, Landlock and seccomp on Linux. Unset {REQUIRE_ENGINE} \
             to let sandbox tests skip",
            launcher.display()
        );
    }
    ok
}
