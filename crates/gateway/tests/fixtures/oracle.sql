-- Test fixture for the Oracle engine. Run as SYSTEM in the pluggable
-- database (CI does this inside the container):
--   sqlplus -s -L system/<password>@//localhost/FREEPDB1 @oracle.sql
-- The schema ROADS owns the data. Two logins read it: COLUMBUS_RO (SELECT
-- only, the recommended setup) and COLUMBUS_RW (the container's APP_USER,
-- which can write), which the tests use to show the engine keeps a session
-- read-only even when the login could write.
WHENEVER SQLERROR EXIT SQL.SQLCODE
SET ECHO OFF
SET FEEDBACK OFF

BEGIN
  EXECUTE IMMEDIATE 'DROP USER roads CASCADE';
EXCEPTION WHEN OTHERS THEN IF SQLCODE != -1918 THEN RAISE; END IF;
END;
/
BEGIN
  EXECUTE IMMEDIATE 'DROP USER columbus_ro CASCADE';
EXCEPTION WHEN OTHERS THEN IF SQLCODE != -1918 THEN RAISE; END IF;
END;
/
BEGIN
  EXECUTE IMMEDIATE 'DROP PUBLIC SYNONYM split_part';
EXCEPTION WHEN OTHERS THEN IF SQLCODE != -1432 THEN RAISE; END IF;
END;
/

CREATE USER roads IDENTIFIED BY "roads_owner_test" QUOTA UNLIMITED ON users;
GRANT CREATE SESSION, CREATE TABLE, CREATE SEQUENCE, CREATE PROCEDURE, CREATE TYPE,
      CREATE VIEW, CREATE PUBLIC SYNONYM TO roads;
CREATE USER columbus_ro IDENTIFIED BY "columbus_ro_test";
GRANT CREATE SESSION TO columbus_ro;

CONNECT roads/"roads_owner_test"@//localhost/FREEPDB1

-- 1,200 segments in Austin (EPSG:4326): even ids are district 14 with a
-- numeric route code, odd ids district 9 with a route code that is not a
-- number and must never appear in an error the agent sees.
CREATE TABLE roads (
  id NUMBER(10) PRIMARY KEY,
  route_code VARCHAR2(30) NOT NULL,
  district NUMBER(4) NOT NULL,
  owner_name VARCHAR2(60),
  geom MDSYS.SDO_GEOMETRY
);
INSERT INTO roads (id, route_code, district, owner_name, geom)
SELECT level,
       CASE WHEN MOD(level, 2) = 0 THEN TO_CHAR(1000 + level) ELSE 'SECRET-' || level END,
       CASE WHEN MOD(level, 2) = 0 THEN 14 ELSE 9 END,
       'Owner ' || level,
       MDSYS.SDO_GEOMETRY(2002, 4326, NULL, MDSYS.SDO_ELEM_INFO_ARRAY(1, 2, 1),
         MDSYS.SDO_ORDINATE_ARRAY(-97.8 + level * 0.0001, 30.2, -97.8 + level * 0.0001 + 0.00005, 30.2001))
FROM dual CONNECT BY level <= 1200;
COMMIT;

INSERT INTO user_sdo_geom_metadata (table_name, column_name, diminfo, srid)
VALUES ('ROADS', 'GEOM',
        MDSYS.SDO_DIM_ARRAY(MDSYS.SDO_DIM_ELEMENT('X', -180, 180, 0.005),
                            MDSYS.SDO_DIM_ELEMENT('Y', -90, 90, 0.005)),
        4326);
COMMIT;
CREATE INDEX roads_geom_sx ON roads (geom) INDEXTYPE IS MDSYS.SPATIAL_INDEX_V2
  PARAMETERS ('layer_gtype=LINE');

-- 400 routes with measures (T-19): linear referencing geometries,
-- SDO_GTYPE 3302 (3 dimensions, the measure third: x, y, m). Even ids are
-- district 14, odd ids district 9; route i (ROUTE_ID 'R' and i in four
-- digits) runs from measure 1000 + 10 i to 1005 + 10 i.
CREATE TABLE routes (
  id NUMBER(10) PRIMARY KEY,
  route_id VARCHAR2(10) NOT NULL,
  district NUMBER(4) NOT NULL,
  geom MDSYS.SDO_GEOMETRY
);
INSERT INTO routes (id, route_id, district, geom)
SELECT level - 1,
       'R' || TO_CHAR(level - 1, 'FM0000'),
       CASE WHEN MOD(level - 1, 2) = 0 THEN 14 ELSE 9 END,
       MDSYS.SDO_GEOMETRY(3302, 4326, NULL, MDSYS.SDO_ELEM_INFO_ARRAY(1, 2, 1),
         MDSYS.SDO_ORDINATE_ARRAY(-97.8, 30.2 + (level - 1) * 0.0001, 1000 + 10 * (level - 1),
                                  -97.7999, 30.2001 + (level - 1) * 0.0001, 1005 + 10 * (level - 1)))
FROM dual CONNECT BY level <= 400;
COMMIT;

INSERT INTO user_sdo_geom_metadata (table_name, column_name, diminfo, srid)
VALUES ('ROUTES', 'GEOM',
        MDSYS.SDO_DIM_ARRAY(MDSYS.SDO_DIM_ELEMENT('X', -180, 180, 0.005),
                            MDSYS.SDO_DIM_ELEMENT('Y', -90, 90, 0.005),
                            MDSYS.SDO_DIM_ELEMENT('M', 0, 100000, 0.005)),
        4326);
COMMIT;

-- The owner's weekly right-of-way job (T-23, REQ-30, success measure 7):
-- three routes with measures in feet (SDO_GTYPE 3302), each running due
-- east from lon -97.75 for 0.003167 degrees (1,000 ft) at its own latitude,
-- measures 0 to 1,000; and an LRS event table of ROW widths with exactly
-- two gaps: route A (RA) has no width from 400 to 600 ft, route C (RC)
-- none from 700 to 1,000 ft. The fake ArcGIS ROW polygons
-- (examples/fake_oauth_server.rs) lie along the same lines.
CREATE TABLE row_routes (
  id NUMBER(10) PRIMARY KEY,
  route_id VARCHAR2(10) NOT NULL,
  route_name VARCHAR2(20) NOT NULL,
  geom MDSYS.SDO_GEOMETRY
);
INSERT INTO row_routes (id, route_id, route_name, geom) VALUES (1, 'RA', 'A',
  MDSYS.SDO_GEOMETRY(3302, 4326, NULL, MDSYS.SDO_ELEM_INFO_ARRAY(1, 2, 1),
    MDSYS.SDO_ORDINATE_ARRAY(-97.75, 30.25, 0, -97.746833, 30.25, 1000)));
INSERT INTO row_routes (id, route_id, route_name, geom) VALUES (2, 'RB', 'B',
  MDSYS.SDO_GEOMETRY(3302, 4326, NULL, MDSYS.SDO_ELEM_INFO_ARRAY(1, 2, 1),
    MDSYS.SDO_ORDINATE_ARRAY(-97.75, 30.252, 0, -97.746833, 30.252, 1000)));
INSERT INTO row_routes (id, route_id, route_name, geom) VALUES (3, 'RC', 'C',
  MDSYS.SDO_GEOMETRY(3302, 4326, NULL, MDSYS.SDO_ELEM_INFO_ARRAY(1, 2, 1),
    MDSYS.SDO_ORDINATE_ARRAY(-97.75, 30.254, 0, -97.746833, 30.254, 1000)));
COMMIT;

INSERT INTO user_sdo_geom_metadata (table_name, column_name, diminfo, srid)
VALUES ('ROW_ROUTES', 'GEOM',
        MDSYS.SDO_DIM_ARRAY(MDSYS.SDO_DIM_ELEMENT('X', -180, 180, 0.005),
                            MDSYS.SDO_DIM_ELEMENT('Y', -90, 90, 0.005),
                            MDSYS.SDO_DIM_ELEMENT('M', 0, 100000, 0.005)),
        4326);
COMMIT;

CREATE TABLE row_events (
  id NUMBER(10) PRIMARY KEY,
  route_id VARCHAR2(10) NOT NULL,
  from_m NUMBER NOT NULL,
  to_m NUMBER NOT NULL,
  row_width_ft NUMBER NOT NULL
);
INSERT INTO row_events (id, route_id, from_m, to_m, row_width_ft) VALUES (1, 'RA', 0, 400, 80);
INSERT INTO row_events (id, route_id, from_m, to_m, row_width_ft) VALUES (2, 'RA', 600, 1000, 86);
INSERT INTO row_events (id, route_id, from_m, to_m, row_width_ft) VALUES (3, 'RB', 0, 1000, 90);
INSERT INTO row_events (id, route_id, from_m, to_m, row_width_ft) VALUES (4, 'RC', 0, 700, 100);
COMMIT;

CREATE TABLE roads_audit (id NUMBER GENERATED BY DEFAULT AS IDENTITY PRIMARY KEY, who VARCHAR2(40));
INSERT INTO roads_audit (who) VALUES ('someone');
COMMIT;

-- A stand-in for Esri's SDE.ST_GEOMETRY (an object type of the same name):
-- the engine reports such columns as unsupported geometry.
CREATE TYPE st_geometry AS OBJECT (entity NUMBER, numpts NUMBER, srid NUMBER, points BLOB);
/
CREATE TABLE parcels (id NUMBER PRIMARY KEY, district NUMBER, shape st_geometry);
INSERT INTO parcels VALUES (1, 14, st_geometry(1, 1, 4326, NULL));
INSERT INTO parcels VALUES (2, 9, st_geometry(1, 1, 4326, NULL));
COMMIT;

-- A function that writes in its own (autonomous) transaction, so a plain
-- SELECT that calls it would write even inside SET TRANSACTION READ ONLY.
CREATE OR REPLACE FUNCTION log_visit RETURN NUMBER IS
  PRAGMA AUTONOMOUS_TRANSACTION;
BEGIN
  INSERT INTO roads_audit (who) VALUES ('visit');
  COMMIT;
  RETURN 1;
END;
/
-- Named like a function on the server's general allow list that Oracle
-- does not have (SPLIT_PART is PostgreSQL's); the public synonym makes the
-- bare name resolve to it, and it reads a hidden field of every row.
CREATE OR REPLACE FUNCTION split_part(a VARCHAR2, b VARCHAR2, c NUMBER) RETURN VARCHAR2 IS
  r VARCHAR2(4000);
BEGIN
  SELECT LISTAGG(owner_name, ',') WITHIN GROUP (ORDER BY id) INTO r FROM roads WHERE ROWNUM < 5;
  RETURN r;
END;
/
CREATE PUBLIC SYNONYM split_part FOR roads.split_part;
GRANT EXECUTE ON split_part TO PUBLIC;

-- Comments and a relationship, for notes drafted from the catalog.
COMMENT ON TABLE roads IS 'Road segments on the county system';
COMMENT ON COLUMN roads.district IS 'Maintenance district';
CREATE TABLE districts (id NUMBER(4) PRIMARY KEY, name VARCHAR2(40));
INSERT INTO districts VALUES (9, 'Waco');
INSERT INTO districts VALUES (14, 'Austin');
CREATE TABLE district_offices (id NUMBER PRIMARY KEY, district_id NUMBER(4) REFERENCES districts(id), city VARCHAR2(40));
INSERT INTO district_offices VALUES (1, 14, 'Austin');
COMMIT;
GRANT SELECT ON districts TO columbus_ro;
GRANT SELECT ON district_offices TO columbus_ro;
GRANT SELECT ON districts TO columbus_rw;
GRANT SELECT ON district_offices TO columbus_rw;

GRANT SELECT ON roads TO columbus_ro;
GRANT SELECT ON routes TO columbus_ro;
GRANT SELECT ON routes TO columbus_rw;
GRANT SELECT ON row_routes TO columbus_ro;
GRANT SELECT ON row_events TO columbus_ro;
GRANT SELECT ON roads_audit TO columbus_ro;
GRANT SELECT ON parcels TO columbus_ro;
GRANT EXECUTE ON st_geometry TO columbus_ro;
GRANT SELECT, INSERT, UPDATE, DELETE ON roads TO columbus_rw;
GRANT SELECT, INSERT, UPDATE, DELETE ON roads_audit TO columbus_rw;
GRANT SELECT, INSERT, UPDATE, DELETE ON parcels TO columbus_rw;
GRANT EXECUTE ON st_geometry TO columbus_rw;
GRANT EXECUTE ON log_visit TO columbus_rw;

BEGIN
  DBMS_STATS.GATHER_TABLE_STATS('ROADS', 'ROADS');
  DBMS_STATS.GATHER_TABLE_STATS('ROADS', 'ROUTES');
END;
/
EXIT
