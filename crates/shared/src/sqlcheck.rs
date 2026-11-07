//! Query checking (design section 3, "Query checking").
//!
//! * `check_where` accepts a where clause made only of comparisons,
//!   `AND`/`OR`/`NOT`, `IN`, `LIKE`, `BETWEEN`, `IS NULL` and an allow list
//!   of functions. No subqueries.
//! * `check_select` accepts free SQL only when it parses, in the
//!   connection's dialect, as one `SELECT` whose every table is allowed. It
//!   then rewrites every table reference to a filtered derived table so the
//!   connection's row filter and data-group filters always apply, and adds a
//!   row limit.
//!
//! Anything that does not parse is refused, never passed through. This is a
//! second line of defence: read connections also run in read-only
//! transactions, and the real guarantee is a read-only database login.

use std::ops::ControlFlow;

use serde::Serialize;
use sqlparser::ast::{
    visit_expressions, visit_expressions_mut, visit_relations, Expr, Ident, ObjectName, Query,
    SetExpr, Statement, TableAlias, TableFactor, VisitMut, VisitorMut,
};
use sqlparser::dialect::{
    Dialect, DuckDbDialect, GenericDialect, MsSqlDialect, OracleDialect, PostgreSqlDialect,
    SQLiteDialect,
};
use sqlparser::parser::Parser;

use crate::error::Denied;
use crate::policy::{normalize_identifier, EffectiveAccess};

/// SQL dialect of a connection, chosen from its type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum SqlDialect {
    Oracle,
    Postgres,
    MsSql,
    Sqlite,
    DuckDb,
    Generic,
}

impl SqlDialect {
    pub fn for_connection_type(kind: &str) -> Self {
        match kind.to_ascii_lowercase().as_str() {
            "oracle" | "sde-oracle" => SqlDialect::Oracle,
            "postgis" | "postgres" | "postgresql" => SqlDialect::Postgres,
            "sqlserver" | "mssql" | "azure-sql" | "sde-sqlserver" => SqlDialect::MsSql,
            // Shapefiles and file geodatabases run in GDAL's SQLite dialect.
            "gpkg" | "geopackage" | "sqlite" | "spatialite" | "shapefile" | "filegdb" => {
                SqlDialect::Sqlite
            }
            "duckdb" | "geoparquet" => SqlDialect::DuckDb,
            _ => SqlDialect::Generic,
        }
    }

    fn parser_dialect(self) -> Box<dyn Dialect> {
        match self {
            SqlDialect::Oracle => Box::new(OracleDialect {}),
            SqlDialect::Postgres => Box::new(PostgreSqlDialect {}),
            SqlDialect::MsSql => Box::new(MsSqlDialect {}),
            SqlDialect::Sqlite => Box::new(SQLiteDialect {}),
            SqlDialect::DuckDb => Box::new(DuckDbDialect {}),
            SqlDialect::Generic => Box::new(GenericDialect {}),
        }
    }
}

/// Functions allowed in where clauses. Upper-case, unqualified.
const WHERE_FUNCTIONS: &[&str] = &[
    "UPPER",
    "LOWER",
    "TRIM",
    "LTRIM",
    "RTRIM",
    "SUBSTR",
    "SUBSTRING",
    "LENGTH",
    "LEN",
    "ABS",
    "ROUND",
    "FLOOR",
    "CEIL",
    "CEILING",
    "COALESCE",
    "NVL",
    "ISNULL",
    "IFNULL",
    "TO_DATE",
    "TO_CHAR",
    "TO_NUMBER",
    "CAST",
    "LPAD",
    "RPAD",
    "REPLACE",
    "CURRENT_DATE",
    "DATE",
    "YEAR",
    "MONTH",
    "EXTRACT",
    "MOD",
];

/// Functions free SQL may call: ordinary scalar, aggregate and window
/// functions, plus the PostGIS / SpatiaLite `ST_*` family (less
/// [`ST_DENIED`]). Anything else is refused. An allow list, not a deny list,
/// because some functions run SQL given to them as text
/// (`query_to_xml('SELECT …')`, `ts_stat`) or read a table by name
/// (`table_to_xml`), and what they read is never checked, filtered or
/// stripped of hidden fields.
const SELECT_FUNCTIONS: &[&str] = &[
    // aggregates
    "COUNT",
    "SUM",
    "AVG",
    "MIN",
    "MAX",
    "STDDEV",
    "STDDEV_POP",
    "STDDEV_SAMP",
    "VARIANCE",
    "VAR_POP",
    "VAR_SAMP",
    "STRING_AGG",
    "ARRAY_AGG",
    "BOOL_AND",
    "BOOL_OR",
    "EVERY",
    "PERCENTILE_CONT",
    "PERCENTILE_DISC",
    "MODE",
    "GROUP_CONCAT",
    "TOTAL",
    "JSON_AGG",
    "JSONB_AGG",
    "JSON_OBJECT_AGG",
    "JSONB_OBJECT_AGG",
    "CORR",
    "COVAR_POP",
    "COVAR_SAMP",
    "MEDIAN",
    "LISTAGG",
    // window
    "ROW_NUMBER",
    "RANK",
    "DENSE_RANK",
    "PERCENT_RANK",
    "CUME_DIST",
    "NTILE",
    "LAG",
    "LEAD",
    "FIRST_VALUE",
    "LAST_VALUE",
    "NTH_VALUE",
    // numbers
    "ABS",
    "CEIL",
    "CEILING",
    "FLOOR",
    "ROUND",
    "TRUNC",
    "SIGN",
    "SQRT",
    "POWER",
    "POW",
    "EXP",
    "LN",
    "LOG",
    "LOG10",
    "MOD",
    "PI",
    "DEGREES",
    "RADIANS",
    "SIN",
    "COS",
    "TAN",
    "ASIN",
    "ACOS",
    "ATAN",
    "ATAN2",
    "WIDTH_BUCKET",
    // conditionals
    "GREATEST",
    "LEAST",
    "COALESCE",
    "NULLIF",
    "IFNULL",
    "NVL",
    "IIF",
    // text
    "LOWER",
    "UPPER",
    "LENGTH",
    "CHAR_LENGTH",
    "CHARACTER_LENGTH",
    "OCTET_LENGTH",
    "TRIM",
    "LTRIM",
    "RTRIM",
    "BTRIM",
    "LPAD",
    "RPAD",
    "SUBSTR",
    "SUBSTRING",
    "LEFT",
    "RIGHT",
    "REPLACE",
    "CONCAT",
    "CONCAT_WS",
    "POSITION",
    "STRPOS",
    "INSTR",
    "INITCAP",
    "REVERSE",
    "REPEAT",
    "SPLIT_PART",
    "REGEXP_REPLACE",
    "REGEXP_MATCHES",
    "REGEXP_SUBSTR",
    "TRANSLATE",
    "MD5",
    "HEX",
    "TYPEOF",
    "TO_CHAR",
    "TO_NUMBER",
    "TO_DATE",
    "TO_TIMESTAMP",
    // dates
    "NOW",
    "CURRENT_DATE",
    "CURRENT_TIMESTAMP",
    "DATE_TRUNC",
    "DATE_PART",
    "EXTRACT",
    "AGE",
    "DATE",
    "DATETIME",
    "JULIANDAY",
    "STRFTIME",
    "MAKE_DATE",
    "YEAR",
    "MONTH",
    "DAY",
    // JSON built from the row values the query can already read
    "JSON_BUILD_OBJECT",
    "JSONB_BUILD_OBJECT",
    "JSON_BUILD_ARRAY",
    "JSONB_BUILD_ARRAY",
    "TO_JSON",
    "TO_JSONB",
    "ROW_TO_JSON",
    "JSON_OBJECT",
    "JSON_ARRAY",
    "JSON_EXTRACT",
    // arrays and sets (set-returning ones only in the select list: FROM
    // accepts plain tables only)
    "STRING_TO_ARRAY",
    "ARRAY_TO_STRING",
    "ARRAY_LENGTH",
    "REGEXP_SPLIT_TO_ARRAY",
    "UNNEST",
    "GENERATE_SERIES",
    "CARDINALITY",
    // more JSON
    "JSON_EXTRACT_PATH_TEXT",
    "JSONB_EXTRACT_PATH_TEXT",
    "JSON_ARRAY_LENGTH",
    "JSONB_ARRAY_LENGTH",
    "JSON_ARRAY_ELEMENTS",
    "JSONB_ARRAY_ELEMENTS",
    "JSONB_PRETTY",
    // more text, dates and statistics
    "FORMAT",
    "CHR",
    "ASCII",
    "RANDOM",
    "MAKE_INTERVAL",
    "LOCALTIMESTAMP",
    "REGR_SLOPE",
    "REGR_INTERCEPT",
    "REGR_R2",
    "REGR_COUNT",
    "REGR_AVGX",
    "REGR_AVGY",
    "REGR_SXX",
    "REGR_SYY",
    "REGR_SXY",
];

/// PostGIS functions not named `ST_*`. Like `ST_*`, on PostgreSQL they are
/// callable only when the PostGIS extension provides them.
const SPATIAL_OTHER: &[&str] = &["GEOMETRY", "GEOGRAPHY", "BOX2D", "BOX3D", "GEOMETRYTYPE"];

/// Types a cast may name beyond the built-in ones the parser knows.
const CAST_TYPES: &[&str] = &["GEOMETRY", "GEOGRAPHY", "BOX2D", "BOX3D", "REGCLASS"];

/// `ST_*` functions refused: they read a table by name (and so outside the
/// row filter), or they are PostGIS internals.
const ST_DENIED: &[&str] = &["ST_ESTIMATEDEXTENT", "ST_FIND_EXTENT", "ST_FINDEXTENT"];

/// The functions free SQL may call on Oracle: the built-in SQL functions
/// among [`SELECT_FUNCTIONS`] (plus DECODE and NVL2), never a name Oracle
/// lacks. Oracle resolves a name that is not a built-in to the login's own
/// schema or a public synonym, so `SPLIT_PART(...)` would run whatever
/// function someone created under that name.
const ORACLE_FUNCTIONS: &[&str] = &[
    "COUNT",
    "SUM",
    "AVG",
    "MIN",
    "MAX",
    "STDDEV",
    "STDDEV_POP",
    "STDDEV_SAMP",
    "VARIANCE",
    "VAR_POP",
    "VAR_SAMP",
    "PERCENTILE_CONT",
    "PERCENTILE_DISC",
    "CORR",
    "COVAR_POP",
    "COVAR_SAMP",
    "MEDIAN",
    "LISTAGG",
    "ROW_NUMBER",
    "RANK",
    "DENSE_RANK",
    "PERCENT_RANK",
    "CUME_DIST",
    "NTILE",
    "LAG",
    "LEAD",
    "FIRST_VALUE",
    "LAST_VALUE",
    "NTH_VALUE",
    "ABS",
    "CEIL",
    "FLOOR",
    "ROUND",
    "TRUNC",
    "SIGN",
    "SQRT",
    "POWER",
    "EXP",
    "LN",
    "LOG",
    "MOD",
    "SIN",
    "COS",
    "TAN",
    "ASIN",
    "ACOS",
    "ATAN",
    "ATAN2",
    "WIDTH_BUCKET",
    "GREATEST",
    "LEAST",
    "COALESCE",
    "NULLIF",
    "NVL",
    "NVL2",
    "DECODE",
    "LOWER",
    "UPPER",
    "LENGTH",
    "TRIM",
    "LTRIM",
    "RTRIM",
    "LPAD",
    "RPAD",
    "SUBSTR",
    "REPLACE",
    "CONCAT",
    "INSTR",
    "INITCAP",
    "REGEXP_REPLACE",
    "REGEXP_SUBSTR",
    "TRANSLATE",
    "CHR",
    "ASCII",
    "TO_CHAR",
    "TO_NUMBER",
    "TO_DATE",
    "TO_TIMESTAMP",
    "CURRENT_DATE",
    "CURRENT_TIMESTAMP",
    "LOCALTIMESTAMP",
    "EXTRACT",
    "JSON_OBJECT",
    "JSON_ARRAY",
    "REGR_SLOPE",
    "REGR_INTERCEPT",
    "REGR_R2",
    "REGR_COUNT",
    "REGR_AVGX",
    "REGR_AVGY",
    "REGR_SXX",
    "REGR_SYY",
    "REGR_SXY",
];

/// Oracle Spatial operators free SQL may call (index operators; written
/// bare or as `MDSYS.<name>`).
const SDO_OPERATORS: &[&str] = &["SDO_RELATE", "SDO_FILTER", "SDO_WITHIN_DISTANCE"];
/// Oracle Spatial packages and types whose members free SQL may call
/// (`SDO_GEOM.SDO_AREA(...)`, `SDO_UTIL.TO_WKTGEOMETRY(...)`), with the
/// members allowed (`*` for every one: SDO_GEOM only computes).
const SDO_PACKAGES: &[(&str, &[&str])] = &[
    ("SDO_GEOM", &["*"]),
    (
        "SDO_UTIL",
        &[
            "TO_WKTGEOMETRY",
            "TO_WKBGEOMETRY",
            "FROM_WKTGEOMETRY",
            "FROM_WKBGEOMETRY",
            "TO_GEOJSON",
        ],
    ),
];
/// Oracle Spatial type constructors.
const SDO_TYPES: &[&str] = &[
    "SDO_GEOMETRY",
    "SDO_POINT_TYPE",
    "SDO_ELEM_INFO_ARRAY",
    "SDO_ORDINATE_ARRAY",
];

/// An Oracle Spatial name as free SQL wrote it (upper-case parts), as the
/// name parts to call it by: always in MDSYS, so a function or synonym of
/// the same name in the login's schema (which Oracle would find first)
/// never runs instead.
fn oracle_spatial(parts: &[String]) -> Option<Vec<String>> {
    let rest: &[String] = match parts {
        [first, rest @ ..] if first == "MDSYS" && !rest.is_empty() => rest,
        _ => parts,
    };
    let ok = match rest {
        [one] => SDO_OPERATORS.contains(&one.as_str()) || SDO_TYPES.contains(&one.as_str()),
        [pkg, member] => SDO_PACKAGES
            .iter()
            .any(|(p, m)| p == pkg && (m.contains(&"*") || m.contains(&member.as_str()))),
        _ => false,
    };
    ok.then(|| {
        std::iter::once("MDSYS".to_string())
            .chain(rest.iter().cloned())
            .collect()
    })
}

/// Upper-case name parts of a function, or None when a part is not a
/// plain identifier.
fn function_parts(name: &ObjectName) -> Option<Vec<String>> {
    name.0
        .iter()
        .map(|p| p.as_ident().map(|i| i.value.to_ascii_uppercase()))
        .collect()
}

/// May free SQL call this function (upper-case, possibly schema-qualified)?
///
/// On PostgreSQL a spatial function must be one the PostGIS extension
/// installed ([`Catalog::extension_functions`]): any function in `public`
/// can be named `st_…`, and one the database owner wrote can read anything.
/// On GeoPackage and SQLite files none are available (see below). On Oracle
/// only built-ins and Oracle Spatial in MDSYS; on SQL Server only
/// unqualified built-ins (it calls user-defined scalar functions only by a
/// two-part name, and a two-part name could as well be a geometry method).
fn select_function_allowed(
    name: &str,
    parts: Option<&[String]>,
    dialect: SqlDialect,
    catalog: &Catalog,
) -> bool {
    let (schema, bare) = match name.rsplit_once('.') {
        Some((s, b)) => (Some(s), b),
        None => (None, name),
    };
    if bare.starts_with('_') || bare.starts_with("POSTGIS") {
        return false;
    }
    match dialect {
        SqlDialect::Oracle => {
            return parts.is_some_and(|p| oracle_spatial(p).is_some())
                || (schema.is_none() && ORACLE_FUNCTIONS.contains(&bare));
        }
        SqlDialect::MsSql => {
            let spatial = bare.starts_with("ST_") || SPATIAL_OTHER.contains(&bare);
            return schema.is_none() && !spatial && SELECT_FUNCTIONS.contains(&bare);
        }
        _ => {}
    }
    let spatial = bare.starts_with("ST_") || SPATIAL_OTHER.contains(&bare);
    if spatial {
        if ST_DENIED.contains(&bare) {
            return false;
        }
        return match dialect {
            SqlDialect::Postgres | SqlDialect::DuckDb => {
                match catalog.extension_functions.get(bare) {
                    Some(ext_schema) => schema.is_none_or(|s| s.eq_ignore_ascii_case(ext_schema)),
                    None => false,
                }
            }
            // The engine opens GeoPackage and SQLite files with plain SQLite:
            // no spatial functions are registered, so they would only fail.
            SqlDialect::Sqlite => false,
            _ => schema.is_none(),
        };
    }
    // Ordinary functions: unqualified (pg_catalog is searched first) or,
    // on PostgreSQL and DuckDB, explicitly pg_catalog. A function of the
    // database's own (roads.log_visit) can do anything.
    let pg = matches!(dialect, SqlDialect::Postgres | SqlDialect::DuckDb);
    schema.is_none_or(|s| pg && s == "PG_CATALOG") && SELECT_FUNCTIONS.contains(&bare)
}

/// Why a function free SQL may not call is refused, in the dialect's terms.
fn function_refusal(name: &str, dialect: SqlDialect) -> String {
    let bare = name.rsplit('.').next().unwrap_or(name);
    let spatial = bare.starts_with("ST_") || SPATIAL_OTHER.contains(&bare);
    match dialect {
        SqlDialect::Sqlite if spatial => format!(
            "function {name} is not available: spatial SQL functions do not run on GeoPackage or SQLite connections; use query_features with bbox, or vector_op"
        ),
        SqlDialect::Oracle if spatial || bare.starts_with("SDO_") => format!(
            "function {name} is not allowed on Oracle: spatial SQL there is Oracle Spatial's, called in MDSYS (SDO_RELATE, SDO_FILTER, SDO_WITHIN_DISTANCE, SDO_GEOM.*, SDO_UTIL.TO_WKTGEOMETRY); or use query_features with bbox, or vector_op"
        ),
        SqlDialect::Oracle => format!(
            "function {name} is not allowed (Oracle's built-in scalar, aggregate and window functions and Oracle Spatial in MDSYS are; the database's own functions are not)"
        ),
        SqlDialect::MsSql if spatial || name.contains('.') => format!(
            "{name} is not allowed on SQL Server: geometry methods (col.STIntersects(...)) and schema functions cannot be told apart safely, so neither is called; use query_features with bbox, or vector_op, for spatial work"
        ),
        SqlDialect::MsSql => format!(
            "function {name} is not allowed (ordinary built-in scalar, aggregate and window functions are)"
        ),
        _ => format!(
            "function {name} is not allowed (ordinary scalar, aggregate and window functions and PostGIS ST_* functions are)"
        ),
    }
}

/// Built-in Oracle types a cast may name that the parser does not know.
const ORACLE_CAST_TYPES: &[&str] = &[
    "NUMBER",
    "VARCHAR2",
    "NVARCHAR2",
    "NCHAR",
    "BINARY_DOUBLE",
    "BINARY_FLOAT",
];

/// Function name prefixes refused anywhere in free SQL: these reach the file
/// system, the network or server internals on at least one supported database.
const DENIED_FUNCTION_PREFIXES: &[&str] = &[
    "PG_",
    "LO_",
    "DBLINK",
    "DBMS_",
    "UTL_",
    "SYS.",
    "SYS_",
    "XP_",
    "SP_",
    "OPENROWSET",
    "OPENDATASOURCE",
    "OPENQUERY",
    "LOAD_FILE",
    "LOAD_EXTENSION",
    "READFILE",
    "WRITEFILE",
    "READ_CSV",
    "READ_PARQUET",
    "READ_JSON",
    "READ_TEXT",
    "READ_BLOB",
    "GLOB",
    "HTTPFS",
    "SET_CONFIG",
    "CURRENT_SETTING",
    "COPY",
    "EDITFILE",
    "FTS3_TOKENIZER",
];

/// A query that passed checking, ready for a worker.
#[derive(Debug, Clone, Serialize)]
pub struct CheckedQuery {
    /// The SQL to run: the original, with tables wrapped in filtered derived
    /// tables and a row limit added.
    pub sql: String,
    /// Tables the query reads, normalized.
    pub tables: Vec<String>,
    /// Maximum rows the worker may read; it must stop there regardless. The
    /// SQL itself fetches one more, so the worker can detect truncation.
    pub max_rows: u64,
    pub dialect: SqlDialect,
    /// Fields the policy hides that were left out of the tables read.
    pub hidden: Vec<String>,
}

fn parse_expr(text: &str, dialect: SqlDialect) -> Result<Expr, Denied> {
    let d = dialect.parser_dialect();
    let mut p = Parser::new(d.as_ref())
        .try_with_sql(text)
        .map_err(|e| Denied::Query(format!("could not parse where clause: {e}")))?;
    let expr = p
        .parse_expr()
        .map_err(|e| Denied::Query(format!("could not parse where clause: {e}")))?;
    // Anything left over (a second statement, a trailing clause) is refused.
    if p.peek_token().token != sqlparser::tokenizer::Token::EOF {
        return Err(Denied::Query(
            "where clause has trailing text after the expression".into(),
        ));
    }
    Ok(expr)
}

fn function_name(name: &ObjectName) -> String {
    name.to_string().to_ascii_uppercase()
}

/// The name a filtered table goes by inside its filter's own query.
const FILTER_ALIAS: &str = "columbus_filtered";

/// Bare names Oracle reads as values, not columns (`EXPIRES > SYSDATE`):
/// left as they are in a filter. They are on Oracle's list of reserved
/// words, so no column of the query around the filter can be called that.
/// (SYSTIMESTAMP and CURRENT_DATE are not reserved, so a filter can't use
/// them: a caller's column of that name might be the one they read.)
const FILTER_KEYWORDS: &[&str] = &["SYSDATE", "USER", "UID"];

/// Names the server gives its own parts of a query: free SQL may not use
/// them anywhere, so no name the caller writes can stand in for them.
const RESERVED_NAMES: &[&str] = &[FILTER_ALIAS, "columbus_q"];

fn filter_keyword(i: &Ident, dialect: SqlDialect) -> bool {
    i.quote_style.is_none()
        && dialect == SqlDialect::Oracle
        && FILTER_KEYWORDS.contains(&i.value.to_ascii_uppercase().as_str())
}

/// A checked filter with each column it names bound to `alias` (the
/// filtered table): `STATUS <> 'R'` becomes `columbus_filtered.STATUS <>
/// 'R'`, and `t.STATUS` too. So a name the table doesn't have is an error,
/// never a column of the query the filtered table sits in. With the
/// table's `columns` known, a name it lacks is refused here, whatever the
/// database would make of it (SQLite and DuckDB look further out for a
/// qualified name the nearest table lacks). Its functions are named as
/// the query's own are (`pin_function`).
fn bind_filter(
    checked: &str,
    dialect: SqlDialect,
    alias: &str,
    columns: Option<&[String]>,
    catalog: &Catalog,
) -> Result<String, Denied> {
    bind_filter_dates(checked, dialect, alias, columns, catalog, &[])
}

/// As [`bind_filter`], refusing a filter that names one of `text_dates`
/// (lower case): dates that compare as text in another form.
fn bind_filter_dates(
    checked: &str,
    dialect: SqlDialect,
    alias: &str,
    columns: Option<&[String]>,
    catalog: &Catalog,
    text_dates: &[String],
) -> Result<String, Denied> {
    let mut expr = parse_expr(checked, dialect)?;
    let mut missing: Option<String> = None;
    let mut bare_value: Option<String> = None;
    let mut text_date: Option<String> = None;
    let mut note = |i: &Ident| {
        if let Some(cols) = columns {
            if missing.is_none() && !cols.iter().any(|c| c.eq_ignore_ascii_case(&i.value)) {
                missing = Some(i.value.clone());
            }
        }
        if text_date.is_none() && text_dates.iter().any(|d| d.eq_ignore_ascii_case(&i.value)) {
            text_date = Some(i.value.clone());
        }
    };
    let _ = visit_expressions_mut(&mut expr, |e| {
        match e {
            Expr::Identifier(i) if filter_keyword(i, dialect) => {}
            Expr::Identifier(i) => {
                note(i);
                *e = Expr::CompoundIdentifier(vec![Ident::new(alias), i.clone()]);
            }
            Expr::CompoundIdentifier(parts) => {
                if let Some(last) = parts.last().cloned() {
                    note(&last);
                    *parts = vec![Ident::new(alias), last];
                }
            }
            Expr::Function(f)
                if dialect == SqlDialect::Oracle
                    && matches!(f.args, sqlparser::ast::FunctionArguments::None) =>
            {
                // CURRENT_DATE and the like: not reserved on Oracle, so a
                // column of the query around might stand in for one.
                if bare_value.is_none() {
                    bare_value = Some(f.name.to_string());
                }
            }
            _ => pin_function(e, dialect, catalog),
        }
        ControlFlow::<()>::Continue(())
    });
    if let Some(v) = bare_value {
        return Err(Denied::Query(format!(
            "{v} can't be used in a row filter on Oracle; use SYSDATE"
        )));
    }
    if let Some(m) = missing {
        return Err(Denied::Query(format!(
            "it names {m}, which the table does not have"
        )));
    }
    if let Some(d) = text_date {
        return Err(Denied::Query(format!(
            "it names {d}, a date this source reads as text (YYYY/MM/DD), so the filter could keep rows it means to drop"
        )));
    }
    Ok(expr.to_string())
}

/// A filter for a single-table edit (`UPDATE t ... WHERE key = ? AND
/// (filter)`): checked, with each name its last part (`p.district` is the
/// edited table's `district`), keywords and functions as for reads.
pub fn edit_filter(text: &str, dialect: SqlDialect) -> Result<String, Denied> {
    let checked = check_where(text, dialect)?;
    let mut expr = parse_expr(&checked, dialect)?;
    let _ = visit_expressions_mut(&mut expr, |e| {
        if let Expr::CompoundIdentifier(parts) = e {
            if let Some(last) = parts.last().cloned() {
                *e = Expr::Identifier(last);
            }
        }
        ControlFlow::<()>::Continue(())
    });
    let out = expr.to_string();
    // The same checks a read's filter gets (bound to an alias only to check).
    bind_filter(&out, dialect, FILTER_ALIAS, None, &Catalog::default())?;
    Ok(out)
}

/// Refuse free SQL that uses one of the server's own names anywhere: as a
/// name in any case or quoting, or as a string (SQLite takes `AS 'name'`
/// as an alias).
fn refuse_reserved_names(sql: &str, dialect: SqlDialect) -> Result<(), Denied> {
    use sqlparser::tokenizer::Token;
    let d = dialect.parser_dialect();
    let tokens = sqlparser::tokenizer::Tokenizer::new(d.as_ref(), sql)
        .tokenize()
        .map_err(|e| Denied::Query(format!("could not parse SQL: {e}")))?;
    for t in tokens {
        let value = match &t {
            Token::Word(w) => w.value.as_str(),
            Token::SingleQuotedString(v)
            | Token::DoubleQuotedString(v)
            | Token::TripleSingleQuotedString(v)
            | Token::TripleDoubleQuotedString(v)
            | Token::NationalStringLiteral(v)
            | Token::EscapedStringLiteral(v)
            | Token::UnicodeStringLiteral(v) => v.as_str(),
            _ => continue,
        };
        let v = value.trim();
        if RESERVED_NAMES.iter().any(|r| v.eq_ignore_ascii_case(r)) {
            return Err(Denied::Query(format!(
                "{v} is a name Columbus uses inside the queries it runs; use another name"
            )));
        }
    }
    Ok(())
}

/// Whether a function's arguments include `*` or `t.*`: a whole row, from
/// any table in scope, is never a filter's to name.
fn wildcard_argument(args: &sqlparser::ast::FunctionArguments) -> bool {
    use sqlparser::ast::{FunctionArg, FunctionArgExpr, FunctionArguments};
    let FunctionArguments::List(list) = args else {
        return false;
    };
    list.args.iter().any(|a| {
        let x = match a {
            FunctionArg::Unnamed(x) => x,
            FunctionArg::Named { arg, .. } | FunctionArg::ExprNamed { arg, .. } => arg,
        };
        matches!(
            x,
            FunctionArgExpr::Wildcard | FunctionArgExpr::QualifiedWildcard(_)
        )
    })
}

/// Check a where clause. Returns it re-rendered from the parsed form, so
/// what runs is exactly what was checked.
pub fn check_where(text: &str, dialect: SqlDialect) -> Result<String, Denied> {
    let expr = parse_expr(text, dialect)?;
    let mut problem: Option<String> = None;
    let _ = visit_expressions(&expr, |e| {
        let ok = match e {
            Expr::Identifier(_)
            | Expr::CompoundIdentifier(_)
            | Expr::Value(_)
            | Expr::TypedString { .. }
            | Expr::Nested(_)
            | Expr::IsNull(_)
            | Expr::IsNotNull(_)
            | Expr::IsTrue(_)
            | Expr::IsFalse(_)
            | Expr::InList { .. }
            | Expr::Between { .. }
            | Expr::Like { .. }
            | Expr::ILike { .. }
            | Expr::Cast { .. }
            | Expr::Extract { .. }
            | Expr::Substring { .. }
            | Expr::Trim { .. } => true,
            Expr::UnaryOp { op, .. } => {
                use sqlparser::ast::UnaryOperator::*;
                matches!(op, Not | Minus | Plus)
            }
            Expr::BinaryOp { op, .. } => {
                use sqlparser::ast::BinaryOperator::*;
                matches!(
                    op,
                    Eq | NotEq
                        | Lt
                        | LtEq
                        | Gt
                        | GtEq
                        | And
                        | Or
                        | Plus
                        | Minus
                        | Multiply
                        | Divide
                        | Modulo
                        | StringConcat
                )
            }
            Expr::Function(f) => {
                let name = function_name(&f.name);
                if wildcard_argument(&f.args) {
                    problem = Some(format!("`{e}`: * is not allowed in a where clause"));
                    return ControlFlow::Break(());
                }
                if WHERE_FUNCTIONS.contains(&name.as_str()) {
                    true
                } else {
                    problem = Some(format!("function {name} is not allowed in a where clause"));
                    return ControlFlow::Break(());
                }
            }
            _ => false,
        };
        if ok {
            ControlFlow::Continue(())
        } else {
            problem = Some(format!(
                "`{e}` is not allowed in a where clause (only comparisons, AND/OR/NOT, IN, LIKE, BETWEEN, IS NULL and simple functions)"
            ));
            ControlFlow::Break(())
        }
    });
    match problem {
        Some(p) => Err(Denied::Query(p)),
        None => {
            let out = expr.to_string();
            let same = parse_expr(&out, dialect).is_ok_and(|again| again.to_string() == out);
            if same {
                Ok(out)
            } else {
                Err(Denied::Query(
                    "the where clause could not be checked reliably (a name or literal does not read back as written)".into(),
                ))
            }
        }
    }
}

fn single_select(sql: &str, dialect: SqlDialect) -> Result<Query, Denied> {
    let d = dialect.parser_dialect();
    let mut stmts = Parser::parse_sql(d.as_ref(), sql)
        .map_err(|e| Denied::Query(format!("could not parse SQL: {e}")))?;
    if stmts.len() != 1 {
        return Err(Denied::Query(format!(
            "expected exactly one statement, found {}",
            stmts.len()
        )));
    }
    let query = match stmts.remove(0) {
        Statement::Query(q) => *q,
        other => {
            let kind = other.to_string();
            let first = kind.split_whitespace().next().unwrap_or("").to_uppercase();
            return Err(Denied::Query(format!(
                "only SELECT is allowed, not {first}"
            )));
        }
    };
    if query.with.is_some() {
        return Err(Denied::Query(
            "WITH (common table expressions) is not supported; use a subquery in FROM".into(),
        ));
    }
    if !query.locks.is_empty() {
        return Err(Denied::Query(
            "locking clauses (FOR UPDATE) are not allowed".into(),
        ));
    }
    if query.for_clause.is_some() || query.settings.is_some() || query.format_clause.is_some() {
        return Err(Denied::Query(
            "FOR/SETTINGS/FORMAT clauses are not allowed".into(),
        ));
    }
    if !query.pipe_operators.is_empty() {
        return Err(Denied::Query("pipe syntax is not supported".into()));
    }
    check_set_expr(&query.body)?;
    Ok(query)
}

fn check_set_expr(body: &SetExpr) -> Result<(), Denied> {
    match body {
        SetExpr::Select(s) => {
            if s.into.is_some() {
                return Err(Denied::Query("SELECT INTO is not allowed".into()));
            }
            Ok(())
        }
        SetExpr::Query(q) => check_set_expr(&q.body),
        SetExpr::SetOperation { left, right, .. } => {
            check_set_expr(left)?;
            check_set_expr(right)
        }
        _ => Err(Denied::Query("only SELECT queries are allowed".into())),
    }
}

/// Collects table references and refuses table factors that are not plain
/// tables, derived tables or joins (table functions, UNNEST and the like can
/// read files or bypass the allow list).
fn check_tables_and_functions(
    query: &Query,
    access: &EffectiveAccess,
    dialect: SqlDialect,
    catalog: &Catalog,
) -> Result<Vec<String>, Denied> {
    let mut tables = Vec::new();
    let mut problem: Option<Denied> = None;
    let _ = visit_relations(query, |rel| {
        // Every rule applies to the table the database will read, never to
        // the caller's spelling of it (a bare name that resolves to a denied
        // schema.table must be denied).
        let name = if catalog.resolve {
            match resolve(rel, &catalog.relations, dialect) {
                Ok(r) => r.parts.join("."),
                // Not a table the connection lists (system catalogs, other
                // schemas, tables the login cannot see): not allowed.
                Err(e) if e.starts_with("cannot check") => {
                    problem = Some(Denied::Dataset(normalize_identifier(&rel.to_string())));
                    return ControlFlow::Break(());
                }
                Err(e) => {
                    problem = Some(Denied::Query(e));
                    return ControlFlow::Break(());
                }
            }
        } else {
            normalize_identifier(&rel.to_string())
        };
        if !access.dataset_allowed(&name) {
            problem = Some(Denied::Dataset(name));
            return ControlFlow::Break(());
        }
        if !tables.contains(&name) {
            tables.push(name);
        }
        ControlFlow::Continue(())
    });
    if let Some(p) = problem {
        return Err(p);
    }

    struct FactorCheck(Option<String>);
    impl sqlparser::ast::Visitor for FactorCheck {
        type Break = ();
        fn pre_visit_table_factor(&mut self, tf: &TableFactor) -> ControlFlow<()> {
            match tf {
                TableFactor::Table {
                    sample: Some(_), ..
                }
                | TableFactor::Derived {
                    sample: Some(_), ..
                } => {
                    self.0 = Some(
                        "TABLESAMPLE is not allowed; use sample_rows, or LIMIT with ORDER BY"
                            .into(),
                    );
                    ControlFlow::Break(())
                }
                TableFactor::Table {
                    args: None,
                    version: None,
                    ..
                }
                | TableFactor::Derived { .. }
                | TableFactor::NestedJoin { .. } => ControlFlow::Continue(()),
                TableFactor::Table { .. } => {
                    self.0 = Some("table arguments and time-travel clauses are not allowed".into());
                    ControlFlow::Break(())
                }
                other => {
                    self.0 = Some(format!(
                        "`{other}` is not allowed in FROM; use plain tables"
                    ));
                    ControlFlow::Break(())
                }
            }
        }
    }
    let mut fc = FactorCheck(None);
    let _ = sqlparser::ast::Visit::visit(query, &mut fc);
    if let Some(p) = fc.0 {
        return Err(Denied::Query(p));
    }

    let _ = visit_expressions(query, |e| {
        if let Expr::Function(f) = e {
            let name = function_name(&f.name);
            let bare = name.rsplit('.').next().unwrap_or(&name);
            let parts = function_parts(&f.name);
            let spatial_ok = dialect == SqlDialect::Oracle
                && parts.as_deref().and_then(oracle_spatial).is_some();
            if (!spatial_ok
                && DENIED_FUNCTION_PREFIXES
                    .iter()
                    .any(|p| name.starts_with(p) || bare.starts_with(p)))
                || !select_function_allowed(&name, parts.as_deref(), dialect, catalog)
            {
                problem = Some(Denied::Query(function_refusal(&name, dialect)));
                return ControlFlow::Break(());
            }
        }
        // Operators and casts a database can define run its own code.
        match e {
            Expr::BinaryOp {
                op:
                    sqlparser::ast::BinaryOperator::PGCustomBinaryOperator(_)
                    | sqlparser::ast::BinaryOperator::Custom(_),
                ..
            } => {
                problem = Some(Denied::Query(format!(
                    "operator in `{e}` is not allowed; use the standard operators"
                )));
                return ControlFlow::Break(());
            }
            // `a::t` is PostgreSQL's cast; SQL Server's `geometry::Point(...)`
            // (a static method) parses the same way but is something else.
            Expr::Cast {
                kind: sqlparser::ast::CastKind::DoubleColon,
                ..
            } if !matches!(dialect, SqlDialect::Postgres | SqlDialect::DuckDb) => {
                problem = Some(Denied::Query(if dialect == SqlDialect::MsSql {
                    format!("`{e}` is not allowed on SQL Server: static geometry methods (geometry::Point, geography::STGeomFromText) are not checked; use query_features with bbox, or vector_op")
                } else {
                    format!("`{e}` is not allowed; use CAST(... AS ...)")
                }));
                return ControlFlow::Break(());
            }
            Expr::Cast {
                data_type: sqlparser::ast::DataType::Custom(t, modifiers),
                ..
            } => {
                let last =
                    t.0.last()
                        .and_then(|p| p.as_ident())
                        .map(|i| i.value.to_ascii_uppercase())
                        .unwrap_or_default();
                let known = match dialect {
                    SqlDialect::Oracle => ORACLE_CAST_TYPES.contains(&last.as_str()),
                    SqlDialect::MsSql => ["GEOMETRY", "GEOGRAPHY"].contains(&last.as_str()),
                    _ => CAST_TYPES.contains(&last.as_str()),
                };
                // Type modifiers are written back as they were read, quotes
                // and all removed: only plain words and numbers may be there.
                let plain = modifiers.iter().all(|m| {
                    !m.is_empty() && m.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
                });
                if t.0.len() > 1 || !known || !plain {
                    problem = Some(Denied::Query(format!(
                        "cast to {t} is not allowed; cast to a built-in type"
                    )));
                    return ControlFlow::Break(());
                }
            }
            _ => {}
        }
        ControlFlow::Continue(())
    });
    if let Some(p) = problem {
        return Err(p);
    }
    if tables.is_empty() {
        return Err(Denied::Query("query reads no tables".into()));
    }
    Ok(tables)
}

/// A friendly early refusal for a query that names a hidden field (in a
/// where clause, an expression, ordering...). It is not what keeps hidden
/// fields in: [`ApplyFilters`] does that by never letting a hidden column
/// into the query at all. Without this check such a query would still fail,
/// just with the database's "no such column" error.
fn check_hidden_references(query: &Query, access: &EffectiveAccess) -> Result<(), Denied> {
    if access.hide_fields.is_empty() {
        return Ok(());
    }
    let mut problem: Option<String> = None;
    let _ = visit_expressions(query, |e| {
        let name = match e {
            Expr::Identifier(id) => Some(&id.value),
            Expr::CompoundIdentifier(parts) => parts.last().map(|p| &p.value),
            _ => None,
        };
        if let Some(n) = name.filter(|n| access.field_hidden(n)) {
            problem = Some(format!(
                "field {n} is hidden by organization policy and cannot be used in a query"
            ));
            return ControlFlow::Break(());
        }
        ControlFlow::Continue(())
    });
    match problem {
        Some(p) => Err(Denied::Query(p)),
        None => Ok(()),
    }
}

/// A relation the connection lists (table, view), by its exact name parts
/// (`["roads", "county"]`) and every column the login can read.
#[derive(Debug, Clone)]
pub struct Relation {
    pub parts: Vec<String>,
    pub columns: Vec<String>,
}

/// What the server knows of a connection's database, for checking free SQL.
#[derive(Debug, Clone, Default)]
pub struct Catalog {
    /// Resolve every table name to one of `relations` and check and read it
    /// by that relation's exact name. On whenever the server could list the
    /// connection; off only where it cannot (then, if the policy hides
    /// fields, any query on a table is refused).
    pub resolve: bool,
    pub relations: Vec<Relation>,
    /// Functions installed by the PostGIS extensions, by upper-case name,
    /// with the schema they live in.
    pub extension_functions: std::collections::BTreeMap<String, String>,
    /// Columns that hold dates but compare as text in another form
    /// (a shapefile's dates are "YYYY/MM/DD" in GDAL's SQLite dialect), as
    /// (relation's parts joined by ".", column), both in lower case: a row
    /// filter naming one could keep rows it means to drop, so it is refused.
    pub text_dates: std::collections::BTreeSet<(String, String)>,
}

/// No catalog: table names are taken as written. For unit tests and
/// connections the server cannot list.
pub static NO_CATALOG: Catalog = Catalog {
    resolve: false,
    relations: Vec::new(),
    extension_functions: std::collections::BTreeMap::new(),
    text_dates: std::collections::BTreeSet::new(),
};

/// Does a name part as the query writes it denote `listed`, by the
/// dialect's rules? Quoted parts are exact; unquoted fold to lower case on
/// PostgreSQL and DuckDB, to upper case on Oracle, and compare without case
/// elsewhere.
fn part_matches(written: &Ident, listed: &str, dialect: SqlDialect) -> bool {
    if written.quote_style.is_some() {
        return written.value == listed;
    }
    match dialect {
        SqlDialect::Postgres | SqlDialect::DuckDb => written.value.to_lowercase() == listed,
        SqlDialect::Oracle => written.value.to_uppercase() == listed,
        _ => written.value.eq_ignore_ascii_case(listed),
    }
}

/// The one listed relation a written table name denotes. A one-part name
/// may match a relation in any schema, but only one; anything ambiguous or
/// unknown is refused, so the server never guesses which table the database
/// will read.
fn resolve<'r>(
    name: &ObjectName,
    relations: &'r [Relation],
    dialect: SqlDialect,
) -> Result<&'r Relation, String> {
    let mut written = Vec::new();
    for p in &name.0 {
        match p.as_ident() {
            Some(i) => written.push(i),
            None => return Err(format!("cannot check the fields of {name}")),
        }
    }
    let hits: Vec<&Relation> = relations
        .iter()
        .filter(|r| {
            let n = written.len();
            let m = r.parts.len();
            (n == m || (n == 1 && m == 2))
                && written
                    .iter()
                    .rev()
                    .zip(r.parts.iter().rev())
                    .all(|(w, l)| part_matches(w, l, dialect))
        })
        .collect();
    match hits.as_slice() {
        [one] => Ok(one),
        [] => Err(format!(
            "cannot check the fields of {name}; name it as list_datasets shows it"
        )),
        _ => Err(format!(
            "{name} could mean more than one table; qualify it with its schema"
        )),
    }
}

fn quote_char(dialect: SqlDialect) -> char {
    match dialect {
        SqlDialect::MsSql => '[',
        _ => '"',
    }
}

fn quote_ident(name: &str, dialect: SqlDialect) -> String {
    match dialect {
        SqlDialect::MsSql => format!("[{}]", name.replace(']', "]]")),
        _ => format!("\"{}\"", name.replace('"', "\"\"")),
    }
}

/// PostgreSQL constructs that look like function calls but are grammar, not
/// pg_catalog functions: they cannot be schema-qualified.
const PG_SYNTAX_FUNCTIONS: &[&str] = &["COALESCE", "NULLIF", "GREATEST", "LEAST"];

/// Text that stops the database from merging the filtered table into the
/// outer query, as (after `SELECT`, after the `WHERE`). Without it the
/// database may evaluate the caller's conditions on rows the row filter
/// excludes (they are reordered freely), and an error raised there
/// ("invalid input syntax: <value>", "Conversion failed when converting the
/// varchar value '<value>'") reports those rows.
///
/// * PostgreSQL, DuckDB: `OFFSET 0` (a subquery with OFFSET is never pulled up).
/// * SQLite: `LIMIT -1 OFFSET 0` (the same rule: no flattening with LIMIT).
/// * Oracle: `AND ROWNUM > 0`. A view whose query uses ROWNUM can be neither
///   merged nor have predicates pushed into it, because either would change
///   which rows get which ROWNUM. `NO_MERGE` is not enough: it stops view
///   merging only, and Oracle still pushes the outer WHERE into the view's
///   own WHERE ("filter push-down"), where it may run before the row filter.
///   A hint is also only a hint, silently ignored when misspelt.
/// * SQL Server: `TOP (9223372036854775807)`. A filter above a TOP cannot be
///   moved below it without changing which rows TOP returns, so it is not.
fn barrier(dialect: SqlDialect) -> (&'static str, &'static str) {
    match dialect {
        SqlDialect::Postgres | SqlDialect::DuckDb | SqlDialect::Generic => ("", " OFFSET 0"),
        SqlDialect::Sqlite => ("", " LIMIT -1 OFFSET 0"),
        SqlDialect::Oracle => ("", " AND ROWNUM > 0"),
        SqlDialect::MsSql => ("TOP (9223372036854775807) ", ""),
    }
}

/// Replaces each `FROM table [alias]` with
/// `FROM (SELECT <visible columns> FROM table WHERE <filters> <barrier>) alias`.
///
/// The derived table holds only the columns the policy does not hide, so
/// nothing the caller writes (`SELECT *`, a UNION, column alias lists,
/// `USING`/`NATURAL` joins, whole-row values like `to_jsonb(t)`) can reach a
/// hidden one: it is not there.
struct ApplyFilters<'a> {
    access: &'a EffectiveAccess,
    dialect: SqlDialect,
    catalog: &'a Catalog,
    hidden: Vec<String>,
    error: Option<Denied>,
}

/// Name a function the way it must run: Oracle Spatial in MDSYS (Oracle
/// would find a function or synonym of the same name in the login's schema
/// first), PostGIS functions in their extension's schema and PostgreSQL's
/// built-ins in pg_catalog, so a function of the same name earlier on the
/// search path is never the one that runs.
fn pin_function(e: &mut Expr, dialect: SqlDialect, catalog: &Catalog) {
    // Oracle Spatial is always called in MDSYS: Oracle would find a
    // function or synonym of the same name in the login's schema first.
    if dialect == SqlDialect::Oracle {
        if let Expr::Function(f) = e {
            if let Some(parts) = function_parts(&f.name).as_deref().and_then(oracle_spatial) {
                f.name = ObjectName::from(parts.into_iter().map(Ident::new).collect::<Vec<_>>());
            }
        }
        return;
    }
    if !matches!(dialect, SqlDialect::Postgres | SqlDialect::DuckDb) {
        return;
    }
    if let Expr::Function(f) = e {
        let upper = function_name(&f.name);
        let bare = upper.rsplit('.').next().unwrap_or(&upper).to_string();
        if let Some(schema) = catalog.extension_functions.get(&bare) {
            f.name = ObjectName::from(vec![
                Ident::with_quote('"', schema.clone()),
                Ident::new(bare.to_ascii_lowercase()),
            ]);
        } else if dialect == SqlDialect::Postgres
            && !matches!(f.args, sqlparser::ast::FunctionArguments::None)
            && !PG_SYNTAX_FUNCTIONS.contains(&bare.as_str())
            && (upper == bare || upper == format!("PG_CATALOG.{bare}"))
        {
            // Every other allowed function is the built-in one. PostgreSQL
            // weighs a function in `public` that takes the argument's
            // exact type (public.upper(varchar)) above pg_catalog's
            // (upper(text)) whatever the search path, so name it in full;
            // a name pg_catalog lacks then fails instead of running
            // someone's function.
            f.name = ObjectName::from(vec![
                Ident::new("pg_catalog"),
                Ident::new(bare.to_ascii_lowercase()),
            ]);
        }
    }
}

impl VisitorMut for ApplyFilters<'_> {
    type Break = ();

    // Call PostGIS functions by their extension's schema, so a function of
    // the same name earlier on the search path is never the one that runs.
    fn post_visit_expr(&mut self, e: &mut Expr) -> ControlFlow<()> {
        pin_function(e, self.dialect, self.catalog);
        ControlFlow::Continue(())
    }

    // Post-visit, so the derived table we insert is not visited again.
    fn post_visit_table_factor(&mut self, tf: &mut TableFactor) -> ControlFlow<()> {
        let TableFactor::Table { name, alias, .. } = tf else {
            return ControlFlow::Continue(());
        };
        // The relation the database will read: resolved against the
        // catalog and named in full, so search_path or case cannot swap it.
        let rel = if self.catalog.resolve {
            match resolve(name, &self.catalog.relations, self.dialect) {
                Ok(r) => Some(r),
                Err(e) => {
                    self.error = Some(Denied::Query(e));
                    return ControlFlow::Break(());
                }
            }
        } else {
            None
        };
        let key = match rel {
            Some(r) => r.parts.join("."),
            None => normalize_identifier(&name.to_string()),
        };
        let filters = self.access.filters_for(&key);
        let mut select = "*".to_string();
        let source = match rel {
            Some(r) => r
                .parts
                .iter()
                .map(|p| quote_ident(p, self.dialect))
                .collect::<Vec<_>>()
                .join("."),
            None => name.to_string(),
        };
        if !self.access.hide_fields.is_empty() {
            // Only the visible columns, always listed by name: a column added
            // since the list was taken is left out rather than let in by `*`.
            let Some(rel) = rel else {
                self.error = Some(Denied::Query(format!(
                    "cannot check the fields of {key}; name it as list_datasets shows it"
                )));
                return ControlFlow::Break(());
            };
            let visible: Vec<&String> = rel
                .columns
                .iter()
                .filter(|c| !self.access.field_hidden(c))
                .collect();
            for c in rel.columns.iter().filter(|c| self.access.field_hidden(c)) {
                if !self.hidden.contains(c) {
                    self.hidden.push(c.clone());
                }
            }
            if visible.is_empty() {
                self.error = Some(Denied::Query(format!(
                    "every field of {key} is hidden by organization policy"
                )));
                return ControlFlow::Break(());
            }
            select = visible
                .iter()
                .map(|c| quote_ident(c, self.dialect))
                .collect::<Vec<_>>()
                .join(", ");
        }
        if filters.is_empty() && select == "*" {
            if let Some(r) = rel {
                // Nothing to filter: just read it by its exact name.
                *name = ObjectName::from(
                    r.parts
                        .iter()
                        .map(|p| Ident::with_quote(quote_char(self.dialect), p.clone()))
                        .collect::<Vec<_>>(),
                );
            }
            return ControlFlow::Continue(());
        }
        let mut checked = Vec::new();
        for f in &filters {
            // Every column a filter names is the filtered table's own: one it
            // lacks is an error, never a column of the query around it.
            // A listed table whose columns aren't known can't have its
            // filter checked: refused rather than run on trust.
            if rel.is_some_and(|r| r.columns.is_empty()) {
                self.error = Some(Denied::Query(format!(
                    "the columns of {key} are not known, so its row filter cannot be checked"
                )));
                return ControlFlow::Break(());
            }
            let columns = rel.map(|r| r.columns.as_slice());
            let text_dates: Vec<String> = rel
                .map(|r| {
                    let rk = r.parts.join(".").to_lowercase();
                    self.catalog
                        .text_dates
                        .iter()
                        .filter(|(t, _)| *t == rk)
                        .map(|(_, c)| c.clone())
                        .collect()
                })
                .unwrap_or_default();
            match check_where(f, self.dialect).and_then(|c| {
                bind_filter_dates(
                    &c,
                    self.dialect,
                    FILTER_ALIAS,
                    columns,
                    self.catalog,
                    &text_dates,
                )
            }) {
                Ok(c) => checked.push(format!("({c})")),
                Err(e) => {
                    self.error = Some(Denied::Query(format!(
                        "configured row filter {f:?} is invalid: {e}"
                    )));
                    return ControlFlow::Break(());
                }
            }
        }
        // The barrier is only needed when rows are filtered out: with hidden
        // fields alone there is no excluded row to protect, and leaving it off
        // lets the database merge the table and use its spatial index.
        let (before, after) = if checked.is_empty() {
            ("", "")
        } else {
            barrier(self.dialect)
        };
        let mut inner_sql = format!("SELECT {before}{select} FROM {source} {FILTER_ALIAS}");
        if !checked.is_empty() {
            inner_sql.push_str(&format!(" WHERE {}", checked.join(" AND ")));
        }
        inner_sql.push_str(after);
        let d = self.dialect.parser_dialect();
        let inner = match Parser::parse_sql(d.as_ref(), &inner_sql) {
            Ok(mut s) if s.len() == 1 => match s.remove(0) {
                Statement::Query(q) => q,
                _ => unreachable!("built a SELECT"),
            },
            _ => {
                self.error = Some(Denied::Query("could not apply row filter".into()));
                return ControlFlow::Break(());
            }
        };
        // The alias is the table's last name part exactly as written,
        // quotes included, so `t.col` in the query still means this table.
        // Unquoted, a name such as `Parcels CROSS JOIN Owners` would turn
        // into SQL of its own.
        let last = name
            .0
            .last()
            .and_then(|p| p.as_ident())
            .cloned()
            .unwrap_or_else(|| Ident::new("t"));
        let alias = alias.clone().unwrap_or(TableAlias {
            explicit: self.dialect != SqlDialect::Oracle,
            name: last,
            columns: vec![],
            at: None,
        });
        *tf = TableFactor::Derived {
            lateral: false,
            subquery: inner,
            alias: Some(alias),
            sample: None,
        };
        ControlFlow::Continue(())
    }
}

/// Check free SQL against the effective access and rewrite it.
///
/// `catalog` is what the server listed of the database: the tables (with
/// their columns) and the PostGIS functions. Pass [`NO_CATALOG`] only where
/// the connection cannot be listed.
pub fn check_select(
    sql: &str,
    access: &EffectiveAccess,
    max_rows: u64,
    catalog: &Catalog,
) -> Result<CheckedQuery, Denied> {
    check_select_limited(sql, access, max_rows, true, catalog)
}

/// As [`check_select`], but without the SQL row limit, for queries whose
/// rows the worker filters further (a bounding box) before counting them
/// against `max_rows`. The worker still stops at `max_rows`; every other
/// check and every row filter applies exactly as in `check_select`.
pub fn check_select_unlimited(
    sql: &str,
    access: &EffectiveAccess,
    max_rows: u64,
    catalog: &Catalog,
) -> Result<CheckedQuery, Denied> {
    check_select_limited(sql, access, max_rows, false, catalog)
}

/// The checked query must read back as itself. The SQL that runs is the
/// parsed query written out again, and in a few places the parser writes
/// back less than it read (a type modifier's quotes, a `]` inside a
/// `[bracketed]` name), which could turn a value into SQL the checks never
/// saw. Parsing the text that will run and writing it out once more must
/// give the same text; anything else is refused.
fn reads_back(sql: &str, dialect: SqlDialect) -> Result<(), Denied> {
    let d = dialect.parser_dialect();
    let same = match Parser::parse_sql(d.as_ref(), sql) {
        Ok(s) => s.len() == 1 && s[0].to_string() == sql,
        Err(_) => false,
    };
    if same {
        Ok(())
    } else {
        Err(Denied::Query(
            "the query could not be checked reliably (a name or literal does not read back as written); write names without brackets or quotes inside them, and type modifiers as plain numbers".into(),
        ))
    }
}

/// The query with its row limit (`fetch`, or none for an unlimited read),
/// in the dialect's syntax, keeping its own ORDER BY in force.
///
/// SQL Server allows ORDER BY in a derived table only with TOP or OFFSET,
/// and needs a name for every column of one, so the limit goes onto the
/// query itself where it can: `TOP` on a plain SELECT, `OFFSET 0 ROWS FETCH
/// FIRST n ROWS ONLY` on an ordered UNION. An unlimited ordered query gets
/// TOP (max) too, so the engine can still wrap it to convert geometry.
/// Oracle takes `FETCH FIRST n ROWS ONLY` on any query without one.
fn with_row_limit(mut query: Query, dialect: SqlDialect, fetch: Option<u64>) -> String {
    let no_limit = query.limit_clause.is_none() && query.fetch.is_none();
    match dialect {
        SqlDialect::Oracle => match fetch {
            None => query.to_string(),
            Some(n) if no_limit => format!("{query} FETCH FIRST {n} ROWS ONLY"),
            Some(n) => format!("SELECT * FROM ({query}) columbus_q FETCH FIRST {n} ROWS ONLY"),
        },
        SqlDialect::MsSql => {
            if no_limit && (fetch.is_some() || query.order_by.is_some()) {
                let n = fetch.unwrap_or(i64::MAX as u64);
                let select = matches!(query.body.as_ref(), SetExpr::Select(_));
                if let SetExpr::Select(s) = query.body.as_mut() {
                    // A caller's own TOP cannot be combined with OFFSET: wrap.
                    if s.top.is_none() {
                        s.top = Some(sqlparser::ast::Top {
                            with_ties: false,
                            percent: false,
                            quantity: Some(sqlparser::ast::TopQuantity::Expr(Expr::value(
                                sqlparser::ast::Value::Number(n.to_string(), false),
                            ))),
                        });
                        return query.to_string();
                    }
                }
                if !select && query.order_by.is_some() {
                    return format!("{query} OFFSET 0 ROWS FETCH FIRST {n} ROWS ONLY");
                }
            }
            match fetch {
                None => query.to_string(),
                Some(n) => format!("SELECT TOP ({n}) * FROM ({query}) AS columbus_q"),
            }
        }
        _ => match fetch {
            None => query.to_string(),
            Some(n) => format!("SELECT * FROM ({query}) AS columbus_q LIMIT {n}"),
        },
    }
}

fn check_select_limited(
    sql: &str,
    access: &EffectiveAccess,
    max_rows: u64,
    limit: bool,
    catalog: &Catalog,
) -> Result<CheckedQuery, Denied> {
    let dialect = SqlDialect::for_connection_type(&access.kind);
    refuse_reserved_names(sql, dialect)?;
    let mut query = single_select(sql, dialect)?;
    let tables = check_tables_and_functions(&query, access, dialect, catalog)?;
    check_hidden_references(&query, access)?;

    let mut apply = ApplyFilters {
        access,
        dialect,
        catalog,
        hidden: Vec::new(),
        error: None,
    };
    let _ = query.visit(&mut apply);
    if let Some(e) = apply.error {
        return Err(e);
    }

    reads_back(&query.to_string(), dialect)?;
    // Ask the database for one row more than the worker may read, so the
    // worker can tell "exactly max_rows" from "cut off at max_rows" and
    // report the truncation instead of returning an incomplete result as if
    // it were whole.
    let fetch = limit.then(|| max_rows.saturating_add(1));
    let sql = with_row_limit(query, dialect, fetch);
    reads_back(&sql, dialect)?;
    Ok(CheckedQuery {
        sql,
        tables,
        max_rows,
        dialect,
        hidden: apply.hidden,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::policy::{Connection, EffectiveAccess, OrgPolicy, ProjectScope};
    use chrono::Utc;

    fn access(kind: &str, filter: Option<&str>) -> EffectiveAccess {
        let mut conn: Connection = serde_yaml::from_str(
            r#"
id: roads
type: postgis
datasets:
  allow: ["ROADS.ROADWAY_*", "ROADS.CONTROL_SECTION", "main.*", "ROADS.COUNTY", "COUNTY",
          "ROADWAY_INVENTORY", "PUBLIC.ROADWAY_INVENTORY", "GIS.ROADS.ROADWAY_INVENTORY", "NOTHING"]
  deny: ["ROADS.*_AUDIT"]
"#,
        )
        .unwrap();
        conn.kind = kind.into();
        conn.row_filter = filter.map(String::from);
        let org = OrgPolicy {
            connection_types: vec![kind.into()],
            ..OrgPolicy::default()
        };
        EffectiveAccess::resolve(&org, &conn, &ProjectScope::default(), Utc::now()).unwrap()
    }

    fn hiding(kind: &str) -> EffectiveAccess {
        let mut a = access(kind, Some("DISTRICT_NBR = 14"));
        a.hide_fields = vec!["*SSN*".into(), "OWNER_NAME".into()];
        a
    }

    fn cat() -> Catalog {
        let mut extension_functions = std::collections::BTreeMap::new();
        for f in [
            "ST_ASGEOJSON",
            "ST_TRANSFORM",
            "ST_LENGTH",
            "ST_AREA",
            "GEOMETRY",
        ] {
            extension_functions.insert(f.to_string(), "public".to_string());
        }
        let mut relations = rels();
        relations.push(Relation {
            parts: vec!["roads".into(), "control_section".into()],
            columns: ["csj", "district_nbr"].map(String::from).to_vec(),
        });
        Catalog {
            resolve: true,
            relations,
            extension_functions,
            text_dates: Default::default(),
        }
    }

    fn rels() -> Vec<Relation> {
        vec![
            Relation {
                parts: vec!["roads".into(), "roadway_inventory".into()],
                columns: [
                    "fid",
                    "route_id",
                    "district_nbr",
                    "owner_name",
                    "emp_ssn",
                    "geom",
                ]
                .map(String::from)
                .to_vec(),
            },
            Relation {
                parts: vec!["roads".into(), "County".into()],
                columns: ["id", "owner_name", "district_nbr"]
                    .map(String::from)
                    .to_vec(),
            },
            Relation {
                parts: vec!["roads".into(), "county".into()],
                columns: ["id", "county", "district_nbr"].map(String::from).to_vec(),
            },
            Relation {
                parts: vec!["public".into(), "county".into()],
                columns: ["id", "county"].map(String::from).to_vec(),
            },
        ]
    }

    /// Table names that need quoting (GeoPackage layers such as "Tribal
    /// Census Tract") keep their quotes when a row filter or hidden fields
    /// wrap them in a derived table, so a name that reads as SQL cannot
    /// become SQL in the alias.
    #[test]
    fn quoted_table_names_stay_quoted_when_filtered() {
        let one_part = |name: &str| Relation {
            parts: vec![name.into()],
            columns: ["pid", "name", "owner_ssn"].map(String::from).to_vec(),
        };
        let catalog = Catalog {
            resolve: true,
            relations: vec![
                one_part("Tribal Census Tract"),
                one_part("Parcels CROSS JOIN Owners"),
                one_part("Owners"),
            ],
            extension_functions: Default::default(),
            text_dates: Default::default(),
        };
        for (kind, q) in [
            ("gpkg", ('"', '"')),
            ("postgis", ('"', '"')),
            ("oracle", ('"', '"')),
            ("sqlserver", ('[', ']')),
        ] {
            let mut conn: Connection = serde_yaml::from_str(
                "id: t\ntype: gpkg\ndatasets:\n  allow: [\"Tribal Census Tract\", \"Parcels CROSS JOIN Owners\"]\n",
            )
            .unwrap();
            conn.kind = kind.into();
            conn.row_filter = Some("pid > 0".into());
            let org = OrgPolicy {
                connection_types: vec![kind.into()],
                ..OrgPolicy::default()
            };
            let mut a = EffectiveAccess::resolve(&org, &conn, &ProjectScope::default(), Utc::now())
                .unwrap();
            let name = |n: &str| format!("{}{n}{}", q.0, q.1);
            for hide in [false, true] {
                a.hide_fields = if hide { vec!["*SSN*".into()] } else { vec![] };
                let sql = format!("SELECT name FROM {}", name("Tribal Census Tract"));
                let c = check_select(&sql, &a, 10, &catalog)
                    .unwrap_or_else(|e| panic!("{kind} hide={hide}: {e:?}"));
                assert!(c.sql.contains("pid > 0"), "{kind}: {}", c.sql);
                assert!(
                    c.sql.contains(&format!(
                        "{} columbus_filtered WHERE",
                        name("Tribal Census Tract")
                    )) || c.sql.ends_with(&name("Tribal Census Tract")),
                    "{kind}: {}",
                    c.sql
                );

                // The other table must not be read, filtered or not.
                let sql = format!(
                    "SELECT name FROM {} WHERE Owners.pid = 1",
                    name("Parcels CROSS JOIN Owners")
                );
                match check_select(&sql, &a, 10, &catalog) {
                    Err(_) => {}
                    Ok(c) => {
                        let reparsed = Parser::parse_sql(
                            SqlDialect::for_connection_type(kind)
                                .parser_dialect()
                                .as_ref(),
                            &c.sql,
                        )
                        .unwrap();
                        let mut tables = Vec::new();
                        let _ = visit_relations(&reparsed, |r| {
                            tables.push(r.to_string());
                            ControlFlow::<()>::Continue(())
                        });
                        assert!(
                            tables.iter().all(|t| t.contains("CROSS JOIN")),
                            "{kind} hide={hide}: reads {tables:?} in {}",
                            c.sql
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn naming_a_hidden_field_is_refused_early() {
        let a = hiding("postgis");
        for sql in [
            "SELECT owner_name FROM roads.roadway_inventory",
            "SELECT route_id, owner_name AS x FROM roads.roadway_inventory",
            "SELECT route_id FROM roads.roadway_inventory WHERE emp_ssn LIKE '1%'",
            "SELECT route_id FROM roads.roadway_inventory r WHERE CAST(r.\"OWNER_NAME\" AS INTEGER) = 1",
            "SELECT route_id FROM roads.roadway_inventory ORDER BY owner_name",
            "SELECT x FROM (SELECT owner_name AS x FROM roads.roadway_inventory) AS t",
        ] {
            let e = check_select(sql, &a, 10, &cat()).unwrap_err().to_string();
            assert!(e.contains("hidden"), "{sql}: {e}");
        }
    }

    #[test]
    fn hidden_columns_never_enter_the_query() {
        // Every form that got past a name check in review: whatever the
        // query does, the table it reads has only the visible columns.
        let a = hiding("postgis");
        for sql in [
            "SELECT * FROM roads.roadway_inventory",
            "SELECT fid, route_id, '' AS a FROM roads.roadway_inventory WHERE false UNION ALL SELECT * FROM roads.roadway_inventory",
            "SELECT x.c FROM roads.roadway_inventory AS x(a, b, c)",
            "SELECT t.d FROM (SELECT * FROM roads.roadway_inventory) AS t(a, b, d)",
            "SELECT x.* FROM roads.roadway_inventory p, LATERAL (SELECT p.*) AS x(a, b, c)",
            "SELECT p.fid FROM roads.roadway_inventory p JOIN (SELECT 'x' AS owner_name) v USING (owner_name)",
            "SELECT p.fid FROM roads.roadway_inventory p NATURAL JOIN (SELECT 'x' AS emp_ssn) v",
            "SELECT to_jsonb(r) FROM roads.roadway_inventory r",
            "SELECT row_to_json(r.*) FROM roads.roadway_inventory r",
            "SELECT * FROM roads.roadway_inventory ORDER BY 3",
        ] {
            let q = check_select(sql, &a, 10, &cat()).unwrap_or_else(|e| panic!("{sql}: {e}"));
            let inner = "(SELECT \"fid\", \"route_id\", \"district_nbr\", \"geom\" FROM \"roads\".\"roadway_inventory\" columbus_filtered WHERE (columbus_filtered.DISTRICT_NBR = 14) OFFSET 0)";
            assert!(q.sql.contains(inner), "{sql}\n=> {}", q.sql);
            assert!(!q.sql.contains("FROM roads.roadway_inventory AS") && !q.sql.contains("FROM roads.roadway_inventory p"), "every reference wrapped: {}", q.sql);
        }
        // A table whose columns are unknown cannot be checked, so it is refused.
        assert!(
            check_select("SELECT * FROM roads.roadway_inventory", &a, 10, &NO_CATALOG).is_err()
        );
        // Without hidden fields no column list is needed.
        let plain = access("postgis", None);
        let q = check_select(
            "SELECT * FROM roads.roadway_inventory",
            &plain,
            10,
            &NO_CATALOG,
        )
        .unwrap();
        assert!(!q.sql.contains("OFFSET 0"), "{}", q.sql);
        // The configured row filter may name any field: it is the server's.
        let mut b = hiding("postgis");
        b.row_filter = Some("OWNER_NAME IS NOT NULL".into());
        assert!(check_select(
            "SELECT route_id FROM roads.roadway_inventory",
            &b,
            10,
            &cat()
        )
        .is_ok());
    }

    #[test]
    fn names_resolve_to_exactly_one_listed_table() {
        let a = hiding("postgis");
        let r = cat();
        // Quoted mixed case is its own table, with its own hidden column.
        let q = check_select("SELECT * FROM roads.\"County\"", &a, 10, &r).unwrap();
        assert!(
            q.sql
                .contains("SELECT \"id\", \"district_nbr\" FROM \"roads\".\"County\""),
            "{}",
            q.sql
        );
        // Unquoted folds to lower case: the other table.
        let q = check_select("SELECT * FROM roads.COUNTY", &a, 10, &r).unwrap();
        assert!(
            q.sql
                .contains("SELECT \"id\", \"county\", \"district_nbr\" FROM \"roads\".\"county\""),
            "{}",
            q.sql
        );
        // A bare name that two schemas have is refused, not guessed.
        let e = check_select("SELECT * FROM county", &a, 10, &r)
            .unwrap_err()
            .to_string();
        assert!(e.contains("more than one table"), "{e}");
        // A bare name with one match reads that table by its full name, so
        // the database's search_path cannot pick another relation.
        let q = check_select("SELECT * FROM roadway_inventory", &a, 10, &r).unwrap();
        assert!(
            q.sql.contains("FROM \"roads\".\"roadway_inventory\""),
            "{}",
            q.sql
        );
        // Unknown names, three-part names and other schemas are refused.
        for sql in [
            "SELECT * FROM gis.roads.roadway_inventory",
            "SELECT * FROM public.roadway_inventory",
            "SELECT * FROM nothing",
        ] {
            assert!(check_select(sql, &a, 10, &r).is_err(), "{sql}");
        }
    }

    #[test]
    fn functions_that_run_sql_text_or_read_tables_are_refused() {
        let a = access("postgis", Some("DISTRICT_NBR = 14"));
        for sql in [
            "SELECT query_to_xml('SELECT * FROM hr.employees', true, false, '') FROM roads.roadway_inventory",
            "SELECT table_to_xml('roads.roadway_inventory', true, false, '') FROM roads.roadway_inventory",
            "SELECT ts_stat('SELECT to_tsvector(owner_name) FROM roads.roadway_inventory') FROM roads.roadway_inventory",
            "SELECT cursor_to_xml('c', 10, true, false, '') FROM roads.roadway_inventory",
            "SELECT ST_EstimatedExtent('roads', 'roadway_inventory', 'geom') FROM roads.roadway_inventory",
            "SELECT roads.log_visit() FROM roads.roadway_inventory",
            "SELECT _postgis_stats('roads.roadway_inventory', 'geom') FROM roads.roadway_inventory",
            "SELECT postgis_full_version() FROM roads.roadway_inventory",
        ] {
            let e = check_select(sql, &a, 10, &NO_CATALOG).unwrap_err().to_string();
            assert!(e.contains("not allowed"), "{sql}: {e}");
        }
        for sql in [
            "SELECT ROUTE_ID, ST_AsGeoJSON(ST_Transform(GEOM, 4326)) FROM roads.roadway_inventory",
            "SELECT DISTRICT_NBR, COUNT(*), SUM(ST_Length(GEOM)) FROM roads.roadway_inventory GROUP BY 1",
            "SELECT ROUTE_ID, ROW_NUMBER() OVER (PARTITION BY DISTRICT_NBR ORDER BY ROUTE_ID) FROM roads.roadway_inventory",
            "SELECT UPPER(TRIM(ROUTE_ID)), COALESCE(LPAD(CSJ, 9, '0'), '') FROM roads.roadway_inventory",
            "SELECT public.ST_Area(GEOM) FROM roads.roadway_inventory",
            "SELECT ROUTE_ID, GEOM::geography FROM roads.roadway_inventory",
            "SELECT array_to_string(string_to_array(ROUTE_ID, '-'), ' ') FROM roads.roadway_inventory",
        ] {
            let q = check_select(sql, &a, 10, &cat()).unwrap_or_else(|e| panic!("{sql}: {e}"));
            // PostGIS functions are called by the extension's schema.
            if sql.contains("ST_") {
                assert!(q.sql.contains("\"public\".st_"), "{}", q.sql);
            }
        }
        // ST_ names the PostGIS extension did not install are refused: a
        // function in public can be called anything.
        let e = check_select(
            "SELECT st_helper_summary() FROM roads.roadway_inventory",
            &a,
            10,
            &cat(),
        )
        .unwrap_err()
        .to_string();
        assert!(e.contains("not allowed"), "{e}");
        assert!(check_select(
            "SELECT ST_Area(GEOM) FROM roads.roadway_inventory",
            &a,
            10,
            &NO_CATALOG
        )
        .is_err());
        assert!(check_select(
            "SELECT roads.ST_Area(GEOM) FROM roads.roadway_inventory",
            &a,
            10,
            &cat()
        )
        .is_err());
        // Operators and casts the database could have defined.
        for sql in [
            "SELECT ROUTE_ID FROM roads.roadway_inventory WHERE ROUTE_ID ~~~ 'x'",
            "SELECT CAST(ROUTE_ID AS roads.secret_domain) FROM roads.roadway_inventory",
            "SELECT CAST(ROUTE_ID AS my_type) FROM roads.roadway_inventory",
        ] {
            assert!(check_select(sql, &a, 10, &cat()).is_err(), "{sql}");
        }
    }

    #[test]
    fn rules_apply_to_the_table_read_not_its_spelling() {
        // Allow everything, deny one table: its bare name must be denied too.
        let mut conn: Connection = serde_yaml::from_str(
            "id: x\ntype: postgis\ndatasets:\n  allow: [\"*\"]\n  deny: [\"roads.secret\"]\n",
        )
        .unwrap();
        conn.kind = "postgis".into();
        let org = OrgPolicy {
            connection_types: vec!["postgis".into()],
            ..OrgPolicy::default()
        };
        let a =
            EffectiveAccess::resolve(&org, &conn, &ProjectScope::default(), Utc::now()).unwrap();
        let c = Catalog {
            resolve: true,
            relations: vec![
                Relation {
                    parts: vec!["roads".into(), "secret".into()],
                    columns: vec!["id".into()],
                },
                Relation {
                    parts: vec!["roads".into(), "open".into()],
                    columns: vec!["id".into()],
                },
            ],
            ..Catalog::default()
        };
        for sql in [
            "SELECT * FROM secret",
            "SELECT * FROM ROADS.SECRET",
            "SELECT * FROM \"roads\".\"secret\"",
        ] {
            assert!(
                matches!(check_select(sql, &a, 10, &c), Err(Denied::Dataset(_))),
                "{sql}"
            );
        }
        let q = check_select("SELECT * FROM open", &a, 10, &c).unwrap();
        assert!(q.sql.contains("FROM \"roads\".\"open\""), "{}", q.sql);
        assert_eq!(q.tables, vec!["roads.open"]);
    }

    #[test]
    fn filtered_tables_are_not_merged_into_the_outer_query() {
        let pg = check_select(
            "SELECT * FROM ROADS.ROADWAY_INVENTORY WHERE ROUTE_ID = 'x'",
            &access("postgis", Some("DISTRICT_NBR = 14")),
            10,
            &NO_CATALOG,
        )
        .unwrap();
        assert!(
            pg.sql
                .contains("(columbus_filtered.DISTRICT_NBR = 14) OFFSET 0)"),
            "{}",
            pg.sql
        );
        let lite = check_select(
            "SELECT * FROM main.roads",
            &access("gpkg", Some("DISTRICT_NBR = 14")),
            10,
            &NO_CATALOG,
        )
        .unwrap();
        assert!(lite.sql.contains("LIMIT -1 OFFSET 0)"), "{}", lite.sql);
    }

    #[test]
    fn where_accepts_simple_predicates() {
        for ok in [
            "FUNC_CLASS IN (1, 2, 3) AND STATUS <> 'R'",
            "NOT (LANES BETWEEN 2 AND 4) OR NAME LIKE 'IH%'",
            "UPPER(CNTY_NM) = 'TRAVIS' AND RETIRED_DT IS NULL",
            "CSJ = LPAD('12345', 9, '0')",
        ] {
            assert!(check_where(ok, SqlDialect::Postgres).is_ok(), "{ok}");
        }
    }

    #[test]
    fn where_refuses_subqueries_and_tricks() {
        for bad in [
            "ROUTE_ID IN (SELECT ROUTE_ID FROM HR.EMPLOYEES)",
            "EXISTS (SELECT 1 FROM ROADS.ROADWAY_AUDIT)",
            "1=1; DROP TABLE ROADS.ROADWAY_INVENTORY",
            "pg_read_file('/etc/passwd') IS NOT NULL",
            "1=1 UNION SELECT * FROM x",
        ] {
            assert!(check_where(bad, SqlDialect::Postgres).is_err(), "{bad}");
        }
    }

    #[test]
    fn select_allowed_table_gets_filter_and_limit() {
        let a = access("postgis", Some("DISTRICT_NBR = 14"));
        let q = check_select(
            "SELECT r.ROUTE_ID, c.CSJ FROM ROADS.ROADWAY_INVENTORY r JOIN ROADS.CONTROL_SECTION c ON r.CSJ = c.CSJ",
            &a,
            500,
            &NO_CATALOG,
        )
        .unwrap();
        assert_eq!(
            q.tables,
            vec!["ROADS.ROADWAY_INVENTORY", "ROADS.CONTROL_SECTION"]
        );
        assert!(
            q.sql.contains(
                "(SELECT * FROM ROADS.ROADWAY_INVENTORY columbus_filtered WHERE (columbus_filtered.DISTRICT_NBR = 14) OFFSET 0) r "
            ),
            "{}",
            q.sql
        );
        assert!(
            q.sql.contains(
                "(SELECT * FROM ROADS.CONTROL_SECTION columbus_filtered WHERE (columbus_filtered.DISTRICT_NBR = 14) OFFSET 0) c "
            ),
            "{}",
            q.sql
        );
        assert!(
            q.sql.ends_with("LIMIT 501"),
            "one sentinel row past the read limit"
        );
    }

    #[test]
    fn select_without_alias_keeps_qualified_columns_working() {
        let a = access("postgis", Some("DISTRICT_NBR = 14"));
        let q = check_select(
            "SELECT ROADWAY_INVENTORY.ROUTE_ID FROM ROADS.ROADWAY_INVENTORY",
            &a,
            10,
            &NO_CATALOG,
        )
        .unwrap();
        assert!(q.sql.contains(") AS ROADWAY_INVENTORY"), "{}", q.sql);
    }

    #[test]
    fn subqueries_in_select_are_checked_and_filtered_too() {
        let a = access("postgis", Some("DISTRICT_NBR = 14"));
        let q = check_select(
            "SELECT * FROM ROADS.ROADWAY_INVENTORY WHERE CSJ IN (SELECT CSJ FROM ROADS.CONTROL_SECTION)",
            &a,
            10,
            &NO_CATALOG,
        )
        .unwrap();
        assert_eq!(q.sql.matches("DISTRICT_NBR = 14").count(), 2, "{}", q.sql);
        let err = check_select(
            "SELECT * FROM ROADS.ROADWAY_INVENTORY WHERE CSJ IN (SELECT CSJ FROM ROADS.ROADWAY_AUDIT)",
            &a,
            10,
            &NO_CATALOG,
        );
        assert!(matches!(err, Err(Denied::Dataset(_))));
    }

    #[test]
    fn refuses_writes_multiple_statements_and_denied_tables() {
        let a = access("postgis", None);
        for bad in [
            "DELETE FROM ROADS.ROADWAY_INVENTORY",
            "UPDATE ROADS.ROADWAY_INVENTORY SET LANES = 0",
            "SELECT 1 FROM ROADS.ROADWAY_INVENTORY; DROP TABLE ROADS.ROADWAY_INVENTORY",
            "SELECT * FROM HR.EMPLOYEES",
            "SELECT * FROM ROADS.ROADWAY_AUDIT",
            "SELECT * INTO ROADS.COPY FROM ROADS.ROADWAY_INVENTORY",
            "SELECT * FROM ROADS.ROADWAY_INVENTORY FOR UPDATE",
            "WITH x AS (SELECT * FROM HR.EMPLOYEES) SELECT * FROM x",
            "SELECT pg_read_file('/etc/passwd') FROM ROADS.ROADWAY_INVENTORY",
            "SELECT 1",
            "this is not sql",
        ] {
            assert!(check_select(bad, &a, 10, &NO_CATALOG).is_err(), "{bad}");
        }
    }

    #[test]
    fn quoting_does_not_bypass_the_allow_list() {
        let a = access("postgis", None);
        assert!(check_select(
            "SELECT * FROM \"ROADS\".\"ROADWAY_AUDIT\"",
            &a,
            10,
            &NO_CATALOG
        )
        .is_err());
        assert!(check_select("SELECT * FROM \"HR\".\"EMPLOYEES\"", &a, 10, &NO_CATALOG).is_err());
        assert!(check_select(
            "SELECT * FROM \"ROADS\".\"ROADWAY_INVENTORY\"",
            &a,
            10,
            &NO_CATALOG
        )
        .is_ok());
    }

    #[test]
    fn dialect_specific_limits() {
        let q = check_select(
            "SELECT * FROM ROADS.ROADWAY_INVENTORY",
            &access("oracle", None),
            7,
            &NO_CATALOG,
        )
        .unwrap();
        assert!(q.sql.ends_with("FETCH FIRST 8 ROWS ONLY"), "{}", q.sql);
        let q = check_select(
            "SELECT * FROM ROADS.ROADWAY_INVENTORY",
            &access("sqlserver", None),
            7,
            &NO_CATALOG,
        )
        .unwrap();
        assert!(q.sql.starts_with("SELECT TOP (8)"), "{}", q.sql);
    }

    #[test]
    fn unlimited_keeps_every_check_and_filter() {
        let a = access("postgis", Some("DISTRICT_NBR = 14"));
        let q =
            check_select_unlimited("SELECT * FROM ROADS.ROADWAY_INVENTORY", &a, 10, &NO_CATALOG)
                .unwrap();
        assert!(
            q.sql.contains("DISTRICT_NBR = 14") && !q.sql.contains("LIMIT"),
            "{}",
            q.sql
        );
        assert!(
            check_select_unlimited("SELECT * FROM ROADS.ROADWAY_AUDIT", &a, 10, &NO_CATALOG)
                .is_err()
        );
        assert!(
            check_select_unlimited("DELETE FROM ROADS.ROADWAY_INVENTORY", &a, 10, &NO_CATALOG)
                .is_err()
        );
    }

    #[test]
    fn sqlite_table_functions_refused() {
        let a = access("gpkg", None);
        assert!(check_select("SELECT * FROM main.roads", &a, 10, &NO_CATALOG).is_ok());
        assert!(check_select(
            "SELECT load_extension('evil') FROM main.roads",
            &a,
            10,
            &NO_CATALOG
        )
        .is_err());
    }

    #[test]
    fn the_barrier_is_only_added_when_rows_are_filtered() {
        // Hidden fields alone: no excluded rows, so the database may merge
        // the table and use its indexes.
        let mut a = access("postgis", None);
        a.hide_fields = vec!["OWNER_NAME".into()];
        let q = check_select(
            "SELECT route_id FROM roads.roadway_inventory WHERE ST_Length(geom) > 10",
            &a,
            10,
            &cat(),
        )
        .unwrap();
        assert!(!q.sql.contains("OFFSET 0"), "{}", q.sql);
        assert!(!q.sql.to_lowercase().contains("owner_name"), "{}", q.sql);
        // With a row filter the barrier is there.
        let q = check_select(
            "SELECT route_id FROM roads.roadway_inventory",
            &hiding("postgis"),
            10,
            &cat(),
        )
        .unwrap();
        assert!(q.sql.contains("OFFSET 0"), "{}", q.sql);
    }

    #[test]
    fn tablesample_is_refused_not_dropped() {
        let e = check_select(
            "SELECT * FROM (SELECT * FROM roads.roadway_inventory) AS x TABLESAMPLE SYSTEM (1)",
            &access("duckdb", None),
            10,
            &NO_CATALOG,
        )
        .unwrap_err();
        assert!(e.to_string().contains("TABLESAMPLE"), "{e}");
        let e = check_select(
            "SELECT * FROM roads.roadway_inventory TABLESAMPLE SYSTEM (1)",
            &hiding("postgis"),
            10,
            &cat(),
        )
        .unwrap_err();
        assert!(e.to_string().contains("TABLESAMPLE"), "{e}");
    }

    #[test]
    fn spatial_functions_are_refused_on_file_connections() {
        let e = check_select(
            "SELECT ST_Area(geom) FROM roads.roadway_inventory",
            &access("gpkg", None),
            10,
            &NO_CATALOG,
        )
        .unwrap_err();
        assert!(e.to_string().contains("query_features"), "{e}");
    }

    #[test]
    fn builtin_functions_are_named_in_full_on_postgres() {
        let q = check_select(
            "SELECT upper(route_id), count(*), coalesce(route_id, 'x'), row_number() OVER (ORDER BY route_id), ST_Length(geom) FROM roads.roadway_inventory GROUP BY route_id, geom",
            &access("postgis", None),
            10,
            &cat(),
        )
        .unwrap();
        println!("{}", q.sql);
        assert!(q.sql.contains("pg_catalog.upper("), "{}", q.sql);
        assert!(q.sql.contains("pg_catalog.count(*)"), "{}", q.sql);
        assert!(q.sql.contains("pg_catalog.row_number()"), "{}", q.sql);
        assert!(
            q.sql.to_lowercase().contains("coalesce(route_id"),
            "{}",
            q.sql
        );
        assert!(
            !q.sql.contains("pg_catalog.COALESCE") && !q.sql.contains("pg_catalog.coalesce"),
            "{}",
            q.sql
        );
        assert!(q.sql.contains("\"public\".st_length("), "{}", q.sql);
    }

    /// The catalog an Oracle connection lists: upper-case names.
    fn ora_cat() -> Catalog {
        Catalog {
            resolve: true,
            relations: vec![Relation {
                parts: vec!["ROADS".into(), "ROADWAY_INVENTORY".into()],
                columns: ["ID", "ROUTE_CODE", "DISTRICT_NBR", "OWNER_NAME", "GEOM"]
                    .map(String::from)
                    .to_vec(),
            }],
            ..Catalog::default()
        }
    }

    fn ms_cat() -> Catalog {
        Catalog {
            resolve: true,
            relations: vec![Relation {
                parts: vec!["roads".into(), "roadway_inventory".into()],
                columns: ["id", "route_code", "district_nbr", "owner_name", "geom"]
                    .map(String::from)
                    .to_vec(),
            }],
            ..Catalog::default()
        }
    }

    #[test]
    fn oracle_barrier_is_rownum_and_names_fold_to_upper_case() {
        let q = check_select(
            "SELECT id FROM roads.roadway_inventory WHERE TO_NUMBER(route_code) = 1",
            &hiding("oracle"),
            10,
            &ora_cat(),
        )
        .unwrap();
        assert!(
            q.sql.contains("(SELECT \"ID\", \"ROUTE_CODE\", \"DISTRICT_NBR\", \"GEOM\" FROM \"ROADS\".\"ROADWAY_INVENTORY\" columbus_filtered WHERE (columbus_filtered.DISTRICT_NBR = 14) AND ROWNUM > 0) roadway_inventory WHERE"),
            "{}",
            q.sql
        );
        // The caller's condition is outside the barrier, never beside the filter.
        let inner_end = q.sql.find("ROWNUM > 0)").unwrap();
        assert!(q.sql[inner_end..].contains("TO_NUMBER(route_code) = 1"));
        assert!(q.sql.ends_with("FETCH FIRST 11 ROWS ONLY"), "{}", q.sql);
        assert!(
            !q.sql.contains(" AS roadway_inventory"),
            "Oracle has no AS for tables"
        );
        // No row filter: no barrier, the table read by its exact name.
        let mut a = access("oracle", None);
        a.hide_fields = vec![];
        let q = check_select("SELECT * FROM roads.roadway_inventory", &a, 10, &ora_cat()).unwrap();
        assert!(!q.sql.contains("ROWNUM"), "{}", q.sql);
        assert!(
            q.sql.contains("FROM \"ROADS\".\"ROADWAY_INVENTORY\""),
            "{}",
            q.sql
        );
        // Quoted lower case is another table, which the connection does not list.
        assert!(check_select(
            "SELECT * FROM \"roads\".\"roadway_inventory\"",
            &a,
            10,
            &ora_cat()
        )
        .is_err());
    }

    #[test]
    fn sql_server_barrier_is_top_and_limits_keep_order_by_legal() {
        let q = check_select(
            "SELECT id FROM roads.roadway_inventory WHERE CAST(route_code AS INT) = 1 ORDER BY id",
            &hiding("sqlserver"),
            10,
            &ms_cat(),
        )
        .unwrap();
        assert!(
            q.sql.contains("(SELECT TOP (9223372036854775807) [id], [route_code], [district_nbr], [geom] FROM [roads].[roadway_inventory] columbus_filtered WHERE (columbus_filtered.DISTRICT_NBR = 14)) AS roadway_inventory WHERE CAST(route_code AS INT) = 1"),
            "{}",
            q.sql
        );
        // TOP goes on the query itself, so its ORDER BY stays legal and in force.
        assert!(q.sql.starts_with("SELECT TOP (11) id FROM"), "{}", q.sql);
        assert!(q.sql.ends_with("ORDER BY id"), "{}", q.sql);
        // An ordered UNION gets OFFSET/FETCH; an unordered one is wrapped.
        let a = access("sqlserver", None);
        let q = check_select(
            "SELECT id FROM roads.roadway_inventory UNION SELECT id FROM roads.roadway_inventory ORDER BY id",
            &a,
            10,
            &ms_cat(),
        )
        .unwrap();
        assert!(
            q.sql
                .ends_with("ORDER BY id OFFSET 0 ROWS FETCH FIRST 11 ROWS ONLY"),
            "{}",
            q.sql
        );
        let q = check_select(
            "SELECT id FROM roads.roadway_inventory UNION SELECT id FROM roads.roadway_inventory",
            &a,
            10,
            &ms_cat(),
        )
        .unwrap();
        assert!(q.sql.starts_with("SELECT TOP (11) * FROM ("), "{}", q.sql);
        // A caller's own TOP: wrapped, TOP with ORDER BY is legal in a derived table.
        let q = check_select(
            "SELECT TOP 5 id FROM roads.roadway_inventory ORDER BY id",
            &a,
            10,
            &ms_cat(),
        )
        .unwrap();
        assert!(
            q.sql.starts_with("SELECT TOP (11) * FROM (SELECT TOP 5"),
            "{}",
            q.sql
        );
        // Unlimited (bbox) reads that are ordered still get a TOP, so the
        // engine can wrap them to convert geometry.
        let q = check_select_unlimited(
            "SELECT id FROM roads.roadway_inventory ORDER BY id",
            &a,
            10,
            &ms_cat(),
        )
        .unwrap();
        assert!(
            q.sql.starts_with("SELECT TOP (9223372036854775807) id"),
            "{}",
            q.sql
        );
        let q = check_select_unlimited("SELECT id FROM roads.roadway_inventory", &a, 10, &ms_cat())
            .unwrap();
        assert!(!q.sql.contains("TOP"), "{}", q.sql);
    }

    #[test]
    fn oracle_allows_builtins_and_oracle_spatial_in_mdsys_only() {
        let a = access("oracle", Some("DISTRICT_NBR = 14"));
        let c = ora_cat();
        let q = check_select(
            "SELECT id, SDO_GEOM.SDO_LENGTH(geom, 0.005) AS len, SDO_UTIL.TO_WKTGEOMETRY(geom) AS wkt, DECODE(district_nbr, 14, 'x', 'y') AS d FROM roads.roadway_inventory r WHERE SDO_RELATE(r.geom, SDO_GEOMETRY(2001, 4326, SDO_POINT_TYPE(1, 2, NULL), NULL, NULL), 'mask=ANYINTERACT') = 'TRUE'",
            &a,
            10,
            &c,
        )
        .unwrap();
        for want in [
            "MDSYS.SDO_GEOM.SDO_LENGTH(",
            "MDSYS.SDO_UTIL.TO_WKTGEOMETRY(",
            "MDSYS.SDO_RELATE(",
            "MDSYS.SDO_GEOMETRY(2001",
            "MDSYS.SDO_POINT_TYPE(",
        ] {
            assert!(q.sql.contains(want), "{want}: {}", q.sql);
        }
        assert!(check_select(
            "SELECT MDSYS.SDO_GEOM.SDO_AREA(geom, 0.005) FROM roads.roadway_inventory",
            &a,
            10,
            &c
        )
        .is_ok());
        for (sql, expect) in [
            // PostGIS names mean nothing here
            (
                "SELECT ST_Length(geom) FROM roads.roadway_inventory",
                "Oracle Spatial",
            ),
            // a name Oracle lacks resolves to whatever someone created
            (
                "SELECT SPLIT_PART(route_code, '-', 1) FROM roads.roadway_inventory",
                "not allowed",
            ),
            (
                "SELECT MD5(route_code) FROM roads.roadway_inventory",
                "not allowed",
            ),
            // the database's own code, or Spatial outside MDSYS
            (
                "SELECT roads.log_visit() FROM roads.roadway_inventory",
                "not allowed",
            ),
            (
                "SELECT ROADS.SDO_RELATE(geom, geom, 'x') FROM roads.roadway_inventory",
                "not allowed",
            ),
            (
                "SELECT SDO_UTIL.SOMETHING_ELSE(geom) FROM roads.roadway_inventory",
                "not allowed",
            ),
            (
                "SELECT r.geom.GET_WKT() FROM roads.roadway_inventory r",
                "not allowed",
            ),
            (
                "SELECT DBMS_LOB.GETLENGTH(route_code) FROM roads.roadway_inventory",
                "not allowed",
            ),
            (
                "SELECT SYS_CONTEXT('USERENV', 'SESSION_USER') FROM roads.roadway_inventory",
                "not allowed",
            ),
            (
                "SELECT CAST(route_code AS roads.my_type) FROM roads.roadway_inventory",
                "cast",
            ),
            ("SELECT * FROM all_tables", "allowed"),
            ("SELECT * FROM dual", "allowed"),
        ] {
            let e = check_select(sql, &a, 10, &c).unwrap_err().to_string();
            assert!(e.contains(expect), "{sql}: {e}");
        }
        assert!(check_select(
            "SELECT CAST(route_code AS NUMBER(10)) FROM roads.roadway_inventory",
            &a,
            10,
            &c
        )
        .is_ok());
    }

    #[test]
    fn sql_server_refuses_methods_static_calls_and_schema_functions() {
        let a = access("sqlserver", Some("DISTRICT_NBR = 14"));
        let c = ms_cat();
        for (sql, expect) in [
            (
                "SELECT geom.STLength() FROM roads.roadway_inventory",
                "query_features",
            ),
            (
                "SELECT id FROM roads.roadway_inventory r WHERE r.geom.STIntersects(r.geom) = 1",
                "query_features",
            ),
            (
                "SELECT roads.owner_list() FROM roads.roadway_inventory",
                "query_features",
            ),
            (
                "SELECT dbo.anything(id) FROM roads.roadway_inventory",
                "not allowed",
            ),
            (
                "SELECT geometry::Point(1, 2, 0) FROM roads.roadway_inventory",
                "static geometry methods",
            ),
            (
                "SELECT ST_Length(geom) FROM roads.roadway_inventory",
                "not allowed",
            ),
            (
                "SELECT pg_catalog.upper(route_code) FROM roads.roadway_inventory",
                "not allowed",
            ),
            (
                "SELECT OPENROWSET('x', 'y') FROM roads.roadway_inventory",
                "not allowed",
            ),
            ("SELECT * FROM sys.objects", "allowed"),
            ("SELECT * FROM INFORMATION_SCHEMA.TABLES", "allowed"),
            ("SELECT * FROM otherdb.roads.roadway_inventory", "allowed"),
            ("SELECT * INTO #t FROM roads.roadway_inventory", "INTO"),
            (
                "SELECT id FROM roads.roadway_inventory FOR JSON AUTO",
                "not allowed",
            ),
        ] {
            let e = check_select(sql, &a, 10, &c).unwrap_err().to_string();
            assert!(e.contains(expect), "{sql}: {e}");
        }
        assert!(check_select(
            "SELECT UPPER(route_code), COUNT(*) AS n, IIF(id > 1, 'a', 'b') AS x FROM roads.roadway_inventory GROUP BY route_code, id",
            &a,
            10,
            &c
        )
        .is_ok());
        // NEXT VALUE FOR advances a sequence, which a rollback does not undo.
        assert!(check_select(
            "SELECT NEXT VALUE FOR roads.seq FROM roads.roadway_inventory",
            &a,
            10,
            &c
        )
        .is_err());
    }

    #[test]
    fn what_runs_is_what_was_checked() {
        // The parser writes a type modifier back without its quotes and a
        // `]` inside a bracketed name unescaped: either would turn a value
        // into SQL that reads a table no check saw. Such queries are refused.
        let pg = access("postgis", None);
        for sql in [
            "SELECT CAST(geom AS geometry('1) FROM hr.employees --')) FROM roads.roadway_inventory",
            "SELECT fid FROM roads.roadway_inventory WHERE CAST(geom AS geometry('a b')) IS NULL",
        ] {
            assert!(check_select(sql, &pg, 10, &cat()).is_err(), "{sql}");
        }
        assert!(check_select(
            "SELECT CAST(geom AS geometry(Point, 3081)) FROM roads.roadway_inventory",
            &pg,
            10,
            &cat()
        )
        .is_ok());
        for kind in ["sqlserver", "gpkg"] {
            let a = access(kind, None);
            let sql = "SELECT 1 AS [x]] FROM hr.employees --] FROM roads.roadway_inventory";
            let r = check_select(sql, &a, 10, &NO_CATALOG);
            assert!(r.is_err(), "{kind}: {:?}", r.map(|q| q.sql));
        }
        assert!(check_where("CAST(x AS geometry('1) OR 1=1 --'))", SqlDialect::Postgres).is_err());
    }
}

#[cfg(test)]
mod filter_binding_tests {
    use super::*;
    use crate::policy::Connection;

    fn access(kind: &str, filter: &str) -> EffectiveAccess {
        let mut conn: Connection =
            serde_yaml::from_str("id: c\ntype: gpkg\ndatasets:\n  allow: [\"*\"]\n").unwrap();
        conn.kind = kind.into();
        conn.row_filter = Some(filter.into());
        let org: crate::policy::OrgPolicy =
            serde_yaml::from_str(&format!("connection_types: [{kind}]\n")).unwrap();
        EffectiveAccess::resolve(
            &org,
            &conn,
            &crate::policy::ProjectScope::default(),
            chrono::Utc::now(),
        )
        .unwrap()
    }

    fn catalog(parts: &[&str], columns: &[&str]) -> Catalog {
        Catalog {
            resolve: true,
            relations: vec![Relation {
                parts: parts.iter().map(|p| p.to_string()).collect(),
                columns: columns.iter().map(|c| c.to_string()).collect(),
            }],
            extension_functions: Default::default(),
            text_dates: Default::default(),
        }
    }

    #[test]
    fn free_sql_cannot_take_the_filtered_tables_name() {
        // A filter on a column the table lacks, which SQLite and DuckDB
        // would otherwise bind to a table the caller named columbus_filtered.
        for kind in ["gpkg", "geoparquet"] {
            let a = access(kind, "DISTRICT_NBR = 14");
            let cat = catalog(&["main", "secrets"], &["id", "secret"]);
            for sql in [
                "SELECT (SELECT group_concat(secret) FROM main.secrets) AS s FROM (SELECT 14 AS district_nbr) columbus_filtered",
                "SELECT s.* FROM (SELECT 14 AS district_nbr) AS columbus_filtered, main.secrets s",
                "SELECT s.* FROM (SELECT 14 AS district_nbr) AS \"COLUMBUS_FILTERED\", main.secrets s",
                "WITH columbus_q AS (SELECT 1 AS x) SELECT x FROM columbus_q",
                "SELECT (SELECT group_concat(secret) FROM main.secrets) FROM (SELECT 14 AS district_nbr) AS 'columbus_filtered'",
                "SELECT (SELECT group_concat(secret) FROM main.secrets) FROM (SELECT 14 AS district_nbr) 'COLUMBUS_FILTERED'",
            ] {
                let e = check_select(sql, &a, 10, &cat);
                assert!(e.is_err(), "{kind}: {sql} -> {e:?}");
            }
            // The table's own query is refused too: the filter names a
            // column it doesn't have.
            let e = check_select("SELECT id FROM main.secrets", &a, 10, &cat)
                .unwrap_err()
                .to_string();
            assert!(e.contains("does not have"), "{kind}: {e}");
            // With the column there, it runs, bound to the table.
            let cat = catalog(&["main", "secrets"], &["id", "secret", "district_nbr"]);
            let q = check_select("SELECT id FROM main.secrets", &a, 10, &cat).unwrap();
            assert!(
                q.sql.contains("columbus_filtered.DISTRICT_NBR = 14"),
                "{}",
                q.sql
            );
        }
    }

    #[test]
    fn database_keywords_in_a_filter_stay_values() {
        let a = access("oracle", "EXPIRES > SYSDATE");
        let cat = catalog(&["ROADS", "PERMITS"], &["ID", "EXPIRES"]);
        let q = check_select("SELECT ID FROM ROADS.PERMITS", &a, 10, &cat).unwrap();
        assert!(
            q.sql.contains("(columbus_filtered.EXPIRES > SYSDATE)"),
            "{}",
            q.sql
        );
        // Elsewhere (PostgreSQL included: SYSDATE isn't a keyword there) a
        // bare name is a column, bound to the table.
        for dialect in [SqlDialect::Sqlite, SqlDialect::Postgres] {
            let r = bind_filter(
                "EXPIRES > SYSDATE",
                dialect,
                FILTER_ALIAS,
                None,
                &Catalog::default(),
            )
            .unwrap();
            assert_eq!(r, "columbus_filtered.EXPIRES > columbus_filtered.SYSDATE");
        }
        // Not reserved on Oracle: refused rather than left as written.
        for f in ["EXPIRES > SYSTIMESTAMP", "EXPIRES > CURRENT_DATE"] {
            let a = access("oracle", f);
            let cat = catalog(&["ROADS", "PERMITS"], &["ID", "EXPIRES"]);
            let e = check_select("SELECT ID FROM ROADS.PERMITS", &a, 10, &cat);
            assert!(e.is_err(), "{f}: {e:?}");
        }
        // Edits: one table, so a name's qualifier is dropped.
        assert_eq!(
            edit_filter("p.district = 14 AND status <> 'R'", SqlDialect::Postgres).unwrap(),
            "district = 14 AND status <> 'R'"
        );
        // A listed table with no known columns: its filter isn't run on trust.
        let a = access("gpkg", "DISTRICT_NBR = 14");
        let cat = catalog(&["main", "secrets"], &[]);
        assert!(check_select("SELECT id FROM main.secrets", &a, 10, &cat).is_err());
    }

    #[test]
    fn a_whole_row_is_never_a_filters_to_name() {
        for f in [
            "COALESCE(o.*, NULL) IS NULL",
            "LENGTH(o.*) > 0",
            "COALESCE(*) IS NULL",
        ] {
            assert!(check_where(f, SqlDialect::Postgres).is_err(), "{f}");
            assert!(check_where(f, SqlDialect::Sqlite).is_err(), "{f}");
        }
    }

    #[test]
    fn a_filters_functions_are_the_built_in_ones() {
        let a = access("postgis", "UPPER(status) = 'A'");
        let cat = catalog(&["public", "parcels"], &["id", "status"]);
        let q = check_select("SELECT id FROM public.parcels", &a, 10, &cat).unwrap();
        assert!(
            q.sql
                .contains("pg_catalog.upper(columbus_filtered.status) = 'A'"),
            "{}",
            q.sql
        );
    }
}
