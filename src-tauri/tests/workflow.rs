//! Guards on the Windows build workflow (`.github/workflows/windows.yml`).
//! It cannot be run here, and it builds the installer people download, so
//! the rules that keep it safe are checked in a test: every action pinned to
//! a commit, read-only permissions, no secrets, no way to start it from a
//! pull request, and the test and build commands present. Each check has
//! fixtures it must catch, so it is known to fail when it should.
//!
//! The workflow is parsed as YAML 1.2 (so the key `on` is a name, not
//! boolean true) and every check reads the parsed tree, so quoting, spacing
//! and flow or block style cannot hide anything. Only a `run:` string is
//! scanned as text, and then all of it, shell comments included: a false hit
//! from a comment is accepted, a missed release is not.

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use yaml_rust2::{Yaml, YamlLoader};

fn workflow() -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../.github/workflows/windows.yml");
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

// ---- reading the tree ----

/// The workflow as one YAML mapping, or why it is not.
fn parse(text: &str) -> Result<Yaml, String> {
    let mut docs = YamlLoader::load_from_str(text)
        .map_err(|e| format!("the workflow does not parse as YAML: {e}"))?;
    match (
        docs.len(),
        docs.first().is_some_and(|d| d.as_hash().is_some()),
    ) {
        (1, true) => Ok(docs.remove(0)),
        (1, false) => Err("the workflow is not a YAML mapping".to_owned()),
        (n, _) => Err(format!("the workflow holds {n} YAML documents, not 1")),
    }
}

/// Runs a check on the parsed workflow; one that does not parse is the one problem.
fn on_tree(text: &str, check: impl FnOnce(&Yaml) -> Vec<String>) -> Vec<String> {
    parse(text).map_or_else(|problem| vec![problem], |doc| check(&doc))
}

/// A scalar as text, however written (`22`, `'22'` and `"22"` are all "22").
fn scalar(node: &Yaml) -> Option<String> {
    match node {
        Yaml::String(text) | Yaml::Real(text) => Some(text.clone()),
        Yaml::Integer(number) => Some(number.to_string()),
        Yaml::Boolean(flag) => Some(flag.to_string()),
        _ => None,
    }
}

/// Every scalar of the tree, keys and values, at any depth.
fn all_texts(node: &Yaml) -> Vec<String> {
    match node {
        Yaml::Hash(map) => map
            .iter()
            .flat_map(|(k, v)| [all_texts(k), all_texts(v)].concat())
            .collect(),
        Yaml::Array(items) => items.iter().flat_map(all_texts).collect(),
        other => scalar(other).into_iter().collect(),
    }
}

fn jobs(doc: &Yaml) -> Vec<(String, &Yaml)> {
    let map = doc["jobs"].as_hash().into_iter().flatten();
    map.map(|(name, job)| (scalar(name).unwrap_or_default(), job))
        .collect()
}

fn steps_of(job: &Yaml) -> Vec<&Yaml> {
    job["steps"].as_vec().into_iter().flatten().collect()
}

fn all_steps(doc: &Yaml) -> Vec<&Yaml> {
    jobs(doc)
        .into_iter()
        .flat_map(|(_, job)| steps_of(job))
        .collect()
}

/// The `uses` of a step or job as text; one that is not a plain scalar is
/// shown as what it is, so it fails the pin check.
fn uses_of(node: &Yaml) -> Option<String> {
    let uses = &node["uses"];
    (!uses.is_badvalue()).then(|| scalar(uses).unwrap_or_else(|| format!("{uses:?}")))
}

fn uploads_artifact(step: &Yaml) -> bool {
    uses_of(step).is_some_and(|uses| uses.starts_with("actions/upload-artifact@"))
}

/// Every action reference: the `uses` of each step, and of each job that calls
/// a reusable workflow. A `uses` inside `with` is an input, not an action.
fn references(doc: &Yaml) -> Vec<String> {
    let nodes = jobs(doc)
        .into_iter()
        .flat_map(|(_, job)| [vec![job], steps_of(job)].concat());
    nodes.filter_map(uses_of).collect()
}

/// The whole `run` string of every step, comments included.
fn scripts(doc: &Yaml) -> Vec<String> {
    all_steps(doc)
        .into_iter()
        .filter_map(|step| scalar(&step["run"]))
        .collect()
}

fn action_references(text: &str) -> Vec<String> {
    parse(text).map(|doc| references(&doc)).unwrap_or_default()
}

fn run_scripts(text: &str) -> Vec<String> {
    parse(text).map(|doc| scripts(&doc)).unwrap_or_default()
}

// ---- pinned actions ----

fn is_pinned(reference: &str) -> bool {
    let sha = |s: &str| s.len() == 40 && s.chars().all(|c| matches!(c, '0'..='9' | 'a'..='f'));
    reference
        .split_once('@')
        .is_some_and(|(name, hash)| name.contains('/') && sha(hash))
}

/// Whether `line` has a `# ...` comment with text right after `reference`
/// (a closing quote may sit in between).
fn has_release_comment(line: &str, reference: &str) -> bool {
    let rest = line.split_once(reference).map_or("", |(_, rest)| rest);
    let rest = rest.trim_start_matches(|c: char| c == '\'' || c == '"' || c.is_whitespace());
    rest.strip_prefix('#')
        .is_some_and(|comment| !comment.trim().is_empty())
}

/// Every `uses` must name `owner/repo@<40 hex digits>` (a tag or branch can
/// be moved to other code; a commit cannot) and its line must end in a
/// `# ...` comment naming the release. The comment is the one thing the tree
/// does not hold, so it is looked up on the `uses:` lines that name the pin.
fn unpinned_actions(text: &str) -> Vec<String> {
    on_tree(text, |doc| {
        let refs = references(doc);
        let (pinned, loose): (Vec<&String>, Vec<&String>) = refs.iter().partition(|r| is_pinned(r));
        let mut problems: Vec<String> = loose
            .iter()
            .map(|r| format!("not pinned to a commit: {r}"))
            .collect();
        let key_of = |line: &&str| {
            line.trim_start()
                .trim_start_matches('-')
                .trim_start()
                .starts_with("uses:")
        };
        let uses_lines: Vec<&str> = text.lines().filter(key_of).collect();
        for pin in pinned.iter().collect::<BTreeSet<_>>() {
            let lines: Vec<&&str> = uses_lines
                .iter()
                .filter(|line| line.contains(pin.as_str()))
                .collect();
            let commented = lines
                .iter()
                .filter(|line| has_release_comment(line, pin))
                .count();
            let used = refs.iter().filter(|r| r == pin).count();
            if lines.len() != used || commented != used {
                problems.push(format!("no release comment: {pin} is used {used}x, {commented} of {} `uses:` lines carry one", lines.len()));
            }
        }
        problems
    })
}

// ---- permissions, secrets, triggers ----

/// Whether a `permissions` node gives `write` or `write-all` anywhere.
fn grants_write(node: &Yaml) -> bool {
    all_texts(node)
        .iter()
        .any(|v| v.eq_ignore_ascii_case("write") || v.eq_ignore_ascii_case("write-all"))
}

/// The top-level block must be exactly `contents: read`, and no `write`
/// permission may appear at the top or in a job. Values are compared as
/// parsed, so `write`, `'write'` and `"write"` are the same.
fn permission_problems(text: &str) -> Vec<String> {
    on_tree(text, |doc| {
        let top = &doc["permissions"];
        let mut problems = Vec::new();
        if top.is_badvalue() {
            problems.push("no top-level permissions block".to_owned());
        } else {
            let read_only = top.as_hash().is_some_and(|map| map.len() == 1)
                && scalar(&top["contents"]).as_deref() == Some("read");
            if !read_only {
                problems.push(format!("top-level permissions are {top:?}"));
            }
            if grants_write(top) {
                problems.push("write permission at the top level".to_owned());
            }
        }
        let writers = jobs(doc)
            .into_iter()
            .filter(|(_, job)| grants_write(&job["permissions"]));
        problems.extend(writers.map(|(name, _)| format!("write permission in job {name}")));
        problems
    })
}

/// Every key or value that reaches a repository secret or the job token: the
/// word `secrets` (`secrets.X`, `secrets['X']`, `toJSON(secrets)`,
/// `secrets: inherit`), `github.token`, `GITHUB_TOKEN`, a `github-token` input.
fn secret_references(text: &str) -> Vec<String> {
    on_tree(text, |doc| {
        let mentions = |value: &String| {
            let lower = value.to_ascii_lowercase();
            // `secrets` as a whole word: not `mysecretsauce`, not `secrets_manager`.
            let word = lower
                .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                .any(|w| w == "secrets");
            word || lower.contains("github.token")
                || lower.replace('-', "_").contains("github_token")
        };
        all_texts(doc).into_iter().filter(mentions).collect()
    })
}

/// `on` must hold `push` and `workflow_dispatch` and nothing else, and `push`
/// must filter on the tag `v*` only (no branches or paths, so no push to a
/// branch starts it). A pull request trigger is therefore a problem,
/// `pull_request_target` above all: it runs with a token that can write, on
/// code from a fork. `on: push`, `on: [push]` and `on: {push: ...}` are read alike.
fn trigger_problems(text: &str) -> Vec<String> {
    on_tree(text, |doc| {
        let on = &doc["on"];
        if on.is_badvalue() {
            return vec!["no on: block".to_owned()];
        }
        let mut events = match on {
            Yaml::Hash(map) => map.keys().filter_map(scalar).collect(),
            other => all_texts(other),
        };
        events.sort_unstable();
        let mut problems = Vec::new();
        if events != ["push", "workflow_dispatch"] {
            problems.push(format!("triggers are {events:?}"));
        }
        let push = &on["push"];
        let filters = push
            .as_hash()
            .into_iter()
            .flat_map(|map| map.keys())
            .filter(|key| scalar(key).as_deref() != Some("tags"));
        problems.extend(filters.map(|key| format!("push is filtered by: {key:?}")));
        let tags = all_texts(&push["tags"]);
        if tags != ["v*"] {
            problems.push(format!("push tags are {tags:?}, not [\"v*\"]"));
        }
        problems
    })
}

// ---- run scripts ----

/// A `${{ }}` inside a `run:` string is substituted before the shell reads
/// it, so anything attacker-controlled in it (a branch name, an issue title)
/// runs. The scripts here need none: none is allowed (the whole string is
/// scanned, comments included), and `github.event` is used nowhere. A
/// workflow in which no script is found is a problem too.
fn injection_problems(text: &str) -> Vec<String> {
    on_tree(text, |doc| {
        let scripts = scripts(doc);
        let mut problems = Vec::new();
        if scripts.is_empty() {
            problems.push("no run scripts found".to_owned());
        }
        let expressions = scripts.iter().filter(|script| script.contains("${{"));
        problems.extend(expressions.map(|script| format!("expression in a run script: {script}")));
        let events = all_texts(doc)
            .into_iter()
            .filter(|value| value.contains("github.event"));
        problems.extend(events.map(|value| format!("github.event used: {value}")));
        problems
    })
}

/// A script without its shell comments (whole-line, and a trailing ` # ...`).
/// Only used to decide whether a command is present, where losing text can
/// make a check fail but never pass.
fn without_comments(script: &str) -> String {
    let code = script
        .lines()
        .filter(|line| !line.trim_start().starts_with('#'));
    code.map(|line| line.split(" #").next().unwrap_or(line))
        .collect::<Vec<_>>()
        .join("\n")
}

/// No script found at all, or a command in `required` that no script contains
/// outside a shell comment.
fn missing_commands(text: &str, required: &[&str]) -> Vec<String> {
    on_tree(text, |doc| {
        let scripts = scripts(doc);
        let mut problems = Vec::new();
        if scripts.is_empty() {
            problems.push("no run scripts found".to_owned());
        }
        let code = scripts
            .iter()
            .map(|script| without_comments(script))
            .collect::<Vec<_>>()
            .join("\n");
        problems.extend(
            required
                .iter()
                .filter(|command| !code.contains(*command))
                .map(|command| format!("missing: {command}")),
        );
        problems
    })
}

// ---- structure and releases ----

/// The words of `text` that name a release: any word containing `release` (an
/// action, `gh release`, a `.../releases` call, `releaseId`) and `tauri-action`
/// (which makes releases). The only exceptions are the exact word `--release`
/// (a cargo or tauri flag) and the start `target/release/` of a word (the
/// build folder); the rest of that word is still read.
fn release_hits(text: &str) -> Vec<String> {
    let words = text
        .split_whitespace()
        .map(|word| word.trim_matches(|c| c == '\'' || c == '"'));
    let words = words.filter(|word| *word != "--release");
    let words = words.map(|word| word.strip_prefix("./").unwrap_or(word));
    let words = words.map(|word| word.strip_prefix("target/release/").unwrap_or(word));
    let release = |word: &&str| {
        ["release", "tauri-action"]
            .iter()
            .any(|bad| word.to_ascii_lowercase().contains(bad))
    };
    words.filter(release).map(str::to_owned).collect()
}

/// Every job has a `runs-on` that is `windows-latest`; the jobs `test` and
/// `build` exist; `build` needs `test`, uploads with `actions/upload-artifact@`
/// and restores no cache (a debug-profile cache does not serve a release
/// build); runs are queued per workflow and ref; and no key or value anywhere
/// (`uses` and `run` included) names a release.
fn structure_problems(text: &str) -> Vec<String> {
    on_tree(text, |doc| {
        let jobs = jobs(doc);
        let mut problems = Vec::new();
        if jobs.is_empty() {
            problems.push("no jobs".to_owned());
        }
        for (name, job) in &jobs {
            let runner = &job["runs-on"];
            match scalar(runner) {
                None if runner.is_badvalue() => problems.push(format!("job {name}: no runs-on")),
                None => problems.push(format!(
                    "job {name}: runs-on is not a single runner name: {runner:?}"
                )),
                Some(runner) if runner != "windows-latest" => problems.push(format!(
                    "job {name}: runner is not windows-latest: {runner}"
                )),
                Some(_) => {}
            }
        }
        let find = |name: &str| {
            jobs.iter()
                .find(|(job, _)| job == name)
                .map(|(_, job)| *job)
        };
        if find("test").is_none() {
            problems.push("no test job".to_owned());
        }
        if let Some(build) = find("build") {
            let needs = &build["needs"];
            if !all_texts(needs).iter().any(|name| name == "test") {
                problems.push("the build job does not have needs: test".to_owned());
            }
            if !steps_of(build).into_iter().any(uploads_artifact) {
                problems.push("the build job does not upload an artifact".to_owned());
            }
            let caches = steps_of(build)
                .into_iter()
                .filter_map(uses_of)
                .filter(|uses| uses.to_ascii_lowercase().contains("cache"));
            problems.extend(caches.map(|uses| format!("the build job restores a cache: {uses}")));
        } else {
            problems.push("no build job".to_owned());
        }
        let group = "${{ github.workflow }}-${{ github.ref }}";
        if doc["concurrency"].is_badvalue() {
            problems.push("no concurrency".to_owned());
        } else if scalar(&doc["concurrency"]["group"]).as_deref() != Some(group) {
            problems.push(format!("the concurrency group is not `{group}`"));
        }
        let hits = all_texts(doc)
            .iter()
            .flat_map(|value| release_hits(value))
            .collect::<Vec<_>>();
        problems.extend(hits.into_iter().map(|hit| format!("release step: {hit}")));
        problems
    })
}

/// `defaults.run.shell` is `bash` (so the quoting in the scripts means the
/// same on every step); every `node-version` is a number of at least 22 (so
/// the quoted glob of `node --test` is expanded by node), and there is one;
/// every upload step sets `if-no-files-found: error` (the action's default
/// only warns), and there is one.
fn setting_problems(text: &str) -> Vec<String> {
    on_tree(text, |doc| {
        let mut problems = Vec::new();
        let shell = scalar(&doc["defaults"]["run"]["shell"]);
        if shell.as_deref() != Some("bash") {
            problems.push(format!("defaults.run.shell is {shell:?}, not bash"));
        }
        let (mut versions, mut uploads) = (0, 0);
        for step in all_steps(doc) {
            let version = &step["with"]["node-version"];
            if !version.is_badvalue() {
                versions += 1;
                let version = scalar(version).unwrap_or_default();
                let major: String = version.chars().take_while(char::is_ascii_digit).collect();
                if !major.parse::<u32>().is_ok_and(|major| major >= 22) {
                    problems.push(format!("node-version is not 22 or newer: {version}"));
                }
            }
            let found = &step["with"]["if-no-files-found"];
            uploads += usize::from(uploads_artifact(step));
            if uploads_artifact(step) || !found.is_badvalue() {
                match scalar(found).as_deref() {
                    Some("error") => {}
                    Some(value) => {
                        problems.push(format!("if-no-files-found is {value}, not error"))
                    }
                    None => problems.push("an upload does not set if-no-files-found".to_owned()),
                }
            }
        }
        problems.extend((versions == 0).then(|| "no node-version".to_owned()));
        problems.extend((uploads == 0).then(|| "no upload-artifact step".to_owned()));
        problems
    })
}

// ---- the bundle build ----

/// The config file that adds the worker sidecar; only the bundle build reads it.
const BUNDLE_CONFIG: &str = "src-tauri/tauri.bundle.conf.json";

/// Whether the words of one command line pass `--config BUNDLE_CONFIG` (or
/// `--config=BUNDLE_CONFIG`).
fn passes_bundle_config(words: &[&str]) -> bool {
    let joined = format!("--config={BUNDLE_CONFIG}");
    words.contains(&joined.as_str())
        || words
            .windows(2)
            .any(|pair| pair == ["--config", BUNDLE_CONFIG])
}

/// The build job runs `cargo tauri build`, and every such line in it, outside
/// shell comments, passes the bundle config. Without it the installer ships
/// without the worker sidecar, and the app falls back to its own, slower
/// worker mode.
fn bundle_build_problems(text: &str) -> Vec<String> {
    on_tree(text, |doc| {
        let Some((_, build)) = jobs(doc).into_iter().find(|(name, _)| name == "build") else {
            return vec!["no build job".to_owned()];
        };
        let scripts = steps_of(build)
            .into_iter()
            .filter_map(|step| scalar(&step["run"]))
            .map(|script| without_comments(&script))
            .collect::<Vec<_>>();
        let lines = scripts.iter().flat_map(|script| script.lines());
        let words = lines.map(|line| {
            line.split_whitespace()
                .map(|word| word.trim_matches(|c| c == '\'' || c == '"'))
                .collect::<Vec<_>>()
        });
        let builds = words
            .filter(|words| words.windows(3).any(|w| w == ["cargo", "tauri", "build"]))
            .collect::<Vec<_>>();
        if builds.is_empty() {
            return vec!["the build job has no cargo tauri build".to_owned()];
        }
        let plain = builds.iter().filter(|words| !passes_bundle_config(words));
        plain
            .map(|words| {
                format!(
                    "a bundle build without --config {BUNDLE_CONFIG}: {}",
                    words.join(" ")
                )
            })
            .collect()
    })
}

const REQUIRED_COMMANDS: &[&str] = &[
    "cargo fmt --all --check",
    "cargo clippy --workspace --all-targets -- -D warnings",
    "cargo test --workspace",
    "node --test \"tests/ui/*.test.js\"",
    "cargo tauri build",
];

// ---- fixtures ----

const PIN: &str = "11bd71901bbe5b1630ceea73d27597364c9af683";

/// The build job's bundle step in `good()`.
const BUILD_STEP: &str =
    "      - run: cargo tauri build --config src-tauri/tauri.bundle.conf.json\n";

const UPLOAD_STEP: &str = "      - uses: actions/upload-artifact@PIN # v7
        with:
          path: target/release/bundle/nsis/*.exe
          if-no-files-found: error
";

/// A workflow every check accepts. It holds a `--release` build and a
/// `target/release` path, which are not release steps, and a cache in the
/// `test` job, which is allowed there. The variants below change one thing.
fn good() -> String {
    let base = "name: x
on:
  workflow_dispatch:
  push:
    tags:
      - 'v*'
permissions:
  contents: read
concurrency:
  group: ${{ github.workflow }}-${{ github.ref }}
defaults:
  run:
    shell: bash
jobs:
  test:
    runs-on: windows-latest
    steps:
      - uses: actions/checkout@PIN # v4
      - uses: actions/setup-node@PIN # v4
        with:
          node-version: 22
      - uses: Swatinem/rust-cache@PIN # v2
      - run: cargo fmt --all --check
      - run: cargo clippy --workspace --all-targets -- -D warnings
      - run: cargo test --workspace
      - run: cargo build --release
      - run: node --test \"tests/ui/*.test.js\"
  build:
    needs: test
    runs-on: windows-latest
    steps:
      - run: cargo tauri build --config src-tauri/tauri.bundle.conf.json
";
    format!("{base}{UPLOAD_STEP}").replace("PIN", PIN)
}

/// `good()` with the first `from` replaced by `to`. Fails when `from` is not
/// there, so a fixture cannot silently be a no-op.
fn with(from: &str, to: &str) -> String {
    let from = from.replace("PIN", PIN);
    assert!(good().contains(&from), "the base fixture lacks {from:?}");
    good().replacen(&from, &to.replace("PIN", PIN), 1)
}

/// `good()` with one more step at the end of the build job (six-space indent).
fn add_step(step: &str) -> String {
    format!("{}{}\n", good(), step.replace("PIN", PIN))
}

/// A one-job workflow around the given steps.
fn steps_doc(steps: &str) -> String {
    let head = "jobs:\n  a:\n    runs-on: windows-latest\n    steps:\n";
    format!("{head}{}\n", steps.replace("PIN", PIN))
}

fn every_check(text: &str) -> Vec<(&'static str, Vec<String>)> {
    vec![
        ("unpinned_actions", unpinned_actions(text)),
        ("permission_problems", permission_problems(text)),
        ("secret_references", secret_references(text)),
        ("trigger_problems", trigger_problems(text)),
        ("injection_problems", injection_problems(text)),
        (
            "missing_commands",
            missing_commands(text, REQUIRED_COMMANDS),
        ),
        ("structure_problems", structure_problems(text)),
        ("setting_problems", setting_problems(text)),
        ("bundle_build_problems", bundle_build_problems(text)),
    ]
}

fn assert_clean(problems: Vec<String>) {
    assert_eq!(problems, Vec::<String>::new());
}

/// A problem that starts with `start` is among `problems`.
fn assert_problem(problems: &[String], start: &str, what: &str) {
    let found = problems.iter().any(|problem| problem.starts_with(start));
    assert!(
        found,
        "{what}: no problem starting {start:?} in {problems:?}"
    );
}

/// `check` reports a problem that starts with `start` on every text.
fn assert_all_caught(check: fn(&str) -> Vec<String>, start: &str, texts: &[String]) {
    texts
        .iter()
        .for_each(|text| assert_problem(&check(text), start, text));
}

// ---- the real file ----

#[test]
fn the_workflow_parses_as_one_yaml_mapping_and_on_is_a_name() {
    let doc = parse(&workflow()).unwrap_or_else(|e| panic!("{e}"));
    let hash = doc.as_hash().expect("a mapping");
    // YAML 1.2: `on` is the name `on`. A YAML 1.1 reader makes it boolean true.
    assert!(hash.contains_key(&Yaml::String("on".to_owned())));
    assert!(!hash.keys().any(|key| matches!(key, Yaml::Boolean(_))));
    assert!(doc["on"].as_hash().is_some());
}

#[test]
fn the_workflow_passes_every_check() {
    for (name, problems) in every_check(&workflow()) {
        assert_eq!(problems, Vec::<String>::new(), "{name}");
    }
}

#[test]
fn the_checks_of_the_real_file_are_not_vacuous() {
    // Checkout, toolchain, cache, node and upload; two line-ending steps,
    // fmt, clippy, test, node, install and build; the installer folder.
    let (refs, scripts) = (
        action_references(&workflow()).len(),
        run_scripts(&workflow()).len(),
    );
    assert!(
        refs >= 5 && scripts >= 8,
        "{refs} action references, {scripts} run scripts"
    );
    assert!(workflow().contains("target/release/bundle/"));
}

// ---- parsing ----

#[test]
fn a_well_formed_workflow_passes_every_check_in_a_fixture() {
    for (name, problems) in every_check(&good()) {
        assert_eq!(problems, Vec::<String>::new(), "{name}");
    }
}

#[test]
fn a_workflow_that_does_not_parse_is_reported_by_every_check_in_a_fixture() {
    for bad in [
        "jobs: [unclosed\n",
        "",
        "a: 1\n---\nb: 2\n",
        "- a\n- b\n",
        "a: 1\na: 2\n",
    ] {
        for (name, problems) in every_check(bad) {
            assert_problem(&problems, "the workflow", &format!("{name} on {bad:?}"));
        }
    }
}

#[test]
fn a_true_key_is_not_read_as_on_in_a_fixture() {
    // The parser reads `on` as a name, and a real boolean key is not `on`.
    assert_eq!(parse("on: x\n").unwrap()["on"].as_str(), Some("x"));
    assert_eq!(
        trigger_problems("true:\n  workflow_dispatch:\n"),
        ["no on: block"]
    );
}

// ---- pinned actions ----

#[test]
#[rustfmt::skip]
fn an_action_pinned_to_a_tag_or_branch_is_caught_in_a_fixture() {
    let upper = PIN.to_uppercase();
    let bad = [
        "uses: actions/checkout@v4", "uses: actions/checkout@main", "uses: actions/checkout", "uses: actions/checkout@11bd719",
        "uses: ./local-action", "uses: docker://alpine:3.20",
        // 41 characters, and 40 with an upper-case digit.
        &format!("uses: actions/checkout@{PIN}0 # v4"), &format!("uses: actions/checkout@{upper} # v4"),
        // The same mistake in spellings a line scan would miss.
        " uses:   actions/checkout@v4", "uses: 'actions/checkout@v4'", "uses: \"actions/checkout@v4\"",
        "{ uses: actions/checkout@v4 }", "name: x\n        uses: actions/checkout@v4",
    ];
    for step in bad {
        let problems = unpinned_actions(&steps_doc(&format!("      - {step}")));
        assert_eq!(problems.len(), 1, "{step}: {problems:?}");
        assert_problem(&problems, "not pinned", step);
    }
    // A reusable workflow called by a job is an action reference too.
    assert_problem(&unpinned_actions("jobs:\n  a:\n    uses: org/repo/.github/workflows/x.yml@main\n"), "not pinned", "job");
}

#[test]
fn a_pinned_action_without_a_release_comment_is_caught_in_a_fixture() {
    for step in [
        format!("      - uses: actions/checkout@{PIN}"),
        format!("      - uses: actions/checkout@{PIN} #"),
    ] {
        assert_problem(
            &unpinned_actions(&steps_doc(&step)),
            "no release comment",
            &step,
        );
    }
    let job = format!("jobs:\n  a:\n    uses: org/repo/.github/workflows/x.yml@{PIN}\n");
    assert_problem(
        &unpinned_actions(&job),
        "no release comment",
        "job-level uses",
    );
}

#[test]
fn a_pin_used_twice_needs_a_comment_on_every_line_in_a_fixture() {
    let line = format!("      - uses: actions/checkout@{PIN}");
    assert_eq!(
        unpinned_actions(&steps_doc(&format!("{line} # v4\n{line}"))).len(),
        1
    );
    assert_clean(unpinned_actions(&steps_doc(&format!(
        "{line} # v4\n{line} # v4"
    ))));
}

#[test]
fn an_action_pinned_to_a_commit_with_a_release_comment_passes_a_fixture() {
    let pin = format!("actions/checkout@{PIN}");
    for ok in [
        format!("      - uses: {pin} # v4.2.2"),
        format!("      -   uses:   {pin}   # v4.2.2"),
        format!("      - uses: '{pin}' # v4.2.2"),
        format!("      - name: x\n        uses: {pin} # v4.2.2"),
        // `uses` inside `with` is an input of an action, not an action.
        "      - name: x\n        run: ls\n        with: { uses: a/b@v1 }".to_owned(),
    ] {
        assert_clean(unpinned_actions(&steps_doc(&ok)));
    }
    assert_clean(unpinned_actions(&format!(
        "jobs:\n  a:\n    uses: o/r/.github/workflows/x.yml@{PIN} # v1\n"
    )));
    // A comment line that mentions `uses:` is not a reference.
    assert_clean(unpinned_actions(&format!(
        "# uses: {pin}@v4 is bad\n{}",
        steps_doc("      - run: ls")
    )));
}

#[test]
#[rustfmt::skip]
fn action_references_are_read_from_the_tree_in_a_fixture() {
    let steps = "      - uses: a/b@v1\n      -  uses: c/d@v1\n      - {uses: e/f@v1}\n      - uses: 'g/h@v1'\n      - name: x\n        with: { uses: i/j@v1 }";
    let text = format!("{}# uses: k/l@v1\n", steps_doc(steps));
    assert_eq!(action_references(&text), ["a/b@v1", "c/d@v1", "e/f@v1", "g/h@v1"]);
}

// ---- permissions ----

#[test]
#[rustfmt::skip]
fn a_write_permission_is_caught_in_a_fixture() {
    let top = |block: &str| permission_problems(&format!("permissions:{block}\njobs:\n  a: {{}}\n"));
    assert_problem(&top("\n  contents: write"), "write permission", "write");
    assert_problem(&top("\n  contents: write"), "top-level permissions", "write");
    assert_eq!(top("\n  contents: read\n  id-token: write").len(), 2);
    assert_problem(&top(" write-all"), "write permission", "write-all");
    assert_eq!(top("\n  contents: read\n  packages: read").len(), 1, "wider than read");
    assert_eq!(permission_problems("name: x\njobs:\n  a: {}\n"), ["no top-level permissions block"]);
    // Read-only at the top, but a job asks for more.
    let job = |block: &str| permission_problems(&format!("permissions:\n  contents: read\njobs:\n  a:\n    permissions:{block}\n"));
    assert_eq!(job("\n      contents: write").len(), 1);
    assert_eq!(job(" write-all").len(), 1);
}

#[test]
#[rustfmt::skip]
fn a_quoted_write_permission_is_caught_in_a_fixture() {
    let job = "permissions:\n  contents: read\njobs:\n  a:\n    permissions:";
    for value in ["'write'", "\"write\"", "  write", "'write-all'", "Write", "{ contents: 'write' }"] {
        let top = format!("permissions:\n  contents: {value}\njobs:\n  a: {{}}\n");
        let in_job = format!("{job}\n      contents: {value}\n");
        let flow = format!("{job} {{contents: {value}}}\n");
        for text in [top, in_job, flow] {
            assert_problem(&permission_problems(&text), "write permission", &text);
        }
    }
    for value in ["'write-all'", "\"write-all\""] {
        assert_problem(&permission_problems(&format!("{job} {value}\n")), "write permission", value);
    }
}

#[test]
fn read_only_contents_permission_passes_a_fixture() {
    for read in [
        "read",
        "'read'",
        "\"read\"",
        "  read",
        "read # nothing else",
    ] {
        assert_clean(permission_problems(&format!(
            "name: x\n\npermissions:\n  contents: {read}\n\njobs:\n  a: {{}}\n"
        )));
    }
    assert_clean(permission_problems(
        "permissions:\n  contents: read\njobs:\n  a:\n    permissions:\n      contents: 'read'\n",
    ));
}

// ---- secrets and tokens ----

#[test]
#[rustfmt::skip]
fn a_secret_reference_is_caught_in_a_fixture() {
    for value in ["${{ secrets.GITHUB_TOKEN }}", "${{secrets.NPM}}", "${{ secrets['NPM'] }}", "${{ toJSON(secrets) }}", "${{ SECRETS.NPM }}"] {
        let step = format!("      - run: ls\n        env:\n          P: {value}");
        assert_eq!(secret_references(&steps_doc(&step)).len(), 1, "{value}");
    }
    let inherit = "jobs:\n  a:\n    uses: o/r/.github/workflows/x.yml@main\n    secrets: inherit\n";
    assert_eq!(secret_references(inherit).len(), 1);
}

#[test]
#[rustfmt::skip]
fn a_token_reference_is_caught_in_a_fixture() {
    for bad in [
        "      - run: ls\n        with:\n          token: ${{ github.token }}",
        "      - run: ls\n        env:\n          GITHUB_TOKEN: abc",
        "      - run: ls\n        env:\n          GitHub_Token: abc",
        "      - run: echo $GITHUB_TOKEN",
        // A token handed to an action as an input.
        "      - uses: a/b@PIN # v1\n        with:\n          github-token: x",
        "      - uses: a/b@PIN # v1\n        with:\n          'github-token': x",
    ] {
        assert_eq!(secret_references(&steps_doc(bad)).len(), 1, "{bad}");
    }
}

#[test]
#[rustfmt::skip]
fn a_workflow_without_secret_words_passes_a_fixture() {
    let steps = "      - run: ls # secrets.X\n      - name: mysecretsauce\n        run: echo secrets_manager\n        env:\n          NAME: github-actions";
    assert_clean(secret_references(&format!("# no secrets.TOKEN here\n{}", steps_doc(steps))));
}

// ---- triggers ----

#[test]
#[rustfmt::skip]
fn a_pull_request_trigger_or_a_branch_push_is_caught_in_a_fixture() {
    let tags = "    tags:\n      - 'v*'\n";
    let events = |on: &str| trigger_problems(&format!("on:\n{on}"));
    assert_problem(&events("  pull_request_target:\n    types: [opened]\n"), "triggers are", "target");
    let pr = events(&format!("  workflow_dispatch:\n  pull_request:\n  push:\n{tags}"));
    assert_eq!(pr.len(), 1);
    assert_problem(&pr, "triggers are", "pull_request");
    for filter in ["branches:\n      - main", "tags-ignore:\n      - 'x*'", "paths:\n      - 'a/**'"] {
        let problems = events(&format!("  workflow_dispatch:\n  push:\n    {filter}\n{tags}"));
        assert_eq!(problems.len(), 1, "{filter}");
        assert_problem(&problems, "push is filtered by", filter);
    }
    assert_problem(&events("  workflow_dispatch:\n  push:\n    tags: ['v*', '*']\n"), "push tags are", "extra tag");
    assert_problem(&events("  workflow_dispatch:\n  push:\n"), "push tags are", "no filter");
    assert_problem(&events(&format!("  push:\n{tags}")), "triggers are", "push only");
    assert_eq!(trigger_problems("jobs:\n  a: {}\n"), ["no on: block"]);
    // The shorter spellings of `on`.
    assert_problem(&trigger_problems("on: pull_request\n"), "triggers are", "scalar");
    assert_problem(&trigger_problems("on: [push, pull_request]\n"), "triggers are", "list");
}

#[test]
#[rustfmt::skip]
fn manual_start_and_a_version_tag_pass_a_fixture() {
    for ok in [
        "on:\n  workflow_dispatch:\n  push:\n    tags:\n      - 'v*'\n",
        "on:\n  push:\n    tags:\n      - \"v*\"\n  workflow_dispatch:\n",
        "on:\n  workflow_dispatch:\n  push:\n    tags: ['v*']\n",
        "on: { push: { tags: ['v*'] }, workflow_dispatch: {} }\n",
    ] {
        assert_clean(trigger_problems(ok));
    }
}

// ---- run scripts ----

#[test]
fn block_scripts_of_every_style_are_read_in_a_fixture() {
    let steps = "      - run: echo a
      - run: |-
          b

          c
      - run: >
          d
          e
      - run: >-
          f
      - run: |+
          g
      - run: |
          h
        shell: bash
      - name: n
        run: |
          i
        env:
          X: y
      -  run: \"quoted\"
      - run: 'single'";
    let expected = [
        "echo a", "b\n\nc", "d e\n", "f", "g\n", "h\n", "i\n", "quoted", "single",
    ];
    assert_eq!(run_scripts(&steps_doc(steps)), expected);
}

#[test]
fn a_hash_after_a_space_in_a_plain_run_is_a_yaml_comment_in_a_fixture() {
    // YAML ends a plain value at ` #`, so the runner never sees the rest, and
    // neither does the parser: what it reads is what would run. The same text
    // in quotes or in a block is read whole (see the release fixtures).
    let plain = steps_doc("      - run: echo \"a #b\"; gh release create v1");
    assert_eq!(run_scripts(&plain), ["echo \"a"]);
    let quoted = steps_doc("      - run: 'echo \"a #b\"; gh release create v1'");
    assert_eq!(
        run_scripts(&quoted),
        ["echo \"a #b\"; gh release create v1"]
    );
}

#[test]
#[rustfmt::skip]
fn an_expression_in_a_script_is_caught_in_a_fixture() {
    let inline = injection_problems(&steps_doc("      - run: echo ${{ github.event.pull_request.title }}"));
    assert_problem(&inline, "expression in a run script", "inline");
    assert_problem(&inline, "github.event used", "inline");
    for step in [
        "      - name: x\n        run: |\n          echo start\n          echo \"${{ github.ref_name }}\"\n      - run: ls",
        // The whole string is scanned, a shell comment inside it included.
        "      - run: |\n          ls\n          # ${{ github.ref_name }}",
        "      - run: \"echo ${{ github.ref_name }}\"",
    ] {
        assert_eq!(injection_problems(&steps_doc(step)).len(), 1, "{step}");
    }
    // Not a run script, so only the github.event rule applies.
    let elsewhere = "      - run: ls\n      - uses: a/b@x\n        with:\n          name: ${{ github.event.head_commit.message }}";
    let problems = injection_problems(&steps_doc(elsewhere));
    assert_eq!(problems.len(), 1);
    assert_problem(&problems, "github.event used", "with");
}

#[test]
#[rustfmt::skip]
fn a_script_without_expressions_passes_a_fixture() {
    let steps = "      - run: cargo test --workspace\n      - name: x\n        run: |\n          node --test \"tests/ui/*.test.js\"\n      - uses: a/b@x\n        with:\n          name: ${{ runner.os }}-bundle";
    assert_clean(injection_problems(&steps_doc(steps)));
}

#[test]
fn a_workflow_without_run_scripts_is_reported_in_a_fixture() {
    let none = steps_doc("      - uses: a/b@x");
    assert_eq!(run_scripts(&none).len(), 0);
    assert_eq!(injection_problems(&none), ["no run scripts found"]);
    // With nothing required, a workflow without scripts is still reported.
    assert_eq!(missing_commands(&none, &[]), ["no run scripts found"]);
    let required = missing_commands(&none, &["cargo test --workspace"]);
    assert_eq!(
        required,
        ["no run scripts found", "missing: cargo test --workspace"]
    );
}

#[test]
#[rustfmt::skip]
fn a_missing_command_is_caught_in_a_fixture() {
    let wanted = ["cargo test --workspace", "cargo fmt --all --check"];
    let only_comment = format!("# cargo test --workspace\n{}", steps_doc("      - run: cargo fmt --all --check"));
    assert_eq!(missing_commands(&only_comment, &wanted), ["missing: cargo test --workspace"]);
    // A command that only appears in a `name:` is not run.
    let named = steps_doc("      - name: cargo test --workspace\n        run: echo hello");
    assert_eq!(missing_commands(&named, &wanted[..1]), ["missing: cargo test --workspace"]);
}

#[test]
#[rustfmt::skip]
fn commands_in_one_line_and_block_scripts_are_found_in_a_fixture() {
    let steps = "      - run: cargo test --workspace\n      - name: x\n        shell: bash\n        run: |\n          node --test \"tests/ui/*.test.js\"";
    assert_clean(missing_commands(&steps_doc(steps), &["cargo test --workspace", "node --test \"tests/ui/*.test.js\""]));
}

#[test]
#[rustfmt::skip]
fn a_command_only_in_a_comment_inside_a_block_script_is_not_present_in_a_fixture() {
    let steps = "      - run: |\n          # cargo test --workspace\n          cargo fmt --all --check # cargo clippy";
    let wanted = ["cargo fmt --all --check", "cargo test --workspace", "cargo clippy"];
    let missing = ["missing: cargo test --workspace", "missing: cargo clippy"];
    assert_eq!(missing_commands(&steps_doc(steps), &wanted), missing);
}

// ---- structure ----

#[test]
#[rustfmt::skip]
fn a_runner_that_is_not_windows_latest_is_caught_in_a_fixture() {
    let one = structure_problems(&with("runs-on: windows-latest", "runs-on: ubuntu-latest"));
    assert_eq!(one.len(), 1, "{one:?}");
    assert_problem(&one, "job test: runner is not windows-latest", "ubuntu");
    assert_eq!(structure_problems(&good().replace("windows-latest", "ubuntu-latest")).len(), 2);
    let list = structure_problems(&with("runs-on: windows-latest", "runs-on: [windows-latest]"));
    assert_problem(&list, "job test: runs-on is not", "list");
}

#[test]
#[rustfmt::skip]
fn a_job_without_runs_on_is_caught_in_a_fixture() {
    // Checked per job: the other job still has its runner.
    let one = with("  test:\n    runs-on: windows-latest\n", "  test:\n");
    assert_eq!(structure_problems(&one), ["job test: no runs-on"]);
    assert_eq!(structure_problems(&good().replace("    runs-on: windows-latest\n", "")).len(), 2);
}

#[test]
fn a_build_job_without_needs_test_is_caught_in_a_fixture() {
    let missing = ["the build job does not have needs: test"];
    assert_eq!(structure_problems(&with("    needs: test\n", "")), missing);
}

#[test]
fn needs_test_on_the_wrong_job_is_caught_in_a_fixture() {
    // `needs: test` is on some line, but the build job does not have it.
    let text = with("    needs: test\n", "").replacen("  test:\n", "  test:\n    needs: test\n", 1);
    assert!(text.contains("needs: test"));
    assert_eq!(
        structure_problems(&text),
        ["the build job does not have needs: test"]
    );
    // A list that holds the job is fine, a list without it is not.
    assert_clean(structure_problems(&with("needs: test", "needs: [test]")));
    assert_eq!(
        structure_problems(&with("needs: test", "needs: [lint]")).len(),
        1
    );
}

#[test]
fn a_renamed_or_missing_test_job_is_caught_in_a_fixture() {
    let renamed = good()
        .replace("  test:\n", "  checks:\n")
        .replace("needs: test", "needs: checks");
    assert_problem(&structure_problems(&renamed), "no test job", "renamed");
    let package = good().replace("  build:\n", "  package:\n");
    assert_problem(
        &structure_problems(&package),
        "no build job",
        "renamed build",
    );
}

#[test]
fn a_build_job_without_an_artifact_upload_is_caught_in_a_fixture() {
    let none = with("actions/upload-artifact@", "actions/download-artifact@");
    assert_eq!(structure_problems(&none).len(), 1);
    // An upload in the test job does not count for the build job.
    let build_line = "      - run: cargo build --release\n";
    let wrong_job =
        with(UPLOAD_STEP, "").replacen(build_line, &format!("{build_line}{UPLOAD_STEP}"), 1);
    assert_eq!(structure_problems(&wrong_job.replace("PIN", PIN)).len(), 1);
}

#[test]
#[rustfmt::skip]
fn a_missing_concurrency_or_a_group_without_the_workflow_name_is_caught_in_a_fixture() {
    let none = with("concurrency:\n  group: ${{ github.workflow }}-${{ github.ref }}\n", "");
    assert_eq!(structure_problems(&none), ["no concurrency"]);
    let group = with("${{ github.workflow }}-", "windows-");
    assert_problem(&structure_problems(&group), "the concurrency group", "group");
}

#[test]
#[rustfmt::skip]
fn a_build_job_that_restores_a_cache_is_caught_in_a_fixture() {
    let build = BUILD_STEP;
    for cache in ["Swatinem/rust-cache@PIN # v2", "actions/cache@PIN # v4", "actions/cache/restore@v4", "actions/cache/save@PIN # v4"] {
        let text = with(build, &format!("{build}      - uses: {cache}\n"));
        assert_problem(&structure_problems(&text), "the build job restores a cache", cache);
    }
}

#[test]
#[rustfmt::skip]
fn a_release_flag_and_a_release_path_in_a_run_script_pass_a_fixture() {
    for ok in [
        "      - run: cargo build --release", "      - run: cargo tauri build --release", "      - run: 'cargo build \"--release\"'",
        "      - run: ./target/release/lunaepix.exe --version", "      - run: ls \"target/release/bundle\"",
    ] {
        assert_clean(structure_problems(&add_step(ok)));
    }
}

#[test]
#[rustfmt::skip]
fn a_release_word_that_is_not_the_flag_or_the_path_is_caught_in_a_fixture() {
    let bad = [
        "      - run: cargo build --release-notes", "      - run: cargo build --RELEASE", "      - run: curl x/target/release/y",
        "      - run: echo target/release/;gh release create v1", "      - run: gh release create v1 --release",
        "      - run: ls target/release/x; curl $U/releases",
    ];
    assert_all_caught(structure_problems, "release step", &bad.map(add_step));
}

#[test]
#[rustfmt::skip]
fn a_release_step_is_caught_in_a_fixture() {
    let steps = [
        "      - uses: softprops/action-gh-release@PIN # v2", "      - uses: actions/create-release@PIN # v1",
        "      - uses: ncipollo/release-action@PIN # v1", "      - uses: actions/upload-release-asset@PIN # v1",
        // A release action with the word in the middle of the path.
        "      - uses: acme/release/pub@PIN # v1", "      - uses: tauri-apps/tauri-action@PIN # v0",
        "      - run: gh release create v1 --notes x", "      -  run: gh release create v1", "      - run: \"gh release create v1\"",
        "      - run: 'echo \"a #b\"; gh release create v1'",
        "      - run: |\n          echo start\n          echo \"a #b\"; gh release upload v1 a.exe",
        "      - run: |\n          # gh release create v1",
        "      - run: gh api repos/o/r/releases -f tag_name=v1", "      - run: curl -X POST https://example.com/releases",
    ];
    assert_all_caught(structure_problems, "release step", &steps.map(add_step));
    // A job that calls a reusable release workflow.
    let job = format!("{}  publish:\n    uses: org/repo/.github/workflows/release.yml@{PIN} # v1\n", good());
    assert_problem(&structure_problems(&job), "release step", "reusable workflow");
}

#[test]
#[rustfmt::skip]
fn a_release_id_input_is_caught_in_a_fixture() {
    let bad = [
        "      - uses: a/b@PIN # v1\n        with:\n          releaseId: 1",
        "      - run: ls\n        env:\n          RELEASE_ID: 1",
        "      - run: echo $releaseId",
    ];
    assert_all_caught(structure_problems, "release step", &bad.map(add_step));
}

// ---- settings ----

#[test]
fn settings_that_are_right_pass_a_fixture() {
    // Quoting does not change a value.
    for version in ["'24.1'", "\"22\"", "'22'", "22"] {
        assert_clean(setting_problems(&with(
            "node-version: 22",
            &format!("node-version: {version}"),
        )));
    }
    assert_clean(setting_problems(&with(
        "if-no-files-found: error",
        "if-no-files-found: 'error'",
    )));
}

#[test]
#[rustfmt::skip]
fn a_shell_other_than_bash_or_no_defaults_is_caught_in_a_fixture() {
    assert_eq!(setting_problems(&with("shell: bash", "shell: pwsh")).len(), 1);
    assert_eq!(setting_problems(&with("defaults:\n  run:\n    shell: bash\n", "")).len(), 1);
    assert_eq!(setting_problems(&with("  run:\n    shell: bash\n", "  run: {}\n")).len(), 1);
}

#[test]
#[rustfmt::skip]
fn an_old_or_unreadable_node_version_is_caught_in_a_fixture() {
    for bad in ["20", "'21.9'", "'lts/*'", "latest"] {
        assert_eq!(setting_problems(&with("node-version: 22", &format!("node-version: {bad}"))).len(), 1, "{bad}");
    }
    assert_eq!(setting_problems(&with("        with:\n          node-version: 22\n", "")), ["no node-version"]);
}

#[test]
#[rustfmt::skip]
fn an_upload_that_ignores_missing_files_is_caught_in_a_fixture() {
    for value in ["warn", "ignore", "'warn'"] {
        let text = with("if-no-files-found: error", &format!("if-no-files-found: {value}"));
        assert_eq!(setting_problems(&text).len(), 1, "{value}");
    }
    // The action's default is to warn, so an upload that sets nothing is wrong.
    assert_eq!(setting_problems(&with("          if-no-files-found: error\n", "")).len(), 1);
    assert_eq!(setting_problems(&with(UPLOAD_STEP, "")).len(), 1);
}

// ---- the bundle build ----

#[test]
#[rustfmt::skip]
fn a_bundle_build_with_the_config_file_passes_a_fixture() {
    assert_clean(bundle_build_problems(&good()));
    for step in [
        "      - run: cargo tauri build --config=src-tauri/tauri.bundle.conf.json\n",
        "      - run: cargo tauri build --verbose --config 'src-tauri/tauri.bundle.conf.json'\n",
        "      - run: |\n          cargo --version\n          cargo tauri build --config \"src-tauri/tauri.bundle.conf.json\"\n",
    ] {
        assert_clean(bundle_build_problems(&with(BUILD_STEP, step)));
    }
}

#[test]
#[rustfmt::skip]
fn a_bundle_build_without_the_config_file_is_caught_in_a_fixture() {
    let plain = "      - run: cargo tauri build\n";
    let without = "a bundle build without --config";
    let test_step = "      - run: cargo test --workspace\n";
    let texts = [
        // The plain build.
        with(BUILD_STEP, plain),
        // The flag only in a shell comment.
        with(BUILD_STEP, "      - run: |\n          cargo tauri build\n          # --config src-tauri/tauri.bundle.conf.json\n"),
        with(BUILD_STEP, "      - run: |\n          cargo tauri build # --config src-tauri/tauri.bundle.conf.json\n"),
        // Another file, or the flag without its file.
        with(BUILD_STEP, "      - run: cargo tauri build --config src-tauri/tauri.conf.json\n"),
        with(BUILD_STEP, "      - run: cargo tauri build --config\n"),
        // The flag only in the test job.
        with(BUILD_STEP, plain).replacen(test_step, &format!("{test_step}{BUILD_STEP}"), 1),
        // A second, plain build after the right one.
        add_step("      - run: cargo tauri build"),
    ];
    assert_all_caught(bundle_build_problems, without, &texts);
    // No bundle build in the build job at all, and no build job.
    let none = with(BUILD_STEP, "      - run: cargo build --release\n");
    assert_problem(&bundle_build_problems(&none), "the build job has no cargo tauri build", &none);
    let no_build = good().replacen("  build:\n", "  installer:\n", 1);
    assert_problem(&bundle_build_problems(&no_build), "no build job", &no_build);
}
