//! Guards on the Homebrew formula, `Formula/lunaepix.rb`. It builds the app
//! and the command line on a user's Mac and cannot be run here, so what keeps
//! it honest is checked in a test. The formula repeats facts that live
//! elsewhere, and a repeated fact that drifts is a defect:
//!
//! - the Tauri command line version, also pinned in both workflows (a drift
//!   builds the brew app with another bundler than the checked download);
//! - the source address, which must be this repository's tag archive of the
//!   workspace version, with a checksum that is the placeholder (64 zeros,
//!   until the release commit) or 64 lower-case hex characters;
//! - the macOS floor, which must be `minimumSystemVersion` of
//!   `tauri.macos.conf.json`, in the spelling `brew style` allows: a bare
//!   `depends_on :macos` while that floor is not newer than Homebrew's oldest
//!   macOS (a named one is then redundant), a named one above it;
//! - the requirements (Apple Silicon, Rust at build time, no NASM, no Node),
//!   the app build line (the pinned `cargo-tauri` binary, called directly from
//!   the install root of the pinned install, never through PATH) and
//!   `skip_clean` of the app, and the note printed after the install (the link
//!   command and its two sentences).
//!
//! Some checks mirror a rule of Homebrew's own `brew style`, which only runs
//! on the Mac (in the Formula job): they are here so a mistake fails the
//! tests on any machine, not only after the push.
//!
//! Each check is a function from text to a list of problems, so a fixture can
//! feed it, and each has fixtures it must catch.
//!
//! The formula is Ruby, not a data format, so it is read as text and every
//! check on it is a lint, not a proof. The reading is small and spelled out:
//!
//! - Ruby comments (`#` to the end of the line, outside a string) are dropped
//!   before anything is read, so a value that is only in a comment does not
//!   count. A string does not span lines, and `#{...}` inside one is part
//!   of the string.
//! - The body of a heredoc (`<<~EOS`, `<<-EOS`, `<<EOS`, up to its terminator
//!   line) is text, not code: the caveats are read from it, with its `#`
//!   characters kept, and nothing else is read from it.
//! - A statement that ends its line in `,` or `\` goes on in the next line,
//!   so a call over two lines is one statement. Other ways to break a line
//!   (an open parenthesis, `%w[]`, a trailing operator) are not read and make
//!   a check fail, never pass.
//! - A call is read as its quoted words: `system "cargo", "install",
//!   "tauri-cli", "--version", "2.12.4"` is the words `cargo`, `install`,
//!   `tauri-cli`, `--version`, `2.12.4`. Words that are not quoted (a splat,
//!   `buildpath/"x"`) are not read, with two exceptions that are read from
//!   the statement's text: the variable after `"--root",` in the tauri-cli
//!   install, and the start of the build call (`system tauri/"bin/cargo-tauri",`),
//!   which must name that variable. A flag that takes a value is accepted as
//!   two words (`"--version", "2.12.4"`) or one (`"--version=2.12.4"`):
//!   `--version` of `cargo install`, and `--bundles` and `--config` of
//!   `cargo-tauri build`. `--locked` is one word.
//! - A stanza (`url`, `sha256`, `depends_on`, `skip_clean`) is read as the
//!   statement with its white space collapsed, in its one exact spelling, in
//!   double quotes. A form with parentheses or single quotes is not accepted
//!   (Homebrew's own style check wants the plain form anyway).
//!
//! The workflows are parsed as YAML (`yaml_rust2`) and only their `run:`
//! scripts are scanned as text, without their shell comments, the way
//! `workflow.rs` does. The workspace `Cargo.toml` is read with `toml`, and
//! `tauri.macos.conf.json` with `serde_json`.
//!
//! `ruby -c` and `brew style` are not available here; the Formula job in
//! `macos.yml` runs Homebrew's own audit and style checks.

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use serde_json::Value;
use yaml_rust2::{Yaml, YamlLoader};

// ---- the real files ----

/// A file of the repository, by its path from the repository root.
fn repo_file(path: &str) -> String {
    let full = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join(path);
    fs::read_to_string(&full).unwrap_or_else(|e| panic!("{}: {e}", full.display()))
}

/// The one config file that adds the worker sidecar; the same path as the
/// bundle build of `workflow.rs`.
const BUNDLE_CONFIG: &str = "src-tauri/tauri.bundle.conf.json";

/// The tag archive of this repository for one version.
fn expected_url(version: &str) -> String {
    format!("https://github.com/Aureus-Lunae/Lunaepix/archive/refs/tags/v{version}.tar.gz")
}

/// `[workspace.package] version` of the workspace `Cargo.toml`.
fn workspace_version(cargo_toml: &str) -> String {
    let table: toml::Table = cargo_toml.parse().expect("the workspace Cargo.toml parses");
    table["workspace"]["package"]["version"]
        .as_str()
        .expect("the workspace version is a string")
        .to_owned()
}

// ---- reading Ruby ----

/// A formula as its lines of code (comments dropped) and the bodies of its
/// heredocs.
struct Ruby {
    code: Vec<String>,
    heredocs: Vec<String>,
}

/// The terminator of a heredoc that starts in `code` (`<<~EOS`, `<<-EOS`,
/// `<<EOS`, the name in upper case, optionally quoted), if one does.
fn heredoc_start(code: &str) -> Option<String> {
    let at = code.find("<<")?;
    let rest = &code[at + 2..];
    let rest = rest.strip_prefix(['~', '-']).unwrap_or(rest);
    let rest = rest.trim_start_matches(['\'', '"']);
    let name: String = rest
        .chars()
        .take_while(|c| c.is_ascii_uppercase() || *c == '_')
        .collect();
    (!name.is_empty()).then_some(name)
}

/// The line up to a comment: a `#` outside a string starts one. A string
/// opens with `"` or `'`, a backslash skips the next character, and one that
/// does not close on its line runs to the end of it.
fn without_ruby_comment(line: &str) -> String {
    let mut out = String::new();
    let mut quote: Option<char> = None;
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        match quote {
            Some(open) if c == open => quote = None,
            Some(_) => {}
            None if c == '"' || c == '\'' => quote = Some(c),
            None if c == '#' => break,
            None => {}
        }
        out.push(c);
        if quote.is_some() && c == '\\' {
            out.extend(chars.next());
        }
    }
    out.trim_end().to_owned()
}

fn read_ruby(text: &str) -> Ruby {
    let mut ruby = Ruby {
        code: Vec::new(),
        heredocs: Vec::new(),
    };
    // The terminator being waited for, and the body so far.
    let mut open: Option<(String, Vec<&str>)> = None;
    for line in text.lines() {
        if let Some((terminator, body)) = open.as_mut() {
            if line.trim() == terminator {
                ruby.heredocs.push(body.join("\n"));
                open = None;
            } else {
                body.push(line);
            }
            continue;
        }
        let code = without_ruby_comment(line);
        if let Some(terminator) = heredoc_start(&code) {
            open = Some((terminator, Vec::new()));
        }
        if !code.trim().is_empty() {
            ruby.code.push(code);
        }
    }
    // A heredoc that never ends is still a body: the rest of the text is
    // text, so a check on the code then misses what it looks for and fails.
    if let Some((_, body)) = open {
        ruby.heredocs.push(body.join("\n"));
    }
    ruby
}

/// The statements of the code: a line that ends in `,` or `\` is joined with
/// the next one. White space inside a statement is collapsed to single
/// spaces.
fn statements(ruby: &Ruby) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut continued = false;
    for line in &ruby.code {
        let line = line.trim();
        let text = line.strip_suffix('\\').unwrap_or(line).trim_end();
        match out.last_mut() {
            Some(last) if continued => {
                last.push(' ');
                last.push_str(text);
            }
            _ => out.push(text.to_owned()),
        }
        continued = line.ends_with(',') || line.ends_with('\\');
    }
    out.iter()
        .map(|s| s.split_whitespace().collect::<Vec<_>>().join(" "))
        .collect()
}

/// The quoted words of a statement, quotes removed; a backslash keeps the
/// next character.
fn quoted_words(statement: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut chars = statement.chars();
    while let Some(c) = chars.next() {
        if c != '"' && c != '\'' {
            continue;
        }
        let mut word = String::new();
        while let Some(d) = chars.next() {
            match d {
                _ if d == c => break,
                '\\' => word.extend(chars.next()),
                _ => word.push(d),
            }
        }
        words.push(word);
    }
    words
}

/// Whether the statement is the call or stanza `name`: the name, then a space
/// or a parenthesis.
fn is_stanza(statement: &str, name: &str) -> bool {
    statement
        .strip_prefix(name)
        .is_some_and(|rest| rest.starts_with([' ', '(']))
}

/// The words of every `system` call of the formula, one list per call.
fn system_calls(formula: &str) -> Vec<Vec<String>> {
    statements(&read_ruby(formula))
        .iter()
        .filter(|s| is_stanza(s, "system"))
        .map(|s| quoted_words(s))
        .collect()
}

/// Whether the words hold `first`, `second` and `third` next to each other.
fn position_of(words: &[String], sequence: [&str; 3]) -> Option<usize> {
    words.windows(3).position(|w| w == sequence)
}

/// The value of a flag in words: `--flag value` or `--flag=value`.
fn flag_value(words: &[String], flag: &str) -> Option<String> {
    let joined = format!("{flag}=");
    words.iter().enumerate().find_map(|(at, word)| {
        if word == flag {
            words.get(at + 1).cloned()
        } else {
            word.strip_prefix(&joined).map(str::to_owned)
        }
    })
}

// ---- reading the workflows ----

/// The `run` scripts of every step of a workflow, or why it cannot be read.
fn workflow_scripts(text: &str) -> Result<Vec<String>, String> {
    let mut docs = YamlLoader::load_from_str(text)
        .map_err(|e| format!("the workflow does not parse as YAML: {e}"))?;
    let doc = match docs.len() {
        1 if docs[0].as_hash().is_some() => docs.remove(0),
        _ => return Err("the workflow is not one YAML mapping".to_owned()),
    };
    let jobs = doc["jobs"].as_hash().into_iter().flatten();
    let steps = jobs.flat_map(|(_, job)| job["steps"].as_vec().into_iter().flatten());
    Ok(steps
        .filter_map(|step| match &step["run"] {
            Yaml::String(script) => Some(script.clone()),
            _ => None,
        })
        .collect())
}

/// A script's lines without their shell comments (whole-line, and a trailing
/// ` # ...`), a `\` at the end of a line joined with the next one. Like
/// `without_comments` in `workflow.rs`: text can be lost, which makes a check
/// fail, never pass.
fn shell_lines(script: &str) -> Vec<String> {
    let code = script
        .lines()
        .filter(|line| !line.trim_start().starts_with('#'))
        .map(|line| line.split(" #").next().unwrap_or(line).trim_end());
    let mut lines: Vec<String> = Vec::new();
    let mut joined = false;
    for line in code {
        let text = line.strip_suffix('\\').unwrap_or(line);
        match lines.last_mut() {
            Some(last) if joined => {
                last.push(' ');
                last.push_str(text);
            }
            _ => lines.push(text.to_owned()),
        }
        joined = line.ends_with('\\');
    }
    lines
}

// ---- the Tauri command line: one version in three places ----

/// One `cargo install tauri-cli` and what it passes.
struct TauriInstall {
    locked: bool,
    version: Option<String>,
}

/// The install in a list of words (`cargo`, `install`, `tauri-cli`, then the
/// flags), if the words have one.
fn tauri_install(words: &[String]) -> Option<TauriInstall> {
    let at = position_of(words, ["cargo", "install", "tauri-cli"])?;
    let flags = &words[at + 3..];
    Some(TauriInstall {
        locked: flags.iter().any(|word| word == "--locked"),
        version: flag_value(flags, "--version"),
    })
}

/// Every `cargo install tauri-cli` in the run scripts of a workflow.
fn workflow_tauri_installs(text: &str) -> Result<Vec<TauriInstall>, String> {
    let scripts = workflow_scripts(text)?;
    let lines = scripts.iter().flat_map(|script| shell_lines(script));
    Ok(lines
        .filter_map(|line| {
            let words: Vec<String> = line
                .split_whitespace()
                .map(|word| word.trim_matches(['\'', '"']).to_owned())
                .collect();
            tauri_install(&words)
        })
        .collect())
}

/// The formula and both workflows install the Tauri command line, the formula
/// with `--locked`, and they all pin the same version. A formula that pins
/// another version than the workflows builds the app with another bundler
/// than the one the Mac workflow checks.
fn tauri_version_problems(formula: &str, macos_yml: &str, windows_yml: &str) -> Vec<String> {
    let mut problems = Vec::new();
    // (where, version)
    let mut seen: Vec<(&str, String)> = Vec::new();

    let installs: Vec<TauriInstall> = system_calls(formula)
        .iter()
        .filter_map(|words| tauri_install(words))
        .collect();
    if installs.is_empty() {
        problems.push("the formula has no cargo install of tauri-cli".to_owned());
    }
    for install in installs {
        if !install.locked {
            problems.push("the formula installs tauri-cli without --locked".to_owned());
        }
        match install.version {
            Some(version) => seen.push(("the formula", version)),
            None => problems.push("the formula's tauri-cli install has no --version".to_owned()),
        }
    }

    for (name, text) in [("macos.yml", macos_yml), ("windows.yml", windows_yml)] {
        match workflow_tauri_installs(text) {
            Err(why) => problems.push(format!("{name}: {why}")),
            Ok(list) if list.is_empty() => {
                problems.push(format!("{name} has no cargo install of tauri-cli"));
            }
            Ok(list) => {
                for install in list {
                    match install.version {
                        Some(version) => seen.push((name, version)),
                        None => {
                            problems.push(format!("{name}: a tauri-cli install has no --version"))
                        }
                    }
                }
            }
        }
    }

    let versions: BTreeSet<&String> = seen.iter().map(|(_, version)| version).collect();
    if versions.len() > 1 {
        let all: Vec<String> = seen.iter().map(|(at, v)| format!("{at} {v}")).collect();
        problems.push(format!(
            "tauri-cli is not pinned to one version: {}",
            all.join(", ")
        ));
    }
    problems
}

// ---- the source: url and sha256 ----

/// The statements of the formula that are the stanza `name`.
fn stanzas(formula: &str, name: &str) -> Vec<String> {
    statements(&read_ruby(formula))
        .into_iter()
        .filter(|s| is_stanza(s, name))
        .collect()
}

/// Exactly one `url`, and it is `url "<this repository's tag archive of the
/// workspace version>"` and nothing else on the line.
fn url_problems(formula: &str, workspace_version: &str) -> Vec<String> {
    let expected = format!("url \"{}\"", expected_url(workspace_version));
    let urls = stanzas(formula, "url");
    match urls.as_slice() {
        [] => vec!["the formula has no url".to_owned()],
        [one] if *one == expected => Vec::new(),
        [one] => vec![format!("the url line is {one:?}, expected {expected:?}")],
        many => vec![format!("the formula has {} url lines, not 1", many.len())],
    }
}

/// 64 lower-case hex characters; the placeholder of zeros is one of them.
fn is_sha256(text: &str) -> bool {
    text.len() == 64 && text.chars().all(|c| matches!(c, '0'..='9' | 'a'..='f'))
}

/// Exactly one `sha256`, as `sha256 "<64 lower-case hex>"`. Zeros are hex, so
/// the placeholder between releases passes.
fn sha256_problems(formula: &str) -> Vec<String> {
    let sums = stanzas(formula, "sha256");
    match sums.as_slice() {
        [] => vec!["the formula has no sha256".to_owned()],
        [one] => {
            let value = one
                .strip_prefix("sha256 \"")
                .and_then(|rest| rest.strip_suffix('"'));
            if value.is_some_and(is_sha256) {
                Vec::new()
            } else {
                vec![format!(
                    "the sha256 line is {one:?}, expected 64 lower-case hex characters in double quotes"
                )]
            }
        }
        many => vec![format!(
            "the formula has {} sha256 lines, not 1",
            many.len()
        )],
    }
}

// ---- the macOS floor ----

/// Homebrew's macOS names and the version each stands for.
const MACOS_NAMES: &[(&str, (u32, u32))] = &[
    ("big_sur", (11, 0)),
    ("monterey", (12, 0)),
    ("ventura", (13, 0)),
    ("sonoma", (14, 0)),
    ("sequoia", (15, 0)),
    ("tahoe", (26, 0)),
];

/// Homebrew's oldest macOS: the first entry of `MacOSVersion::SYMBOLS` in
/// Homebrew's `Library/Homebrew/macos_version.rb`. It is a copy of Homebrew's
/// own table and must follow it: when Homebrew drops big_sur, change this.
const HOMEBREW_OLDEST_MACOS: (&str, (u32, u32)) = ("big_sur", (11, 0));

/// `"11.0"` or `"11"` as major and minor.
fn parse_version(text: &str) -> Option<(u32, u32)> {
    let mut parts = text.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next().map_or(Some(0), |part| part.parse().ok())?;
    parts.next().is_none().then_some((major, minor))
}

/// The floor the formula declares is the one `minimumSystemVersion` in
/// `tauri.macos.conf.json` sets, so the formula refuses what the app refuses.
/// It has two spellings, and Homebrew's `brew style` decides which one is
/// allowed (`Library/Homebrew/rubocops/os_depends_on.rb`):
///
/// - `minimumSystemVersion` is not newer than Homebrew's oldest macOS
///   (`HOMEBREW_OLDEST_MACOS`): the floor is redundant, so the formula has
///   exactly one bare `depends_on :macos` (it only limits the formula to
///   macOS) and no `depends_on macos:` line. brew style refuses a named floor
///   that old, and a bare line next to a named one.
/// - It is newer: exactly one `depends_on macos: :<name>` that names that
///   version, and no bare line.
///
/// The config is read first, so a config problem is reported alone.
fn macos_floor_problems(formula: &str, tauri_macos_json: &str) -> Vec<String> {
    let config: Value = match serde_json::from_str(tauri_macos_json) {
        Ok(value) => value,
        Err(e) => return vec![format!("tauri.macos.conf.json does not parse: {e}")],
    };
    let Some(text) = config["bundle"]["macOS"]["minimumSystemVersion"].as_str() else {
        return vec![
            "tauri.macos.conf.json has no bundle.macOS.minimumSystemVersion string".to_owned(),
        ];
    };
    let Some(config_floor) = parse_version(text) else {
        return vec![format!(
            "minimumSystemVersion {text:?} is not a version number"
        )];
    };

    let (oldest_name, oldest) = HOMEBREW_OLDEST_MACOS;
    let depends = stanzas(formula, "depends_on");
    let bare = depends.iter().filter(|s| *s == "depends_on :macos").count();
    let named: Vec<&String> = depends
        .iter()
        .filter(|s| s.starts_with("depends_on macos:"))
        .collect();

    let mut problems = Vec::new();
    if config_floor <= oldest {
        for line in &named {
            problems.push(format!(
                "the formula names a macOS floor ({line}), but minimumSystemVersion {text:?} is not newer than Homebrew's oldest macOS ({oldest_name}, {}.{}): brew style refuses that as redundant, the line must be depends_on :macos",
                oldest.0, oldest.1
            ));
        }
        if bare > 1 || (bare == 0 && named.is_empty()) {
            problems.push(format!(
                "the formula has {bare} depends_on :macos lines, not 1"
            ));
        }
    } else {
        if bare > 0 {
            problems.push(format!(
                "the formula has a bare depends_on :macos, but minimumSystemVersion {text:?} is newer than Homebrew's oldest macOS ({oldest_name}): name it with depends_on macos: :<name>"
            ));
        }
        match named.as_slice() {
            // Only the bare line, already reported.
            [] if bare > 0 => {}
            [line] => problems.extend(named_floor_problem(line, config_floor, text)),
            many => problems.push(format!(
                "the formula has {} depends_on macos: lines, not 1",
                many.len()
            )),
        }
    }
    problems
}

/// The problem with one `depends_on macos: :<name>` line, if it is not the
/// name of `config_floor`.
fn named_floor_problem(line: &str, config_floor: (u32, u32), text: &str) -> Option<String> {
    let known = line
        .strip_prefix("depends_on macos: :")
        .and_then(|name| MACOS_NAMES.iter().find(|(known, _)| *known == name));
    let Some(&(name, formula_floor)) = known else {
        return Some(format!("{line} is not a macOS name this check knows"));
    };
    (formula_floor != config_floor).then(|| {
        format!(
            "the formula's floor :{name} is macOS {}.{}, minimumSystemVersion is {text:?}",
            formula_floor.0, formula_floor.1
        )
    })
}

// ---- the requirements ----

/// Apple Silicon and Rust at build time are declared, and nothing declares
/// NASM or Node: the Mac needs neither (mozjpeg assembles its Arm SIMD code
/// with clang, and there is no Node build step).
fn requirement_problems(formula: &str) -> Vec<String> {
    let depends = stanzas(formula, "depends_on");
    let mut problems = Vec::new();
    for required in ["depends_on arch: :arm64", "depends_on \"rust\" => :build"] {
        if !depends.iter().any(|line| line == required) {
            problems.push(format!("the formula lacks {required}"));
        }
    }
    for line in &depends {
        let name = quoted_words(line).into_iter().next().unwrap_or_default();
        if name == "nasm" || name == "node" || name.starts_with("node@") {
            problems.push(format!("the formula depends on {name}: {line}"));
        }
    }
    problems
}

// ---- the app build ----

/// A `system` call of the formula: the statement as written (white space
/// collapsed) and its quoted words.
struct SystemCall {
    statement: String,
    words: Vec<String>,
}

/// Every `system` call of the formula.
fn system_statements(formula: &str) -> Vec<SystemCall> {
    statements(&read_ruby(formula))
        .into_iter()
        .filter(|s| is_stanza(s, "system"))
        .map(|statement| SystemCall {
            words: quoted_words(&statement),
            statement,
        })
        .collect()
}

/// Whether the words are a call of the Tauri command line's binary, not of
/// `cargo`: the first word is `cargo-tauri` or a path that ends in
/// `/cargo-tauri` (the formula writes `tauri/"bin/cargo-tauri"`, so the quoted
/// word is `bin/cargo-tauri`), and the second is `build`. A `cargo tauri
/// build` is not one: `cargo_build_problems` names it.
fn is_cargo_tauri_build(words: &[String]) -> bool {
    let binary = words
        .first()
        .is_some_and(|word| word == "cargo-tauri" || word.ends_with("/cargo-tauri"));
    binary && words.get(1).is_some_and(|word| word == "build")
}

/// Every `cargo-tauri build` call of the formula.
fn tauri_build_calls(formula: &str) -> Vec<SystemCall> {
    system_statements(formula)
        .into_iter()
        .filter(|call| is_cargo_tauri_build(&call.words))
        .collect()
}

/// The variable a `cargo install tauri-cli` call passes as its root: the
/// unquoted word after `"--root",` (`"--root", tauri` is `tauri`). `--root=...`,
/// a quoted path or an expression (`tauri/"x"`, `tauri.to_s`) is not a
/// variable and is not read, so a check on it fails, never passes.
fn root_variable(statement: &str) -> Option<String> {
    let (_, after) = statement.split_once("\"--root\",")?;
    let after = after.trim_start();
    let name: String = after
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
        .collect();
    let rest = after[name.len()..].trim_start();
    let first = name.chars().next()?;
    ((first.is_ascii_lowercase() || first == '_') && (rest.is_empty() || rest.starts_with(',')))
        .then_some(name)
}

/// The install roots (variable names) of the formula's `cargo install
/// tauri-cli` calls.
fn tauri_install_roots(formula: &str) -> Vec<String> {
    system_statements(formula)
        .iter()
        .filter(|call| tauri_install(&call.words).is_some())
        .filter_map(|call| root_variable(&call.statement))
        .collect()
}

/// The app is built by calling the pinned binary directly: `system
/// <root>/"bin/cargo-tauri", "build", ...`, where `<root>` is the variable the
/// formula's `cargo install tauri-cli` passes as `--root`. So the binary is the
/// one installed by the pinned install, never one found through PATH.
/// Every such call passes `--bundles app` and `--config
/// src-tauri/tauri.bundle.conf.json` (the Mac workflow's build, without the
/// disk image), and the app is kept as built: `skip_clean "Lunaepix.app"`.
/// Other spellings of the call are not read: a leftover `cargo tauri build` is
/// "no cargo-tauri build" here, and `cargo_build_problems` names it.
fn app_build_problems(formula: &str) -> Vec<String> {
    let mut problems = Vec::new();
    let calls = tauri_build_calls(formula);
    let roots = tauri_install_roots(formula);
    if calls.is_empty() {
        problems.push("the formula has no cargo-tauri build".to_owned());
    } else if roots.is_empty() {
        problems.push(
            "the tauri-cli install has no --root variable, so the build's binary cannot be tied to it"
                .to_owned(),
        );
    }
    for call in &calls {
        let shown = call.words.join(" ");
        if flag_value(&call.words, "--bundles").as_deref() != Some("app") {
            problems.push(format!(
                "a cargo-tauri build without --bundles app: {shown}"
            ));
        }
        if flag_value(&call.words, "--config").as_deref() != Some(BUNDLE_CONFIG) {
            problems.push(format!(
                "a cargo-tauri build without --config {BUNDLE_CONFIG}: {shown}"
            ));
        }
        let from_root = roots.iter().any(|root| {
            call.statement
                .starts_with(&format!("system {root}/\"bin/cargo-tauri\","))
        });
        if !roots.is_empty() && !from_root {
            problems.push(format!(
                "a cargo-tauri build that is not run from the install root ({}/\"bin/cargo-tauri\"): {}",
                roots.join(" or "),
                call.statement
            ));
        }
    }
    if !stanzas(formula, "skip_clean")
        .iter()
        .any(|line| line == "skip_clean \"Lunaepix.app\"")
    {
        problems.push("the formula lacks skip_clean \"Lunaepix.app\"".to_owned());
    }
    problems
}

// ---- no cargo build call ----

/// Whether the words of a call hold both `cargo` and `build`, in any
/// positions.
fn has_cargo_and_build(words: &[String]) -> bool {
    words.iter().any(|word| word == "cargo") && words.iter().any(|word| word == "build")
}

/// No `system` call of the formula has both a `cargo` word and a `build`
/// word. This mirrors the text rule of Homebrew's `brew style`
/// (`FormulaAudit/Text` in `Library/Homebrew/rubocops/text.rb`: "Use
/// `"cargo", "install", *std_cargo_args`"), which only runs on the Mac, in the
/// Formula job. It is stricter: brew style spares a call that passes
/// `--lib`, this check does not, because the formula has no use for one.
fn cargo_build_problems(formula: &str) -> Vec<String> {
    system_statements(formula)
        .iter()
        .filter(|call| has_cargo_and_build(&call.words))
        .map(|call| {
            format!(
                "a system call with both \"cargo\" and \"build\": {}",
                call.words.join(" ")
            )
        })
        .collect()
}

// ---- the note after the install ----

const LINK_LINE: &str = "ln -sfn \"#{opt_prefix}/Lunaepix.app\" /Applications/Lunaepix.app";
const SPOTLIGHT_SENTENCE: &str =
    "Spotlight and Launchpad do not list a linked app: open Lunaepix from the Applications folder.";
/// The user's sentence (the spec's Handoff): a download user has a real
/// folder in Applications, and `ln -sfn` would put the link inside it.
const DOWNLOAD_SENTENCE: &str =
    "If Lunaepix from the download is in Applications, move it to the Bin first.";

/// The heredoc text of the formula (the caveats) has the link command on a
/// line of its own, with `-n` so an existing link is not followed, and both
/// sentences. The sentences are read with their line breaks collapsed.
fn caveat_problems(formula: &str) -> Vec<String> {
    let bodies = read_ruby(formula).heredocs;
    let link = bodies
        .iter()
        .flat_map(|body| body.lines())
        .any(|line| line.trim() == LINK_LINE);
    let flat = bodies
        .iter()
        .map(|body| body.split_whitespace().collect::<Vec<_>>().join(" "))
        .collect::<Vec<_>>()
        .join("\n");
    let mut problems = Vec::new();
    if !link {
        problems.push(format!("the caveats lack the link line {LINK_LINE}"));
    }
    if !flat.contains(SPOTLIGHT_SENTENCE) {
        problems.push(format!(
            "the caveats lack the sentence {SPOTLIGHT_SENTENCE:?}"
        ));
    }
    if !flat.contains(DOWNLOAD_SENTENCE) {
        problems.push(format!(
            "the caveats lack the sentence {DOWNLOAD_SENTENCE:?}"
        ));
    }
    problems
}

// ---- every check ----

/// Every check on a set of texts, by name.
fn every_formula_check(
    formula: &str,
    macos_yml: &str,
    windows_yml: &str,
    workspace_version: &str,
    tauri_macos_json: &str,
) -> Vec<(&'static str, Vec<String>)> {
    vec![
        (
            "tauri_version_problems",
            tauri_version_problems(formula, macos_yml, windows_yml),
        ),
        ("url_problems", url_problems(formula, workspace_version)),
        ("sha256_problems", sha256_problems(formula)),
        (
            "macos_floor_problems",
            macos_floor_problems(formula, tauri_macos_json),
        ),
        ("requirement_problems", requirement_problems(formula)),
        ("app_build_problems", app_build_problems(formula)),
        ("cargo_build_problems", cargo_build_problems(formula)),
        ("caveat_problems", caveat_problems(formula)),
    ]
}

// ---- fixtures ----

const GOOD_VERSION: &str = "0.1.5";

const GOOD_JSON: &str =
    r#"{"bundle": {"macOS": {"signingIdentity": "-", "minimumSystemVersion": "11.0"}}}"#;

const GOOD_MACOS_YML: &str = "name: macOS build
jobs:
  build:
    steps:
      # The version is pinned.
      - name: Install the Tauri command line
        run: cargo install tauri-cli --locked --version 2.12.4
";

const GOOD_WINDOWS_YML: &str = "name: Windows build
jobs:
  build:
    steps:
      - name: Install the Tauri command line
        run: |
          cargo install tauri-cli --locked --version 2.12.4
";

/// The formula as it is meant to be, without its `test do` block. Every
/// formula fixture is this text with one change.
const GOOD_FORMULA: &str = r##"class Lunaepix < Formula
  desc "Make PNG, JPG and WebP images smaller, lossless or lossy"
  homepage "https://github.com/Aureus-Lunae/Lunaepix"
  url "https://github.com/Aureus-Lunae/Lunaepix/archive/refs/tags/v0.1.5.tar.gz"
  # The real checksum is filled in by the commit right after the release tag.
  sha256 "0000000000000000000000000000000000000000000000000000000000000000"
  license "MIT"

  depends_on "rust" => :build
  depends_on arch: :arm64
  depends_on :macos

  # Any change inside the app breaks its seal.
  skip_clean "Lunaepix.app"

  def install
    system "cargo", "install", "--target-dir", "target", *std_cargo_args(path: "crates/cli")

    tauri = buildpath/"tauri-cli"
    system "cargo", "install", "tauri-cli", "--locked", "--version", "2.12.4", "--root", tauri

    system tauri/"bin/cargo-tauri", "build", "--bundles", "app",
           "--config", "src-tauri/tauri.bundle.conf.json"
    prefix.install "target/release/bundle/macos/Lunaepix.app"
  end

  def caveats
    <<~EOS
      Lunaepix.app is installed in:
        #{opt_prefix}/Lunaepix.app

      To open it from the Applications folder, link it there once. If Lunaepix
      from the download is in Applications, move it to the Bin first.
        ln -sfn "#{opt_prefix}/Lunaepix.app" /Applications/Lunaepix.app

      The link keeps working after `brew upgrade`. Spotlight and Launchpad
      do not list a linked app: open Lunaepix from the Applications folder.
    EOS
  end
end
"##;

/// The formula fixture with `from` replaced by `to`; `from` must be in it,
/// so a fixture that changes nothing fails instead of passing.
fn formula_with(from: &str, to: &str) -> String {
    assert!(GOOD_FORMULA.contains(from), "the fixture has no {from:?}");
    GOOD_FORMULA.replace(from, to)
}

/// The good fixtures, one check at a time.
fn good_checks(formula: &str) -> Vec<(&'static str, Vec<String>)> {
    every_formula_check(
        formula,
        GOOD_MACOS_YML,
        GOOD_WINDOWS_YML,
        GOOD_VERSION,
        GOOD_JSON,
    )
}

fn assert_clean(problems: Vec<String>) {
    assert_eq!(problems, Vec::<String>::new());
}

/// Exactly one problem, and it starts with `start`.
fn assert_one_problem(problems: &[String], start: &str, what: &str) {
    assert!(
        problems.len() == 1 && problems[0].starts_with(start),
        "{what}: expected one problem starting {start:?}, got {problems:?}"
    );
}

/// A problem that starts with `start` is among `problems`.
fn assert_problem(problems: &[String], start: &str, what: &str) {
    let found = problems.iter().any(|problem| problem.starts_with(start));
    assert!(
        found,
        "{what}: no problem starting {start:?} in {problems:?}"
    );
}

// ---- the real files ----

#[test]
fn the_real_files_pass_every_formula_check() {
    let formula = repo_file("Formula/lunaepix.rb");
    let checks = every_formula_check(
        &formula,
        &repo_file(".github/workflows/macos.yml"),
        &repo_file(".github/workflows/windows.yml"),
        &workspace_version(&repo_file("Cargo.toml")),
        &repo_file("src-tauri/tauri.macos.conf.json"),
    );
    for (name, problems) in checks {
        assert_eq!(problems, Vec::<String>::new(), "{name}");
    }
}

#[test]
fn the_formula_checks_are_not_vacuous() {
    let formula = repo_file("Formula/lunaepix.rb");
    assert_eq!(stanzas(&formula, "url").len(), 1);
    assert_eq!(stanzas(&formula, "sha256").len(), 1);
    assert_eq!(stanzas(&formula, "skip_clean").len(), 1);
    assert_eq!(tauri_build_calls(&formula).len(), 1);
    // One install of tauri-cli (the command line's own `cargo install` is another call).
    let installs = system_calls(&formula)
        .iter()
        .filter(|words| position_of(words, ["cargo", "install", "tauri-cli"]).is_some())
        .count();
    assert_eq!(installs, 1);
    assert!(stanzas(&formula, "depends_on").len() >= 3);
    // One floor line, and it is the bare one (the config says 11.0, Homebrew's oldest).
    let floors: Vec<String> = stanzas(&formula, "depends_on")
        .into_iter()
        .filter(|line| line.contains("macos"))
        .collect();
    assert_eq!(floors, ["depends_on :macos"]);
    // The install root the build runs from, and no `cargo`+`build` call.
    assert_eq!(tauri_install_roots(&formula).len(), 1);
    assert!(
        system_calls(&formula)
            .iter()
            .all(|words| !has_cargo_and_build(words))
    );
    assert_eq!(read_ruby(&formula).heredocs.len(), 1, "the caveats");
    for name in ["macos.yml", "windows.yml"] {
        let text = repo_file(&format!(".github/workflows/{name}"));
        let found = workflow_tauri_installs(&text).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(found.len(), 1, "{name}");
    }
    let config: Value =
        serde_json::from_str(&repo_file("src-tauri/tauri.macos.conf.json")).expect("JSON");
    assert!(config["bundle"]["macOS"]["minimumSystemVersion"].is_string());
    // The bundle config the formula names is a file of the repository.
    assert!(!repo_file(BUNDLE_CONFIG).is_empty());
}

// ---- reading Ruby ----

#[test]
fn a_well_formed_formula_passes_every_check_in_a_fixture() {
    for (name, problems) in good_checks(GOOD_FORMULA) {
        assert_eq!(problems, Vec::<String>::new(), "{name}");
    }
}

#[test]
fn a_ruby_comment_is_dropped_from_the_code_in_a_fixture() {
    let ruby = read_ruby("a 1\n# b 2\n  # c 3\nd 4 # e 5\n");
    assert_eq!(ruby.code, ["a 1", "d 4"]);
    assert!(ruby.heredocs.is_empty());
}

#[test]
fn a_hash_inside_a_string_is_not_a_comment_in_a_fixture() {
    let ruby = read_ruby(
        "a \"x # y\" # gone\nb 'x # y'\nc \"#{z}\"\nd \"q \\\" # still string\"\ne \"never closes # here\n",
    );
    assert_eq!(
        ruby.code,
        [
            "a \"x # y\"",
            "b 'x # y'",
            "c \"#{z}\"",
            "d \"q \\\" # still string\"",
            "e \"never closes # here",
        ]
    );
}

#[test]
fn a_heredoc_body_is_text_not_code_in_a_fixture() {
    for start in ["x = <<~EOS", "x = <<-EOS", "x = <<EOS", "x = <<~'EOS'"] {
        let text = format!("{start}\n  # kept #{{y}}\n  body\n  EOS\nz 1 # gone\n");
        let ruby = read_ruby(&text);
        assert_eq!(ruby.code, [start, "z 1"], "{start}");
        assert_eq!(ruby.heredocs, ["  # kept #{y}\n  body"], "{start}");
    }
    // One that never ends: the rest is body, not code.
    let ruby = read_ruby("x = <<~EOS\n  system \"cargo\"\n");
    assert_eq!(ruby.code, ["x = <<~EOS"]);
    assert_eq!(ruby.heredocs, ["  system \"cargo\""]);
    // `<<` that is not a heredoc (an append of a lower-case name) is code.
    let ruby = read_ruby("list << item\nnext 1\n");
    assert_eq!(ruby.code, ["list << item", "next 1"]);
    assert!(ruby.heredocs.is_empty());
}

#[test]
fn a_call_continued_on_the_next_line_is_one_call_in_a_fixture() {
    let ruby = read_ruby(
        "system \"a\", \"b\",\n       \"c\"\nsystem \"d\" \\\n  \"e\"\nsystem \"f\"\n\"g\"\n",
    );
    assert_eq!(
        statements(&ruby),
        [
            "system \"a\", \"b\", \"c\"",
            "system \"d\" \"e\"",
            "system \"f\"",
            "\"g\""
        ]
    );
    assert_eq!(
        quoted_words("system \"a\", 'b c', \"d\\\"e\""),
        ["a", "b c", "d\"e"]
    );
}

// ---- the Tauri version ----

#[test]
fn the_same_tauri_version_in_all_three_passes_in_a_fixture() {
    assert_clean(tauri_version_problems(
        GOOD_FORMULA,
        GOOD_MACOS_YML,
        GOOD_WINDOWS_YML,
    ));
    // `--version=`, in the formula and in a workflow.
    let formula = formula_with("\"--version\", \"2.12.4\"", "\"--version=2.12.4\"");
    let macos = GOOD_MACOS_YML.replace("--version 2.12.4", "--version=2.12.4");
    assert_clean(tauri_version_problems(&formula, &macos, GOOD_WINDOWS_YML));
    // The flags in another order, and a call over two lines.
    let formula = formula_with(
        "\"tauri-cli\", \"--locked\", \"--version\", \"2.12.4\", \"--root\", tauri",
        "\"tauri-cli\", \"--root\", tauri,\n           \"--version\", \"2.12.4\", \"--locked\"",
    );
    assert_clean(tauri_version_problems(
        &formula,
        GOOD_MACOS_YML,
        GOOD_WINDOWS_YML,
    ));
}

#[test]
fn a_formula_that_pins_another_tauri_version_is_caught_in_a_fixture() {
    let formula = formula_with("\"2.12.4\"", "\"2.12.3\"");
    let problems = tauri_version_problems(&formula, GOOD_MACOS_YML, GOOD_WINDOWS_YML);
    assert_one_problem(
        &problems,
        "tauri-cli is not pinned to one version",
        "formula",
    );
}

#[test]
fn a_workflow_that_pins_another_tauri_version_is_caught_in_a_fixture() {
    let macos = GOOD_MACOS_YML.replace("2.12.4", "2.12.5");
    let problems = tauri_version_problems(GOOD_FORMULA, &macos, GOOD_WINDOWS_YML);
    assert_one_problem(&problems, "tauri-cli is not pinned to one version", "macos");
    let windows = GOOD_WINDOWS_YML.replace("2.12.4", "2.11.0");
    let problems = tauri_version_problems(GOOD_FORMULA, GOOD_MACOS_YML, &windows);
    assert_one_problem(
        &problems,
        "tauri-cli is not pinned to one version",
        "windows",
    );
}

#[test]
fn a_formula_without_a_tauri_install_is_caught_in_a_fixture() {
    let start = "the formula has no cargo install of tauri-cli";
    let installs = "system \"cargo\", \"install\", \"tauri-cli\", \"--locked\", \"--version\", \"2.12.4\", \"--root\", tauri";
    // Removed; commented out; another crate (`tauri`, `tauri-cli-x`).
    for formula in [
        formula_with(installs, "ENV[\"X\"] = \"1\""),
        formula_with(installs, &format!("# {installs}")),
        formula_with("\"tauri-cli\", \"--locked\"", "\"tauri\", \"--locked\""),
        formula_with(
            "\"tauri-cli\", \"--locked\"",
            "\"tauri-cli-x\", \"--locked\"",
        ),
    ] {
        let problems = tauri_version_problems(&formula, GOOD_MACOS_YML, GOOD_WINDOWS_YML);
        assert_one_problem(&problems, start, &formula);
    }
}

#[test]
fn a_tauri_install_without_locked_is_caught_in_a_fixture() {
    let start = "the formula installs tauri-cli without --locked";
    for formula in [
        formula_with("\"--locked\", ", ""),
        formula_with("\"--locked\", ", "# \"--locked\"\n    "),
    ] {
        let problems = tauri_version_problems(&formula, GOOD_MACOS_YML, GOOD_WINDOWS_YML);
        assert_one_problem(&problems, start, &formula);
    }
}

#[test]
fn a_tauri_version_only_in_a_ruby_comment_is_caught_in_a_fixture() {
    let formula = formula_with(
        "\"--version\", \"2.12.4\", ",
        "# \"--version\", \"2.12.4\"\n           ",
    );
    // The fixture's first line is `... "--locked", # "--version", "2.12.4"`:
    // its code part ends in a comma, so the next line (`"--root", tauri`) is
    // joined to it, and the version, being only in the comment, is not read:
    // the install has no version in code.
    let problems = tauri_version_problems(&formula, GOOD_MACOS_YML, GOOD_WINDOWS_YML);
    assert_problem(
        &problems,
        "the formula's tauri-cli install has no --version",
        &formula,
    );
    // A trailing comment after the call is not the call's version either.
    let formula = formula_with("\"--version\", \"2.12.4\", ", "")
        .replace("\"--root\", tauri", "\"--root\", tauri # --version 2.12.4");
    let problems = tauri_version_problems(&formula, GOOD_MACOS_YML, GOOD_WINDOWS_YML);
    assert_one_problem(
        &problems,
        "the formula's tauri-cli install has no --version",
        &formula,
    );
}

#[test]
fn a_workflow_without_a_tauri_install_is_caught_in_a_fixture() {
    let none = "name: x\njobs:\n  build:\n    steps:\n      - run: cargo build\n";
    let comment = "name: x\njobs:\n  build:\n    steps:\n      - run: |\n          # cargo install tauri-cli --locked --version 2.12.4\n          cargo build\n";
    for text in [none, comment] {
        let problems = tauri_version_problems(GOOD_FORMULA, text, GOOD_WINDOWS_YML);
        assert_one_problem(
            &problems,
            "macos.yml has no cargo install of tauri-cli",
            text,
        );
        let problems = tauri_version_problems(GOOD_FORMULA, GOOD_MACOS_YML, text);
        assert_one_problem(
            &problems,
            "windows.yml has no cargo install of tauri-cli",
            text,
        );
    }
    // A workflow that is not YAML, or not one mapping.
    for text in ["a: [unclosed", "- a\n- b\n", "a: 1\n---\nb: 2\n"] {
        let problems = tauri_version_problems(GOOD_FORMULA, text, GOOD_WINDOWS_YML);
        assert_one_problem(&problems, "macos.yml: the workflow ", text);
    }
    // An install without a version.
    let text = GOOD_MACOS_YML.replace(" --version 2.12.4", "");
    let problems = tauri_version_problems(GOOD_FORMULA, &text, GOOD_WINDOWS_YML);
    assert_one_problem(
        &problems,
        "macos.yml: a tauri-cli install has no --version",
        &text,
    );
}

// ---- url ----

#[test]
fn a_url_for_this_versions_tag_archive_passes_in_a_fixture() {
    assert_clean(url_problems(GOOD_FORMULA, "0.1.5"));
    // The version is the workspace's: 0.1.6 passes a formula for 0.1.6.
    let formula = formula_with("v0.1.5.tar.gz", "v0.1.6.tar.gz");
    assert_clean(url_problems(&formula, "0.1.6"));
}

#[test]
fn a_url_for_another_version_repository_branch_or_format_is_caught_in_a_fixture() {
    let start = "the url line is";
    for to in ["v0.1.4.tar.gz", "v0.1.5.zip", "v0.1.5.tar.gz?x=1"] {
        let formula = formula_with("v0.1.5.tar.gz", to);
        assert_one_problem(&url_problems(&formula, "0.1.5"), start, to);
    }
    for (from, to) in [
        ("Aureus-Lunae/Lunaepix/archive", "Someone/Lunaepix/archive"),
        (
            "Aureus-Lunae/Lunaepix/archive",
            "Aureus-Lunae/Other/archive",
        ),
        ("refs/tags/v0.1.5.tar.gz", "refs/heads/main.tar.gz"),
        (
            "archive/refs/tags/v0.1.5.tar.gz",
            "releases/download/v0.1.5/Lunaepix.tar.gz",
        ),
    ] {
        let formula = formula_with(from, to);
        assert_one_problem(&url_problems(&formula, "0.1.5"), start, to);
    }
    // A branch archive written the way GitHub does.
    let formula = formula_with(
        "archive/refs/tags/v0.1.5.tar.gz",
        "archive/refs/heads/main.tar.gz",
    );
    assert_one_problem(&url_problems(&formula, "0.1.5"), start, "branch");
    // The formula is right for 0.1.5 and the workspace says another version.
    assert_one_problem(&url_problems(GOOD_FORMULA, "0.1.6"), start, "workspace");
    // Another spelling of the stanza, or more on the line, is not the one form.
    let url = expected_url("0.1.5");
    for line in [
        format!("url(\"{url}\")"),
        format!("url '{url}'"),
        format!("url \"{url}\", using: :curl"),
    ] {
        let formula = GOOD_FORMULA.replace(&format!("url \"{url}\""), &line);
        assert_one_problem(&url_problems(&formula, "0.1.5"), start, &line);
    }
}

#[test]
fn a_missing_or_repeated_url_is_caught_in_a_fixture() {
    let line = "url \"https://github.com/Aureus-Lunae/Lunaepix/archive/refs/tags/v0.1.5.tar.gz\"";
    let none = formula_with(line, "");
    let commented = formula_with(line, &format!("# {line}"));
    for formula in [none, commented] {
        assert_one_problem(
            &url_problems(&formula, "0.1.5"),
            "the formula has no url",
            &formula,
        );
    }
    let twice = formula_with(line, &format!("{line}\n  {line}"));
    assert_one_problem(
        &url_problems(&twice, "0.1.5"),
        "the formula has 2 url lines",
        "twice",
    );
}

// ---- sha256 ----

#[test]
fn a_placeholder_or_a_lowercase_hex_checksum_passes_in_a_fixture() {
    assert_clean(sha256_problems(GOOD_FORMULA));
    let zeros = "0".repeat(64);
    let real = "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08";
    let formula = formula_with(&zeros, real);
    assert_clean(sha256_problems(&formula));
}

#[test]
fn a_checksum_of_the_wrong_length_case_or_form_is_caught_in_a_fixture() {
    let zeros = "0".repeat(64);
    let real = "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08";
    let upper = real.to_uppercase();
    for value in [
        &real[..63],
        &format!("{real}0"),
        &upper,
        &format!("g{}", &real[1..]),
        "",
    ] {
        let formula = formula_with(&zeros, value);
        assert_one_problem(&sha256_problems(&formula), "the sha256 line is", value);
    }
    for line in [
        format!("sha256 '{zeros}'"),
        format!("sha256({zeros:?})"),
        format!("sha256 \"{zeros}\", foo: 1"),
    ] {
        let formula = formula_with(&format!("sha256 \"{zeros}\""), &line);
        assert_one_problem(&sha256_problems(&formula), "the sha256 line is", &line);
    }
}

#[test]
fn a_missing_or_repeated_checksum_is_caught_in_a_fixture() {
    let line = format!("sha256 \"{}\"", "0".repeat(64));
    for formula in [
        formula_with(&line, ""),
        formula_with(&line, &format!("# {line}")),
    ] {
        assert_one_problem(
            &sha256_problems(&formula),
            "the formula has no sha256",
            &formula,
        );
    }
    let twice = formula_with(&line, &format!("{line}\n  {line}"));
    assert_one_problem(
        &sha256_problems(&twice),
        "the formula has 2 sha256 lines",
        "twice",
    );
}

// ---- the macOS floor ----

/// The good fixture's floor line, and a formula with it replaced.
const BARE_FLOOR: &str = "depends_on :macos";

fn with_floor_line(line: &str) -> String {
    formula_with(BARE_FLOOR, line)
}

/// The good config with another `minimumSystemVersion`.
fn config_with_floor(version: &str) -> String {
    GOOD_JSON.replace("11.0", version)
}

#[test]
fn a_floor_that_matches_the_config_passes_in_a_fixture() {
    assert_clean(macos_floor_problems(GOOD_FORMULA, GOOD_JSON));
    // Not newer than Homebrew's oldest: the bare line, also for "11" and for
    // an older floor.
    for version in ["11", "10.15"] {
        assert_clean(macos_floor_problems(
            GOOD_FORMULA,
            &config_with_floor(version),
        ));
    }
    // Newer: the name is read from the table, not hard-coded to one.
    for (name, version) in [("monterey", "12.0"), ("tahoe", "26.0"), ("sequoia", "15")] {
        let formula = with_floor_line(&format!("depends_on macos: :{name}"));
        assert_clean(macos_floor_problems(&formula, &config_with_floor(version)));
    }
}

#[test]
fn homebrews_oldest_macos_is_the_lowest_name_in_the_table() {
    // No negative test: two constants of this file that agree or do not.
    assert!(MACOS_NAMES.contains(&HOMEBREW_OLDEST_MACOS));
    assert!(
        MACOS_NAMES
            .iter()
            .all(|(_, version)| *version >= HOMEBREW_OLDEST_MACOS.1)
    );
}

#[test]
fn a_named_floor_that_is_not_newer_than_homebrews_oldest_is_caught_in_a_fixture() {
    let start = "the formula names a macOS floor";
    // The rc2 formula, which brew style refused, and other spellings.
    for line in [
        "depends_on macos: :big_sur",
        "depends_on macos: :monterey",
        "depends_on macos: \"big_sur\"",
        "depends_on :macos\n  depends_on macos: :big_sur",
    ] {
        let formula = with_floor_line(line);
        assert_one_problem(&macos_floor_problems(&formula, GOOD_JSON), start, line);
        let older = config_with_floor("10.15");
        assert_one_problem(&macos_floor_problems(&formula, &older), start, line);
    }
}

#[test]
fn a_bare_floor_when_the_config_is_newer_is_caught_in_a_fixture() {
    let start = "the formula has a bare depends_on :macos";
    for version in ["12.0", "26.0", "11.1"] {
        let json = config_with_floor(version);
        assert_one_problem(&macos_floor_problems(GOOD_FORMULA, &json), start, version);
    }
    // The bare line next to the right name is still the bare line's problem.
    let formula = with_floor_line("depends_on :macos\n  depends_on macos: :monterey");
    let json = config_with_floor("12.0");
    assert_one_problem(&macos_floor_problems(&formula, &json), start, "both");
}

#[test]
fn a_floor_that_differs_from_the_config_is_caught_in_a_fixture() {
    let start = "the formula's floor";
    for (name, version) in [
        ("big_sur", "12.0"),
        ("big_sur", "11.1"),
        ("monterey", "13.0"),
        ("monterey", "12.1"),
    ] {
        let formula = with_floor_line(&format!("depends_on macos: :{name}"));
        let json = config_with_floor(version);
        assert_one_problem(
            &macos_floor_problems(&formula, &json),
            start,
            &format!("{name} {version}"),
        );
    }
}

#[test]
fn a_missing_or_repeated_floor_is_caught_in_a_fixture() {
    // Not newer: no bare line, a commented one, and two.
    let start = "the formula has 0 depends_on :macos lines";
    for formula in [
        with_floor_line(""),
        with_floor_line(&format!("# {BARE_FLOOR}")),
    ] {
        let problems = macos_floor_problems(&formula, GOOD_JSON);
        assert_one_problem(&problems, start, &formula);
    }
    let twice = with_floor_line(&format!("{BARE_FLOOR}\n  {BARE_FLOOR}"));
    assert_one_problem(
        &macos_floor_problems(&twice, GOOD_JSON),
        "the formula has 2 depends_on :macos lines",
        "twice",
    );
    // Newer: no named floor, a commented one, two, a name the table does not
    // know, and a floor that is not a symbol.
    let newer = config_with_floor("12.0");
    let named = "depends_on macos: :monterey";
    for formula in [with_floor_line(""), with_floor_line(&format!("# {named}"))] {
        assert_one_problem(
            &macos_floor_problems(&formula, &newer),
            "the formula has 0 depends_on macos: lines",
            &formula,
        );
    }
    let twice = with_floor_line(&format!("{named}\n  {named}"));
    assert_one_problem(
        &macos_floor_problems(&twice, &newer),
        "the formula has 2 depends_on macos: lines",
        "two names",
    );
    for line in [
        "depends_on macos: :hollywood",
        "depends_on macos: \"monterey\"",
    ] {
        let formula = with_floor_line(line);
        assert_one_problem(
            &macos_floor_problems(&formula, &newer),
            &format!("{line} is not a macOS name"),
            line,
        );
    }
}

#[test]
fn an_unreadable_config_is_caught_in_a_fixture() {
    // Whatever the formula says, the config is read first. Not JSON, a
    // missing key, the key as another type.
    let problems = macos_floor_problems(GOOD_FORMULA, "{not json");
    assert_one_problem(
        &problems,
        "tauri.macos.conf.json does not parse",
        "not json",
    );
    for json in [
        "{}",
        r#"{"bundle": {"macOS": {}}}"#,
        r#"{"bundle": {"macOS": {"minimumSystemVersion": 11}}}"#,
    ] {
        let problems = macos_floor_problems(GOOD_FORMULA, json);
        assert_one_problem(&problems, "tauri.macos.conf.json has no bundle.macOS", json);
    }
    // A version that is not numbers.
    for version in ["eleven", "11.x", "11.0.1", ""] {
        let json = config_with_floor(version);
        let problems = macos_floor_problems(GOOD_FORMULA, &json);
        assert_one_problem(&problems, "minimumSystemVersion", version);
    }
    // A formula with no floor at all does not hide a config problem.
    let problems = macos_floor_problems(&with_floor_line(""), "{not json");
    assert_one_problem(
        &problems,
        "tauri.macos.conf.json does not parse",
        "no floor",
    );
}

// ---- the requirements ----

#[test]
fn the_required_requirements_pass_in_a_fixture() {
    assert_clean(requirement_problems(GOOD_FORMULA));
    // Other dependencies are not this check's business.
    let formula = formula_with(
        "depends_on arch: :arm64",
        "depends_on arch: :arm64\n  depends_on \"pkgconf\" => :build",
    );
    assert_clean(requirement_problems(&formula));
}

#[test]
fn a_missing_arm64_or_rust_requirement_is_caught_in_a_fixture() {
    let arch = "depends_on arch: :arm64";
    let rust = "depends_on \"rust\" => :build";
    for formula in [
        formula_with(arch, ""),
        formula_with(arch, &format!("# {arch}")),
    ] {
        assert_one_problem(
            &requirement_problems(&formula),
            "the formula lacks depends_on arch",
            &formula,
        );
    }
    for formula in [
        formula_with(rust, ""),
        formula_with(rust, &format!("# {rust}")),
        formula_with(rust, "depends_on \"rust\""),
        formula_with(rust, "depends_on \"rust\" => :test"),
    ] {
        assert_one_problem(
            &requirement_problems(&formula),
            "the formula lacks depends_on \"rust\"",
            &formula,
        );
    }
}

#[test]
fn a_nasm_or_node_requirement_is_caught_in_a_fixture() {
    for extra in [
        "depends_on \"nasm\"",
        "depends_on \"nasm\" => :build",
        "depends_on \"node\"",
        "depends_on \"node\" => :build",
        "depends_on \"node@22\" => :build",
    ] {
        let formula = formula_with(
            "depends_on arch: :arm64",
            &format!("depends_on arch: :arm64\n  {extra}"),
        );
        assert_one_problem(
            &requirement_problems(&formula),
            "the formula depends on",
            extra,
        );
    }
}

// ---- the app build ----

/// The good formula with the tauri-cli install root named `root` instead of
/// `tauri`, in the variable, the install and the build.
fn formula_with_root_named(root: &str) -> String {
    GOOD_FORMULA
        .replace("tauri = buildpath", &format!("{root} = buildpath"))
        .replace("\"--root\", tauri", &format!("\"--root\", {root}"))
        .replace("system tauri/", &format!("system {root}/"))
}

#[test]
fn a_build_with_the_app_bundle_and_the_config_passes_in_a_fixture() {
    assert_clean(app_build_problems(GOOD_FORMULA));
    let build =
        "\"--bundles\", \"app\",\n           \"--config\", \"src-tauri/tauri.bundle.conf.json\"";
    for to in [
        "\"--bundles=app\", \"--config=src-tauri/tauri.bundle.conf.json\"",
        "\"--bundles=app\",\n           \"--config\", \"src-tauri/tauri.bundle.conf.json\"",
        "\"--config\", \"src-tauri/tauri.bundle.conf.json\", \"--bundles\", \"app\"",
        "\"--config=src-tauri/tauri.bundle.conf.json\", \"--bundles\", \"app\"",
    ] {
        assert_clean(app_build_problems(&formula_with(build, to)));
    }
    // The install root is a variable of any name, the same in the install and
    // the build.
    assert_clean(app_build_problems(&formula_with_root_named("cli_root")));
}

#[test]
fn a_build_without_the_app_bundle_or_the_config_is_caught_in_a_fixture() {
    let bundles = "\"--bundles\", \"app\"";
    let config = "\"--config\", \"src-tauri/tauri.bundle.conf.json\"";
    for to in [
        "".to_owned(),
        "\"--bundles\", \"dmg\"".to_owned(),
        "\"--bundles\", \"app,dmg\"".to_owned(),
        "\"--bundles=dmg\"".to_owned(),
        "\"--bundles\"".to_owned(),
        // In a comment only.
        "# \"--bundles\", \"app\"".to_owned(),
    ] {
        let formula = formula_with(bundles, &to);
        let problems = app_build_problems(&formula);
        assert_problem(&problems, "a cargo-tauri build without --bundles app", &to);
    }
    for to in [
        "".to_owned(),
        "\"--config\", \"src-tauri/tauri.conf.json\"".to_owned(),
        "\"--config=other.json\"".to_owned(),
        "\"--config\"".to_owned(),
    ] {
        let formula = formula_with(config, &to);
        let problems = app_build_problems(&formula);
        assert_problem(&problems, "a cargo-tauri build without --config", &to);
    }
    // One missing flag is one problem, not two.
    let formula = formula_with(bundles, "\"--bundles\", \"dmg\"");
    assert_one_problem(
        &app_build_problems(&formula),
        "a cargo-tauri build without --bundles app",
        "dmg",
    );
}

#[test]
fn a_formula_without_a_cargo_tauri_build_is_caught_in_a_fixture() {
    let call = "system tauri/\"bin/cargo-tauri\", \"build\", \"--bundles\", \"app\",\n           \"--config\", \"src-tauri/tauri.bundle.conf.json\"";
    for to in [
        "".to_owned(),
        "system \"cargo\", \"build\", \"--release\"".to_owned(),
        // The rc2 call: `cargo tauri build` is not a cargo-tauri call. It is
        // left to `cargo_build_problems`, which names it.
        "system \"cargo\", \"tauri\", \"build\", \"--bundles\", \"app\",\n           \"--config\", \"src-tauri/tauri.bundle.conf.json\"".to_owned(),
        // Another subcommand of the binary.
        "system tauri/\"bin/cargo-tauri\", \"dev\"".to_owned(),
        // Commented out, both lines.
        "# system tauri/\"bin/cargo-tauri\", \"build\", \"--bundles\", \"app\",\n    # \"--config\", \"src-tauri/tauri.bundle.conf.json\"".to_owned(),
    ] {
        let formula = formula_with(call, &to);
        let problems = app_build_problems(&formula);
        assert_one_problem(&problems, "the formula has no cargo-tauri build", &to);
    }
}

#[test]
fn a_build_that_is_not_run_from_the_install_root_is_caught_in_a_fixture() {
    let start = "a cargo-tauri build that is not run from the install root";
    let binary = "system tauri/\"bin/cargo-tauri\",";
    // Found through PATH, from another variable, from the root but not its
    // `bin`, from an absolute path, written with parentheses.
    for to in [
        "system \"cargo-tauri\",",
        "system other/\"bin/cargo-tauri\",",
        "system tauri/\"cargo-tauri\",",
        "system \"/opt/x/bin/cargo-tauri\",",
        "system(tauri/\"bin/cargo-tauri\",",
    ] {
        let formula = formula_with(binary, to);
        assert_one_problem(&app_build_problems(&formula), start, to);
    }
    // The install writes to another root than the build runs from.
    let formula = formula_with("\"--root\", tauri", "\"--root\", other");
    assert_one_problem(&app_build_problems(&formula), start, "root");
}

#[test]
fn a_tauri_install_without_a_root_variable_is_caught_in_a_fixture() {
    let start = "the tauri-cli install has no --root variable";
    for to in [
        "",
        ", \"--root=/tmp/x\"",
        ", \"--root\", \"/tmp/x\"",
        ", \"--root\", tauri/\"x\"",
        ", \"--root\", tauri.to_s",
    ] {
        let formula = formula_with(", \"--root\", tauri", to);
        assert_one_problem(&app_build_problems(&formula), start, to);
    }
}

#[test]
fn a_formula_without_skip_clean_for_the_app_is_caught_in_a_fixture() {
    let line = "skip_clean \"Lunaepix.app\"";
    for to in [
        String::new(),
        "skip_clean \"Other.app\"".to_owned(),
        format!("# {line}"),
        "skip_clean \"Lunaepix.app\", \"bin\"".to_owned(),
    ] {
        let formula = formula_with(line, &to);
        let problems = app_build_problems(&formula);
        assert_one_problem(&problems, "the formula lacks skip_clean", &to);
    }
}

// ---- no cargo build call ----

#[test]
fn a_system_call_without_both_cargo_and_build_passes_in_a_fixture() {
    // The good formula has two `cargo install` calls and the direct binary.
    assert_clean(cargo_build_problems(GOOD_FORMULA));
    // Only one of the two words, or neither.
    for call in [
        "system \"cargo\", \"test\"",
        "system \"make\", \"build\"",
        "system \"cargo build\"",
    ] {
        let formula = formula_with(
            "    prefix.install",
            &format!("    {call}\n    prefix.install"),
        );
        assert_clean(cargo_build_problems(&formula));
    }
    // Only in a comment, or in the text of the caveats: not a call.
    let call = "system \"cargo\", \"build\", \"--release\"";
    let commented = formula_with(
        "    prefix.install",
        &format!("    # {call}\n    prefix.install"),
    );
    assert_clean(cargo_build_problems(&commented));
    let in_text = formula_with(
        "      Lunaepix.app is installed in:",
        &format!("      {call}\n      Lunaepix.app is installed in:"),
    );
    assert_clean(cargo_build_problems(&in_text));
}

#[test]
fn a_system_call_with_cargo_and_build_is_caught_in_a_fixture() {
    let start = "a system call with both \"cargo\" and \"build\"";
    // The rc2 formula's call, and the plain build.
    let rc2 = formula_with(
        "system tauri/\"bin/cargo-tauri\", \"build\",",
        "system \"cargo\", \"tauri\", \"build\",",
    );
    assert_one_problem(&cargo_build_problems(&rc2), start, "rc2");
    for call in [
        "system \"cargo\", \"build\", \"--release\"",
        // Any position, any order.
        "system \"build\", \"--release\", \"cargo\"",
        "system \"cargo\", \"--release\", \"x\", \"build\"",
        // Brew style spares `--lib`; this check does not.
        "system \"cargo\", \"build\", \"--lib\"",
        // Over two lines, and with parentheses.
        "system \"cargo\",\n           \"build\"",
        "system(\"cargo\", \"build\")",
    ] {
        let formula = formula_with(
            "    prefix.install",
            &format!("    {call}\n    prefix.install"),
        );
        assert_one_problem(&cargo_build_problems(&formula), start, call);
    }
    // Two such calls are two problems.
    let formula = formula_with(
        "    prefix.install",
        "    system \"cargo\", \"build\"\n    system \"cargo\", \"build\", \"--lib\"\n    prefix.install",
    );
    assert_eq!(cargo_build_problems(&formula).len(), 2);
}

// ---- the note after the install ----

#[test]
fn the_caveats_with_the_link_and_both_sentences_pass_in_a_fixture() {
    assert_clean(caveat_problems(GOOD_FORMULA));
    // The same sentences, wrapped at other places.
    let formula = formula_with(
        "      The link keeps working after `brew upgrade`. Spotlight and Launchpad\n      do not list a linked app: open Lunaepix from the Applications folder.",
        "      The link keeps working after `brew upgrade`. Spotlight\n      and Launchpad do not list a linked app:\n      open Lunaepix from the Applications folder.",
    );
    assert_clean(caveat_problems(&formula));
}

#[test]
fn a_caveat_link_without_the_n_flag_or_pointing_elsewhere_is_caught_in_a_fixture() {
    let start = "the caveats lack the link line";
    let link = "ln -sfn \"#{opt_prefix}/Lunaepix.app\" /Applications/Lunaepix.app";
    for to in [
        "",
        "ln -sf \"#{opt_prefix}/Lunaepix.app\" /Applications/Lunaepix.app",
        "ln -sfn \"#{opt_prefix}/Lunaepix.app\" /Applications/Other.app",
        "ln -sfn \"#{prefix}/Lunaepix.app\" /Applications/Lunaepix.app",
        "ln -sfn #{opt_prefix}/Lunaepix.app /Applications/Lunaepix.app",
    ] {
        let formula = formula_with(link, to);
        assert_one_problem(&caveat_problems(&formula), start, to);
    }
}

#[test]
fn a_caveat_without_the_spotlight_sentence_is_caught_in_a_fixture() {
    let sentence = "Spotlight and Launchpad\n      do not list a linked app: open Lunaepix from the Applications folder.";
    for to in [
        "",
        "Spotlight lists it.",
        "Spotlight and Launchpad do not list a linked app.",
    ] {
        let formula = formula_with(sentence, to);
        assert_one_problem(
            &caveat_problems(&formula),
            "the caveats lack the sentence \"Spotlight",
            to,
        );
    }
}

#[test]
fn a_caveat_without_the_download_sentence_is_caught_in_a_fixture() {
    let sentence =
        " If Lunaepix\n      from the download is in Applications, move it to the Bin first.";
    for to in [
        "",
        " Move it to the Bin first.",
        " If Lunaepix from the download is in Applications, move it first.",
    ] {
        let formula = formula_with(sentence, to);
        assert_one_problem(
            &caveat_problems(&formula),
            "the caveats lack the sentence \"If Lunaepix",
            to,
        );
    }
}

#[test]
fn a_caveat_text_only_in_a_ruby_comment_is_caught_in_a_fixture() {
    // The whole note moved into comments, with no heredoc left.
    let start = GOOD_FORMULA.find("  def caveats").expect("caveats");
    let end = GOOD_FORMULA.find("    EOS\n  end\n").expect("end") + "    EOS\n  end\n".len();
    let commented: String = GOOD_FORMULA[start..end]
        .lines()
        .map(|line| format!("  # {}\n", line.trim_start()))
        .collect();
    let formula = format!(
        "{}{}{}",
        &GOOD_FORMULA[..start],
        commented,
        &GOOD_FORMULA[end..]
    );
    let problems = caveat_problems(&formula);
    assert_eq!(problems.len(), 3, "{problems:?}");
    assert_problem(&problems, "the caveats lack the link line", "link");
    assert_problem(
        &problems,
        "the caveats lack the sentence \"Spotlight",
        "spotlight",
    );
    assert_problem(
        &problems,
        "the caveats lack the sentence \"If Lunaepix",
        "download",
    );
}
