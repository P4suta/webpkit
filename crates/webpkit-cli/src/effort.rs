//! Map cwebp's effort dials onto the continuous [`Effort`] scale.
//!
//! For **lossless** output each of cwebp's numeric effort knobs (`-m` / `-z` /
//! `-q`) maps monotonically onto a fixed [`Effort::level`] (`0..=9`) — no flag is
//! flattened into a coarse bucket; when several are given, `-m` wins over `-z`
//! wins over `-q`. With no effort flag the encoder adapts ([`Effort::AUTO`]). For
//! **lossy** output `-q` is the real quality (not effort), so only `-m` selects
//! the effort [`Effort`], on the same `-m` scale.

use webpkit::Effort;

/// Resolve the lossless effort flags to an [`Effort`] ([`Effort::AUTO`] when none
/// given).
#[must_use]
pub(crate) fn resolve(method: Option<i64>, level: Option<i64>, quality: Option<f64>) -> Effort {
    match (method, level, quality) {
        (Some(m), _, _) => from_m(m),
        (_, Some(z), _) => from_z(z),
        (_, _, Some(q)) => from_q(q),
        _ => Effort::AUTO,
    }
}

/// Resolve the lossy effort [`Effort`] from `-m` alone ([`Effort::AUTO`] by
/// default), on the same `-m` scale as the lossless path.
#[must_use]
pub(crate) const fn lossy_method(method: Option<i64>) -> Effort {
    match method {
        Some(m) => from_m(m),
        None => Effort::AUTO,
    }
}

/// Clamp a cwebp `-q` float to the encoder's integer quality (`0..=100`).
///
/// `-q` values outside the range saturate, matching cwebp. The nearest integer is
/// found by scanning `0..=100` rather than casting the float, which keeps the
/// conversion free of a lossy `f64 as u8` cast.
#[must_use]
pub(crate) fn lossy_quality(quality: f64) -> u8 {
    let target = quality.clamp(0.0, 100.0).round();
    (0..=100u8).find(|&n| f64::from(n) >= target).unwrap_or(100)
}

/// Map cwebp `-m` (method, `0..=6`) monotonically onto the `0..=9` breadth scale.
const fn from_m(m: i64) -> Effort {
    match m {
        i64::MIN..=0 => Effort::level(0),
        1 => Effort::level(1),
        2 => Effort::level(3),
        3 => Effort::level(4),
        4 => Effort::level(6),
        5 => Effort::level(7),
        _ => Effort::level(9),
    }
}

/// Map cwebp `-z` (lossless preset, `0..=9`) straight onto the breadth level.
const fn from_z(z: i64) -> Effort {
    match z {
        i64::MIN..=0 => Effort::level(0),
        1 => Effort::level(1),
        2 => Effort::level(2),
        3 => Effort::level(3),
        4 => Effort::level(4),
        5 => Effort::level(5),
        6 => Effort::level(6),
        7 => Effort::level(7),
        8 => Effort::level(8),
        _ => Effort::level(9),
    }
}

/// Map a cwebp lossless `-q` (`0..=100`) onto the breadth level in tenths.
fn from_q(q: f64) -> Effort {
    let target = q.clamp(0.0, 100.0);
    let level = (0..=9u8)
        .rev()
        .find(|&n| target >= f64::from(n) * 10.0)
        .unwrap_or(0);
    Effort::level(level)
}

#[cfg(test)]
mod tests {
    use webpkit::Effort;

    use super::{lossy_method, lossy_quality, resolve};

    #[test]
    fn defaults_to_auto() {
        assert_eq!(resolve(None, None, None), Effort::AUTO);
    }

    #[test]
    fn lossy_method_maps_dash_m_only() {
        // `-m` alone selects effort; no flag adapts. The `-m 0..=6` scale spreads
        // monotonically across the `0..=9` breadth levels.
        assert_eq!(lossy_method(None), Effort::AUTO);
        assert_eq!(lossy_method(Some(0)), Effort::level(0));
        assert_eq!(lossy_method(Some(2)), Effort::level(3));
        assert_eq!(lossy_method(Some(4)), Effort::level(6));
        assert_eq!(lossy_method(Some(6)), Effort::level(9));
    }

    #[test]
    fn lossy_quality_rounds_and_saturates() {
        assert_eq!(lossy_quality(75.0), 75);
        assert_eq!(lossy_quality(0.0), 0);
        assert_eq!(lossy_quality(100.0), 100);
        assert_eq!(lossy_quality(101.0), 100);
        assert_eq!(lossy_quality(-5.0), 0);
        assert_eq!(lossy_quality(79.4), 79);
        assert_eq!(lossy_quality(79.6), 80);
    }

    #[test]
    fn method_flag_maps_monotonically() {
        assert_eq!(resolve(Some(0), None, None), Effort::level(0));
        assert_eq!(resolve(Some(4), None, None), Effort::level(6));
        assert_eq!(resolve(Some(6), None, None), Effort::level(9));
    }

    #[test]
    fn level_and_quality_map_continuously() {
        assert_eq!(resolve(None, Some(1), None), Effort::level(1));
        assert_eq!(resolve(None, Some(9), None), Effort::level(9));
        assert_eq!(resolve(None, None, Some(10.0)), Effort::level(1));
        assert_eq!(resolve(None, None, Some(75.0)), Effort::level(7));
        assert_eq!(resolve(None, None, Some(100.0)), Effort::level(9));
    }

    #[test]
    fn method_wins_over_level_and_quality() {
        assert_eq!(resolve(Some(0), Some(9), Some(100.0)), Effort::level(0));
        assert_eq!(resolve(None, Some(0), Some(100.0)), Effort::level(0));
    }
}
