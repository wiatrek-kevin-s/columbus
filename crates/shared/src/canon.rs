//! Canonical JSON and hashing, so the same request always hashes the same
//! regardless of key order or whitespace.

use serde_json::Value;
use sha2::{Digest, Sha256};

/// A character that does not show, shows as something else, or reorders or
/// breaks the text around it: control characters (tab and newline
/// included: callers that allow them check for them first), soft hyphen and
/// other invisible fillers, zero-width and bidirectional formatting
/// characters, Mongolian free variation selectors, line and paragraph
/// separators, variation selectors, interlinear annotation characters and
/// tag characters. One list for everything a person must read exactly as
/// it is used: scripts and tools, tool descriptions, notes, edits.
pub fn hidden_char(c: char) -> bool {
    c.is_control()
        || matches!(c as u32,
            0xAD | 0x034F | 0x061C | 0x115F | 0x1160 | 0x17B4 | 0x17B5 | 0x180B..=0x180F
            | 0x200B..=0x200F | 0x2028..=0x202E | 0x2060..=0x206F | 0x3164 | 0xFE00..=0xFE0F
            | 0xFEFF | 0xFFA0 | 0xFFF9..=0xFFFB | 0xE0000..=0xE0FFF)
}

/// Serialize with object keys sorted, recursively, and no whitespace.
pub fn canonical_json(v: &Value) -> String {
    let mut out = String::new();
    write(v, &mut out);
    out
}

fn write(v: &Value, out: &mut String) {
    match v {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            out.push('{');
            for (i, k) in keys.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(&serde_json::to_string(k).expect("string serializes"));
                out.push(':');
                write(&map[*k], out);
            }
            out.push('}');
        }
        Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write(item, out);
            }
            out.push(']');
        }
        other => out.push_str(&other.to_string()),
    }
}

pub fn sha256_hex(data: &[u8]) -> String {
    hex::encode(Sha256::digest(data))
}

/// SHA-256 and size of a file, read in chunks.
pub fn sha256_file(path: &std::path::Path) -> std::io::Result<(String, u64)> {
    use std::io::Read;
    let mut f = std::fs::File::open(path)?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    let mut n = 0u64;
    loop {
        let k = f.read(&mut buf)?;
        if k == 0 {
            break;
        }
        h.update(&buf[..k]);
        n += k as u64;
    }
    Ok((hex::encode(h.finalize()), n))
}

/// Hash of a tool call: tool name plus canonical arguments.
pub fn request_hash(tool: &str, args: &Value) -> String {
    sha256_hex(format!("{tool}\n{}", canonical_json(args)).as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn key_order_does_not_matter() {
        let a = json!({"b": 1, "a": {"y": [1, 2], "x": "s"}});
        let b = json!({"a": {"x": "s", "y": [1, 2]}, "b": 1});
        assert_eq!(canonical_json(&a), canonical_json(&b));
        assert_eq!(canonical_json(&a), r#"{"a":{"x":"s","y":[1,2]},"b":1}"#);
        assert_ne!(request_hash("t", &a), request_hash("u", &a));
    }
}
