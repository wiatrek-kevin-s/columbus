# Sets up Columbus's GIS engine: a Python 3.12 environment with GDAL, PROJ,
# GeoPandas, rasterio and the other packages in requirements-gis.txt, and the
# database drivers in requirements-db.txt (PostgreSQL, Oracle in thin mode,
# SQL Server through pyodbc), in engine\venv beside columbus-mcp.exe, where
# the server finds it. SQL Server also needs Microsoft ODBC Driver 18 for
# SQL Server installed on the machine; the script says whether it is.
#
#   powershell -ExecutionPolicy Bypass -File engine\setup-engine.ps1
#
# The packages are wheels that bundle GDAL and PROJ, so no separate GDAL
# install is needed. Every package is pinned by exact version and SHA-256
# and installed with --require-hashes, so a file that differs from the
# pinned one is refused. Needs Python 3.12 (the python.org installer, or
# `winget install Python.Python.3.12`) and access to PyPI, or a wheelhouse
# folder given with -Wheels for offline installs (it must also hold the exact
# DuckDB wheels requirements-projectdb.txt pins by hash, and the uv wheel
# requirements-uv.txt pins).
param(
    [string]$Python = "",
    [string]$Wheels = ""
)
$ErrorActionPreference = "Stop"
$here = Split-Path -Parent $MyInvocation.MyCommand.Path
$root = Split-Path -Parent $here
$venv = Join-Path $here "venv"
$req = Join-Path $root "workers\python\requirements-gis.txt"
$db = Join-Path $root "workers\python\requirements-db.txt"
# Hash-pinned, installed on their own (see the file).
$pdb = Join-Path $root "workers\python\requirements-projectdb.txt"
# Columbus's own uv, for environments the person approves (hash-pinned).
$uvreq = Join-Path $root "workers\python\requirements-uv.txt"

if (-not $Python) {
    $py = Get-Command py -ErrorAction SilentlyContinue
    if ($py) { $Python = "py -3.12" } else { $Python = "python" }
}
Write-Host "Creating $venv with $Python"
if ($Python -like "py *") {
    & py -3.12 -m venv $venv
    if ($LASTEXITCODE -ne 0) { throw "setup stopped: creating the environment failed (exit $LASTEXITCODE)" }
} else {
    & $Python -m venv $venv
    if ($LASTEXITCODE -ne 0) { throw "setup stopped: creating the environment failed (exit $LASTEXITCODE)" }
}
$vpy = Join-Path $venv "Scripts\python.exe"
& $vpy -m pip install --upgrade pip
if ($LASTEXITCODE -ne 0) { throw "setup stopped: pip install failed (exit $LASTEXITCODE)" }
if ($Wheels) {
    & $vpy -m pip install --no-index --find-links $Wheels --require-hashes -r $req -r $db
    if ($LASTEXITCODE -ne 0) { throw "setup stopped: pip install failed (exit $LASTEXITCODE)" }
    & $vpy -m pip install --no-index --find-links $Wheels --require-hashes --no-deps --only-binary :all: -r $pdb
    if ($LASTEXITCODE -ne 0) { throw "setup stopped: the hash-checked DuckDB install (requirements-projectdb.txt) failed (exit $LASTEXITCODE)" }
    & $vpy -m pip install --no-index --find-links $Wheels --require-hashes --no-deps --only-binary :all: -r $uvreq
    if ($LASTEXITCODE -ne 0) { throw "setup stopped: the hash-checked uv install (requirements-uv.txt) failed (exit $LASTEXITCODE)" }
} else {
    & $vpy -m pip install --require-hashes -r $req -r $db
    if ($LASTEXITCODE -ne 0) { throw "setup stopped: pip install failed (exit $LASTEXITCODE)" }
    & $vpy -m pip install --require-hashes --no-deps --only-binary :all: -r $pdb
    if ($LASTEXITCODE -ne 0) { throw "setup stopped: the hash-checked DuckDB install (requirements-projectdb.txt) failed (exit $LASTEXITCODE)" }
    & $vpy -m pip install --require-hashes --no-deps --only-binary :all: -r $uvreq
    if ($LASTEXITCODE -ne 0) { throw "setup stopped: the hash-checked uv install (requirements-uv.txt) failed (exit $LASTEXITCODE)" }
}
& $vpy -c "import geopandas, pyogrio, shapely, pyproj, rasterio; print('engine ready: GDAL', pyogrio.__gdal_version_string__)"
if ($LASTEXITCODE -ne 0) { throw "setup stopped: the check failed (exit $LASTEXITCODE)" }
# The spatial extension is found as a file; its package is never imported.
& $vpy -I -c "import importlib.util, os, duckdb; s = importlib.util.find_spec('duckdb_extension_spatial'); p = s and os.path.join(os.path.dirname(s.origin), 'extensions', 'v' + duckdb.__version__, 'spatial.duckdb_extension'); print('project databases ready: DuckDB', duckdb.__version__, 'with the spatial extension' if p and os.path.isfile(p) else 'WITHOUT the spatial extension (spatial SQL functions unavailable)')"
if ($LASTEXITCODE -ne 0) { throw "setup stopped: the check failed (exit $LASTEXITCODE)" }
& $vpy -c "import psycopg, oracledb, pyodbc; print('database drivers ready: oracledb', oracledb.__version__, '(thin mode), pyodbc', pyodbc.version)"
if ($LASTEXITCODE -ne 0) { throw "setup stopped: the check failed (exit $LASTEXITCODE)" }
$odbc = & $vpy -c "import pyodbc; print('ODBC Driver 18 for SQL Server' in pyodbc.drivers())"
if ($odbc -ne "True") {
    Write-Host "Note: ODBC Driver 18 for SQL Server is not installed; SQL Server connections need it (https://learn.microsoft.com/sql/connect/odbc/download-odbc-driver-for-sql-server)."
}
Write-Host ""
Write-Host "Done. columbus-mcp finds this environment by itself when it runs from this folder."
Write-Host "If you install the .mcpb extension in Claude desktop, paste this as its Engine Python:"
Write-Host "  $vpy"
