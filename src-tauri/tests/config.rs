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

// ---------------------------------------------------------------------
// Third-party licences. `THIRD-PARTY-LICENSES.html` is generated by
// cargo-about from `about.toml`; these checks keep copyleft out of the
// policy, keep the file in the installer, and keep the credits that the
// licences of mozjpeg (IJG) and libdeflate (MIT) ask for. Structured files
// (TOML, JSON) are parsed, never scanned as text, so that no other spelling
// of the same thing slips past.
// ---------------------------------------------------------------------

/// The licences `about.toml` may accept: permissive ones, plus MPL-2.0 (a
/// user decision, because oxvg pulls in MPL-2.0 crates). Adding to this list
/// is the user's call, never a way to make a licence run pass.
const PERMITTED_LICENCES: &[&str] = &[
    "MIT",
    "Apache-2.0",
    "Apache-2.0 WITH LLVM-exception",
    "BSD-2-Clause",
    "BSD-3-Clause",
    "ISC",
    "Zlib",
    "Unicode-3.0",
    "Unicode-DFS-2016",
    "BSL-1.0",
    "CC0-1.0",
    "0BSD",
    "IJG",
    "MPL-2.0",
];

/// The keys of about.toml that we use. Anything else is refused, so a new
/// cargo-about setting (`workarounds`, `private`, ...) is a decision, not a
/// surprise.
const ABOUT_KEYS: &[&str] = &[
    "accepted",
    "targets",
    "ignore-dev-dependencies",
    "ignore-build-dependencies",
];
/// The keys of a per-crate table (`[some-crate]`).
const CRATE_KEYS: &[&str] = &["accepted", "clarify"];
/// The keys of `[some-crate.clarify]`.
const CLARIFY_KEYS: &[&str] = &["license", "files"];
/// The keys of one `[[some-crate.clarify.files]]` entry.
const CLARIFY_FILE_KEYS: &[&str] = &["path", "checksum", "license"];

/// The sentence the IJG licence asks every product to carry.
const IJG_SENTENCE: &str =
    "This software is based in part on the work of the Independent JPEG Group.";

const LICENCE_FILE: &str = "THIRD-PARTY-LICENSES.html";

fn repo_file(name: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join(name);
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

/// The terms of an SPDX expression that are not permitted. The expression is
/// split on `AND`, `OR` and parentheses; a term `X WITH Y` is permitted when
/// the whole term or `X` is (an exception only narrows a licence).
fn unpermitted_terms(expression: &str) -> Vec<String> {
    let spaced = expression.replace(['(', ')'], " ");
    let mut terms = Vec::new();
    let mut current: Vec<&str> = Vec::new();
    for token in spaced.split_whitespace() {
        if token.eq_ignore_ascii_case("AND") || token.eq_ignore_ascii_case("OR") {
            terms.push(current.join(" "));
            current.clear();
        } else {
            current.push(token);
        }
    }
    terms.push(current.join(" "));
    terms
        .into_iter()
        .filter(|term| {
            let base = term.split(" WITH ").next().unwrap_or(term);
            !PERMITTED_LICENCES.contains(&term.as_str()) && !PERMITTED_LICENCES.contains(&base)
        })
        .collect()
}

/// Problems with one `accepted` value: it is an array of strings, each one
/// a permitted licence.
fn accepted_problems(place: &str, value: &toml::Value, problems: &mut Vec<String>) {
    let Some(items) = value.as_array() else {
        problems.push(format!("{place}: not an array"));
        return;
    };
    for item in items {
        match item.as_str() {
            Some(licence) if PERMITTED_LICENCES.contains(&licence) => {}
            Some(licence) => problems.push(format!("{place}: {licence} is not permitted")),
            None => problems.push(format!("{place}: {item} is not a string")),
        }
    }
}

fn expression_problems(place: &str, value: &toml::Value, problems: &mut Vec<String>) {
    match value.as_str() {
        Some(expression) => {
            for term in unpermitted_terms(expression) {
                problems.push(format!("{place}: {term} is not permitted"));
            }
        }
        None => problems.push(format!("{place}: not a string")),
    }
}

fn unknown_keys(place: &str, table: &toml::Table, known: &[&str], problems: &mut Vec<String>) {
    for key in table.keys().filter(|key| !known.contains(&key.as_str())) {
        problems.push(format!("{place}: unknown key {key}"));
    }
}

/// Everything wrong with an `about.toml`: a file that does not parse, a key
/// or table we do not know, an `accepted` string (global or per crate) or a
/// `clarify` licence that is not permitted. Empty means the policy is sound.
fn about_problems(about_toml: &str) -> Vec<String> {
    let table = match about_toml.parse::<toml::Table>() {
        Ok(table) => table,
        Err(error) => return vec![format!("about.toml does not parse: {error}")],
    };
    let mut problems = Vec::new();
    for (key, value) in &table {
        if key == "accepted" {
            accepted_problems("accepted", value, &mut problems);
        } else if ABOUT_KEYS.contains(&key.as_str()) {
            // A plain setting: nothing licence-related in it.
        } else if let Some(krate) = value.as_table() {
            crate_problems(key, krate, &mut problems);
        } else {
            problems.push(format!("unknown key {key}"));
        }
    }
    problems
}

fn crate_problems(name: &str, krate: &toml::Table, problems: &mut Vec<String>) {
    unknown_keys(name, krate, CRATE_KEYS, problems);
    if let Some(accepted) = krate.get("accepted") {
        accepted_problems(&format!("{name}.accepted"), accepted, problems);
    }
    let Some(clarify) = krate.get("clarify") else {
        return;
    };
    let place = format!("{name}.clarify");
    let Some(clarify) = clarify.as_table() else {
        problems.push(format!("{place}: not a table"));
        return;
    };
    unknown_keys(&place, clarify, CLARIFY_KEYS, problems);
    match clarify.get("license") {
        Some(expression) => expression_problems(&format!("{place}.license"), expression, problems),
        None => problems.push(format!("{place}: no license")),
    }
    let files = clarify.get("files").and_then(toml::Value::as_array);
    for file in files.into_iter().flatten() {
        let Some(file) = file.as_table() else {
            problems.push(format!("{place}.files: an entry is not a table"));
            continue;
        };
        unknown_keys(&format!("{place}.files"), file, CLARIFY_FILE_KEYS, problems);
        if let Some(expression) = file.get("license") {
            expression_problems(&format!("{place}.files.license"), expression, problems);
        }
    }
}

/// Whether the problems name `needle`.
fn names(problems: &[String], needle: &str) -> bool {
    problems.iter().any(|problem| problem.contains(needle))
}

/// The files the installer takes along, as `(source, target)`: the entries of
/// the `bundle.resources` map. The array form has no target, so it gives none.
fn bundled(config: &Value) -> Vec<(String, String)> {
    config["bundle"]["resources"]
        .as_object()
        .into_iter()
        .flatten()
        .filter_map(|(source, target)| Some((source.clone(), target.as_str()?.to_owned())))
        .collect()
}

/// Whether the licence file is bundled under its own name at the resource
/// root (a source `../THIRD-PARTY-LICENSES.html` and that same name as target).
fn bundles_the_licence_file(config: &Value) -> bool {
    bundled(config).iter().any(|(source, target)| {
        Path::new(source)
            .file_name()
            .is_some_and(|name| name == LICENCE_FILE)
            && target == LICENCE_FILE
    })
}

/// Text with every run of whitespace as one space, so a sentence that a file
/// wraps over two lines still matches.
fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn has_ijg_sentence(text: &str) -> bool {
    one_line(text).contains(IJG_SENTENCE)
}

/// The html without its `<pre>` blocks, which hold the licence texts.
fn without_licence_texts(html: &str) -> String {
    let mut parts = html.split("<pre");
    let mut out = parts.next().unwrap_or("").to_owned();
    for part in parts {
        if let Some((_, after)) = part.split_once("</pre>") {
            out.push_str(after);
        }
    }
    out
}

/// The IJG statement stands in the page's own words, outside the licence texts.
fn has_ijg_statement_in_intro(html: &str) -> bool {
    has_ijg_sentence(&without_licence_texts(html))
}

/// The IJG licence text: its own wording, as it appears in libjpeg's README.
fn has_ijg_licence_text(html: &str) -> bool {
    one_line(html).contains("based in part on the work of the Independent JPEG Group")
}

/// libdeflate's MIT notice, as in `libdeflate/COPYING` inside libdeflate-sys.
fn has_libdeflate_notice(html: &str) -> bool {
    let text = one_line(html);
    text.contains("Copyright 2016 Eric Biggers") && text.contains("Copyright 2024 Google LLC")
}

#[test]
fn the_real_about_toml_is_read_and_holds_no_copyleft_and_nothing_outside_the_permitted_set() {
    let about = repo_file("about.toml");
    assert_eq!(about_problems(&about), Vec::<String>::new());
    // Not vacuous: the parse saw the accepted list and the clarification.
    let table = about.parse::<toml::Table>().unwrap();
    let accepted = table["accepted"].as_array().unwrap();
    assert!(
        accepted
            .iter()
            .any(|licence| licence.as_str() == Some("MIT"))
    );
    assert_eq!(
        table["libdeflate-sys"]["clarify"]["license"].as_str(),
        Some("Apache-2.0 AND MIT")
    );
}

#[test]
fn a_copyleft_or_unlisted_licence_in_about_toml_is_caught_in_a_fixture() {
    // Every spelling cargo-about accepts must be read.
    for (fixture, needle) in [
        (
            "accepted = [\"MIT\", \"LGPL-2.1-or-later\"]",
            "LGPL-2.1-or-later",
        ),
        ("accepted = [\"AGPL-3.0-only\"]", "AGPL-3.0-only"),
        ("accepted = [\"EPL-2.0\"]", "EPL-2.0"),
        ("accepted = ['GPL-3.0-only']", "GPL-3.0-only"),
        ("accepted = ['''GPL-3.0-only''']", "GPL-3.0-only"),
        ("accepted = [\"\"\"GPL-3.0-only\"\"\"]", "GPL-3.0-only"),
        (
            "accepted = [\n  \"MIT\", # permissive [ok]\n  \"GPL-3.0-only\",\n]",
            "GPL-3.0-only",
        ),
        (
            "# [not the list\naccepted = [\"GPL-3.0-only\"]",
            "GPL-3.0-only",
        ),
        (
            "[foo]\naccepted = [\"GPL-3.0-only\"]",
            "foo.accepted: GPL-3.0-only",
        ),
        (
            "accepted = [\"MIT\"]\n[foo]\naccepted = 'GPL-3.0-only'",
            "foo.accepted",
        ),
    ] {
        let problems = about_problems(fixture);
        assert!(names(&problems, needle), "{fixture}: {problems:?}");
    }
}

#[test]
fn a_second_accepted_key_is_a_parse_error_not_a_hidden_list() {
    let problems = about_problems("accepted = [\"MIT\"]\naccepted = [\"GPL-3.0-only\"]");
    assert_eq!(problems.len(), 1, "{problems:?}");
    assert!(
        problems[0].starts_with("about.toml does not parse"),
        "{problems:?}"
    );
    let problems = about_problems("accepted = [\"MIT\"");
    assert!(names(&problems, "does not parse"), "{problems:?}");
}

#[test]
fn a_clarify_entry_with_a_copyleft_licence_is_caught_and_a_permitted_one_is_not() {
    let permitted = "accepted = [\"MIT\"]\n\
        [libdeflate-sys.clarify]\nlicense = \"Apache-2.0 AND MIT\"\n\
        [[libdeflate-sys.clarify.files]]\npath = \"LICENSE\"\nchecksum = \"00\"\n\
        [[libdeflate-sys.clarify.files]]\npath = \"libdeflate/COPYING\"\nchecksum = \"11\"\nlicense = \"MIT\"\n";
    assert_eq!(about_problems(permitted), Vec::<String>::new());
    let crate_level = permitted.replace("Apache-2.0 AND MIT", "Apache-2.0 AND GPL-3.0-only");
    assert!(names(&about_problems(&crate_level), "GPL-3.0-only"));
    let file_level = permitted.replace("license = \"MIT\"", "license = \"LGPL-2.1-or-later\"");
    assert!(names(&about_problems(&file_level), "LGPL-2.1-or-later"));
    // A clarify cannot relabel a crate as copyleft by any spelling of the expression.
    for expression in [
        "MIT AND (LGPL-2.1-or-later OR MIT)",
        "(GPL-2.0-only)",
        "GPL-3.0-only WITH Classpath-exception-2.0",
        "mit and gpl-3.0-only",
    ] {
        let fixture = format!("[x.clarify]\nlicense = \"{expression}\"\n");
        assert!(!about_problems(&fixture).is_empty(), "{expression}");
    }
    for expression in [
        "(MIT OR Apache-2.0) AND Unicode-3.0",
        "Apache-2.0 WITH LLVM-exception",
        "IJG AND Zlib AND BSD-3-Clause",
    ] {
        let fixture = format!("[x.clarify]\nlicense = \"{expression}\"\n");
        assert_eq!(
            about_problems(&fixture),
            Vec::<String>::new(),
            "{expression}"
        );
    }
}

#[test]
fn an_unknown_key_or_table_in_about_toml_is_refused() {
    let table = about_problems("[foo.bar]\nx = 1\n");
    assert!(names(&table, "foo: unknown key bar"), "{table:?}");
    let key = about_problems("workarounds = [\"ring\"]\n");
    assert!(names(&key, "unknown key workarounds"), "{key:?}");
    let clarify_key = about_problems("[foo.clarify]\nlicense = \"MIT\"\ngit = []\n");
    assert!(names(&clarify_key, "unknown key git"), "{clarify_key:?}");
    let file_key = about_problems(
        "[foo.clarify]\nlicense = \"MIT\"\n[[foo.clarify.files]]\npath = \"a\"\nchecksum = \"b\"\nstart = \"c\"\n",
    );
    assert!(names(&file_key, "unknown key start"), "{file_key:?}");
    let no_license = about_problems("[foo.clarify]\n");
    assert!(names(&no_license, "no license"), "{no_license:?}");
    // The settings we use are known.
    let known = "accepted = [\"MIT\"]\ntargets = [\"x\"]\n\
                 ignore-dev-dependencies = true\nignore-build-dependencies = true\n";
    assert_eq!(about_problems(known), Vec::<String>::new());
}

#[test]
fn the_installer_bundles_the_licence_file_at_its_own_name_and_the_file_exists() {
    let config = read_json("tauri.conf.json");
    assert!(bundles_the_licence_file(&config));
    let sources = bundled(&config);
    assert!(!sources.is_empty());
    for (source, _) in sources {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(&source);
        assert!(path.is_file(), "bundled {source} does not exist");
    }
}

#[test]
fn a_config_without_the_licence_file_at_its_own_name_is_caught_in_a_fixture() {
    let none = serde_json::json!({ "bundle": { "active": true } });
    assert!(!bundles_the_licence_file(&none));
    let other = serde_json::json!({ "bundle": { "resources": { "../LICENSE": "LICENSE" } } });
    assert!(!bundles_the_licence_file(&other));
    // The right source with another target does not install it under its name.
    let wrong_target = serde_json::json!({
        "bundle": { "resources": { "../THIRD-PARTY-LICENSES.html": "docs/other.html" } }
    });
    assert!(!bundles_the_licence_file(&wrong_target));
    // The array form gives no target, so the file would land under `_up_/`.
    let array = serde_json::json!({
        "bundle": { "resources": ["../THIRD-PARTY-LICENSES.html"] }
    });
    assert!(!bundles_the_licence_file(&array));
    // The name in a value that is not a resource does not count.
    let in_icon = serde_json::json!({
        "bundle": { "icon": ["THIRD-PARTY-LICENSES.html"], "resources": {} }
    });
    assert!(!bundles_the_licence_file(&in_icon));
    let right = serde_json::json!({
        "bundle": { "resources": { "../THIRD-PARTY-LICENSES.html": "THIRD-PARTY-LICENSES.html" } }
    });
    assert!(bundles_the_licence_file(&right));
}

#[test]
fn the_third_party_licences_carry_the_ijg_statement_in_their_intro_and_the_licence_text() {
    let html = repo_file(LICENCE_FILE);
    assert!(has_ijg_statement_in_intro(&html));
    assert!(has_ijg_licence_text(&html));
}

#[test]
fn the_third_party_licences_carry_libdeflates_mit_notice() {
    assert!(has_libdeflate_notice(&repo_file(LICENCE_FILE)));
}

#[test]
fn the_readme_carries_the_ijg_sentence() {
    assert!(has_ijg_sentence(&repo_file("README.md")));
}

#[test]
fn a_text_without_the_ijg_wording_or_the_libdeflate_notice_is_caught_in_a_fixture() {
    assert!(!has_ijg_licence_text("<h1>MIT License</h1>"));
    assert!(!has_ijg_sentence("Lunaepix is MIT licensed."));
    // A changed sentence is not the sentence.
    assert!(!has_ijg_sentence(
        "This software is based on the work of the Independent JPEG Group."
    ));
    // A wrapped line still counts.
    assert!(has_ijg_sentence(
        "This software is based in part on\nthe work of the Independent JPEG Group."
    ));
    // The licence's own condition text is not the statement in the intro.
    let only_in_licence = "<p>Intro</p><pre>(2) the documentation must state that \
        \"this software is based in part on the work of the Independent JPEG Group\".</pre>";
    assert!(has_ijg_licence_text(only_in_licence));
    assert!(!has_ijg_statement_in_intro(only_in_licence));
    let even_capitalised = format!("<p>Intro</p><pre>{IJG_SENTENCE}</pre>");
    assert!(!has_ijg_statement_in_intro(&even_capitalised));
    let in_intro = format!("<main><p>{IJG_SENTENCE}</p></main><pre>licence</pre>");
    assert!(has_ijg_statement_in_intro(&in_intro));
    // libdeflate: one of the two holders is not the notice; neither is none.
    assert!(!has_libdeflate_notice(
        "<pre>Copyright 2016 Eric Biggers</pre>"
    ));
    assert!(!has_libdeflate_notice("<pre>MIT License</pre>"));
    assert!(has_libdeflate_notice(
        "<pre>Copyright 2016 Eric Biggers\nCopyright 2024 Google LLC</pre>"
    ));
}
