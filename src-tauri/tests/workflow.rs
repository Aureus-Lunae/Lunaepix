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

/// What is wrong with the permissions: the top-level block must be exactly
/// `contents: read`, and no `write` permission may appear anywhere, at the
/// top or in a job (`write-all` included).
fn permission_problems(workflow: &str) -> Vec<String> {
    let mut problems = Vec::new();
    match top_level_block(workflow, "permissions:") {
        None => problems.push("no top-level permissions block".to_owned()),
        Some(block) => {
            let entries: Vec<&str> = block.iter().map(|line| code_of(line)).collect();
            if entries != ["contents: read"] {
                problems.push(format!("top-level permissions are {entries:?}"));
            }
        }
    }
    for line in workflow.lines().filter(|line| !is_comment(line)) {
        let code = code_of(line);
        if code.contains(": write") || code.contains("write-all") {
            problems.push(format!("write permission: {code}"));
        }
    }
    problems
}

/// Every place the workflow could reach a repository secret.
fn secret_references(workflow: &str) -> Vec<String> {
    workflow
        .lines()
        .filter(|line| !is_comment(line))
        .map(code_of)
        .filter(|code| code.contains("secrets.") || code.starts_with("secrets:"))
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

/// The text of every `run:` step, whether one line or a `|` block.
fn run_scripts(workflow: &str) -> Vec<String> {
    let lines: Vec<&str> = workflow.lines().collect();
    let mut scripts = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let key = lines[i].trim_start().trim_start_matches("- ");
        if let Some(value) = key.strip_prefix("run:").filter(|_| !is_comment(lines[i])) {
            let value = code_of(value);
            if matches!(value, "|" | ">" | "|-" | ">-") {
                let key_indent = indent_of(lines[i]);
                let mut body = Vec::new();
                while i + 1 < lines.len()
                    && (lines[i + 1].trim().is_empty() || indent_of(lines[i + 1]) > key_indent)
                {
                    i += 1;
                    body.push(lines[i].trim());
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
/// not used anywhere.
fn injection_problems(workflow: &str) -> Vec<String> {
    let mut problems: Vec<String> = run_scripts(workflow)
        .into_iter()
        .filter(|script| script.contains("${{"))
        .map(|script| format!("expression in a run script: {script}"))
        .collect();
    for line in workflow.lines().filter(|line| !is_comment(line)) {
        if code_of(line).contains("github.event") {
            problems.push(format!("github.event used: {}", line.trim()));
        }
    }
    problems
}

/// The commands in `required` that no `run:` script contains.
fn missing_commands<'a>(workflow: &str, required: &[&'a str]) -> Vec<&'a str> {
    let scripts = run_scripts(workflow).join("\n");
    required
        .iter()
        .copied()
        .filter(|command| !scripts.contains(command))
        .collect()
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
    // Not vacuous: checkout, toolchain, cache, node and upload are used.
    let uses = workflow.lines().filter(|l| l.contains("uses:")).count();
    assert!(uses >= 5, "only {uses} uses: lines were found");
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
fn the_workflow_injects_nothing_into_a_script() {
    assert_eq!(injection_problems(&workflow()), Vec::<String>::new());
}

#[test]
fn the_workflow_runs_the_checks_and_builds_the_bundle() {
    assert_eq!(
        missing_commands(&workflow(), REQUIRED_COMMANDS),
        Vec::<&str>::new()
    );
}

#[test]
fn the_workflow_runs_on_windows_builds_after_the_tests_and_uploads_the_bundle() {
    let workflow = workflow();
    let runners: Vec<&str> = workflow
        .lines()
        .map(code_of)
        .filter(|code| code.starts_with("runs-on:"))
        .collect();
    assert_eq!(
        runners,
        ["runs-on: windows-latest", "runs-on: windows-latest"]
    );
    // The build job waits for the test job.
    assert!(
        workflow
            .lines()
            .map(code_of)
            .any(|code| code == "needs: test")
    );
    // The installer is only uploaded as an artifact, from the bundle folder.
    assert!(workflow.contains("target/release/bundle/"), "{workflow}");
    assert!(workflow.contains("actions/upload-artifact@"));
    // No release is made or published.
    for forbidden in [
        "tauri-action",
        "gh release",
        "action-gh-release",
        "releaseId",
    ] {
        assert!(!workflow.contains(forbidden), "{forbidden}");
    }
    // The pull request trigger is absent, and the runs are queued per ref.
    assert!(workflow.lines().any(|l| code_of(l) == "concurrency:"));
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
fn read_only_contents_permission_passes_a_fixture() {
    let ok = "name: x\n\npermissions:\n  contents: read # nothing else\n\njobs:\n  a: {}\n";
    assert!(permission_problems(ok).is_empty());
}

#[test]
fn a_secret_reference_is_caught_in_a_fixture() {
    for bad in [
        "        env:\n          TOKEN: ${{ secrets.GITHUB_TOKEN }}",
        "      password: ${{secrets.NPM}}",
        "    secrets: inherit",
    ] {
        assert_eq!(secret_references(bad).len(), 1, "{bad}");
    }
    assert!(secret_references("# no secrets.TOKEN here\n  contents: read").is_empty());
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
    let event_elsewhere = "      - uses: a/b@x\n        with:\n          name: ${{ github.event.head_commit.message }}\n";
    assert_eq!(injection_problems(event_elsewhere).len(), 1);
}

#[test]
fn a_script_without_expressions_passes_a_fixture() {
    let ok = "      - run: cargo test --workspace\n      - name: x\n        run: |\n          node --test \"tests/ui/*.test.js\"\n      - uses: a/b@x\n        with:\n          name: ${{ runner.os }}-bundle\n";
    assert!(injection_problems(ok).is_empty());
}

#[test]
fn a_missing_command_is_caught_in_a_fixture() {
    let only_comment = "# cargo test --workspace\n      - run: cargo fmt --all --check\n";
    assert_eq!(
        missing_commands(
            only_comment,
            &["cargo test --workspace", "cargo fmt --all --check"]
        ),
        ["cargo test --workspace"]
    );
    // A command that only appears in a `name:` or an `env:` is not run.
    let named = "      - name: cargo test --workspace\n        run: echo hello\n";
    assert_eq!(
        missing_commands(named, &["cargo test --workspace"]).len(),
        1
    );
    assert_eq!(
        run_scripts("      - run: echo a\n      - run: |\n          b\n          c\n"),
        ["echo a", "b\nc"]
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
