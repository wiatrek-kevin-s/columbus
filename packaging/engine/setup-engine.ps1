# Sets up Columbus's GIS engine: a Python 3.12 environment with GDAL, PROJ,
# GeoPandas, rasterio and the other packages in requirements-gis.txt, in
# engine\venv beside columbus-mcp.exe, where the server finds it.
#
#   powershell -ExecutionPolicy Bypass -File engine\setup-engine.ps1
#
# The packages are wheels that bundle GDAL and PROJ, so no separate GDAL
# install is needed. Needs Python 3.12 (the python.org installer, or
# `winget install Python.Python.3.12`) and access to PyPI, or a wheelhouse
# folder given with -Wheels for offline installs.
param(
    [string]$Python = "",
    [string]$Wheels = ""
)
$ErrorActionPreference = "Stop"
$here = Split-Path -Parent $MyInvocation.MyCommand.Path
$root = Split-Path -Parent $here
$venv = Join-Path $here "venv"
$req = Join-Path $root "workers\python\requirements-gis.txt"

if (-not $Python) {
    $py = Get-Command py -ErrorAction SilentlyContinue
    if ($py) { $Python = "py -3.12" } else { $Python = "python" }
}
Write-Host "Creating $venv with $Python"
if ($Python -like "py *") {
    & py -3.12 -m venv $venv
} else {
    & $Python -m venv $venv
}
$vpy = Join-Path $venv "Scripts\python.exe"
& $vpy -m pip install --upgrade pip
if ($Wheels) {
    & $vpy -m pip install --no-index --find-links $Wheels -r $req "psycopg[binary]>=3.2"
} else {
    & $vpy -m pip install -r $req "psycopg[binary]>=3.2"
}
& $vpy -c "import geopandas, pyogrio, shapely, pyproj, rasterio; print('engine ready: GDAL', pyogrio.__gdal_version_string__)"
Write-Host "Done. columbus-mcp finds this environment by itself."
