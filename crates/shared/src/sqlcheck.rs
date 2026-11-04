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
    visit_expressions, visit_relations, Expr, Ident, ObjectName, Query, SetExpr, Statement,
    TableAlias, TableFactor, VisitMut, VisitorMut,
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
            "sqlserver" | "mssql" | "azure-sql" => SqlDialect::MsSql,
            "gpkg" | "geopackage" | "sqlite" | "spatialite" => SqlDialect::Sqlite,
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

/// May free SQL call this function (upper-case, possibly schema-qualified)?
///
/// On PostgreSQL a spatial function must be one the PostGIS extension
/// installed ([`Catalog::extension_functions`]): any function in `public`
/// can be named `st_…`, and one the database owner wrote can read anything.
/// On GeoPackage and SQLite files none are available (see below).
fn select_function_allowed(name: &str, dialect: SqlDialect, catalog: &Catalog) -> bool {
    let (schema, bare) = match name.rsplit_once('.') {
        Some((s, b)) => (Some(s), b),
        None => (None, name),
    };
    if bare.starts_with('_') || bare.starts_with("POSTGIS") {
        return false;
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
    // Ordinary functions: unqualified (pg_catalog is searched first) or
    // explicitly pg_catalog. A function of the database's own
    // (roads.log_visit) can do anything.
    schema.is_none_or(|s| s == "PG_CATALOG") && SELECT_FUNCTIONS.contains(&bare)
}

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
        None => Ok(expr.to_string()),
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
            if DENIED_FUNCTION_PREFIXES
                .iter()
                .any(|p| name.starts_with(p) || bare.starts_with(p))
                || !select_function_allowed(&name, dialect, catalog)
            {
                let spatial = bare.starts_with("ST_") || SPATIAL_OTHER.contains(&bare);
                problem = Some(Denied::Query(if spatial && dialect == SqlDialect::Sqlite {
                    format!("function {name} is not available: spatial SQL functions do not run on GeoPackage or SQLite connections; use query_features with bbox, or vector_op")
                } else {
                    format!("function {name} is not allowed (ordinary scalar, aggregate and window functions and PostGIS ST_* functions are)")
                }));
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
            Expr::Cast {
                data_type: sqlparser::ast::DataType::Custom(t, _),
                ..
            } => {
                let last =
                    t.0.last()
                        .and_then(|p| p.as_ident())
                        .map(|i| i.value.to_ascii_uppercase())
                        .unwrap_or_default();
                if t.0.len() > 1 || !CAST_TYPES.contains(&last.as_str()) {
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
}

/// No catalog: table names are taken as written. For unit tests and
/// connections the server cannot list.
pub static NO_CATALOG: Catalog = Catalog {
    resolve: false,
    relations: Vec::new(),
    extension_functions: std::collections::BTreeMap::new(),
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

/// Text that stops the database from merging the filtered table into the
/// outer query. Without it PostgreSQL may evaluate the caller's conditions on
/// rows the row filter excludes (they are reordered freely), and an error
/// raised there ("invalid input syntax: <value>") reports those rows.
fn barrier(dialect: SqlDialect) -> &'static str {
    match dialect {
        SqlDialect::Postgres | SqlDialect::DuckDb | SqlDialect::Generic => " OFFSET 0",
        SqlDialect::Sqlite => " LIMIT -1 OFFSET 0",
        // Not run yet (no engine): revisit with the Oracle and SQL Server
        // engines (NO_MERGE / TOP with ORDER BY).
        SqlDialect::Oracle | SqlDialect::MsSql => "",
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

impl VisitorMut for ApplyFilters<'_> {
    type Break = ();

    // Call PostGIS functions by their extension's schema, so a function of
    // the same name earlier on the search path is never the one that runs.
    fn post_visit_expr(&mut self, e: &mut Expr) -> ControlFlow<()> {
        if !matches!(self.dialect, SqlDialect::Postgres | SqlDialect::DuckDb) {
            return ControlFlow::Continue(());
        }
        if let Expr::Function(f) = e {
            let upper = function_name(&f.name);
            let bare = upper.rsplit('.').next().unwrap_or(&upper).to_string();
            if let Some(schema) = self.catalog.extension_functions.get(&bare) {
                f.name = ObjectName::from(vec![
                    Ident::with_quote('"', schema.clone()),
                    Ident::new(bare.to_ascii_lowercase()),
                ]);
            }
        }
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
            match check_where(f, self.dialect) {
                Ok(c) => checked.push(format!("({c})")),
                Err(e) => {
                    self.error = Some(Denied::Query(format!(
                        "configured row filter {f:?} is invalid: {e}"
                    )));
                    return ControlFlow::Break(());
                }
            }
        }
        let mut inner_sql = format!("SELECT {select} FROM {source}");
        if !checked.is_empty() {
            inner_sql.push_str(&format!(" WHERE {}", checked.join(" AND ")));
        }
        // The barrier is only needed when rows are filtered out: with hidden
        // fields alone there is no excluded row to protect, and leaving it off
        // lets the database merge the table and use its spatial index.
        if !checked.is_empty() {
            inner_sql.push_str(barrier(self.dialect));
        }
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
        let last = name
            .0
            .last()
            .map(|p| p.to_string())
            .unwrap_or_else(|| "t".into());
        let alias = alias.clone().unwrap_or(TableAlias {
            explicit: self.dialect != SqlDialect::Oracle,
            name: Ident::new(normalize_identifier(&last)),
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

fn check_select_limited(
    sql: &str,
    access: &EffectiveAccess,
    max_rows: u64,
    limit: bool,
    catalog: &Catalog,
) -> Result<CheckedQuery, Denied> {
    let dialect = SqlDialect::for_connection_type(&access.kind);
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

    let inner = query.to_string();
    if !limit {
        return Ok(CheckedQuery {
            sql: inner,
            tables,
            max_rows,
            dialect,
            hidden: apply.hidden,
        });
    }
    // Ask the database for one row more than the worker may read, so the
    // worker can tell "exactly max_rows" from "cut off at max_rows" and
    // report the truncation instead of returning an incomplete result as if
    // it were whole.
    let fetch = max_rows.saturating_add(1);
    let sql = match dialect {
        SqlDialect::Oracle => {
            format!("SELECT * FROM ({inner}) columbus_q FETCH FIRST {fetch} ROWS ONLY")
        }
        SqlDialect::MsSql => format!("SELECT TOP ({fetch}) * FROM ({inner}) AS columbus_q"),
        _ => format!("SELECT * FROM ({inner}) AS columbus_q LIMIT {fetch}"),
    };
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
                columns: ["id", "owner_name"].map(String::from).to_vec(),
            },
            Relation {
                parts: vec!["roads".into(), "county".into()],
                columns: ["id", "county"].map(String::from).to_vec(),
            },
            Relation {
                parts: vec!["public".into(), "county".into()],
                columns: ["id", "county"].map(String::from).to_vec(),
            },
        ]
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
            let inner = "(SELECT \"fid\", \"route_id\", \"district_nbr\", \"geom\" FROM \"roads\".\"roadway_inventory\" WHERE (DISTRICT_NBR = 14) OFFSET 0)";
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
            q.sql.contains("SELECT \"id\" FROM \"roads\".\"County\""),
            "{}",
            q.sql
        );
        // Unquoted folds to lower case: the other table.
        let q = check_select("SELECT * FROM roads.COUNTY", &a, 10, &r).unwrap();
        assert!(
            q.sql
                .contains("SELECT \"id\", \"county\" FROM \"roads\".\"county\""),
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
            pg.sql.contains("(DISTRICT_NBR = 14) OFFSET 0)"),
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
                "(SELECT * FROM ROADS.ROADWAY_INVENTORY WHERE (DISTRICT_NBR = 14) OFFSET 0) r "
            ),
            "{}",
            q.sql
        );
        assert!(
            q.sql.contains(
                "(SELECT * FROM ROADS.CONTROL_SECTION WHERE (DISTRICT_NBR = 14) OFFSET 0) c "
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
}
