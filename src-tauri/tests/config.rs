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

// ---------------------------------------------------------------------
// Static rules for the window (`ui/`). The content security policy blocks
// most of these at run time; the checks keep them out of the source, and
// keep the page's structure as the accessibility rules need it.
// ---------------------------------------------------------------------

/// The files of the window: everything directly in `ui/` (not the node
/// tests in `ui/tests`), as `(file name, text)`.
fn ui_files() -> Vec<(String, String)> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../ui");
    let mut files = Vec::new();
    for entry in fs::read_dir(&dir).unwrap() {
        let path = entry.unwrap().path();
        let is_source = matches!(
            path.extension().and_then(|e| e.to_str()),
            Some("html" | "css" | "js")
        );
        if path.is_file() && is_source {
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            files.push((name, fs::read_to_string(&path).unwrap()));
        }
    }
    assert!(
        files.iter().any(|(name, _)| name == "index.html"),
        "ui/index.html is missing"
    );
    files
}

fn index_html() -> String {
    ui_files()
        .into_iter()
        .find(|(name, _)| name == "index.html")
        .unwrap()
        .1
}

/// Text that must appear nowhere in the window's files, in any case.
/// `tauri://` events carry dropped paths; the others write server or file
/// data into the page as markup.
const FORBIDDEN_TEXT: &[&str] = &[
    "tauri://",
    "innerhtml",
    "outerhtml",
    "insertadjacenthtml",
    "autofocus",
];

fn forbidden_text(text: &str) -> Vec<&'static str> {
    let lower = text.to_ascii_lowercase();
    FORBIDDEN_TEXT
        .iter()
        .copied()
        .filter(|forbidden| lower.contains(forbidden))
        .collect()
}

/// The tags of an HTML text, from `<` to `>`, with spaces around `=`
/// removed so `title = "x"` reads as `title="x"`.
fn tags(html: &str) -> Vec<String> {
    html.split('<')
        .skip(1)
        .filter_map(|part| part.split_once('>').map(|(tag, _)| tag))
        .map(|tag| {
            let mut tag = tag.replace(['\n', '\t', '\r'], " ");
            while tag.contains(" =") || tag.contains("= ") {
                tag = tag.replace(" =", "=").replace("= ", "=");
            }
            tag
        })
        .collect()
}

/// Problems with attributes in the HTML: a `title` or `style` attribute,
/// an `on...` event handler, or an inline `<script>` (one without `src`).
fn html_attribute_problems(html: &str) -> Vec<String> {
    let mut problems = Vec::new();
    for tag in tags(html) {
        let mut words = tag.split_whitespace();
        let element = words.next().unwrap_or("").to_ascii_lowercase();
        if element == "script" && !tag.to_ascii_lowercase().contains(" src=") {
            problems.push("inline <script> without src".to_owned());
        }
        for word in words {
            let Some((name, _)) = word.split_once('=') else {
                continue;
            };
            let name = name.to_ascii_lowercase();
            let is_handler = name.len() > 2
                && name.starts_with("on")
                && name.chars().all(|c| c.is_ascii_lowercase());
            if name == "title" || name == "style" || is_handler {
                problems.push(format!("{name}= attribute in <{element}>"));
            }
        }
    }
    problems
}

/// What `index.html` needs for the accessibility rules, as problems.
fn structure_problems(html: &str) -> Vec<String> {
    let mut problems = Vec::new();
    let count = |needle: &str| html.matches(needle).count();
    let mut expect = |ok: bool, what: &str| {
        if !ok {
            problems.push(what.to_owned());
        }
    };
    expect(html.contains("<html lang=\"en\""), "html lang=\"en\"");
    expect(count("<h1") == 1, "exactly one <h1>");
    expect(count("<header") == 1, "one <header>");
    expect(count("<main") == 1, "one <main>");
    expect(count("<caption") == 1, "one <caption>");
    expect(count("<title>") == 1, "one <title>");
    expect(count("scope=\"col\"") == 5, "five scope=\"col\" headers");
    problems
}

#[test]
fn the_window_files_hold_no_forbidden_text() {
    for (name, text) in ui_files() {
        assert_eq!(forbidden_text(&text), Vec::<&str>::new(), "in ui/{name}");
    }
}

#[test]
fn forbidden_text_is_caught_in_a_fixture() {
    for bad in [
        "listen('tauri://drag-drop', f)",
        "row.innerHTML = name",
        "row.outerHTML = name",
        "row.insertAdjacentHTML('beforeend', name)",
        "<button AUTOFOCUS>",
        "el.InnerHTML = x",
    ] {
        assert_eq!(forbidden_text(bad).len(), 1, "{bad}");
    }
    assert!(forbidden_text("row.textContent = name; tbody.replaceChildren(row)").is_empty());
}

#[test]
fn the_index_page_has_no_title_style_handler_or_inline_script() {
    let html = index_html();
    assert_eq!(html_attribute_problems(&html), Vec::<String>::new());
    // The module script is loaded from a file.
    assert!(html.contains("<script type=\"module\" src=\"app.js\">"));
}

#[test]
fn html_attribute_problems_are_caught_in_a_fixture() {
    assert_eq!(html_attribute_problems("<button title=\"Open\">").len(), 1);
    assert_eq!(
        html_attribute_problems("<button title = \"Open\">").len(),
        1
    );
    assert_eq!(html_attribute_problems("<p style=\"color: red\">").len(), 1);
    assert_eq!(html_attribute_problems("<p STYLE='x'>").len(), 1);
    assert_eq!(
        html_attribute_problems("<button onclick=\"go()\">").len(),
        1
    );
    assert_eq!(html_attribute_problems("<body onload = \"go()\">").len(), 1);
    assert_eq!(html_attribute_problems("<script>run()</script>").len(), 1);
    assert_eq!(
        html_attribute_problems("<script type=\"module\">run()</script>").len(),
        1
    );
    // Clean markup, including the <title> element and a script from a file.
    assert!(
        html_attribute_problems(
            "<title>Lunaepix</title><script type=\"module\" src=\"app.js\"></script>\
         <button class=\"on\" type=\"button\">Open</button>\
         <link rel=\"stylesheet\" href=\"a.css\">"
        )
        .is_empty()
    );
}

#[test]
fn the_index_page_has_the_structure_the_accessibility_rules_need() {
    assert_eq!(structure_problems(&index_html()), Vec::<String>::new());
}

#[test]
fn a_page_without_that_structure_is_flagged() {
    assert_eq!(structure_problems("").len(), 7);
    let two_headings = "<html lang=\"en\"><title>x</title><h1>a</h1><h1>b</h1>\
                        <header></header><main></main><caption>c</caption>\
                        <th scope=\"col\"><th scope=\"col\"><th scope=\"col\">\
                        <th scope=\"col\"><th scope=\"col\">";
    assert_eq!(structure_problems(two_headings), vec!["exactly one <h1>"]);
    let no_lang = two_headings
        .replace("<h1>b</h1>", "")
        .replace("lang=\"en\"", "");
    assert_eq!(structure_problems(&no_lang), vec!["html lang=\"en\""]);
}
