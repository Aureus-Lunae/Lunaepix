//! Guards on the window's security settings, so a later change cannot widen
//! them silently: the capability grants the twelve app commands and event
//! listening only, and the content security policy stays strict (only
//! `img-src` names the `thumb` and `preview` schemes).

use std::cell::RefCell;
use std::collections::HashSet;
use std::fs;
use std::path::Path;

use html5ever::tokenizer::states::RawKind;
use html5ever::tokenizer::{
    BufferQueue, TagKind, Token, TokenSink, TokenSinkResult, Tokenizer, TokenizerOpts,
};
use serde_json::Value;

/// Every permission the window may have.
const ALLOWED: &[&str] = &[
    "allow-choose-files",
    "allow-clear-list",
    "allow-readd-list",
    "allow-choose-output-folder",
    "allow-clear-output-folder",
    "allow-start-run",
    "allow-open-output-folder",
    "allow-set-view",
    "allow-choose-image",
    "allow-preview-image",
    "allow-save-image",
    "allow-move-drop-to-batch",
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
/// network origins allowed are Tauri's IPC endpoint on Windows, and the
/// `thumb` and `preview` schemes' origins on Windows in `img-src` only. Also missing
/// `default-src 'self'` or `script-src 'self'`.
fn csp_problems(csp: &str) -> Vec<String> {
    let mut problems = Vec::new();
    let directives: Vec<Vec<&str>> = csp
        .split(';')
        .map(|directive| directive.split_whitespace().collect())
        .filter(|tokens: &Vec<&str>| !tokens.is_empty())
        .collect();
    for tokens in &directives {
        let img_src = tokens[0].eq_ignore_ascii_case("img-src");
        for source in &tokens[1..] {
            let lower = source.to_ascii_lowercase();
            let allowed_origin = lower == "http://ipc.localhost"
                || (img_src
                    && (lower == "http://thumb.localhost" || lower == "http://preview.localhost"));
            let weak = lower.starts_with("'unsafe-")
                || lower == "*"
                || lower.contains("*.")
                || lower == "data:"
                || lower == "blob:"
                || lower == "http:"
                || lower == "https:"
                || ((lower.starts_with("http://") || lower.starts_with("https://"))
                    && !allowed_origin);
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

/// The policy as it must be, directive by directive. Only `img-src` differs
/// from the policy before thumbnails: the `thumb` and `preview` schemes, as
/// Linux and macOS name them (`thumb:`, `preview:`) and as Windows serves
/// them (`http://thumb.localhost`, `http://preview.localhost`).
const EXPECTED_CSP: &[(&str, &[&str])] = &[
    ("default-src", &["'self'"]),
    ("script-src", &["'self'"]),
    ("style-src", &["'self'"]),
    (
        "img-src",
        &[
            "'self'",
            "thumb:",
            "http://thumb.localhost",
            "preview:",
            "http://preview.localhost",
        ],
    ),
    ("font-src", &["'self'"]),
    ("connect-src", &["ipc:", "http://ipc.localhost"]),
    ("object-src", &["'none'"]),
    ("base-uri", &["'none'"]),
    ("form-action", &["'none'"]),
    ("frame-src", &["'none'"]),
];

/// How a policy differs from [`EXPECTED_CSP`]: a directive missing, given
/// twice, not expected, or with other sources. Names and sources are
/// compared without case (as the browser reads them) and sources in any
/// order.
fn csp_shape_problems(csp: &str) -> Vec<String> {
    let mut problems = Vec::new();
    let directives: Vec<(String, Vec<String>)> = csp
        .split(';')
        .map(|directive| directive.split_whitespace().collect::<Vec<_>>())
        .filter(|tokens| !tokens.is_empty())
        .map(|tokens| {
            let mut sources: Vec<String> =
                tokens[1..].iter().map(|s| s.to_ascii_lowercase()).collect();
            sources.sort();
            (tokens[0].to_ascii_lowercase(), sources)
        })
        .collect();
    for (name, _) in &directives {
        if !EXPECTED_CSP.iter().any(|(expected, _)| expected == name) {
            problems.push(format!("unexpected directive {name}"));
        }
    }
    for (name, expected) in EXPECTED_CSP {
        let given: Vec<&Vec<String>> = directives
            .iter()
            .filter(|(directive, _)| directive == name)
            .map(|(_, sources)| sources)
            .collect();
        let mut expected: Vec<String> = expected.iter().map(|s| s.to_ascii_lowercase()).collect();
        expected.sort();
        match given.as_slice() {
            [] => problems.push(format!("missing {name}")),
            [sources] if **sources == expected => {}
            [sources] => problems.push(format!("{name} is {sources:?}, not {expected:?}")),
            _ => problems.push(format!("{name} given more than once")),
        }
    }
    problems
}

/// Every problem [`csp_problems`] and [`csp_shape_problems`] find.
fn all_csp_problems(csp: &str) -> Vec<String> {
    let mut problems = csp_problems(csp);
    problems.extend(csp_shape_problems(csp));
    problems
}

/// The policy the window must have, written out as a fixture.
const GOOD_CSP: &str = "default-src 'self'; script-src 'self'; style-src 'self'; \
    img-src 'self' thumb: http://thumb.localhost preview: http://preview.localhost; \
    font-src 'self'; connect-src ipc: http://ipc.localhost; object-src 'none'; \
    base-uri 'none'; form-action 'none'; frame-src 'none'";

/// The `img-src` directive of [`GOOD_CSP`].
const GOOD_IMG_SRC: &str =
    "img-src 'self' thumb: http://thumb.localhost preview: http://preview.localhost";

/// The permissions a capability grants that are plain names, in order.
fn granted(capability: &Value) -> Vec<&str> {
    capability["permissions"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(Value::as_str)
        .collect()
}

/// What is wrong with a capability's permissions: one that is not in
/// [`ALLOWED`], one of [`ALLOWED`] that is missing or given twice, and any
/// count of `allow-` commands other than twelve.
fn capability_problems(capability: &Value) -> Vec<String> {
    let mut problems: Vec<String> = unexpected_permissions(capability)
        .into_iter()
        .map(|permission| format!("unexpected {permission}"))
        .collect();
    let granted = granted(capability);
    for allowed in ALLOWED {
        match granted.iter().filter(|p| *p == allowed).count() {
            1 => {}
            0 => problems.push(format!("missing {allowed}")),
            _ => problems.push(format!("{allowed} given more than once")),
        }
    }
    let commands = granted.iter().filter(|p| p.starts_with("allow-")).count();
    if commands != 12 {
        problems.push(format!("{commands} app commands, not twelve"));
    }
    problems
}

#[test]
fn a_capability_with_eleven_or_thirteen_commands_is_flagged_in_a_fixture() {
    let all: Vec<Value> = ALLOWED.iter().map(|p| Value::from(*p)).collect();
    // The control: the full list has nothing to report.
    assert_eq!(
        capability_problems(&serde_json::json!({ "permissions": all })),
        Vec::<String>::new()
    );
    // Eleven: no `allow-move-drop-to-batch`.
    let eleven: Vec<Value> = all
        .iter()
        .filter(|p| p.as_str() != Some("allow-move-drop-to-batch"))
        .cloned()
        .collect();
    assert_eq!(
        capability_problems(&serde_json::json!({ "permissions": eleven })),
        [
            "missing allow-move-drop-to-batch",
            "11 app commands, not twelve"
        ]
    );
    // Thirteen: an extra command.
    let mut thirteen = all.clone();
    thirteen.push(Value::from("allow-delete-everything"));
    assert_eq!(
        capability_problems(&serde_json::json!({ "permissions": thirteen })),
        [
            r#"unexpected "allow-delete-everything""#,
            "13 app commands, not twelve"
        ]
    );
    // A permission given twice.
    let mut twice = all;
    twice.push(Value::from("allow-set-view"));
    assert!(
        capability_problems(&serde_json::json!({ "permissions": twice }))
            .contains(&"allow-set-view given more than once".to_owned())
    );
}

#[test]
fn the_capability_grants_exactly_the_twelve_commands_and_event_listening() {
    let capability = read_json("capabilities/default.json");
    assert_eq!(capability_problems(&capability), Vec::<String>::new());
    assert_eq!(
        granted(&capability).len(),
        ALLOWED.len(),
        "nothing but the allowed permissions"
    );
    assert_eq!(capability["windows"], serde_json::json!(["main"]));
    // No `remote` block: no web page may use the capability.
    assert!(capability.get("remote").is_none());
}

#[test]
fn a_capability_listing_choose_folder_is_flagged() {
    let mut permissions: Vec<Value> = ALLOWED.iter().map(|p| Value::from(*p)).collect();
    permissions.push(Value::from("allow-choose-folder"));
    let capability = serde_json::json!({ "permissions": permissions });
    assert_eq!(
        unexpected_permissions(&capability),
        [r#""allow-choose-folder""#]
    );
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
    assert_eq!(csp_shape_problems(csp), Vec::<String>::new());
}

#[test]
fn an_img_src_with_anything_more_or_another_changed_directive_is_flagged() {
    // The control: the policy as it must be, also in another spelling.
    assert_eq!(all_csp_problems(GOOD_CSP), Vec::<String>::new());
    let respelled = GOOD_CSP.replace(
        GOOD_IMG_SRC,
        "IMG-SRC http://PREVIEW.localhost Preview: http://THUMB.localhost 'SELF'  thumb:",
    );
    assert_eq!(all_csp_problems(&respelled), Vec::<String>::new());

    let img_src = GOOD_IMG_SRC;
    for extra in [
        "data:",
        "blob:",
        "*",
        "https:",
        "http:",
        "http://evil.localhost",
        "http://*.localhost",
        "https://thumb.localhost",
        "https://preview.localhost",
        "thumbs:",
        "previews:",
        "'unsafe-inline'",
    ] {
        let widened = GOOD_CSP.replace(img_src, &format!("{img_src} {extra}"));
        assert_ne!(widened, GOOD_CSP);
        assert!(!all_csp_problems(&widened).is_empty(), "{extra}");
    }
    let changed = [
        // The scheme anywhere but `img-src`.
        GOOD_CSP.replace(
            "script-src 'self'",
            "script-src 'self' http://thumb.localhost",
        ),
        GOOD_CSP.replace("script-src 'self'", "script-src 'self' thumb:"),
        GOOD_CSP.replace(
            "connect-src ipc: http://ipc.localhost",
            "connect-src ipc: http://ipc.localhost thumb:",
        ),
        // `img-src` narrowed, given twice, or dropped.
        GOOD_CSP.replace(img_src, "img-src 'self' thumb: http://thumb.localhost"),
        GOOD_CSP.replace(img_src, &format!("{img_src}; img-src 'self'")),
        GOOD_CSP.replace(&format!("{img_src}; "), ""),
        // Another directive changed or added.
        GOOD_CSP.replace("object-src 'none'", "object-src 'self'"),
        format!("{GOOD_CSP}; worker-src 'self'"),
    ];
    for csp in &changed {
        assert_ne!(csp, GOOD_CSP);
        assert!(!all_csp_problems(csp).is_empty(), "{csp}");
    }
}

/// The policy after Single image mode, exactly (Global Constraints).
const NEW_CSP: &str = "default-src 'self'; script-src 'self'; style-src 'self'; img-src 'self' thumb: http://thumb.localhost preview: http://preview.localhost; font-src 'self'; connect-src ipc: http://ipc.localhost; object-src 'none'; base-uri 'none'; form-action 'none'; frame-src 'none'";

#[test]
fn the_csp_is_exactly_the_new_string() {
    let config = read_json("tauri.conf.json");
    assert_eq!(config["app"]["security"]["csp"].as_str(), Some(NEW_CSP));
    // The fixture the other checks use is the same policy.
    assert_eq!(GOOD_CSP, NEW_CSP);
    assert_eq!(all_csp_problems(NEW_CSP), Vec::<String>::new());
}

#[test]
fn a_csp_without_the_preview_scheme_is_flagged_in_a_fixture() {
    let without = [
        // The policy before this work.
        GOOD_CSP.replace(" preview: http://preview.localhost", ""),
        GOOD_CSP.replace(" preview:", ""),
        GOOD_CSP.replace(" http://preview.localhost", ""),
        // The scheme moved out of `img-src` or spelled for another origin.
        GOOD_CSP
            .replace(" preview: http://preview.localhost", "")
            .replace("connect-src ipc:", "connect-src preview: ipc:"),
        GOOD_CSP.replace("http://preview.localhost", "https://preview.localhost"),
        GOOD_CSP.replace("preview:", "previews:"),
    ];
    for csp in &without {
        assert_ne!(csp, GOOD_CSP);
        assert!(!all_csp_problems(csp).is_empty(), "{csp}");
    }
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

/// The bundle formats we ship: deb and rpm for Linux, NSIS and MSI for
/// Windows. Never `all` and never an AppImage, which would carry about 190
/// LGPL/GPL system libraries without notices or a source offer.
const BUNDLE_TARGETS: &[&str] = &["deb", "rpm", "nsis", "msi"];

/// What is wrong with `bundle.targets`: it must be a non-empty list, and
/// every entry one of [`BUNDLE_TARGETS`]. A missing list means Tauri's default
/// (`all`), and the string `"all"` is not a list; both are refused.
fn bundle_target_problems(config: &Value) -> Vec<String> {
    let Some(targets) = config
        .get("bundle")
        .and_then(|bundle| bundle.get("targets"))
    else {
        return vec!["bundle.targets: no list (Tauri then builds all)".to_owned()];
    };
    let Some(list) = targets.as_array() else {
        return vec![format!("bundle.targets: {targets} is not a list")];
    };
    let mut problems = Vec::new();
    if list.is_empty() {
        problems.push("bundle.targets: the list is empty".to_owned());
    }
    for target in list {
        match target.as_str() {
            Some(name) if BUNDLE_TARGETS.contains(&name) => {}
            Some(name) => problems.push(format!(
                "bundle.targets: {name} is not one of {BUNDLE_TARGETS:?}"
            )),
            None => problems.push(format!("bundle.targets: {target} is not a string")),
        }
    }
    problems
}

#[test]
fn the_bundle_targets_are_the_explicit_list_deb_rpm_nsis_msi() {
    let config = read_json("tauri.conf.json");
    assert_eq!(bundle_target_problems(&config), Vec::<String>::new());
    assert_eq!(
        config["bundle"]["targets"],
        serde_json::json!(BUNDLE_TARGETS)
    );
}

#[test]
fn a_bundle_target_list_outside_the_four_is_caught_in_a_fixture() {
    let with = |targets: Value| serde_json::json!({ "bundle": { "targets": targets } });
    for (fixture, needle) in [
        (with(serde_json::json!("all")), "not a list"),
        (with(serde_json::json!("deb")), "not a list"),
        (with(serde_json::json!(["all"])), "all"),
        (with(serde_json::json!(["deb", "appimage"])), "appimage"),
        (with(serde_json::json!(["AppImage"])), "AppImage"),
        (with(serde_json::json!(["rpm", "dmg"])), "dmg"),
        (with(serde_json::json!(["msi", 3])), "not a string"),
        (with(serde_json::json!([])), "empty"),
        (
            serde_json::json!({ "bundle": { "active": true } }),
            "no list",
        ),
        (serde_json::json!({}), "no list"),
    ] {
        let problems = bundle_target_problems(&fixture);
        assert!(names(&problems, needle), "{fixture}: {problems:?}");
    }
    // Any non-empty choice among the four is fine.
    for ok in [
        serde_json::json!(["deb"]),
        serde_json::json!(BUNDLE_TARGETS),
    ] {
        assert_eq!(bundle_target_problems(&with(ok)), Vec::<String>::new());
    }
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

/// The ids a script asks for that `html` does not have as an `id` attribute.
fn missing_ids(js: &str, html: &str) -> Vec<String> {
    let present: HashSet<String> = start_tags(html)
        .into_iter()
        .filter_map(|tag| tag.attr("id").map(str::to_owned))
        .collect();
    requested_ids(js)
        .into_iter()
        .filter(|id| !present.contains(id))
        .collect()
}

/// A lint, not a guarantee: it looks for known spellings in the text of the
/// window's files, after whitespace is normalised (see [`lint_normalised`]),
/// in any case. Another spelling of the same thing can still pass. The
/// reliable guards are the capability and the content security policy; this
/// keeps the common ways out of the source. `tauri://` events carry dropped
/// paths (`onDragDropEvent` subscribes to one without the text); the others
/// write file data into the page as markup, or set a `title`, a `style` or a
/// whole class list (which the CSP, the accessibility rules or the class rules
/// forbid), or run code from a string. Patterns are written in the normalised
/// form: lower case, no space next to punctuation.
const JS_LINT_PATTERNS: &[&str] = &[
    "tauri://",
    "ondragdropevent",
    "innerhtml",
    "outerhtml",
    "insertadjacenthtml",
    "autofocus",
    ".title=",
    "['title']",
    "[\"title\"]",
    "[`title`]",
    "setattribute('title'",
    "setattribute(\"title\"",
    "setattribute(`title`",
    "setattribute('style'",
    "setattribute(\"style\"",
    "setattribute(`style`",
    "classname=",
    "document.write",
    "eval(",
    "new function",
];

/// The characters that a space next to is dropped by [`lint_normalised`].
const LINT_PUNCTUATION: &[char] = &['(', ')', '[', ']', '=', '.', '\'', '"', '`'];

/// Lower case, every run of whitespace as one space, and no space next to
/// punctuation, so `el.setAttribute (\n 'title'` reads as
/// `el.setattribute('title'`.
fn lint_normalised(text: &str) -> String {
    let mut text = text
        .to_ascii_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    for punctuation in LINT_PUNCTUATION {
        text = text
            .replace(&format!(" {punctuation}"), &punctuation.to_string())
            .replace(&format!("{punctuation} "), &punctuation.to_string());
    }
    text
}

fn js_lint_hits(text: &str) -> Vec<&'static str> {
    let normalised = lint_normalised(text);
    JS_LINT_PATTERNS
        .iter()
        .copied()
        .filter(|pattern| normalised.contains(pattern))
        .collect()
}

// ---------------------------------------------------------------------
// HTML is read as tokens (html5ever's tokenizer), never scanned as text: a
// `>` inside an attribute value, a tag spelled in a comment, a script or a
// title, or `data-id` next to `id` all read as what they are.
// ---------------------------------------------------------------------

/// What the tokenizer found, reduced to what the checks need: start tags with
/// their attributes (names in lower case, values decoded), end tags, text and
/// comments. Doctypes are dropped.
enum Piece {
    Start(StartTag),
    End(String),
    Text(String),
    Comment(String),
}

struct StartTag {
    name: String,
    attrs: Vec<(String, String)>,
}

impl StartTag {
    fn attr(&self, name: &str) -> Option<&str> {
        self.attrs
            .iter()
            .find(|(attr, _)| attr == name)
            .map(|(_, value)| value.as_str())
    }
}

/// Collects the pieces. A bare tokenizer does not know that the text inside
/// `<script>`, `<style>` and `<title>` is not markup (the tree builder tells it),
/// so this sink does the same for the elements that matter.
struct Collector(RefCell<Vec<Piece>>);

impl TokenSink for Collector {
    type Handle = ();

    fn process_token(&self, token: Token, _line_number: u64) -> TokenSinkResult<()> {
        let Token::TagToken(tag) = token else {
            match token {
                Token::CharacterTokens(text) => {
                    self.0.borrow_mut().push(Piece::Text(text.to_string()));
                }
                Token::CommentToken(text) => {
                    self.0.borrow_mut().push(Piece::Comment(text.to_string()));
                }
                _ => {}
            }
            return TokenSinkResult::Continue;
        };
        let name = tag.name.to_string();
        if tag.kind == TagKind::EndTag {
            self.0.borrow_mut().push(Piece::End(name));
            return TokenSinkResult::Continue;
        }
        let result = match name.as_str() {
            "script" => TokenSinkResult::RawData(RawKind::ScriptData),
            "style" | "xmp" | "iframe" | "noembed" | "noframes" => {
                TokenSinkResult::RawData(RawKind::Rawtext)
            }
            "title" | "textarea" => TokenSinkResult::RawData(RawKind::Rcdata),
            "plaintext" => TokenSinkResult::Plaintext,
            _ => TokenSinkResult::Continue,
        };
        let attrs = tag
            .attrs
            .iter()
            .map(|attr| (attr.name.local.to_string(), attr.value.to_string()))
            .collect();
        self.0
            .borrow_mut()
            .push(Piece::Start(StartTag { name, attrs }));
        result
    }
}

fn pieces(html: &str) -> Vec<Piece> {
    let queue = BufferQueue::default();
    queue.push_back(html.into());
    let tokenizer = Tokenizer::new(
        Collector(RefCell::new(Vec::new())),
        TokenizerOpts::default(),
    );
    let _ = tokenizer.feed(&queue);
    tokenizer.end();
    tokenizer.sink.0.take()
}

fn start_tags(html: &str) -> Vec<StartTag> {
    pieces(html)
        .into_iter()
        .filter_map(|piece| match piece {
            Piece::Start(tag) => Some(tag),
            _ => None,
        })
        .collect()
}

/// The comments of `html`, in order.
fn comments(html: &str) -> Vec<String> {
    pieces(html)
        .into_iter()
        .filter_map(|piece| match piece {
            Piece::Comment(text) => Some(text),
            _ => None,
        })
        .collect()
}

/// The text nodes of `html` that are not inside one of the `skipped`
/// elements. Comments are never text.
fn text_outside(html: &str, skipped: &[&str]) -> String {
    let mut depth = 0usize;
    let mut text = String::new();
    for piece in pieces(html) {
        match piece {
            Piece::Start(tag) if skipped.contains(&tag.name.as_str()) => depth += 1,
            Piece::End(name) if skipped.contains(&name.as_str()) => {
                depth = depth.saturating_sub(1);
            }
            Piece::Text(chunk) if depth == 0 => text.push_str(&chunk),
            _ => {}
        }
    }
    text
}

/// Problems with attributes in the HTML: a `title` or `style` attribute,
/// an `on...` event handler, or an inline `<script>` (one without a `src`
/// attribute).
fn html_attribute_problems(html: &str) -> Vec<String> {
    let mut problems = Vec::new();
    for tag in start_tags(html) {
        if tag.name == "script" && tag.attr("src").is_none() {
            problems.push("inline <script> without src".to_owned());
        }
        for (name, _) in &tag.attrs {
            let is_handler = name.len() > 2
                && name.starts_with("on")
                && name.chars().all(|c| c.is_ascii_lowercase());
            if name == "title" || name == "style" || is_handler {
                problems.push(format!("{name}= attribute in <{}>", tag.name));
            }
        }
    }
    problems
}

/// What `index.html` needs for the accessibility rules, as problems.
fn structure_problems(html: &str) -> Vec<String> {
    let tags = start_tags(html);
    let count = |name: &str| tags.iter().filter(|tag| tag.name == name).count();
    let mut problems = Vec::new();
    let mut expect = |ok: bool, what: &str| {
        if !ok {
            problems.push(what.to_owned());
        }
    };
    expect(
        tags.iter()
            .any(|tag| tag.name == "html" && tag.attr("lang") == Some("en")),
        "html lang=\"en\"",
    );
    expect(count("h1") == 1, "exactly one <h1>");
    expect(count("header") == 1, "one <header>");
    expect(count("main") == 1, "one <main>");
    expect(count("caption") == 1, "one <caption>");
    expect(count("title") == 1, "one <title>");
    expect(
        tags.iter()
            .filter(|tag| tag.name == "th" && tag.attr("scope") == Some("col"))
            .count()
            == 5,
        "five scope=\"col\" headers",
    );
    problems
}

#[test]
fn the_window_files_pass_the_js_lint() {
    for (name, text) in ui_files() {
        assert_eq!(js_lint_hits(&text), Vec::<&str>::new(), "in ui/{name}");
    }
}

#[test]
fn js_lint_catches_every_listed_pattern_in_a_fixture() {
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
        // The event that carries dropped paths, subscribed without its name.
        "getCurrentWebview().onDragDropEvent(cb)",
        "getCurrentWebview().ONDRAGDROPEVENT(cb)",
        // Spacing and line breaks do not hide a pattern.
        "el.setAttribute( 'title', text)",
        "el.setAttribute ('title' , text)",
        "el.setAttribute(\n  'title',\n  text)",
        "el.setAttribute( \"title\", text)",
        "el.setAttribute(`title`, text)",
        "el.setAttribute( 'style', css)",
        "el['title'] = text",
        "el[ \"title\" ] = text",
        "el[`title`] = text",
        "el.title   = text",
        "el.title\n  = text",
        "el . title = text",
        "el.className\n= 'a b'",
        "new\n  Function('return 1')",
        "eval (code)",
    ] {
        assert_eq!(js_lint_hits(bad).len(), 1, "{bad}");
    }
    assert!(js_lint_hits("row.textContent = name; tbody.replaceChildren(row)").is_empty());
    // Safe ways of doing the same things are not flagged.
    assert!(
        js_lint_hits(
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
    // Only an `id` attribute counts: not `data-id`, not text, not a comment.
    assert_eq!(
        missing_ids("document.getElementById('x')", "<p data-id=\"x\"></p>"),
        vec!["x"]
    );
    assert_eq!(
        missing_ids(
            "document.getElementById('x')",
            "<!-- <p id=\"x\"> --><p>id=\"x\"</p>"
        ),
        vec!["x"]
    );
    assert_eq!(
        missing_ids(
            "document.getElementById('x')",
            "<p title=\" id=\"x\"\" data-x='id=\"x\"'>"
        ),
        vec!["x"]
    );
    // Any spelling of a real id attribute counts.
    assert!(
        missing_ids(
            "document.getElementById('x'); document.getElementById('y')",
            "<p class=\"a\" ID='x'></p><p id = \"y\"></p>"
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
    assert!(loads_the_module_script(&html));
}

/// Whether `html` has a `<script>` start tag that loads `app.js` as a
/// module, read as tokens: a comment or text that spells it does not count.
fn loads_the_module_script(html: &str) -> bool {
    start_tags(html).iter().any(|tag| {
        tag.name == "script"
            && tag.attr("src") == Some("app.js")
            && tag.attr("type") == Some("module")
    })
}

#[test]
fn the_module_script_tag_is_read_as_a_token_in_a_fixture() {
    assert!(loads_the_module_script(
        "<body><script type=\"module\" src=\"app.js\"></script></body>"
    ));
    assert!(loads_the_module_script(
        "<SCRIPT SRC='app.js' TYPE='module'></SCRIPT>"
    ));
    // Only inside a comment, or as text: no script is loaded.
    assert!(!loads_the_module_script(
        "<body><!-- <script type=\"module\" src=\"app.js\"></script> --></body>"
    ));
    assert!(!loads_the_module_script(
        "<p>&lt;script type=\"module\" src=\"app.js\"&gt;</p>"
    ));
    // Another file, or not a module.
    assert!(!loads_the_module_script(
        "<script type=\"module\" src=\"other.js\"></script>"
    ));
    assert!(!loads_the_module_script("<script src=\"app.js\"></script>"));
    assert!(!loads_the_module_script(
        "<script data-src=\"app.js\" type=\"module\"></script>"
    ));
}

// --- the static C runtime on Windows ---------------------------------------------

const WINDOWS_MSVC: &str = "x86_64-pc-windows-msvc";

/// Problems with the Cargo configuration that links the C runtime
/// statically into the Windows build, so that a clean Windows machine
/// without the Visual C++ redistributable can start the app. Read as TOML:
/// `target.x86_64-pc-windows-msvc.rustflags` must hold `-C` followed by
/// `target-feature=+crt-static`, or the joined `-Ctarget-feature=+crt-static`.
fn crt_static_problems(config_toml: &str) -> Vec<String> {
    let table = match config_toml.parse::<toml::Table>() {
        Ok(table) => table,
        Err(error) => return vec![format!("not TOML: {error}")],
    };
    let Some(target) = table
        .get("target")
        .and_then(toml::Value::as_table)
        .and_then(|targets| targets.get(WINDOWS_MSVC))
        .and_then(toml::Value::as_table)
    else {
        return vec![format!("no [target.{WINDOWS_MSVC}] table")];
    };
    let Some(flags) = target.get("rustflags").and_then(toml::Value::as_array) else {
        return vec!["no rustflags array".to_owned()];
    };
    let flags: Vec<&str> = flags.iter().filter_map(toml::Value::as_str).collect();
    let static_crt = flags.contains(&"-Ctarget-feature=+crt-static")
        || flags
            .windows(2)
            .any(|pair| pair == ["-C", "target-feature=+crt-static"]);
    if static_crt {
        Vec::new()
    } else {
        vec![format!(
            "rustflags {flags:?} do not link the CRT statically"
        )]
    }
}

fn cargo_config() -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../.cargo/config.toml");
    fs::read_to_string(path).unwrap()
}

#[test]
fn the_windows_build_links_the_c_runtime_statically() {
    assert_eq!(crt_static_problems(&cargo_config()), Vec::<String>::new());
}

#[test]
fn a_cargo_config_with_the_static_runtime_in_either_spelling_passes_a_fixture() {
    for flags in [
        r#"["-C", "target-feature=+crt-static"]"#,
        r#"["-Ctarget-feature=+crt-static"]"#,
        r#"["-C", "link-arg=/X", "-C", "target-feature=+crt-static"]"#,
    ] {
        let config = format!("[target.{WINDOWS_MSVC}]\nrustflags = {flags}\n");
        assert_eq!(
            crt_static_problems(&config),
            Vec::<String>::new(),
            "{config}"
        );
    }
}

#[test]
fn a_cargo_config_without_the_static_runtime_is_caught_in_a_fixture() {
    let configs = [
        // Only in a comment.
        format!(
            "[target.{WINDOWS_MSVC}]\n# rustflags = [\"-C\", \"target-feature=+crt-static\"]\n"
        ),
        format!("[target.{WINDOWS_MSVC}]\nrustflags = [] # -C target-feature=+crt-static\n"),
        // For another target.
        "[target.x86_64-pc-windows-gnu]\nrustflags = [\"-C\", \"target-feature=+crt-static\"]\n"
            .to_owned(),
        // The flag without its `-C`, the pair the wrong way round, or another feature.
        format!("[target.{WINDOWS_MSVC}]\nrustflags = [\"target-feature=+crt-static\"]\n"),
        format!("[target.{WINDOWS_MSVC}]\nrustflags = [\"target-feature=+crt-static\", \"-C\"]\n"),
        format!("[target.{WINDOWS_MSVC}]\nrustflags = [\"-C\", \"target-feature=-crt-static\"]\n"),
        // A string where an array belongs, and no rustflags at all.
        format!("[target.{WINDOWS_MSVC}]\nrustflags = \"-C target-feature=+crt-static\"\n"),
        format!("[target.{WINDOWS_MSVC}]\nlinker = \"link.exe\"\n"),
        // Not TOML.
        format!("[target.{WINDOWS_MSVC}\nrustflags = [\"-C\", \"target-feature=+crt-static\"]\n"),
    ];
    for config in &configs {
        assert_eq!(crt_static_problems(config).len(), 1, "{config}");
    }
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
    // A `>` inside a quoted value does not end the tag.
    assert_eq!(
        html_attribute_problems("<button data-x=\"a>b\" title=\"tip\">").len(),
        1
    );
    assert_eq!(
        html_attribute_problems("<button data-x='a>b' onclick='go()'>").len(),
        1
    );
    // The text ` src=` inside another attribute's value is not a `src`.
    assert_eq!(
        html_attribute_problems("<script data-n=\" src=\">alert(1)</script>").len(),
        1
    );
    assert_eq!(
        html_attribute_problems("<script data-src=\"app.js\"></script>").len(),
        1
    );
    assert_eq!(html_attribute_problems("<SCRIPT>run()</SCRIPT>").len(), 1);
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
fn an_attribute_spelled_in_a_comment_or_in_text_is_not_an_attribute() {
    assert!(
        html_attribute_problems(
            "<!-- <p title=\"x\" onclick=\"y\"> --><p>a title=\"x\" in text</p>\
             <!-- <script>x</script> -->"
        )
        .is_empty()
    );
    assert!(
        html_attribute_problems(
            "<p data-title=\"x\" data-style=\"y\" data-onclick=\"z\" class=\"title\">"
        )
        .is_empty()
    );
    // A script's own text is not markup: the `<p title>` inside it is a string.
    assert!(
        html_attribute_problems("<script src=\"a.js\">s = '<p title=\"x\">';</script>").is_empty()
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

/// A page with exactly the structure the rules need, with `title`, `heading`
/// and `extra` to vary.
fn page(title: &str, heading: &str, extra: &str) -> String {
    format!(
        "<html lang=\"en\"><title>{title}</title>{heading}{extra}<header></header>\
         <main></main><caption>c</caption>\
         <th scope=\"col\"><th scope=\"col\"><th scope=\"col\">\
         <th scope=\"col\"><th scope=\"col\">"
    )
}

#[test]
fn headings_in_comments_scripts_and_titles_are_not_counted() {
    let h1 = "<h1>a</h1>";
    assert_eq!(structure_problems(&page("x", h1, "")), Vec::<String>::new());
    // A second `<h1` that is only comment, script, style or title text.
    for extra in [
        "<!-- <h1>b</h1> -->",
        "<script src=\"a.js\">document.write('<h1>b</h1>')</script>",
        "<style>/* <h1> */</style>",
    ] {
        assert_eq!(
            structure_problems(&page("x", h1, extra)),
            Vec::<String>::new(),
            "{extra}"
        );
    }
    assert_eq!(
        structure_problems(&page("a <h1> b", h1, "")),
        Vec::<String>::new()
    );
    // A real second h1 still counts.
    assert_eq!(
        structure_problems(&page("x", h1, "<h1>b</h1>")),
        vec!["exactly one <h1>"]
    );
    // And the only h1 inside a comment is no h1.
    assert_eq!(
        structure_problems(&page("x", "<!-- <h1>a</h1> -->", "")),
        vec!["exactly one <h1>"]
    );
}

#[test]
fn only_a_real_html_lang_and_real_column_headers_satisfy_the_structure() {
    let good = page("x", "<h1>a</h1>", "");
    // `lang` on another element, or in a comment, is not on <html>.
    let other = good.replace("<html lang=\"en\">", "<html><p lang=\"en\">");
    assert_eq!(structure_problems(&other), vec!["html lang=\"en\""]);
    let commented = good.replace("<html lang=\"en\">", "<!-- <html lang=\"en\"> --><html>");
    assert_eq!(structure_problems(&commented), vec!["html lang=\"en\""]);
    // A scope="col" that is only comment text is not a header.
    let four = good.replacen("<th scope=\"col\">", "", 1) + "<!-- <th scope=\"col\"> -->";
    assert_eq!(
        structure_problems(&four),
        vec!["five scope=\"col\" headers"]
    );
    // Another value of scope is not "col".
    let rows = good.replacen("scope=\"col\"", "scope=\"row\"", 1);
    assert_eq!(
        structure_problems(&rows),
        vec!["five scope=\"col\" headers"]
    );
}

// --- the run settings (Task 11) ---------------------------------------------

/// The id of Single image's compare divider: a range input of its own, which
/// the Quality checks below leave out (it is checked by
/// [`single_screen_problems`]).
const DIVIDER_ID: &str = "divider";

/// Problems with the settings controls the accessibility rules need, read as
/// tokens: the "Compression" choice is a `<fieldset>` whose `<legend>` says
/// "Compression" and that holds two radio inputs, and the quality range and
/// number inputs each have a `<label for>` and the bounds min 0, max 100,
/// step 1. The page has one range input besides the compare divider (id
/// [`DIVIDER_ID`]), and that one is Quality's.
fn settings_problems(html: &str) -> Vec<String> {
    let pieces = pieces(html);
    let mut problems = Vec::new();

    // Each fieldset as (its legend's text, the radio inputs inside it).
    let mut fieldsets: Vec<(String, usize)> = Vec::new();
    let mut open: Vec<(String, usize)> = Vec::new();
    let mut in_legend = 0usize;
    for piece in &pieces {
        match piece {
            Piece::Start(tag) if tag.name == "fieldset" => open.push((String::new(), 0)),
            Piece::End(name) if name == "fieldset" => {
                if let Some(closed) = open.pop() {
                    fieldsets.push(closed);
                }
            }
            Piece::Start(tag) if tag.name == "legend" && !open.is_empty() => in_legend += 1,
            Piece::End(name) if name == "legend" => in_legend = in_legend.saturating_sub(1),
            Piece::Text(text) if in_legend > 0 => {
                if let Some(current) = open.last_mut() {
                    current.0.push_str(text);
                }
            }
            Piece::Start(tag)
                if tag.name == "input"
                    && tag
                        .attr("type")
                        .is_some_and(|kind| kind.eq_ignore_ascii_case("radio")) =>
            {
                if let Some(current) = open.last_mut() {
                    current.1 += 1;
                }
            }
            _ => {}
        }
    }
    fieldsets.extend(open);
    match fieldsets
        .iter()
        .find(|(legend, _)| one_line(legend) == "Compression")
    {
        None => problems.push("a fieldset with the legend \"Compression\"".to_owned()),
        Some((_, radios)) if *radios != 2 => {
            problems.push("two radio inputs in the Compression fieldset".to_owned());
        }
        Some(_) => {}
    }

    let tags: Vec<&StartTag> = pieces
        .iter()
        .filter_map(|piece| match piece {
            Piece::Start(tag) => Some(tag),
            _ => None,
        })
        .collect();
    let labelled: HashSet<&str> = tags
        .iter()
        .filter(|tag| tag.name == "label")
        .filter_map(|tag| tag.attr("for"))
        .collect();
    for kind in ["range", "number"] {
        let inputs: Vec<&&StartTag> = tags
            .iter()
            .filter(|tag| {
                tag.name == "input"
                    && tag.attr("id") != Some(DIVIDER_ID)
                    && tag
                        .attr("type")
                        .is_some_and(|value| value.eq_ignore_ascii_case(kind))
            })
            .collect();
        let [input] = inputs.as_slice() else {
            problems.push(format!("one {kind} input"));
            continue;
        };
        if !input.attr("id").is_some_and(|id| labelled.contains(id)) {
            problems.push(format!("a <label for> on the {kind} input"));
        }
        for (name, value) in [("min", "0"), ("max", "100"), ("step", "1")] {
            if input.attr(name) != Some(value) {
                problems.push(format!("{name}=\"{value}\" on the {kind} input"));
            }
        }
    }
    problems
}

/// Settings markup with everything [`settings_problems`] asks for.
const SETTINGS_FIXTURE: &str = "<fieldset><legend>Compression</legend>\
     <label><input type=\"radio\" name=\"m\"> Lossless</label>\
     <label><input type=\"radio\" name=\"m\"> Lossy</label></fieldset>\
     <label for=\"r\">Quality</label>\
     <input id=\"r\" type=\"range\" min=\"0\" max=\"100\" step=\"1\">\
     <label for=\"n\">Quality number</label>\
     <input id=\"n\" type=\"number\" min=\"0\" max=\"100\" step=\"1\">";

#[test]
fn the_settings_have_a_compression_fieldset_and_two_labelled_bounded_quality_inputs() {
    assert_eq!(settings_problems(&index_html()), Vec::<String>::new());
}

#[test]
fn a_settings_fixture_with_everything_passes_in_any_spelling() {
    assert_eq!(settings_problems(SETTINGS_FIXTURE), Vec::<String>::new());
    // Upper-case tags, attribute names and type values, other quotes, a
    // wrapped legend: the same markup to a browser.
    let spelled = SETTINGS_FIXTURE
        .replace(
            "<legend>Compression</legend>",
            "<LEGEND>\n  Compression\n</LEGEND>",
        )
        .replace("type=\"radio\"", "TYPE='RADIO'")
        .replace(
            "<input id=\"r\" type=\"range\"",
            "<INPUT ID='r' TYPE='Range'",
        );
    assert_eq!(settings_problems(&spelled), Vec::<String>::new());
}

#[test]
fn a_settings_fixture_without_a_legend_a_label_or_a_bound_is_flagged() {
    let cases: [(String, &str); 10] = [
        (
            SETTINGS_FIXTURE.replace("<legend>Compression</legend>", ""),
            "a fieldset with the legend \"Compression\"",
        ),
        (
            SETTINGS_FIXTURE.replace(
                "<legend>Compression</legend>",
                "<!-- <legend>Compression</legend> -->",
            ),
            "a fieldset with the legend \"Compression\"",
        ),
        (
            SETTINGS_FIXTURE.replace("<legend>Compression</legend>", "<legend>Mode</legend>"),
            "a fieldset with the legend \"Compression\"",
        ),
        (
            SETTINGS_FIXTURE.replacen("type=\"radio\"", "type=\"checkbox\"", 1),
            "two radio inputs in the Compression fieldset",
        ),
        (
            SETTINGS_FIXTURE.replace("<label for=\"r\">Quality</label>", ""),
            "a <label for> on the range input",
        ),
        (
            SETTINGS_FIXTURE.replace("<label for=\"n\">", "<label for=\"x\">"),
            "a <label for> on the number input",
        ),
        (
            SETTINGS_FIXTURE.replace("<label for=\"n\">", "<label data-for=\"n\">"),
            "a <label for> on the number input",
        ),
        (
            SETTINGS_FIXTURE.replace(
                "type=\"number\" min=\"0\" max=\"100\"",
                "type=\"number\" min=\"0\" max=\"101\"",
            ),
            "max=\"100\" on the number input",
        ),
        (
            SETTINGS_FIXTURE.replace(
                "type=\"range\" min=\"0\" max=\"100\" step=\"1\"",
                "type=\"range\" min=\"0\" max=\"100\"",
            ),
            "step=\"1\" on the range input",
        ),
        (
            SETTINGS_FIXTURE.replace("type=\"range\" min=\"0\"", "type=\"range\" min=\"1\""),
            "min=\"0\" on the range input",
        ),
    ];
    for (html, problem) in cases {
        assert_eq!(settings_problems(&html), vec![problem.to_owned()], "{html}");
    }
    // A quality input that is missing altogether.
    let no_number = SETTINGS_FIXTURE.replace(
        "<input id=\"n\" type=\"number\" min=\"0\" max=\"100\" step=\"1\">",
        "",
    );
    assert_eq!(
        settings_problems(&no_number),
        vec!["one number input".to_owned()]
    );
}

// --- the Save to buttons and the quality row (Task 13) ---------------------

/// Problems with the Save to buttons and the quality row, read as tokens:
/// `chooseOutput` says "Choose folder"; `clearOutput` says "Clear" and has the
/// `aria-label` "Clear output folder" (the visible text is inside the
/// accessible name); and an element with the id `qualityRow` holds the range
/// input (every one but the compare divider, id [`DIVIDER_ID`]), the number
/// input and the `qualityError` line, so one `hidden` on the row hides the
/// whole Quality row.
fn output_and_quality_problems(html: &str) -> Vec<String> {
    const VOID: &[&str] = &["input", "br", "img", "hr", "meta", "link"];
    const ROW: &str = "qualityRow";
    let mut problems = Vec::new();

    // Each button with an id, as (id, aria-label, text).
    let mut buttons: Vec<(String, Option<String>, String)> = Vec::new();
    let mut in_button = false;
    // The open elements as (name, id), innermost last.
    let mut open: Vec<(String, Option<String>)> = Vec::new();
    let mut row_seen = false;
    // Whether each range input, number input and `qualityError` is inside the row.
    let (mut ranges, mut numbers, mut errors): (Vec<bool>, Vec<bool>, Vec<bool>) =
        (Vec::new(), Vec::new(), Vec::new());
    for piece in pieces(html) {
        match piece {
            Piece::Start(tag) => {
                let inside = open.iter().any(|(_, id)| id.as_deref() == Some(ROW));
                let id = tag.attr("id");
                row_seen |= id == Some(ROW);
                let kind = tag.attr("type").map(str::to_ascii_lowercase);
                if tag.name == "input" && kind.as_deref() == Some("range") {
                    // The compare divider is Single image's own range input.
                    if id != Some(DIVIDER_ID) {
                        ranges.push(inside);
                    }
                } else if tag.name == "input" && kind.as_deref() == Some("number") {
                    numbers.push(inside);
                } else if id == Some("qualityError") {
                    errors.push(inside);
                }
                if tag.name == "button"
                    && let Some(id) = id
                {
                    buttons.push((
                        id.to_owned(),
                        tag.attr("aria-label").map(str::to_owned),
                        String::new(),
                    ));
                    in_button = true;
                }
                if !VOID.contains(&tag.name.as_str()) {
                    open.push((tag.name.clone(), id.map(str::to_owned)));
                }
            }
            Piece::End(name) => {
                if name == "button" {
                    in_button = false;
                }
                // An end tag closes the nearest open element of its name
                // and whatever was left open inside it.
                if let Some(at) = open.iter().rposition(|(open_name, _)| *open_name == name) {
                    open.truncate(at);
                }
            }
            Piece::Text(text) if in_button => {
                if let Some(last) = buttons.last_mut() {
                    last.2.push_str(&text);
                }
            }
            _ => {}
        }
    }

    for (id, text, aria) in [
        ("chooseOutput", "Choose folder", None),
        ("clearOutput", "Clear", Some("Clear output folder")),
    ] {
        let Some((_, label, content)) = buttons.iter().find(|(button, _, _)| button == id) else {
            problems.push(format!("a button with the id {id}"));
            continue;
        };
        if one_line(content) != text {
            problems.push(format!("{id} text \"{text}\""));
        }
        if let Some(aria) = aria
            && label.as_deref() != Some(aria)
        {
            problems.push(format!("aria-label=\"{aria}\" on {id}"));
        }
    }
    if !row_seen {
        problems.push(format!("an element with the id {ROW}"));
    }
    for (what, found) in [
        ("the range input", ranges),
        ("the number input", numbers),
        ("qualityError", errors),
    ] {
        if found.is_empty() || found.contains(&false) {
            problems.push(format!("{what} inside {ROW}"));
        }
    }
    problems
}

/// Markup with everything [`output_and_quality_problems`] asks for.
const OUTPUT_FIXTURE: &str = "<div id=\"qualityRow\">\
     <label for=\"r\">Quality</label><input id=\"r\" type=\"range\">\
     <label for=\"n\">Quality number</label><input id=\"n\" type=\"number\">\
     <p id=\"qualityError\" hidden>Enter a whole number.</p></div>\
     <div><button id=\"chooseOutput\" type=\"button\">Choose folder</button>\
     <button id=\"clearOutput\" type=\"button\" aria-label=\"Clear output folder\" hidden>Clear</button></div>";

#[test]
fn the_output_buttons_and_the_quality_row_match_the_rules() {
    assert_eq!(
        output_and_quality_problems(&index_html()),
        Vec::<String>::new()
    );
}

#[test]
fn an_output_and_quality_fixture_with_everything_passes_in_any_spelling() {
    assert_eq!(
        output_and_quality_problems(OUTPUT_FIXTURE),
        Vec::<String>::new()
    );
    // Upper-case tags and attribute names, other quotes, wrapped button text,
    // and an input nested deeper inside the row: the same markup to a browser.
    let spelled = OUTPUT_FIXTURE
        .replace("<div id=\"qualityRow\">", "<DIV ID='qualityRow'>")
        .replace(
            "<input id=\"n\" type=\"number\">",
            "<span><INPUT ID='n' TYPE='Number'></span>",
        )
        .replace(
            "<button id=\"chooseOutput\" type=\"button\">Choose folder</button>",
            "<BUTTON ID='chooseOutput'>\n  Choose\n  folder\n</BUTTON>",
        )
        .replace("aria-label=", "ARIA-LABEL=");
    assert_eq!(output_and_quality_problems(&spelled), Vec::<String>::new());
}

#[test]
fn an_output_and_quality_fixture_with_old_texts_no_label_or_a_misplaced_input_is_flagged() {
    let moved_out = |input: &str| OUTPUT_FIXTURE.replace(input, "") + input;
    let number = "<input id=\"n\" type=\"number\">";
    let range = "<input id=\"r\" type=\"range\">";
    let error = "<p id=\"qualityError\" hidden>Enter a whole number.</p>";
    let cases: Vec<(String, Vec<&str>)> = vec![
        (
            OUTPUT_FIXTURE.replace(">Clear</button>", ">Next to the originals</button>"),
            vec!["clearOutput text \"Clear\""],
        ),
        (
            OUTPUT_FIXTURE.replace(">Choose folder<", ">Choose output folder<"),
            vec!["chooseOutput text \"Choose folder\""],
        ),
        (
            OUTPUT_FIXTURE.replace(" aria-label=\"Clear output folder\"", ""),
            vec!["aria-label=\"Clear output folder\" on clearOutput"],
        ),
        (
            OUTPUT_FIXTURE.replace("aria-label=\"Clear output folder\"", "aria-label=\"Clear\""),
            vec!["aria-label=\"Clear output folder\" on clearOutput"],
        ),
        (
            OUTPUT_FIXTURE.replace("<button id=\"clearOutput\"", "<div id=\"clearOutput\""),
            vec!["a button with the id clearOutput"],
        ),
        (
            moved_out(number),
            vec!["the number input inside qualityRow"],
        ),
        (moved_out(range), vec!["the range input inside qualityRow"]),
        (moved_out(error), vec!["qualityError inside qualityRow"]),
        (
            OUTPUT_FIXTURE.replace("<div id=\"qualityRow\">", "<div>"),
            vec![
                "an element with the id qualityRow",
                "the range input inside qualityRow",
                "the number input inside qualityRow",
                "qualityError inside qualityRow",
            ],
        ),
    ];
    for (html, problems) in cases {
        let expected: Vec<String> = problems.into_iter().map(str::to_owned).collect();
        assert_eq!(output_and_quality_problems(&html), expected, "{html}");
    }
}

// --- the compare divider is the page's second range input (Task 9) ----------

/// The Single image compare divider: a range input that is not Quality's.
const DIVIDER: &str = "<input id=\"divider\" type=\"range\" min=\"0\" max=\"100\" step=\"1\">";

#[test]
fn a_settings_fixture_with_the_divider_still_passes() {
    for html in [
        format!("{SETTINGS_FIXTURE}{DIVIDER}"),
        format!("{DIVIDER}{SETTINGS_FIXTURE}"),
    ] {
        assert_eq!(settings_problems(&html), Vec::<String>::new(), "{html}");
    }
}

#[test]
fn an_output_fixture_with_the_divider_outside_the_row_still_passes() {
    assert_eq!(
        output_and_quality_problems(&format!("{OUTPUT_FIXTURE}{DIVIDER}")),
        Vec::<String>::new()
    );
}

#[test]
fn a_second_range_input_that_is_not_the_divider_is_still_flagged() {
    let other = "<input id=\"other\" type=\"range\" min=\"0\" max=\"100\" step=\"1\">";
    assert_eq!(
        settings_problems(&format!("{SETTINGS_FIXTURE}{other}")),
        vec!["one range input".to_owned()]
    );
    assert_eq!(
        output_and_quality_problems(&format!("{OUTPUT_FIXTURE}{other}")),
        vec!["the range input inside qualityRow".to_owned()]
    );
    // `data-id` or a comment is not the divider's id.
    let spoofed = "<input data-id=\"divider\" type=\"range\">";
    assert_eq!(
        settings_problems(&format!("{SETTINGS_FIXTURE}{spoofed}")),
        vec!["one range input".to_owned()]
    );
}

// --- the Single image screen (Task 9) ----------------------------------------

/// Each `<fieldset>` of `html`, as (its legend's text on one line, the
/// `value`s of the radio inputs inside it, in order). Read as tokens, so a
/// legend or an input in a comment does not count.
fn fieldsets_with_radios(html: &str) -> Vec<(String, Vec<String>)> {
    let mut done = Vec::new();
    let mut open: Vec<(String, Vec<String>)> = Vec::new();
    let mut in_legend = 0usize;
    for piece in pieces(html) {
        match piece {
            Piece::Start(tag) if tag.name == "fieldset" => open.push((String::new(), Vec::new())),
            Piece::End(name) if name == "fieldset" => {
                if let Some(closed) = open.pop() {
                    done.push(closed);
                }
            }
            Piece::Start(tag) if tag.name == "legend" && !open.is_empty() => in_legend += 1,
            Piece::End(name) if name == "legend" => in_legend = in_legend.saturating_sub(1),
            Piece::Text(text) if in_legend > 0 => {
                if let Some(current) = open.last_mut() {
                    current.0.push_str(&text);
                }
            }
            Piece::Start(tag)
                if tag.name == "input"
                    && tag
                        .attr("type")
                        .is_some_and(|kind| kind.eq_ignore_ascii_case("radio")) =>
            {
                if let Some(current) = open.last_mut() {
                    current
                        .1
                        .push(tag.attr("value").unwrap_or_default().to_owned());
                }
            }
            _ => {}
        }
    }
    done.extend(open);
    done.into_iter()
        .map(|(legend, radios)| (one_line(&legend), radios))
        .collect()
}

/// The text of each `<label for>` of `html`, as (the `for` value, the text on
/// one line).
fn label_texts(html: &str) -> Vec<(String, String)> {
    let mut labels: Vec<(String, String)> = Vec::new();
    let mut in_label = false;
    for piece in pieces(html) {
        match piece {
            Piece::Start(tag) if tag.name == "label" => {
                in_label = true;
                labels.push((
                    tag.attr("for").unwrap_or_default().to_owned(),
                    String::new(),
                ));
            }
            Piece::End(name) if name == "label" => in_label = false,
            Piece::Text(text) if in_label => {
                if let Some(last) = labels.last_mut() {
                    last.1.push_str(&text);
                }
            }
            _ => {}
        }
    }
    labels
        .into_iter()
        .map(|(target, text)| (target, one_line(&text)))
        .collect()
}

/// What the Single image screen needs for the accessibility rules, read as
/// tokens: the Single image / Batch switch and the layout switch are each a
/// `<fieldset>` with its legend ("Work on", "Settings") and exactly its two
/// radios; the picture area is `role="region"` with `aria-label="Picture"`
/// and `tabindex="0"`; and the compare divider is a range input (id
/// `divider`, 0 to 100, step 1) named by a `<label for>` and sitting inside
/// that region.
fn single_screen_problems(html: &str) -> Vec<String> {
    let mut problems = Vec::new();
    let fieldsets = fieldsets_with_radios(html);
    for (legend, radios, what) in [
        (
            "Work on",
            ["single", "batch"],
            "a fieldset with the legend \"Work on\" and the radios single, batch",
        ),
        (
            "Settings",
            ["under", "beside"],
            "a fieldset with the legend \"Settings\" and the radios under, beside",
        ),
    ] {
        let found = fieldsets
            .iter()
            .any(|(text, values)| text == legend && values.as_slice() == radios);
        if !found {
            problems.push(what.to_owned());
        }
    }

    // The region, and whether the divider is inside it.
    let mut region: Option<StartTag> = None;
    let mut depth = 0usize;
    let mut divider: Option<(StartTag, bool)> = None;
    for piece in pieces(html) {
        match piece {
            Piece::Start(tag) => {
                let void = VOID_ELEMENTS.contains(&tag.name.as_str());
                let is_region = region.is_none()
                    && tag.attr("role") == Some("region")
                    && tag.attr("aria-label") == Some("Picture");
                if tag.name == "input" && tag.attr("id") == Some("divider") {
                    divider = Some((tag, depth > 0));
                    continue;
                }
                if is_region && !void {
                    region = Some(tag);
                    depth = 1;
                } else if depth > 0 && !void {
                    depth += 1;
                }
            }
            Piece::End(_) if depth > 0 => depth -= 1,
            _ => {}
        }
    }
    match &region {
        None => {
            problems.push("an element with role=\"region\" and aria-label=\"Picture\"".to_owned())
        }
        Some(tag) if tag.attr("tabindex") != Some("0") => {
            problems.push("tabindex=\"0\" on the Picture region".to_owned());
        }
        Some(_) => {}
    }
    let labelled = label_texts(html)
        .iter()
        .any(|(target, text)| target == "divider" && text == "Compare original and compressed");
    if !labelled {
        problems.push(
            "a <label for=\"divider\"> saying \"Compare original and compressed\"".to_owned(),
        );
    }
    let range_ok = divider.as_ref().is_some_and(|(tag, _)| {
        tag.attr("type")
            .is_some_and(|kind| kind.eq_ignore_ascii_case("range"))
            && tag.attr("min") == Some("0")
            && tag.attr("max") == Some("100")
            && tag.attr("step") == Some("1")
    });
    if !range_ok {
        problems.push("a range input with the id divider, min 0, max 100, step 1".to_owned());
    }
    if !divider.as_ref().is_some_and(|(_, inside)| *inside) {
        problems.push("the divider inside the Picture region".to_owned());
    }
    problems
}

/// Markup with everything [`single_screen_problems`] asks for.
const SINGLE_FIXTURE: &str = "<header><fieldset><legend>Work on</legend>\
     <label><input type=\"radio\" name=\"view\" value=\"single\" checked> Single image</label>\
     <label><input type=\"radio\" name=\"view\" value=\"batch\"> Batch</label></fieldset></header>\
     <fieldset><legend>Settings</legend>\
     <label><input type=\"radio\" name=\"layout\" value=\"under\" checked> Settings under</label>\
     <label><input type=\"radio\" name=\"layout\" value=\"beside\"> Settings beside</label></fieldset>\
     <div id=\"picture\" role=\"region\" aria-label=\"Picture\" tabindex=\"0\">\
     <div><img alt=\"Original\"></div>\
     <label for=\"divider\">Compare original and compressed</label>\
     <input id=\"divider\" type=\"range\" min=\"0\" max=\"100\" step=\"1\" value=\"50\"></div>";

#[test]
fn index_html_has_both_switches_as_fieldsets_with_legends_the_region_and_the_labelled_divider() {
    assert_eq!(single_screen_problems(&index_html()), Vec::<String>::new());
}

#[test]
fn a_single_screen_fixture_with_everything_passes_in_any_spelling() {
    assert_eq!(single_screen_problems(SINGLE_FIXTURE), Vec::<String>::new());
    let spelled = SINGLE_FIXTURE
        .replace(
            "<legend>Work on</legend>",
            "<LEGEND>\n  Work\n  on\n</LEGEND>",
        )
        .replace("type=\"radio\"", "TYPE='Radio'")
        .replace(
            "<label for=\"divider\">Compare original and compressed</label>",
            "<LABEL FOR='divider'>\n Compare original\n and compressed </LABEL>",
        )
        .replace("role=\"region\"", "ROLE='region'");
    assert_eq!(single_screen_problems(&spelled), Vec::<String>::new());
}

#[test]
fn a_single_screen_fixture_without_a_divider_label_a_legend_or_the_region_is_flagged() {
    const VIEWS: &str = "a fieldset with the legend \"Work on\" and the radios single, batch";
    const LAYOUTS: &str = "a fieldset with the legend \"Settings\" and the radios under, beside";
    const REGION: &str = "an element with role=\"region\" and aria-label=\"Picture\"";
    const TABINDEX: &str = "tabindex=\"0\" on the Picture region";
    const LABEL: &str = "a <label for=\"divider\"> saying \"Compare original and compressed\"";
    const RANGE: &str = "a range input with the id divider, min 0, max 100, step 1";
    const INSIDE: &str = "the divider inside the Picture region";
    let label = "<label for=\"divider\">Compare original and compressed</label>";
    let divider =
        "<input id=\"divider\" type=\"range\" min=\"0\" max=\"100\" step=\"1\" value=\"50\">";
    let cases: Vec<(String, Vec<&str>)> = vec![
        (SINGLE_FIXTURE.replace(label, ""), vec![LABEL]),
        (
            SINGLE_FIXTURE.replace(label, &format!("<!-- {label} -->")),
            vec![LABEL],
        ),
        (
            SINGLE_FIXTURE.replace(">Compare original and compressed<", ">Compare<"),
            vec![LABEL],
        ),
        (
            SINGLE_FIXTURE.replace("<label for=\"divider\">", "<label for=\"other\">"),
            vec![LABEL],
        ),
        (
            SINGLE_FIXTURE.replace("<legend>Work on</legend>", "<!-- <legend>Work on</legend> -->"),
            vec![VIEWS],
        ),
        (
            SINGLE_FIXTURE.replace(
                "Batch</label></fieldset>",
                "Batch</label><label><input type=\"radio\" name=\"view\" value=\"both\"></label></fieldset>",
            ),
            vec![VIEWS],
        ),
        (
            SINGLE_FIXTURE.replace("value=\"batch\"", "value=\"Batch\""),
            vec![VIEWS],
        ),
        (
            SINGLE_FIXTURE.replace("<legend>Settings</legend>", "<legend>Layout</legend>"),
            vec![LAYOUTS],
        ),
        (
            SINGLE_FIXTURE.replace(" tabindex=\"0\"", ""),
            vec![TABINDEX],
        ),
        (
            SINGLE_FIXTURE.replace("tabindex=\"0\"", "tabindex=\"-1\""),
            vec![TABINDEX],
        ),
        (
            SINGLE_FIXTURE.replace("aria-label=\"Picture\"", "aria-label=\"Image\""),
            vec![REGION, INSIDE],
        ),
        (
            SINGLE_FIXTURE.replace(divider, "") + "</div>" + divider,
            vec![INSIDE],
        ),
        (
            SINGLE_FIXTURE.replace(" step=\"1\" value=\"50\"", " value=\"50\""),
            vec![RANGE],
        ),
        (SINGLE_FIXTURE.replace(divider, ""), vec![RANGE, INSIDE]),
    ];
    for (html, problems) in cases {
        let expected: Vec<String> = problems.into_iter().map(str::to_owned).collect();
        assert_eq!(single_screen_problems(&html), expected, "{html}");
    }
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

/// The exceptions a permitted licence may carry (`X WITH Y`).
const PERMITTED_EXCEPTIONS: &[&str] = &["LLVM-exception"];

/// The terms of an SPDX expression that are not permitted. The expression is
/// split on `AND`, `OR` and parentheses. A term is permitted when it is in
/// [`PERMITTED_LICENCES`], or is `X WITH Y` with a permitted `X` and a `Y` in
/// [`PERMITTED_EXCEPTIONS`] (an exception only narrows a licence). A term with
/// two `WITH`s, or an exception we do not know, is refused.
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
            if PERMITTED_LICENCES.contains(&term.as_str()) {
                return false;
            }
            match term.split(" WITH ").collect::<Vec<_>>().as_slice() {
                [base, exception] => {
                    !(PERMITTED_LICENCES.contains(base) && PERMITTED_EXCEPTIONS.contains(exception))
                }
                _ => true,
            }
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
    let none = Vec::new();
    let files = match clarify.get("files").map(toml::Value::as_array) {
        None => &none,
        Some(Some(files)) => files,
        Some(None) => {
            problems.push(format!("{place}.files: not an array"));
            &none
        }
    };
    for file in files {
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

/// The text of the licence page as a reader sees it: text nodes only, so a
/// comment, a `<script>` or a `<style>` does not count.
fn page_text(html: &str) -> String {
    one_line(&text_outside(html, &["script", "style"]))
}

/// The IJG statement stands in the page's own words, outside the licence
/// texts (`<pre>` blocks).
fn has_ijg_statement_in_intro(html: &str) -> bool {
    has_ijg_sentence(&text_outside(html, &["script", "style", "pre"]))
}

/// The IJG licence text: its own wording, as it appears in libjpeg's README.
fn has_ijg_licence_text(html: &str) -> bool {
    page_text(html).contains("based in part on the work of the Independent JPEG Group")
}

/// libdeflate's MIT notice, as in `libdeflate/COPYING` inside libdeflate-sys.
fn has_libdeflate_notice(html: &str) -> bool {
    let text = page_text(html);
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
        // At most one WITH per term, and only a known exception.
        "Apache-2.0 WITH LLVM-exception WITH Classpath-exception-2.0",
        "Apache-2.0 WITH LLVM-exception WITH LLVM-exception",
        "MIT WITH Classpath-exception-2.0",
        "Apache-2.0 WITH",
        "MIT AND Apache-2.0 WITH LLVM-exception WITH GCC-exception-3.1",
    ] {
        let fixture = format!("[x.clarify]\nlicense = \"{expression}\"\n");
        assert!(!about_problems(&fixture).is_empty(), "{expression}");
    }
    for expression in [
        "(MIT OR Apache-2.0) AND Unicode-3.0",
        "Apache-2.0 WITH LLVM-exception",
        "IJG AND Zlib AND BSD-3-Clause",
        "MIT AND Apache-2.0 WITH LLVM-exception",
        "MIT WITH LLVM-exception",
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
fn clarify_files_that_is_not_an_array_is_a_problem_and_an_array_is_not() {
    for files in ["\"LICENSE\"", "{ path = \"a\", checksum = \"b\" }", "3"] {
        let fixture = format!("[x.clarify]\nlicense = \"MIT\"\nfiles = {files}\n");
        let problems = about_problems(&fixture);
        assert!(
            names(&problems, "x.clarify.files: not an array"),
            "{files}: {problems:?}"
        );
    }
    let empty = "[x.clarify]\nlicense = \"MIT\"\nfiles = []\n";
    assert_eq!(about_problems(empty), Vec::<String>::new());
    let none = "[x.clarify]\nlicense = \"MIT\"\n";
    assert_eq!(about_problems(none), Vec::<String>::new());
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
    // Text in a comment, a script or a style is not on the page.
    let in_comment = format!("<p>Intro</p><!-- {IJG_SENTENCE} --><pre>licence</pre>");
    assert!(!has_ijg_statement_in_intro(&in_comment));
    let in_script = format!("<p>Intro</p><script>var s = '{IJG_SENTENCE}';</script>");
    assert!(!has_ijg_statement_in_intro(&in_script));
    let in_style = format!("<p>Intro</p><style>/* {IJG_SENTENCE} */</style>");
    assert!(!has_ijg_statement_in_intro(&in_style));
    // A `<pre` in a comment does not hide the real intro.
    let pre_in_comment = format!("<!-- <pre> --><p>{IJG_SENTENCE}</p>");
    assert!(has_ijg_statement_in_intro(&pre_in_comment));
    // Text nodes are read as text: across inline tags, with entities decoded.
    assert!(has_ijg_statement_in_intro(
        "<p>This software is based in part on the work of the <b>Independent JPEG Group</b>.</p>"
    ));
    assert!(has_ijg_statement_in_intro(
        "<p>This software is based in part on the work of the Independent&nbsp;JPEG Group.</p>"
    ));
    // The licence's own wording, likewise, must be on the page.
    let wording = "based in part on the work of the Independent JPEG Group";
    assert!(!has_ijg_licence_text(&format!("<!-- {wording} -->")));
    assert!(!has_ijg_licence_text(&format!(
        "<script>'{wording}'</script>"
    )));
    assert!(has_ijg_licence_text(&format!("<pre>{wording}</pre>")));
    // libdeflate: the notice in a comment, a script or a style is not a notice.
    let notice = "Copyright 2016 Eric Biggers Copyright 2024 Google LLC";
    assert!(!has_libdeflate_notice(&format!("<!-- {notice} -->")));
    assert!(!has_libdeflate_notice(&format!(
        "<script>'{notice}'</script>"
    )));
    assert!(!has_libdeflate_notice(&format!(
        "<style>/* {notice} */</style>"
    )));
    assert!(has_libdeflate_notice(&format!("<pre>{notice}</pre>")));
    // libdeflate: one of the two holders is not the notice; neither is none.
    assert!(!has_libdeflate_notice(
        "<pre>Copyright 2016 Eric Biggers</pre>"
    ));
    assert!(!has_libdeflate_notice("<pre>MIT License</pre>"));
    assert!(has_libdeflate_notice(
        "<pre>Copyright 2016 Eric Biggers\nCopyright 2024 Google LLC</pre>"
    ));
}

// --- the licences of libwebp and quantizr ------------------------------------------

/// SHA-256 of `vendor/COPYING` in libwebp-sys 0.14.4 (libwebp's BSD-3-Clause).
const LIBWEBP_COPYING_SHA256: &str =
    "5aec868f669e384a22372a4e8a1a6cd7d44c64cd451f960ca69cc170d1e13acf";
/// SHA-256 of `vendor/PATENTS` in libwebp-sys 0.14.4 (Google's patent grant).
const LIBWEBP_PATENTS_SHA256: &str =
    "cc3273e0694ea5896145e0677699b53471b03ea43021ddc50e7923fbb9f5023c";
/// SHA-256 of `LICENSE` in quantizr 1.4.3 (MIT).
const QUANTIZR_LICENSE_SHA256: &str =
    "407b2d167f73ef78da1fcbfb6c2f1051f195b1984b9bfeeac0a39b34c198a7fe";

/// What `about.toml` must clarify, as `(crate, licence expression, file path,
/// file checksum, file licence)`. libwebp-sys declares only MIT but vendors
/// BSD-3-Clause C code; quantizr declares no licence in its manifest at all
/// (`license-file` only), so cargo-about cannot guess it.
const CLARIFICATIONS: &[(&str, &str, &str, &str, &str)] = &[
    (
        "libwebp-sys",
        "MIT AND BSD-3-Clause",
        "vendor/COPYING",
        LIBWEBP_COPYING_SHA256,
        "BSD-3-Clause",
    ),
    ("quantizr", "MIT", "LICENSE", QUANTIZR_LICENSE_SHA256, "MIT"),
];

/// What is missing or wrong in `about.toml`'s clarifications of libwebp-sys
/// and quantizr: the table, the licence expression, and the licence file with
/// its path, checksum and licence.
fn clarification_problems(about_toml: &str) -> Vec<String> {
    let table = match about_toml.parse::<toml::Table>() {
        Ok(table) => table,
        Err(error) => return vec![format!("about.toml does not parse: {error}")],
    };
    let mut problems = Vec::new();
    for (name, expression, path, checksum, file_licence) in CLARIFICATIONS {
        let Some(clarify) = table
            .get(*name)
            .and_then(toml::Value::as_table)
            .and_then(|krate| krate.get("clarify"))
            .and_then(toml::Value::as_table)
        else {
            problems.push(format!("{name}: no clarify table"));
            continue;
        };
        if clarify.get("license").and_then(toml::Value::as_str) != Some(*expression) {
            problems.push(format!("{name}: license is not {expression}"));
        }
        let listed = clarify
            .get("files")
            .and_then(toml::Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(toml::Value::as_table)
            .any(|file| {
                let field = |key: &str| file.get(key).and_then(toml::Value::as_str);
                field("path") == Some(*path)
                    && field("checksum") == Some(*checksum)
                    && field("license") == Some(*file_licence)
            });
        if !listed {
            problems.push(format!(
                "{name}: no file {path} with checksum {checksum} and licence {file_licence}"
            ));
        }
    }
    problems
}

/// libwebp's BSD-3-Clause text (its copyright line and the clause that names
/// Google), quantizr's MIT notice, Google's patent grant (its heading, the
/// definition and the scope sentence) and the checksum of the `PATENTS` file
/// in a comment beside it. Text in a comment, script or style does not count
/// as shown; the checksum counts only as a comment.
fn licence_page_problems(html: &str) -> Vec<String> {
    let text = page_text(html);
    let mut problems = Vec::new();
    for (what, needle) in [
        ("libwebp-sys in the list of users", "libwebp-sys"),
        (
            "libwebp's copyright line",
            "Copyright (c) 2010, Google Inc.",
        ),
        (
            "libwebp's BSD-3-Clause text",
            "Neither the name of Google nor the names of its contributors",
        ),
        ("quantizr in the list of users", "quantizr"),
        (
            "quantizr's copyright line",
            "Copyright (c) 2017 Sergey Alexandrovich",
        ),
        (
            "the patent grant heading",
            "Additional IP Rights Grant (Patents)",
        ),
        // Not the Apache-2.0 wording the page also holds: the grant's own words.
        (
            "the patent grant's definition",
            "implement the WebM codecs distributed by Google as part of the WebM Project.",
        ),
        (
            "the patent grant's scope",
            "necessarily infringed by these implementations of WebM.",
        ),
    ] {
        if !text.contains(needle) {
            problems.push(format!("the page does not show {what}"));
        }
    }
    if !comments(html)
        .iter()
        .any(|comment| comment.contains(LIBWEBP_PATENTS_SHA256))
    {
        problems.push("no comment gives the PATENTS checksum".to_owned());
    }
    problems
}

const LIBWEBP_FIXTURE: &str = "<li>libwebp-sys 0.14.4</li><pre>Copyright (c) 2010, Google Inc. \
    All rights reserved. Neither the name of Google nor the names of its contributors may be used</pre>";
const QUANTIZR_FIXTURE: &str =
    "<li>quantizr 1.4.3</li><pre>MIT License Copyright (c) 2017 Sergey Alexandrovich</pre>";
const PATENTS_TEXT_FIXTURE: &str = "<h3>libwebp: Additional IP Rights Grant (Patents)</h3><pre>\
    \"These implementations\" means the copyrightable works that implement the WebM codecs distributed \
    by Google as part of the WebM Project. ... necessarily infringed by these implementations of WebM. ...</pre>";
const PATENTS_SUM_FIXTURE: &str = "<!-- vendor/PATENTS of libwebp-sys 0.14.4, sha256 cc3273e0694ea5896145e0677699b53471b03ea43021ddc50e7923fbb9f5023c -->";

fn licence_fixture(parts: &[&str]) -> String {
    format!("<main>{}</main>", parts.concat())
}

fn full_licence_fixture() -> String {
    licence_fixture(&[
        LIBWEBP_FIXTURE,
        QUANTIZR_FIXTURE,
        PATENTS_SUM_FIXTURE,
        PATENTS_TEXT_FIXTURE,
    ])
}

/// An `about.toml` fixture with the clarification of libwebp-sys and/or
/// quantizr, each as the real file should have it.
fn about_fixture(libwebp: bool, quantizr: bool) -> String {
    let mut about = String::from("accepted = [\"MIT\", \"BSD-3-Clause\"]\n");
    for (include, (name, expression, path, checksum, licence)) in
        [libwebp, quantizr].into_iter().zip(CLARIFICATIONS)
    {
        if include {
            about.push_str(&format!(
                "[{name}.clarify]\nlicense = \"{expression}\"\n\
                 [[{name}.clarify.files]]\npath = \"{path}\"\nchecksum = \"{checksum}\"\nlicense = \"{licence}\"\n"
            ));
        }
    }
    about
}

#[test]
fn about_toml_clarifies_libwebp_sys_and_quantizr_with_checksums() {
    let about = repo_file("about.toml");
    assert_eq!(clarification_problems(&about), Vec::<String>::new());
    // Not vacuous: the parse saw both entries, and the policy is still sound.
    let table = about.parse::<toml::Table>().unwrap();
    assert_eq!(
        table["libwebp-sys"]["clarify"]["license"].as_str(),
        Some("MIT AND BSD-3-Clause")
    );
    assert_eq!(
        table["quantizr"]["clarify"]["license"].as_str(),
        Some("MIT")
    );
    assert_eq!(about_problems(&about), Vec::<String>::new());
}

#[test]
fn a_clarification_that_is_missing_or_wrong_is_caught_in_a_fixture() {
    // The pair is right: nothing to flag (so each case below fails on its own).
    assert_eq!(
        clarification_problems(&about_fixture(true, true)),
        Vec::<String>::new()
    );
    let no_quantizr = clarification_problems(&about_fixture(true, false));
    assert!(
        names(&no_quantizr, "quantizr: no clarify table"),
        "{no_quantizr:?}"
    );
    assert!(!names(&no_quantizr, "libwebp-sys"), "{no_quantizr:?}");
    let no_libwebp = clarification_problems(&about_fixture(false, true));
    assert!(
        names(&no_libwebp, "libwebp-sys: no clarify table"),
        "{no_libwebp:?}"
    );
    assert!(!names(&no_libwebp, "quantizr"), "{no_libwebp:?}");
    // A changed checksum is a changed file: flagged for that crate only.
    let wrong_sum = about_fixture(true, true).replace(QUANTIZR_LICENSE_SHA256, "00");
    let problems = clarification_problems(&wrong_sum);
    assert!(
        names(&problems, "quantizr: no file LICENSE"),
        "{problems:?}"
    );
    assert!(!names(&problems, "libwebp-sys"), "{problems:?}");
    // libwebp-sys left at MIT alone drops the BSD-3-Clause notice.
    let mit_only = about_fixture(true, true).replace("MIT AND BSD-3-Clause", "MIT");
    let problems = clarification_problems(&mit_only);
    assert!(
        names(
            &problems,
            "libwebp-sys: license is not MIT AND BSD-3-Clause"
        ),
        "{problems:?}"
    );
    // A file entry under another path is not the file.
    let wrong_path = about_fixture(true, true).replace("vendor/COPYING", "COPYING");
    let problems = clarification_problems(&wrong_path);
    assert!(
        names(&problems, "libwebp-sys: no file vendor/COPYING"),
        "{problems:?}"
    );
    // A file that does not parse says so, instead of reading as "nothing wrong".
    assert!(names(
        &clarification_problems("accepted = ["),
        "does not parse"
    ));
}

#[test]
fn the_licence_page_names_libwebp_quantizr_and_the_patents_grant() {
    let html = repo_file(LICENCE_FILE);
    assert_eq!(licence_page_problems(&html), Vec::<String>::new());
}

#[test]
fn a_licence_page_without_libwebp_quantizr_or_the_patents_grant_is_caught_in_a_fixture() {
    // The full fixture is right: nothing to flag (so each case below fails on its own).
    assert_eq!(
        licence_page_problems(&full_licence_fixture()),
        Vec::<String>::new()
    );
    let without = |parts: &[&str]| licence_page_problems(&licence_fixture(parts));
    let no_libwebp = without(&[QUANTIZR_FIXTURE, PATENTS_SUM_FIXTURE, PATENTS_TEXT_FIXTURE]);
    assert!(
        names(&no_libwebp, "libwebp's copyright line"),
        "{no_libwebp:?}"
    );
    assert!(
        names(&no_libwebp, "libwebp's BSD-3-Clause text"),
        "{no_libwebp:?}"
    );
    assert!(!names(&no_libwebp, "quantizr"), "{no_libwebp:?}");
    let no_quantizr = without(&[LIBWEBP_FIXTURE, PATENTS_SUM_FIXTURE, PATENTS_TEXT_FIXTURE]);
    assert!(
        names(&no_quantizr, "quantizr's copyright line"),
        "{no_quantizr:?}"
    );
    assert!(!names(&no_quantizr, "libwebp"), "{no_quantizr:?}");
    let no_patents = without(&[LIBWEBP_FIXTURE, QUANTIZR_FIXTURE, PATENTS_SUM_FIXTURE]);
    assert!(names(&no_patents, "the patent grant"), "{no_patents:?}");
    assert!(!names(&no_patents, "PATENTS checksum"), "{no_patents:?}");
    // The patent text without the checksum comment: the file cannot be re-checked.
    let no_sum = without(&[LIBWEBP_FIXTURE, QUANTIZR_FIXTURE, PATENTS_TEXT_FIXTURE]);
    assert_eq!(
        no_sum,
        vec!["no comment gives the PATENTS checksum".to_owned()]
    );
    // The checksum as page text is not a comment, and another file's checksum is not it.
    let sum_as_text = full_licence_fixture()
        .replace("<!-- ", "<p>")
        .replace(" -->", "</p>");
    assert_eq!(
        licence_page_problems(&sum_as_text),
        vec!["no comment gives the PATENTS checksum".to_owned()]
    );
    let other_sum = full_licence_fixture().replace(LIBWEBP_PATENTS_SHA256, LIBWEBP_COPYING_SHA256);
    assert_eq!(
        licence_page_problems(&other_sum),
        vec!["no comment gives the PATENTS checksum".to_owned()]
    );
    // A notice that stands only in a comment, a script or a style is not on the page.
    let in_comment = format!(
        "<!-- {LIBWEBP_FIXTURE} -->{QUANTIZR_FIXTURE}{PATENTS_SUM_FIXTURE}{PATENTS_TEXT_FIXTURE}"
    );
    assert!(names(
        &licence_page_problems(&in_comment),
        "libwebp's copyright line"
    ));
    let in_script = format!(
        "<script>'{PATENTS_TEXT_FIXTURE}'</script>{LIBWEBP_FIXTURE}{QUANTIZR_FIXTURE}{PATENTS_SUM_FIXTURE}"
    );
    assert!(names(
        &licence_page_problems(&in_script),
        "the patent grant"
    ));
}

// --- the worker sidecar, in the bundle build only ---------------------------------

/// The worker the bundle ships beside the app, relative to `src-tauri/`.
/// tauri-build copies it into the target folder on every build that sees
/// it, and stops when the file is missing: so it lives in a config file
/// that only the bundle build reads, and `cargo test` and `clippy` never do.
const SIDECAR: &str = "binaries/lunaepix-worker";

/// The command that builds the worker and stages it under `binaries/`
/// before Tauri bundles (`crates/xtask`).
const SIDECAR_BUILD: &str = "cargo run --locked -p lunaepix-xtask -- sidecar";

fn read_text(name: &str) -> String {
    fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join(name)).unwrap()
}

/// What is wrong with the bundle-only config: `bundle.externalBin` must be
/// exactly `[SIDECAR]`, and `build.beforeBuildCommand` exactly the string
/// [`SIDECAR_BUILD`].
fn bundle_config_problems(text: &str) -> Vec<String> {
    let config: Value = match serde_json::from_str(text) {
        Ok(config) => config,
        Err(error) => return vec![format!("not JSON: {error}")],
    };
    let mut problems = Vec::new();
    match config.pointer("/bundle/externalBin") {
        Some(Value::Array(list)) if list.len() == 1 && list[0] == SIDECAR => {}
        Some(other) => problems.push(format!(
            "bundle.externalBin: {other} is not [\"{SIDECAR}\"]"
        )),
        None => problems.push("bundle.externalBin: missing".to_owned()),
    }
    match config.pointer("/build/beforeBuildCommand") {
        Some(Value::String(command)) if command == SIDECAR_BUILD => {}
        Some(other) => problems.push(format!(
            "build.beforeBuildCommand: {other} is not {SIDECAR_BUILD:?}"
        )),
        None => problems.push("build.beforeBuildCommand: missing".to_owned()),
    }
    problems
}

/// What is wrong with a config every build reads: it may not name a sidecar,
/// or `cargo test` stops as soon as `src-tauri/binaries/` is empty.
fn main_config_problems(config: &Value) -> Vec<String> {
    match config.pointer("/bundle/externalBin") {
        None => Vec::new(),
        Some(found) => vec![format!(
            "bundle.externalBin in a config every build reads: {found}"
        )],
    }
}

/// The platform configs Tauri merges into every build on that platform
/// (`tauri.<platform>.conf.json`, `.json5`, `Tauri.<platform>.toml`).
fn platform_configs(dir: &Path) -> Vec<String> {
    let platforms = ["linux", "windows", "macos", "android", "ios"];
    let names = platforms.iter().flat_map(|platform| {
        [
            format!("tauri.{platform}.conf.json"),
            format!("tauri.{platform}.conf.json5"),
            format!("Tauri.{platform}.toml"),
        ]
    });
    names.filter(|name| dir.join(name).exists()).collect()
}

#[test]
fn the_bundle_config_adds_the_worker_sidecar_and_builds_it_first() {
    let text = read_text("tauri.bundle.conf.json");
    assert_eq!(bundle_config_problems(&text), Vec::<String>::new());
    // Nothing else: the rest comes from tauri.conf.json.
    let config: Value = serde_json::from_str(&text).unwrap();
    let mut keys: Vec<&str> = config
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    assert_eq!(keys, ["$schema", "build", "bundle"]);
}

#[test]
fn a_bundle_config_without_the_sidecar_or_its_build_is_caught_in_a_fixture() {
    let build = format!("\"build\": {{ \"beforeBuildCommand\": \"{SIDECAR_BUILD}\" }}");
    let bundle = format!("\"bundle\": {{ \"externalBin\": [\"{SIDECAR}\"] }}");
    let good = format!("{{ {build}, {bundle} }}");
    assert_eq!(bundle_config_problems(&good), Vec::<String>::new());
    let other_bin = "\"bundle\": { \"externalBin\": [\"binaries/lunaepix-app\"] }";
    let extra_bin = format!("\"bundle\": {{ \"externalBin\": [\"{SIDECAR}\", \"binaries/x\"] }}");
    let string_bin = format!("\"bundle\": {{ \"externalBin\": \"{SIDECAR}\" }}");
    let other_build = "\"build\": { \"beforeBuildCommand\": \"cargo build --release\" }";
    let object_build =
        format!("\"build\": {{ \"beforeBuildCommand\": {{ \"script\": \"{SIDECAR_BUILD}\" }} }}");
    for (fixture, needle) in [
        (format!("{{ {build} }}"), "externalBin: missing"),
        (format!("{{ {build}, {other_bin} }}"), "externalBin"),
        (format!("{{ {build}, {extra_bin} }}"), "externalBin"),
        (format!("{{ {build}, {string_bin} }}"), "externalBin"),
        (format!("{{ {bundle} }}"), "beforeBuildCommand: missing"),
        (
            format!("{{ {other_build}, {bundle} }}"),
            "beforeBuildCommand",
        ),
        (
            format!("{{ {object_build}, {bundle} }}"),
            "beforeBuildCommand",
        ),
        (format!("{{ {build}, {bundle}"), "not JSON"),
    ] {
        let problems = bundle_config_problems(&fixture);
        assert!(names(&problems, needle), "{fixture}: {problems:?}");
    }
}

#[test]
fn the_main_config_has_no_external_bin_and_there_are_no_platform_configs() {
    let config = read_json("tauri.conf.json");
    assert_eq!(main_config_problems(&config), Vec::<String>::new());
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    assert_eq!(platform_configs(dir), Vec::<String>::new());
}

#[test]
fn a_main_config_with_an_external_bin_is_caught_in_a_fixture() {
    for external_bin in [
        serde_json::json!([SIDECAR]),
        serde_json::json!([]),
        serde_json::json!(SIDECAR),
    ] {
        let fixture =
            serde_json::json!({ "bundle": { "active": true, "externalBin": external_bin } });
        let problems = main_config_problems(&fixture);
        assert!(names(&problems, "externalBin"), "{fixture}: {problems:?}");
    }
    // A platform config beside the main one is found.
    let dir = std::env::temp_dir().join(format!("lunaepix-config-test-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    assert_eq!(platform_configs(&dir), Vec::<String>::new());
    fs::write(dir.join("tauri.linux.conf.json"), "{}").unwrap();
    fs::write(dir.join("Tauri.windows.toml"), "").unwrap();
    let found = platform_configs(&dir);
    fs::remove_dir_all(&dir).unwrap();
    assert_eq!(found, ["tauri.linux.conf.json", "Tauri.windows.toml"]);
}

// ---------------------------------------------------------------------
// WebP is named in every text. The old wording ("PNG, JPG or SVG") must not
// come back, in the window or in the skip reasons the core sends to it.
// ---------------------------------------------------------------------

/// True when `text` lists the formats without WebP: the old skip reason and
/// the old filter wording, or the drop hint "PNG · JPG · SVG" that does not
/// go on with "· WebP". Whitespace (a line break inside the phrase included)
/// and letter case do not matter.
fn names_only_png_jpg_and_svg(text: &str) -> bool {
    let text = one_line(text).to_lowercase();
    if text.contains("png, jpg or svg") || text.contains("png, jpg and svg") {
        return true;
    }
    let hint = "png · jpg · svg";
    text.match_indices(hint)
        .any(|(at, _)| !text[at + hint.len()..].starts_with(" · webp"))
}

/// The places in an HTML page where the old wording shows to a reader or to a
/// screen reader: its text nodes (comments are not text) and every attribute
/// value. The page is read as tokens, never scanned as text.
fn old_wording_in_html(html: &str) -> Vec<String> {
    let mut hits = Vec::new();
    if names_only_png_jpg_and_svg(&text_outside(html, &[])) {
        hits.push("text".to_owned());
    }
    for tag in start_tags(html) {
        for (name, value) in &tag.attrs {
            if names_only_png_jpg_and_svg(value) {
                hits.push(format!("<{} {name}>", tag.name));
            }
        }
    }
    hits
}

/// The files among `files` that carry the old wording. HTML is read as
/// tokens; every other file (the scripts) is free text and is matched as text,
/// which makes that half a lint and not a guarantee.
fn files_with_old_wording(files: &[(String, String)]) -> Vec<String> {
    files
        .iter()
        .filter(|(name, text)| {
            if name.ends_with(".html") {
                !old_wording_in_html(text).is_empty()
            } else {
                names_only_png_jpg_and_svg(text)
            }
        })
        .map(|(name, _)| name.clone())
        .collect()
}

/// Every reason the core can give for skipping a file, as the window shows it.
fn skip_texts() -> Vec<String> {
    use lunaepix_core::Skip;
    [
        Skip::UnsupportedType,
        Skip::TooLarge,
        Skip::OverRunLimit,
        Skip::IsOwnDestination,
        Skip::NotAFile,
    ]
    .iter()
    .map(ToString::to_string)
    .collect()
}

/// True when the text nodes of `html` hold `phrase`, whitespace aside.
fn page_says(html: &str, phrase: &str) -> bool {
    one_line(&text_outside(html, &[])).contains(phrase)
}

#[test]
fn no_text_names_only_png_jpg_and_svg() {
    let files = ui_files();
    assert_eq!(files_with_old_wording(&files), Vec::<String>::new());
    let skips = skip_texts();
    assert_eq!(
        skips
            .iter()
            .filter(|text| names_only_png_jpg_and_svg(text))
            .collect::<Vec<_>>(),
        Vec::<&String>::new()
    );
    // Not vacuous: the page and the scripts that hold the texts were scanned,
    // and the skip reason for an unsupported file is among the texts.
    let scanned: Vec<&str> = files.iter().map(|(name, _)| name.as_str()).collect();
    assert!(scanned.contains(&"index.html") && scanned.contains(&"state.js"));
    assert!(skips.iter().any(|text| text.starts_with("Not a ")));
}

#[test]
fn the_scan_for_the_old_wording_finds_it_in_every_spelling_in_a_fixture() {
    let page = |body: &str| format!("<!doctype html><html><body>{body}</body></html>");
    for html in [
        page("<p>PNG · JPG · SVG</p>"),
        page("<p>png · jpg · svg</p>"),
        page("<p>PNG · JPG · <b>SVG</b></p>"),
        page("<p>Not a PNG, JPG\n   or SVG file</p>"),
        page("<p>PNG · JPG · SVG\n · Gif</p>"),
        page("<button aria-label=\"PNG, JPG and SVG images\">x</button>"),
        page("<p data-hint='No PNG,  JPG or SVG files'>x</p>"),
    ] {
        assert!(!old_wording_in_html(&html).is_empty(), "{html}");
    }
    let files = [
        ("a.js", "const a = 'Not a PNG, JPG or SVG file';"),
        ("b.js", "const b = 'No PNG, JPG\n  or SVG files';"),
        ("c.js", "dialog('PNG, JPG and SVG images')"),
        ("d.js", "hint('PNG · JPG · SVG')"),
        ("e.html", "<p>PNG · JPG · SVG</p>"),
    ]
    .map(|(name, text)| (name.to_owned(), text.to_owned()));
    assert_eq!(
        files_with_old_wording(&files),
        ["a.js", "b.js", "c.js", "d.js", "e.html"]
    );
    assert!(names_only_png_jpg_and_svg("Not a PNG, JPG or SVG file"));
}

#[test]
fn the_scan_for_the_old_wording_leaves_the_new_wording_alone_in_a_fixture() {
    for html in [
        "<p>PNG · JPG · SVG · WebP</p>",
        "<p>PNG · JPG · SVG\n  · WebP</p>",
        "<p>Not a PNG, JPG, SVG or WebP file</p>",
        "<p>No PNG, JPG, SVG or WebP files directly in this folder</p>",
        "<button aria-label=\"PNG, JPG, SVG and WebP images\">x</button>",
        // A comment is not text a reader sees.
        "<!-- PNG · JPG · SVG --><p>PNG · JPG · SVG · WebP</p>",
    ] {
        assert_eq!(old_wording_in_html(html), Vec::<String>::new(), "{html}");
    }
    let files = [
        ("a.js", "'No PNG, JPG, SVG or WebP files'"),
        ("b.js", "'PNG, JPG, SVG and WebP images'"),
        ("c.js", "'PNG · JPG · SVG · WebP'"),
    ]
    .map(|(name, text)| (name.to_owned(), text.to_owned()));
    assert_eq!(files_with_old_wording(&files), Vec::<String>::new());
}

#[test]
fn the_window_and_the_skip_reason_name_webp() {
    let html = index_html();
    assert!(page_says(&html, "PNG · JPG · SVG · WebP"));
    let state = ui_files()
        .into_iter()
        .find(|(name, _)| name == "state.js")
        .unwrap()
        .1;
    assert!(one_line(&state).contains(
        "No PNG, JPG, SVG or WebP files directly in this folder (subfolders are not included)"
    ));
    assert!(skip_texts().contains(&"Not a PNG, JPG, SVG or WebP file".to_owned()));
}

#[test]
fn a_page_without_webp_in_its_text_does_not_say_it_in_a_fixture() {
    assert!(!page_says(
        "<p>PNG · JPG · SVG</p>",
        "PNG · JPG · SVG · WebP"
    ));
    // Only in a comment, or in an attribute, is not the page saying it.
    assert!(!page_says(
        "<!-- PNG · JPG · SVG · WebP --><p>PNG · JPG · SVG</p>",
        "PNG · JPG · SVG · WebP"
    ));
    assert!(!page_says(
        "<p title=\"PNG · JPG · SVG · WebP\">PNG · JPG</p>",
        "PNG · JPG · SVG · WebP"
    ));
    assert!(page_says(
        "<p>PNG · JPG · SVG\n  · WebP</p>",
        "PNG · JPG · SVG · WebP"
    ));
}

// ---------------------------------------------------------------------
// The moon logo in the window header: one decorative image, loaded from the
// window folder (the CSP allows images from the window folder and the
// `thumb` and `preview` schemes only), made from the same SVG as the app icon.
// ---------------------------------------------------------------------

/// Problems with the images in `html`. Besides Single image's two compare
/// pictures (class `lunaepixSingle__image`), which get their address from the
/// `preview` scheme at run time and so carry no `src` in the page, the page
/// must hold exactly one `<img>`, the logo; it must be decorative (`alt`
/// present and empty) and its `src` must be a relative path inside the window
/// folder: no scheme, no leading slash, no `..` or backslash, and naming a
/// file in `window_files`. Read as tokens.
fn logo_problems(html: &str, window_files: &[String]) -> Vec<String> {
    let is_compare = |tag: &StartTag| {
        tag.attr("class").is_some_and(|class| {
            class
                .split_whitespace()
                .any(|name| name == "lunaepixSingle__image")
        })
    };
    let (compare, images): (Vec<StartTag>, Vec<StartTag>) = start_tags(html)
        .into_iter()
        .filter(|tag| tag.name == "img")
        .partition(is_compare);
    if compare.iter().any(|tag| tag.attr("src").is_some()) {
        return vec!["a compare picture has a src in the page".to_owned()];
    }
    if images.len() != 1 {
        return vec![format!(
            "exactly one <img> expected, found {}",
            images.len()
        )];
    }
    let image = &images[0];
    let mut problems = Vec::new();
    if image.attr("alt") != Some("") {
        problems.push("the logo needs alt=\"\" (it is decorative)".to_owned());
    }
    match image.attr("src") {
        None => problems.push("the logo has no src".to_owned()),
        Some(src) => {
            let outside = src.contains(':')
                || src.starts_with('/')
                || src.contains('\\')
                || src.split('/').any(|part| part == ".." || part == ".");
            if outside {
                problems.push(format!("src={src:?} leaves the window folder"));
            } else if !window_files.iter().any(|name| name == src) {
                problems.push(format!("src={src:?} names no file under ui/"));
            }
        }
    }
    problems
}

/// The index of the first byte where `a` and `b` differ, or the length of the
/// shorter one when one is a prefix of the other. `None` when they are equal.
fn first_difference(a: &[u8], b: &[u8]) -> Option<usize> {
    if a == b {
        return None;
    }
    Some(a.iter().zip(b).take_while(|(x, y)| x == y).count())
}

fn ui_file_names() -> Vec<String> {
    ui_entries().into_iter().map(|(name, _)| name).collect()
}

#[test]
fn the_header_shows_one_decorative_logo_from_the_window_folder() {
    assert_eq!(
        logo_problems(&index_html(), &ui_file_names()),
        Vec::<String>::new()
    );
}

#[test]
fn a_logo_without_alt_or_from_outside_the_window_folder_is_caught_in_a_fixture() {
    let files = vec!["lunaepix-logo.svg".to_owned()];
    let good = "<header><img class=\"lunaepixHeader__logo\" src=\"lunaepix-logo.svg\" alt=\"\" \
                width=\"32\" height=\"32\"><h1>Lunaepix</h1></header>";
    assert_eq!(logo_problems(good, &files), Vec::<String>::new());
    for (bad, html) in [
        ("no alt", "<img src=\"lunaepix-logo.svg\">"),
        (
            "alt with text",
            "<img src=\"lunaepix-logo.svg\" alt=\"Moon\">",
        ),
        (
            "https",
            "<img src=\"https://example.com/lunaepix-logo.svg\" alt=\"\">",
        ),
        (
            "upper-case scheme",
            "<img src=\"HTTPS://example.com/a.svg\" alt=\"\">",
        ),
        (
            "data uri",
            "<img src=\"data:image/svg+xml;base64,AAAA\" alt=\"\">",
        ),
        (
            "parent folder",
            "<img src=\"../src-tauri/icons/lunaepix-icon.svg\" alt=\"\">",
        ),
        (
            "protocol-relative",
            "<img src=\"//example.com/lunaepix-logo.svg\" alt=\"\">",
        ),
        ("absolute path", "<img src=\"/lunaepix-logo.svg\" alt=\"\">"),
        ("backslash", "<img src=\"..\\lunaepix-logo.svg\" alt=\"\">"),
        ("no src", "<img alt=\"\">"),
        ("file not in ui", "<img src=\"missing.svg\" alt=\"\">"),
        (
            "two images",
            "<img src=\"lunaepix-logo.svg\" alt=\"\"><img src=\"lunaepix-logo.svg\" alt=\"\">",
        ),
        ("no image", "<h1>Lunaepix</h1>"),
        // Only a real tag counts: a comment that spells one is not an image.
        (
            "tag only in a comment",
            "<!-- <img src=\"lunaepix-logo.svg\" alt=\"\"> --><h1>Lunaepix</h1>",
        ),
    ] {
        assert!(!logo_problems(html, &files).is_empty(), "{bad}: {html}");
    }
}

#[test]
fn the_compare_pictures_without_a_src_do_not_count_as_logos() {
    let files = vec!["lunaepix-logo.svg".to_owned()];
    let logo = "<img class=\"lunaepixHeader__logo\" src=\"lunaepix-logo.svg\" alt=\"\">";
    let compare = "<img class=\"lunaepixSingle__image -original\" alt=\"Original\" hidden>\
                   <img class=\"lunaepixSingle__image -compressed\" alt=\"Compressed\" hidden>";
    assert_eq!(
        logo_problems(&format!("{logo}{compare}"), &files),
        Vec::<String>::new()
    );
}

#[test]
fn a_compare_picture_with_a_src_or_a_second_logo_is_caught_in_a_fixture() {
    let files = vec!["lunaepix-logo.svg".to_owned()];
    let logo = "<img class=\"lunaepixHeader__logo\" src=\"lunaepix-logo.svg\" alt=\"\">";
    for (bad, extra) in [
        (
            "a compare picture with a src",
            "<img class=\"lunaepixSingle__image -original\" src=\"lunaepix-logo.svg\" alt=\"Original\">",
        ),
        (
            "a compare picture from elsewhere",
            "<img class=\"lunaepixSingle__image\" src=\"https://example.com/a.png\" alt=\"\">",
        ),
        (
            "an image whose class only contains the name",
            "<img class=\"lunaepixSingle__imageX\" alt=\"\">",
        ),
    ] {
        assert!(
            !logo_problems(&format!("{logo}{extra}"), &files).is_empty(),
            "{bad}"
        );
    }
}

#[test]
fn the_window_logo_is_the_icon_source() {
    let ui = Path::new(env!("CARGO_MANIFEST_DIR")).join("../ui/lunaepix-logo.svg");
    let icon = Path::new(env!("CARGO_MANIFEST_DIR")).join("icons/lunaepix-icon.svg");
    let icon = fs::read(icon).unwrap();
    assert!(!icon.is_empty(), "the icon source is empty");
    let logo = fs::read(ui).expect("ui/lunaepix-logo.svg is missing");
    assert_eq!(first_difference(&logo, &icon), None);
}

#[test]
fn the_logo_comparison_reports_a_one_byte_difference() {
    let a = b"<svg>moon</svg>".to_vec();
    let mut b = a.clone();
    b[7] = b'M';
    assert_eq!(first_difference(&a, &b), Some(7));
    assert_eq!(first_difference(&a, &a), None);
    // A copy that is cut short or has a byte added differs as well.
    assert_eq!(first_difference(&a, &a[..a.len() - 1]), Some(a.len() - 1));
    let mut longer = a.clone();
    longer.push(b'\n');
    assert_eq!(first_difference(&a, &longer), Some(a.len()));
}

#[test]
fn the_logo_svg_does_not_trip_the_text_scans() {
    // The SVG is text, so every scan over `ui/` reads it. It must be among
    // the scanned files and leave the HTML-attribute check and the JS lint
    // with nothing to report (the real tests above run over all of `ui/`).
    let files = ui_files();
    let (_, svg) = files
        .iter()
        .find(|(name, _)| name == "lunaepix-logo.svg")
        .expect("ui/lunaepix-logo.svg is not among the scanned files");
    assert_eq!(js_lint_hits(svg), Vec::<&str>::new());
    assert_eq!(html_attribute_problems(svg), Vec::<String>::new());
}

// ---------------------------------------------------------------------
// The summary row after a run: "Re-add" and then "Open folder", side by
// side. Read as tokens, with the nesting walked, so a button outside
// `#summary` or one spelled in a comment is not taken for the real one.
// ---------------------------------------------------------------------

/// Elements that have no end tag.
const VOID_ELEMENTS: &[&str] = &[
    "area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "source", "track",
    "wbr",
];

/// A `<button>` as the checks need it.
struct ButtonTag {
    id: Option<String>,
    kind: Option<String>,
    class: Option<String>,
    text: String,
}

/// The buttons inside the element whose id is `container`, in document
/// order, with their text (whitespace collapsed).
fn buttons_inside(html: &str, container: &str) -> Vec<ButtonTag> {
    let mut buttons: Vec<ButtonTag> = Vec::new();
    let mut depth = 0usize;
    let mut open_button = false;
    for piece in pieces(html) {
        match piece {
            Piece::Start(tag) => {
                let void = VOID_ELEMENTS.contains(&tag.name.as_str());
                if depth == 0 {
                    if tag.attr("id") == Some(container) && !void {
                        depth = 1;
                    }
                    continue;
                }
                if tag.name == "button" {
                    buttons.push(ButtonTag {
                        id: tag.attr("id").map(str::to_owned),
                        kind: tag.attr("type").map(str::to_owned),
                        class: tag.attr("class").map(str::to_owned),
                        text: String::new(),
                    });
                    open_button = true;
                }
                if !void {
                    depth += 1;
                }
            }
            Piece::End(name) if depth > 0 => {
                if name == "button" {
                    open_button = false;
                }
                depth -= 1;
            }
            Piece::Text(text) if depth > 0 && open_button => {
                let button = buttons.last_mut().expect("a button is open");
                button.text.push_str(&text);
            }
            _ => {}
        }
    }
    for button in &mut buttons {
        button.text = button.text.split_whitespace().collect::<Vec<_>>().join(" ");
    }
    buttons
}

/// Problems with the Re-add button: exactly one `#readdList` on the page; in
/// `#summary`, directly before `#openFolder`; text "Re-add"; `type="button"`;
/// the summary's button class.
fn readd_button_problems(html: &str) -> Vec<String> {
    let mut problems = Vec::new();
    let on_page = start_tags(html)
        .iter()
        .filter(|tag| tag.attr("id") == Some("readdList"))
        .count();
    if on_page != 1 {
        problems.push(format!("{on_page} elements with id readdList, not one"));
    }
    let buttons = buttons_inside(html, "summary");
    let ids: Vec<Option<&str>> = buttons.iter().map(|button| button.id.as_deref()).collect();
    if ids != [Some("readdList"), Some("openFolder")] {
        problems.push(format!(
            "the summary's buttons are {ids:?}, not readdList then openFolder"
        ));
    }
    if let Some(button) = buttons
        .iter()
        .find(|button| button.id.as_deref() == Some("readdList"))
    {
        if button.text != "Re-add" {
            problems.push(format!("the text is {:?}, not \"Re-add\"", button.text));
        }
        if button.kind.as_deref() != Some("button") {
            problems.push("the button has no type=\"button\"".to_owned());
        }
        let has_class = button.class.as_deref().is_some_and(|class| {
            class
                .split_whitespace()
                .any(|name| name == "lunaepixSummary__button")
        });
        if !has_class {
            problems.push("the button lacks the class lunaepixSummary__button".to_owned());
        }
    }
    problems
}

#[test]
fn the_summary_has_readd_before_open_folder() {
    assert_eq!(readd_button_problems(&index_html()), Vec::<String>::new());
}

#[test]
fn a_summary_without_readd_or_with_it_misplaced_is_flagged_in_a_fixture() {
    let good = r#"<div id="summary"><p id="summaryText"></p>
        <button class="lunaepixSummary__button" id="readdList" type="button">Re-add</button>
        <button class="lunaepixSummary__button" id="openFolder" type="button">Open folder</button></div>"#;
    assert_eq!(readd_button_problems(good), Vec::<String>::new());
    // Line breaks inside the button do not matter.
    let spread = good.replace(">Re-add<", ">\n  Re-add\n<");
    assert_eq!(readd_button_problems(&spread), Vec::<String>::new());

    let open = r#"<button class="lunaepixSummary__button" id="openFolder" type="button">Open folder</button>"#;
    let readd =
        r#"<button class="lunaepixSummary__button" id="readdList" type="button">Re-add</button>"#;
    for (what, html) in [
        ("missing", format!(r#"<div id="summary">{open}</div>"#)),
        (
            "after Open folder",
            format!(r#"<div id="summary">{open}{readd}</div>"#),
        ),
        (
            "outside the summary",
            format!(r#"<div id="summary">{open}</div>{readd}"#),
        ),
        (
            "before the summary",
            format!(r#"{readd}<div id="summary">{open}</div>"#),
        ),
        ("other text", good.replace(">Re-add<", ">Reuse files<")),
        (
            "no type",
            good.replace(r#"id="readdList" type="button""#, r#"id="readdList""#),
        ),
        (
            "wrong type",
            good.replace(
                r#"id="readdList" type="button""#,
                r#"id="readdList" type="submit""#,
            ),
        ),
        (
            "other class",
            good.replacen("lunaepixSummary__button", "lunaepixPanel__button", 1),
        ),
        (
            "twice",
            good.replace(
                r#"<button class="lunaepixSummary__button" id="openFolder""#,
                &format!(r#"{readd}<button class="lunaepixSummary__button" id="openFolder""#),
            ),
        ),
        // Only a real tag counts: a comment that spells one is not a button.
        (
            "only in a comment",
            format!(r#"<!-- {readd} --><div id="summary">{open}</div>"#),
        ),
    ] {
        assert!(!readd_button_problems(&html).is_empty(), "{what}: {html}");
    }
}

// --- which commands run on the main thread ------------------------------------

/// The commands that run on the main thread (a plain `#[tauri::command]`).
/// An event sent from the main thread is shown at once, while one sent from
/// any other thread waits in the event loop's queue, so a main-thread event
/// can overtake an earlier one. These commands send nothing but `run-busy`,
/// whose place among other events the window does not depend on; their
/// real answers come later from the dialog plugin's thread, or not at all.
/// Every other command is `async`, so all its events go through the one
/// queue in the order they were sent.
const MAIN_THREAD_COMMANDS: &[&str] = &[
    "choose_files",
    "choose_output_folder",
    "set_view",
    "choose_image",
    "open_output_folder",
];

/// Whether `path` is exactly `tauri::command`.
fn is_tauri_command(path: &syn::Path) -> bool {
    let segments: Vec<String> = path
        .segments
        .iter()
        .map(|segment| segment.ident.to_string())
        .collect();
    segments == ["tauri", "command"]
}

/// The functions in the Rust `source` marked `#[tauri::command]` without
/// `async` among the attribute's arguments: the commands Tauri runs on the
/// main thread.
fn main_thread_commands(source: &str) -> HashSet<String> {
    let file = syn::parse_file(source).expect("valid Rust");
    file.items
        .iter()
        .filter_map(|item| match item {
            syn::Item::Fn(function) => Some(function),
            _ => None,
        })
        .filter_map(|function| {
            let attribute = function
                .attrs
                .iter()
                .find(|attribute| is_tauri_command(attribute.path()))?;
            let is_async = match &attribute.meta {
                syn::Meta::List(list) => list
                    .tokens
                    .clone()
                    .into_iter()
                    .any(|token| token.to_string() == "async"),
                _ => false,
            };
            (!is_async).then(|| function.sig.ident.to_string())
        })
        .collect()
}

fn name_set(list: &[&str]) -> HashSet<String> {
    list.iter().map(|name| (*name).to_owned()).collect()
}

#[test]
fn only_commands_that_send_nothing_but_run_busy_run_on_the_main_thread() {
    let lib = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/lib.rs");
    let source = fs::read_to_string(lib).unwrap();
    assert_eq!(
        main_thread_commands(&source),
        name_set(MAIN_THREAD_COMMANDS)
    );
}

#[test]
fn a_command_without_async_is_found_in_a_fixture() {
    let fixture = r#"
        #[tauri::command]
        fn preview_image() {}
        #[tauri::command(async)]
        fn start_run() {}
        #[tauri::command(rename_all = "snake_case")]
        fn clear_list() {}
        #[tauri::command(async, rename_all = "snake_case")]
        fn readd_list() {}
        #[tauri::command(rename_all = "async")]
        fn renamed() {}
        #[other::command]
        fn set_view() {}
        fn not_a_command() {}
        /// A doc comment that says #[tauri::command] is not an attribute.
        fn documented() {}
    "#;
    assert_eq!(
        main_thread_commands(fixture),
        name_set(&["preview_image", "clear_list", "renamed"])
    );
}
