//! The Save as name fixture, `tests/fixtures/save-names.json`, which the Rust
//! name rules (`src/names.rs`) and the window's (`ui/names.js`, checked by
//! `tests/ui/names.test.js`) are both tested against, so the two copies of the
//! rules cannot drift apart.
//!
//! Include it from a test with
//! `#[path = "common/save_names.rs"] mod save_names;` (in `src/`, with
//! `../tests/common/save_names.rs`). [`load`] gives the names as plain
//! strings: a long name is a rule in the file, `{"repeat": "a", "times": 196}`,
//! and is expanded here. [`SaveNames::refused`] is keyed by the name of the
//! `NameProblem` variant each name must give (`"Empty"`, `"Reserved"`, ...),
//! so a test that iterates it needs no JSON of its own.
#![allow(dead_code)]

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;

/// The fixture, every name expanded.
pub struct SaveNames {
    /// The extension the stems are saved with (`".jpg"`).
    pub extension: String,
    /// Stems that must be accepted.
    pub accepted: Vec<String>,
    /// `(NameProblem variant name, stems that must give that problem)`, by
    /// variant name in alphabetical order.
    pub refused: Vec<(String, Vec<String>)>,
}

/// Reads and expands the fixture. Panics with the reason when the file is
/// missing or malformed: a test that cannot read its fixture must fail.
pub fn load() -> SaveNames {
    parse(&raw()).unwrap_or_else(|e| panic!("{}: {e}", path().display()))
}

/// Where the fixture is.
pub fn path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../tests/fixtures/save-names.json")
}

/// The fixture file as it is, for a test of how it is written.
pub fn raw() -> String {
    fs::read_to_string(path()).unwrap_or_else(|e| panic!("{}: {e}", path().display()))
}

/// [`load`] on the text of a fixture.
pub fn parse(text: &str) -> Result<SaveNames, String> {
    let value: Value = serde_json::from_str(text).map_err(|e| e.to_string())?;
    let extension = value["extension"]
        .as_str()
        .ok_or("\"extension\" is not a string")?
        .to_owned();
    let expand_all = |list: &Value, what: &str| -> Result<Vec<String>, String> {
        list.as_array()
            .ok_or_else(|| format!("{what} is not a list"))?
            .iter()
            .map(expand)
            .collect()
    };
    let accepted = expand_all(&value["accepted"], "\"accepted\"")?;
    let refused = value["refused"]
        .as_object()
        .ok_or("\"refused\" is not an object")?
        .iter()
        .map(|(problem, list)| Ok((problem.clone(), expand_all(list, problem)?)))
        .collect::<Result<_, String>>()?;
    Ok(SaveNames {
        extension,
        accepted,
        refused,
    })
}

/// The most times a rule may repeat its text: a typo must not fill memory.
const MAX_TIMES: u64 = 10_000;

/// One name of the fixture: a string as it is, or a rule
/// `{"before": text, "repeat": text, "times": n, "then": text}` (`before` and
/// `then` optional) for `before`, then the text `n` times (at most
/// [`MAX_TIMES`]), then `then`. `n` is a whole number written without a
/// decimal point or exponent (`2`, not `2.0`): the window's loader refuses the
/// other spellings, so this one does too. Anything else is an error.
pub fn expand(entry: &Value) -> Result<String, String> {
    if let Some(text) = entry.as_str() {
        return Ok(text.to_owned());
    }
    let rule = entry
        .as_object()
        .ok_or_else(|| format!("not a string or a rule: {entry}"))?;
    if let Some(key) = rule
        .keys()
        .find(|key| !["before", "repeat", "times", "then"].contains(&key.as_str()))
    {
        return Err(format!("unknown key {key:?} in {entry}"));
    }
    let repeat = rule
        .get("repeat")
        .and_then(Value::as_str)
        .ok_or_else(|| format!("\"repeat\" is not a string in {entry}"))?;
    let times = rule
        .get("times")
        .and_then(Value::as_u64)
        .filter(|times| *times <= MAX_TIMES)
        .ok_or_else(|| {
            format!("\"times\" is not a whole number from 0 to {MAX_TIMES} in {entry}")
        })?;
    let optional_text = |key: &str| match rule.get(key) {
        None => Ok(""),
        Some(text) => text
            .as_str()
            .ok_or_else(|| format!("\"{key}\" is not a string in {entry}")),
    };
    let before = optional_text("before")?;
    let then = optional_text("then")?;
    Ok(format!("{before}{}{then}", repeat.repeat(times as usize)))
}
