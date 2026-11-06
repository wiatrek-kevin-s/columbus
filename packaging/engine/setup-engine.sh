#!/bin/sh
# Sets up Columbus's GIS engine on macOS (and Linux): a Python 3.12
# environment with GDAL, PROJ, GeoPandas, rasterio and the other packages in
# requirements-gis.txt, and the database drivers in requirements-db.txt
# (PostgreSQL, Oracle in thin mode, SQL Server through pyodbc), in
# engine/venv beside columbus-mcp, where the server finds it.
#
#   sh engine/setup-engine.sh
#   sh engine/setup-engine.sh --python /path/to/python3.12 --wheels /path/to/wheels
#
# The packages are wheels that bundle GDAL and PROJ, so no separate GDAL
# install is needed. Needs Python 3.12 (the python.org installer, or
# `brew install python@3.12`) and access to PyPI, or a folder of downloaded
# wheels given with --wheels for offline installs.
set -eu

here=$(cd "$(dirname "$0")" && pwd)
root=$(dirname "$here")
venv="$here/venv"
req="$root/workers/python/requirements-gis.txt"
db="$root/workers/python/requirements-db.txt"
python=""
wheels=""

while [ $# -gt 0 ]; do
    case "$1" in
        --python) python="$2"; shift 2 ;;
        --wheels) wheels="$2"; shift 2 ;;
        *) echo "unknown option $1 (expected --python or --wheels)" >&2; exit 2 ;;
    esac
done

if [ -z "$python" ]; then
    for candidate in python3.12 /opt/homebrew/bin/python3.12 /usr/local/bin/python3.12; do
        if command -v "$candidate" >/dev/null 2>&1; then
            python=$(command -v "$candidate")
            break
        fi
    done
fi
if [ -z "$python" ]; then
    echo "Python 3.12 was not found. Install it from python.org, or with: brew install python@3.12" >&2
    echo "Then run this again, or pass --python /path/to/python3.12." >&2
    exit 1
fi

echo "Creating $venv with $python"
"$python" -m venv "$venv"
vpy="$venv/bin/python3"
"$vpy" -m pip install --upgrade pip
if [ -n "$wheels" ]; then
    "$vpy" -m pip install --no-index --find-links "$wheels" -r "$req" -r "$db"
else
    "$vpy" -m pip install -r "$req" -r "$db"
fi

"$vpy" -c "import geopandas, pyogrio, shapely, pyproj, rasterio; print('engine ready: GDAL', pyogrio.__gdal_version_string__)"
"$vpy" -c "import duckdb, duckdb_extension_spatial; print('project databases ready: DuckDB', duckdb.__version__, 'with the spatial extension')"
"$vpy" -c "import psycopg, oracledb; print('database drivers ready: PostgreSQL, Oracle', oracledb.__version__, '(thin mode)')"
# pyodbc needs unixODBC, and SQL Server needs Microsoft's ODBC driver; both
# are separate installs, needed only for SQL Server connections.
if ! "$vpy" -c "import pyodbc; raise SystemExit(0 if 'ODBC Driver 18 for SQL Server' in pyodbc.drivers() else 1)" 2>/dev/null; then
    echo "Note: SQL Server connections also need unixODBC and Microsoft ODBC Driver 18 for SQL Server"
    echo "(https://learn.microsoft.com/sql/connect/odbc/linux-mac/install-microsoft-odbc-driver-sql-server-macos)."
    echo "Everything else works without them."
fi

echo
echo "Done. columbus-mcp finds this environment by itself when it runs from this folder."
echo "If you install the .mcpb extension in Claude desktop, paste this as its Engine Python:"
echo "  $vpy"
