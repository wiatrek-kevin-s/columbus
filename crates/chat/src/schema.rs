//! Columbus's tools as the two OpenAI formats take them.
//!
//! Columbus's own tool schemas are JSON Schema as its server writes them
//! (draft 2020-12, with `$schema`, `$defs` and `$ref`, `["string", "null"]`
//! types, `const`, `oneOf`, boolean schemas and number formats named after
//! Rust's types). The Anthropic format takes them as they are. Services
//! that follow OpenAI's formats differ in how much of that they accept, so
//! for those the schemas are made plain here: the same meaning in the part
//! of JSON Schema every one of them reads.

use serde_json::{json, Map, Value};

use crate::wire::Tool;

/// The longest description a tool keeps in the tool list. (OpenAI refuses
/// a longer one. The whole text goes into the instructions instead.)
pub(crate) const MAX_DESCRIPTION: usize = 1024;

const MORE: &str = " (The rest is in your instructions, under this tool's name.)";

/// How deep a schema may refer to others before a reference is read as
/// "anything" (a schema that refers to itself would never end).
const MAX_DEPTH: usize = 16;

/// String formats that are JSON Schema's own; others are left out.
const FORMATS: &[&str] = &[
    "date-time",
    "date",
    "time",
    "duration",
    "email",
    "uri",
    "uuid",
    "hostname",
    "ipv4",
    "ipv6",
];

/// A tool's description, short enough for the tool list; whether it was cut.
pub(crate) fn short(description: &str) -> (String, bool) {
    if description.chars().count() <= MAX_DESCRIPTION {
        return (description.to_string(), false);
    }
    let room = MAX_DESCRIPTION - MORE.chars().count();
    let head: String = description.chars().take(room).collect();
    // Back to the end of a sentence, if one is near.
    let cut = head
        .rfind(". ")
        .or(head.rfind(".\n"))
        .map(|i| i + 1)
        .filter(|i| *i > head.len() / 2)
        .unwrap_or(head.len());
    (format!("{}{MORE}", head[..cut].trim_end()), true)
}

/// What is added to the instructions for the tools whose description was
/// cut in the tool list: each one's whole description.
pub(crate) fn notes(tools: &[Tool]) -> String {
    let long: Vec<&Tool> = tools.iter().filter(|t| short(&t.description).1).collect();
    if long.is_empty() {
        return String::new();
    }
    let mut out = String::from(
        "\n\nThe whole descriptions of the tools whose description in the tool list was cut short:",
    );
    for t in long {
        out.push_str(&format!("\n\n### {}\n\n{}", t.name, t.description));
    }
    out
}

/// `schema` made plain: no `$schema`, references resolved, one type each,
/// arrays that say what they hold.
pub(crate) fn plain(schema: &Value) -> Value {
    let empty = Map::new();
    let defs = schema["$defs"]
        .as_object()
        .or(schema["definitions"].as_object())
        .unwrap_or(&empty);
    let mut out = walk(schema, defs, 0);
    // A tool's arguments are always an object.
    if out["type"] != "object" {
        out = json!({"type": "object", "properties": {}});
    }
    if !out["properties"].is_object() {
        out["properties"] = json!({});
    }
    out
}

fn walk(s: &Value, defs: &Map<String, Value>, depth: usize) -> Value {
    let Some(m) = s.as_object() else {
        // `true` (anything) and `false` alike: no constraint Columbus can
        // write plainly.
        return json!({});
    };
    let mut out = Map::new();
    // A reference: the schema it names, with what is written beside it.
    if let Some(r) = m.get("$ref").and_then(Value::as_str) {
        let name = r
            .strip_prefix("#/$defs/")
            .or(r.strip_prefix("#/definitions/"));
        if let Some(d) = name.and_then(|n| defs.get(n)).filter(|_| depth < MAX_DEPTH) {
            if let Value::Object(o) = walk(d, defs, depth + 1) {
                out = o;
            }
        }
    }
    for (k, v) in m {
        match k.as_str() {
            "$ref" | "$schema" | "$id" | "$defs" | "definitions" | "$comment" | "$anchor"
            | "type" => {}
            "properties" | "patternProperties" | "dependentSchemas" => {
                let each: Map<String, Value> = v
                    .as_object()
                    .into_iter()
                    .flatten()
                    .map(|(name, x)| (name.clone(), walk(x, defs, depth + 1)))
                    .collect();
                out.insert(k.clone(), Value::Object(each));
            }
            // `additionalProperties: true/false` is said as it is.
            "additionalProperties" | "unevaluatedProperties" if v.is_boolean() => {
                out.insert(k.clone(), v.clone());
            }
            "items"
            | "additionalProperties"
            | "unevaluatedProperties"
            | "not"
            | "contains"
            | "propertyNames"
            | "if"
            | "then"
            | "else" => {
                out.insert(k.clone(), walk(v, defs, depth + 1));
            }
            "anyOf" | "oneOf" | "allOf" | "prefixItems" => {
                let each: Vec<Value> = v
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|x| walk(x, defs, depth + 1))
                    .collect();
                // "Exactly one of" reads as "one of" to a model.
                let key = if k == "oneOf" { "anyOf" } else { k.as_str() };
                out.insert(key.to_string(), Value::Array(each));
            }
            "const" => {
                out.insert("enum".into(), json!([v]));
            }
            "format" => {
                if v.as_str().is_some_and(|f| FORMATS.contains(&f)) {
                    out.insert(k.clone(), v.clone());
                }
            }
            // "No default" needs no saying (and null is no longer a type).
            "default" if v.is_null() => {}
            _ => {
                out.insert(k.clone(), v.clone());
            }
        }
    }
    // One type: `["string", "null"]` is a string that may be left out.
    match m.get("type") {
        Some(Value::String(t)) => {
            out.insert("type".into(), json!(t));
        }
        Some(Value::Array(a)) => {
            let types: Vec<&str> = a
                .iter()
                .filter_map(Value::as_str)
                .filter(|t| *t != "null")
                .collect();
            match types.as_slice() {
                [] => {}
                [one] => {
                    out.insert("type".into(), json!(one));
                }
                several if !out.contains_key("anyOf") => {
                    let each: Vec<Value> = several.iter().map(|t| branch(t, &mut out)).collect();
                    out.insert("anyOf".into(), Value::Array(each));
                }
                several => {
                    out.insert("type".into(), json!(several[0]));
                }
            }
        }
        _ => {}
    }
    // "This or nothing" is the thing itself, which may be left out.
    if let Some(Value::Array(a)) = out.get("anyOf") {
        let real: Vec<Value> = a.iter().filter(|b| b["type"] != "null").cloned().collect();
        match real.as_slice() {
            [] => {
                out.remove("anyOf");
            }
            [one] => {
                let one = one.clone();
                out.remove("anyOf");
                for (k, v) in one.as_object().into_iter().flatten() {
                    out.entry(k.clone()).or_insert_with(|| v.clone());
                }
            }
            _ => {
                out.insert("anyOf".into(), Value::Array(real));
            }
        }
    }
    if out.get("type").is_some_and(|t| t == "array") && !out.contains_key("items") {
        out.insert("items".into(), json!({}));
    }
    Value::Object(out)
}

/// One type of several, with the parts of the schema that belong to it
/// (taken out of `whole`).
fn branch(t: &str, whole: &mut Map<String, Value>) -> Value {
    let mut b = Map::new();
    b.insert("type".into(), json!(t));
    let own: &[&str] = match t {
        "array" => &[
            "items",
            "prefixItems",
            "minItems",
            "maxItems",
            "uniqueItems",
        ],
        "object" => &[
            "properties",
            "required",
            "additionalProperties",
            "patternProperties",
        ],
        "string" => &["minLength", "maxLength", "pattern", "format"],
        "number" | "integer" => &["minimum", "maximum", "exclusiveMinimum", "exclusiveMaximum"],
        _ => &[],
    };
    for k in own {
        if let Some(v) = whole.remove(*k) {
            b.insert((*k).to_string(), v);
        }
    }
    if t == "array" && !b.contains_key("items") {
        b.insert("items".into(), json!({}));
    }
    Value::Object(b)
}

/// What is left in a plain schema that should not be: for the test that
/// goes through every tool. Empty when the schema is plain.
#[doc(hidden)]
pub fn not_plain(schema: &Value) -> Vec<String> {
    fn look(s: &Value, at: &str, out: &mut Vec<String>) {
        let Some(m) = s.as_object() else {
            out.push(format!("{at}: a schema that is not an object"));
            return;
        };
        for k in ["$schema", "$ref", "$defs", "definitions", "const", "oneOf"] {
            if m.contains_key(k) {
                out.push(format!("{at}: {k}"));
            }
        }
        match m.get("type") {
            None | Some(Value::String(_)) => {}
            Some(other) => out.push(format!("{at}: type {other}")),
        }
        if m.get("type").is_some_and(|t| t == "null") {
            out.push(format!("{at}: a null type"));
        }
        if m.get("type").is_some_and(|t| t == "array") && !m.contains_key("items") {
            out.push(format!("{at}: an array that does not say what it holds"));
        }
        if m.get("default").is_some_and(Value::is_null) {
            out.push(format!("{at}: default null"));
        }
        if let Some(f) = m.get("format") {
            if !f.as_str().is_some_and(|f| FORMATS.contains(&f)) {
                out.push(format!("{at}: format {f}"));
            }
        }
        for k in ["properties", "patternProperties"] {
            for (name, x) in m.get(k).and_then(Value::as_object).into_iter().flatten() {
                look(x, &format!("{at}/{k}/{name}"), out);
            }
        }
        for k in ["items", "additionalProperties", "not"] {
            if let Some(x) = m.get(k).filter(|x| !x.is_boolean() || k == "items") {
                look(x, &format!("{at}/{k}"), out);
            }
        }
        for k in ["anyOf", "allOf", "prefixItems"] {
            for (i, x) in m
                .get(k)
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .enumerate()
            {
                look(x, &format!("{at}/{k}/{i}"), out);
            }
        }
    }
    let mut out = Vec::new();
    if schema["type"] != "object" || !schema["properties"].is_object() {
        out.push("the arguments are not an object with properties".to_string());
    }
    look(schema, "", &mut out);
    out
}

/// A tool's name as the OpenAI formats take one: letters, digits, `_` and
/// `-`, at most 64 characters.
#[doc(hidden)]
pub fn name_ok(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_schema_is_made_plain() {
        let s = json!({
            "$schema": "https://json-schema.org/draft/2020-12/schema",
            "$defs": {
                "Kind": {"oneOf": [
                    {"const": "layer", "description": "A layer.", "type": "string"},
                    {"const": "table", "description": "A table.", "type": "string"}]},
                "Shape": {"enum": ["point", "line"], "type": "string"},
                "Field": {"type": "object", "additionalProperties": false, "required": ["name", "type"],
                    "properties": {"name": {"type": "string"}, "type": {"$ref": "#/$defs/Shape"},
                        "format": {"anyOf": [{"$ref": "#/$defs/Shape"}, {"type": "null"}], "default": null, "description": "How it is written."},
                        "default": {"default": null}}},
                "Loop": {"type": "object", "properties": {"again": {"$ref": "#/$defs/Loop"}}}
            },
            "type": "object",
            "additionalProperties": false,
            "required": ["kind"],
            "properties": {
                "kind": {"$ref": "#/$defs/Kind"},
                "name": {"type": ["string", "null"], "default": null, "description": "Its name."},
                "limit": {"type": ["integer", "null"], "format": "uint64", "minimum": 0, "default": null},
                "ratio": {"type": "number", "format": "double"},
                "when": {"type": "string", "format": "date-time"},
                "changes": {"type": "array", "items": true},
                "fields": {"type": ["array", "null"], "items": {"$ref": "#/$defs/Field"}},
                "labels": {"type": ["object", "null"], "additionalProperties": {"type": "string"}},
                "zoom_to": {"type": ["string", "array", "null"], "default": null, "description": "A handle or an extent."},
                "args": {"type": "object", "additionalProperties": true, "default": {}},
                "loop": {"$ref": "#/$defs/Loop"},
                "elsewhere": {"$ref": "https://example.org/schema.json"}
            }
        });
        let p = plain(&s);
        assert_eq!(not_plain(&p), Vec::<String>::new(), "{p:#}");
        assert_eq!(p["type"], "object");
        assert_eq!(p["additionalProperties"], false);
        assert_eq!(p["required"], json!(["kind"]));
        let props = &p["properties"];
        // A choice of constants becomes a choice of one-value lists.
        assert_eq!(
            props["kind"]["anyOf"],
            json!([
                {"enum": ["layer"], "description": "A layer.", "type": "string"},
                {"enum": ["table"], "description": "A table.", "type": "string"}])
        );
        // "A string or nothing" is a string; its null default goes.
        assert_eq!(
            props["name"],
            json!({"type": "string", "description": "Its name."})
        );
        // Rust's number formats go; JSON Schema's own stay.
        assert_eq!(props["limit"], json!({"type": "integer", "minimum": 0}));
        assert_eq!(props["ratio"], json!({"type": "number"}));
        assert_eq!(props["when"]["format"], "date-time");
        assert_eq!(props["changes"], json!({"type": "array", "items": {}}));
        // A reference is replaced by what it names, all the way down.
        let field = &props["fields"]["items"];
        assert_eq!(props["fields"]["type"], "array");
        assert_eq!(
            field["properties"]["type"],
            json!({"enum": ["point", "line"], "type": "string"})
        );
        assert_eq!(field["required"], json!(["name", "type"]));
        // Properties named like keywords are properties, not keywords.
        assert_eq!(
            field["properties"]["format"],
            json!({"enum": ["point", "line"], "type": "string", "description": "How it is written."})
        );
        assert_eq!(field["properties"]["default"], json!({}));
        assert_eq!(
            props["labels"],
            json!({"type": "object", "additionalProperties": {"type": "string"}})
        );
        // Several types: one branch each, an array saying what it holds.
        assert_eq!(
            props["zoom_to"],
            json!({"description": "A handle or an extent.", "anyOf": [{"type": "string"}, {"type": "array", "items": {}}]})
        );
        assert_eq!(
            props["args"],
            json!({"type": "object", "additionalProperties": true, "default": {}})
        );
        // A schema that refers to itself ends; one from elsewhere is "anything".
        assert!(props["loop"]["properties"]["again"].is_object());
        assert_eq!(props["elsewhere"], json!({}));
        // No arguments at all is still an object with properties.
        assert_eq!(
            plain(&json!({})),
            json!({"type": "object", "properties": {}})
        );
        assert_eq!(
            plain(&json!(true)),
            json!({"type": "object", "properties": {}})
        );
        assert_eq!(
            plain(&json!({"type": "object"})),
            json!({"type": "object", "properties": {}})
        );
    }

    #[test]
    fn what_is_not_plain_is_named() {
        let bad = json!({"type": "object", "properties": {
            "a": {"$ref": "#/$defs/A"}, "b": {"type": ["string", "null"]}, "c": {"type": "array"},
            "d": {"const": 1}, "e": {"type": "integer", "format": "uint64"}, "f": {"default": null}, "g": true}});
        let said = not_plain(&bad).join("; ");
        for part in [
            "a: $ref",
            "b: type",
            "c: an array",
            "d: const",
            "e: format",
            "f: default null",
            "g: a schema that is not an object",
        ] {
            assert!(said.contains(part), "{part}: {said}");
        }
        assert!(!not_plain(&json!({"type": "string"})).is_empty());
    }

    #[test]
    fn a_long_description_is_cut_at_a_sentence_and_kept_whole_in_the_notes() {
        let tool = |name: &str, d: &str| Tool {
            name: name.into(),
            description: d.into(),
            schema: json!({}),
        };
        let (s, cut) = short("Lists layers.");
        assert_eq!((s.as_str(), cut), ("Lists layers.", false));
        let exact = "x".repeat(MAX_DESCRIPTION);
        assert!(!short(&exact).1);
        let long = format!(
            "{}Last sentence that does not fit at all.",
            "A sentence of words. ".repeat(60)
        );
        let (s, cut) = short(&long);
        assert!(cut);
        assert!(
            s.chars().count() <= MAX_DESCRIPTION,
            "{}",
            s.chars().count()
        );
        assert!(s.ends_with(&format!("A sentence of words.{MORE}")), "{s}");
        // Characters, not bytes: accents and dashes do not break the cut.
        let accents = "é—".repeat(700);
        let (s, cut) = short(&accents);
        assert!(cut && s.chars().count() <= MAX_DESCRIPTION);
        let tools = [tool("whoami", "Short."), tool("run_python", &long)];
        let n = notes(&tools);
        assert!(
            n.contains("### run_python") && n.contains("Last sentence that does not fit at all.")
        );
        assert!(!n.contains("### whoami"));
        assert_eq!(notes(&tools[..1]), "");
    }

    #[test]
    fn tool_names_are_plain() {
        assert!(name_ok("query_features") && name_ok("a-b_C9"));
        assert!(
            !name_ok("")
                && !name_ok("settings.reload")
                && !name_ok("a b")
                && !name_ok(&"n".repeat(65))
        );
    }
}
