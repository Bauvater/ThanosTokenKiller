//! JSON compiler.
//!
//! Two strictly separated strategies:
//!
//! * **minify** (every mode): remove formatting only. The output is verified
//!   to be *semantically equal* to the input by the firewall
//!   ([`Reparse::JsonEquivalent`]), so this can never lose data.
//! * **structural summary** (`balanced` / `maximum` only): for large
//!   homogeneous arrays, describe the schema and keep a few sample rows. This
//!   is lossy by construction, is flagged as such, and the full document stays
//!   in the capsule.

use serde_json::Value;
use ttk_core::firewall::{Candidate, Reparse};
use ttk_core::invariants::Invariant;
use ttk_core::invariants::InvariantKind;
use ttk_core::ir::{IrDoc, IrSection};

use crate::{CompileInput, Compiler, clip};

/// Arrays with at least this many elements are candidates for a summary.
const SUMMARY_MIN_ELEMENTS: usize = 25;
const SAMPLE_ROWS: usize = 3;

pub struct JsonCompiler;

impl Compiler for JsonCompiler {
    fn id(&self) -> &'static str {
        "json.structural"
    }

    fn version(&self) -> u32 {
        1
    }

    fn detect(&self, input: &CompileInput<'_>) -> bool {
        use ttk_core::content::ContentType;
        if input.content.len() as u64 > input.config.limits.max_parse_bytes {
            return false;
        }
        input.content_type == ContentType::Json || {
            let t = input.content.trim_start();
            t.starts_with('{') || t.starts_with('[')
        }
    }

    fn compile(&self, input: &CompileInput<'_>) -> Option<Candidate> {
        let value: Value = serde_json::from_str(input.content).ok()?;
        if depth(&value, 0) > input.config.limits.max_parse_depth {
            return None;
        }

        if input.config.mode.allows_lossy()
            && let Some(c) = self.summarize(&value, input)
        {
            return Some(c);
        }

        let minified = serde_json::to_string(&value).ok()?;
        Some(
            Candidate::new(self.id(), self.version(), minified)
                .reparse(Reparse::JsonEquivalent)
                .note("formatting removed; content is byte-for-byte equivalent JSON"),
        )
    }
}

impl JsonCompiler {
    /// Describe a large homogeneous array instead of shipping all of it.
    fn summarize(&self, value: &Value, input: &CompileInput<'_>) -> Option<Candidate> {
        let (path, array) = find_big_array(value)?;
        if array.len() < SUMMARY_MIN_ELEMENTS {
            return None;
        }

        let mut keys: Vec<(String, String)> = Vec::new();
        if let Some(Value::Object(first)) = array.first() {
            for (k, v) in first {
                keys.push((k.clone(), type_name(v).to_string()));
            }
        }

        let mut doc = IrDoc::new("json")
            .variant("summary")
            .head("array", path.clone())
            .field_num("elements", array.len());

        if !keys.is_empty() {
            let mut sec = IrSection::new("schema");
            for (k, t) in &keys {
                sec = sec.line(format!("{k}: {t}"));
            }
            doc = doc.section(sec);
        }

        let mut sample = IrSection::new("sample").attr("rows", SAMPLE_ROWS.to_string());
        for v in array.iter().take(SAMPLE_ROWS) {
            sample = sample.line(clip(&serde_json::to_string(v).ok()?, 400));
        }
        doc = doc.section(sample);

        if let Some(r) = input.capsule_ref {
            doc = doc.raw_capsule(r.trim_start_matches("cap://"));
        }

        Some(
            Candidate::new("json.summary", self.version(), doc.render())
                .invariants(vec![Invariant::new(InvariantKind::Custom, path)])
                .reparse(Reparse::TokenIr)
                .lossy(true)
                .note("large array summarised; full JSON in the capsule"),
        )
    }
}

fn type_name(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "bool",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

fn depth(v: &Value, current: u32) -> u32 {
    match v {
        Value::Array(a) => a
            .iter()
            .map(|x| depth(x, current + 1))
            .max()
            .unwrap_or(current),
        Value::Object(o) => o
            .values()
            .map(|x| depth(x, current + 1))
            .max()
            .unwrap_or(current),
        _ => current,
    }
}

/// Find the largest array reachable from the root, with its JSON path.
fn find_big_array(v: &Value) -> Option<(String, &Vec<Value>)> {
    fn walk<'a>(v: &'a Value, path: String, best: &mut Option<(String, &'a Vec<Value>)>) {
        match v {
            Value::Array(a) => {
                if best.as_ref().is_none_or(|(_, b)| a.len() > b.len()) {
                    *best = Some((path.clone(), a));
                }
                if let Some(first) = a.first() {
                    walk(first, format!("{path}[0]"), best);
                }
            }
            Value::Object(o) => {
                for (k, val) in o {
                    let child = if path.is_empty() {
                        k.clone()
                    } else {
                        format!("{path}.{k}")
                    };
                    walk(val, child, best);
                }
            }
            _ => {}
        }
    }
    let mut best = None;
    walk(v, String::new(), &mut best);
    best.map(|(p, a)| (if p.is_empty() { "$".to_string() } else { p }, a))
}
