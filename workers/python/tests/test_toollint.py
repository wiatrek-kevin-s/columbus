"""Tests for the checks on a proposed personal tool's code. Standard library only."""

import os
import sys
import textwrap
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.dirname(HERE))

from columbus_toollint import lint_tool  # noqa: E402

REPO = os.path.dirname(os.path.dirname(os.path.dirname(HERE)))


def rules(code):
    r = lint_tool(textwrap.dedent(code))
    return {e["rule"] for e in r["errors"]}, {w["rule"] for w in r["warnings"]}, r["uses"]


class ToolLintTest(unittest.TestCase):
    def test_the_example_tool_is_clean(self):
        with open(os.path.join(REPO, "examples", "tools", "locate-measures", "code.py"), encoding="utf-8") as f:
            r = lint_tool(f.read())
        self.assertEqual(r["errors"], [])
        self.assertEqual(r["warnings"], [])
        self.assertEqual(r["uses"], {"inputs": ["points", "routes"], "params": ["max_distance", "measure_unit", "route_field"],
                                     "outputs": ["located"], "checks": ["lrs.off_route"]})

    def test_the_inverse_example_is_clean(self):
        with open(os.path.join(REPO, "examples", "tools", "place-measures", "code.py"), encoding="utf-8") as f:
            r = lint_tool(f.read())
        self.assertEqual((r["errors"], r["warnings"]), ([], []))
        self.assertEqual(r["uses"]["checks"], ["lrs.out_of_range", "lrs.unknown_route"])

    def test_network_processes_and_native_code_are_refused(self):
        for code in ["import socket", "import urllib.request", "from requests import get", "import subprocess",
                     "import ctypes", "from http import client", "import importlib", "import pickle", "import getpass"]:
            errors, _, _ = rules(code + "\ndef run(ctx):\n    return {}\n")
            self.assertIn("tool.import", errors, code)

    def test_environment_credentials_paths_and_urls_are_refused(self):
        cases = {
            "x = os.environ['HOME']": "tool.environment",
            "x = os.getenv('PGPASSWORD')": "tool.environment",
            "os.system('ls')": "tool.process",
            "open('/etc/passwd')": "tool.path",
            "open(r'C:\\\\Users\\\\me\\\\secret.txt')": "tool.path",
            "open('~/x')": "tool.path",
            "u = 'https://example.com/data.json'": "tool.url",
            "u = '/vsicurl/x'": "tool.url",
            "password = 'hunter22'": "tool.credential",
            "f(api_key='abcd1234')": "tool.credential",
            "k = 'AKIAABCDEFGHIJKLMNOP'": "tool.credential",
            "eval('1')": "tool.call",
            "ctx.dataset('routes')": "tool.dataset",
        }
        for line, rule in cases.items():
            errors, _, _ = rules(f"import os\ndef run(ctx):\n    {line}\n    return {{}}\n")
            self.assertIn(rule, errors, line)

    def test_code_reads_as_it_runs(self):
        # Python folds full-width and other look-alike letters into ASCII:
        # the tree says run where the text says something else.
        self.assertIn("tool.identifier", rules("def \uff52\uff55\uff4e(ctx):\n    return {}\n")[0])
        self.assertIn("tool.identifier", rules("def run(ctx):\n    \u0430 = 1\n    return {}\n")[0])
        self.assertIn("tool.identifier", rules("def run(ctx):\n    return ctx.p\u0430ram('x')\n")[0])
        self.assertIn("tool.identifier", rules("def run(ctx):\n    return dict(\u0430=1)\n")[0])
        # A reader sees the first run; Python runs the last.
        self.assertIn("tool.redefined", rules("def run(ctx):\n    return 1\n\ndef run(ctx):\n    return 2\n")[0])
        self.assertIn("tool.top_level", rules("def run(ctx):\n    return 1\nif True:\n    def run(ctx):\n        return 2\n")[0])
        self.assertIn("tool.top_level", rules("def run(ctx):\n    return 1\nfor run in [print]:\n    pass\n")[0])
        self.assertIn("tool.redefined", rules("def helper():\n    pass\nfrom os import path as helper\ndef run(ctx):\n    return 1\n")[0])
        self.assertIn("tool.redefined", rules("def run(ctx):\n    global run\n    return 1\n")[0])
        self.assertIn("tool.call", rules("def run(ctx):\n    globals()['run'] = print\n")[0])
        # Code pushed out of view by blank lines.
        self.assertIn("tool.blank_lines", rules("def run(ctx):\n    x = 1\n" + "\n" * 6 + "    return x\n")[0])
        self.assertEqual(rules("def run(ctx):\n    x = 1\n" + "\n" * 5 + "    return x\n")[0], set())

    def test_run_cannot_be_swapped(self):
        swaps = {
            "class C:\n    run.__code__ = _h.__code__\n": "tool.class",
            "def helper(x=run.__globals__.update(run=_h)):\n    pass\n": "tool.default",
            "def helper(x: run.__globals__.update(run=_h)):\n    pass\n": "tool.annotation",
            "def helper() -> run.__globals__.update(run=_h):\n    pass\n": "tool.annotation",
            "@_d\ndef helper():\n    pass\n": "tool.decorator",
            "class M(type):\n    pass\nclass C(metaclass=M):\n    pass\n": "tool.class",
            "class C(_base()):\n    pass\n": "tool.class",
            "X = [run.__globals__.update(run=_h)]\n": "tool.top_level",
            "X = print('loading')\n": "tool.top_level",
            "LIMIT = 1\nLIMIT = 1000\n": "tool.redefined",
            "def later(ctx):\n    global LIMIT\n    LIMIT = 9\n": "tool.state",
            "def later(ctx):\n    import sys\n    sys.modules[__name__].run = _h\n": "tool.introspection",
            "def later(ctx):\n    getattr(run, '__setattr__')('x', 1)\n": "tool.introspection",
            "def later(ctx):\n    object.__setattr__(run, '__code__', _h.__code__)\n": "tool.introspection",
            "def later(ctx):\n    return run.__defaults__, run.__closure__, run.__kwdefaults__, run.__dict__, run.__class__\n": "tool.introspection",
            "def later(ctx):\n    return __builtins__\n": "tool.introspection",
            "def later(ctx):\n    import __main__\n    __main__.run = _h\n": "tool.import",
            "def later(ctx):\n    import inspect\n    return inspect.currentframe().f_globals\n": "tool.import",
            "import builtins\n": "tool.import",
        }
        for extra, rule in swaps.items():
            code = "def _h(ctx):\n    return {'evil': 1}\n\n\ndef run(ctx):\n    return {}\n\n\n" + extra
            self.assertIn(rule, rules(code)[0], extra)
        self.assertIn("tool.decorator", rules("def _d(f):\n    return f\n\n\n@_d\ndef run(ctx):\n    return {}\n")[0])
        self.assertIn("tool.decorator", rules("import functools\n\n\n@functools.cache\ndef run(ctx):\n    return {}\n")[0])
        # A decorator the file defines itself, under an allowed name.
        self.assertIn("tool.decorator", rules("def staticmethod(f):\n    return f\n\n\n@staticmethod\ndef helper():\n    pass\n\n\ndef run(ctx):\n    return {}\n")[0])
        self.assertIn("tool.decorator", rules("from mylib import thing as dataclass\n\n\n@dataclass\nclass C:\n    pass\n\n\ndef run(ctx):\n    return {}\n")[0])

    def test_what_is_allowed_at_the_top_level(self):
        ok = (
            '"""Doc."""\nimport math\nfrom functools import lru_cache\n\nLIMIT = 10\nUNITS = {"mile": 1609.344, "foot": [0.3048, -1]}\n'
            "NAMES: tuple = ('a', 'b')\n\n\nclass Pair:\n    'Two.'\n    size = 2\n\n    def total(self, a: int, b: int = 0) -> int:\n        return a + b\n\n\n"
            "@lru_cache(maxsize=8)\ndef helper(x: float | None = None) -> float:\n    return math.sqrt(x or 0)\n\n\ndef run(ctx):\n    return {'n': helper(LIMIT)}\n"
        )
        errors, _, _ = rules(ok)
        self.assertEqual(errors, set())

    def test_long_code_and_comment_runs_are_refused(self):
        self.assertIn("tool.length", rules("def run(ctx):\n" + "    x = 1\n" * 2000 + "    return {}\n")[0])
        self.assertIn("tool.comment_lines", rules("def run(ctx):\n" + "    # note\n" * 41 + "    return {}\n")[0])
        self.assertEqual(rules("def run(ctx):\n" + "    # note\n" * 40 + "    return {}\n")[0], set())

    def test_unreadable_tokens_are_an_error(self):
        from columbus_toollint import _Lint, _source_checks
        lint = _Lint()
        _source_checks("def run(ctx):\n    return (1,\n", lint)
        self.assertIn("tool.syntax", {e["rule"] for e in lint.errors})

    def test_run_must_be_there_and_take_ctx(self):
        self.assertIn("tool.run", rules("def main(ctx):\n    pass\n")[0])
        self.assertIn("tool.run", rules("def run(ctx, other):\n    pass\n")[0])
        self.assertIn("tool.run", rules("async def run(ctx):\n    pass\n")[0])
        self.assertIn("tool.syntax", rules("def run(ctx)\n    pass\n")[0])

    def test_columbus_adds_the_sdk_and_the_main_block(self):
        self.assertIn("tool.sdk", rules("from columbus_plugin import Plugin\ndef run(ctx):\n    pass\n")[0])
        self.assertIn("tool.main", rules("def run(ctx):\n    pass\nif __name__ == '__main__':\n    run(None)\n")[0])
        errors, _, _ = rules("from columbus_plugin import ToolError, Severity\ndef run(ctx):\n    raise ToolError('x')\n")
        self.assertEqual(errors, set())

    def test_docstrings_may_mention_a_url(self):
        errors, warnings, _ = rules('"""After https://example.com/method."""\ndef run(ctx):\n    return {}\n')
        self.assertEqual(errors, set())
        self.assertIn("tool.url", warnings)

    def test_work_at_import_time_and_dynamic_names_are_warned(self):
        errors, warnings, uses = rules("""
            import geopandas as gpd
            DATA = gpd.read_file("x.gpkg")
            def run(ctx):
                for n in ("a", "b"):
                    ctx.input_file(n)
                ctx.warn("Bad Id", "warn", "x")
        """)
        self.assertIn("tool.top_level", errors)
        self.assertIn("tool.name", warnings)
        self.assertIn("tool.check_id", errors)
        self.assertEqual(uses["inputs"], [])

    def test_what_the_code_names_is_reported(self):
        _, _, uses = rules("""
            def helper(ctx):
                return ctx.param("tolerance")
            def run(context):
                a = context.input_file("routes")
                context.set_output("report", rows=1)
                context.warn("lrs.gap", "warn", "gap")
                return {"t": helper(context)}
        """)
        self.assertEqual(uses, {"inputs": ["routes"], "params": ["tolerance"], "outputs": ["report"], "checks": ["lrs.gap"]})


if __name__ == "__main__":
    unittest.main()
