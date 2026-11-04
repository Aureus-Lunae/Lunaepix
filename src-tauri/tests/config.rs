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

/// Every file under `ui/`, at any depth, as `(path relative to ui/, bytes)`.
/// Nothing is excluded: `ui/` is what Tauri embeds in the app, so whatever
/// sits there ships.
fn ui_entries() -> Vec<(String, Vec<u8>)> {
    entries_under(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../ui"))
}

/// Every file under `root`, at any depth, as `(relative path, bytes)`.
fn entries_under(root: &Path) -> Vec<(String, Vec<u8>)> {
    fn walk(dir: &Path, prefix: &str, out: &mut Vec<(String, Vec<u8>)>) {
        for entry in fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            let name = format!("{prefix}{}", path.file_name().unwrap().to_string_lossy());
            if path.is_dir() {
                walk(&path, &format!("{name}/"), out);
            } else {
                out.push((name, fs::read(&path).unwrap()));
            }
        }
    }
    let mut out = Vec::new();
    walk(root, "", &mut out);
    out
}

/// The text files among `entries` (any extension; a file that is not UTF-8
/// text, such as an image, cannot hold code and is skipped).
fn text_files(entries: Vec<(String, Vec<u8>)>) -> Vec<(String, String)> {
    entries
        .into_iter()
        .filter_map(|(name, bytes)| String::from_utf8(bytes).ok().map(|text| (name, text)))
        .collect()
}

/// The text files under `ui/`, as `(path, text)`.
fn ui_files() -> Vec<(String, String)> {
    let files = text_files(ui_entries());
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

/// The paths among `paths` that are node test files. They live in
/// `tests/ui/`, not in `ui/`, so they do not ship inside the app.
fn test_files_in(paths: &[String]) -> Vec<String> {
    paths
        .iter()
        .filter(|path| {
            let name = path.rsplit('/').next().unwrap_or(path);
            name.contains(".test.")
        })
        .cloned()
        .collect()
}

/// The ids that `getElementById` asks for in a script.
fn requested_ids(js: &str) -> Vec<String> {
    js.split("getElementById(")
        .skip(1)
        .filter_map(|rest| {
            let quote = rest.chars().next().filter(|c| matches!(c, '\'' | '"'))?;
            let inner = &rest[1..];
            inner.find(quote).map(|end| inner[..end].to_owned())
        })
        .collect()
}

/// The ids a script asks for that `html` does not have.
fn missing_ids(js: &str, html: &str) -> Vec<String> {
    requested_ids(js)
        .into_iter()
        .filter(|id| !html.contains(&format!("id=\"{id}\"")))
        .collect()
}

/// Text that must appear nowhere in the window's files, in any case.
/// `tauri://` events carry dropped paths; the others write file data into
/// the page as markup, or set a `title`, a `style` or a whole class list
/// (which the CSP, the accessibility rules or the class rules forbid), or run
/// code from a string.
const FORBIDDEN_TEXT: &[&str] = &[
    "tauri://",
    "innerhtml",
    "outerhtml",
    "insertadjacenthtml",
    "autofocus",
    ".title =",
    ".title=",
    "setattribute('title'",
    "setattribute(\"title\"",
    "setattribute('style'",
    "setattribute(\"style\"",
    "classname =",
    "classname=",
    "document.write",
    "eval(",
    "new function",
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
        "el.title = 'Open'",
        "el.title='Open'",
        "el.setAttribute('title', text)",
        "el.setAttribute(\"title\", text)",
        "el.setAttribute('style', 'color: red')",
        "el.setAttribute(\"style\", css)",
        "el.className = 'a b'",
        "el.className='a b'",
        "document.write(html)",
        "eval(code)",
        "new Function('return 1')",
    ] {
        assert_eq!(forbidden_text(bad).len(), 1, "{bad}");
    }
    assert!(forbidden_text("row.textContent = name; tbody.replaceChildren(row)").is_empty());
    // Safe ways of doing the same things are not flagged.
    assert!(
        forbidden_text(
            "tr.classList.add('-failed'); tr.setAttribute('aria-disabled', 'true'); \
             el.style.color = 'red'; const retrieved = items;"
        )
        .is_empty()
    );
}

#[test]
fn the_scan_reads_the_real_window_files() {
    let scanned: Vec<String> = ui_files().into_iter().map(|(name, _)| name).collect();
    for needed in ["index.html", "app.css", "app.js", "format.js", "state.js"] {
        assert!(scanned.contains(&needed.to_owned()), "{needed} not scanned");
    }
}

#[test]
fn the_scan_reads_every_depth_and_every_text_extension_but_not_binary_files() {
    let root = tempfile::tempdir().unwrap();
    fs::create_dir_all(root.path().join("a/b")).unwrap();
    fs::write(root.path().join("x.mjs"), "m").unwrap();
    fs::write(root.path().join("y.svg"), "<svg/>").unwrap();
    fs::write(root.path().join("notes"), "no extension").unwrap();
    fs::write(root.path().join("a/b/deep.js"), "d").unwrap();
    fs::write(root.path().join("a/pic.png"), [0xff, 0xd8, 0x00, 0xfe]).unwrap();
    let mut names: Vec<String> = entries_under(root.path())
        .into_iter()
        .map(|(name, _)| name)
        .collect();
    names.sort();
    assert_eq!(
        names,
        ["a/b/deep.js", "a/pic.png", "notes", "x.mjs", "y.svg"]
    );
    let mut texts: Vec<String> = text_files(entries_under(root.path()))
        .into_iter()
        .map(|(name, _)| name)
        .collect();
    texts.sort();
    assert_eq!(texts, ["a/b/deep.js", "notes", "x.mjs", "y.svg"]);
}

#[test]
fn no_node_test_file_sits_under_ui() {
    // The tests are in tests/ui/. Under ui/ they would ship inside the app.
    let paths: Vec<String> = ui_entries().into_iter().map(|(name, _)| name).collect();
    assert_eq!(test_files_in(&paths), Vec::<String>::new());
}

#[test]
fn a_node_test_file_under_ui_is_flagged_in_a_fixture() {
    let paths = [
        "index.html".to_owned(),
        "app.js".to_owned(),
        "tests/format.test.js".to_owned(),
        "state.test.mjs".to_owned(),
    ];
    assert_eq!(
        test_files_in(&paths),
        vec!["tests/format.test.js", "state.test.mjs"]
    );
    assert!(test_files_in(&paths[..2]).is_empty());
}

#[test]
fn every_element_the_scripts_ask_for_is_in_the_page() {
    let html = index_html();
    let mut asked = 0;
    for (name, text) in ui_files() {
        asked += requested_ids(&text).len();
        assert_eq!(
            missing_ids(&text, &html),
            Vec::<String>::new(),
            "in ui/{name}"
        );
    }
    // Not vacuous: app.js does ask for elements.
    assert!(asked >= 10, "only {asked} getElementById calls were found");
}

#[test]
fn a_missing_element_id_is_caught_in_a_fixture() {
    let html = "<p id=\"yes\"></p><p id=\"also\"></p>";
    assert_eq!(
        missing_ids("document.getElementById('nope')", html),
        vec!["nope"]
    );
    assert_eq!(
        missing_ids(
            "a = document.getElementById(\"yes\"); b = document.getElementById(\"gone\")",
            html
        ),
        vec!["gone"]
    );
    // An id that only appears as text, or as part of another id, is missing.
    assert_eq!(
        missing_ids("document.getElementById('ye')", "<p id=\"yes\">ye</p>"),
        vec!["ye"]
    );
    assert!(
        missing_ids(
            "document.getElementById('yes'); document.getElementById('also')",
            html
        )
        .is_empty()
    );
    // A call with a variable is not an id the check can read.
    assert!(missing_ids("document.getElementById(name)", html).is_empty());
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
