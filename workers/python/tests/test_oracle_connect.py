"""How an Oracle connection's settings become python-oracledb's arguments
(no database: a stand-in module records what would be passed)."""

import os
import sys
import unittest
from unittest import mock

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.dirname(HERE))

import columbus_worker as w  # noqa: E402


class _Stop(Exception):
    pass


class _FakeOracle:
    class Error(Exception):
        pass

    def __init__(self):
        self.params = None

    def connect(self, **params):
        self.params = params
        raise _Stop()


class OracleConnect(unittest.TestCase):
    def params(self, conn):
        fake = _FakeOracle()
        with mock.patch.object(w, "_ora", lambda: fake):
            with self.assertRaises(_Stop):
                w._ora_connect({"user": "gis", "password": "pw", **conn}, 5.0)
        return fake.params

    def test_host_and_service_name(self):
        p = self.params({"host": "db.corp", "port": 1522, "database": "ORCLPDB1"})
        self.assertEqual((p["host"], p["port"], p["service_name"]), ("db.corp", 1522, "ORCLPDB1"))

    def test_service_name_saved_in_dsn_beside_a_host(self):
        # As 0.15.0's form saved it: Host, Port and "Service name (DSN)".
        p = self.params({"host": "db.corp", "dsn": "ORCLPDB1"})
        self.assertEqual((p["service_name"], p["port"]), ("ORCLPDB1", 1521))
        self.assertNotIn("dsn", p)

    def test_alias_without_host(self):
        self.assertEqual(self.params({"dsn": "ORAPROD"})["dsn"], "ORAPROD")

    def test_tcps_when_ssl_is_required(self):
        p = self.params({"host": "db", "database": "S", "sslmode": "verify-full"})
        self.assertEqual((p["protocol"], p["ssl_server_dn_match"]), ("tcps", True))


if __name__ == "__main__":
    unittest.main()
