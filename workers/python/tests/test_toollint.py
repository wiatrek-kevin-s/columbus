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
            # A star import binds names the reader cannot see, run among them.
            "from cProfile import *\n": "tool.import",
            "from os import *\n": "tool.import",
            # Refused builtins passed along rather than called.
            "def later(ctx):\n    import functools\n    return functools.reduce(getattr, ['a'], ctx)\n": "tool.call",
            "def later(ctx):\n    return list(map(eval, ['1']))\n": "tool.call",
            "def later(ctx):\n    f = exec\n    return f\n": "tool.call",
        }
        for extra, rule in swaps.items():
            code = "def _h(ctx):\n    return {'evil': 1}\n\n\ndef run(ctx):\n    return {}\n\n\n" + extra
            self.assertIn(rule, rules(code)[0], extra)
        self.assertIn("tool.decorator", rules("def _d(f):\n    return f\n\n\n@_d\ndef run(ctx):\n    return {}\n")[0])
        self.assertIn("tool.decorator", rules("import functools\n\n\n@functools.cache\ndef run(ctx):\n    return {}\n")[0])
        # A decorator the file defines itself, under an allowed name.
        self.assertIn("tool.decorator", rules("def staticmethod(f):\n    return f\n\n\n@staticmethod\ndef helper():\n    pass\n\n\ndef run(ctx):\n    return {}\n")[0])
        self.assertIn("tool.decorator", rules("from mylib import thing as dataclass\n\n\n@dataclass\nclass C:\n    pass\n\n\ndef run(ctx):\n    return {}\n")[0])

    def test_names_mean_the_same_everywhere(self):
        # What a name stands for does not depend on where or in which order
        # the checks read it.
        cases = {
            "def _unused():\n    from re import compile as exec\n\n\ndef run(ctx):\n    exec(ctx.param('code'))\n": "tool.call",
            "class K:\n    def m(self):\n        import re as getattr\n\n\ndef run(ctx):\n    return getattr(ctx, ctx.param('a'))\n": "tool.call",
            "def run(ctx):\n    def go():\n        return o.system('id')\n    import os as o\n    return go()\n": "tool.process",
            "import os\n\n\ndef run(ctx):\n    o = os\n    return o.system('id')\n": "tool.module",
            "import re as m\n\n\ndef run(ctx):\n    import os as m\n    return m.system('x')\n": "tool.import",
            "def run(ctx):\n    input = ctx.input_file('a')\n    return {}\n": "tool.call",
        }
        for code, rule in cases.items():
            self.assertIn(rule, rules(code)[0], code)

    def test_text_cannot_name_code_or_attributes(self):
        cases = {
            "import string\n\n\ndef run(ctx):\n    return string.Formatter().get_field('0.__builtins__', [run], {})\n": "tool.introspection",
            "def run(ctx):\n    return '{0.__globals__}'.format(run)\n": "tool.introspection",
            "import timeit\n\n\ndef run(ctx):\n    return timeit.timeit(ctx.param('c'))\n": "tool.import",
            "import pydoc\n\n\ndef run(ctx):\n    return pydoc.locate('builtins.exec')\n": "tool.import",
            "from pkgutil import resolve_name\n\n\ndef run(ctx):\n    return resolve_name('builtins:exec')\n": "tool.import",
            "from logging import config\n\n\ndef run(ctx):\n    return config\n": "tool.import",
            "import operator\n\n\ndef run(ctx):\n    return operator.attrgetter('x')(ctx)\n": "tool.introspection",
            "from operator import methodcaller\n\n\ndef run(ctx):\n    return methodcaller('x')(ctx)\n": "tool.import",
        }
        for code, rule in cases.items():
            self.assertIn(rule, rules(code)[0], code)
        # Ordinary use of the same modules stays fine.
        ok = "import logging\nimport operator\nimport functools\n\n\ndef run(ctx):\n    log = logging.getLogger('t')\n    log.info('start')\n    return {'n': functools.reduce(operator.add, [1, 2]), 'k': sorted([(1, 'b')], key=operator.itemgetter(1))}\n"
        self.assertEqual(rules(ok)[0], set())

    def test_only_allowed_modules_and_public_parts(self):
        # Checked as text only; none of this is run.
        refused = {
            "import gzip\n": "tool.import",
            "import six\n": "tool.import",
            "import posix\n": "tool.import",
            "from _operator import itemgetter\n": "tool.import",
            "import os\n": "tool.import",
            "import shutil\n\n\ndef later(ctx):\n    return shutil\n": "tool.import",
            "import logging\n\n\ndef later(ctx):\n    return logging.os\n": "tool.introspection",
            "import enum\n\n\ndef later(ctx):\n    return enum.bltns\n": "tool.introspection",
            "import pandas as pd\n\n\ndef later(ctx):\n    return pd.errors.ctypes\n": "tool.introspection",
            "import dataclasses\n\n\ndef later(ctx):\n    return dataclasses.sys\n": "tool.introspection",
            "import pandas as pd\n\n\ndef later(ctx):\n    return pd._config\n": "tool.private",
            "def later(ctx):\n    return ctx._plugin\n": "tool.private",
            "import typing\n\n\ndef later(ctx):\n    return typing.ForwardRef('x')\n": "tool.introspection",
            "import pandas as pd\n\n\ndef later(ctx):\n    return pd.read_pickle(ctx.input_file('a'))\n": "tool.pickle",
            "import numpy as np\n\n\ndef later(ctx):\n    return np.load(ctx.input_file('a'), allow_pickle=True)\n": "tool.pickle",
            "import pandas as pd\n\n\ndef later(ctx):\n    return pd.eval('1')\n": "tool.introspection",
        }
        refused.update({
            "from numpy import ctypeslib\n": "tool.import",
            "import numpy.f2py\n": "tool.import",
            "from logging import os\n": "tool.import",
            "from pandas import read_clipboard\n": "tool.import",
            "from numpy.lib import ctypeslib\n": "tool.import",
            "def later(ctx, df):\n    return df.plot(backend=ctx.param('b'))\n": "tool.settings",
            "import pandas as pd\n\n\ndef later(ctx):\n    return pd.plotting.plot_params\n": "tool.settings",
            "import networkx as nx\n\n\ndef later(ctx):\n    return nx.config\n": "tool.settings",
            "import rasterio\n\n\ndef later(ctx):\n    rasterio.env.setenv(X=1)\n": "tool.settings",
            "import rasterio\n\n\ndef later(ctx):\n    return rasterio.Env.from_defaults(X=1)\n": "tool.settings",
            "import fiona\n\n\ndef later(ctx):\n    return fiona.drivers(X=1)\n": "tool.settings",
            "import pyproj\n\n\ndef later(ctx):\n    pyproj.datadir.set_data_dir(ctx.param('d'))\n": "tool.settings",
            "import pyproj\n\n\ndef later(ctx):\n    pyproj.network.set_network_enabled(True)\n": "tool.settings",
            "from pyproj.datadir import set_data_dir\n": "tool.import",
            "import zipfile._path\n": "tool.private",
            "import pandas as pd\n\n\ndef later(ctx):\n    return pd.compat.pickle_compat\n": "tool.import",
            "import numpy as np\n\n\ndef later(ctx):\n    return np.load(ctx.input_file('a'), None, True)\n": "tool.call",
            "import numpy as np\n\n\ndef later(ctx):\n    return np.lib.format\n": "tool.import",
            "def later(ctx, df):\n    return df.plot(**{'kind': 'line'})\n": "tool.call",
            "def later(ctx, df):\n    kw = {'backend': 'x'}\n    return df.plot(**kw)\n": "tool.settings",
            "def later(ctx, s):\n    return s.hist(None, None, None, None, None, None, None, None, None, 'x')\n": "tool.call",
            "import rasterio\n\n\ndef later(ctx):\n    e = rasterio.Env\n    return e\n": "tool.settings",
            "from rasterio import Env\n\n\nclass E(Env):\n    pass\n": "tool.settings",
            "def later(ctx, gdf):\n    q = gdf.query\n    return q\n": "tool.eval",
            "def later(ctx, gdf):\n    return list(map(gdf.eval, ['a']))\n": "tool.eval",
            "import rasterio\n\n\ndef later(ctx):\n    rasterio.env.set_proj_data_search_path('x')\n": "tool.settings",
            "def later(ctx, df):\n    return df.style.env\n": "tool.settings",
            "import scipy\n\n\ndef later(ctx):\n    return scipy.LowLevelCallable\n": "tool.introspection",
            "import networkx.drawing.nx_pydot\n": "tool.import",
            "import numpy as np\n\n\ndef later(ctx):\n    return np.ctypeslib\n": "tool.introspection",
            "import logging.handlers\n": "tool.import",
            "import pandas as pd\n\n\ndef later(ctx):\n    return pd.read_clipboard()\n": "tool.settings",
            "def later(ctx, df):\n    df.to_clipboard()\n": "tool.settings",
            "import pandas as pd\n\n\ndef later(ctx):\n    pd.set_option(ctx.param('k'), ctx.param('v'))\n": "tool.settings",
            "import pandas as pd\n\n\ndef later(ctx):\n    return pd.options\n": "tool.introspection",
            "import rasterio\n\n\ndef later(ctx):\n    return rasterio.Env(GDAL_DATA='x')\n": "tool.settings",
        })
        for extra, rule in refused.items():
            code = extra + "\n\ndef run(ctx):\n    return {}\n"
            self.assertIn(rule, rules(code)[0], extra)
        # Allowed: zipfile for a zipped shapefile, rasterio.Env() with no settings.
        ok = "import zipfile\nimport rasterio\n\n\ndef run(ctx):\n    with rasterio.Env():\n        pass\n    with zipfile.ZipFile(ctx.output_path('z'), 'w') as z:\n        z.writestr('a.txt', 'x')\n    return {}\n"
        self.assertEqual(rules(ok)[0], set())
        # Ordinary names that only look like settings, and a package imported
        # with one of its parts, are fine.
        ok = ("import rasterio\nimport rasterio.features\nimport networkx as nx\nimport geopandas as gpd\n\n\ndef run(ctx):\n    gdf = gpd.read_file(ctx.input_file('a'))\n"
              "    rows = [r.network for r in gdf.itertuples()]\n    ids = gdf['network_id']\n    cfg = {'config_name': 1}\n"
              "    profile = {'driver': 'GTiff'}\n    path = nx.shortest_path(nx.Graph(), 1, 1)\n"
              "    with rasterio.open(ctx.output_path('o'), 'w', **profile) as dst:\n        pass\n"
              "    shapes = rasterio.features.shapes\n    return {'n': len(rows), 'p': path, 'ids': list(ids), 'c': cfg, 's': shapes}\n")
        self.assertEqual(rules(ok)[0], set())

    def test_spatial_queries_and_parameters_choosing_functions(self):
        head = "import geopandas as gpd\nimport shapely\nimport rasterio\n\n\ndef run(ctx):\n    gdf = gpd.read_file(ctx.input_file('a'))\n"
        ok = [
            "    hits = gdf.sindex.query(gdf.geometry.iloc[0], predicate='intersects')\n    return {'n': len(hits)}\n",
            "    tree = shapely.STRtree(gdf.geometry.values)\n    return {'n': len(tree.query(gdf.geometry.iloc[0]))}\n",
            "    stat = ctx.param('stat')\n    how = {'sum': 'sum', 'mean': 'mean'}[stat]\n    return {'t': gdf.agg(how).to_dict()}\n",
            "    field = ctx.param('field')\n    out = gdf.dissolve(field, aggfunc='sum')\n    return {'n': len(out)}\n",
            "    with rasterio.open(ctx.input_file('r')) as src:\n        with rasterio.open(ctx.output_path('o'), 'w', **src.profile) as dst:\n            dst.write(src.read())\n    return {}\n",
        ]
        for body in ok:
            self.assertEqual(rules(head + body)[0], set(), body)
        refused = [
            "    return {'t': gdf.agg(ctx.param('stat'))}\n",
            "    stat = ctx.param('stat')\n    return {'t': gdf.apply(stat)}\n",
            "    return {'t': gdf.dissolve('a', aggfunc=ctx.param('how'))}\n",
            "    return {'t': gdf.agg('to_clipboard')}\n",
            "    return {'t': gdf.agg('_private')}\n",
        ]
        for body in refused:
            self.assertIn("tool.dispatch", rules(head + body)[0], body)

    def test_expressions_given_as_text_are_refused(self):
        base = "import geopandas as gpd\n\n\ndef run(ctx):\n    gdf = gpd.read_file(ctx.input_file('layer'))\n"
        for line in ("    return gdf.query(ctx.param('where'))\n",
                     "    return gdf.eval(ctx.param('expr'))\n",
                     "    return gdf.query(expr=ctx.param('where'))\n",
                     "    return gdf.query('a > @limit')\n"):
            self.assertIn("tool.eval", rules(base + line)[0], line)
        # A fixed expression, or a mask, is fine.
        self.assertEqual(rules(base + "    return gdf.query('lanes > 2')\n")[0], set())
        self.assertEqual(rules(base + "    f = ctx.param('field')\n    return gdf[gdf[f] > 2]\n")[0], set())

    def test_ordinary_code_still_passes(self):
        ok = (
            "from dataclasses import dataclass, field\nimport logging\nfrom pathlib import Path\n\n\n"
            "@dataclass\nclass Hit:\n    route: str\n    notes: list = field(default_factory=list)\n    offset: float = 0.0\n\n\n"
            "def run(ctx):\n    log = logging.getLogger('t')\n    out = Path(ctx.output_path('o'))\n"
            "    names = ['Shape__Length__', 'Shape__Area']\n    log.info('%s', names)\n    return {'out': str(out), 'hits': [Hit('IH0035')]}\n"
        )
        self.assertEqual(rules(ok)[0], set())

    def test_what_is_allowed_at_the_top_level(self):
        ok = (
            '"""Doc."""\nimport math\nfrom functools import lru_cache\n\nLIMIT = 10\nUNITS = {"mile": 1609.344, "foot": [0.3048, -1]}\n'
            "NAMES: tuple = ('a', 'b')\n\n\nclass Pair:\n    'Two.'\n    size = 2\n\n    def total(self, a: int, b: int = 0) -> int:\n        return a + b\n\n\n"
            "@lru_cache(maxsize=8)\ndef helper(x: float | None = None) -> float:\n    return math.sqrt(x or 0)\n\n\ndef run(ctx):\n    return {'n': helper(LIMIT)}\n"
        )
        errors, _, _ = rules(ok)
        self.assertEqual(errors, set())
        # A name imported under a refused builtin's name is that import, not the builtin.
        errors, _, _ = rules("from re import compile\n\n\ndef run(ctx):\n    return {'ok': bool(compile('a').match('a'))}\n")
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
