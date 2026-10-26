//! Dependency-free decisions shared by production automation and source proofs.
#![no_std]
#![forbid(unsafe_code)]

#[path = "../../src/ci.rs"]
mod decisions;

pub use decisions::{
    Association, Candidate, Event, JobReport, Outcome, Qualified, Refusal, Repository, Revision,
    Role, SourceFacts, candidate, final_summary, ordinary_summary, qualify, source,
};

#[cfg(test)]
mod tests {
    use super::{Association, Event, JobReport, Outcome, Repository, Revision, Role, SourceFacts};

    fn revision() -> Result<Revision, super::Refusal> {
        Revision::parse(&[b'a'; Revision::LENGTH])
    }

    #[test]
    fn full_identity_rejects_short_long_and_nonhex_inputs() {
        assert!(Revision::parse(&[b'a'; 39]).is_err());
        assert!(Revision::parse(&[b'a'; 41]).is_err());
        for byte in 0..=u8::MAX {
            let input = [byte; Revision::LENGTH];
            assert_eq!(Revision::parse(&input).is_ok(), byte.is_ascii_hexdigit());
        }
        assert_eq!(Revision::parse(&[b'A'; Revision::LENGTH]), revision());
    }

    #[test]
    fn either_tracked_delta_or_wrong_head_refuses_source() {
        let expected = revision().unwrap();
        let other = Revision::parse(&[b'b'; Revision::LENGTH]).unwrap();
        for head in [expected, other] {
            for index_clean in [false, true] {
                for worktree_clean in [false, true] {
                    assert_eq!(
                        super::source(
                            expected,
                            SourceFacts {
                                head,
                                index_clean,
                                worktree_clean
                            }
                        )
                        .is_ok(),
                        head == expected && index_clean && worktree_clean
                    );
                }
            }
        }
    }

    #[test]
    fn summary_never_accepts_skipped_quality_or_cancelled_work() {
        for event in [
            Event::PullRequest,
            Event::MergeGroup,
            Event::WorkflowDispatch,
        ] {
            for quality in [
                Outcome::Success,
                Outcome::Failure,
                Outcome::Cancelled,
                Outcome::Skipped,
            ] {
                for title in [
                    Outcome::Success,
                    Outcome::Failure,
                    Outcome::Cancelled,
                    Outcome::Skipped,
                ] {
                    let title_allowed = title == Outcome::Success
                        || !matches!(event, Event::PullRequest) && title == Outcome::Skipped;
                    assert_eq!(
                        super::ordinary_summary(event, quality, title).is_ok(),
                        quality == Outcome::Success && title_allowed
                    );
                }
                assert_eq!(
                    super::final_summary(quality).is_ok(),
                    quality == Outcome::Success
                );
            }
        }
    }

    #[test]
    fn every_role_is_required_once_on_the_postcheck_source() {
        let revision = revision().unwrap();
        let reports = Role::ALL.map(|role| JobReport {
            role,
            outcome: Outcome::Success,
            revision: Some(revision),
        });
        assert_eq!(
            super::qualify(revision, &reports).unwrap().revision(),
            revision
        );
        for index in 0..reports.len() {
            for outcome in [Outcome::Failure, Outcome::Cancelled, Outcome::Skipped] {
                let mut changed = reports;
                changed[index].outcome = outcome;
                assert!(super::qualify(revision, &changed).is_err());
            }
            for source in [
                None,
                Some(Revision::parse(&[b'b'; Revision::LENGTH]).unwrap()),
            ] {
                let mut changed = reports;
                changed[index].revision = source;
                assert!(super::qualify(revision, &changed).is_err());
            }
            let mut changed = reports;
            changed[index].role = reports[(index + 1) % reports.len()].role;
            assert!(super::qualify(revision, &changed).is_err());
        }
        assert!(super::qualify(revision, &reports[..12]).is_err());
        assert!(super::qualify(revision, &[]).is_err());
    }

    #[test]
    fn candidate_requires_unique_merged_canonical_association_and_current_main() {
        let revision = revision().unwrap();
        let association = Association {
            release_branch: true,
            merged: true,
            repository: Repository::Webpkit,
            main_branch: true,
            merge_revision: Some(revision),
        };
        assert_eq!(
            super::candidate(Repository::Webpkit, true, revision, revision, &[]),
            Ok(None)
        );
        assert_eq!(
            super::candidate(
                Repository::Webpkit,
                true,
                revision,
                revision,
                &[association]
            )
            .unwrap()
            .unwrap()
            .revision(),
            revision
        );
        assert!(
            super::candidate(
                Repository::Webpkit,
                true,
                revision,
                revision,
                &[association, association]
            )
            .is_err()
        );
        for changed in [
            Association {
                merged: false,
                ..association
            },
            Association {
                repository: Repository::Other,
                ..association
            },
            Association {
                main_branch: false,
                ..association
            },
            Association {
                merge_revision: None,
                ..association
            },
        ] {
            assert!(
                super::candidate(Repository::Webpkit, true, revision, revision, &[changed])
                    .is_err()
            );
        }
        assert!(
            super::candidate(Repository::Other, true, revision, revision, &[association]).is_err()
        );
        assert!(
            super::candidate(
                Repository::Webpkit,
                false,
                revision,
                revision,
                &[association]
            )
            .is_err()
        );
        assert!(
            super::candidate(
                Repository::Webpkit,
                true,
                revision,
                Revision::parse(&[b'b'; Revision::LENGTH]).unwrap(),
                &[association]
            )
            .is_err()
        );
    }
}
