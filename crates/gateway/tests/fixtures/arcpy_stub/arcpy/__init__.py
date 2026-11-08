"""A stub of Esri's arcpy, for Columbus's tests only (T-8, REQ-16).

It is never shipped or installed by Columbus: tests copy this folder into
a throwaway venv's site-packages. What ProductInfo() returns is the first
line of stub_product.txt beside this file (default "ArcInfo"). The line
"sleep:<seconds>" makes importing it hang instead, after starting a child
process that touches heartbeat.txt (path on the second line) every 0.2 s,
so a test can tell that the whole process tree was stopped.
"""
import os as _os
import sys as _sys
import time as _time

_here = _os.path.dirname(_os.path.abspath(__file__))
try:
    with open(_os.path.join(_here, "stub_product.txt"), encoding="utf-8") as _f:
        _lines = _f.read().splitlines()
except OSError:
    _lines = []
_product = (_lines[0].strip() if _lines else "") or "ArcInfo"

if _product.startswith("sleep:"):
    import subprocess as _subprocess

    if len(_lines) > 1:
        _beat = _lines[1].strip()
        _subprocess.Popen(
            [
                _sys.executable,
                "-I",
                "-c",
                "import sys, time\n"
                "while True:\n"
                "    open(sys.argv[1], 'a').write('.')\n"
                "    time.sleep(0.2)\n",
                _beat,
            ],
            stdin=_subprocess.DEVNULL,
            stdout=_subprocess.DEVNULL,
            stderr=_subprocess.DEVNULL,
        )
    _time.sleep(float(_product.split(":", 1)[1]))


def ProductInfo():
    return _product
