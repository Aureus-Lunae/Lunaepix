//! Guards on the Windows build workflow (`.github/workflows/windows.yml`).
//! It cannot be run here, and it builds the installer people download, so
//! the rules that keep it safe are checked as text: every action pinned to
//! a commit, read-only permissions, no secrets, no way to start it from a
//! pull request, and the test and build commands present. Each check has a
//! fixture it must catch, so it is known to fail when it should.
//!
//! The workflow is read line by line, not parsed as YAML; it is our own
//! file and kept in the plain block style the checks expect.

use std::fs;
use std::path::Path;

fn workflow() -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../.github/workflows/windows.yml");
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

/// The line without its trailing ` # comment`, trimmed. (No value in the
/// workflow contains ` #`.)
fn code_of(line: &str) -> &str {
    line.split(" #").next().unwrap_or(line).trim()
}

/// How many spaces the line is indented.
fn indent_of(line: &str) -> usize {
    line.len() - line.trim_start().len()
}

/// The lines of a whole-line comment are not code.
fn is_comment(line: &str) -> bool {
    line.trim_start().starts_with('#')
}

/// Whether `text` is a full 40-character commit hash.
fn is_commit_sha(text: &str) -> bool {
    text.len() == 40 && text.chars().all(|c| matches!(c, '0'..='9' | 'a'..='f'))
}

/// Every `uses:` the line scan reads: lines that start with `uses:` (after an
/// optional `- `), as the text after the colon, without a trailing comment.
/// Whole-line comments, and a `uses:` further along a line, are not read.
fn action_references(workflow: &str) -> Vec<String> {
    workflow
        .lines()
        .filter(|line| !is_comment(line))
        .map(|line| line.trim_start().trim_start_matches("- "))
        .filter_map(|key| key.strip_prefix("uses:"))
        .map(|reference| code_of(reference).trim().to_owned())
        .collect()
}

/// The ways an action reference is not pinned: every `uses:` must name
/// `owner/repo@<40 hex digits>` and carry a trailing `# ...` comment that
/// says which release that is. A tag or branch can be moved to other code;
/// a commit cannot. A `uses:` anywhere but at the start of a step key is
/// also a problem, because the checks could not see it.
fn unpinned_actions(workflow: &str) -> Vec<String> {
    let mut problems = Vec::new();
    for line in workflow.lines().filter(|line| !is_comment(line)) {
        let key = line.trim_start().trim_start_matches("- ");
        if !key.starts_with("uses:") {
            if line.contains("uses:") {
                problems.push(format!("uses: in an unexpected place: {}", line.trim()));
            }
            continue;
        }
        let reference = code_of(key).trim_start_matches("uses:").trim();
        let pinned = reference
            .split_once('@')
            .is_some_and(|(name, sha)| name.contains('/') && is_commit_sha(sha));
        if !pinned {
            problems.push(format!("not pinned to a commit: {}", line.trim()));
        } else if !line.contains(" # ") {
            problems.push(format!("no release comment: {}", line.trim()));
        }
    }
    problems
}

/// The block under a key at column 0 (`permissions:`, `on:`, `jobs:`): the
/// following lines, up to the next line at column 0. Comments and blank
/// lines are left out.
fn top_level_block<'a>(workflow: &'a str, key: &str) -> Option<Vec<&'a str>> {
    let mut lines = workflow.lines();
    lines.find(|line| code_of(line) == key && indent_of(line) == 0)?;
    Some(
        lines
            .take_while(|line| line.trim().is_empty() || indent_of(line) > 0)
            .filter(|line| !line.trim().is_empty() && !is_comment(line))
            .collect(),
    )
}

/// A line with every run of whitespace made one space.
fn squeezed(line: &str) -> String {
    line.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Whether a line gives a `write` permission, however it is spaced
/// (`contents:  write`, `contents:write`, `{contents: write}`), or asks for
/// `write-all`. A word that only starts with "write" is not a permission.
fn grants_write(line: &str) -> bool {
    let squashed: String = line.split_whitespace().collect();
    if squashed.contains("write-all") {
        return true;
    }
    squashed.match_indices(":write").any(|(at, found)| {
        let after = squashed[at + found.len()..].chars().next();
        !after.is_some_and(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    })
}

/// What is wrong with the permissions: the top-level block must be exactly
/// `contents: read`, and no `write` permission may appear anywhere, at the
/// top or in a job (`write-all` included).
fn permission_problems(workflow: &str) -> Vec<String> {
    let mut problems = Vec::new();
    match top_level_block(workflow, "permissions:") {
        None => problems.push("no top-level permissions block".to_owned()),
        Some(block) => {
            let entries: Vec<String> = block.iter().map(|line| squeezed(code_of(line))).collect();
            if entries != ["contents: read"] {
                problems.push(format!("top-level permissions are {entries:?}"));
            }
        }
    }
    for line in workflow.lines().filter(|line| !is_comment(line)) {
        let code = code_of(line);
        if grants_write(code) {
            problems.push(format!("write permission: {code}"));
        }
    }
    problems
}

/// Whether `word` appears in `text` as a whole word (not inside a longer
/// name such as `mysecretsauce` or `secrets_manager`).
fn has_word(text: &str, word: &str) -> bool {
    let is_name = |c: char| c.is_ascii_alphanumeric() || c == '_';
    text.match_indices(word).any(|(at, found)| {
        let before = text[..at].chars().next_back();
        let after = text[at + found.len()..].chars().next();
        !before.is_some_and(is_name) && !after.is_some_and(is_name)
    })
}

/// Every place the workflow could reach a repository secret or the job
/// token: the word `secrets` in any form (`secrets.X`, `secrets['X']`,
/// `toJSON(secrets)`, `secrets: inherit`), `github.token` and `GITHUB_TOKEN`.
fn secret_references(workflow: &str) -> Vec<String> {
    workflow
        .lines()
        .filter(|line| !is_comment(line))
        .map(code_of)
        .filter(|code| {
            let lower = code.to_ascii_lowercase();
            has_word(code, "secrets")
                || lower.contains("github.token")
                || lower.contains("github_token")
        })
        .map(str::to_owned)
        .collect()
}

/// What is wrong with the triggers: the `on:` block must hold `push` and
/// `workflow_dispatch` and nothing else, and `push` must filter on the tag
/// `v*` only (no branches, so no push to a branch starts it). A pull request
/// trigger of any kind is therefore a problem, `pull_request_target` above
/// all: it runs with a token that can write, on code from a fork.
fn trigger_problems(workflow: &str) -> Vec<String> {
    let Some(block) = top_level_block(workflow, "on:") else {
        return vec!["no on: block".to_owned()];
    };
    let mut problems = Vec::new();
    let mut events: Vec<&str> = block
        .iter()
        .filter(|line| indent_of(line) == 2)
        .map(|line| code_of(line).trim_end_matches(':'))
        .collect();
    events.sort_unstable();
    if events != ["push", "workflow_dispatch"] {
        problems.push(format!("triggers are {events:?}"));
    }
    let mut under_push = false;
    for line in &block {
        if indent_of(line) == 2 {
            under_push = code_of(line) == "push:";
        } else if under_push {
            let code = code_of(line);
            if !matches!(code, "tags:" | "- 'v*'" | "- \"v*\"") {
                problems.push(format!("push is filtered by: {code}"));
            }
        }
    }
    if !block
        .iter()
        .any(|line| matches!(code_of(line), "- 'v*'" | "- \"v*\""))
    {
        problems.push("push has no v* tag filter".to_owned());
    }
    problems
}

/// The text of every `run:` step, whether one line or a block (`|`, `|-`,
/// `|+`, `>`, `>-`, ...). In a block, blank lines stay, whole-line and
/// trailing shell comments go (a command named only in a comment is not
/// run), and the block ends at the first line not indented deeper than the
/// `run` key, so a sibling key after it is not read as script.
fn run_scripts(workflow: &str) -> Vec<String> {
    let lines: Vec<&str> = workflow.lines().collect();
    let mut scripts = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let key = lines[i].trim_start().trim_start_matches("- ");
        if let Some(value) = key.strip_prefix("run:").filter(|_| !is_comment(lines[i])) {
            let value = code_of(value);
            if value.starts_with('|') || value.starts_with('>') {
                let dash = if lines[i].trim_start().starts_with("- ") {
                    2
                } else {
                    0
                };
                let key_indent = indent_of(lines[i]) + dash;
                let mut body: Vec<&str> = Vec::new();
                while i + 1 < lines.len()
                    && (lines[i + 1].trim().is_empty() || indent_of(lines[i + 1]) > key_indent)
                {
                    i += 1;
                    if !is_comment(lines[i]) {
                        body.push(code_of(lines[i]));
                    }
                }
                while body.last().is_some_and(|line| line.is_empty()) {
                    body.pop();
                }
                scripts.push(body.join("\n"));
            } else {
                scripts.push(value.to_owned());
            }
        }
        i += 1;
    }
    scripts
}

/// Context expressions where a shell script could be injected: a `${{ }}`
/// inside a `run:` script is substituted before the shell reads it, so
/// anything attacker-controlled in it (a branch name, an issue title) runs.
/// The scripts here need none, so none is allowed, and `github.event` is
/// not used anywhere. A workflow in which no script is found is a problem
/// too: the scan would be checking nothing.
fn injection_problems(workflow: &str) -> Vec<String> {
    let scripts = run_scripts(workflow);
    let mut problems = Vec::new();
    if scripts.is_empty() {
        problems.push("no run scripts found".to_owned());
    }
    problems.extend(
        scripts
            .into_iter()
            .filter(|script| script.contains("${{"))
            .map(|script| format!("expression in a run script: {script}")),
    );
    for line in workflow.lines().filter(|line| !is_comment(line)) {
        if code_of(line).contains("github.event") {
            problems.push(format!("github.event used: {}", line.trim()));
        }
    }
    problems
}

/// What is wrong with the commands: no script found at all, or a command in
/// `required` that no `run:` script contains.
fn missing_commands(workflow: &str, required: &[&str]) -> Vec<String> {
    let scripts = run_scripts(workflow);
    let mut problems = Vec::new();
    if scripts.is_empty() {
        problems.push("no run scripts found".to_owned());
    }
    let all = scripts.join("\n");
    problems.extend(
        required
            .iter()
            .filter(|command| !all.contains(*command))
            .map(|command| format!("missing: {command}")),
    );
    problems
}

/// The jobs of the `jobs:` block as `(name, its lines)`. A job starts at a
/// line indented two spaces that ends in `:`.
fn jobs(workflow: &str) -> Vec<(String, Vec<&str>)> {
    let mut jobs: Vec<(String, Vec<&str>)> = Vec::new();
    for line in top_level_block(workflow, "jobs:").unwrap_or_default() {
        if indent_of(line) == 2 && code_of(line).ends_with(':') {
            jobs.push((code_of(line).trim_end_matches(':').to_owned(), Vec::new()));
        } else if let Some((_, lines)) = jobs.last_mut() {
            lines.push(line);
        }
    }
    jobs
}

/// Whether a line names a release: a release action, `gh release`, a call to
/// a `.../releases` API, or `tauri-action` (which makes releases).
/// `--release` builds and `target/release/` paths are not releases.
fn makes_a_release(text: &str) -> bool {
    let lower = text
        .to_ascii_lowercase()
        .replace("--release", "")
        .replace("/release/", "");
    lower.contains("release") || lower.contains("tauri-action")
}

/// What is wrong with the shape of the workflow: every runner is
/// `windows-latest`; the job `build` exists, needs the job `test` (the
/// `needs:` sits inside `build`), uploads with `actions/upload-artifact@` and
/// restores no cache (a debug-profile cache does not serve a release build);
/// runs are queued per workflow and ref; and no `uses:` or `run:` line makes
/// a release (`releaseId` is refused on any line).
fn structure_problems(workflow: &str) -> Vec<String> {
    let mut problems = Vec::new();

    let runners: Vec<&str> = workflow
        .lines()
        .map(code_of)
        .filter(|code| code.starts_with("runs-on:"))
        .collect();
    if runners.is_empty() {
        problems.push("no runs-on".to_owned());
    }
    for runner in runners.iter().filter(|r| **r != "runs-on: windows-latest") {
        problems.push(format!("runner is not windows-latest: {runner}"));
    }

    let jobs = jobs(workflow);
    let find = |name: &str| jobs.iter().find(|(job, _)| job == name);
    if find("test").is_none() {
        problems.push("no test job".to_owned());
    }
    match find("build") {
        None => problems.push("no build job".to_owned()),
        Some((_, lines)) => {
            if !lines
                .iter()
                .any(|line| indent_of(line) == 4 && code_of(line) == "needs: test")
            {
                problems.push("the build job does not have needs: test".to_owned());
            }
            let code: Vec<&str> = lines.iter().map(|line| code_of(line)).collect();
            if !code.iter().any(|c| {
                c.trim_start_matches("- ")
                    .starts_with("uses: actions/upload-artifact@")
            }) {
                problems.push("the build job does not upload an artifact".to_owned());
            }
            if code.iter().any(|c| c.contains("Swatinem/rust-cache")) {
                problems.push("the build job restores a cache".to_owned());
            }
        }
    }

    match top_level_block(workflow, "concurrency:") {
        None => problems.push("no concurrency".to_owned()),
        Some(block) => {
            let group = "group: ${{ github.workflow }}-${{ github.ref }}";
            if !block.iter().any(|line| code_of(line) == group) {
                problems.push(format!("the concurrency group is not `{group}`"));
            }
        }
    }

    for reference in action_references(workflow) {
        if makes_a_release(&reference) {
            problems.push(format!("release step: uses: {reference}"));
        }
    }
    for script in run_scripts(workflow) {
        for line in script.lines().filter(|line| makes_a_release(line)) {
            problems.push(format!("release step: run: {line}"));
        }
    }
    for line in workflow.lines().filter(|line| !is_comment(line)) {
        if code_of(line).contains("releaseId") {
            problems.push(format!("release step: {}", line.trim()));
        }
    }
    problems
}

/// The value of every `key:` line (`- ` and a trailing comment removed).
fn values_of<'a>(workflow: &'a str, key: &str) -> Vec<&'a str> {
    workflow
        .lines()
        .filter(|line| !is_comment(line))
        .map(|line| code_of(line).trim_start_matches("- "))
        .filter_map(|code| code.strip_prefix(key)?.strip_prefix(':'))
        .map(|value| value.trim().trim_matches(['\'', '"']))
        .collect()
}

/// What is wrong with three settings: `defaults.run.shell` is `bash` (so the
/// quoting in the scripts means the same on every step), every
/// `node-version` is a number of at least 22 (so the quoted glob of
/// `node --test` is expanded by node), and every `if-no-files-found` is
/// `error` (uploading nothing must fail the run); at least one of each of the
/// last two must exist.
fn setting_problems(workflow: &str) -> Vec<String> {
    let mut problems = Vec::new();
    let defaults: Vec<String> = top_level_block(workflow, "defaults:")
        .unwrap_or_default()
        .iter()
        .map(|line| squeezed(code_of(line)))
        .collect();
    if defaults != ["run:", "shell: bash"] {
        problems.push(format!("defaults are {defaults:?}, not run.shell: bash"));
    }
    let versions = values_of(workflow, "node-version");
    if versions.is_empty() {
        problems.push("no node-version".to_owned());
    }
    for version in versions {
        let major: String = version.chars().take_while(char::is_ascii_digit).collect();
        if major.parse::<u32>().is_ok_and(|major| major >= 22) {
            continue;
        }
        problems.push(format!("node-version is not 22 or newer: {version}"));
    }
    let uploads = values_of(workflow, "if-no-files-found");
    if uploads.is_empty() {
        problems.push("no if-no-files-found".to_owned());
    }
    for value in uploads.into_iter().filter(|value| *value != "error") {
        problems.push(format!("if-no-files-found is {value}, not error"));
    }
    problems
}

const REQUIRED_COMMANDS: &[&str] = &[
    "cargo fmt --all --check",
    "cargo clippy --workspace --all-targets -- -D warnings",
    "cargo test --workspace",
    "node --test \"tests/ui/*.test.js\"",
    "cargo tauri build",
];

// ---------------------------------------------------------------------
// The real file
// ---------------------------------------------------------------------

#[test]
fn every_action_in_the_workflow_is_pinned_to_a_commit_with_a_release_comment() {
    let workflow = workflow();
    assert_eq!(unpinned_actions(&workflow), Vec::<String>::new());
    // Not vacuous: checkout, toolchain, cache, node and upload are used, and
    // only the references the scan actually parsed are counted.
    let found = action_references(&workflow).len();
    assert!(found >= 5, "only {found} action references were parsed");
}

#[test]
fn the_workflow_may_only_read_the_repository() {
    assert_eq!(permission_problems(&workflow()), Vec::<String>::new());
}

#[test]
fn the_workflow_uses_no_secrets() {
    assert_eq!(secret_references(&workflow()), Vec::<String>::new());
}

#[test]
fn the_workflow_starts_by_hand_or_on_a_version_tag_only() {
    assert_eq!(trigger_problems(&workflow()), Vec::<String>::new());
}

#[test]
fn the_scan_finds_the_run_scripts_of_the_real_file() {
    // Two line-ending steps, fmt, clippy, test, node, install and build.
    let found = run_scripts(&workflow()).len();
    assert!(found >= 8, "only {found} run scripts were found");
}

#[test]
fn the_workflow_injects_nothing_into_a_script() {
    assert_eq!(injection_problems(&workflow()), Vec::<String>::new());
}

#[test]
fn the_workflow_runs_the_checks_and_builds_the_bundle() {
    assert_eq!(
        missing_commands(&workflow(), REQUIRED_COMMANDS),
        Vec::<String>::new()
    );
}

#[test]
fn the_workflow_runs_on_windows_builds_after_the_tests_and_uploads_the_bundle() {
    let workflow = workflow();
    assert_eq!(structure_problems(&workflow), Vec::<String>::new());
    // The installer comes from the bundle folder.
    assert!(workflow.contains("target/release/bundle/"), "{workflow}");
}

#[test]
fn the_workflow_pins_its_shell_node_version_and_upload_failure_mode() {
    assert_eq!(setting_problems(&workflow()), Vec::<String>::new());
}

// ---------------------------------------------------------------------
// Fixtures the checks must catch
// ---------------------------------------------------------------------

const PIN: &str = "11bd71901bbe5b1630ceea73d27597364c9af683";

#[test]
fn an_action_pinned_to_a_tag_or_branch_is_caught_in_a_fixture() {
    for bad in [
        "      - uses: actions/checkout@v4",
        "      - uses: actions/checkout@main",
        "      - uses: actions/checkout",
        "      - uses: actions/checkout@11bd719",
        // 41 characters, and 40 with an upper-case digit.
        "      - uses: actions/checkout@11bd71901bbe5b1630ceea73d27597364c9af6830 # v4.2.2",
        "      - uses: actions/checkout@11BD71901bbe5b1630ceea73d27597364c9af683 # v4.2.2",
        "      - uses: ./local-action",
        "      - uses: docker://alpine:3.20",
    ] {
        assert_eq!(unpinned_actions(bad).len(), 1, "{bad}");
    }
    // Pinned, but without the comment that names the release.
    let no_comment = format!("      - uses: actions/checkout@{PIN}");
    assert_eq!(unpinned_actions(&no_comment).len(), 1);
    // A reference the line-by-line scan could not see.
    assert_eq!(
        unpinned_actions("      - name: x\n        with: { uses: a/b@v1 }").len(),
        1
    );
}

#[test]
fn an_action_pinned_to_a_commit_with_a_release_comment_passes_a_fixture() {
    let pinned = format!("      - uses: actions/checkout@{PIN} # v4.2.2");
    assert!(unpinned_actions(&pinned).is_empty());
    let first_key = format!("        uses: actions/cache/save@{PIN} # v4.2.0");
    assert!(unpinned_actions(&first_key).is_empty());
    // A comment line that mentions `uses:` is not a reference.
    assert!(unpinned_actions("# uses: actions/checkout@v4 is not allowed").is_empty());
}

#[test]
fn an_unparsed_uses_is_not_counted_as_a_reference_in_a_fixture() {
    let text = "      - name: x\n        with: { uses: a/b@v1 }\n# uses: c/d@v1\n";
    assert_eq!(action_references(text).len(), 0);
    let two = format!(
        "      - uses: actions/checkout@{PIN} # v4\n        uses: a/b@v1\n# uses: c/d@v1\n"
    );
    assert_eq!(action_references(&two).len(), 2);
}

#[test]
fn a_write_permission_or_a_missing_permissions_block_is_caught_in_a_fixture() {
    let write = "name: x\npermissions:\n  contents: write\njobs:\n  a:\n    runs-on: x\n";
    assert_eq!(permission_problems(write).len(), 2);
    let extra = "permissions:\n  contents: read\n  id-token: write\njobs:\n  a: {}\n";
    assert_eq!(permission_problems(extra).len(), 2);
    let all = "permissions: write-all\njobs:\n  a: {}\n";
    assert!(!permission_problems(all).is_empty());
    let none = "name: x\njobs:\n  a: {}\n";
    assert_eq!(
        permission_problems(none),
        ["no top-level permissions block"]
    );
    // Read-only at the top, but a job asks for more.
    let job =
        "permissions:\n  contents: read\njobs:\n  a:\n    permissions:\n      contents: write\n";
    assert_eq!(permission_problems(job).len(), 1);
    // Wider than read is not allowed either.
    let wide = "permissions:\n  contents: read\n  packages: read\njobs:\n  a: {}\n";
    assert_eq!(permission_problems(wide).len(), 1);
}

#[test]
fn a_write_permission_with_odd_spacing_is_caught_in_a_fixture() {
    for spacing in [
        "contents:  write",
        "contents:write",
        "contents:\twrite",
        "{contents: write}",
        "{ contents:write }",
    ] {
        let job = format!(
            "permissions:\n  contents: read\njobs:\n  a:\n    permissions:\n      {spacing}\n"
        );
        let problems = permission_problems(&job);
        assert!(
            problems.iter().any(|p| p.starts_with("write permission")),
            "{spacing}: {problems:?}"
        );
    }
    let top = "permissions:\n  contents:write\njobs:\n  a: {}\n";
    assert!(!permission_problems(top).is_empty());
}

#[test]
fn read_only_contents_permission_passes_a_fixture() {
    let ok = "name: x\n\npermissions:\n  contents: read # nothing else\n\njobs:\n  a: {}\n";
    assert!(permission_problems(ok).is_empty());
    // Extra spaces do not make a read permission something else.
    let spaced = "permissions:\n  contents:   read\njobs:\n  a: {}\n";
    assert!(permission_problems(spaced).is_empty());
}

#[test]
fn a_secret_reference_is_caught_in_a_fixture() {
    for bad in [
        "        env:\n          TOKEN: ${{ secrets.GITHUB_TOKEN }}",
        "      password: ${{secrets.NPM}}",
        "    secrets: inherit",
        "      password: ${{ secrets['NPM'] }}",
        "      all: ${{ toJSON(secrets) }}",
    ] {
        assert_eq!(secret_references(bad).len(), 1, "{bad}");
    }
}

#[test]
fn a_token_reference_is_caught_in_a_fixture() {
    for bad in [
        "      token: ${{ github.token }}",
        "        GITHUB_TOKEN: abc",
        "        GitHub_Token: abc",
        "      run: echo $GITHUB_TOKEN",
    ] {
        assert_eq!(secret_references(bad).len(), 1, "{bad}");
    }
}

#[test]
fn a_workflow_without_secret_words_passes_a_fixture() {
    let ok = "# no secrets.TOKEN here\n  contents: read\n  name: mysecretsauce\n  x: secrets_manager\n  y: 1 # secrets.X\n";
    assert!(secret_references(ok).is_empty());
}

#[test]
fn a_pull_request_trigger_or_a_branch_push_is_caught_in_a_fixture() {
    let target = "on:\n  pull_request_target:\n    types: [opened]\njobs:\n  a: {}\n";
    assert!(!trigger_problems(target).is_empty());
    let pr =
        "on:\n  workflow_dispatch:\n  pull_request:\n  push:\n    tags:\n      - 'v*'\njobs:\n";
    assert_eq!(trigger_problems(pr).len(), 1);
    let branches = "on:\n  workflow_dispatch:\n  push:\n    branches:\n      - main\n    tags:\n      - 'v*'\njobs:\n";
    assert_eq!(trigger_problems(branches).len(), 2);
    let no_tag_filter = "on:\n  workflow_dispatch:\n  push:\njobs:\n";
    assert_eq!(
        trigger_problems(no_tag_filter),
        ["push has no v* tag filter"]
    );
    let only_push = "on:\n  push:\n    tags:\n      - 'v*'\njobs:\n";
    assert_eq!(trigger_problems(only_push).len(), 1);
    assert_eq!(trigger_problems("jobs:\n  a: {}\n"), ["no on: block"]);
}

#[test]
fn manual_start_and_a_version_tag_pass_a_fixture() {
    let ok = "on:\n  workflow_dispatch:\n  push:\n    tags:\n      - 'v*'\njobs:\n  a: {}\n";
    assert!(trigger_problems(ok).is_empty());
    let double_quoted = "on:\n  push:\n    tags:\n      - \"v*\"\n  workflow_dispatch:\n";
    assert!(trigger_problems(double_quoted).is_empty());
}

#[test]
fn an_expression_in_a_script_is_caught_in_a_fixture() {
    let inline = "    steps:\n      - run: echo ${{ github.event.pull_request.title }}\n";
    assert_eq!(injection_problems(inline).len(), 2);
    let block = "      - name: x\n        run: |\n          echo start\n          echo \"${{ github.ref_name }}\"\n      - run: ls\n";
    assert_eq!(injection_problems(block).len(), 1);
    // Not a run script, so only the github.event rule applies.
    let event_elsewhere = "      - run: ls\n      - uses: a/b@x\n        with:\n          name: ${{ github.event.head_commit.message }}\n";
    assert_eq!(injection_problems(event_elsewhere).len(), 1);
}

#[test]
fn a_script_without_expressions_passes_a_fixture() {
    let ok = "      - run: cargo test --workspace\n      - name: x\n        run: |\n          node --test \"tests/ui/*.test.js\"\n      - uses: a/b@x\n        with:\n          name: ${{ runner.os }}-bundle\n";
    assert!(injection_problems(ok).is_empty());
}

#[test]
fn a_workflow_without_run_scripts_is_reported_in_a_fixture() {
    let none = "jobs:\n  a:\n    steps:\n      - uses: a/b@x\n";
    assert_eq!(run_scripts(none).len(), 0);
    assert_eq!(injection_problems(none), ["no run scripts found"]);
    // With nothing required, a workflow without scripts is still reported.
    assert_eq!(missing_commands(none, &[]), ["no run scripts found"]);
    assert_eq!(
        missing_commands(none, &["cargo test --workspace"]),
        ["no run scripts found", "missing: cargo test --workspace"]
    );
}

#[test]
fn a_missing_command_is_caught_in_a_fixture() {
    let only_comment = "# cargo test --workspace\n      - run: cargo fmt --all --check\n";
    assert_eq!(
        missing_commands(
            only_comment,
            &["cargo test --workspace", "cargo fmt --all --check"]
        ),
        ["missing: cargo test --workspace"]
    );
    // A command that only appears in a `name:` or an `env:` is not run.
    let named = "      - name: cargo test --workspace\n        run: echo hello\n";
    assert_eq!(
        missing_commands(named, &["cargo test --workspace"]),
        ["missing: cargo test --workspace"]
    );
}

#[test]
fn commands_in_one_line_and_block_scripts_are_found_in_a_fixture() {
    let ok = "      - run: cargo test --workspace\n      - name: x\n        shell: bash\n        run: |\n          node --test \"tests/ui/*.test.js\"\n";
    assert!(
        missing_commands(
            ok,
            &[
                "cargo test --workspace",
                "node --test \"tests/ui/*.test.js\""
            ]
        )
        .is_empty()
    );
}

#[test]
fn block_scripts_of_every_style_are_read_in_a_fixture() {
    let text = "      - run: echo a\n      - run: |-\n          b\n\n          c\n      - run: >\n          d\n          e\n      - run: >-\n          f\n      - run: |+\n          g\n      - run: |\n          h\n        shell: bash\n      - name: n\n        run: |\n          i\n        env:\n          X: y\n";
    assert_eq!(
        run_scripts(text),
        ["echo a", "b\n\nc", "d\ne", "f", "g", "h", "i"]
    );
}

#[test]
fn a_command_only_in_a_comment_inside_a_block_script_is_not_present_in_a_fixture() {
    let text = "      - run: |\n          # cargo test --workspace\n          cargo fmt --all --check # cargo clippy\n";
    assert_eq!(run_scripts(text), ["cargo fmt --all --check"]);
    assert_eq!(
        missing_commands(
            text,
            &[
                "cargo fmt --all --check",
                "cargo test --workspace",
                "cargo clippy"
            ]
        ),
        ["missing: cargo test --workspace", "missing: cargo clippy"]
    );
}

/// A workflow the structure checks accept. The variants below change one
/// thing each. It holds a `--release` build and a `target/release` path,
/// which are not release steps.
fn good_structure() -> String {
    [
        "name: x",
        "concurrency:",
        "  group: ${{ github.workflow }}-${{ github.ref }}",
        "  cancel-in-progress: true",
        "jobs:",
        "  test:",
        "    runs-on: windows-latest",
        "    steps:",
        "      - uses: Swatinem/rust-cache@x # v2",
        "      - run: cargo build --release",
        "  build:",
        "    needs: test",
        "    runs-on: windows-latest",
        "    steps:",
        "      - uses: actions/upload-artifact@x # v7",
        "        with:",
        "          path: target/release/bundle/nsis/*.exe",
        "",
    ]
    .join("\n")
}

#[test]
fn a_well_formed_workflow_passes_the_structure_checks_in_a_fixture() {
    assert_eq!(structure_problems(&good_structure()), Vec::<String>::new());
}

#[test]
fn a_runner_that_is_not_windows_latest_is_caught_in_a_fixture() {
    let one = good_structure().replacen("windows-latest", "ubuntu-latest", 1);
    assert_eq!(structure_problems(&one).len(), 1);
    let both = good_structure().replace("windows-latest", "ubuntu-latest");
    assert_eq!(structure_problems(&both).len(), 2);
    let none = good_structure().replace("    runs-on: windows-latest\n", "");
    assert!(!structure_problems(&none).is_empty());
}

#[test]
fn a_build_job_without_needs_test_is_caught_in_a_fixture() {
    let text = good_structure().replace("    needs: test\n", "");
    assert_eq!(structure_problems(&text).len(), 1);
}

#[test]
fn needs_test_on_the_wrong_job_is_caught_in_a_fixture() {
    // `needs: test` is on some line, but the build job does not have it.
    let text = good_structure()
        .replace("    needs: test\n", "")
        .replace("  test:\n", "  test:\n    needs: test\n");
    assert!(text.contains("needs: test"));
    assert_eq!(structure_problems(&text).len(), 1);
    let renamed = good_structure().replace("  build:\n", "  package:\n");
    assert!(!structure_problems(&renamed).is_empty());
}

#[test]
fn a_build_job_without_an_artifact_upload_is_caught_in_a_fixture() {
    let none = good_structure().replace("upload-artifact", "download-artifact");
    assert_eq!(structure_problems(&none).len(), 1);
    // An upload in the test job does not count for the build job.
    let wrong_job = good_structure()
        .replace("      - uses: actions/upload-artifact@x # v7\n", "")
        .replace(
            "      - run: cargo build --release\n",
            "      - run: cargo build --release\n      - uses: actions/upload-artifact@x # v7\n",
        );
    assert_eq!(structure_problems(&wrong_job).len(), 1);
}

#[test]
fn a_missing_concurrency_or_a_group_without_the_workflow_name_is_caught_in_a_fixture() {
    let none = good_structure().replace(
        "concurrency:\n  group: ${{ github.workflow }}-${{ github.ref }}\n  cancel-in-progress: true\n",
        "",
    );
    assert_eq!(structure_problems(&none).len(), 1);
    let group = good_structure().replace("${{ github.workflow }}-", "windows-");
    assert_eq!(structure_problems(&group).len(), 1);
}

#[test]
fn a_release_step_is_caught_in_a_fixture() {
    for step in [
        "      - uses: softprops/action-gh-release@x # v2",
        "      - uses: actions/create-release@x # v1",
        "      - uses: ncipollo/release-action@x # v1",
        "      - uses: actions/upload-release-asset@x # v1",
        "      - uses: tauri-apps/tauri-action@x # v0",
        "      - run: gh release create v1 --notes x",
        "      - run: |\n          echo start\n          gh release upload v1 a.exe",
        "      - run: gh api repos/o/r/releases -f tag_name=v1",
        "      - run: curl -X POST https://example.com/releases",
    ] {
        let text = format!("{}{step}\n", good_structure());
        assert_eq!(structure_problems(&text).len(), 1, "{step}");
    }
}

#[test]
fn a_build_job_that_restores_a_cache_is_caught_in_a_fixture() {
    let text = format!(
        "{}      - uses: Swatinem/rust-cache@x # v2\n",
        good_structure()
    );
    assert_eq!(structure_problems(&text).len(), 1);
}

fn good_settings() -> String {
    [
        "defaults:",
        "  run:",
        "    shell: bash",
        "jobs:",
        "  a:",
        "    steps:",
        "      - uses: actions/setup-node@x # v7",
        "        with:",
        "          node-version: 22",
        "      - uses: actions/upload-artifact@x # v7",
        "        with:",
        "          if-no-files-found: error",
        "",
    ]
    .join("\n")
}

#[test]
fn settings_that_are_right_pass_a_fixture() {
    assert_eq!(setting_problems(&good_settings()), Vec::<String>::new());
    let newer = good_settings().replace("node-version: 22", "node-version: '24.1'");
    assert!(setting_problems(&newer).is_empty());
}

#[test]
fn a_shell_other_than_bash_or_no_defaults_is_caught_in_a_fixture() {
    let pwsh = good_settings().replace("shell: bash", "shell: pwsh");
    assert_eq!(setting_problems(&pwsh).len(), 1);
    let none = good_settings().replace("defaults:\n  run:\n    shell: bash\n", "");
    assert_eq!(setting_problems(&none).len(), 1);
}

#[test]
fn an_old_or_unreadable_node_version_is_caught_in_a_fixture() {
    for bad in ["20", "'21.9'", "'lts/*'", "latest"] {
        let text = good_settings().replace("node-version: 22", &format!("node-version: {bad}"));
        assert_eq!(setting_problems(&text).len(), 1, "{bad}");
    }
    let none = good_settings().replace("          node-version: 22\n", "");
    assert_eq!(setting_problems(&none).len(), 1);
}

#[test]
fn an_upload_that_ignores_missing_files_is_caught_in_a_fixture() {
    for value in ["warn", "ignore"] {
        let text = good_settings().replace(
            "if-no-files-found: error",
            &format!("if-no-files-found: {value}"),
        );
        assert_eq!(setting_problems(&text).len(), 1, "{value}");
    }
    let none = good_settings().replace("          if-no-files-found: error\n", "");
    assert_eq!(setting_problems(&none).len(), 1);
}
