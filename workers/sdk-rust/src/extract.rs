//! Read-only access to dataset extracts.
//!
//! The gateway hands a plugin each dataset as an *extract*: a SQLite file
//! it produced through the governed read path (allow lists, row filters,
//! hidden fields removed). The file holds the table `data` (the rows) and
//! the table `columbus_extract(key, value)` (metadata). See
//! `docs/worker-protocol.md`, section "Extracts".

use std::collections::{BTreeMap, VecDeque};
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use rusqlite::types::{Value as SqlValue, ValueRef};
use rusqlite::{Connection, OpenFlags, Statement};
use serde_json::{Map, Value};

use crate::geometry::{GeomError, Geometry, GeometryFormat};

/// Rows fetched per query while iterating.
const BATCH: i64 = 512;

/// Why an extract could not be opened or read.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum ExtractError {
    /// The file could not be opened or queried.
    #[error("extract {path}: {message}")]
    Sqlite { path: PathBuf, message: String },
    /// The file is SQLite but not an extract (a table is missing).
    #[error("{path} is not a Columbus extract: {message}")]
    NotAnExtract { path: PathBuf, message: String },
    /// A row getter named a column the extract does not have.
    #[error("the extract has no column named {0:?}")]
    NoColumn(String),
    /// A typed getter found a value of another type.
    #[error("column {column:?} holds {found}, not {expected}")]
    Type {
        column: String,
        expected: &'static str,
        found: &'static str,
    },
}

/// One value of a row, as SQLite stores it.
#[derive(Debug, Clone, PartialEq)]
pub enum Cell {
    Null,
    Integer(i64),
    Real(f64),
    Text(String),
    Blob(Vec<u8>),
}

impl Cell {
    /// The SQLite storage class: `"null"`, `"integer"`, `"real"`, `"text"`
    /// or `"blob"`.
    pub fn kind(&self) -> &'static str {
        match self {
            Cell::Null => "null",
            Cell::Integer(_) => "integer",
            Cell::Real(_) => "real",
            Cell::Text(_) => "text",
            Cell::Blob(_) => "blob",
        }
    }

    /// The value as JSON. Blobs become `null` (JSON has no bytes);
    /// non-finite reals become `null`.
    pub fn to_json(&self) -> Value {
        match self {
            Cell::Null | Cell::Blob(_) => Value::Null,
            Cell::Integer(i) => Value::from(*i),
            Cell::Real(f) => serde_json::Number::from_f64(*f).map_or(Value::Null, Value::Number),
            Cell::Text(s) => Value::String(s.clone()),
        }
    }
}

impl From<SqlValue> for Cell {
    fn from(v: SqlValue) -> Self {
        match v {
            SqlValue::Null => Cell::Null,
            SqlValue::Integer(i) => Cell::Integer(i),
            SqlValue::Real(f) => Cell::Real(f),
            SqlValue::Text(s) => Cell::Text(s),
            SqlValue::Blob(b) => Cell::Blob(b),
        }
    }
}

#[derive(Debug)]
struct Columns {
    names: Vec<String>,
    index: BTreeMap<String, usize>,
    geometry: Option<usize>,
    format: Option<GeometryFormat>,
}

/// An extract, opened read-only. Close it by dropping it.
pub struct Extract {
    path: PathBuf,
    conn: Connection,
    meta: BTreeMap<String, String>,
    columns: Arc<Columns>,
    row_count: u64,
    /// A name SQLite resolves to the rowid (one not shadowed by a column).
    rowid: &'static str,
}

impl fmt::Debug for Extract {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Extract")
            .field("path", &self.path)
            .field("meta", &self.meta)
            .field("columns", &self.columns.names)
            .field("row_count", &self.row_count)
            .finish()
    }
}

impl Extract {
    /// Open an extract read-only (`SQLITE_OPEN_READ_ONLY`; nothing can be
    /// written through this handle) and read its metadata and columns.
    pub fn open(path: impl AsRef<Path>) -> Result<Extract, ExtractError> {
        let path = path.as_ref().to_path_buf();
        let sql_err = |e: rusqlite::Error| ExtractError::Sqlite {
            path: path.clone(),
            message: e.to_string(),
        };
        if !path.is_file() {
            return Err(ExtractError::Sqlite {
                path: path.clone(),
                message: "file not found".into(),
            });
        }
        let flags = OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX;
        let conn = Connection::open_with_flags(&path, flags).map_err(sql_err)?;
        let not_extract = |message: String| ExtractError::NotAnExtract {
            path: path.clone(),
            message,
        };

        let mut meta = BTreeMap::new();
        {
            let mut stmt = conn
                .prepare("SELECT key, value FROM columbus_extract")
                .map_err(|e| not_extract(format!("no readable columbus_extract table ({e})")))?;
            let rows = stmt
                .query_map([], |r| {
                    Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?))
                })
                .map_err(sql_err)?;
            for row in rows {
                let (k, v) = row.map_err(sql_err)?;
                meta.insert(k, v.unwrap_or_default());
            }
        }

        let names: Vec<String> = {
            let mut stmt = conn
                .prepare("PRAGMA table_info(\"data\")")
                .map_err(sql_err)?;
            let rows = stmt
                .query_map([], |r| r.get::<_, String>(1))
                .map_err(sql_err)?;
            rows.collect::<Result<_, _>>().map_err(sql_err)?
        };
        if names.is_empty() {
            return Err(not_extract("no table named data".into()));
        }
        let index: BTreeMap<String, usize> = names
            .iter()
            .enumerate()
            .map(|(i, n)| (n.clone(), i))
            .collect();
        let geometry = meta
            .get("geometry_column")
            .filter(|c| !c.is_empty())
            .and_then(|c| index.get(c).copied());
        let format = meta
            .get("geometry_format")
            .and_then(|f| GeometryFormat::from_name(f));

        let rowid = ["_rowid_", "rowid", "oid"]
            .into_iter()
            .find(|alias| !names.iter().any(|n| n.eq_ignore_ascii_case(alias)))
            .ok_or_else(|| not_extract("columns shadow every rowid alias".into()))?;

        let row_count = match meta.get("rows").and_then(|r| r.trim().parse::<u64>().ok()) {
            Some(n) => n,
            None => conn
                .query_row("SELECT count(*) FROM \"data\"", [], |r| r.get::<_, i64>(0))
                .map_err(sql_err)? as u64,
        };

        Ok(Extract {
            path,
            conn,
            meta,
            columns: Arc::new(Columns {
                names,
                index,
                geometry,
                format,
            }),
            row_count,
            rowid,
        })
    }

    /// The extract's file.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// A metadata value from `columbus_extract`: `connection`, `dataset`,
    /// `rows`, `geometry_column`, `geometry_format`, `srid`, `labels`.
    pub fn meta(&self, key: &str) -> Option<String> {
        self.meta.get(key).cloned()
    }

    /// The column names of `data`, in table order.
    pub fn columns(&self) -> Vec<String> {
        self.columns.names.clone()
    }

    /// The number of rows: the `rows` metadata, or a count when it is
    /// missing.
    pub fn row_count(&self) -> u64 {
        self.row_count
    }

    /// The geometry column, when the extract has one.
    pub fn geometry_column(&self) -> Option<&str> {
        self.columns
            .geometry
            .map(|i| self.columns.names[i].as_str())
    }

    /// How geometry is encoded (`geometry_format`), when known.
    pub fn geometry_format(&self) -> Option<GeometryFormat> {
        self.columns.format
    }

    /// The dataset's SRID (`srid` metadata), when known.
    pub fn srid(&self) -> Option<i32> {
        self.meta.get("srid").and_then(|s| s.trim().parse().ok())
    }

    /// The data labels the gateway attached (`labels`, a JSON list).
    pub fn labels(&self) -> Vec<String> {
        self.meta
            .get("labels")
            .and_then(|l| serde_json::from_str(l).ok())
            .unwrap_or_default()
    }

    /// Iterate over the rows in storage order. Rows are fetched in batches,
    /// so memory stays flat however large the extract is.
    ///
    /// ```no_run
    /// # fn f(ex: &columbus_plugin::Extract) -> Result<(), columbus_plugin::ToolError> {
    /// for row in ex.rows()? {
    ///     let row = row?;
    ///     let name = row.get_str("name")?;
    ///     if let Some(g) = row.geometry() {
    ///         let g = g?;
    ///         println!("{name:?}: {:?}", g.bbox());
    ///     }
    /// }
    /// # Ok(()) }
    /// ```
    pub fn rows(&self) -> Result<Rows<'_>, ExtractError> {
        let sql = format!(
            "SELECT {id}, * FROM \"data\" WHERE {id} > ?1 ORDER BY {id} LIMIT {BATCH}",
            id = self.rowid
        );
        let stmt = self.conn.prepare(&sql).map_err(|e| self.sql_error(e))?;
        Ok(Rows {
            extract: self,
            stmt,
            after: i64::MIN,
            buffer: VecDeque::new(),
            done: false,
        })
    }

    /// Run a read-only SQL query against the extract and collect the rows,
    /// for plugins that want SQLite to aggregate or filter. Writes are
    /// refused by the read-only connection.
    pub fn query(&self, sql: &str) -> Result<Vec<Vec<Cell>>, ExtractError> {
        let mut stmt = self.conn.prepare(sql).map_err(|e| self.sql_error(e))?;
        let n = stmt.column_count();
        let rows = stmt
            .query_map([], |r| {
                (0..n)
                    .map(|i| r.get::<_, SqlValue>(i).map(Cell::from))
                    .collect()
            })
            .map_err(|e| self.sql_error(e))?;
        rows.collect::<Result<_, _>>()
            .map_err(|e| self.sql_error(e))
    }

    fn sql_error(&self, e: rusqlite::Error) -> ExtractError {
        ExtractError::Sqlite {
            path: self.path.clone(),
            message: e.to_string(),
        }
    }
}

/// An iterator over an extract's rows. See [`Extract::rows`].
pub struct Rows<'a> {
    extract: &'a Extract,
    stmt: Statement<'a>,
    after: i64,
    buffer: VecDeque<Row>,
    done: bool,
}

impl Rows<'_> {
    fn fill(&mut self) -> Result<(), ExtractError> {
        let columns = &self.extract.columns;
        let n = columns.names.len();
        let mut rows = self
            .stmt
            .query([self.after])
            .map_err(|e| self.extract.sql_error(e))?;
        let mut fetched = 0;
        while let Some(r) = rows.next().map_err(|e| self.extract.sql_error(e))? {
            let id: i64 = r.get(0).map_err(|e| self.extract.sql_error(e))?;
            let mut values = Vec::with_capacity(n);
            for i in 0..n {
                let v = r.get_ref(i + 1).map_err(|e| self.extract.sql_error(e))?;
                values.push(match v {
                    ValueRef::Null => Cell::Null,
                    ValueRef::Integer(i) => Cell::Integer(i),
                    ValueRef::Real(f) => Cell::Real(f),
                    ValueRef::Text(t) => Cell::Text(String::from_utf8_lossy(t).into_owned()),
                    ValueRef::Blob(b) => Cell::Blob(b.to_vec()),
                });
            }
            self.after = id;
            fetched += 1;
            self.buffer.push_back(Row {
                values,
                columns: Arc::clone(columns),
            });
        }
        if fetched < BATCH {
            self.done = true;
        }
        Ok(())
    }
}

impl Iterator for Rows<'_> {
    type Item = Result<Row, ExtractError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.buffer.is_empty() && !self.done {
            if let Err(e) = self.fill() {
                self.done = true;
                return Some(Err(e));
            }
        }
        self.buffer.pop_front().map(Ok)
    }
}

/// One row of an extract. Getters take a column name.
///
/// Typed getters return `Ok(None)` for SQL NULL and an error when the
/// column does not exist or holds another type.
#[derive(Debug, Clone)]
pub struct Row {
    values: Vec<Cell>,
    columns: Arc<Columns>,
}

impl Row {
    /// The raw value of a column, or `None` when there is no such column.
    pub fn get(&self, column: &str) -> Option<&Cell> {
        self.columns.index.get(column).map(|&i| &self.values[i])
    }

    /// All values, in column order.
    pub fn values(&self) -> &[Cell] {
        &self.values
    }

    fn cell(&self, column: &str) -> Result<&Cell, ExtractError> {
        self.get(column)
            .ok_or_else(|| ExtractError::NoColumn(column.to_string()))
    }

    fn mismatch(column: &str, expected: &'static str, found: &Cell) -> ExtractError {
        ExtractError::Type {
            column: column.to_string(),
            expected,
            found: found.kind(),
        }
    }

    /// A number. Integers are converted.
    pub fn get_f64(&self, column: &str) -> Result<Option<f64>, ExtractError> {
        match self.cell(column)? {
            Cell::Null => Ok(None),
            Cell::Real(f) => Ok(Some(*f)),
            Cell::Integer(i) => Ok(Some(*i as f64)),
            other => Err(Self::mismatch(column, "a number", other)),
        }
    }

    /// An integer. A real with no fractional part is accepted.
    pub fn get_i64(&self, column: &str) -> Result<Option<i64>, ExtractError> {
        match self.cell(column)? {
            Cell::Null => Ok(None),
            Cell::Integer(i) => Ok(Some(*i)),
            Cell::Real(f) if f.fract() == 0.0 && f.abs() < 9.007_199_254_740_992e15 => {
                Ok(Some(*f as i64))
            }
            other => Err(Self::mismatch(column, "an integer", other)),
        }
    }

    /// Text.
    pub fn get_str(&self, column: &str) -> Result<Option<&str>, ExtractError> {
        match self.cell(column)? {
            Cell::Null => Ok(None),
            Cell::Text(s) => Ok(Some(s)),
            other => Err(Self::mismatch(column, "text", other)),
        }
    }

    /// Raw bytes.
    pub fn get_blob(&self, column: &str) -> Result<Option<&[u8]>, ExtractError> {
        match self.cell(column)? {
            Cell::Null => Ok(None),
            Cell::Blob(b) => Ok(Some(b)),
            other => Err(Self::mismatch(column, "a blob", other)),
        }
    }

    /// The row's geometry, decoded from the extract's geometry column with
    /// its `geometry_format` (detected from the bytes when unknown).
    ///
    /// `None` when the extract has no geometry column or the value is NULL;
    /// `Some(Err(_))` when the bytes cannot be decoded.
    pub fn geometry(&self) -> Option<Result<Geometry, GeomError>> {
        let i = self.columns.geometry?;
        let bytes = match &self.values[i] {
            Cell::Null => return None,
            Cell::Blob(b) => b,
            other => return Some(Err(GeomError::NotBlob(other.kind()))),
        };
        Some(match self.columns.format {
            Some(GeometryFormat::Gpkg) => Geometry::from_gpkg(bytes).map(|(g, _)| g),
            Some(GeometryFormat::Ewkb) => Geometry::from_wkb(bytes).map(|(g, _)| g),
            None => Geometry::decode(bytes),
        })
    }

    /// Every non-geometry column as JSON, for GeoJSON `properties` or a
    /// summary. Blobs become `null`.
    pub fn properties(&self) -> Map<String, Value> {
        self.columns
            .names
            .iter()
            .enumerate()
            .filter(|(i, _)| Some(*i) != self.columns.geometry)
            .map(|(i, name)| (name.clone(), self.values[i].to_json()))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geometry::GeometryType;

    fn gpkg_point(x: f64, y: f64) -> Vec<u8> {
        let mut b = vec![b'G', b'P', 0, 1];
        b.extend_from_slice(&4326i32.to_le_bytes());
        b.push(1);
        b.extend_from_slice(&1u32.to_le_bytes());
        b.extend_from_slice(&x.to_le_bytes());
        b.extend_from_slice(&y.to_le_bytes());
        b
    }

    fn make(dir: &Path, columns: &str, meta: &[(&str, &str)], rows: usize) -> PathBuf {
        let path = dir.join("extract.sqlite");
        let db = Connection::open(&path).unwrap();
        db.execute_batch(&format!(
            "CREATE TABLE data ({columns}); CREATE TABLE columbus_extract (key TEXT PRIMARY KEY, value TEXT);"
        ))
        .unwrap();
        for (k, v) in meta {
            db.execute("INSERT INTO columbus_extract VALUES (?1, ?2)", [k, v])
                .unwrap();
        }
        for i in 0..rows {
            let geom = if i % 10 == 9 {
                None
            } else {
                Some(gpkg_point(i as f64, -(i as f64)))
            };
            db.execute(
                "INSERT INTO data VALUES (?1, ?2, ?3, ?4)",
                rusqlite::params![i as i64, format!("road {i}"), i as f64 * 0.5, geom],
            )
            .unwrap();
        }
        path
    }

    const META: &[(&str, &str)] = &[
        ("connection", "roads-prod"),
        ("dataset", "ROADS.ROADWAY"),
        ("rows", "1300"),
        ("geometry_column", "geom"),
        ("geometry_format", "gpkg"),
        ("srid", "4326"),
        ("labels", "[\"internal\",\"pii-free\"]"),
    ];

    #[test]
    fn reads_metadata_columns_and_rows() {
        let dir = tempfile::tempdir().unwrap();
        let path = make(dir.path(), "fid, name, measure, geom", META, 1300);
        let ex = Extract::open(&path).unwrap();
        assert_eq!(ex.meta("dataset").as_deref(), Some("ROADS.ROADWAY"));
        assert_eq!(ex.meta("missing"), None);
        assert_eq!(ex.columns(), ["fid", "name", "measure", "geom"]);
        assert_eq!(ex.row_count(), 1300);
        assert_eq!(ex.geometry_column(), Some("geom"));
        assert_eq!(ex.geometry_format(), Some(GeometryFormat::Gpkg));
        assert_eq!(ex.srid(), Some(4326));
        assert_eq!(ex.labels(), ["internal", "pii-free"]);

        // More rows than one batch, in order, none lost.
        let mut n = 0;
        let mut nulls = 0;
        for (i, row) in ex.rows().unwrap().enumerate() {
            let row = row.unwrap();
            assert_eq!(row.get_i64("fid").unwrap(), Some(i as i64));
            assert_eq!(
                row.get_str("name").unwrap(),
                Some(format!("road {i}").as_str())
            );
            assert_eq!(row.get_f64("measure").unwrap(), Some(i as f64 * 0.5));
            assert_eq!(row.get_f64("fid").unwrap(), Some(i as f64));
            match row.geometry() {
                None => nulls += 1,
                Some(g) => {
                    let g = g.unwrap();
                    assert_eq!(g.geometry_type(), GeometryType::Point);
                    assert_eq!(
                        g.bbox(),
                        Some([i as f64, -(i as f64), i as f64, -(i as f64)])
                    );
                }
            }
            n += 1;
        }
        assert_eq!(n, 1300);
        assert_eq!(nulls, 130);
    }

    #[test]
    fn getter_errors_and_properties() {
        let dir = tempfile::tempdir().unwrap();
        let path = make(dir.path(), "fid, name, measure, geom", META, 2);
        let ex = Extract::open(&path).unwrap();
        let row = ex.rows().unwrap().next().unwrap().unwrap();
        assert_eq!(
            row.get_f64("nope"),
            Err(ExtractError::NoColumn("nope".into()))
        );
        assert!(matches!(
            row.get_f64("name"),
            Err(ExtractError::Type { found: "text", .. })
        ));
        assert!(matches!(row.get_str("fid"), Err(ExtractError::Type { .. })));
        assert!(row.get_i64("measure").unwrap() == Some(0));
        assert_eq!(
            row.get_blob("geom").unwrap().map(|b| &b[..2]),
            Some(&b"GP"[..])
        );
        let props = row.properties();
        assert_eq!(
            Value::Object(props),
            serde_json::json!({"fid": 0, "name": "road 0", "measure": 0.0})
        );
        assert_eq!(row.get("geom").map(Cell::kind), Some("blob"));
    }

    #[test]
    fn opened_read_only() {
        let dir = tempfile::tempdir().unwrap();
        let path = make(dir.path(), "fid, name, measure, geom", META, 3);
        let ex = Extract::open(&path).unwrap();
        assert!(ex.conn.execute("DELETE FROM data", []).is_err());
        assert!(ex.query("INSERT INTO data VALUES (1, 2, 3, 4)").is_err());
        let count = ex.query("SELECT count(*) FROM data").unwrap();
        assert_eq!(count, vec![vec![Cell::Integer(3)]]);
    }

    #[test]
    fn counts_rows_when_metadata_lacks_them_and_handles_rowid_columns() {
        let dir = tempfile::tempdir().unwrap();
        // A source column called "rowid" must not confuse iteration.
        let path = make(
            dir.path(),
            "rowid, name, measure, geom",
            &[("geometry_column", "geom")],
            600,
        );
        let ex = Extract::open(&path).unwrap();
        assert_eq!(ex.row_count(), 600);
        assert_eq!(ex.geometry_format(), None);
        assert_eq!(ex.labels(), Vec::<String>::new());
        let rows: Vec<Row> = ex.rows().unwrap().map(Result::unwrap).collect();
        assert_eq!(rows.len(), 600);
        assert_eq!(rows[599].get_i64("rowid").unwrap(), Some(599));
        // Format detected from the bytes.
        assert!(rows[0].geometry().unwrap().is_ok());
    }

    #[test]
    fn refuses_files_that_are_not_extracts() {
        let dir = tempfile::tempdir().unwrap();
        let missing = Extract::open(dir.path().join("nope.sqlite")).unwrap_err();
        assert!(matches!(missing, ExtractError::Sqlite { .. }));

        let path = dir.path().join("plain.sqlite");
        Connection::open(&path)
            .unwrap()
            .execute_batch("CREATE TABLE data (a)")
            .unwrap();
        assert!(matches!(
            Extract::open(&path),
            Err(ExtractError::NotAnExtract { .. })
        ));

        let text = dir.path().join("text.sqlite");
        std::fs::write(
            &text,
            b"this is not a database at all, not even close......",
        )
        .unwrap();
        assert!(Extract::open(&text).is_err());
    }

    #[test]
    fn non_blob_geometry_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("e.sqlite");
        let db = Connection::open(&path).unwrap();
        db.execute_batch(
            "CREATE TABLE data (geom); INSERT INTO data VALUES ('POINT(1 2)');
             CREATE TABLE columbus_extract (key TEXT PRIMARY KEY, value TEXT);
             INSERT INTO columbus_extract VALUES ('geometry_column', 'geom'), ('geometry_format', 'ewkb');",
        )
        .unwrap();
        drop(db);
        let ex = Extract::open(&path).unwrap();
        let row = ex.rows().unwrap().next().unwrap().unwrap();
        assert_eq!(row.geometry(), Some(Err(GeomError::NotBlob("text"))));
    }
}
