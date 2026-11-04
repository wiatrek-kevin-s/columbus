//! Writers for the two common output types: `table` (CSV) and `layer`
//! (GeoJSON). Both stream to disk through a buffer and create the parent
//! folder when needed.

use std::fs::File;
use std::io::{self, BufWriter, Write};
use std::path::Path;

use serde_json::Value;

fn create(path: &Path) -> io::Result<BufWriter<File>> {
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir)?;
    }
    Ok(BufWriter::new(File::create(path)?))
}

/// One CSV field, quoted per RFC 4180 when it contains a comma, a double
/// quote, CR or LF (or begins or ends with a space, which some readers
/// trim). Quotes inside are doubled.
pub fn csv_field(field: &str) -> std::borrow::Cow<'_, str> {
    let needs =
        field.contains([',', '"', '\r', '\n']) || field.starts_with(' ') || field.ends_with(' ');
    if needs {
        format!("\"{}\"", field.replace('"', "\"\"")).into()
    } else {
        field.into()
    }
}

fn write_record<W: Write, S: AsRef<str>>(
    w: &mut W,
    fields: impl IntoIterator<Item = S>,
) -> io::Result<()> {
    for (i, f) in fields.into_iter().enumerate() {
        if i > 0 {
            w.write_all(b",")?;
        }
        w.write_all(csv_field(f.as_ref()).as_bytes())?;
    }
    // RFC 4180 line ending.
    w.write_all(b"\r\n")
}

/// Write a CSV file (UTF-8, RFC 4180: CRLF line endings, fields quoted
/// when needed). Returns the number of data rows written, which is what
/// [`Context::set_output_count`](crate::Context::set_output_count) wants
/// for `"rows"`.
///
/// ```no_run
/// let n = columbus_plugin::write_csv(
///     "out.csv",
///     &["metric", "value"],
///     [vec!["rows".to_string(), "12".to_string()]],
/// )?;
/// # Ok::<(), std::io::Error>(())
/// ```
pub fn write_csv<P, H, R, S>(path: P, header: &[H], rows: R) -> io::Result<u64>
where
    P: AsRef<Path>,
    H: AsRef<str>,
    R: IntoIterator,
    R::Item: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut w = create(path.as_ref())?;
    write_record(&mut w, header.iter().map(AsRef::as_ref))?;
    let mut n = 0;
    for row in rows {
        write_record(&mut w, row)?;
        n += 1;
    }
    w.flush()?;
    Ok(n)
}

/// Write a GeoJSON FeatureCollection, one feature at a time, without
/// holding the collection in memory. Each item should be a GeoJSON
/// `Feature` object (see [`feature`]). Returns the number of features
/// written (the `"features"` count for
/// [`Context::set_output_count`](crate::Context::set_output_count)).
pub fn write_geojson<P, I>(path: P, features: I) -> io::Result<u64>
where
    P: AsRef<Path>,
    I: IntoIterator<Item = Value>,
{
    let mut w = create(path.as_ref())?;
    w.write_all(b"{\"type\":\"FeatureCollection\",\"features\":[")?;
    let mut n = 0u64;
    for f in features {
        if n > 0 {
            w.write_all(b",\n")?;
        } else {
            w.write_all(b"\n")?;
        }
        serde_json::to_writer(&mut w, &f)?;
        n += 1;
    }
    w.write_all(b"\n]}\n")?;
    w.flush()?;
    Ok(n)
}

/// Build a GeoJSON `Feature` from a geometry object (or `null`) and
/// properties.
pub fn feature(geometry: Option<Value>, properties: serde_json::Map<String, Value>) -> Value {
    serde_json::json!({
        "type": "Feature",
        "geometry": geometry.unwrap_or(Value::Null),
        "properties": properties,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn csv_quoting() {
        assert_eq!(csv_field("plain"), "plain");
        assert_eq!(csv_field(""), "");
        assert_eq!(csv_field("a,b"), "\"a,b\"");
        assert_eq!(csv_field("say \"hi\""), "\"say \"\"hi\"\"\"");
        assert_eq!(csv_field("two\nlines"), "\"two\nlines\"");
        assert_eq!(csv_field("cr\r"), "\"cr\r\"");
        assert_eq!(csv_field(" padded"), "\" padded\"");
        assert_eq!(csv_field("IH 35"), "IH 35");
        assert_eq!(csv_field("ñandú"), "ñandú");
    }

    #[test]
    fn csv_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("out.csv");
        let rows = vec![vec!["1", "a,b"], vec!["2", "q\"uote"], vec!["3", ""]];
        let n = write_csv(&path, &["id", "name"], rows).unwrap();
        assert_eq!(n, 3);
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(text, "id,name\r\n1,\"a,b\"\r\n2,\"q\"\"uote\"\r\n3,\r\n");
    }

    #[test]
    fn csv_with_owned_strings_and_no_rows() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("empty.csv");
        let n = write_csv(&path, &["metric".to_string()], Vec::<Vec<String>>::new()).unwrap();
        assert_eq!(n, 0);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "metric\r\n");
    }

    #[test]
    fn geojson_stream() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out.geojson");
        let feats = (0..3).map(|i| {
            let mut p = serde_json::Map::new();
            p.insert("i".into(), json!(i));
            feature(Some(json!({"type": "Point", "coordinates": [i, i]})), p)
        });
        assert_eq!(write_geojson(&path, feats).unwrap(), 3);
        let v: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(v["type"], "FeatureCollection");
        assert_eq!(v["features"].as_array().unwrap().len(), 3);
        assert_eq!(v["features"][2]["properties"]["i"], 2);

        let empty = dir.path().join("empty.geojson");
        assert_eq!(write_geojson(&empty, std::iter::empty()).unwrap(), 0);
        let v: Value = serde_json::from_str(&std::fs::read_to_string(&empty).unwrap()).unwrap();
        assert_eq!(v, json!({"type": "FeatureCollection", "features": []}));
    }
}
