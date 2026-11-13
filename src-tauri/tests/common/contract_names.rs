//! The names the window and the backend share, `tests/fixtures/contract-names.json`:
//! every event the backend sends and every command the window may call. The
//! backend's side is tested against it in `src/events.rs` (every `Event`
//! variant's name) and `tests/config.rs` (the
//! `generate_handler!` list in `src/lib.rs`, the `commands(&[…])` list in
//! `build.rs` and the capability); the window's side reads the same file in
//! `tests/ui/window.test.js` (the events it listens to and the commands it
//! sends). So renaming a name on one side fails the other side's suite.
//!
//! Include it from a test with
//! `#[path = "common/contract_names.rs"] mod contract_names;` (in `src/`, with
//! `../tests/common/contract_names.rs`).
#![allow(dead_code)]

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;

/// The fixture: the two lists, in the file's order.
pub struct ContractNames {
    /// Every event name the window listens to.
    pub events: Vec<String>,
    /// Every command name the window may call.
    pub commands: Vec<String>,
}

/// Reads the fixture. Panics with the reason when the file is missing or
/// malformed: a test that cannot read its fixture must fail.
pub fn load() -> ContractNames {
    parse(&raw()).unwrap_or_else(|e| panic!("{}: {e}", path().display()))
}

/// Where the fixture is.
pub fn path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../tests/fixtures/contract-names.json")
}

/// The fixture file as it is.
pub fn raw() -> String {
    fs::read_to_string(path()).unwrap_or_else(|e| panic!("{}: {e}", path().display()))
}

/// [`load`] on the text of a fixture: an object with exactly the keys
/// `events` and `commands`, each a list of distinct, non-empty strings, as the
/// window's loader (`contractNames` in `tests/ui/window.test.js`) reads it.
/// Anything else is an error.
pub fn parse(text: &str) -> Result<ContractNames, String> {
    let value: Value = serde_json::from_str(text).map_err(|e| e.to_string())?;
    let object = value.as_object().ok_or("the fixture is not an object")?;
    let mut keys: Vec<&str> = object.keys().map(String::as_str).collect();
    keys.sort_unstable();
    if keys != ["commands", "events"] {
        return Err(format!(
            "the fixture's keys are {keys:?}, not commands and events"
        ));
    }
    let list = |key: &str| -> Result<Vec<String>, String> {
        let names = object[key]
            .as_array()
            .ok_or_else(|| format!("{key} is not a list"))?
            .iter()
            .map(|name| {
                name.as_str()
                    .filter(|name| !name.is_empty())
                    .map(str::to_owned)
                    .ok_or_else(|| format!("{key}: {name} is not a name"))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let mut seen = HashSet::new();
        if let Some(twice) = names.iter().find(|name| !seen.insert(name.as_str())) {
            return Err(format!("{key}: {twice} is listed twice"));
        }
        Ok(names)
    };
    Ok(ContractNames {
        events: list("events")?,
        commands: list("commands")?,
    })
}

/// What differs between the names `expected` and the names `found`: "missing
/// x" for each expected name not found, then "extra y" for each name found
/// that is not expected, "y given twice" for one found more than once. The
/// order of either list does not matter.
pub fn problems<A: AsRef<str>, B: AsRef<str>>(expected: &[A], found: &[B]) -> Vec<String> {
    let found: Vec<&str> = found.iter().map(AsRef::as_ref).collect();
    let expected: Vec<&str> = expected.iter().map(AsRef::as_ref).collect();
    let mut problems: Vec<String> = expected
        .iter()
        .filter(|name| !found.contains(name))
        .map(|name| format!("missing {name}"))
        .collect();
    let mut seen = HashSet::new();
    for name in &found {
        if !seen.insert(*name) {
            problems.push(format!("{name} given twice"));
        } else if !expected.contains(name) {
            problems.push(format!("extra {name}"));
        }
    }
    problems
}
