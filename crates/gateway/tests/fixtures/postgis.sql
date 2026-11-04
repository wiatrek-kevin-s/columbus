-- Test fixture for the PostGIS engine. Load as a superuser:
--   psql -v ON_ERROR_STOP=1 -f crates/gateway/tests/fixtures/postgis.sql
-- Creates two logins: columbus_ro (SELECT only, the recommended setup) and
-- columbus_rw (can write), which the tests use to prove the server keeps a
-- connection read-only even when the login could write.
CREATE EXTENSION IF NOT EXISTS postgis;

DROP SCHEMA IF EXISTS roads CASCADE;
DROP SCHEMA IF EXISTS hr CASCADE;
DROP ROLE IF EXISTS columbus_ro;
DROP ROLE IF EXISTS columbus_rw;

CREATE SCHEMA roads;
CREATE SCHEMA hr;

CREATE TABLE roads.roadway_inventory (
  fid serial PRIMARY KEY,
  route_id text NOT NULL,
  district_nbr integer NOT NULL,
  csj char(9) NOT NULL,
  func_class smallint,
  owner_name text,
  geom geometry(LineString, 3081)
);
-- 1,200 segments: 600 in district 14, 300 each in 12 and 15.
INSERT INTO roads.roadway_inventory (route_id, district_nbr, csj, func_class, owner_name, geom)
SELECT 'IH' || lpad(i::text, 4, '0'),
       CASE WHEN i % 2 = 0 THEN 14 WHEN i % 4 = 1 THEN 12 ELSE 15 END,
       lpad(i::text, 9, '0'),
       (i % 7) + 1,
       'Owner ' || i,
       ST_SetSRID(ST_MakeLine(ST_MakePoint(1000000 + i * 10, 7000000), ST_MakePoint(1000000 + i * 10 + 5, 7000010)), 3081)
FROM generate_series(0, 1199) AS i;

CREATE TABLE roads.roadway_audit (id serial PRIMARY KEY, who text);
INSERT INTO roads.roadway_audit (who) VALUES ('someone');

CREATE TABLE hr.employees (id integer, ssn text);
INSERT INTO hr.employees VALUES (1, '123-45-6789');

-- A function with a side effect: a plain SELECT that calls it writes. Only
-- the read-only transaction stops this, which is what the tests check.
CREATE FUNCTION roads.log_visit() RETURNS integer LANGUAGE sql VOLATILE AS
$$ INSERT INTO roads.roadway_audit (who) VALUES ('visit') RETURNING id $$;

ANALYZE roads.roadway_inventory;

CREATE ROLE columbus_ro LOGIN PASSWORD 'columbus_ro_test';
GRANT USAGE ON SCHEMA roads TO columbus_ro;
GRANT SELECT ON roads.roadway_inventory, roads.roadway_audit TO columbus_ro;

CREATE ROLE columbus_rw LOGIN PASSWORD 'columbus_rw_test';
GRANT USAGE ON SCHEMA roads TO columbus_rw;
GRANT SELECT, INSERT, UPDATE, DELETE ON ALL TABLES IN SCHEMA roads TO columbus_rw;
GRANT USAGE ON ALL SEQUENCES IN SCHEMA roads TO columbus_rw;
GRANT EXECUTE ON FUNCTION roads.log_visit() TO columbus_rw;
