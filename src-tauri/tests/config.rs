//! Guards on the window's security settings, so a later change cannot widen
//! them silently: the capability grants the three app commands and event
//! listening only, and the content security policy stays strict.

use std::fs;
use std::path::Path;

use serde_json::Value;

/// Every permission the window may have.
const ALLOWED: &[&str] = &[
    "allow-choose-files",
    "allow-choose-folder",
    "allow-open-output-folder",
    "core:event:allow-listen",
    "core:event:allow-unlisten",
];

fn read_json(name: &str) -> Value {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(name);
    serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap()
}

/// The permissions in a capability that are not in [`ALLOWED`]. A
/// permission given as an object (`{ "identifier": ..., "allow": ... }`)
/// carries a scope, which none of ours needs, so it is always unexpected.
fn unexpected_permissions(capability: &Value) -> Vec<String> {
    capability["permissions"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|permission| !permission.as_str().is_some_and(|p| ALLOWED.contains(&p)))
        .map(|permission| permission.to_string())
        .collect()
}

/// What makes a content security policy weaker than we allow: inline code,
/// `eval`, wildcards, `data:`/`blob:`, and any remote source. The only
/// network origin allowed is Tauri's IPC endpoint on Windows. Also missing
/// `default-src 'self'` or `script-src 'self'`.
fn csp_problems(csp: &str) -> Vec<String> {
    let mut problems = Vec::new();
    let directives: Vec<Vec<&str>> = csp
        .split(';')
        .map(|directive| directive.split_whitespace().collect())
        .filter(|tokens: &Vec<&str>| !tokens.is_empty())
        .collect();
    for tokens in &directives {
        for source in &tokens[1..] {
            let lower = source.to_ascii_lowercase();
            let weak = lower.starts_with("'unsafe-")
                || lower == "*"
                || lower.contains("*.")
                || lower == "data:"
                || lower == "blob:"
                || lower == "http:"
                || lower == "https:"
                || ((lower.starts_with("http://") || lower.starts_with("https://"))
                    && lower != "http://ipc.localhost");
            if weak {
                problems.push(format!("{} {source}", tokens[0]));
            }
        }
    }
    for required in ["default-src", "script-src"] {
        let is_self = directives
            .iter()
            .any(|tokens| tokens[0] == required && tokens[1..] == ["'self'"]);
        if !is_self {
            problems.push(format!("{required} is not exactly 'self'"));
        }
    }
    problems
}

#[test]
fn the_capability_grants_the_three_commands_and_event_listening_only() {
    let capability = read_json("capabilities/default.json");
    assert_eq!(unexpected_permissions(&capability), Vec::<String>::new());
    let granted: Vec<&str> = capability["permissions"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(Value::as_str)
        .collect();
    for needed in ALLOWED {
        assert!(granted.contains(needed), "missing {needed}");
    }
    assert_eq!(capability["windows"], serde_json::json!(["main"]));
    // No `remote` block: no web page may use the capability.
    assert!(capability.get("remote").is_none());
}

#[test]
fn a_capability_with_plugin_or_core_default_permissions_is_flagged() {
    let capability = serde_json::json!({
        "permissions": [
            "allow-choose-files",
            "core:default",
            "dialog:default",
            "fs:allow-read-file",
            "opener:allow-open-path",
            "shell:allow-execute",
            { "identifier": "core:event:allow-listen", "allow": [] }
        ]
    });
    assert_eq!(unexpected_permissions(&capability).len(), 6);
}

#[test]
fn the_content_security_policy_is_strict() {
    let config = read_json("tauri.conf.json");
    let csp = config["app"]["security"]["csp"]
        .as_str()
        .expect("the csp is set, as one string");
    assert_eq!(csp_problems(csp), Vec::<String>::new());
    assert!(
        csp.contains("connect-src ipc: http://ipc.localhost"),
        "{csp}"
    );
}

#[test]
fn a_weak_content_security_policy_is_flagged() {
    let weak = "default-src 'self' https://cdn.example.com; \
                script-src 'self' 'unsafe-inline' 'unsafe-eval'; \
                style-src *; img-src data: blob:";
    let problems = csp_problems(weak);
    assert_eq!(problems.len(), 8, "{problems:?}");
    assert!(problems.contains(&"default-src is not exactly 'self'".to_owned()));
    assert!(problems.contains(&"script-src is not exactly 'self'".to_owned()));
    assert!(!csp_problems("script-src 'self'").is_empty());
}

#[test]
fn the_window_is_lunaepix_with_the_global_api_and_native_drag_and_drop() {
    let config = read_json("tauri.conf.json");
    // The version comes from Cargo.toml.
    assert!(config.get("version").is_none());
    assert_eq!(config["app"]["withGlobalTauri"], true);
    let windows = config["app"]["windows"].as_array().unwrap();
    assert_eq!(windows.len(), 1);
    let window = &windows[0];
    assert_eq!(window["label"], "main");
    assert_eq!(window["title"], "Lunaepix");
    // Native drag and drop keeps dropped paths in Rust; `false` would hand
    // the drop to the web page instead.
    assert_ne!(window.get("dragDropEnabled"), Some(&Value::Bool(false)));
    assert!(window["minWidth"].as_u64().unwrap() >= 360);
}
