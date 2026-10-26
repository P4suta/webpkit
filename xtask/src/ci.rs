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
        Association, Event, JobReport, Outcome, Repository, Revision, Role, SourceFacts, candidate,
        final_summary, ordinary_summary, qualify, source,
    };

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
