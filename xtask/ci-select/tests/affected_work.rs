//! Checked Git and path-selection boundaries for ordinary verification.

use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::Path;
use std::process::{Command, Output};
use tempfile::TempDir;
use webpkit_ci_core::{AffectedWork, CodeqlScope, QualityWork, affected_work};

const WORKFLOW_ONLY: AffectedWork = AffectedWork {
    quality: QualityWork::Workflows,
    codeql: CodeqlScope::Actions,
};
const FULL_RUST: AffectedWork = AffectedWork {
    quality: QualityWork::Full,
    codeql: CodeqlScope::Rust,
};
const FULL_ACTIONS: AffectedWork = AffectedWork {
    quality: QualityWork::Full,
    codeql: CodeqlScope::Actions,
};
const WORKFLOW_BOTH: AffectedWork = AffectedWork {
    quality: QualityWork::Workflows,
    codeql: CodeqlScope::Both,
};
const FULL_BOTH: AffectedWork = AffectedWork {
    quality: QualityWork::Full,
    codeql: CodeqlScope::Both,
};

#[test]
fn release_workflow_uses_the_small_gate_and_actions_codeql() {
    assert_eq!(
        affected_work(&[".github/workflows/release-plz.yml"]),
        WORKFLOW_ONLY
    );
}

#[test]
fn ordinary_quality_workflow_requires_full_quality_and_both_codeql_languages() {
    assert_eq!(affected_work(&[".github/workflows/ci.yml"]), FULL_BOTH);
}

#[test]
fn codeql_workflow_uses_workflow_checks_and_both_codeql_languages() {
    assert_eq!(
        affected_work(&[".github/workflows/codeql.yml"]),
        WORKFLOW_BOTH
    );
}

#[test]
fn rust_source_uses_full_quality_and_rust_codeql() {
    assert_eq!(affected_work(&["crates/webpkit/src/lib.rs"]), FULL_RUST);
}

#[test]
fn action_definition_uses_full_quality_and_actions_codeql() {
    assert_eq!(
        affected_work(&[".github/actions/setup/action.yml"]),
        FULL_ACTIONS
    );
}

#[test]
fn unknown_action_metadata_and_nested_files_fail_safe_to_both_languages() {
    for path in [
        ".github/actions/setup/script.js",
        ".github/actions/action.yml",
        ".github/actions/setup/nested/metadata.yml",
        "crates/webpkit/src/upper.RS",
    ] {
        assert_eq!(affected_work(&[path]), FULL_BOTH, "path: {path}");
    }
}

#[test]
fn mixed_workflows_and_rust_use_full_quality_and_both_codeql_languages() {
    assert_eq!(
        affected_work(&[
            ".github/workflows/release-plz.yml",
            "crates/webpkit/src/lib.rs"
        ]),
        FULL_BOTH
    );
}

#[test]
fn empty_unknown_and_malformed_path_sets_fail_safe_to_full() {
    for paths in [
        &[][..],
        &["README.md"][..],
        &["../.github/workflows/release-plz.yml"][..],
        &[".github\\workflows\\release-plz.yml"][..],
        &["/absolute/path.yml"][..],
        &[".github/workflows/.yml"][..],
        &[""][..],
    ] {
        assert_eq!(affected_work(paths), FULL_BOTH);
    }
}

#[test]
fn adding_any_unclassified_path_disables_the_workflow_only_gate() {
    assert_eq!(
        affected_work(&[".github/workflows/release-plz.yml", "docs/ci.md"]),
        FULL_BOTH
    );
}

struct RepositoryFixture {
    directory: TempDir,
    base: String,
}

impl RepositoryFixture {
    fn new() -> io::Result<Self> {
        let directory = TempDir::new()?;
        let root = directory.path();
        git(root, ["init", "--quiet"])?;
        git(root, ["config", "user.name", "Affected-work fixture"])?;
        git(
            root,
            ["config", "user.email", "affected-work@example.invalid"],
        )?;
        fs::create_dir_all(root.join(".fixture-hooks"))?;
        git(root, ["config", "core.hooksPath", ".fixture-hooks"])?;
        git(root, ["config", "commit.gpgsign", "false"])?;
        git(root, ["config", "core.fsmonitor", "false"])?;
        write(
            root,
            "Justfile",
            "check:\n    @echo full-gate\n\ncheck-workflows:\n    @echo workflow-gate\n",
        )?;
        write(root, ".github/workflows/release-plz.yml", "name: fixture\n")?;
        write(root, ".github/workflows/ci.yml", "name: ordinary\n")?;
        write(root, ".github/actions/setup/action.yml", "name: setup\n")?;
        write(root, "notes/rename-input.txt", "rename fixture\n")?;
        git(root, ["add", "--all"])?;
        git(
            root,
            [
                "commit",
                "--quiet",
                "-m",
                "test: initialize selector fixture",
            ],
        )?;
        let base = output(&git(root, ["rev-parse", "HEAD"])?)?;
        git(
            root,
            ["update-ref", "refs/remotes/origin/main", base.as_str()],
        )?;
        Ok(Self { directory, base })
    }

    fn root(&self) -> &Path {
        self.directory.path()
    }

    fn change_and_commit(&self, path: &str, contents: &str) -> io::Result<String> {
        write(self.root(), path, contents)?;
        git(self.root(), ["add", "--all"])?;
        git(
            self.root(),
            ["commit", "--quiet", "-m", "test: change selector fixture"],
        )?;
        output(&git(self.root(), ["rev-parse", "HEAD"])?)
    }
}

fn write(root: &Path, path: &str, contents: &str) -> io::Result<()> {
    let destination = root.join(path);
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(destination, contents)
}

fn git<const N: usize>(root: &Path, arguments: [&str; N]) -> io::Result<Output> {
    let mut command = Command::new("git");
    isolate_git_environment(&mut command);
    let output = command.current_dir(root).args(arguments).output()?;
    if !output.status.success() {
        return Err(io::Error::other(format!(
            "Git fixture command failed: {}",
            String::from_utf8_lossy(&output.stderr)
        )));
    }
    Ok(output)
}

fn isolate_git_environment(command: &mut Command) {
    let inherited = std::env::vars_os().map(|(name, _)| name);
    let configured = command.get_envs().map(|(name, _)| name.to_os_string());
    let names = inherited
        .chain(configured)
        .filter(is_git_environment_name)
        .collect::<Vec<_>>();
    for name in names {
        command.env_remove(name);
    }
}

fn is_git_environment_name(name: &OsString) -> bool {
    name.to_string_lossy()
        .to_ascii_uppercase()
        .starts_with("GIT_")
}

fn output(result: &Output) -> io::Result<String> {
    String::from_utf8(result.stdout.clone())
        .map(|stdout| stdout.trim().to_owned())
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

fn cli(root: &Path, arguments: &[&str]) -> io::Result<Output> {
    let mut command = Command::new(env!("CARGO_BIN_EXE_ci-select"));
    isolate_git_environment(&mut command);
    command
        .current_dir(root)
        .args(arguments)
        .env_remove("CI_BASE_REVISION")
        .env_remove("CI_HEAD_REVISION")
        .output()
}

fn cli_with_environment(
    root: &Path,
    arguments: &[&str],
    base: &str,
    head: &str,
) -> io::Result<Output> {
    let mut command = Command::new(env!("CARGO_BIN_EXE_ci-select"));
    isolate_git_environment(&mut command);
    command
        .current_dir(root)
        .args(arguments)
        .env("CI_BASE_REVISION", base)
        .env("CI_HEAD_REVISION", head)
        .output()
}

fn cli_with_injected_git_config(root: &Path, arguments: &[&str]) -> io::Result<Output> {
    let mut command = Command::new(env!("CARGO_BIN_EXE_ci-select"));
    command
        .env("GIT_CONFIG_COUNT", "1")
        .env("GIT_CONFIG_KEY_0", "core.worktree")
        .env("GIT_CONFIG_VALUE_0", "nonexistent-fixture-worktree");
    isolate_git_environment(&mut command);
    command
        .current_dir(root)
        .args(arguments)
        .env_remove("CI_BASE_REVISION")
        .env_remove("CI_HEAD_REVISION")
        .output()
}

#[test]
fn codeql_matrix_is_only_json_and_uses_the_complete_workflow_diff() -> io::Result<()> {
    let fixture = RepositoryFixture::new()?;
    let head = fixture.change_and_commit(
        ".github/workflows/release-plz.yml",
        "name: changed fixture\n",
    )?;
    let result = cli(
        fixture.root(),
        &["codeql-matrix", "--base", &fixture.base, "--head", &head],
    )?;
    assert!(
        result.status.success(),
        "selector failed: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(
        output(&result)?,
        "{\"include\":[{\"language\":\"actions\",\"build-mode\":\"none\"}]}"
    );
    Ok(())
}

#[test]
fn codeql_matrix_uses_configured_base_and_head_when_flags_are_absent() -> io::Result<()> {
    let fixture = RepositoryFixture::new()?;
    let head = fixture.change_and_commit(
        ".github/workflows/release-plz.yml",
        "name: changed fixture\n",
    )?;
    let result = cli_with_environment(fixture.root(), &["codeql-matrix"], &fixture.base, &head)?;
    assert!(result.status.success());
    assert_eq!(
        output(&result)?,
        "{\"include\":[{\"language\":\"actions\",\"build-mode\":\"none\"}]}"
    );
    Ok(())
}

#[test]
fn selector_fixture_removes_all_injected_git_configuration_before_running() -> io::Result<()> {
    let fixture = RepositoryFixture::new()?;
    let head = fixture.change_and_commit(
        ".github/workflows/release-plz.yml",
        "name: changed fixture\n",
    )?;
    let result = cli_with_injected_git_config(
        fixture.root(),
        &["codeql-matrix", "--base", &fixture.base, "--head", &head],
    )?;
    assert!(result.status.success());
    assert_eq!(
        output(&result)?,
        "{\"include\":[{\"language\":\"actions\",\"build-mode\":\"none\"}]}"
    );
    Ok(())
}

#[test]
fn explicit_flags_override_environment_and_invalid_recipe_status_fails() -> io::Result<()> {
    let fixture = RepositoryFixture::new()?;
    let head = fixture.change_and_commit(
        ".github/workflows/release-plz.yml",
        "name: changed fixture\n",
    )?;
    let selected = cli_with_environment(
        fixture.root(),
        &["codeql-matrix", "--base", &fixture.base, "--head", &head],
        "missing-base",
        &"b".repeat(40),
    )?;
    assert!(selected.status.success());
    assert_eq!(
        output(&selected)?,
        "{\"include\":[{\"language\":\"actions\",\"build-mode\":\"none\"}]}"
    );

    write(
        fixture.root(),
        "Justfile",
        "check:\n    @exit 7\n\ncheck-workflows:\n    @echo workflow-gate\n",
    )?;
    write(fixture.root(), "xtask/src/failure.rs", "// fixture\n")?;
    git(fixture.root(), ["add", "--all"])?;
    git(
        fixture.root(),
        [
            "commit",
            "--quiet",
            "-m",
            "test: fail the selected quality recipe",
        ],
    )?;
    let failing_head = output(&git(fixture.root(), ["rev-parse", "HEAD"])?)?;
    let failed = cli(
        fixture.root(),
        &["check", "--base", &fixture.base, "--head", &failing_head],
    )?;
    assert!(!failed.status.success());
    assert!(String::from_utf8_lossy(&failed.stderr).contains("`just check` failed"));
    Ok(())
}

#[test]
fn check_dispatches_workflow_only_changes_to_the_workflow_gate() -> io::Result<()> {
    let fixture = RepositoryFixture::new()?;
    let head = fixture.change_and_commit(
        ".github/workflows/release-plz.yml",
        "name: changed fixture\n",
    )?;
    let result = cli(
        fixture.root(),
        &["check", "--base", &fixture.base, "--head", &head],
    )?;
    assert!(
        result.status.success(),
        "selector failed: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(output(&result)?, "workflow-gate");
    Ok(())
}

#[test]
fn ci_workflow_change_dispatches_full_quality_and_both_codeql_languages() -> io::Result<()> {
    let fixture = RepositoryFixture::new()?;
    let head = fixture.change_and_commit(".github/workflows/ci.yml", "name: changed ordinary\n")?;
    let result = cli(
        fixture.root(),
        &["check", "--base", &fixture.base, "--head", &head],
    )?;
    assert!(
        result.status.success(),
        "selector failed: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(output(&result)?, "full-gate");
    assert!(String::from_utf8_lossy(&result.stderr).contains("quality=full; codeql=both"));
    Ok(())
}

#[test]
fn dirty_checkout_and_unresolvable_base_use_full_work_and_both_languages() -> io::Result<()> {
    let fixture = RepositoryFixture::new()?;
    write(fixture.root(), "untracked file.md", "uncommitted\n")?;
    let dirty = cli(fixture.root(), &["codeql-matrix", "--base", &fixture.base])?;
    assert!(dirty.status.success());
    assert_eq!(
        output(&dirty)?,
        "{\"include\":[{\"language\":\"rust\",\"build-mode\":\"none\"},{\"language\":\"actions\",\"build-mode\":\"none\"}]}"
    );

    fs::remove_file(fixture.root().join("untracked file.md"))?;
    let unresolved = cli(fixture.root(), &["codeql-matrix", "--base", "missing-base"])?;
    assert!(unresolved.status.success());
    assert_eq!(
        output(&unresolved)?,
        "{\"include\":[{\"language\":\"rust\",\"build-mode\":\"none\"},{\"language\":\"actions\",\"build-mode\":\"none\"}]}"
    );
    Ok(())
}

#[test]
fn explicitly_empty_base_uses_full_work_and_both_languages() -> io::Result<()> {
    let fixture = RepositoryFixture::new()?;
    let result = cli(fixture.root(), &["codeql-matrix", "--base", ""])?;
    assert!(result.status.success());
    assert_eq!(
        output(&result)?,
        "{\"include\":[{\"language\":\"rust\",\"build-mode\":\"none\"},{\"language\":\"actions\",\"build-mode\":\"none\"}]}"
    );
    Ok(())
}

#[test]
fn changed_rust_input_selects_rust_and_empty_diff_selects_both() -> io::Result<()> {
    let fixture = RepositoryFixture::new()?;
    let head =
        fixture.change_and_commit("crates/webpkit/src/selector.rs", "pub fn fixture() {}\n")?;
    let rust = cli(
        fixture.root(),
        &["codeql-matrix", "--base", &fixture.base, "--head", &head],
    )?;
    assert!(rust.status.success());
    assert_eq!(
        output(&rust)?,
        "{\"include\":[{\"language\":\"rust\",\"build-mode\":\"none\"}]}"
    );

    let empty = cli(
        fixture.root(),
        &["codeql-matrix", "--base", &head, "--head", &head],
    )?;
    assert!(empty.status.success());
    assert_eq!(
        output(&empty)?,
        "{\"include\":[{\"language\":\"rust\",\"build-mode\":\"none\"},{\"language\":\"actions\",\"build-mode\":\"none\"}]}"
    );
    Ok(())
}

#[test]
fn configured_head_must_match_the_fixture_checkout() -> io::Result<()> {
    let fixture = RepositoryFixture::new()?;
    let result = cli(
        fixture.root(),
        &[
            "codeql-matrix",
            "--base",
            &fixture.base,
            "--head",
            &"b".repeat(40),
        ],
    )?;
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("does not match"));
    Ok(())
}

#[test]
fn configured_head_must_be_a_full_sha() -> io::Result<()> {
    let fixture = RepositoryFixture::new()?;
    for head in ["", "abcd", "g000000000000000000000000000000000000000"] {
        let result = cli(
            fixture.root(),
            &["codeql-matrix", "--base", &fixture.base, "--head", head],
        )?;
        assert!(!result.status.success(), "head: {head:?}");
        assert!(String::from_utf8_lossy(&result.stderr).contains("full 40-character"));
    }
    Ok(())
}

#[test]
fn renamed_paths_are_compared_without_rename_detection() -> io::Result<()> {
    let fixture = RepositoryFixture::new()?;
    let source = fixture.root().join("notes/rename-input.txt");
    let destination = fixture.root().join(".github/workflows/deploy.yml");
    let destination_parent = destination.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "workflow destination has no parent",
        )
    })?;
    fs::create_dir_all(destination_parent)?;
    fs::rename(source, destination)?;
    git(fixture.root(), ["add", "--all"])?;
    git(
        fixture.root(),
        ["commit", "--quiet", "-m", "test: rename selector fixture"],
    )?;
    let head = output(&git(fixture.root(), ["rev-parse", "HEAD"])?)?;
    let result = cli(
        fixture.root(),
        &["codeql-matrix", "--base", &fixture.base, "--head", &head],
    )?;
    assert!(result.status.success());
    assert_eq!(
        output(&result)?,
        "{\"include\":[{\"language\":\"rust\",\"build-mode\":\"none\"},{\"language\":\"actions\",\"build-mode\":\"none\"}]}"
    );
    let quality = cli(
        fixture.root(),
        &["check", "--base", &fixture.base, "--head", &head],
    )?;
    assert!(quality.status.success());
    assert_eq!(output(&quality)?, "full-gate");
    Ok(())
}

#[test]
fn deleting_a_workflow_keeps_workflow_only_quality_and_actions_codeql() -> io::Result<()> {
    let fixture = RepositoryFixture::new()?;
    fs::remove_file(fixture.root().join(".github/workflows/release-plz.yml"))?;
    git(fixture.root(), ["add", "--all"])?;
    git(
        fixture.root(),
        [
            "commit",
            "--quiet",
            "-m",
            "test: delete selector workflow fixture",
        ],
    )?;
    let head = output(&git(fixture.root(), ["rev-parse", "HEAD"])?)?;
    let matrix = cli(
        fixture.root(),
        &["codeql-matrix", "--base", &fixture.base, "--head", &head],
    )?;
    assert!(matrix.status.success());
    assert_eq!(
        output(&matrix)?,
        "{\"include\":[{\"language\":\"actions\",\"build-mode\":\"none\"}]}"
    );
    let quality = cli(
        fixture.root(),
        &["check", "--base", &fixture.base, "--head", &head],
    )?;
    assert!(quality.status.success());
    assert_eq!(output(&quality)?, "workflow-gate");
    Ok(())
}

#[test]
fn codeql_workflow_and_mixed_source_changes_use_real_git_diffs() -> io::Result<()> {
    let workflow = RepositoryFixture::new()?;
    let workflow_head =
        workflow.change_and_commit(".github/workflows/codeql.yml", "name: CodeQL\n")?;
    let workflow_matrix = cli(
        workflow.root(),
        &[
            "codeql-matrix",
            "--base",
            &workflow.base,
            "--head",
            &workflow_head,
        ],
    )?;
    assert!(workflow_matrix.status.success());
    assert_eq!(
        output(&workflow_matrix)?,
        "{\"include\":[{\"language\":\"rust\",\"build-mode\":\"none\"},{\"language\":\"actions\",\"build-mode\":\"none\"}]}"
    );
    let workflow_quality = cli(
        workflow.root(),
        &["check", "--base", &workflow.base, "--head", &workflow_head],
    )?;
    assert!(workflow_quality.status.success());
    assert_eq!(output(&workflow_quality)?, "workflow-gate");

    let mixed = RepositoryFixture::new()?;
    write(
        mixed.root(),
        ".github/workflows/release-plz.yml",
        "name: mixed\n",
    )?;
    write(
        mixed.root(),
        "crates/webpkit/src/mixed.rs",
        "pub fn mixed() {}\n",
    )?;
    git(mixed.root(), ["add", "--all"])?;
    git(
        mixed.root(),
        [
            "commit",
            "--quiet",
            "-m",
            "test: mix workflow and Rust inputs",
        ],
    )?;
    let mixed_head = output(&git(mixed.root(), ["rev-parse", "HEAD"])?)?;
    let mixed_matrix = cli(
        mixed.root(),
        &[
            "codeql-matrix",
            "--base",
            &mixed.base,
            "--head",
            &mixed_head,
        ],
    )?;
    assert!(mixed_matrix.status.success());
    assert_eq!(
        output(&mixed_matrix)?,
        "{\"include\":[{\"language\":\"rust\",\"build-mode\":\"none\"},{\"language\":\"actions\",\"build-mode\":\"none\"}]}"
    );
    let mixed_quality = cli(
        mixed.root(),
        &["check", "--base", &mixed.base, "--head", &mixed_head],
    )?;
    assert!(mixed_quality.status.success());
    assert_eq!(output(&mixed_quality)?, "full-gate");
    Ok(())
}
