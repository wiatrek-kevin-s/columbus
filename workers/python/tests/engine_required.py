"""Skipping tests that need the GIS engine, unless CI requires it (REQ-22).

Every CI job that installs the engine sets COLUMBUS_TEST_REQUIRE_ENGINE=1.
There a test that would skip because the engine (geopandas, pyogrio,
shapely, pyproj, rasterio, pyarrow, pypdf, or DuckDB and its spatial
extension) is missing fails instead, with a message naming the variable.
Elsewhere it skips, as before.
"""

import functools
import os
import unittest

VARIABLE = "COLUMBUS_TEST_REQUIRE_ENGINE"


def required():
    return os.environ.get(VARIABLE) == "1"


def message(why):
    return (f"{VARIABLE}=1, so this test must not skip, but {why}. Install "
            f"workers/python/requirements-gis.txt and requirements-projectdb.txt "
            f"(see docs/install.md), or unset {VARIABLE} to let engine tests skip")


def skip_unless_engine(available, why):
    """Like unittest.skipUnless(available, why) for a test class or method,
    failing instead of skipping when the engine is required."""
    if available:
        return lambda obj: obj
    if not required():
        return unittest.skip(why)

    def fail(obj):
        if isinstance(obj, type):
            def setUpClass(cls):
                raise AssertionError(message(why))
            obj.setUpClass = classmethod(setUpClass)
            return obj

        @functools.wraps(obj)
        def failing(*args, **kwargs):
            raise AssertionError(message(why))
        return failing
    return fail


def skip_engine(test, why):
    """test.skipTest(why) for a missing engine part, or a failure when the
    engine is required."""
    if required():
        test.fail(message(why))
    test.skipTest(why)
