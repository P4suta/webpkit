//! Pure decisions for complete qualification of an exact repository revision.

/// A refused source or qualification transition.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Refusal {
    /// A revision isn't a full hexadecimal Git object identity.
    Revision,
    /// Repository facts don't identify the expected active main branch.
    Repository,
    /// The event no longer identifies the current main commit.
    MainChanged,
    /// Release association metadata is ambiguous or inconsistent.
    Association,
    /// The checked-out tracked source differs from the promised revision.
    Source,
    /// Required ordinary work didn't finish successfully.
    Ordinary,
    /// Qualification is missing, duplicated, unsuccessful, or from another source.
    Qualification,
}

/// The ordinary quality work required by the changed source paths.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum QualityWork {
    /// Run only the source-bound workflow checks.
    Workflows,
    /// Run the complete ordinary quality gate.
    Full,
}

/// The `CodeQL` languages selected by the changed source paths.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CodeqlScope {
    /// Analyze Rust only.
    Rust,
    /// Analyze GitHub Actions only.
    Actions,
    /// Analyze both languages.
    Both,
}

/// Independent ordinary-quality and `CodeQL` work selected for a complete path set.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AffectedWork {
    /// The ordinary quality gate to run.
    pub quality: QualityWork,
    /// The `CodeQL` languages to analyze.
    pub codeql: CodeqlScope,
}

/// Select affected work from a complete set of repository-relative Git paths.
///
/// Workflow YAML other than the ordinary CI definition selects workflow-only quality.
/// The `CodeQL` workflow selects both analysis languages.
/// Unknown, empty, invalid, and mixed path sets select full quality and both analyses.
#[must_use]
pub fn affected_work(paths: &[&str]) -> AffectedWork {
    if paths.is_empty() {
        return full_both();
    }

    let mut workflows_only = true;
    let mut rust = false;
    let mut actions = false;
    for path in paths {
        if !valid_repository_path(path) {
            return full_both();
        }
        if let Some(name) = workflow_yaml(path) {
            actions = true;
            if name == "ci.yml" || name == "ci.yaml" {
                workflows_only = false;
                rust = true;
            }
            if name == "codeql.yml" || name == "codeql.yaml" {
                rust = true;
            }
            continue;
        }
        workflows_only = false;
        if action_definition(path) {
            actions = true;
            continue;
        }
        if rust_input(path) {
            rust = true;
        } else {
            return full_both();
        }
    }

    AffectedWork {
        quality: if workflows_only {
            QualityWork::Workflows
        } else {
            QualityWork::Full
        },
        codeql: match (rust, actions) {
            (true, true) => CodeqlScope::Both,
            (true, false) => CodeqlScope::Rust,
            (false, true) => CodeqlScope::Actions,
            (false, false) => return full_both(),
        },
    }
}

const fn full_both() -> AffectedWork {
    AffectedWork {
        quality: QualityWork::Full,
        codeql: CodeqlScope::Both,
    }
}

fn valid_repository_path(path: &str) -> bool {
    !path.is_empty()
        && !path.starts_with('/')
        && !path.contains('\\')
        && !path.contains(':')
        && !path.bytes().any(|byte| byte.is_ascii_control())
        && path
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != "..")
}

fn workflow_yaml(path: &str) -> Option<&str> {
    let name = path.strip_prefix(".github/workflows/")?;
    if name.contains('/') {
        return None;
    }
    let stem = name
        .strip_suffix(".yml")
        .or_else(|| name.strip_suffix(".yaml"))?;
    (!stem.is_empty()).then_some(name)
}

fn action_definition(path: &str) -> bool {
    let Some(name) = path.strip_prefix(".github/actions/") else {
        return false;
    };
    name.contains('/') && (name.ends_with("/action.yml") || name.ends_with("/action.yaml"))
}

fn rust_input(path: &str) -> bool {
    matches!(
        path,
        "Cargo.toml"
            | "Cargo.lock"
            | "rust-toolchain"
            | "rust-toolchain.toml"
            | "rustfmt.toml"
            | "clippy.toml"
            | ".cargo/config"
            | ".cargo/config.toml"
            | "build.rs"
    ) || ((path.starts_with("crates/") || path.starts_with("xtask/"))
        && (path
            .rsplit_once('.')
            .is_some_and(|(_, extension)| extension == "rs")
            || path.ends_with("/Cargo.toml")))
}

/// A complete, normalized Git SHA-1 identity.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Revision([u8; Self::LENGTH]);

impl Revision {
    /// Length of the full hexadecimal object identity used by this repository.
    pub const LENGTH: usize = 40;

    /// Validate the full identity without accepting abbreviations or whitespace.
    ///
    /// # Errors
    /// Returns `Refusal::Revision` for a wrong length or a nonhexadecimal byte.
    pub fn parse(bytes: &[u8]) -> Result<Self, Refusal> {
        if bytes.len() != Self::LENGTH {
            return Err(Refusal::Revision);
        }
        let mut identity = [0; Self::LENGTH];
        for (output, byte) in identity.iter_mut().zip(bytes) {
            if !byte.is_ascii_hexdigit() {
                return Err(Refusal::Revision);
            }
            *output = byte.to_ascii_lowercase();
        }
        Ok(Self(identity))
    }

    /// Return the validated lowercase hexadecimal bytes.
    #[must_use]
    pub const fn bytes(&self) -> &[u8; Self::LENGTH] {
        &self.0
    }
}

/// The repository identity supplied by the read-only provider adapter.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Repository {
    /// The canonical owner repository.
    Webpkit,
    /// A different repository.
    Other,
}

/// Provider facts about one pull request associated with the event commit.
#[derive(Clone, Copy, Debug)]
pub struct Association {
    /// Whether the branch matches the tracked release proposal prefix.
    pub release_branch: bool,
    /// Whether the pull request is merged and closed.
    pub merged: bool,
    /// The shared identity of its base and head repositories.
    pub repository: Repository,
    /// Whether its base branch is main.
    pub main_branch: bool,
    /// The provider's complete merge commit identity.
    pub merge_revision: Option<Revision>,
}

/// A main revision associated with exactly one recognized merged release proposal.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Candidate(Revision);

impl Candidate {
    /// Return the exact source eligible for qualification.
    #[must_use]
    pub const fn revision(self) -> Revision {
        self.0
    }
}

/// Select a candidate from complete provider facts, refusing uncertain metadata.
///
/// # Errors
/// Refuses a foreign repository, a changed main commit, or an ambiguous release association.
pub fn candidate(
    repository: Repository,
    active_main: bool,
    event: Revision,
    current_main: Revision,
    associations: &[Association],
) -> Result<Option<Candidate>, Refusal> {
    if repository != Repository::Webpkit || !active_main {
        return Err(Refusal::Repository);
    }
    if event != current_main {
        return Err(Refusal::MainChanged);
    }
    let mut selected = None;
    for association in associations {
        if !association.release_branch {
            continue;
        }
        if !association.merged
            || association.repository != Repository::Webpkit
            || !association.main_branch
            || association.merge_revision != Some(event)
            || selected.is_some()
        {
            return Err(Refusal::Association);
        }
        selected = Some(Candidate(event));
    }
    Ok(selected)
}

/// The tracked checkout facts checked before and after qualification work.
#[derive(Clone, Copy, Debug)]
pub struct SourceFacts {
    /// The current HEAD commit.
    pub head: Revision,
    /// Whether the index equals HEAD.
    pub index_clean: bool,
    /// Whether the tracked working tree equals the index.
    pub worktree_clean: bool,
}

/// Reject a stale checkout or either form of tracked source modification.
///
/// # Errors
/// Returns `Refusal::Source` when HEAD, the index, or the tracked working tree differs.
pub fn source(expected: Revision, facts: SourceFacts) -> Result<Revision, Refusal> {
    if facts.head != expected || !facts.index_clean || !facts.worktree_clean {
        return Err(Refusal::Source);
    }
    Ok(expected)
}

/// Events admitted by ordinary pull request verification.
#[derive(Clone, Copy, Debug)]
#[cfg_attr(kani, derive(kani::Arbitrary))]
pub enum Event {
    /// A pull request requires title validation.
    PullRequest,
    /// A merge queue doesn't run pull request title validation.
    MergeGroup,
    /// An explicit ordinary run doesn't run pull request title validation.
    WorkflowDispatch,
}

/// A completed GitHub job result.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[cfg_attr(kani, derive(kani::Arbitrary))]
pub enum Outcome {
    /// The job completed successfully.
    Success,
    /// The job failed.
    Failure,
    /// The job was cancelled.
    Cancelled,
    /// The job was skipped.
    Skipped,
}

/// Require real ordinary work, with only the event-specific title skip admitted.
///
/// # Errors
/// Returns `Refusal::Ordinary` for an unsuccessful required role.
pub fn ordinary_summary(event: Event, quality: Outcome, title: Outcome) -> Result<(), Refusal> {
    let title_complete = match event {
        Event::PullRequest => title == Outcome::Success,
        Event::MergeGroup | Event::WorkflowDispatch => {
            title == Outcome::Success || title == Outcome::Skipped
        },
    };
    if quality != Outcome::Success || !title_complete {
        return Err(Refusal::Ordinary);
    }
    Ok(())
}

/// Preserve the required context without accepting skipped or unfinished work.
///
/// # Errors
/// Returns `Refusal::Ordinary` unless the strict ordinary aggregate succeeded.
pub fn final_summary(ordinary: Outcome) -> Result<(), Refusal> {
    if ordinary != Outcome::Success {
        return Err(Refusal::Ordinary);
    }
    Ok(())
}

/// The complete release qualification role inventory.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[cfg_attr(kani, derive(kani::Arbitrary))]
pub enum Role {
    /// Ordinary Linux quality and behavioral checks.
    Quality,
    /// Native Windows tests and doctests.
    NativeWindows,
    /// Native macOS tests and doctests.
    NativeMacos,
    /// The declared minimum supported Rust version.
    Msrv,
    /// The supported allocation-only target.
    NoStd,
    /// The committed corpus ledger.
    Corpus,
    /// The committed compression and memory ledgers.
    Metrics,
    /// The committed algorithmic work ledgers.
    Work,
    /// Differential checks against libwebp.
    Oracle,
    /// The existing coverage thresholds.
    Coverage,
    /// The complete mutation inventory.
    Mutation,
    /// The source-bound controller proof inventory.
    PolicyProofs,
    /// All declared fuzz targets.
    Fuzz,
}

impl Role {
    /// Number of distinct required release qualification roles.
    pub const COUNT: usize = 13;

    /// Every required qualification role exactly once.
    pub const ALL: [Self; Self::COUNT] = [
        Self::Quality,
        Self::NativeWindows,
        Self::NativeMacos,
        Self::Msrv,
        Self::NoStd,
        Self::Corpus,
        Self::Metrics,
        Self::Work,
        Self::Oracle,
        Self::Coverage,
        Self::Mutation,
        Self::PolicyProofs,
        Self::Fuzz,
    ];

    /// The workflow job identity bound to this role.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Quality => "quality",
            Self::NativeWindows => "native-windows",
            Self::NativeMacos => "native-macos",
            Self::Msrv => "msrv",
            Self::NoStd => "no-std",
            Self::Corpus => "corpus",
            Self::Metrics => "metrics",
            Self::Work => "work",
            Self::Oracle => "oracle",
            Self::Coverage => "coverage",
            Self::Mutation => "mutation",
            Self::PolicyProofs => "policy-proofs",
            Self::Fuzz => "fuzz",
        }
    }

    const fn mask(self) -> u16 {
        1u16 << (self as u8)
    }
}

/// One completed role and its post-check source identity.
#[derive(Clone, Copy, Debug)]
pub struct JobReport {
    /// The role performed by the job.
    pub role: Role,
    /// The job's actual result.
    pub outcome: Outcome,
    /// The revision emitted after its final source check.
    pub revision: Option<Revision>,
}

/// Evidence that every required role succeeded on one exact revision.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Qualified(Revision);

impl Qualified {
    /// Return the revision bound to the complete qualification.
    #[must_use]
    pub const fn revision(self) -> Revision {
        self.0
    }
}

/// Refuse partial, duplicate, skipped, failed, or mixed-source qualification.
///
/// # Errors
/// Returns `Refusal::Qualification` unless every role succeeded once on the expected revision.
pub fn qualify(revision: Revision, reports: &[JobReport]) -> Result<Qualified, Refusal> {
    if reports.len() != Role::ALL.len() {
        return Err(Refusal::Qualification);
    }
    let mut observed = 0;
    for report in reports {
        let mask = report.role.mask();
        if observed & mask != 0
            || report.outcome != Outcome::Success
            || report.revision != Some(revision)
        {
            return Err(Refusal::Qualification);
        }
        observed |= mask;
    }
    Ok(Qualified(revision))
}

#[cfg(kani)]
mod proofs {
    use super::{
        AffectedWork, Association, CodeqlScope, Event, JobReport, Outcome, QualityWork, Repository,
        Revision, Role, SourceFacts, affected_work, candidate, final_summary, ordinary_summary,
        qualify, source,
    };

    #[kani::proof]
    #[kani::unwind(64)]
    fn affected_work_routes_recognized_workflow_boundaries() {
        assert_eq!(
            affected_work(&[".github/workflows/release-plz.yml"]),
            AffectedWork {
                quality: QualityWork::Workflows,
                codeql: CodeqlScope::Actions,
            }
        );
        assert_eq!(
            affected_work(&[".github/workflows/ci.yml"]),
            AffectedWork {
                quality: QualityWork::Full,
                codeql: CodeqlScope::Both,
            }
        );
        assert_eq!(
            affected_work(&[".github/workflows/codeql.yml"]),
            AffectedWork {
                quality: QualityWork::Workflows,
                codeql: CodeqlScope::Both,
            }
        );

        // The bounded ASCII suffix keeps the generated path outside the reserved workflow names.
        let suffix: [u8; 8] = kani::any();
        for byte in suffix {
            kani::assume(byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-');
        }
        let prefix = b".github/workflows/release-";
        let extension = b".yml";
        let mut path_bytes = [0u8; b".github/workflows/release-".len() + 8 + b".yml".len()];
        let suffix_start = prefix.len();
        let extension_start = suffix_start + suffix.len();
        path_bytes[..suffix_start].copy_from_slice(prefix);
        path_bytes[suffix_start..extension_start].copy_from_slice(&suffix);
        path_bytes[extension_start..].copy_from_slice(extension);
        let path = core::str::from_utf8(&path_bytes).expect("bounded ASCII path is UTF-8");
        assert_eq!(
            affected_work(&[path]),
            AffectedWork {
                quality: QualityWork::Workflows,
                codeql: CodeqlScope::Actions,
            }
        );
        kani::cover!(suffix[0] == b'a');
    }

    #[kani::proof]
    #[kani::unwind(64)]
    fn affected_work_routes_recognized_rust_and_action_inputs() {
        assert_eq!(
            affected_work(&["crates/webpkit/src/lib.rs"]),
            AffectedWork {
                quality: QualityWork::Full,
                codeql: CodeqlScope::Rust,
            }
        );
        assert_eq!(
            affected_work(&[".github/actions/setup/action.yml"]),
            AffectedWork {
                quality: QualityWork::Full,
                codeql: CodeqlScope::Actions,
            }
        );
        assert_eq!(
            affected_work(&[
                ".github/workflows/release-plz.yml",
                "crates/webpkit/src/lib.rs"
            ]),
            AffectedWork {
                quality: QualityWork::Full,
                codeql: CodeqlScope::Both,
            }
        );

        // Fixed suffixes place the bounded ASCII component inside recognized source namespaces.
        let component: [u8; 8] = kani::any();
        for byte in component {
            kani::assume(byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-');
        }
        let crate_prefix = b"crates/";
        let crate_suffix = b"/src/lib.rs";
        let mut crate_path_bytes = [0u8; b"crates/".len() + 8 + b"/src/lib.rs".len()];
        let crate_component_end = crate_prefix.len() + component.len();
        crate_path_bytes[..crate_prefix.len()].copy_from_slice(crate_prefix);
        crate_path_bytes[crate_prefix.len()..crate_component_end].copy_from_slice(&component);
        crate_path_bytes[crate_component_end..].copy_from_slice(crate_suffix);
        let crate_path =
            core::str::from_utf8(&crate_path_bytes).expect("bounded ASCII path is UTF-8");
        assert_eq!(
            affected_work(&[crate_path]),
            AffectedWork {
                quality: QualityWork::Full,
                codeql: CodeqlScope::Rust,
            }
        );

        let action_prefix = b".github/actions/";
        let action_suffix = b"/action.yml";
        let mut action_path_bytes = [0u8; b".github/actions/".len() + 8 + b"/action.yml".len()];
        let action_component_end = action_prefix.len() + component.len();
        action_path_bytes[..action_prefix.len()].copy_from_slice(action_prefix);
        action_path_bytes[action_prefix.len()..action_component_end].copy_from_slice(&component);
        action_path_bytes[action_component_end..].copy_from_slice(action_suffix);
        let action_path =
            core::str::from_utf8(&action_path_bytes).expect("bounded ASCII path is UTF-8");
        assert_eq!(
            affected_work(&[action_path]),
            AffectedWork {
                quality: QualityWork::Full,
                codeql: CodeqlScope::Actions,
            }
        );
        kani::cover!(component[0] == b'a');
    }

    #[kani::proof]
    #[kani::unwind(64)]
    fn affected_work_unknown_and_empty_inputs_keep_full_selection() {
        assert_eq!(
            affected_work(&[]),
            AffectedWork {
                quality: QualityWork::Full,
                codeql: CodeqlScope::Both,
            }
        );
        assert_eq!(
            affected_work(&["docs/unknown-format.md"]),
            AffectedWork {
                quality: QualityWork::Full,
                codeql: CodeqlScope::Both,
            }
        );
        assert_eq!(
            affected_work(&[
                ".github/workflows/release-plz.yml",
                "docs/unknown-format.md"
            ]),
            AffectedWork {
                quality: QualityWork::Full,
                codeql: CodeqlScope::Both,
            }
        );

        // Category indices select recognized paths; bounded ASCII names under `notes/` remain unknown.
        let categories: [u8; 4] = kani::any();
        for category in categories {
            kani::assume(category < 5);
        }
        let prefix_count: usize = kani::any();
        kani::assume(prefix_count <= categories.len());
        let mut paths = [""; 5];
        for index in 0..prefix_count {
            paths[index] = match categories[index] {
                0 => ".github/workflows/release-plz.yml",
                1 => ".github/workflows/ci.yml",
                2 => ".github/workflows/codeql.yml",
                3 => "crates/webpkit/src/lib.rs",
                _ => ".github/actions/setup/action.yml",
            };
        }
        let suffix: [u8; 8] = kani::any();
        for byte in suffix {
            kani::assume(byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-');
        }
        let prefix = b"notes/";
        let mut unknown_bytes = [0u8; b"notes/".len() + 8];
        unknown_bytes[..prefix.len()].copy_from_slice(prefix);
        unknown_bytes[prefix.len()..].copy_from_slice(&suffix);
        let unknown = core::str::from_utf8(&unknown_bytes).expect("bounded ASCII path is UTF-8");
        assert_eq!(
            affected_work(&[unknown]),
            AffectedWork {
                quality: QualityWork::Full,
                codeql: CodeqlScope::Both,
            }
        );
        paths[prefix_count] = unknown;
        assert_eq!(
            affected_work(&paths[..prefix_count + 1]),
            AffectedWork {
                quality: QualityWork::Full,
                codeql: CodeqlScope::Both,
            }
        );
        kani::cover!(prefix_count == categories.len());

        // A symbolic choice ranges over independently stated malformed path shapes.
        let malformed_kind: u8 = kani::any();
        kani::assume(malformed_kind < 7);
        let malformed = match malformed_kind {
            0 => "",
            1 => "../notes/file.txt",
            2 => "/notes/file.txt",
            3 => "notes//file.txt",
            4 => "notes\\file.txt",
            5 => "notes/file:name.txt",
            _ => "notes/file\nname.txt",
        };
        assert_eq!(
            affected_work(&[malformed]),
            AffectedWork {
                quality: QualityWork::Full,
                codeql: CodeqlScope::Both,
            }
        );
    }

    #[kani::proof]
    #[kani::unwind(42)]
    fn commit_identity_is_exact() {
        let bytes: [u8; Revision::LENGTH + 1] = kani::any();
        let length = usize::from(kani::any::<u8>());
        kani::assume(length <= bytes.len());
        let input = &bytes[..length];
        let parsed = Revision::parse(input);
        assert_eq!(
            parsed.is_ok(),
            length == Revision::LENGTH && input.iter().all(u8::is_ascii_hexdigit)
        );
        if let Ok(revision) = parsed {
            for (actual, input) in revision.bytes().iter().zip(input) {
                assert_eq!(*actual, input.to_ascii_lowercase());
            }
            assert_eq!(Revision::parse(revision.bytes()), Ok(revision));
            let index_clean: bool = kani::any();
            let worktree_clean: bool = kani::any();
            let result = source(
                revision,
                SourceFacts {
                    head: revision,
                    index_clean,
                    worktree_clean,
                },
            );
            assert_eq!(result.is_ok(), index_clean && worktree_clean);
        }
        kani::cover!(parsed.is_ok());
        kani::cover!(length < Revision::LENGTH);
        kani::cover!(length > Revision::LENGTH);
    }

    #[kani::proof]
    #[kani::unwind(41)]
    fn candidate_selection_preserves_source() {
        let revision = Revision([b'a'; Revision::LENGTH]);
        let release_branch: bool = kani::any();
        let merged: bool = kani::any();
        let same_repository: bool = kani::any();
        let main_branch: bool = kani::any();
        let active_main: bool = kani::any();
        let current_revision: bool = kani::any();
        let merge_revision: bool = kani::any();
        let present: bool = kani::any();
        let other = Revision([b'b'; Revision::LENGTH]);
        let record = Association {
            release_branch,
            merged,
            repository: if same_repository {
                Repository::Webpkit
            } else {
                Repository::Other
            },
            main_branch,
            merge_revision: present.then_some(if merge_revision { revision } else { other }),
        };
        let result = candidate(
            Repository::Webpkit,
            active_main,
            revision,
            if current_revision { revision } else { other },
            &[record],
        );
        assert_eq!(
            result.is_ok(),
            active_main
                && current_revision
                && (!release_branch
                    || merged && same_repository && main_branch && present && merge_revision)
        );
        if let Ok(Some(selected)) = result {
            assert_eq!(selected.revision(), revision);
        }
        assert!(candidate(Repository::Other, true, revision, revision, &[record]).is_err());
        assert!(
            candidate(
                Repository::Webpkit,
                true,
                revision,
                revision,
                &[record, record]
            )
            .is_err()
                || !release_branch
        );
        kani::cover!(matches!(result, Ok(Some(_))));
        kani::cover!(matches!(result, Ok(None)));
    }

    #[kani::proof]
    fn ordinary_summary_requires_complete_work() {
        let event: Event = kani::any();
        let quality: Outcome = kani::any();
        let title: Outcome = kani::any();
        let result = ordinary_summary(event, quality, title);
        if result.is_ok() {
            assert_eq!(quality, Outcome::Success);
            if matches!(event, Event::PullRequest) {
                assert_eq!(title, Outcome::Success);
            }
        }
        assert_eq!(final_summary(quality).is_ok(), quality == Outcome::Success);
        kani::cover!(result.is_ok());
        kani::cover!(result.is_err());
    }

    #[kani::proof]
    #[kani::unwind(14)]
    fn publication_requires_complete_same_source_qualification() {
        let revision = Revision([b'a'; Revision::LENGTH]);
        let other = Revision([b'b'; Revision::LENGTH]);
        let mut reports = [JobReport {
            role: Role::Quality,
            outcome: Outcome::Success,
            revision: Some(revision),
        }; Role::COUNT];
        let mut all_success = true;
        for (report, role) in reports.iter_mut().zip(Role::ALL) {
            let outcome: Outcome = kani::any();
            let present: bool = kani::any();
            let same_source: bool = kani::any();
            *report = JobReport {
                role,
                outcome,
                revision: present.then_some(if same_source { revision } else { other }),
            };
            all_success &= outcome == Outcome::Success && present && same_source;
        }
        let result = qualify(revision, &reports);
        assert_eq!(result.is_ok(), all_success);
        if let Ok(qualified) = result {
            assert_eq!(qualified.revision(), revision);
        }
        assert!(qualify(revision, &reports[..Role::COUNT - 1]).is_err());
        reports[1].role = reports[0].role;
        assert!(qualify(revision, &reports).is_err());
        kani::cover!(result.is_ok());
        kani::cover!(result.is_err());
    }

    #[kani::proof]
    #[kani::unwind(14)]
    fn reject_incomplete_qualification_probe() {
        let revision = Revision([b'a'; Revision::LENGTH]);
        let mut reports = [JobReport {
            role: Role::Quality,
            outcome: Outcome::Success,
            revision: Some(revision),
        }; Role::COUNT];
        for (report, role) in reports.iter_mut().zip(Role::ALL) {
            report.role = role;
        }
        reports[Role::COUNT - 1].outcome = Outcome::Skipped;
        assert!(qualify(revision, &reports).is_ok());
    }
}
