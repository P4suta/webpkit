//! Process and provider boundaries for the pure qualification decisions.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use anyhow::{Context, Result, bail, ensure};
use clap::Subcommand;
use serde::Deserialize;
use webpkit_ci_core::{
    Association, Event, JobReport, Outcome, Repository, Revision, Role, SourceFacts,
};

const REPOSITORY: &str = "P4suta/webpkit";
const DEFAULT_PREFIX: &str = "release-plz-";
const PUBLISHER_PREFIX: &str = "qualified-source:";

/// Checked CI entry points used by the workflows.
#[derive(Debug, Subcommand)]
pub(crate) enum CiCommand {
    /// Require the exact ordinary or final summary inventory.
    Required {
        #[arg(long)]
        event: Option<String>,
        #[arg(long = "final", conflicts_with = "event")]
        final_summary: bool,
        #[arg(long)]
        needs: String,
    },
    /// Resolve a release proposal association without publishing or building.
    Candidate {
        #[arg(long)]
        repository: String,
        #[arg(long)]
        revision: String,
        #[arg(long)]
        output: PathBuf,
    },
    /// Check HEAD, index, and tracked working-tree identity.
    Source {
        #[arg(long)]
        revision: String,
        #[arg(long)]
        output: PathBuf,
    },
    /// Require all qualification roles to succeed on one exact source.
    Qualify {
        #[arg(long)]
        revision: String,
        #[arg(long)]
        needs: String,
        #[arg(long)]
        output: PathBuf,
    },
    /// Recheck eligibility and prepare an isolated publisher configuration.
    PublishSource {
        #[arg(long)]
        repository: String,
        #[arg(long)]
        revision: String,
        #[arg(long)]
        config: PathBuf,
        #[arg(long)]
        output: PathBuf,
    },
}

pub(crate) fn run(command: CiCommand) -> Result<()> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .context("xtask must be inside the workspace")?;
    match command {
        CiCommand::Required {
            event,
            final_summary,
            needs,
        } => required(event.as_deref(), final_summary, &needs),
        CiCommand::Source { revision, output } => {
            let revision = parse_revision(&revision)?;
            check_source(root, revision)?;
            emit(&output, "revision", &revision_text(revision)?)
        },
        CiCommand::Candidate {
            repository,
            revision,
            output,
        } => {
            let revision = parse_revision(&revision)?;
            let selected = resolve_candidate(root, &Github, &repository, revision)?;
            emit(
                &output,
                "candidate",
                if selected { "true" } else { "false" },
            )?;
            if selected {
                emit(&output, "revision", &revision_text(revision)?)?;
            }
            Ok(())
        },
        CiCommand::Qualify {
            revision,
            needs,
            output,
        } => {
            let revision = parse_revision(&revision)?;
            let reports = qualification_reports(&needs, revision)?;
            webpkit_ci_core::qualify(revision, &reports)
                .map_err(|error| anyhow::anyhow!("qualification refused: {error:?}"))?;
            emit(&output, "revision", &revision_text(revision)?)
        },
        CiCommand::PublishSource {
            repository,
            revision,
            config,
            output,
        } => {
            let revision = parse_revision(&revision)?;
            ensure!(
                resolve_candidate(root, &Github, &repository, revision)?,
                "source isn't a release candidate"
            );
            write_publisher_config(root, &config)?;
            check_source(root, revision)?;
            emit(
                &output,
                "config_path",
                config
                    .to_str()
                    .context("publisher configuration path must be UTF-8")?,
            )
        },
    }
}

fn parse_revision(text: &str) -> Result<Revision> {
    Revision::parse(text.as_bytes()).map_err(|error| anyhow::anyhow!("invalid revision: {error:?}"))
}

fn revision_text(revision: Revision) -> Result<String> {
    String::from_utf8(revision.bytes().to_vec()).context("rendering validated revision")
}

fn emit(path: &Path, key: &str, value: &str) -> Result<()> {
    ensure!(
        !value.contains(['\r', '\n']),
        "output value must occupy one line"
    );
    let mut file = OpenOptions::new()
        .append(true)
        .create(true)
        .open(path)
        .with_context(|| format!("opening output {}", path.display()))?;
    writeln!(file, "{key}={value}").context("writing checked workflow output")
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Need {
    result: String,
    outputs: NeedOutputs,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NeedOutputs {
    revision: Option<String>,
}

fn outcome(need: &Need) -> Result<Outcome> {
    match need.result.as_str() {
        "success" => Ok(Outcome::Success),
        "failure" => Ok(Outcome::Failure),
        "cancelled" => Ok(Outcome::Cancelled),
        "skipped" => Ok(Outcome::Skipped),
        _ => bail!("job has an unknown or unfinished result"),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OrdinaryNeeds {
    quality: Need,
    #[serde(rename = "conventional-commits")]
    title: Need,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FinalNeeds {
    #[serde(rename = "ci-required")]
    ordinary: Need,
}

fn required(event: Option<&str>, final_summary: bool, json: &str) -> Result<()> {
    if final_summary {
        ensure!(
            event.is_none(),
            "final summary doesn't accept an event override"
        );
        let needs: FinalNeeds =
            serde_json::from_str(json).context("reading final needs inventory")?;
        return webpkit_ci_core::final_summary(outcome(&needs.ordinary)?)
            .map_err(|error| anyhow::anyhow!("final summary refused: {error:?}"));
    }
    let event = match event {
        Some("pull_request") => Event::PullRequest,
        Some("merge_group") => Event::MergeGroup,
        Some("workflow_dispatch") => Event::WorkflowDispatch,
        _ => bail!("ordinary summary requires a supported event"),
    };
    let needs: OrdinaryNeeds =
        serde_json::from_str(json).context("reading ordinary needs inventory")?;
    webpkit_ci_core::ordinary_summary(event, outcome(&needs.quality)?, outcome(&needs.title)?)
        .map_err(|error| anyhow::anyhow!("ordinary summary refused: {error:?}"))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct QualificationNeeds {
    candidate: CandidateNeed,
    quality: Need,
    #[serde(rename = "native-windows")]
    native_windows: Need,
    #[serde(rename = "native-macos")]
    native_macos: Need,
    msrv: Need,
    #[serde(rename = "no-std")]
    no_std: Need,
    corpus: Need,
    metrics: Need,
    work: Need,
    oracle: Need,
    coverage: Need,
    mutation: Need,
    #[serde(rename = "policy-proofs")]
    policy_proofs: Need,
    fuzz: Need,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CandidateNeed {
    result: String,
    outputs: CandidateOutputs,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CandidateOutputs {
    candidate: String,
    revision: String,
}

fn qualification_reports(json: &str, revision: Revision) -> Result<Vec<JobReport>> {
    let needs: QualificationNeeds =
        serde_json::from_str(json).context("reading complete qualification inventory")?;
    ensure!(
        needs.candidate.result == "success" && needs.candidate.outputs.candidate == "true",
        "candidate dependency didn't select a release source"
    );
    ensure!(
        parse_revision(&needs.candidate.outputs.revision)? == revision,
        "candidate dependency has a different revision"
    );
    let ordered = [
        needs.quality,
        needs.native_windows,
        needs.native_macos,
        needs.msrv,
        needs.no_std,
        needs.corpus,
        needs.metrics,
        needs.work,
        needs.oracle,
        needs.coverage,
        needs.mutation,
        needs.policy_proofs,
        needs.fuzz,
    ];
    Role::ALL
        .into_iter()
        .zip(ordered)
        .map(|(role, need)| {
            let revision = need
                .outputs
                .revision
                .as_deref()
                .context("qualification role lacks a post-check revision")?;
            Ok(JobReport {
                role,
                outcome: outcome(&need)?,
                revision: Some(parse_revision(revision)?),
            })
        })
        .collect()
}

fn git(root: &Path) -> Command {
    let mut command = Command::new("git");
    command.current_dir(root);
    for (key, _) in std::env::vars_os() {
        if key
            .to_string_lossy()
            .to_ascii_uppercase()
            .starts_with("GIT_")
        {
            command.env_remove(key);
        }
    }
    command
}

fn successful(output: Output, operation: &str) -> Result<Vec<u8>> {
    ensure!(
        output.status.success(),
        "{operation} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(output.stdout)
}

fn clean(root: &Path, cached: bool) -> Result<bool> {
    let mut command = git(root);
    command.args(["diff", "--quiet"]);
    if cached {
        command.args(["--cached", "HEAD"]);
    }
    let output = command
        .arg("--")
        .output()
        .context("checking tracked Git changes")?;
    match output.status.code() {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        _ => bail!(
            "Git source comparison failed: {}",
            String::from_utf8_lossy(&output.stderr)
        ),
    }
}

fn check_source(root: &Path, expected: Revision) -> Result<()> {
    let bytes = successful(
        git(root).args(["rev-parse", "--verify", "HEAD"]).output()?,
        "reading HEAD",
    )?;
    let text = std::str::from_utf8(&bytes)?.trim_end_matches(['\r', '\n']);
    let facts = SourceFacts {
        head: parse_revision(text)?,
        index_clean: clean(root, true)?,
        worktree_clean: clean(root, false)?,
    };
    webpkit_ci_core::source(expected, facts)
        .map_err(|error| anyhow::anyhow!("tracked source refused: {error:?}"))?;
    Ok(())
}

trait Provider {
    fn read(&self, endpoint: &str, paginate: bool) -> Result<Vec<u8>>;
}

struct Github;

fn github_command(endpoint: &str, paginate: bool) -> Command {
    let mut command = Command::new("gh");
    command.args(["api", "--hostname", "github.com", "--method", "GET"]);
    if paginate {
        command.args(["--paginate", "--slurp"]);
    }
    command.arg(endpoint);
    command
}

impl Provider for Github {
    fn read(&self, endpoint: &str, paginate: bool) -> Result<Vec<u8>> {
        successful(
            github_command(endpoint, paginate)
                .output()
                .context("reading GitHub metadata")?,
            "GitHub metadata request",
        )
    }
}

#[derive(Deserialize)]
struct RepositoryMetadata {
    full_name: String,
    default_branch: String,
    archived: bool,
}

#[derive(Deserialize)]
struct RefMetadata {
    object: ObjectMetadata,
}

#[derive(Deserialize)]
struct ObjectMetadata {
    sha: String,
}

#[derive(Deserialize)]
struct RepositoryName {
    full_name: String,
}

#[derive(Deserialize)]
struct PullBranch {
    #[serde(rename = "ref")]
    reference: String,
    repo: Option<RepositoryName>,
}

#[derive(Deserialize)]
struct PullMetadata {
    state: String,
    merged_at: Option<String>,
    merge_commit_sha: Option<String>,
    base: PullBranch,
    head: PullBranch,
}

fn tracked_config(root: &Path) -> Result<toml::Value> {
    let text = fs::read_to_string(root.join("release-plz.toml"))
        .context("reading tracked release configuration")?;
    let config: toml::Value =
        toml::from_str(&text).context("parsing tracked release configuration")?;
    let workspace = config
        .get("workspace")
        .and_then(toml::Value::as_table)
        .context("release configuration needs a workspace table")?;
    ensure!(
        workspace
            .get("release_always")
            .and_then(toml::Value::as_bool)
            == Some(false),
        "tracked release eligibility must remain explicit"
    );
    ensure!(
        workspace.get("semver_check").and_then(toml::Value::as_bool) == Some(true),
        "semantic-version checking must remain enabled"
    );
    Ok(config)
}

fn resolve_candidate(
    root: &Path,
    provider: &impl Provider,
    repository: &str,
    revision: Revision,
) -> Result<bool> {
    ensure!(
        repository == REPOSITORY,
        "candidate repository must be canonical"
    );
    check_source(root, revision)?;
    let config = tracked_config(root)?;
    let prefix = config["workspace"]
        .get("pr_branch_prefix")
        .map_or(Ok(DEFAULT_PREFIX), |value| {
            value.as_str().context("release prefix must be a string")
        })?;
    ensure!(!prefix.is_empty(), "release prefix must be nonempty");
    let repo: RepositoryMetadata =
        serde_json::from_slice(&provider.read(&format!("repos/{REPOSITORY}"), false)?)?;
    let revision_string = std::str::from_utf8(revision.bytes())?;
    let pages: Vec<Vec<PullMetadata>> = serde_json::from_slice(&provider.read(
        &format!("repos/{REPOSITORY}/commits/{revision_string}/pulls?per_page=100"),
        true,
    )?)?;
    ensure!(
        !pages.is_empty(),
        "provider omitted the association page inventory"
    );
    let associations = pages
        .into_iter()
        .flatten()
        .map(|pull| {
            let same_repository = pull
                .base
                .repo
                .as_ref()
                .is_some_and(|repo| repo.full_name == REPOSITORY)
                && pull
                    .head
                    .repo
                    .as_ref()
                    .is_some_and(|repo| repo.full_name == REPOSITORY);
            ensure!(
                matches!(pull.state.as_str(), "open" | "closed"),
                "unknown pull request state"
            );
            Ok(Association {
                release_branch: pull.head.reference.starts_with(prefix),
                merged: pull.state == "closed"
                    && pull.merged_at.as_ref().is_some_and(|date| !date.is_empty()),
                repository: if same_repository {
                    Repository::Webpkit
                } else {
                    Repository::Other
                },
                main_branch: pull.base.reference == "main",
                merge_revision: pull
                    .merge_commit_sha
                    .as_deref()
                    .map(parse_revision)
                    .transpose()?,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let main: RefMetadata = serde_json::from_slice(
        &provider.read(&format!("repos/{REPOSITORY}/git/ref/heads/main"), false)?,
    )?;
    let selected = webpkit_ci_core::candidate(
        if repo.full_name == REPOSITORY {
            Repository::Webpkit
        } else {
            Repository::Other
        },
        repo.default_branch == "main" && !repo.archived,
        revision,
        parse_revision(&main.object.sha)?,
        &associations,
    )
    .map_err(|error| anyhow::anyhow!("candidate metadata refused: {error:?}"))?;
    check_source(root, revision)?;
    Ok(selected.is_some())
}

fn publisher_config(mut config: toml::Value) -> Result<toml::Value> {
    let workspace = config
        .get_mut("workspace")
        .and_then(toml::Value::as_table_mut)
        .context("release configuration needs a workspace table")?;
    workspace.insert("release_always".to_owned(), toml::Value::Boolean(true));
    workspace.insert(
        "pr_branch_prefix".to_owned(),
        toml::Value::String(PUBLISHER_PREFIX.to_owned()),
    );
    Ok(config)
}

fn write_publisher_config(root: &Path, path: &Path) -> Result<()> {
    ensure!(
        path.is_absolute(),
        "publisher configuration path must be absolute"
    );
    let parent = path
        .parent()
        .context("publisher configuration needs a parent directory")?
        .canonicalize()?;
    ensure!(
        !parent.starts_with(root.canonicalize()?),
        "publisher configuration must be outside tracked source"
    );
    let config = publisher_config(tracked_config(root)?)?;
    let rendered = toml::to_string(&config).context("rendering publisher configuration")?;
    let parsed: toml::Value = toml::from_str(&rendered)?;
    ensure!(
        parsed == config,
        "publisher configuration didn't round-trip"
    );
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .context("creating isolated publisher configuration")?;
    file.write_all(rendered.as_bytes())
        .context("writing publisher configuration")
}

#[cfg(test)]
mod tests {
    use super::{
        Provider, REPOSITORY, Role, check_source, git, parse_revision, publisher_config,
        qualification_reports, required, resolve_candidate,
    };
    use std::fs;
    use std::path::Path;

    use anyhow::{Result, bail};
    use serde_json::{Value, json};
    use tempfile::TempDir;

    fn fixture_git(root: &Path, arguments: &[&str]) -> Vec<u8> {
        let output = git(root)
            .args(["-c", "core.hooksPath=", "-c", "commit.gpgsign=false"])
            .args(arguments)
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        output.stdout
    }

    fn fixture() -> (TempDir, super::Revision) {
        let root = TempDir::new().unwrap();
        fixture_git(root.path(), &["init", "--initial-branch=main"]);
        fixture_git(root.path(), &["config", "user.name", "CI fixture"]);
        fixture_git(root.path(), &["config", "user.email", "ci@example.invalid"]);
        fs::write(root.path().join("tracked.txt"), "original\n").unwrap();
        fs::write(
            root.path().join("release-plz.toml"),
            "[workspace]\nrelease_always = false\nsemver_check = true\n",
        )
        .unwrap();
        fixture_git(root.path(), &["add", "."]);
        fixture_git(root.path(), &["commit", "-m", "test: initialize fixture"]);
        let revision = fixture_git(root.path(), &["rev-parse", "HEAD"]);
        let revision = parse_revision(std::str::from_utf8(&revision).unwrap().trim()).unwrap();
        (root, revision)
    }

    fn need(result: &str, revision: Option<&str>) -> Value {
        revision.map_or_else(
            || json!({"result": result, "outputs": {}}),
            |revision| json!({"result": result, "outputs": {"revision": revision}}),
        )
    }

    #[test]
    fn summary_json_refuses_missing_extra_duplicate_unknown_and_unfinished_jobs() {
        let valid = json!({"quality": need("success", None), "conventional-commits": need("success", None)});
        assert!(required(Some("pull_request"), false, &valid.to_string()).is_ok());
        for text in [
            "{}",
            "null",
            "{\"quality\":null}",
            "{\"quality\":{},\"quality\":{}}",
        ] {
            assert!(required(Some("pull_request"), false, text).is_err());
        }
        let duplicate_output = r#"{"quality":{"result":"success","outputs":{"revision":"a","revision":"b"}},"conventional-commits":{"result":"success","outputs":{}}}"#;
        assert!(required(Some("pull_request"), false, duplicate_output).is_err());
        for result in ["failure", "cancelled", "skipped", "pending", "", "SUCCESS"] {
            let invalid = json!({"quality": need(result, None), "conventional-commits": need("success", None)});
            assert!(required(Some("pull_request"), false, &invalid.to_string()).is_err());
        }
        let mut extra = valid.clone();
        extra["unexpected"] = need("success", None);
        assert!(required(Some("pull_request"), false, &extra.to_string()).is_err());
        assert!(required(Some("push"), false, &valid.to_string()).is_err());
        let mut extra_output = valid;
        extra_output["quality"]["outputs"]["unexpected"] = json!("unused");
        assert!(required(Some("pull_request"), false, &extra_output.to_string()).is_err());
        assert!(
            required(
                None,
                true,
                &json!({"ci-required":need("skipped", None)}).to_string()
            )
            .is_err()
        );
    }

    #[test]
    fn every_qualification_json_role_and_candidate_identity_is_mandatory() {
        let identity = "a".repeat(super::Revision::LENGTH);
        let revision = parse_revision(&identity).unwrap();
        let mut value = json!({"candidate":{"result":"success","outputs":{"candidate":"true","revision":identity}}});
        for role in Role::ALL {
            value[role.name()] = need("success", Some(&identity));
        }
        let reports = qualification_reports(&value.to_string(), revision).unwrap();
        assert!(webpkit_ci_core::qualify(revision, &reports).is_ok());
        for role in Role::ALL {
            let mut removed = value.clone();
            removed.as_object_mut().unwrap().remove(role.name());
            assert!(qualification_reports(&removed.to_string(), revision).is_err());
            let mut missing_identity = value.clone();
            missing_identity[role.name()]["outputs"] = json!({});
            assert!(qualification_reports(&missing_identity.to_string(), revision).is_err());
        }
        value["candidate"]["outputs"]["candidate"] = json!("false");
        assert!(qualification_reports(&value.to_string(), revision).is_err());
    }

    #[test]
    fn tracked_git_boundary_rejects_staged_delta_restored_in_worktree() {
        let (root, revision) = fixture();
        assert!(check_source(root.path(), revision).is_ok());
        assert!(
            check_source(
                root.path(),
                parse_revision(&"b".repeat(super::Revision::LENGTH)).unwrap()
            )
            .is_err()
        );
        fs::write(root.path().join("tracked.txt"), "staged\n").unwrap();
        fixture_git(root.path(), &["add", "tracked.txt"]);
        fs::write(root.path().join("tracked.txt"), "original\n").unwrap();
        assert!(check_source(root.path(), revision).is_err());
        fixture_git(root.path(), &["add", "tracked.txt"]);
        assert!(check_source(root.path(), revision).is_ok());
        fs::write(root.path().join("tracked.txt"), "unstaged\n").unwrap();
        assert!(check_source(root.path(), revision).is_err());
    }

    struct FixtureProvider {
        repository: Value,
        pages: Value,
        main: Value,
        failure: bool,
    }

    #[test]
    fn production_provider_uses_complete_readonly_github_pagination() {
        let endpoint = "repos/P4suta/webpkit/commits/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa/pulls?per_page=100";
        let command = super::github_command(endpoint, true);
        let arguments = command
            .get_args()
            .map(|arg| arg.to_str().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(
            arguments,
            [
                "api",
                "--hostname",
                "github.com",
                "--method",
                "GET",
                "--paginate",
                "--slurp",
                endpoint
            ]
        );
    }

    impl Provider for FixtureProvider {
        fn read(&self, endpoint: &str, paginate: bool) -> Result<Vec<u8>> {
            if self.failure {
                bail!("injected provider failure");
            }
            let value = if paginate {
                assert!(endpoint.contains("/pulls?per_page=100"));
                &self.pages
            } else if endpoint.ends_with("/main") {
                &self.main
            } else {
                &self.repository
            };
            Ok(serde_json::to_vec(value)?)
        }
    }

    struct ChangingProvider<'a> {
        provider: &'a FixtureProvider,
        root: &'a Path,
    }

    impl Provider for ChangingProvider<'_> {
        fn read(&self, endpoint: &str, paginate: bool) -> Result<Vec<u8>> {
            if endpoint.ends_with("/main") {
                fs::write(self.root.join("tracked.txt"), "new committed source\n")?;
                fixture_git(self.root, &["add", "tracked.txt"]);
                fixture_git(
                    self.root,
                    &["commit", "-m", "test: change provider fixture source"],
                );
            }
            self.provider.read(endpoint, paginate)
        }
    }

    #[test]
    fn provider_read_cannot_hide_a_head_change_after_initial_source_check() {
        let (root, revision) = fixture();
        let provider = FixtureProvider {
            repository: json!({"full_name":REPOSITORY,"default_branch":"main","archived":false}),
            pages: json!([[]]),
            main: json!({"object":{"sha":std::str::from_utf8(revision.bytes()).unwrap()}}),
            failure: false,
        };
        let changing = ChangingProvider {
            provider: &provider,
            root: root.path(),
        };
        assert!(resolve_candidate(root.path(), &changing, REPOSITORY, revision).is_err());
    }

    #[test]
    fn provider_boundary_reads_all_pages_and_never_coerces_failure_to_no_candidate() {
        let (root, revision) = fixture();
        let identity = std::str::from_utf8(revision.bytes()).unwrap();
        let pull = json!({"state":"closed","merged_at":"2026-10-26T00:00:00Z","merge_commit_sha":identity,
            "base":{"ref":"main","repo":{"full_name":REPOSITORY}},
            "head":{"ref":"release-plz-2026","repo":{"full_name":REPOSITORY}}});
        let mut provider = FixtureProvider {
            repository: json!({"full_name":REPOSITORY,"default_branch":"main","archived":false}),
            pages: json!([[], [pull]]),
            main: json!({"object":{"sha":identity}}),
            failure: false,
        };
        assert!(resolve_candidate(root.path(), &provider, REPOSITORY, revision).unwrap());
        provider.pages = json!([[pull.clone()], [pull]]);
        assert!(resolve_candidate(root.path(), &provider, REPOSITORY, revision).is_err());
        provider.pages = json!([[]]);
        assert!(!resolve_candidate(root.path(), &provider, REPOSITORY, revision).unwrap());
        for malformed in [json!(null), json!([]), json!([null]), json!([[{}]])] {
            provider.pages = malformed;
            assert!(resolve_candidate(root.path(), &provider, REPOSITORY, revision).is_err());
        }
        provider.pages = json!([[]]);
        provider.failure = true;
        assert!(resolve_candidate(root.path(), &provider, REPOSITORY, revision).is_err());
        provider.failure = false;
        provider.main = json!({"object":{"sha":"b".repeat(super::Revision::LENGTH)}});
        assert!(resolve_candidate(root.path(), &provider, REPOSITORY, revision).is_err());
    }

    #[test]
    fn publisher_changes_exactly_two_workspace_fields_without_mutating_tracked_config() {
        let original: toml::Value = toml::from_str("[workspace]\nrelease_always=false\nsemver_check=true\nchangelog_update=true\n[[package]]\nname='webpkit'\npublish=true\n").unwrap();
        let mut expected = original.clone();
        expected["workspace"]["release_always"] = toml::Value::Boolean(true);
        expected["workspace"].as_table_mut().unwrap().insert(
            "pr_branch_prefix".to_owned(),
            toml::Value::String(super::PUBLISHER_PREFIX.to_owned()),
        );
        assert_eq!(publisher_config(original.clone()).unwrap(), expected);
        assert_eq!(
            original["workspace"]["release_always"].as_bool(),
            Some(false)
        );
        let (root, _) = fixture();
        let output_root = TempDir::new().unwrap();
        let output = output_root.path().join("publisher.toml");
        let tracked = fs::read(root.path().join("release-plz.toml")).unwrap();
        super::write_publisher_config(root.path(), &output).unwrap();
        assert_eq!(
            fs::read(root.path().join("release-plz.toml")).unwrap(),
            tracked
        );
        assert!(super::write_publisher_config(root.path(), &output).is_err());
        assert!(
            super::write_publisher_config(root.path(), &root.path().join("generated.toml"))
                .is_err()
        );
    }
}
