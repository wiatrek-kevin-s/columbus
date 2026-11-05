-- Test fixture for the SQL Server engine. Run as sa (CI does this inside
-- the container):
--   sqlcmd -S localhost -U sa -P <password> -C -b -i mssql.sql
-- Creates the database gis with schema roads, and two logins: columbus_ro
-- (SELECT only, the recommended setup) and columbus_rw (can write), which
-- the tests use to show the engine keeps every query's effects from being
-- kept even when the login could write.
SET NOCOUNT ON;
IF DB_ID('gis') IS NULL CREATE DATABASE gis;
GO
USE gis;
GO
IF SCHEMA_ID('roads') IS NULL EXEC('CREATE SCHEMA roads');
GO
DROP FUNCTION IF EXISTS roads.owner_list;
DROP TABLE IF EXISTS roads.roads;
DROP TABLE IF EXISTS roads.places;
DROP TABLE IF EXISTS roads.roads_audit;
GO

-- 1,200 segments (EPSG:3081, Texas State Mapping System): even ids are
-- district 14 with a numeric route code, odd ids district 9 with a route
-- code that is not a number and must never appear in an error the agent sees.
CREATE TABLE roads.roads (
  id INT PRIMARY KEY,
  route_code VARCHAR(30) NOT NULL,
  district INT NOT NULL,
  owner_name NVARCHAR(60),
  geom geometry
);
WITH n AS (
  SELECT TOP (1200) ROW_NUMBER() OVER (ORDER BY (SELECT NULL)) AS i
  FROM sys.all_objects a CROSS JOIN sys.all_objects b
)
INSERT INTO roads.roads (id, route_code, district, owner_name, geom)
SELECT i,
       CASE WHEN i % 2 = 0 THEN CAST(1000 + i AS VARCHAR(30)) ELSE CONCAT('SECRET-', i) END,
       CASE WHEN i % 2 = 0 THEN 14 ELSE 9 END,
       CONCAT(N'Owner ', i),
       geometry::STGeomFromText(CONCAT('LINESTRING(', 1000000 + i * 10, ' 7000000, ',
                                       1000000 + i * 10 + 5, ' 7000010)'), 3081)
FROM n;

CREATE TABLE roads.places (id INT PRIMARY KEY, district INT NOT NULL, name NVARCHAR(40), loc geography);
INSERT INTO roads.places VALUES
  (1, 14, N'Capitol', geography::Point(30.2747, -97.7404, 4326)),
  (2, 9, N'Elsewhere', geography::Point(31.0, -98.0, 4326));

CREATE TABLE roads.roads_audit (id INT IDENTITY PRIMARY KEY, who NVARCHAR(40));
INSERT INTO roads.roads_audit (who) VALUES (N'someone');
GO

-- Comments and a relationship, for notes drafted from the catalog.
EXEC sp_addextendedproperty 'MS_Description', N'Road segments on the county system', 'SCHEMA', 'roads', 'TABLE', 'roads';
EXEC sp_addextendedproperty 'MS_Description', N'Maintenance district', 'SCHEMA', 'roads', 'TABLE', 'roads', 'COLUMN', 'district';
CREATE TABLE roads.districts (id INT PRIMARY KEY, name NVARCHAR(40));
INSERT INTO roads.districts VALUES (9, N'Waco'), (14, N'Austin');
CREATE TABLE roads.district_offices (id INT PRIMARY KEY, district_id INT REFERENCES roads.districts(id), city NVARCHAR(40));
INSERT INTO roads.district_offices VALUES (1, 14, N'Austin');
GO

-- A scalar function that reads a hidden field of every row. SQL Server
-- calls such functions only by a two-part name, which the server refuses.
CREATE FUNCTION roads.owner_list() RETURNS NVARCHAR(MAX) AS
BEGIN
  RETURN (SELECT STRING_AGG(owner_name, ',') FROM roads.roads);
END;
GO

IF SUSER_ID('columbus_ro') IS NULL
  CREATE LOGIN columbus_ro WITH PASSWORD = 'Columbus_ro_test1', CHECK_POLICY = OFF;
IF SUSER_ID('columbus_rw') IS NULL
  CREATE LOGIN columbus_rw WITH PASSWORD = 'Columbus_rw_test1', CHECK_POLICY = OFF;
GO
IF USER_ID('columbus_ro') IS NULL CREATE USER columbus_ro FOR LOGIN columbus_ro;
IF USER_ID('columbus_rw') IS NULL CREATE USER columbus_rw FOR LOGIN columbus_rw;
GRANT SELECT ON SCHEMA::roads TO columbus_ro;
GRANT SELECT, INSERT, UPDATE, DELETE ON SCHEMA::roads TO columbus_rw;
GRANT EXECUTE ON roads.owner_list TO columbus_rw;
-- For the tests' plan checks only (SET SHOWPLAN_ALL), never needed by Columbus.
GRANT SHOWPLAN TO columbus_rw;
GO
