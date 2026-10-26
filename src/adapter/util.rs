//! Small shared helpers for JSONL-parsing adapters: bounded reads, message
//! text extraction, mtime. Kept dependency-light on purpose.

use serde_json::Value;
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::Path;
use std::time::UNIX_EPOCH;

/// Serializes env-mutating fixture tests against env-reading tests
/// (`format_lines` reads HOME/COLUMNS). Held while HXR_HOME is set.
#[cfg(test)]
pub(crate) static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Parse whole JSON lines from the first `limit` bytes of `path` (a final line
/// truncated by the cap fails to parse and is skipped).
pub(crate) fn read_jsonl_head(path: &Path, limit: u64) -> Vec<Value> {
    read_jsonl_range(path, 0, limit)
}

/// Parse whole JSON lines from the last `limit` bytes of `path` (the first
/// fragment is usually a partial line and is skipped).
pub(crate) fn read_jsonl_tail(path: &Path, limit: u64) -> Vec<Value> {
    let len = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    let start = len.saturating_sub(limit);
    read_jsonl_range(path, start, limit)
}

fn read_jsonl_range(path: &Path, start: u64, limit: u64) -> Vec<Value> {
    let Ok(mut file) = std::fs::File::open(path) else {
        return Vec::new();
    };
    let len = file.metadata().map(|m| m.len()).unwrap_or(0);
    let start = start.min(len);
    let end = start.saturating_add(limit).min(len);
    if file.seek(SeekFrom::Start(start)).is_err() {
        return Vec::new();
    }
    let mut buf = Vec::with_capacity((end - start) as usize);
    if file.take(end - start).read_to_end(&mut buf).is_err() {
        return Vec::new();
    }
    let skip_partial_first = start > 0;
    buf.split(|&b| b == b'\n')
        .skip(usize::from(skip_partial_first))
        .filter_map(|line| serde_json::from_slice(line).ok())
        .collect()
}

/// File mtime as epoch milliseconds (0 when unknown).
pub(crate) fn mtime_ms(path: &Path) -> i64 {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// First line of `s`, cut to at most `max` chars (ellipsis marker when cut).
pub(crate) fn first_line_cut(s: &str, max: usize) -> String {
    let line = s.lines().next().unwrap_or("").trim();
    let mut out: String = line.chars().take(max.saturating_sub(1)).collect();
    if line.chars().count() > max {
        out.push('…');
    }
    out
}

/// Extract displayable text from a chat message body living in `content`
/// (plain string or `[{type:"text","text":...}]`) or `parts` (`[{text:...}]`).
pub(crate) fn text_from_message(msg: &Value) -> Option<String> {
    let body = msg.get("content").or_else(|| msg.get("parts"))?;
    match body {
        Value::String(s) => Some(s.clone()),
        Value::Array(parts) => parts
            .iter()
            .find_map(|p| p.get("text").and_then(|t| t.as_str()).map(str::to_string)),
        _ => None,
    }
}

/// First displayable user message text from ordered parsed entries.
/// `type_tag` is the entry type that marks chat turns ("user" or "message");
/// when the inner message carries a `role`, it must be "user". Skips
/// tool-result / command wrappers (empty or `<`-prefixed text).
pub(crate) fn first_user_text(entries: &[Value], type_tag: &str) -> Option<String> {
    for v in entries {
        if v.get("type").and_then(|t| t.as_str()) != Some(type_tag) {
            continue;
        }
        let Some(msg) = v.get("message") else {
            continue;
        };
        if let Some(role) = msg.get("role").and_then(|r| r.as_str())
            && role != "user"
        {
            continue;
        }
        let Some(text) = text_from_message(msg) else {
            continue;
        };
        let text = text.trim();
        if !text.is_empty() && !text.starts_with('<') {
            return Some(first_line_cut(text, 80));
        }
    }
    None
}

/// Walk a JSON value and collect every string *value* (keys excluded).
pub(crate) fn json_string_values<'a>(v: &'a Value, out: &mut Vec<&'a str>) {
    match v {
        Value::String(s) => out.push(s.as_str()),
        Value::Array(items) => {
            for item in items {
                json_string_values(item, out);
            }
        }
        Value::Object(map) => {
            for value in map.values() {
                json_string_values(value, out);
            }
        }
        _ => {}
    }
}

/// Content-scan one JSONL file: parse each line, match the pattern against
/// the line's string values only (JSON keys never match), return the first
/// snippet. Reads the whole file: content search must see beyond the
/// head-caps used for listing.
pub(crate) fn jsonl_content_match(path: &Path, pat: &super::SearchPattern) -> Option<String> {
    use std::io::BufRead;
    let file = std::fs::File::open(path).ok()?;
    for line in BufReader::new(file).lines() {
        let Ok(line) = line else { continue };
        // sound pre-check ONLY for literals JSON leaves verbatim; anything
        // else (regex, escapable chars) must go through full parsing
        if pat.probe_safe() && pat.find(&line).is_none() {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let mut values = Vec::new();
        json_string_values(&v, &mut values);
        for value in values {
            if let Some(m) = pat.find(value) {
                return Some(super::snippet(value, m.range()));
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn message_body_shapes() {
        let str_content = json!({"role": "user", "content": "plain title"});
        let parts_content = json!({"role": "user", "parts": [{"text": "gemini shape"}]});
        let tool_result = json!({"role": "user", "content": [{"type": "tool_result"}]});
        assert_eq!(
            text_from_message(&str_content).as_deref(),
            Some("plain title")
        );
        assert_eq!(
            text_from_message(&parts_content).as_deref(),
            Some("gemini shape")
        );
        assert_eq!(text_from_message(&tool_result), None);
    }

    #[test]
    fn first_user_text_walks_entries() {
        let entries = vec![
            json!({"type": "message", "message": {"role": "assistant", "content": "nope"}}),
            json!({"type": "message", "message": {"role": "user", "content": [{"type": "tool_result"}]}}),
            json!({"type": "message", "message": {"role": "user", "content": "<command-caveat>x"}}),
            json!({"type": "message", "message": {"role": "user", "content": "the real title\nmore"}}),
        ];
        assert_eq!(
            first_user_text(&entries, "message").as_deref(),
            Some("the real title")
        );
    }

    #[test]
    fn head_tail_of_jsonl() {
        let dir = std::env::temp_dir().join("hxr-test-util");
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("s.jsonl");
        // fixed 14-byte lines (13 chars + \n) -> deterministic cap math
        let lines: Vec<String> = (0..200).map(|i| format!("{{\"n\":\"{i:05}\"}}")).collect();
        let joined = lines.join("\n");
        std::fs::write(&f, &joined).unwrap();
        assert_eq!(joined.len(), 200 * 14 - 1);
        assert_eq!(read_jsonl_head(&f, 1024).len(), 73); // 1024/14 = 73 full lines
        assert_eq!(
            read_jsonl_tail(&f, 1024).first().unwrap()["n"].as_str(),
            Some("00127")
        );
        assert_eq!(read_jsonl_head(&f, 5).len(), 0); // only a partial line
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
