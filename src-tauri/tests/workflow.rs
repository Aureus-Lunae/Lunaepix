//! Guards on the build workflows: `.github/workflows/windows.yml` and, from
//! the Mac build on, `.github/workflows/macos.yml`. They cannot be run here,
//! and they build the installers people download, so the rules that keep them
//! safe are checked in a test: every action pinned to a commit, read-only
//! permissions, no secrets, no way to start one from a pull request, the test
//! and build commands present, NASM installed before cargo runs on Windows
//! and absent from the Mac file (Apple Silicon needs none), and the uploaded
//! artifact named, filled and kept as agreed. The rules are the same for
//! both; what differs per platform (the file, the runner, the cache, NASM and
//! Rust-test policies, the artifact, the Formula job) is one `Platform`
//! profile that every check reads. The Mac cache steps save only from `main`,
//! and its Rust tests run after a failed lint; Windows has neither rule. The
//! Mac file has two rules more. A check step between the build and the upload
//! verifies the app's signature and that the worker is sealed in, in
//! `bundle/macos/` and inside the disk image. A Formula job builds this commit
//! with the Homebrew formula: it needs the tests, has 60 minutes, uses no cache
//! and installs nothing the formula does not declare, and runs `brew audit`,
//! `brew style`, `git archive` of `HEAD`, `brew install --build-from-source`
//! and `brew test`, in that order; the Windows file must have no such job. The
//! shared rules (pinned actions, runner, read-only token, no expression in a
//! script, no release, no NASM on the Mac) read every job, the Formula job
//! included. Each check has fixtures it must catch, so it is known to fail
//! when it should.
//!
//! The workflow is parsed as YAML 1.2 (so the key `on` is a name, not
//! boolean true) and every check reads the parsed tree, so quoting, spacing
//! and flow or block style cannot hide anything. Only a `run:` string is
//! scanned as text (and a `uses` for the word `nasm`), and then, for the
//! checks that look for a command, all of it, shell comments included: a
//! false hit from a comment is accepted, a missed release is not. The checks
//! that decide whether a command is present read the script without its shell
//! comments instead: they can lose text, which makes them fail, never pass.
//! The Mac rule that no NASM is installed, and the Formula job's rule that
//! nothing but the formula is installed, are the other way round (losing text
//! could make them pass), so they drop only whole-line comments, and a
//! trailing `# nasm` or `# brew install x` is a false hit they accept. They
//! are lints, not proofs.

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use yaml_rust2::{Yaml, YamlLoader};

/// Whether the Installer job restores a build cache.
enum BuildCache {
    /// No cache: the test job's cache is a debug-profile cache, which a
    /// release build cannot use, and one of its own would only add upload time.
    Forbidden,
    /// Its own `Swatinem/rust-cache`, before its first cargo step: Mac minutes
    /// cost more than the cache upload, and the cache also keeps the installed
    /// Tauri command line.
    Required,
}

/// How a job gets NASM, which mozjpeg's SIMD code is assembled with.
enum NasmInstall {
    /// `choco install nasm --version=<x.y.z>`, with NASM's folder added to
    /// `GITHUB_PATH` (Chocolatey does not put it on the path).
    ChocolateyPinned,
    /// No NASM anywhere in the workflow. On Apple Silicon mozjpeg-sys
    /// assembles its Arm SIMD code with clang (`gas_supported`, `build_gas`
    /// in its `build.rs`); only x86 and x86_64 need NASM
    /// (`nasm_needed_for_arch`). A step that installs NASM (`brew install
    /// nasm`, `choco install nasm`, `apt-get install nasm`, an action such as
    /// `setup-nasm`) or runs it (`nasm -v`) is refused.
    NotNeeded,
}

/// What differs between the platforms' workflow files. Every check reads the
/// profile it is given, so the same rules guard each file. The one rule only
/// the Mac has, its check step, is added by `every_check` for `MACOS.file`.
struct Platform {
    /// The file in `.github/workflows/`.
    file: &'static str,
    /// The exact `runs-on` of every job.
    runner: &'static str,
    build_cache: BuildCache,
    nasm: NasmInstall,
    /// The `save-if` every `Swatinem/rust-cache` step must carry, as the
    /// expression text; `None`: no requirement.
    cache_save_if: Option<&'static str>,
    /// Whether the step that runs `cargo test` must carry
    /// `if: success() || failure()`, so it still runs after a failed
    /// formatting or lint step and one run reports every failure.
    rust_tests_after_failure: bool,
    /// The `name` of the uploaded artifact.
    artifact: &'static str,
    /// The `path` lines of the upload, in any order.
    artifact_paths: &'static [&'static str],
    /// Whether the workflow has the job `formula`, which builds this commit
    /// with the Homebrew formula and tests what it installed
    /// (`formula_job_problems` lists what it must hold). `false`: no job may
    /// be named `formula`.
    formula_job: bool,
}

/// The only `save-if` the Mac cache steps may have: rc tags cannot restore
/// each other's cache, only `main`'s, so only `main` saves one.
const SAVE_ONLY_FROM_MAIN: &str = "${{ github.ref == 'refs/heads/main' }}";

const WINDOWS: Platform = Platform {
    file: "windows.yml",
    runner: "windows-latest",
    build_cache: BuildCache::Forbidden,
    nasm: NasmInstall::ChocolateyPinned,
    cache_save_if: None,
    rust_tests_after_failure: false,
    artifact: "lunaepix-windows-installer",
    artifact_paths: &[
        "target/release/bundle/nsis/*.exe",
        "target/release/bundle/msi/*.msi",
    ],
    formula_job: false,
};

/// Apple Silicon (`macos-latest`); the artifact is the disk image.
const MACOS: Platform = Platform {
    file: "macos.yml",
    runner: "macos-latest",
    build_cache: BuildCache::Required,
    nasm: NasmInstall::NotNeeded,
    cache_save_if: Some(SAVE_ONLY_FROM_MAIN),
    rust_tests_after_failure: true,
    artifact: "lunaepix-macos-dmg",
    artifact_paths: &["target/release/bundle/dmg/*.dmg"],
    formula_job: true,
};

fn workflow(platform: &Platform) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../.github/workflows")
        .join(platform.file);
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
/// Used to decide whether a command is present, where losing text can make a
/// check fail but never pass, and where a step's first cargo command is
/// (`nasm_problems`, the `Required` cache rule): there a `cargo` after a ` #`
/// inside quotes is lost, so that step does not count as a cargo step.
fn without_comments(script: &str) -> String {
    let code = script
        .lines()
        .filter(|line| !line.trim_start().starts_with('#'));
    code.map(|line| line.split(" #").next().unwrap_or(line))
        .collect::<Vec<_>>()
        .join("\n")
}

/// A step's `run` script without its shell comments; empty for a step that
/// runs no script.
fn step_code(step: &Yaml) -> String {
    scalar(&step["run"]).map_or_else(String::new, |script| without_comments(&script))
}

/// Whether a script (comments already removed) runs cargo: it has the word
/// `cargo`.
fn runs_cargo(code: &str) -> bool {
    code.split_whitespace().any(|word| word == "cargo")
}

/// Whether a script (comments already removed) runs `cargo test`: the words
/// `cargo` and `test` next to each other, on any line.
fn runs_cargo_test(code: &str) -> bool {
    code.lines().any(|line| {
        let words = line.split_whitespace().collect::<Vec<_>>();
        words.windows(2).any(|pair| pair == ["cargo", "test"])
    })
}

/// Whether a step is the `Swatinem/rust-cache` action, however its name is cased.
fn is_rust_cache(step: &Yaml) -> bool {
    uses_of(step).is_some_and(|uses| {
        uses.to_ascii_lowercase()
            .starts_with("swatinem/rust-cache@")
    })
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

/// Every job has a `runs-on` that is the platform's runner; the jobs `test`
/// and `build` exist; `build` needs `test`, uploads with
/// `actions/upload-artifact@` (what it uploads is `upload_problems`' business)
/// and follows the platform's `BuildCache` (`Forbidden`: no action with
/// `cache` in its name; `Required`: a `Swatinem/rust-cache@` step before its
/// first cargo step); runs are queued per workflow and ref; and no key or
/// value anywhere (`uses` and `run` included) names a release.
fn structure_problems(text: &str, platform: &Platform) -> Vec<String> {
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
                Some(runner) if runner != platform.runner => problems.push(format!(
                    "job {name}: runner is not {}: {runner}",
                    platform.runner
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
            match platform.build_cache {
                BuildCache::Forbidden => {
                    let caches = steps_of(build)
                        .into_iter()
                        .filter_map(uses_of)
                        .filter(|uses| uses.to_ascii_lowercase().contains("cache"));
                    problems.extend(
                        caches.map(|uses| format!("the build job restores a cache: {uses}")),
                    );
                }
                BuildCache::Required => {
                    let steps = steps_of(build);
                    let cache = steps.iter().position(|step| is_rust_cache(step));
                    let cargo = steps.iter().position(|step| runs_cargo(&step_code(step)));
                    match (cache, cargo) {
                        (None, _) => problems
                            .push("the build job does not restore Swatinem/rust-cache".to_owned()),
                        (Some(cache), Some(cargo)) if cache > cargo => problems.push(
                            "the build job restores its cache after its first cargo step"
                                .to_owned(),
                        ),
                        (Some(_), _) => {}
                    }
                }
            }
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
/// the quoted glob of `node --test` is expanded by node), and there is one.
/// What an upload sets is `upload_problems`' business.
fn setting_problems(text: &str) -> Vec<String> {
    on_tree(text, |doc| {
        let mut problems = Vec::new();
        let shell = scalar(&doc["defaults"]["run"]["shell"]);
        if shell.as_deref() != Some("bash") {
            problems.push(format!("defaults.run.shell is {shell:?}, not bash"));
        }
        let mut versions = 0;
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
        }
        problems.extend((versions == 0).then(|| "no node-version".to_owned()));
        problems
    })
}

/// The days an uploaded artifact is kept; the action's default is 90.
const RETENTION_DAYS: &str = "30";

/// There is an upload step, and every one (in any job) matches the platform:
/// - `name` is the platform's artifact;
/// - `path` is a text whose trimmed, non-empty lines are exactly the platform's
///   `artifact_paths`, as a set (`str::lines` and `trim`, so order, blank
///   lines, trailing spaces, tabs and Windows line endings do not matter, and a
///   `!` exclusion line or a folded single line does);
/// - `if-no-files-found` is `error` (the action's default only warns);
/// - `retention-days` is 30. `scalar` turns the parsed value into text, so
///   every spelling that YAML 1.2 reads as the integer 30 (`30`, `030`, `+30`,
///   `0x1E`, `0o36`) and the string `"30"` are accepted; anything else
///   (`30.0`, the string `"030"`, a list, null, any other number) is refused.
///
/// Any other step that sets `if-no-files-found` must set it to `error` too.
/// One that is set but is not a single value (a list, null, a mapping) is
/// reported as such, apart from an upload that does not set it at all.
fn upload_problems(text: &str, platform: &Platform) -> Vec<String> {
    on_tree(text, |doc| {
        let mut problems = Vec::new();
        let mut uploads = 0;
        let wanted: BTreeSet<&str> = platform.artifact_paths.iter().copied().collect();
        for step in all_steps(doc) {
            let with = &step["with"];
            let found = &with["if-no-files-found"];
            let upload = uploads_artifact(step);
            uploads += usize::from(upload);
            if upload || !found.is_badvalue() {
                match scalar(found).as_deref() {
                    Some("error") => {}
                    Some(value) => {
                        problems.push(format!("if-no-files-found is {value}, not error"))
                    }
                    None if found.is_badvalue() => {
                        problems.push("an upload does not set if-no-files-found".to_owned())
                    }
                    None => problems.push(format!(
                        "if-no-files-found is not a single value: {found:?}"
                    )),
                }
            }
            if !upload {
                continue;
            }
            let (name, path, days) = (&with["name"], &with["path"], &with["retention-days"]);
            match scalar(name) {
                None if name.is_badvalue() => {
                    problems.push("an upload does not set a name".to_owned())
                }
                None => problems.push(format!("an upload's name is not a single value: {name:?}")),
                Some(name) if name != platform.artifact => problems.push(format!(
                    "an upload is named {name}, not {}",
                    platform.artifact
                )),
                Some(_) => {}
            }
            match scalar(path) {
                None if path.is_badvalue() => {
                    problems.push("an upload does not set a path".to_owned())
                }
                None => problems.push(format!(
                    "an upload's path is not a block of lines: {path:?}"
                )),
                Some(path) => {
                    let lines: BTreeSet<&str> = path
                        .lines()
                        .map(str::trim)
                        .filter(|line| !line.is_empty())
                        .collect();
                    if lines != wanted {
                        problems.push(format!("an upload's paths are {lines:?}, not {wanted:?}"));
                    }
                }
            }
            match scalar(days) {
                None if days.is_badvalue() => {
                    problems.push("an upload does not set retention-days".to_owned());
                }
                None => problems.push(format!(
                    "an upload's retention-days is not a number: {days:?}"
                )),
                Some(days) if days != RETENTION_DAYS => problems.push(format!(
                    "an upload's retention-days is {days}, not {RETENTION_DAYS}"
                )),
                Some(_) => {}
            }
        }
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

// ---- NASM ----

/// Whether the line of a script that runs `choco install nasm` carries a
/// `--version=<x.y.z>` pin (`NasmInstall::ChocolateyPinned`): the workflow
/// pins every action by hash, so the build tool is pinned too.
fn nasm_install_is_pinned(script: &str) -> bool {
    script
        .lines()
        .filter(|line| line.contains("choco install nasm"))
        .any(|line| {
            line.split_whitespace().any(|word| {
                word.strip_prefix("--version=")
                    .is_some_and(|v| !v.is_empty())
            })
        })
}

/// Whether a line of a script (whole-line comments removed) names NASM: a word
/// `nasm` in any case, where a word is a run of letters and digits and
/// underscores, so `brew install nasm`, `nasm -v`, `/opt/homebrew/bin/nasm`,
/// `nasm@2` and `NASM` all name it, and `nasmfoo`, `unasm` and `nasm_x` do not.
fn names_nasm(line: &str) -> bool {
    line.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .any(|word| word.eq_ignore_ascii_case("nasm"))
}

/// A script without its whole-line shell comments. A trailing ` # ...` is
/// kept: a ` #` inside quotes would otherwise hide what follows it, and for a
/// rule that wants something absent, losing text is the unsafe direction.
fn without_comment_lines(script: &str) -> String {
    let code = script
        .lines()
        .filter(|line| !line.trim_start().starts_with('#'));
    code.collect::<Vec<_>>().join("\n")
}

/// `NasmInstall::NotNeeded`: no step of any job installs or runs NASM. This
/// is a lint over the `run:` text without its whole-line shell comments (a
/// line that names the word `nasm`: `brew install nasm`, `choco install nasm`,
/// `apt-get install nasm`, `nasm -v`, a path ending in `/nasm`; a trailing
/// `# nasm` is read as code, a false hit this rule accepts) and over the
/// `uses:` of every step (an action whose name has `nasm` in it, such as
/// `setup-nasm`). It proves no such spelling is there, not that no other
/// route to NASM exists. One problem per step.
fn nasm_absent_problems(text: &str) -> Vec<String> {
    on_tree(text, |doc| {
        let mut problems = Vec::new();
        for (name, job) in jobs(doc) {
            for step in steps_of(job) {
                let uses = uses_of(step).filter(|uses| uses.to_ascii_lowercase().contains("nasm"));
                if let Some(uses) = uses {
                    problems.push(format!("job {name}: a step uses a nasm action: {uses}"));
                    continue;
                }
                let code = scalar(&step["run"])
                    .map_or_else(String::new, |script| without_comment_lines(&script));
                if let Some(line) = code.lines().find(|line| names_nasm(line)) {
                    problems.push(format!(
                        "job {name}: a step installs or runs nasm: {}",
                        line.trim()
                    ));
                }
            }
        }
        problems
    })
}

/// `NasmInstall::ChocolateyPinned`: every job with a cargo step installs NASM
/// in an earlier step, with `choco install nasm`: mozjpeg's x86 SIMD code is
/// assembled with it when lunaepix-core builds. The install step is found
/// outside shell comments. It also puts NASM's folder on the path through
/// `GITHUB_PATH`, and it carries a `--version=` pin. A cargo step is any step
/// whose script has the word `cargo` outside shell comments.
/// `NasmInstall::NotNeeded` is `nasm_absent_problems`' rule.
fn nasm_problems(text: &str, platform: &Platform) -> Vec<String> {
    if matches!(platform.nasm, NasmInstall::NotNeeded) {
        return nasm_absent_problems(text);
    }
    on_tree(text, |doc| {
        let mut problems = Vec::new();
        for (name, job) in jobs(doc) {
            let code = steps_of(job).into_iter().map(step_code).collect::<Vec<_>>();
            let Some(cargo) = code.iter().position(|script| runs_cargo(script)) else {
                continue;
            };
            let install = code
                .iter()
                .position(|script| script.contains("choco install nasm"));
            match install {
                None => problems.push(format!("job {name}: does not install nasm")),
                Some(at) if at > cargo => problems.push(format!(
                    "job {name}: installs nasm after its first cargo step"
                )),
                Some(at) => {
                    if !code[at].contains("GITHUB_PATH") {
                        problems.push(format!(
                            "job {name}: the nasm step does not add NASM to GITHUB_PATH"
                        ));
                    }
                    if !nasm_install_is_pinned(&code[at]) {
                        problems.push(format!(
                            "job {name}: the nasm install is not pinned with --version="
                        ));
                    }
                }
            }
        }
        problems
    })
}

/// When the platform has a `cache_save_if`: there is a `Swatinem/rust-cache`
/// step, and every one (in any job) has `with.save-if` equal to it, compared
/// as parsed text with the outer whitespace trimmed. `scalar` turns the parsed
/// value into text, so quoting (`'...'`, `"..."`) and a block scalar (which
/// ends in a newline) read the same. The inside of the expression is compared
/// as written: another spacing of the same expression, another ref, a
/// boolean, a list or a missing `with` are all refused, so a change to the
/// condition is a change to this rule, never a silent one.
fn cache_save_problems(text: &str, platform: &Platform) -> Vec<String> {
    on_tree(text, |doc| {
        // No requirement: the file is still parsed, so one that does not parse
        // is reported by every check.
        let Some(wanted) = platform.cache_save_if else {
            return Vec::new();
        };
        let mut problems = Vec::new();
        let mut caches = 0;
        for step in all_steps(doc)
            .into_iter()
            .filter(|step| is_rust_cache(step))
        {
            caches += 1;
            let save_if = &step["with"]["save-if"];
            match scalar(save_if) {
                Some(value) if value.trim() == wanted => {}
                Some(value) => problems.push(format!(
                    "a rust-cache step's save-if is {value:?}, not {wanted:?}"
                )),
                None if save_if.is_badvalue() => {
                    problems.push("a rust-cache step has no save-if".to_owned());
                }
                None => problems.push(format!(
                    "a rust-cache step's save-if is not a single value: {save_if:?}"
                )),
            }
        }
        problems.extend((caches == 0).then(|| "no rust-cache step to check".to_owned()));
        problems
    })
}

/// Whether a step has `if: success() || failure()`, exactly: the condition
/// that lets it run after an earlier step failed, but not after a cancel.
fn runs_after_failure(step: &Yaml) -> bool {
    scalar(&step["if"]).as_deref() == Some(AFTER_FAILURE)
}

/// When the platform has `rust_tests_after_failure`: there is a step that runs
/// `cargo test` (outside shell comments), and every such step has
/// `if: success() || failure()`, so it still runs after a failed formatting or
/// lint step and one run reports every failure (`node_step_problems` is the
/// same rule for the window script tests).
fn rust_test_problems(text: &str, platform: &Platform) -> Vec<String> {
    on_tree(text, |doc| {
        // No requirement: the file is still parsed, so one that does not parse
        // is reported by every check.
        if !platform.rust_tests_after_failure {
            return Vec::new();
        }
        let steps = all_steps(doc)
            .into_iter()
            .filter(|step| runs_cargo_test(&step_code(step)))
            .collect::<Vec<_>>();
        let mut problems = steps
            .iter()
            .filter(|step| !runs_after_failure(step))
            .map(|step| {
                format!(
                    "a cargo test step without `if: {AFTER_FAILURE}`: {:?}",
                    step["if"]
                )
            })
            .collect::<Vec<_>>();
        problems.extend(steps.is_empty().then(|| "no cargo test step".to_owned()));
        problems
    })
}

/// The condition that lets a step run after an earlier step failed, but not
/// after the run was cancelled.
const AFTER_FAILURE: &str = "success() || failure()";

/// Every step that runs the window script tests (`node --test` outside a
/// shell comment) must have `if: success() || failure()`, so it still runs
/// when the Rust tests before it failed and one run reports both.
fn node_step_problems(text: &str) -> Vec<String> {
    on_tree(text, |doc| {
        all_steps(doc)
            .into_iter()
            .filter(|step| {
                scalar(&step["run"])
                    .is_some_and(|run| without_comments(&run).contains("node --test"))
            })
            .filter(|step| !runs_after_failure(step))
            .map(|step| {
                format!(
                    "a node test step without `if: {AFTER_FAILURE}`: {:?}",
                    step["if"]
                )
            })
            .collect()
    })
}

// ---- the Mac check step ----

/// The command that makes a step the check step.
const CODESIGN_VERIFY: &str = "codesign --verify --deep --strict";

/// Mac only: the build job has a check step between the bundle build and the
/// upload, so a broken app or disk image fails the run before anyone downloads
/// it. The check step is the first step of the build job whose script runs
/// `codesign --verify --deep --strict` outside shell comments (the seal over
/// the app and its worker). It must also run `plutil -extract` (the worker is
/// listed in the seal) and `hdiutil attach` (the app inside the disk image is
/// checked too), come after a step that runs `cargo tauri build`, and come
/// before the first upload. Like the other script checks this is a lint on the
/// `run:` text: it proves the commands are there and in order, not what the
/// script does with their results.
fn mac_check_step_problems(text: &str) -> Vec<String> {
    on_tree(text, |doc| {
        let Some((_, build)) = jobs(doc).into_iter().find(|(name, _)| name == "build") else {
            return vec!["no build job".to_owned()];
        };
        let steps = steps_of(build);
        let code = steps.iter().map(|step| step_code(step)).collect::<Vec<_>>();
        let Some(check) = code
            .iter()
            .position(|script| script.contains(CODESIGN_VERIFY))
        else {
            return vec![format!(
                "no check step: no step of the build job runs {CODESIGN_VERIFY}"
            )];
        };
        let mut problems = Vec::new();
        for command in ["plutil -extract", "hdiutil attach"] {
            if !code[check].contains(command) {
                problems.push(format!("the check step does not run {command}"));
            }
        }
        let bundles = |script: &String| {
            script.lines().any(|line| {
                let words = line.split_whitespace().collect::<Vec<_>>();
                words.windows(3).any(|w| w == ["cargo", "tauri", "build"])
            })
        };
        if !code[..check].iter().any(bundles) {
            problems.push("the check step does not come after a cargo tauri build".to_owned());
        }
        let upload = steps.iter().position(|step| uploads_artifact(step));
        if !upload.is_some_and(|upload| upload > check) {
            problems.push("the check step does not come before the upload".to_owned());
        }
        problems
    })
}

// ---- the Formula job ----

/// The commands the Formula job runs, in this order: the committed formula is
/// audited and style-checked as it is, then this commit is archived the way
/// GitHub archives a tag, built through a copy of the formula, and tested.
const FORMULA_COMMANDS: &[&str] = &[
    "brew audit",
    "brew style",
    "git archive",
    "brew install --build-from-source",
    "brew test",
];

/// The `timeout-minutes` of the Formula job.
const FORMULA_TIMEOUT: &str = "60";

/// Whether the words of one shell command (quotes trimmed) are the one install
/// the Formula job may run: `brew install` with `--build-from-source`, any
/// other flags, and exactly one name, the tap's formula (`…/lunaepix`).
fn is_formula_install(words: &[&str]) -> bool {
    let Some(rest) = words.strip_prefix(&["brew", "install"][..]) else {
        return false;
    };
    let names = rest
        .iter()
        .filter(|word| !word.starts_with('-'))
        .collect::<Vec<_>>();
    rest.contains(&"--build-from-source")
        && matches!(names.as_slice(), [name] if name.ends_with("/lunaepix"))
}

/// The first problem of one script line (whole-line comments already removed)
/// of the Formula job: a toolchain (`rustup` anywhere in it) or an install that
/// is not the formula's. The line is cut into commands at `;`, `&`, `|`, `(`,
/// `)` and backquotes, and a command that has the word `install` or
/// `reinstall` must be `is_formula_install`. A command it cannot read that way
/// (`sudo brew install …`, a line continued with `\`) is refused: it fails
/// closed.
fn formula_line_problem(line: &str) -> Option<String> {
    if line.to_ascii_lowercase().contains("rustup") {
        return Some(format!(
            "the formula job installs a toolchain: {}",
            line.trim()
        ));
    }
    let commands = line.split([';', '&', '|', '(', ')', '`']);
    let refused = commands.map(|command| {
        command
            .split_whitespace()
            .map(|word| word.trim_matches(|c| c == '\'' || c == '"'))
            .collect::<Vec<_>>()
    });
    let refused = refused
        .filter(|words| {
            words
                .iter()
                .any(|word| matches!(*word, "install" | "reinstall"))
        })
        .any(|words| !is_formula_install(&words));
    refused.then(|| {
        format!(
            "the formula job installs what the formula does not declare: {}",
            line.trim()
        )
    })
}

/// When the platform has `formula_job`, the job `formula` exists and:
/// - has `needs: test` (a name or a list that holds it), so it builds only
///   what the tests passed;
/// - uses no action with `cache` in its name (a user's Mac has no cache);
/// - installs nothing the formula does not declare (Review Focus 3): no
///   action with `toolchain` or `setup-` in its name (`dtolnay/rust-toolchain`,
///   `setup-node`), no `rustup`, and no command with the word `install` or
///   `reinstall` but `brew install --build-from-source` of the tap's formula
///   (`formula_line_problem`). This part reads the scripts without their
///   whole-line comments only, as `nasm_absent_problems` does: a trailing
///   `# brew install x` is a false hit it accepts, and a ` #` inside quotes
///   cannot hide what follows it;
/// - runs `FORMULA_COMMANDS`, in that order, in its scripts without their
///   shell comments (`without_comments`, where lost text makes the check fail),
///   compared by where each first appears; the first `git archive` line names
///   `HEAD`;
/// - has `timeout-minutes: 60`, compared as parsed text like `retention-days`.
///
/// Without `formula_job`, no job may be named `formula`. The scripts are read
/// as text, so this is a lint: it proves the commands are there and in order,
/// not what they do.
fn formula_job_problems(text: &str, platform: &Platform) -> Vec<String> {
    on_tree(text, |doc| {
        let job = jobs(doc)
            .into_iter()
            .find(|(name, _)| name == "formula")
            .map(|(_, job)| job);
        let Some(job) = job.filter(|_| platform.formula_job) else {
            return match (job, platform.formula_job) {
                (Some(_), _) => vec![format!("{} must have no formula job", platform.file)],
                (None, true) => vec!["no formula job".to_owned()],
                (None, false) => Vec::new(),
            };
        };
        let mut problems = Vec::new();
        if !all_texts(&job["needs"]).iter().any(|name| name == "test") {
            problems.push("the formula job does not have needs: test".to_owned());
        }
        let steps = steps_of(job);
        for uses in steps.iter().filter_map(|step| uses_of(step)) {
            let lower = uses.to_ascii_lowercase();
            if lower.contains("cache") {
                problems.push(format!("the formula job restores a cache: {uses}"));
            } else if lower.contains("toolchain") || lower.contains("setup-") {
                problems.push(format!("the formula job installs a toolchain: {uses}"));
            }
        }
        let lines = steps
            .iter()
            .filter_map(|step| scalar(&step["run"]))
            .map(|script| without_comment_lines(&script))
            .collect::<Vec<_>>();
        problems.extend(
            lines
                .iter()
                .flat_map(|script| script.lines())
                .filter_map(formula_line_problem),
        );
        let code = steps
            .iter()
            .map(|step| step_code(step))
            .collect::<Vec<_>>()
            .join("\n");
        let at: Vec<Option<usize>> = FORMULA_COMMANDS
            .iter()
            .map(|command| code.find(command))
            .collect();
        for (command, found) in FORMULA_COMMANDS.iter().zip(&at) {
            if found.is_none() {
                problems.push(format!("the formula job does not run {command}"));
            }
        }
        for pair in FORMULA_COMMANDS
            .iter()
            .zip(&at)
            .collect::<Vec<_>>()
            .windows(2)
        {
            if let [(earlier, Some(first)), (later, Some(second))] = pair
                && second < first
            {
                problems.push(format!("the formula job runs {later} before {earlier}"));
            }
        }
        let archive = code.lines().find(|line| line.contains("git archive"));
        if let Some(line) = archive {
            let mut words = line
                .split_whitespace()
                .map(|word| word.trim_matches(|c| c == '\'' || c == '"'));
            if !words.any(|word| word == "HEAD") {
                problems.push(format!(
                    "the formula job's git archive is not of HEAD: {}",
                    line.trim()
                ));
            }
        }
        let timeout = &job["timeout-minutes"];
        match scalar(timeout) {
            Some(minutes) if minutes == FORMULA_TIMEOUT => {}
            Some(minutes) => problems.push(format!(
                "the formula job's timeout-minutes is {minutes}, not {FORMULA_TIMEOUT}"
            )),
            None if timeout.is_badvalue() => {
                problems.push("the formula job has no timeout-minutes".to_owned());
            }
            None => problems.push(format!(
                "the formula job's timeout-minutes is not a single value: {timeout:?}"
            )),
        }
        problems
    })
}

const REQUIRED_COMMANDS: &[&str] = &[
    "cargo fmt --all --check",
    "cargo clippy --workspace --all-targets -- -D warnings",
    "cargo test --workspace --no-fail-fast",
    "node --test \"tests/ui/*.test.js\"",
    "cargo tauri build",
];

// ---- fixtures ----

const PIN: &str = "11bd71901bbe5b1630ceea73d27597364c9af683";

/// The build job's bundle step in `good()` and `good_mac()`.
const BUILD_STEP: &str =
    "      - run: cargo tauri build --config src-tauri/tauri.bundle.conf.json\n";

/// The NASM install step, in both jobs of `good()` before their cargo steps.
const NASM_STEP: &str = r#"      - run: |
          choco install nasm --version=2.16.3 --no-progress -y
          echo 'C:\Program Files\NASM' >> "$GITHUB_PATH"
"#;

const UPLOAD_STEP: &str = "      - uses: actions/upload-artifact@PIN # v7
        with:
          name: lunaepix-windows-installer
          path: |
            target/release/bundle/nsis/*.exe
            target/release/bundle/msi/*.msi
          if-no-files-found: error
          retention-days: 30
";

/// A Windows workflow every check accepts under `WINDOWS` (the Mac one is
/// `good_mac()`). It holds a `--release` build and a `target/release` path,
/// which are not release steps, and a cache in the `test` job, which is
/// allowed there. The variants below change one thing.
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
NASM      - run: cargo fmt --all --check
      - run: cargo clippy --workspace --all-targets -- -D warnings
      - run: cargo test --workspace --no-fail-fast
      - run: cargo build --release
      - run: node --test \"tests/ui/*.test.js\"
        if: success() || failure()
  build:
    needs: test
    runs-on: windows-latest
    steps:
NASM      - run: cargo tauri build --config src-tauri/tauri.bundle.conf.json
";
    format!("{base}{UPLOAD_STEP}")
        .replace("NASM", NASM_STEP)
        .replace("PIN", PIN)
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

// ---- Mac fixtures ----

/// The `with:` block of the test job's cache in `good_mac()`.
const MAC_SAVE_IF: &str =
    "        with:\n          save-if: ${{ github.ref == 'refs/heads/main' }}\n";

/// The Installer job's own cache in `good_mac()`; the test job's carries
/// another release comment, so the first line is only in the Installer job.
const MAC_BUILD_CACHE: &str = "      - uses: Swatinem/rust-cache@PIN # v2.9.3\n        with:\n          save-if: ${{ github.ref == 'refs/heads/main' }}\n";

/// The Rust tests step of `good_mac()`.
const MAC_RUST_TESTS: &str =
    "      - run: cargo test --workspace --no-fail-fast\n        if: success() || failure()\n";

/// The Tauri command line install, the Installer job's first cargo step.
const MAC_TAURI_INSTALL: &str = "      - run: cargo install tauri-cli --locked --version 2.12.4\n";

/// The check step of `good_mac()`, with the script `macos.yml` runs.
const MAC_CHECK_STEP: &str = r#"      - run: |
          check_app() {
            test -x "$1/Contents/MacOS/lunaepix-worker"
            codesign --verify --deep --strict --verbose=2 "$1"
            details=$(codesign -dv "$1" 2>&1)
            echo "$details"
            grep -qx 'Signature=adhoc' <<<"$details"
            grep -q '^Sealed Resources version=2' <<<"$details"
            plutil -extract 'files2.MacOS/lunaepix-worker' xml1 -o - \
              "$1/Contents/_CodeSignature/CodeResources" >/dev/null
          }
          check_app target/release/bundle/macos/Lunaepix.app
          dmg=$(ls target/release/bundle/dmg/*.dmg)
          mount=$(mktemp -d)
          hdiutil attach -nobrowse -readonly -mountpoint "$mount" "$dmg"
          trap 'hdiutil detach "$mount"' EXIT
          check_app "$mount/Lunaepix.app"
"#;

const MAC_UPLOAD_STEP: &str = "      - uses: actions/upload-artifact@PIN # v7
        with:
          name: lunaepix-macos-dmg
          path: target/release/bundle/dmg/*.dmg
          if-no-files-found: error
          retention-days: 30
";

/// The Formula job of `good_mac()`: the five commands in their order, in the
/// three steps `macos.yml` runs them in, and nothing installed but the
/// formula. Its scripts name the taps through `$CHECK_TAP` and `$BUILD_TAP`,
/// as the real job does.
const MAC_FORMULA_JOB: &str = r#"  formula:
    needs: test
    runs-on: macos-latest
    timeout-minutes: 60
    env:
      CHECK_TAP: ci/check
      BUILD_TAP: ci/build
    steps:
      - uses: actions/checkout@PIN # v7.0.1
      - run: |
          brew audit --formula "$CHECK_TAP/lunaepix"
          brew style "$CHECK_TAP/lunaepix"
      - run: |
          git archive --format=tar.gz -o "$RUNNER_TEMP/v0.1.5.tar.gz" HEAD
          brew install --build-from-source "$BUILD_TAP/lunaepix"
      - run: brew test "$BUILD_TAP/lunaepix"
"#;

/// The last step of `MAC_FORMULA_JOB`.
const FORMULA_TEST_STEP: &str = "      - run: brew test \"$BUILD_TAP/lunaepix\"\n";

/// A Mac workflow every check accepts under `MACOS`: a cache in the test and
/// Installer jobs that saves only from `main`, no NASM, the Rust tests after a
/// failed lint, the check step between the build and the upload, and the
/// Formula job. The Formula job sits between the other two, so a step added
/// at the end of the fixture lands in the Installer job, as in `good()`.
fn good_mac() -> String {
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
    runs-on: macos-latest
    steps:
      - uses: actions/checkout@PIN # v7.0.1
      - uses: actions/setup-node@PIN # v7.1.0
        with:
          node-version: 22
      - uses: Swatinem/rust-cache@PIN # v2
SAVEIF      - run: cargo fmt --all --check
      - run: cargo clippy --workspace --all-targets -- -D warnings
RUSTTESTS      - run: node --test \"tests/ui/*.test.js\"
        if: success() || failure()
FORMULA  build:
    needs: test
    runs-on: macos-latest
    steps:
CACHEINSTALLBUILDCHECKUPLOAD";
    base.replace("SAVEIF", MAC_SAVE_IF)
        .replace("RUSTTESTS", MAC_RUST_TESTS)
        .replace("CACHE", MAC_BUILD_CACHE)
        .replace("INSTALL", MAC_TAURI_INSTALL)
        .replace("BUILD", BUILD_STEP)
        .replace("CHECK", MAC_CHECK_STEP)
        .replace("UPLOAD", MAC_UPLOAD_STEP)
        // After the tokens above: the job's `$BUILD_TAP` and `$CHECK_TAP`
        // hold two of them.
        .replace("FORMULA", MAC_FORMULA_JOB)
        .replace("PIN", PIN)
}

/// `good_mac()` with the first `from` replaced by `to`. Fails when `from` is
/// not there, so a fixture cannot silently be a no-op.
fn with_mac(from: &str, to: &str) -> String {
    let from = from.replace("PIN", PIN);
    assert!(good_mac().contains(&from), "the Mac fixture lacks {from:?}");
    good_mac().replacen(&from, &to.replace("PIN", PIN), 1)
}

/// `good_mac()` with each `(from, to)` applied in turn inside its Formula job
/// only, the first `from` each time. Fails when a `from` is not there, so a
/// fixture cannot silently be a no-op.
fn with_formula_all(edits: &[(&str, &str)]) -> String {
    let original = MAC_FORMULA_JOB.replace("PIN", PIN);
    assert_eq!(
        good_mac().matches(&original).count(),
        1,
        "the job is in good_mac() once"
    );
    let mut job = original.clone();
    for (from, to) in edits {
        let from = from.replace("PIN", PIN);
        assert!(job.contains(&from), "the formula job lacks {from:?}");
        job = job.replacen(&from, &to.replace("PIN", PIN), 1);
    }
    good_mac().replacen(&original, &job, 1)
}

/// `good_mac()` with the first `from` of its Formula job replaced by `to`.
fn with_formula(from: &str, to: &str) -> String {
    with_formula_all(&[(from, to)])
}

/// `good_mac()` with `step` (six-space indent, newline-ended) added as the
/// last step of its Formula job.
fn formula_with_step(step: &str) -> String {
    with_formula(FORMULA_TEST_STEP, &format!("{FORMULA_TEST_STEP}{step}"))
}

/// `every_check` on `text` with `platform`'s profile finds exactly one
/// problem: from the check `check`, starting with `start`.
fn assert_only_problem(text: &str, platform: &Platform, check: &str, start: &str) {
    let found: Vec<(&str, String)> = every_check(text, platform)
        .into_iter()
        .flat_map(|(name, problems)| problems.into_iter().map(move |p| (name, p)))
        .collect();
    assert_eq!(found.len(), 1, "{check} {start:?}: {found:?} in\n{text}");
    let (name, problem) = &found[0];
    assert_eq!(*name, check, "{problem}");
    assert!(
        problem.starts_with(start),
        "{problem:?} does not start with {start:?}"
    );
}

/// The problems the check named `check` reports when `every_check` runs on
/// `text` with the Mac profile.
fn mac_check(text: &str, check: &str) -> Vec<String> {
    every_check(text, &MACOS)
        .into_iter()
        .find(|(name, _)| *name == check)
        .map(|(_, problems)| problems)
        .unwrap_or_else(|| panic!("every_check has no {check} for the Mac"))
}

/// Every check, named, on `text` with `platform`'s profile; the Mac's file
/// also gets `mac_check_step_problems`.
fn every_check(text: &str, platform: &Platform) -> Vec<(&'static str, Vec<String>)> {
    let mut checks = vec![
        ("unpinned_actions", unpinned_actions(text)),
        ("permission_problems", permission_problems(text)),
        ("secret_references", secret_references(text)),
        ("trigger_problems", trigger_problems(text)),
        ("injection_problems", injection_problems(text)),
        (
            "missing_commands",
            missing_commands(text, REQUIRED_COMMANDS),
        ),
        ("structure_problems", structure_problems(text, platform)),
        ("setting_problems", setting_problems(text)),
        ("upload_problems", upload_problems(text, platform)),
        ("bundle_build_problems", bundle_build_problems(text)),
        ("nasm_problems", nasm_problems(text, platform)),
        ("node_step_problems", node_step_problems(text)),
        ("cache_save_problems", cache_save_problems(text, platform)),
        ("rust_test_problems", rust_test_problems(text, platform)),
        ("formula_job_problems", formula_job_problems(text, platform)),
    ];
    if platform.file == MACOS.file {
        checks.push(("mac_check_step_problems", mac_check_step_problems(text)));
    }
    checks
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
fn assert_all_caught(check: impl Fn(&str) -> Vec<String>, start: &str, texts: &[String]) {
    texts
        .iter()
        .for_each(|text| assert_problem(&check(text), start, text));
}

// ---- the real file ----

#[test]
fn the_workflow_parses_as_one_yaml_mapping_and_on_is_a_name() {
    let doc = parse(&workflow(&WINDOWS)).unwrap_or_else(|e| panic!("{e}"));
    let hash = doc.as_hash().expect("a mapping");
    // YAML 1.2: `on` is the name `on`. A YAML 1.1 reader makes it boolean true.
    assert!(hash.contains_key(&Yaml::String("on".to_owned())));
    assert!(!hash.keys().any(|key| matches!(key, Yaml::Boolean(_))));
    assert!(doc["on"].as_hash().is_some());
}

#[test]
fn the_workflow_passes_every_check() {
    for (name, problems) in every_check(&workflow(&WINDOWS), &WINDOWS) {
        assert_eq!(problems, Vec::<String>::new(), "{name}");
    }
}

#[test]
fn the_checks_of_the_real_file_are_not_vacuous() {
    // Checkout, toolchain, cache, node and upload; two line-ending steps,
    // fmt, clippy, test, node, install and build; the installer folder.
    let (refs, scripts) = (
        action_references(&workflow(&WINDOWS)).len(),
        run_scripts(&workflow(&WINDOWS)).len(),
    );
    assert!(
        refs >= 5 && scripts >= 8,
        "{refs} action references, {scripts} run scripts"
    );
    assert!(workflow(&WINDOWS).contains("target/release/bundle/"));
}

// ---- parsing ----

#[test]
fn a_well_formed_workflow_passes_every_check_in_a_fixture() {
    for (name, problems) in every_check(&good(), &WINDOWS) {
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
        for platform in [&WINDOWS, &MACOS] {
            for (name, problems) in every_check(bad, platform) {
                assert_problem(&problems, "the workflow", &format!("{name} on {bad:?}"));
            }
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

// The window script tests run even when the Rust tests failed, so one run
// (Windows or Mac) reports the failures of both.
#[test]
#[rustfmt::skip]
fn a_node_step_that_runs_after_a_failed_step_passes_in_a_fixture() {
    assert_clean(node_step_problems(&good()));
    let block = "      - name: x\n        if: success() || failure()\n        run: |\n          node --test \"tests/ui/*.test.js\"";
    assert_clean(node_step_problems(&steps_doc(block)));
}

#[test]
#[rustfmt::skip]
fn a_node_step_skipped_after_a_failed_step_is_caught_in_a_fixture() {
    let node = "      - run: node --test \"tests/ui/*.test.js\"\n        if: success() || failure()\n";
    let texts = [
        // No condition: skipped once a step before it failed.
        with(node, "      - run: node --test \"tests/ui/*.test.js\"\n"),
        // Only after success.
        with(node, "      - run: node --test \"tests/ui/*.test.js\"\n        if: success()\n"),
        // The condition only in a comment.
        with(node, "      - run: node --test \"tests/ui/*.test.js\"\n        # if: success() || failure()\n"),
    ];
    assert_all_caught(node_step_problems, "a node test step without", &texts);
}

// The Rust tests run every test binary even after one fails, so a single
// run reports every failure on its platform, not only the first binary's.
#[test]
#[rustfmt::skip]
fn the_rust_tests_run_without_stopping_at_the_first_failing_binary() {
    let steps = "      - run: cargo test --workspace --no-fail-fast";
    assert!(missing_commands(&steps_doc(steps), REQUIRED_COMMANDS)
        .iter()
        .all(|problem| !problem.contains("cargo test")));
}

#[test]
#[rustfmt::skip]
fn rust_tests_that_stop_at_the_first_failing_binary_are_reported_in_a_fixture() {
    let steps = "      - run: cargo test --workspace";
    let problems = missing_commands(&steps_doc(steps), REQUIRED_COMMANDS);
    assert!(problems.contains(&"missing: cargo test --workspace --no-fail-fast".to_owned()), "{problems:?}");
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
    let one = structure_problems(&with("runs-on: windows-latest", "runs-on: ubuntu-latest"), &WINDOWS);
    assert_eq!(one.len(), 1, "{one:?}");
    assert_problem(&one, "job test: runner is not windows-latest", "ubuntu");
    assert_eq!(structure_problems(&good().replace("windows-latest", "ubuntu-latest"), &WINDOWS).len(), 2);
    let list = structure_problems(&with("runs-on: windows-latest", "runs-on: [windows-latest]"), &WINDOWS);
    assert_problem(&list, "job test: runs-on is not", "list");
}

#[test]
#[rustfmt::skip]
fn a_job_without_runs_on_is_caught_in_a_fixture() {
    // Checked per job: the other job still has its runner.
    let one = with("  test:\n    runs-on: windows-latest\n", "  test:\n");
    assert_eq!(structure_problems(&one, &WINDOWS), ["job test: no runs-on"]);
    assert_eq!(structure_problems(&good().replace("    runs-on: windows-latest\n", ""), &WINDOWS).len(), 2);
}

#[test]
fn a_build_job_without_needs_test_is_caught_in_a_fixture() {
    let missing = ["the build job does not have needs: test"];
    assert_eq!(
        structure_problems(&with("    needs: test\n", ""), &WINDOWS),
        missing
    );
}

#[test]
fn needs_test_on_the_wrong_job_is_caught_in_a_fixture() {
    // `needs: test` is on some line, but the build job does not have it.
    let text = with("    needs: test\n", "").replacen("  test:\n", "  test:\n    needs: test\n", 1);
    assert!(text.contains("needs: test"));
    assert_eq!(
        structure_problems(&text, &WINDOWS),
        ["the build job does not have needs: test"]
    );
    // A list that holds the job is fine, a list without it is not.
    assert_clean(structure_problems(
        &with("needs: test", "needs: [test]"),
        &WINDOWS,
    ));
    assert_eq!(
        structure_problems(&with("needs: test", "needs: [lint]"), &WINDOWS).len(),
        1
    );
}

#[test]
fn a_renamed_or_missing_test_job_is_caught_in_a_fixture() {
    let renamed = good()
        .replace("  test:\n", "  checks:\n")
        .replace("needs: test", "needs: checks");
    assert_problem(
        &structure_problems(&renamed, &WINDOWS),
        "no test job",
        "renamed",
    );
    let package = good().replace("  build:\n", "  package:\n");
    assert_problem(
        &structure_problems(&package, &WINDOWS),
        "no build job",
        "renamed build",
    );
}

#[test]
fn a_build_job_without_an_artifact_upload_is_caught_in_a_fixture() {
    let none = with("actions/upload-artifact@", "actions/download-artifact@");
    assert_eq!(structure_problems(&none, &WINDOWS).len(), 1);
    // An upload in the test job does not count for the build job.
    let build_line = "      - run: cargo build --release\n";
    let wrong_job =
        with(UPLOAD_STEP, "").replacen(build_line, &format!("{build_line}{UPLOAD_STEP}"), 1);
    assert_eq!(
        structure_problems(&wrong_job.replace("PIN", PIN), &WINDOWS).len(),
        1
    );
}

#[test]
#[rustfmt::skip]
fn a_missing_concurrency_or_a_group_without_the_workflow_name_is_caught_in_a_fixture() {
    let none = with("concurrency:\n  group: ${{ github.workflow }}-${{ github.ref }}\n", "");
    assert_eq!(structure_problems(&none, &WINDOWS), ["no concurrency"]);
    let group = with("${{ github.workflow }}-", "windows-");
    assert_problem(&structure_problems(&group, &WINDOWS), "the concurrency group", "group");
}

#[test]
#[rustfmt::skip]
fn a_build_job_that_restores_a_cache_is_caught_in_a_fixture() {
    let build = BUILD_STEP;
    for cache in ["Swatinem/rust-cache@PIN # v2", "actions/cache@PIN # v4", "actions/cache/restore@v4", "actions/cache/save@PIN # v4"] {
        let text = with(build, &format!("{build}      - uses: {cache}\n"));
        assert_problem(&structure_problems(&text, &WINDOWS), "the build job restores a cache", cache);
    }
}

#[test]
#[rustfmt::skip]
fn a_release_flag_and_a_release_path_in_a_run_script_pass_a_fixture() {
    for ok in [
        "      - run: cargo build --release", "      - run: cargo tauri build --release", "      - run: 'cargo build \"--release\"'",
        "      - run: ./target/release/lunaepix.exe --version", "      - run: ls \"target/release/bundle\"",
    ] {
        assert_clean(structure_problems(&add_step(ok), &WINDOWS));
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
    assert_all_caught(|text| structure_problems(text, &WINDOWS), "release step", &bad.map(add_step));
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
    assert_all_caught(|text| structure_problems(text, &WINDOWS), "release step", &steps.map(add_step));
    // A job that calls a reusable release workflow.
    let job = format!("{}  publish:\n    uses: org/repo/.github/workflows/release.yml@{PIN} # v1\n", good());
    assert_problem(&structure_problems(&job, &WINDOWS), "release step", "reusable workflow");
}

#[test]
#[rustfmt::skip]
fn a_release_id_input_is_caught_in_a_fixture() {
    let bad = [
        "      - uses: a/b@PIN # v1\n        with:\n          releaseId: 1",
        "      - run: ls\n        env:\n          RELEASE_ID: 1",
        "      - run: echo $releaseId",
    ];
    assert_all_caught(|text| structure_problems(text, &WINDOWS), "release step", &bad.map(add_step));
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
    assert_clean(upload_problems(
        &with("if-no-files-found: error", "if-no-files-found: 'error'"),
        &WINDOWS,
    ));
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
        assert_eq!(upload_problems(&text, &WINDOWS).len(), 1, "{value}");
    }
    // The action's default is to warn, so an upload that sets nothing is wrong.
    assert_eq!(upload_problems(&with("          if-no-files-found: error\n", ""), &WINDOWS).len(), 1);
    assert_eq!(upload_problems(&with(UPLOAD_STEP, ""), &WINDOWS).len(), 1);
}

#[test]
fn a_step_that_is_not_an_upload_but_ignores_missing_files_is_caught_in_a_fixture() {
    let step =
        "      - uses: other/action@PIN # v1\n        with:\n          if-no-files-found: warn\n";
    assert_problem(
        &upload_problems(&add_step(step), &WINDOWS),
        "if-no-files-found is warn",
        "other action",
    );
    let fine =
        "      - uses: other/action@PIN # v1\n        with:\n          if-no-files-found: error\n";
    assert_clean(upload_problems(&add_step(fine), &WINDOWS));
}

#[test]
#[rustfmt::skip]
fn an_if_no_files_found_that_is_not_a_single_value_has_its_own_message_in_a_fixture() {
    // Set, but to a list, a null or a mapping: not "does not set".
    for value in ["[error]", "null", "{a: error}"] {
        let text = with("if-no-files-found: error", &format!("if-no-files-found: {value}"));
        let problems = upload_problems(&text, &WINDOWS);
        assert_eq!(problems.len(), 1, "{value}: {problems:?}");
        assert_problem(&problems, "if-no-files-found is not a single value", value);
    }
    // The same on a step that is not an upload.
    let other = "      - uses: other/action@PIN # v1\n        with:\n          if-no-files-found: [warn]\n";
    assert_problem(&upload_problems(&add_step(other), &WINDOWS), "if-no-files-found is not a single value", "other action");
    // An upload that leaves it out keeps its own message.
    let missing = upload_problems(&with("          if-no-files-found: error\n", ""), &WINDOWS);
    assert_eq!(missing, ["an upload does not set if-no-files-found"]);
}

// ---- the upload ----

/// The two path lines of the upload step in `good()`.
const UPLOAD_PATHS: &str =
    "            target/release/bundle/nsis/*.exe\n            target/release/bundle/msi/*.msi\n";

#[test]
#[rustfmt::skip]
fn the_upload_names_the_platform_artifact_its_paths_and_thirty_days_in_a_fixture() {
    assert_clean(upload_problems(&good(), &WINDOWS));
    // The paths are a set of trimmed lines: order, trailing spaces, blank
    // lines, tabs, Windows line endings, `|-` and `|+` change nothing.
    let swapped = "            target/release/bundle/msi/*.msi\n            target/release/bundle/nsis/*.exe\n";
    let spaced = "            target/release/bundle/nsis/*.exe   \n\n            target/release/bundle/msi/*.msi \t\n";
    for paths in [swapped, spaced] {
        assert_clean(upload_problems(&with(UPLOAD_PATHS, paths), &WINDOWS));
    }
    assert_clean(upload_problems(&with("path: |\n", "path: |-\n"), &WINDOWS));
    assert_clean(upload_problems(&with("path: |\n", "path: |+\n"), &WINDOWS));
    let quoted = "path: \"target/release/bundle/nsis/*.exe\\ntarget/release/bundle/msi/*.msi\"\n";
    let flat = with(UPLOAD_PATHS, "").replace("path: |\n", quoted);
    assert_clean(upload_problems(&flat, &WINDOWS));
    assert_clean(upload_problems(&good().replace('\n', "\r\n"), &WINDOWS));
    // The name may be quoted; the retention is the number 30 however it is
    // written: the parser reads `30`, `030`, `+30`, `0x1E` and `0o36` as the
    // integer 30 and `"30"` as a string, and `scalar` turns each into the text
    // 30. A quoted `'030'` is the text 030, which is refused below.
    for name in ["'lunaepix-windows-installer'", "\"lunaepix-windows-installer\""] {
        assert_clean(upload_problems(&with("name: lunaepix-windows-installer", &format!("name: {name}")), &WINDOWS));
    }
    for days in ["'30'", "\"30\"", "30", "030", "+30", "0x1E", "0o36"] {
        assert_clean(upload_problems(&with("retention-days: 30", &format!("retention-days: {days}")), &WINDOWS));
    }
}

#[test]
#[rustfmt::skip]
fn an_upload_with_another_name_path_or_retention_is_caught_in_a_fixture() {
    let name = upload_problems(&with("name: lunaepix-windows-installer", "name: lunaepix"), &WINDOWS);
    assert_eq!(name.len(), 1, "{name:?}");
    assert_problem(&name, "an upload is named lunaepix, not lunaepix-windows-installer", "name");
    let no_msi = upload_problems(&with("            target/release/bundle/msi/*.msi\n", ""), &WINDOWS);
    assert_eq!(no_msi.len(), 1, "{no_msi:?}");
    assert_problem(&no_msi, "an upload's paths are", "missing .msi");
    let extra = with(UPLOAD_PATHS, &format!("{UPLOAD_PATHS}            target/release/lunaepix.exe\n"));
    assert_problem(&upload_problems(&extra, &WINDOWS), "an upload's paths are", "extra path");
    let days = upload_problems(&with("retention-days: 30", "retention-days: 90"), &WINDOWS);
    assert_eq!(days.len(), 1, "{days:?}");
    assert_problem(&days, "an upload's retention-days is 90, not 30", "90");
    // The action's default retention is 90 days, so an upload that sets nothing is wrong.
    let none = upload_problems(&with("          retention-days: 30\n", ""), &WINDOWS);
    assert_eq!(none.len(), 1, "{none:?}");
    assert_problem(&none, "an upload does not set retention-days", "none");
}

#[test]
#[rustfmt::skip]
fn an_upload_with_a_shape_the_rule_does_not_expect_is_caught_in_a_fixture() {
    // Other spellings of the same three inputs.
    for days in ["'90'", "29", "31", "3", "300", "30.0", "'30 '", "'30d'", "[30]", "{a: 30}", "'030'", "\"030\"", "0", "''", "null"] {
        let text = with("retention-days: 30", &format!("retention-days: {days}"));
        assert_eq!(upload_problems(&text, &WINDOWS).len(), 1, "retention-days: {days}");
    }
    for name in ["lunaepix-windows-installer-2", "Lunaepix-Windows-Installer", "'lunaepix-windows-installer '", "[lunaepix-windows-installer]", "''", "null"] {
        let text = with("name: lunaepix-windows-installer", &format!("name: {name}"));
        assert_eq!(upload_problems(&text, &WINDOWS).len(), 1, "name: {name}");
    }
    let no_name = with("          name: lunaepix-windows-installer\n", "");
    assert_problem(&upload_problems(&no_name, &WINDOWS), "an upload does not set a name", "no name");
    let one_line = with("path: |\n", "path: >\n");
    assert_problem(&upload_problems(&one_line, &WINDOWS), "an upload's paths are", "folded into one line");
    let list = with(UPLOAD_PATHS, "").replace("path: |\n", "path:\n            - target/release/bundle/nsis/*.exe\n            - target/release/bundle/msi/*.msi\n");
    assert_problem(&upload_problems(&list, &WINDOWS), "an upload's path is not", "a YAML list");
    let no_path = with("          path: |\n", "          x: |\n");
    assert_problem(&upload_problems(&no_path, &WINDOWS), "an upload does not set a path", "no path");
    let negated = with(UPLOAD_PATHS, &format!("{UPLOAD_PATHS}            !target/release/bundle/msi/x.msi\n"));
    assert_problem(&upload_problems(&negated, &WINDOWS), "an upload's paths are", "exclusion line");
    let bare = with("        with:\n          name: lunaepix-windows-installer\n          path: |\n            target/release/bundle/nsis/*.exe\n            target/release/bundle/msi/*.msi\n          if-no-files-found: error\n          retention-days: 30\n", "");
    assert_eq!(upload_problems(&bare, &WINDOWS).len(), 4, "{:?}", upload_problems(&bare, &WINDOWS));
}

#[test]
fn every_upload_in_the_workflow_is_checked_not_only_the_build_jobs_in_a_fixture() {
    let upload = UPLOAD_STEP.replace("lunaepix-windows-installer", "lunaepix");
    let second = add_step(&upload);
    assert_problem(
        &upload_problems(&second, &WINDOWS),
        "an upload is named lunaepix",
        "a second upload",
    );
    // The same upload twice is fine.
    assert_clean(upload_problems(&add_step(UPLOAD_STEP), &WINDOWS));
}

#[test]
fn a_workflow_without_an_upload_is_reported_by_the_upload_check_in_a_fixture() {
    let none = with("actions/upload-artifact@", "actions/download-artifact@");
    assert_problem(
        &upload_problems(&none, &WINDOWS),
        "no upload-artifact step",
        "download instead",
    );
    assert_problem(
        &upload_problems(&steps_doc("      - run: cargo test\n"), &WINDOWS),
        "no upload-artifact step",
        "no step",
    );
}

// ---- the platform profile ----

/// A profile that differs from `WINDOWS` in its file, runner and artifact. Its
/// cache, NASM, `save-if`, Rust-test and Formula-job policies are Windows's;
/// those are crossed with `MACOS` in the Mac tests instead.
const OTHER: Platform = Platform {
    file: "other.yml",
    runner: "other-runner",
    artifact: "other-artifact",
    artifact_paths: &["out/*.pkg"],
    ..WINDOWS
};

#[test]
fn the_runner_comes_from_the_profile_in_a_fixture() {
    let other = good().replace("windows-latest", "other-runner");
    assert_clean(structure_problems(&other, &OTHER));
    // Under the other profile the Windows runner is the wrong one, in both jobs,
    // and under the Windows profile the other runner is.
    let wrong = structure_problems(&good(), &OTHER);
    assert_eq!(wrong.len(), 2, "{wrong:?}");
    assert_problem(
        &wrong,
        "job test: runner is not other-runner",
        "windows under other",
    );
    assert_eq!(structure_problems(&other, &WINDOWS).len(), 2);
}

#[test]
fn the_artifact_and_its_paths_come_from_the_profile_in_a_fixture() {
    let other = with("name: lunaepix-windows-installer", "name: other-artifact")
        .replace(UPLOAD_PATHS, "            out/*.pkg\n");
    assert_clean(upload_problems(&other, &OTHER));
    let wrong = upload_problems(&good(), &OTHER);
    assert_eq!(wrong.len(), 2, "{wrong:?}");
    assert_problem(
        &wrong,
        "an upload is named lunaepix-windows-installer, not other-artifact",
        "name",
    );
    assert_problem(&wrong, "an upload's paths are", "paths");
    assert_eq!(upload_problems(&other, &WINDOWS).len(), 2);
}

#[test]
fn the_artifact_name_and_the_paths_are_each_read_from_the_profile_in_a_fixture() {
    // Only the name is the other profile's: the paths alone are wrong.
    let name_only = with("name: lunaepix-windows-installer", "name: other-artifact");
    let paths = upload_problems(&name_only, &OTHER);
    assert_eq!(paths.len(), 1, "{paths:?}");
    assert_problem(&paths, "an upload's paths are", "paths only");
    // Only the paths are the other profile's: the name alone is wrong.
    let paths_only = with(UPLOAD_PATHS, "            out/*.pkg\n");
    let name = upload_problems(&paths_only, &OTHER);
    assert_eq!(name.len(), 1, "{name:?}");
    assert_problem(
        &name,
        "an upload is named lunaepix-windows-installer",
        "name only",
    );
}

#[test]
#[should_panic(expected = "other.yml")]
fn the_workflow_file_comes_from_the_profile() {
    // `workflow(&WINDOWS)` reads windows.yml in the real-file tests; a profile
    // that names a file that does not exist cannot be read.
    workflow(&OTHER);
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
    let test_step = "      - run: cargo test --workspace --no-fail-fast\n";
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

// ---- NASM ----

#[test]
fn nasm_before_cargo_in_every_job_passes_a_fixture() {
    assert_clean(nasm_problems(&good(), &WINDOWS));
    // A job without a cargo step needs no NASM.
    assert_clean(nasm_problems(
        &steps_doc("      - run: node --version"),
        &WINDOWS,
    ));
    // The install and the path line may be separate lines of one script.
    let one_line = "      - run: choco install nasm --version=2.16.3 --no-progress -y && echo x >> \"$GITHUB_PATH\"\n";
    assert_clean(nasm_problems(&with(NASM_STEP, one_line), &WINDOWS));
}

#[test]
#[rustfmt::skip]
fn a_job_without_nasm_or_with_nasm_after_cargo_is_caught_in_a_fixture() {
    // Without it in the test job (the first one), in the build job, in both.
    let test_job = with(NASM_STEP, "");
    assert_eq!(nasm_problems(&test_job, &WINDOWS), ["job test: does not install nasm"]);
    let neither = good().replace(NASM_STEP, "");
    assert_eq!(
        nasm_problems(&neither, &WINDOWS),
        ["job test: does not install nasm", "job build: does not install nasm"]
    );
    let build_missing = {
        let at = good().rfind(NASM_STEP).unwrap();
        let mut text = good();
        text.replace_range(at..at + NASM_STEP.len(), "");
        text
    };
    assert_eq!(nasm_problems(&build_missing, &WINDOWS), ["job build: does not install nasm"]);
    // After the first cargo step.
    let late = with(NASM_STEP, "").replacen(
        "      - run: cargo clippy",
        &format!("{NASM_STEP}      - run: cargo clippy"),
        1,
    );
    assert_eq!(nasm_problems(&late, &WINDOWS), ["job test: installs nasm after its first cargo step"]);
    // Only in a shell comment, or only as a step name.
    let commented = "      - run: |\n          # choco install nasm\n          echo x >> \"$GITHUB_PATH\"\n";
    assert_eq!(nasm_problems(&with(NASM_STEP, commented), &WINDOWS), ["job test: does not install nasm"]);
    let named = "      - name: choco install nasm\n        run: echo x >> \"$GITHUB_PATH\"\n";
    assert_eq!(nasm_problems(&with(NASM_STEP, named), &WINDOWS), ["job test: does not install nasm"]);
    // Installed, but its folder never reaches the path.
    let no_path = "      - run: choco install nasm --version=2.16.3 --no-progress -y\n";
    assert_eq!(
        nasm_problems(&with(NASM_STEP, no_path), &WINDOWS),
        ["job test: the nasm step does not add NASM to GITHUB_PATH"]
    );
}

#[test]
#[rustfmt::skip]
fn a_nasm_install_without_a_version_pin_is_caught_in_a_fixture() {
    let path_line = "          echo x >> \"$GITHUB_PATH\"\n";
    let step = |install: &str| format!("      - run: |\n          {install}\n{path_line}");
    // No pin at all, in the test job (the first one) and in the build job.
    let unpinned = step("choco install nasm --no-progress -y");
    assert_eq!(
        nasm_problems(&with(NASM_STEP, &unpinned), &WINDOWS),
        ["job test: the nasm install is not pinned with --version="]
    );
    let build_unpinned = {
        let at = good().rfind(NASM_STEP).unwrap();
        let mut text = good();
        text.replace_range(at..at + NASM_STEP.len(), &unpinned);
        text
    };
    assert_eq!(
        nasm_problems(&build_unpinned, &WINDOWS),
        ["job build: the nasm install is not pinned with --version="]
    );
    // A pin flag with no version after it.
    let empty = step("choco install nasm --version= --no-progress -y");
    assert_eq!(
        nasm_problems(&with(NASM_STEP, &empty), &WINDOWS),
        ["job test: the nasm install is not pinned with --version="]
    );
    // A pin on another line of the script does not pin the nasm install.
    let elsewhere = "      - run: |\n          choco install nasm --no-progress -y\n          choco install other --version=1.0.0\n          echo x >> \"$GITHUB_PATH\"\n";
    assert_eq!(
        nasm_problems(&with(NASM_STEP, elsewhere), &WINDOWS),
        ["job test: the nasm install is not pinned with --version="]
    );
    // A pinned install is not reported (the positive case), wherever the flag sits.
    assert_clean(nasm_problems(&with(
        NASM_STEP,
        &step("choco install nasm --no-progress -y --version=2.16.3"),
    ), &WINDOWS));
}

// ---- the Mac workflow: the real file ----

#[test]
fn the_mac_workflow_parses_and_passes_every_check() {
    let text = workflow(&MACOS);
    let doc = parse(&text).unwrap_or_else(|e| panic!("{e}"));
    // YAML 1.2: `on` is the name `on`, not boolean true.
    assert!(doc["on"].as_hash().is_some());
    for (name, problems) in every_check(&text, &MACOS) {
        assert_eq!(problems, Vec::<String>::new(), "{name}");
    }
}

#[test]
fn the_checks_of_the_real_mac_file_are_not_vacuous() {
    // Checkout, toolchain and cache in the test and Installer jobs, node,
    // upload, and the Formula job's checkout; fmt, clippy, test, node, the
    // Tauri install, build and check, and the Formula job's three scripts;
    // the bundle folder; the check-step, cache, Rust-tests and Formula-job
    // guards are among the checks, with a cache step, a cargo test step and
    // the Formula job for them to read.
    let text = workflow(&MACOS);
    let (refs, scripts) = (action_references(&text).len(), run_scripts(&text).len());
    assert!(
        refs >= 9 && scripts >= 10,
        "{refs} action references, {scripts} run scripts"
    );
    assert!(text.contains("target/release/bundle/"));
    let names: Vec<&str> = every_check(&text, &MACOS)
        .into_iter()
        .map(|(name, _)| name)
        .collect();
    for name in [
        "mac_check_step_problems",
        "cache_save_problems",
        "rust_test_problems",
        "formula_job_problems",
    ] {
        assert!(names.contains(&name), "{names:?}");
    }
    let doc = parse(&text).unwrap_or_else(|e| panic!("{e}"));
    let formula = jobs(&doc)
        .into_iter()
        .find(|(name, _)| name == "formula")
        .map(|(_, job)| steps_of(job).len());
    assert!(
        formula.is_some_and(|steps| steps >= 4),
        "the formula job and its steps: {formula:?}"
    );
    let caches = all_steps(&doc)
        .into_iter()
        .filter(|step| is_rust_cache(step))
        .count();
    assert_eq!(caches, 2, "a rust-cache step in each job");
    let tests = all_steps(&doc)
        .into_iter()
        .filter(|step| runs_cargo_test(&step_code(step)))
        .count();
    assert_eq!(tests, 1, "one cargo test step");
}

// ---- the Mac workflow: fixtures ----

#[test]
fn a_well_formed_mac_workflow_passes_every_check_in_a_fixture() {
    for (name, problems) in every_check(&good_mac(), &MACOS) {
        assert_eq!(problems, Vec::<String>::new(), "{name}");
    }
}

#[test]
#[should_panic(expected = "the Mac fixture lacks")]
fn a_mac_fixture_change_that_finds_nothing_fails() {
    with_mac("brew install nasm", "");
}

#[test]
fn the_check_step_guard_runs_in_every_check_for_the_mac_only_in_a_fixture() {
    let none = with_mac(MAC_CHECK_STEP, "");
    assert_problem(
        &mac_check(&none, "mac_check_step_problems"),
        "no check step",
        "Mac",
    );
    // Not for Windows, whose file has no check step.
    let windows: Vec<&str> = every_check(&good(), &WINDOWS)
        .into_iter()
        .map(|(name, _)| name)
        .collect();
    assert!(!windows.contains(&"mac_check_step_problems"), "{windows:?}");
}

// The rules both files share, each broken once in the Mac fixture and caught
// by its check through `every_check(.., &MACOS)`.
#[test]
#[rustfmt::skip]
fn the_shared_guards_catch_their_problem_in_the_mac_fixture() {
    let fmt = "      - run: cargo fmt --all --check\n";
    let node = "      - run: node --test \"tests/ui/*.test.js\"\n        if: success() || failure()\n";
    let cases = [
        (with_mac("contents: read", "contents: write"), "permission_problems", "write permission"),
        (with_mac("  workflow_dispatch:\n", "  workflow_dispatch:\n  pull_request:\n"), "trigger_problems", "triggers are"),
        (with_mac("actions/checkout@PIN # v7.0.1", "actions/checkout@v7"), "unpinned_actions", "not pinned"),
        (with_mac(fmt, &format!("{fmt}        env:\n          T: ${{{{ secrets.X }}}}\n")), "secret_references", "${{ secrets.X }}"),
        (with_mac(fmt, &format!("{fmt}      - run: echo \"${{{{ github.ref_name }}}}\"\n")), "injection_problems", "expression in a run script"),
        (with_mac("cargo test --workspace --no-fail-fast", "cargo test --workspace"), "missing_commands", "missing: cargo test --workspace --no-fail-fast"),
        (with_mac(node, "      - run: node --test \"tests/ui/*.test.js\"\n"), "node_step_problems", "a node test step without"),
        (with_mac(BUILD_STEP, "      - run: cargo tauri build\n"), "bundle_build_problems", "a bundle build without"),
        (format!("{}      - run: gh release create v1\n", good_mac()), "structure_problems", "release step"),
        (with_mac("if-no-files-found: error", "if-no-files-found: warn"), "upload_problems", "if-no-files-found is warn"),
    ];
    for (text, check, start) in &cases {
        assert_problem(&mac_check(text, check), start, check);
    }
}

#[test]
fn a_mac_runner_that_is_not_macos_latest_is_caught_in_a_fixture() {
    for runner in ["windows-latest", "macos-13"] {
        let text = with_mac("runs-on: macos-latest", &format!("runs-on: {runner}"));
        assert_eq!(
            structure_problems(&text, &MACOS),
            [format!("job test: runner is not macos-latest: {runner}")]
        );
    }
}

#[test]
fn the_mac_build_job_restores_its_own_cache_in_a_fixture() {
    assert_clean(structure_problems(&good_mac(), &MACOS));
    // A build job with its cache and no cargo step: nothing for it to be late for.
    let no_cargo = with_mac(MAC_TAURI_INSTALL, "").replace(BUILD_STEP, "");
    assert_clean(structure_problems(&no_cargo, &MACOS));
}

#[test]
#[rustfmt::skip]
fn a_mac_build_job_without_its_cache_or_with_it_after_cargo_is_caught_in_a_fixture() {
    let missing = ["the build job does not restore Swatinem/rust-cache"];
    // Only the test job keeps its cache.
    assert_eq!(structure_problems(&with_mac(MAC_BUILD_CACHE, ""), &MACOS), missing);
    // Another cache action is not the one the job relies on.
    let other = with_mac(MAC_BUILD_CACHE, "      - uses: actions/cache@PIN # v4\n");
    assert_eq!(structure_problems(&other, &MACOS), missing);
    // After the first cargo step (the Tauri install), so that install is never cached.
    let late = with_mac(
        &format!("{MAC_BUILD_CACHE}{MAC_TAURI_INSTALL}"),
        &format!("{MAC_TAURI_INSTALL}{MAC_BUILD_CACHE}"),
    );
    assert_eq!(structure_problems(&late, &MACOS), ["the build job restores its cache after its first cargo step"]);
    // The policy comes from the profile: the Windows shape (no Installer cache)
    // is wrong for the Mac, and the Mac shape is wrong for Windows.
    let windows_shape = good().replace("windows-latest", "macos-latest");
    assert_eq!(structure_problems(&windows_shape, &MACOS), missing);
    let mac_shape = good_mac().replace("macos-latest", "windows-latest");
    let forbidden = structure_problems(&mac_shape, &WINDOWS);
    assert_eq!(forbidden.len(), 1, "{forbidden:?}");
    assert_problem(&forbidden, "the build job restores a cache: Swatinem/rust-cache@", "Mac shape");
}

// ---- the Mac workflow: no NASM ----

/// `good_mac()` with `step` (six-space indent, newline-ended) added as the
/// first step before the test job's first cargo step.
fn mac_with_step_in_test_job(step: &str) -> String {
    let fmt = "      - run: cargo fmt --all --check\n";
    with_mac(fmt, &format!("{step}{fmt}"))
}

/// `good_mac()` with `step` added before the Installer job's Tauri install.
fn mac_with_step_in_build_job(step: &str) -> String {
    with_mac(MAC_TAURI_INSTALL, &format!("{step}{MAC_TAURI_INSTALL}"))
}

#[test]
fn a_mac_workflow_without_nasm_passes_a_fixture() {
    assert_clean(nasm_problems(&good_mac(), &MACOS));
    // A job without a cargo step is read too, and passes without NASM.
    assert_clean(nasm_problems(
        &steps_doc("      - run: node --version"),
        &MACOS,
    ));
}

#[test]
#[rustfmt::skip]
fn nasm_only_in_a_comment_a_step_name_or_another_word_passes_a_mac_fixture() {
    for step in [
        "      - run: |\n          # brew install nasm\n          echo done\n",
        // In a plain value YAML itself ends the text at ` #`.
        "      - run: echo done # nasm -v\n",
        "      - name: brew install nasm\n        run: echo done\n",
        "      - name: nasm -v\n        run: echo done\n",
        "      - run: echo nasmfoo unasm nasm_x\n",
        "      - uses: actions/setup-node@PIN # nasm is not needed\n",
    ] {
        assert_clean(nasm_problems(&mac_with_step_in_test_job(step), &MACOS));
    }
}

// Each spelling of a NASM install or run, once, in the test job. The rule is a
// lint over the `run:` text and the `uses:` name, so these are the spellings it
// is known to catch.
#[test]
#[rustfmt::skip]
fn a_mac_step_that_installs_or_runs_nasm_is_caught_in_a_fixture() {
    let steps = [
        "      - run: brew install nasm\n",
        "      - run: |\n          brew install nasm\n          nasm -v\n",
        "      - run: brew install nasm && nasm -v\n",
        "      - run: brew install --formula nasm\n",
        "      - run: brew install nasm@2\n",
        "      - run: choco install nasm --version=2.16.3 --no-progress -y\n",
        "      - run: sudo apt-get install -y nasm\n",
        "      - run: sudo apt install nasm\n",
        "      - run: nasm -v\n",
        "      - run: 'nasm -v'\n",
        "      - run: /opt/homebrew/bin/nasm -v\n",
        "      - run: echo \"$(brew --prefix)/bin/nasm\" >> \"$GITHUB_PATH\"\n",
        "      - run: NASM -v\n",
        // A trailing comment is read as code: a false hit the rule accepts.
        "      - run: |\n          echo done # nasm -v\n",
        // A ` #` inside quotes must not hide what follows it.
        "      - run: 'echo \"a #b\"; brew install nasm'\n",
    ];
    for step in steps {
        let problems = nasm_problems(&mac_with_step_in_test_job(step), &MACOS);
        assert_eq!(problems.len(), 1, "{step}: {problems:?}");
        assert_problem(&problems, "job test: a step installs or runs nasm", step);
    }
}

#[test]
#[rustfmt::skip]
fn a_mac_step_that_uses_a_nasm_action_is_caught_in_a_fixture() {
    for step in [
        "      - uses: ilammy/setup-nasm@PIN # v1.5.2\n",
        "      - uses: Some/Install-NASM@v1\n",
    ] {
        let problems = nasm_problems(&mac_with_step_in_test_job(step), &MACOS);
        assert_eq!(problems.len(), 1, "{step}: {problems:?}");
        assert_problem(&problems, "job test: a step uses a nasm action", step);
    }
}

#[test]
fn nasm_in_the_installer_job_or_in_a_job_without_cargo_is_caught_in_a_fixture() {
    let in_build = nasm_problems(
        &mac_with_step_in_build_job("      - run: brew install nasm\n"),
        &MACOS,
    );
    assert_eq!(in_build.len(), 1, "{in_build:?}");
    assert_problem(
        &in_build,
        "job build: a step installs or runs nasm",
        "build job",
    );
    // A third job that never runs cargo is read like the others.
    let docs = format!(
        "{}  docs:\n    runs-on: macos-latest\n    steps:\n      - run: nasm -v\n",
        good_mac()
    );
    let problems = nasm_problems(&docs, &MACOS);
    assert_eq!(problems.len(), 1, "{problems:?}");
    assert_problem(
        &problems,
        "job docs: a step installs or runs nasm",
        "job without cargo",
    );
}

#[test]
fn the_nasm_rule_comes_from_the_profile_in_a_fixture() {
    // The Windows file has Chocolatey's install: wrong for the Mac, in both jobs.
    let windows_under_mac = nasm_problems(&good(), &MACOS);
    assert_eq!(windows_under_mac.len(), 2, "{windows_under_mac:?}");
    assert_problem(
        &windows_under_mac,
        "job test: a step installs or runs nasm",
        "test",
    );
    assert_problem(
        &windows_under_mac,
        "job build: a step installs or runs nasm",
        "build",
    );
    // The Mac file has no NASM: wrong for Windows, in both jobs.
    assert_eq!(
        nasm_problems(&good_mac(), &WINDOWS),
        [
            "job test: does not install nasm",
            "job build: does not install nasm"
        ]
    );
}

// ---- the Mac workflow: the cache saves only from main ----

/// The two `save-if` lines of `good_mac()` become `with:` blocks that carry
/// `save_if` (the Installer job's block is replaced too).
fn mac_with_save_if(save_if: &str) -> String {
    let line = "          save-if: ${{ github.ref == 'refs/heads/main' }}\n";
    good_mac().replace(line, &format!("          save-if: {save_if}\n"))
}

#[test]
fn a_mac_cache_that_saves_only_from_main_passes_a_fixture() {
    assert_clean(cache_save_problems(&good_mac(), &MACOS));
    // The accepted spellings: the expression as parsed, outer whitespace
    // trimmed. Quoting, a trailing space inside quotes, and a block scalar
    // (which ends in a newline) are the same text.
    let expression = "${{ github.ref == 'refs/heads/main' }}";
    for spelling in [
        expression.to_owned(),
        "'${{ github.ref == ''refs/heads/main'' }}'".to_owned(),
        format!("\"{expression}\""),
        format!("\"{expression} \""),
        format!("\" {expression}\""),
        format!("|\n            {expression}"),
        format!(">-\n            {expression}"),
    ] {
        let text = mac_with_save_if(&spelling);
        assert_clean(cache_save_problems(&text, &MACOS));
    }
}

#[test]
#[rustfmt::skip]
fn a_mac_cache_with_another_save_if_is_caught_in_a_fixture() {
    // Refused spellings: another ref, a negation, a boolean, no expression
    // wrapper, another expression with the same meaning, a different inner
    // spacing (the rule compares the text, and fails closed), a list, null.
    for save_if in [
        "${{ github.ref == 'refs/heads/develop' }}",
        "${{ github.ref != 'refs/heads/main' }}",
        "${{ github.ref_name == 'main' }}",
        "${{ github.ref == 'refs/heads/main' || true }}",
        "${{github.ref == 'refs/heads/main'}}",
        "${{ github.ref=='refs/heads/main' }}",
        "github.ref == 'refs/heads/main'",
        "true", "'true'", "false", "always", "''", "[a]", "null", "{a: b}",
    ] {
        let text = mac_with_save_if(save_if);
        let problems = cache_save_problems(&text, &MACOS);
        assert_eq!(problems.len(), 2, "save-if: {save_if}: {problems:?}");
        assert_problem(&problems, "a rust-cache step's save-if", save_if);
    }
}

#[test]
fn a_mac_cache_without_save_if_is_caught_in_each_job_in_a_fixture() {
    // Neither cache has a `with:` block of its own saying so.
    let none = good_mac().replace(
        "        with:\n          save-if: ${{ github.ref == 'refs/heads/main' }}\n",
        "",
    );
    let problems = cache_save_problems(&none, &MACOS);
    assert_eq!(problems.len(), 2, "{problems:?}");
    assert_problem(&problems, "a rust-cache step has no save-if", "no with");
    // A `with:` block that sets something else.
    let other = good_mac().replace("save-if:", "key:");
    let problems = cache_save_problems(&other, &MACOS);
    assert_eq!(problems.len(), 2, "{problems:?}");
    assert_problem(&problems, "a rust-cache step has no save-if", "other input");
    // Only the test job's cache, or only the Installer job's.
    let test_only = with_mac(
        MAC_BUILD_CACHE,
        "      - uses: Swatinem/rust-cache@PIN # v2.9.3\n",
    );
    assert_eq!(cache_save_problems(&test_only, &MACOS).len(), 1);
    let build_only = with_mac(MAC_SAVE_IF, "");
    assert_eq!(cache_save_problems(&build_only, &MACOS).len(), 1);
}

#[test]
fn a_mac_workflow_without_any_rust_cache_step_is_caught_in_a_fixture() {
    let problems = cache_save_problems(&steps_doc("      - run: cargo test"), &MACOS);
    assert_eq!(problems, ["no rust-cache step to check"]);
}

#[test]
fn the_save_if_rule_comes_from_the_profile_in_a_fixture() {
    // Windows has no rule: its caches may carry any save-if, or none.
    assert_clean(cache_save_problems(&good(), &WINDOWS));
    let cache = format!("      - uses: Swatinem/rust-cache@{PIN} # v2\n");
    let any = good().replace(
        &cache,
        &format!("{cache}        with:\n          save-if: true\n"),
    );
    assert_ne!(any, good(), "the fixture changed nothing");
    assert_clean(cache_save_problems(&any, &WINDOWS));
    // The same Windows file under the Mac profile is wrong.
    assert_problem(
        &cache_save_problems(&good(), &MACOS),
        "a rust-cache step has no save-if",
        "windows file",
    );
}

// ---- the Mac workflow: the Rust tests run after a failed lint ----

#[test]
fn mac_rust_tests_that_run_after_a_failed_step_pass_a_fixture() {
    assert_clean(rust_test_problems(&good_mac(), &MACOS));
    let block = with_mac(
        MAC_RUST_TESTS,
        "      - name: x\n        if: success() || failure()\n        run: |\n          cargo test --workspace --no-fail-fast\n",
    );
    assert_clean(rust_test_problems(&block, &MACOS));
}

#[test]
#[rustfmt::skip]
fn mac_rust_tests_skipped_after_a_failed_step_are_caught_in_a_fixture() {
    let test = "cargo test --workspace --no-fail-fast";
    for step in [
        // No condition: skipped once clippy failed.
        format!("      - run: {test}\n"),
        // Only after success.
        format!("      - run: {test}\n        if: success()\n"),
        // Runs after a cancel too.
        format!("      - run: {test}\n        if: always()\n"),
        // The condition only in a comment.
        format!("      - run: {test}\n        # if: success() || failure()\n"),
        // In a block with other commands.
        format!("      - run: |\n          echo start\n          {test}\n"),
    ] {
        let text = with_mac(MAC_RUST_TESTS, &step);
        let problems = rust_test_problems(&text, &MACOS);
        assert_eq!(problems.len(), 1, "{step}: {problems:?}");
        assert_problem(&problems, "a cargo test step without `if: success() || failure()`", &step);
    }
    // A second cargo test step is checked too.
    let second = format!("{}      - run: cargo test -p x\n", good_mac());
    assert_problem(&rust_test_problems(&second, &MACOS), "a cargo test step without", "second step");
    // No cargo test step at all.
    assert_eq!(rust_test_problems(&with_mac(MAC_RUST_TESTS, ""), &MACOS), ["no cargo test step"]);
}

#[test]
fn the_rust_tests_rule_comes_from_the_profile_in_a_fixture() {
    // Windows's test step has no condition, and the Windows file must not change.
    assert!(good().contains("      - run: cargo test --workspace --no-fail-fast\n      - run:"));
    assert_clean(rust_test_problems(&good(), &WINDOWS));
    // The same file under the Mac profile is wrong.
    assert_problem(
        &rust_test_problems(&good(), &MACOS),
        "a cargo test step without",
        "windows file",
    );
}

#[test]
fn the_new_guards_are_checks_of_both_platforms_in_a_fixture() {
    for platform in [&WINDOWS, &MACOS] {
        let names: Vec<&str> = every_check(&good_mac(), platform)
            .into_iter()
            .map(|(name, _)| name)
            .collect();
        for name in [
            "cache_save_problems",
            "rust_test_problems",
            "formula_job_problems",
        ] {
            assert!(names.contains(&name), "{name} in {names:?}");
        }
    }
}

// The new Mac rules, each broken once in the Mac fixture and caught by its
// check through `every_check(.., &MACOS)`.
#[test]
#[rustfmt::skip]
fn the_new_mac_guards_catch_their_problem_in_the_mac_fixture() {
    let cases = [
        (mac_with_step_in_test_job("      - run: brew install nasm\n"), "nasm_problems", "job test: a step installs or runs nasm"),
        (mac_with_save_if("true"), "cache_save_problems", "a rust-cache step's save-if"),
        (with_mac(MAC_RUST_TESTS, "      - run: cargo test --workspace --no-fail-fast\n"), "rust_test_problems", "a cargo test step without"),
    ];
    for (text, check, start) in &cases {
        assert_problem(&mac_check(text, check), start, check);
    }
}

#[test]
fn a_mac_upload_with_the_windows_name_or_the_app_path_is_caught_in_a_fixture() {
    assert_clean(upload_problems(&good_mac(), &MACOS));
    let name = with_mac(
        "name: lunaepix-macos-dmg",
        "name: lunaepix-windows-installer",
    );
    assert_eq!(
        upload_problems(&name, &MACOS),
        ["an upload is named lunaepix-windows-installer, not lunaepix-macos-dmg"]
    );
    let app = with_mac(
        "path: target/release/bundle/dmg/*.dmg",
        "path: target/release/bundle/macos/*.app",
    );
    let app = upload_problems(&app, &MACOS);
    assert_eq!(app.len(), 1, "{app:?}");
    assert_problem(&app, "an upload's paths are", "the .app");
}

// ---- the Mac check step ----

#[test]
fn a_check_step_between_the_build_and_the_upload_passes_a_fixture() {
    assert_clean(mac_check_step_problems(&good_mac()));
}

#[test]
#[rustfmt::skip]
fn a_missing_or_commented_check_step_is_caught_in_a_fixture() {
    let verify = "            codesign --verify --deep --strict --verbose=2 \"$1\"\n";
    let texts = [
        with_mac(MAC_CHECK_STEP, ""),
        with_mac(verify, "            # codesign --verify --deep --strict --verbose=2 \"$1\"\n"),
        with_mac(verify, "            true # codesign --verify --deep --strict --verbose=2 \"$1\"\n"),
    ];
    for text in &texts {
        assert_eq!(mac_check_step_problems(text), ["no check step: no step of the build job runs codesign --verify --deep --strict"]);
    }
    let no_build = good_mac().replacen("  build:\n", "  installer:\n", 1);
    assert_eq!(mac_check_step_problems(&no_build), ["no build job"]);
}

#[test]
#[rustfmt::skip]
fn a_check_step_without_plutil_or_hdiutil_is_caught_in_a_fixture() {
    let plutil = "            plutil -extract 'files2.MacOS/lunaepix-worker' xml1 -o - \\\n              \"$1/Contents/_CodeSignature/CodeResources\" >/dev/null\n";
    let commented = format!("            # {}", plutil.trim_start());
    for text in [with_mac(plutil, ""), with_mac(plutil, &commented)] {
        assert_eq!(mac_check_step_problems(&text), ["the check step does not run plutil -extract"]);
    }
    let attach = "          hdiutil attach -nobrowse -readonly -mountpoint \"$mount\" \"$dmg\"\n";
    let commented = format!("          # {}", attach.trim_start());
    for text in [with_mac(attach, ""), with_mac(attach, &commented)] {
        assert_eq!(mac_check_step_problems(&text), ["the check step does not run hdiutil attach"]);
    }
}

#[test]
#[rustfmt::skip]
fn a_check_step_out_of_order_is_caught_in_a_fixture() {
    let after = ["the check step does not come after a cargo tauri build"];
    // Before the build, and with no build at all.
    let early = with_mac(&format!("{BUILD_STEP}{MAC_CHECK_STEP}"), &format!("{MAC_CHECK_STEP}{BUILD_STEP}"));
    assert_eq!(mac_check_step_problems(&early), after);
    assert_eq!(mac_check_step_problems(&with_mac(BUILD_STEP, "")), after);
    // After the upload, and with no upload at all.
    let before = ["the check step does not come before the upload"];
    let late = format!("{}{MAC_CHECK_STEP}", with_mac(MAC_CHECK_STEP, ""));
    assert_eq!(mac_check_step_problems(&late), before);
    assert_eq!(mac_check_step_problems(&with_mac(MAC_UPLOAD_STEP, "")), before);
}

// ---- the Formula job ----

/// The lines of `MAC_FORMULA_JOB` the fixtures below change.
const FORMULA_AUDIT: &str = "          brew audit --formula \"$CHECK_TAP/lunaepix\"\n";
const FORMULA_STYLE: &str = "          brew style \"$CHECK_TAP/lunaepix\"\n";
const FORMULA_ARCHIVE: &str =
    "          git archive --format=tar.gz -o \"$RUNNER_TEMP/v0.1.5.tar.gz\" HEAD\n";
const FORMULA_INSTALL: &str =
    "          brew install --build-from-source \"$BUILD_TAP/lunaepix\"\n";

#[test]
#[should_panic(expected = "the formula job lacks")]
fn a_formula_fixture_change_that_finds_nothing_fails() {
    with_formula("brew install node", "");
}

#[test]
#[rustfmt::skip]
fn a_formula_job_that_needs_the_tests_passes_a_fixture() {
    assert_clean(formula_job_problems(&good_mac(), &MACOS));
    let one_script = format!(
        "      - run: |\n{FORMULA_AUDIT}{FORMULA_STYLE}{FORMULA_ARCHIVE}{FORMULA_INSTALL}          brew test \"$BUILD_TAP/lunaepix\"\n"
    );
    let texts = [
        // `needs` as a list that holds the test job.
        with_formula("needs: test", "needs: [test]"),
        with_formula("needs: test", "needs: [test, build]"),
        // The timeout quoted.
        with_formula("timeout-minutes: 60", "timeout-minutes: '60'"),
        // Another flag after --build-from-source (the presence check looks for
        // `brew install --build-from-source` as written, so the flag comes first).
        with_formula("--build-from-source \"$BUILD_TAP", "--build-from-source --verbose \"$BUILD_TAP"),
        // An install only in a whole-line comment.
        formula_with_step("      - run: |\n          # brew install node\n          echo done\n"),
        // The five commands in one script.
        with_formula_all(&[
            ("      - run: |\n          git archive", "          git archive"),
            (FORMULA_TEST_STEP, "          brew test \"$BUILD_TAP/lunaepix\"\n"),
        ]),
    ];
    assert!(texts[5].contains(&one_script), "{}", texts[5]);
    for text in &texts {
        for (name, problems) in every_check(text, &MACOS) {
            assert_eq!(problems, Vec::<String>::new(), "{name} in\n{text}");
        }
    }
}

#[test]
fn a_mac_workflow_without_the_formula_job_is_caught_in_a_fixture() {
    let job = MAC_FORMULA_JOB.replace("PIN", PIN);
    let none = good_mac().replacen(&job, "", 1);
    assert_only_problem(&none, &MACOS, "formula_job_problems", "no formula job");
    let renamed = good_mac().replacen("  formula:\n", "  brew:\n", 1);
    assert_only_problem(&renamed, &MACOS, "formula_job_problems", "no formula job");
    // The Windows fixture has none.
    assert_eq!(formula_job_problems(&good(), &MACOS), ["no formula job"]);
}

#[test]
fn a_formula_job_without_needs_test_is_caught_in_a_fixture() {
    for text in [
        with_formula("    needs: test\n", ""),
        with_formula("needs: test", "needs: build"),
        with_formula("needs: test", "needs: [build]"),
    ] {
        assert_only_problem(
            &text,
            &MACOS,
            "formula_job_problems",
            "the formula job does not have needs: test",
        );
    }
}

#[test]
fn a_formula_job_with_a_cache_is_caught_in_a_fixture() {
    for step in [MAC_BUILD_CACHE, "      - uses: actions/cache@PIN # v4\n"] {
        assert_only_problem(
            &formula_with_step(step),
            &MACOS,
            "formula_job_problems",
            "the formula job restores a cache",
        );
    }
}

#[test]
#[rustfmt::skip]
fn a_formula_job_that_installs_a_toolchain_is_caught_in_a_fixture() {
    for step in [
        "      - uses: dtolnay/rust-toolchain@PIN # stable\n",
        "      - uses: actions/setup-node@PIN # v7.1.0\n",
        "      - run: rustup toolchain install stable\n",
        "      - run: curl --proto '=https' -sSf https://sh.rustup.rs | sh -s -- -y\n",
    ] {
        assert_only_problem(&formula_with_step(step), &MACOS, "formula_job_problems", "the formula job installs a toolchain");
    }
}

// Each spelling once, as a step added after a valid job. The rule is a lint
// over the `run:` text without its whole-line comments, so these are the
// spellings it is known to catch.
#[test]
#[rustfmt::skip]
fn a_formula_job_that_installs_another_package_is_caught_in_a_fixture() {
    let other = "the formula job installs what the formula does not declare";
    let steps = [
        "      - run: brew install node\n",
        "      - run: brew install --build-from-source node\n",
        "      - run: brew install --formula node\n",
        // The tap's formula, but not built from source.
        "      - run: brew install \"$BUILD_TAP/lunaepix\"\n",
        // The tap's formula and one more.
        "      - run: brew install --build-from-source \"$BUILD_TAP/lunaepix\" node\n",
        "      - run: echo \"$(brew install node)\"\n",
        "      - run: cargo install tauri-cli --locked --version 2.12.4\n",
        "      - run: npm install\n",
        "      - run: brew reinstall node\n",
        // Another command in front fails closed.
        "      - run: sudo brew install --build-from-source \"$BUILD_TAP/lunaepix\"\n",
        // So does a line continued with `\`.
        "      - run: |\n          brew install --build-from-source \\\n            \"$BUILD_TAP/lunaepix\"\n",
        // A trailing comment is read as code: a false hit the rule accepts.
        "      - run: |\n          echo x # brew install node\n",
        // A ` #` inside quotes must not hide what follows it.
        "      - run: 'echo \"a #b\"; brew install node'\n",
    ];
    for step in steps {
        assert_only_problem(&formula_with_step(step), &MACOS, "formula_job_problems", other);
    }
    // A second command on the line of the allowed install.
    let chained = with_formula(FORMULA_INSTALL, &format!("{} && brew install node\n", FORMULA_INSTALL.trim_end()));
    assert_only_problem(&chained, &MACOS, "formula_job_problems", other);
}

#[test]
#[rustfmt::skip]
fn a_formula_job_missing_a_required_command_is_caught_in_a_fixture() {
    let commented_test = "      - run: |\n          # brew test \"$BUILD_TAP/lunaepix\"\n";
    let cases = [
        ("brew audit", FORMULA_AUDIT, String::new()),
        ("brew style", FORMULA_STYLE, String::new()),
        ("git archive", FORMULA_ARCHIVE, String::new()),
        ("brew install --build-from-source", FORMULA_INSTALL, String::new()),
        ("brew test", FORMULA_TEST_STEP, String::new()),
        // Only in a whole-line shell comment.
        ("brew audit", FORMULA_AUDIT, FORMULA_AUDIT.replacen("brew", "# brew", 1)),
        ("brew style", FORMULA_STYLE, FORMULA_STYLE.replacen("brew", "# brew", 1)),
        ("git archive", FORMULA_ARCHIVE, FORMULA_ARCHIVE.replacen("git", "# git", 1)),
        ("brew install --build-from-source", FORMULA_INSTALL, FORMULA_INSTALL.replacen("brew", "# brew", 1)),
        ("brew test", FORMULA_TEST_STEP, commented_test.to_owned()),
    ];
    for (command, from, to) in &cases {
        let text = with_formula(from, to);
        assert_only_problem(&text, &MACOS, "formula_job_problems", &format!("the formula job does not run {command}"));
    }
    // `brew test` only in the Installer job (the last job of the fixture).
    let elsewhere = format!("{}{FORMULA_TEST_STEP}", with_formula(FORMULA_TEST_STEP, ""));
    assert_only_problem(&elsewhere, &MACOS, "formula_job_problems", "the formula job does not run brew test");
}

#[test]
#[rustfmt::skip]
fn a_formula_job_with_its_commands_out_of_order_is_caught_in_a_fixture() {
    let test_line = "          brew test \"$BUILD_TAP/lunaepix\"\n";
    let cases = [
        // `brew test` before `brew install`.
        (
            with_formula_all(&[(FORMULA_TEST_STEP, ""), (FORMULA_INSTALL, &format!("{test_line}{FORMULA_INSTALL}"))]),
            "the formula job runs brew test before brew install --build-from-source",
        ),
        // `git archive` after `brew install`.
        (
            with_formula(&format!("{FORMULA_ARCHIVE}{FORMULA_INSTALL}"), &format!("{FORMULA_INSTALL}{FORMULA_ARCHIVE}")),
            "the formula job runs brew install --build-from-source before git archive",
        ),
        // `brew style` before `brew audit`.
        (
            with_formula(&format!("{FORMULA_AUDIT}{FORMULA_STYLE}"), &format!("{FORMULA_STYLE}{FORMULA_AUDIT}")),
            "the formula job runs brew style before brew audit",
        ),
    ];
    for (text, start) in &cases {
        assert_only_problem(text, &MACOS, "formula_job_problems", start);
    }
}

#[test]
fn a_formula_job_whose_archive_is_not_of_head_is_caught_in_a_fixture() {
    for rev in ["v0.1.5", "HEAD~1", "main"] {
        let text = with_formula("\" HEAD\n", &format!("\" {rev}\n"));
        assert_only_problem(
            &text,
            &MACOS,
            "formula_job_problems",
            "the formula job's git archive is not of HEAD",
        );
    }
}

#[test]
fn a_formula_job_with_another_timeout_is_caught_in_a_fixture() {
    let cases = [
        (
            with_formula("timeout-minutes: 60", "timeout-minutes: 45"),
            "the formula job's timeout-minutes is 45, not 60",
        ),
        (
            with_formula("timeout-minutes: 60", "timeout-minutes: 60.0"),
            "the formula job's timeout-minutes is 60.0, not 60",
        ),
        (
            with_formula("    timeout-minutes: 60\n", ""),
            "the formula job has no timeout-minutes",
        ),
        (
            with_formula("timeout-minutes: 60", "timeout-minutes: [60]"),
            "the formula job's timeout-minutes is not a single value",
        ),
    ];
    for (text, start) in &cases {
        assert_only_problem(text, &MACOS, "formula_job_problems", start);
    }
}

#[test]
fn the_formula_job_rule_comes_from_the_profile_in_a_fixture() {
    // Windows has none, and must have none.
    assert_clean(formula_job_problems(&good(), &WINDOWS));
    let job = MAC_FORMULA_JOB
        .replace("PIN", PIN)
        .replace("macos-latest", "windows-latest");
    let windows = good().replacen("  build:\n", &format!("{job}  build:\n"), 1);
    assert_ne!(windows, good(), "the fixture changed nothing");
    assert_only_problem(
        &windows,
        &WINDOWS,
        "formula_job_problems",
        "windows.yml must have no formula job",
    );
    // The Mac fixture is wrong for Windows, and the Windows one for the Mac.
    assert_eq!(
        formula_job_problems(&good_mac(), &WINDOWS),
        ["windows.yml must have no formula job"]
    );
    assert_eq!(formula_job_problems(&good(), &MACOS), ["no formula job"]);
}

// The rules every job keeps, each broken once inside the Formula job only and
// caught by its own check, with nothing else reported.
#[test]
#[rustfmt::skip]
fn the_shared_guards_reach_the_formula_job_in_a_fixture() {
    let tap = "      BUILD_TAP: ci/build\n";
    let cases = [
        (with_formula("actions/checkout@PIN # v7.0.1", "actions/checkout@v7"), "unpinned_actions", "not pinned to a commit: actions/checkout@v7"),
        (with_formula("runs-on: macos-latest", "runs-on: macos-13"), "structure_problems", "job formula: runner is not macos-latest"),
        (with_formula(FORMULA_TEST_STEP, "      - run: brew test \"${{ env.BUILD_TAP }}/lunaepix\"\n"), "injection_problems", "expression in a run script"),
        (with_formula(tap, &format!("{tap}      RELEASE_TAP: ci/x\n")), "structure_problems", "release step: RELEASE_TAP"),
        (formula_with_step("      - run: nasm -v\n"), "nasm_problems", "job formula: a step installs or runs nasm"),
        (with_formula("    timeout-minutes: 60\n", "    timeout-minutes: 60\n    permissions:\n      contents: write\n"), "permission_problems", "write permission in job formula"),
        (with_formula(tap, &format!("{tap}      T: ${{{{ secrets.X }}}}\n")), "secret_references", "${{ secrets.X }}"),
    ];
    for (text, check, start) in &cases {
        assert_only_problem(text, &MACOS, check, start);
    }
}
