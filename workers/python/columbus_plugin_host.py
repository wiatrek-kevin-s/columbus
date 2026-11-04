"""Starts a Python engine plugin for the gateway.

    python -I -u columbus_plugin_host.py <plugin folder> <entry script>

Isolated mode (-I) keeps PYTHON* variables and the user's site folder out.
This launcher then puts exactly two folders first on the import path: the
plugin's own folder and the SDK's folder (both installed by an
administrator), and runs the entry script as __main__.
"""

import os
import runpy
import sys


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
    runpy.run_path(entry, run_name="__main__")


if __name__ == "__main__":
    main()
