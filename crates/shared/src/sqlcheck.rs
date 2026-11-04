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
    visit_expressions, visit_relations, Expr, FunctionArg, FunctionArgExpr, FunctionArguments,
    GroupByExpr, Ident, ObjectName, OrderByKind, Query, SetExpr, Statement, TableAlias,
    TableFactor, Value, VisitMut, VisitorMut,
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
) -> Result<Vec<String>, Denied> {
    let mut tables = Vec::new();
    let mut problem: Option<Denied> = None;
    let _ = visit_relations(query, |rel| {
        let name = normalize_identifier(&rel.to_string());
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
            {
                problem = Some(Denied::Query(format!("function {name} is not allowed")));
                return ControlFlow::Break(());
            }
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

/// Refuses any way a query could read a field hidden by organization policy
/// other than by its name in the output (which the worker strips):
///
/// * a hidden field named anywhere: in a where clause, a join condition, an
///   expression, an alias (`owner_name AS x`), ordering or grouping;
/// * `t.*` passed to a function (`row_to_json(t.*)`), and on PostgreSQL and
///   DuckDB a bare table name or alias used as a value (`to_jsonb(t)`),
///   both of which turn a whole row, hidden fields included, into one value;
/// * ordering or grouping by position (`ORDER BY 3`), which can sort by a
///   hidden column selected through `*`.
fn check_hidden_references(
    query: &Query,
    access: &EffectiveAccess,
    dialect: SqlDialect,
) -> Result<(), Denied> {
    if access.hide_fields.is_empty()
        && !matches!(dialect, SqlDialect::Postgres | SqlDialect::DuckDb)
    {
        return check_positions(query);
    }
    // Table names and aliases, for whole-row references.
    struct Names(Vec<String>);
    impl sqlparser::ast::Visitor for Names {
        type Break = ();
        fn pre_visit_table_factor(&mut self, tf: &TableFactor) -> ControlFlow<()> {
            if let TableFactor::Table { name, alias, .. } = tf {
                if let Some(last) = name.0.last() {
                    self.0
                        .push(normalize_identifier(&last.to_string()).to_ascii_lowercase());
                }
                if let Some(a) = alias {
                    self.0.push(a.name.value.to_ascii_lowercase());
                }
            }
            if let TableFactor::Derived { alias: Some(a), .. } = tf {
                self.0.push(a.name.value.to_ascii_lowercase());
            }
            ControlFlow::Continue(())
        }
    }
    let mut names = Names(Vec::new());
    let _ = sqlparser::ast::Visit::visit(query, &mut names);
    let whole_rows = matches!(dialect, SqlDialect::Postgres | SqlDialect::DuckDb);

    let mut problem: Option<String> = None;
    let _ = visit_expressions(query, |e| {
        let hidden = |id: &Ident| access.field_hidden(&id.value);
        match e {
            Expr::Identifier(id) if hidden(id) => {
                problem = Some(format!(
                    "field {} is hidden by organization policy and cannot be used in a query",
                    id.value
                ));
            }
            Expr::CompoundIdentifier(parts) if parts.last().is_some_and(hidden) => {
                problem = Some(format!(
                    "field {} is hidden by organization policy and cannot be used in a query",
                    parts.last().map(|p| p.value.as_str()).unwrap_or("")
                ));
            }
            Expr::Identifier(id)
                if whole_rows && names.0.contains(&id.value.to_ascii_lowercase()) =>
            {
                problem = Some(format!(
                    "`{}` names a table, which here would read the whole row; name the column (for example {}.column)",
                    id.value, id.value
                ));
            }
            Expr::QualifiedWildcard(..) => {
                problem =
                    Some("`t.*` inside an expression is not allowed; name the columns".into());
            }
            Expr::Function(f) => {
                if let FunctionArguments::List(list) = &f.args {
                    for a in &list.args {
                        let arg = match a {
                            FunctionArg::Named { arg, .. }
                            | FunctionArg::ExprNamed { arg, .. }
                            | FunctionArg::Unnamed(arg) => arg,
                        };
                        let count = function_name(&f.name) == "COUNT";
                        match arg {
                            FunctionArgExpr::QualifiedWildcard(_) => {
                                problem = Some(format!(
                                    "{}(t.*) would read whole rows; name the columns",
                                    function_name(&f.name)
                                ));
                            }
                            FunctionArgExpr::Wildcard if !count => {
                                problem = Some(format!(
                                    "{}(*) would read whole rows; name the columns",
                                    function_name(&f.name)
                                ));
                            }
                            _ => {}
                        }
                    }
                }
            }
            _ => {}
        }
        if problem.is_some() {
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        }
    });
    if let Some(p) = problem {
        return Err(Denied::Query(p));
    }
    check_positions(query)
}

/// Refuses `ORDER BY 2`, `GROUP BY 3` and `ORDER BY ALL`.
fn check_positions(query: &Query) -> Result<(), Denied> {
    fn positional(e: &Expr) -> bool {
        matches!(e, Expr::Value(v) if matches!(v.value, Value::Number(..)))
    }
    fn body_ok(body: &SetExpr) -> Result<(), Denied> {
        match body {
            SetExpr::Select(s) => match &s.group_by {
                GroupByExpr::All(_) => Err(Denied::Query(
                    "GROUP BY ALL is not allowed; name the columns".into(),
                )),
                GroupByExpr::Expressions(exprs, _) if exprs.iter().any(positional) => {
                    Err(Denied::Query(
                        "GROUP BY by column position is not allowed; name the columns".into(),
                    ))
                }
                _ => Ok(()),
            },
            SetExpr::Query(q) => body_ok(&q.body),
            SetExpr::SetOperation { left, right, .. } => {
                body_ok(left)?;
                body_ok(right)
            }
            _ => Ok(()),
        }
    }
    struct Positions(Option<Denied>);
    impl sqlparser::ast::Visitor for Positions {
        type Break = ();
        fn pre_visit_query(&mut self, q: &Query) -> ControlFlow<()> {
            if let Some(o) = &q.order_by {
                let bad = match &o.kind {
                    OrderByKind::All(_) => true,
                    OrderByKind::Expressions(list) => list.iter().any(|x| positional(&x.expr)),
                };
                if bad {
                    self.0 = Some(Denied::Query(
                        "ORDER BY by column position is not allowed; name the columns".into(),
                    ));
                    return ControlFlow::Break(());
                }
            }
            if let Err(e) = body_ok(&q.body) {
                self.0 = Some(e);
                return ControlFlow::Break(());
            }
            ControlFlow::Continue(())
        }
    }
    let mut v = Positions(None);
    let _ = sqlparser::ast::Visit::visit(query, &mut v);
    match v.0 {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

/// Replaces each `FROM table [alias]` with
/// `FROM (SELECT * FROM table WHERE <filters>) alias`.
struct ApplyFilters<'a> {
    access: &'a EffectiveAccess,
    dialect: SqlDialect,
    error: Option<Denied>,
}

impl VisitorMut for ApplyFilters<'_> {
    type Break = ();

    // Post-visit, so the derived table we insert is not visited again.
    fn post_visit_table_factor(&mut self, tf: &mut TableFactor) -> ControlFlow<()> {
        let TableFactor::Table { name, alias, .. } = tf else {
            return ControlFlow::Continue(());
        };
        let filters = self
            .access
            .filters_for(&normalize_identifier(&name.to_string()));
        if filters.is_empty() {
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
        let inner_sql = format!("SELECT * FROM {name} WHERE {}", checked.join(" AND "));
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
pub fn check_select(
    sql: &str,
    access: &EffectiveAccess,
    max_rows: u64,
) -> Result<CheckedQuery, Denied> {
    check_select_limited(sql, access, max_rows, true)
}

/// As [`check_select`], but without the SQL row limit, for queries whose
/// rows the worker filters further (a bounding box) before counting them
/// against `max_rows`. The worker still stops at `max_rows`; every other
/// check and every row filter applies exactly as in `check_select`.
pub fn check_select_unlimited(
    sql: &str,
    access: &EffectiveAccess,
    max_rows: u64,
) -> Result<CheckedQuery, Denied> {
    check_select_limited(sql, access, max_rows, false)
}

fn check_select_limited(
    sql: &str,
    access: &EffectiveAccess,
    max_rows: u64,
    limit: bool,
) -> Result<CheckedQuery, Denied> {
    let dialect = SqlDialect::for_connection_type(&access.kind);
    let mut query = single_select(sql, dialect)?;
    let tables = check_tables_and_functions(&query, access)?;
    check_hidden_references(&query, access, dialect)?;

    let mut apply = ApplyFilters {
        access,
        dialect,
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
  allow: ["ROADS.ROADWAY_*", "ROADS.CONTROL_SECTION", "main.*"]
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

    #[test]
    fn hidden_fields_cannot_be_used_anywhere() {
        let a = hiding("postgis");
        for sql in [
            "SELECT owner_name FROM roads.roadway_inventory",
            "SELECT route_id, owner_name AS x FROM roads.roadway_inventory",
            "SELECT route_id FROM roads.roadway_inventory WHERE emp_ssn LIKE '1%'",
            "SELECT route_id FROM roads.roadway_inventory r WHERE CAST(r.\"OWNER_NAME\" AS INTEGER) = 1",
            "SELECT route_id FROM roads.roadway_inventory ORDER BY owner_name",
            "SELECT x FROM (SELECT owner_name AS x FROM roads.roadway_inventory) AS t",
            "SELECT a.route_id FROM roads.roadway_inventory a JOIN roads.roadway_inventory b ON a.owner_name = b.owner_name",
            "SELECT to_jsonb(r) FROM roads.roadway_inventory r",
            "SELECT row_to_json(r.*) FROM roads.roadway_inventory r",
            "SELECT to_jsonb(roadway_inventory) FROM roads.roadway_inventory",
            "SELECT * FROM roads.roadway_inventory ORDER BY 3",
            "SELECT district_nbr, count(*) FROM roads.roadway_inventory GROUP BY 1",
        ] {
            assert!(check_select(sql, &a, 10).is_err(), "should refuse: {sql}");
        }
        for sql in [
            "SELECT * FROM roads.roadway_inventory",
            "SELECT r.route_id, count(*) AS n FROM roads.roadway_inventory r GROUP BY r.route_id ORDER BY n",
            "SELECT COUNT(*) AS n FROM roads.roadway_inventory",
        ] {
            assert!(check_select(sql, &a, 10).is_ok(), "should allow: {sql}");
        }
        // The configured row filter may name any field: it is the server's.
        let mut b = hiding("postgis");
        b.row_filter = Some("OWNER_NAME IS NOT NULL".into());
        assert!(check_select("SELECT route_id FROM roads.roadway_inventory", &b, 10).is_ok());
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
        )
        .unwrap();
        assert_eq!(
            q.tables,
            vec!["ROADS.ROADWAY_INVENTORY", "ROADS.CONTROL_SECTION"]
        );
        assert!(
            q.sql
                .contains("(SELECT * FROM ROADS.ROADWAY_INVENTORY WHERE (DISTRICT_NBR = 14)) r "),
            "{}",
            q.sql
        );
        assert!(
            q.sql
                .contains("(SELECT * FROM ROADS.CONTROL_SECTION WHERE (DISTRICT_NBR = 14)) c "),
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
        )
        .unwrap();
        assert_eq!(q.sql.matches("DISTRICT_NBR = 14").count(), 2, "{}", q.sql);
        let err = check_select(
            "SELECT * FROM ROADS.ROADWAY_INVENTORY WHERE CSJ IN (SELECT CSJ FROM ROADS.ROADWAY_AUDIT)",
            &a,
            10,
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
            assert!(check_select(bad, &a, 10).is_err(), "{bad}");
        }
    }

    #[test]
    fn quoting_does_not_bypass_the_allow_list() {
        let a = access("postgis", None);
        assert!(check_select("SELECT * FROM \"ROADS\".\"ROADWAY_AUDIT\"", &a, 10).is_err());
        assert!(check_select("SELECT * FROM \"HR\".\"EMPLOYEES\"", &a, 10).is_err());
        assert!(check_select("SELECT * FROM \"ROADS\".\"ROADWAY_INVENTORY\"", &a, 10).is_ok());
    }

    #[test]
    fn dialect_specific_limits() {
        let q = check_select(
            "SELECT * FROM ROADS.ROADWAY_INVENTORY",
            &access("oracle", None),
            7,
        )
        .unwrap();
        assert!(q.sql.ends_with("FETCH FIRST 8 ROWS ONLY"), "{}", q.sql);
        let q = check_select(
            "SELECT * FROM ROADS.ROADWAY_INVENTORY",
            &access("sqlserver", None),
            7,
        )
        .unwrap();
        assert!(q.sql.starts_with("SELECT TOP (8)"), "{}", q.sql);
    }

    #[test]
    fn unlimited_keeps_every_check_and_filter() {
        let a = access("postgis", Some("DISTRICT_NBR = 14"));
        let q = check_select_unlimited("SELECT * FROM ROADS.ROADWAY_INVENTORY", &a, 10).unwrap();
        assert!(
            q.sql.contains("DISTRICT_NBR = 14") && !q.sql.contains("LIMIT"),
            "{}",
            q.sql
        );
        assert!(check_select_unlimited("SELECT * FROM ROADS.ROADWAY_AUDIT", &a, 10).is_err());
        assert!(check_select_unlimited("DELETE FROM ROADS.ROADWAY_INVENTORY", &a, 10).is_err());
    }

    #[test]
    fn sqlite_table_functions_refused() {
        let a = access("gpkg", None);
        assert!(check_select("SELECT * FROM main.roads", &a, 10).is_ok());
        assert!(check_select("SELECT load_extension('evil') FROM main.roads", &a, 10).is_err());
    }
}
