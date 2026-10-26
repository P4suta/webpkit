//! Revision parser invariants shared by fuzz discovery and committed seed replay.
#![forbid(unsafe_code)]

use webpkit_ci_core::Revision;

/// The accepted and rejected inputs reached by one exercise.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ExerciseCounts {
    /// Inputs accepted by the revision parser.
    pub accepted: usize,
    /// Inputs rejected by the revision parser.
    pub rejected: usize,
}

impl ExerciseCounts {
    const fn record(&mut self, accepted: bool) {
        if accepted {
            self.accepted += 1;
        } else {
            self.rejected += 1;
        }
    }
}

fn check(input: &[u8]) -> bool {
    let expected = input.len() == Revision::LENGTH
        && input
            .iter()
            .all(|byte| b"0123456789abcdefABCDEF".contains(byte));
    let parsed = Revision::parse(input);
    assert_eq!(parsed.is_ok(), expected);
    if let Ok(revision) = parsed {
        for (&actual, &original) in revision.bytes().iter().zip(input) {
            let normalized = match original {
                b'A'..=b'F' => original + (b'a' - b'A'),
                _ => original,
            };
            assert_eq!(actual, normalized);
        }
        assert_eq!(Revision::parse(revision.bytes()), Ok(revision));
    }
    expected
}

/// Check the raw input and every prefix through the full revision length.
///
/// At most 41 prefixes follow the raw check, including the empty prefix.
///
/// # Panics
/// Panics if acceptance, normalization, or reparsing violates the revision contract.
#[must_use]
pub fn exercise(input: &[u8]) -> ExerciseCounts {
    let mut counts = ExerciseCounts::default();
    counts.record(check(input));
    for length in 0..=input.len().min(Revision::LENGTH) {
        counts.record(check(&input[..length]));
    }
    counts
}

#[cfg(test)]
mod tests {
    use std::error::Error;
    use std::fs;
    use std::path::Path;

    use super::{ExerciseCounts, check, exercise};

    #[test]
    fn literal_acceptance_nonhex_and_length_cases_reach_the_parser() {
        let valid = b"ABCDEF0123456789ABCDEF0123456789ABCDEF01";
        assert!(check(valid));
        let mut invalid = *valid;
        invalid[0] = b'!';
        assert!(!check(&invalid));
        assert!(!check(&valid[..valid.len() - 1]));
        assert!(!check(b"ABCDEF0123456789ABCDEF0123456789ABCDEF01\n"));
    }

    #[test]
    fn newline_seed_reaches_accepted_prefix_and_rejected_raw_input() {
        let counts = exercise(b"ABCDEF0123456789ABCDEF0123456789ABCDEF01\n");
        assert_eq!(counts.accepted, 1);
        assert_eq!(counts.accepted + counts.rejected, 42);
    }

    #[test]
    fn replay_all_committed_revision_seeds_reaches_both_results() -> Result<(), Box<dyn Error>> {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("corpus/revision");
        let mut seeds = fs::read_dir(root)?.collect::<Result<Vec<_>, _>>()?;
        seeds.sort_by_key(std::fs::DirEntry::file_name);
        assert!(
            seeds.len() >= 3,
            "the corpus must retain all three committed revision seeds"
        );
        let mut total = ExerciseCounts::default();
        for seed in seeds {
            assert!(seed.file_type()?.is_file());
            let counts = exercise(&fs::read(seed.path())?);
            total.accepted += counts.accepted;
            total.rejected += counts.rejected;
        }
        assert!(total.accepted > 0);
        assert!(total.rejected > 0);
        Ok(())
    }
}
