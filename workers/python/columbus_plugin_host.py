"""Starts a Python engine plugin for the gateway.

    python -I -u columbus_plugin_host.py <plugin folder> <entry script>

Isolated mode (-I) keeps PYTHON* variables and the user's site folder out.
This launcher then puts exactly two folders first on the import path: the
plugin's own folder and the SDK's folder (both installed by an
administrator), and runs the entry script as __main__.

The entry is read as UTF-8 text and compiled from that text, never handed
to Python to decode: a coding declaration (``# coding: utf-7``) would make
Python decode the file into other code than a person reads.
"""

import os
import sys
import types


def main() -> None:
    if len(sys.argv) != 3:
        sys.stderr.write(__doc__ or "")
        sys.exit(2)
    sdk = os.path.dirname(os.path.abspath(__file__))
    plugin_dir = os.path.abspath(sys.argv[1])
    entry = os.path.abspath(os.path.join(plugin_dir, sys.argv[2]))
    if os.path.commonpath([plugin_dir, entry]) != plugin_dir or not os.path.isfile(entry):
        sys.stderr.write("entry must be a file inside the plugin folder\n")
        sys.exit(2)
    sys.path[0:0] = [plugin_dir, sdk]
    sys.argv = [entry]
    run_text(entry)


def run_text(path: str) -> None:
    """Run a Python file as __main__ from its UTF-8 text, as it reads."""
    with open(path, encoding="utf-8", newline="") as f:
        text = f.read()
    code = compile(text, path, "exec", dont_inherit=True)
    main = types.ModuleType("__main__")
    main.__file__ = path
    main.__builtins__ = __builtins__
    sys.modules["__main__"] = main
    exec(code, main.__dict__)


if __name__ == "__main__":
    main()
