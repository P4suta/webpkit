//! Map cwebp's effort dials onto the three-level codec presets.
//!
//! For **lossless** output all of cwebp's numeric effort knobs (`-m` / `-z` /
//! `-q`) collapse onto the three-level [`Effort`], chosen so each flag's *default*
//! (`-m 4` / `-z 6` / `-q 75`) lands on Balanced; when several are given, `-m`
//! wins over `-z` wins over `-q`. For **lossy** output `-q` is the real quality
//! (not effort), so only `-m` selects the effort [`Effort`], using the same
//! three-tier `-m` buckets.

use webpkit::Effort;

/// Resolve the lossless effort flags to an [`Effort`] (Balanced when none given).
#[must_use]
pub fn resolve(method: Option<i64>, level: Option<i64>, quality: Option<f64>) -> Effort {
    match (method, level, quality) {
        (Some(m), _, _) => from_m(m),
        (_, Some(z), _) => from_z(z),
        (_, _, Some(q)) => from_q(q),
        _ => Effort::Balanced,
    }
}

/// Resolve the lossy effort [`Effort`] from `-m` alone (Balanced by default),
/// using the same three-tier `-m` buckets as the lossless path.
#[must_use]
pub const fn lossy_method(method: Option<i64>) -> Effort {
    match method {
        Some(m) if m <= 2 => Effort::Fast,
        Some(m) if m <= 5 => Effort::Balanced,
        Some(_) => Effort::Best,
        None => Effort::Balanced,
    }
}

/// Clamp a cwebp `-q` float to the encoder's integer quality (`0..=100`).
///
/// `-q` values outside the range saturate, matching cwebp. The nearest integer is
/// found by scanning `0..=100` rather than casting the float, which keeps the
/// conversion free of a lossy `f64 as u8` cast.
#[must_use]
pub fn lossy_quality(quality: f64) -> u8 {
    let target = quality.clamp(0.0, 100.0).round();
    (0..=100u8).find(|&n| f64::from(n) >= target).unwrap_or(100)
}

const fn from_m(m: i64) -> Effort {
    if m <= 2 {
        Effort::Fast
    } else if m <= 5 {
        Effort::Balanced
    } else {
        Effort::Best
    }
}

const fn from_z(z: i64) -> Effort {
    if z <= 2 {
        Effort::Fast
    } else if z <= 6 {
        Effort::Balanced
    } else {
        Effort::Best
    }
}

fn from_q(q: f64) -> Effort {
    if q < 34.0 {
        Effort::Fast
    } else if q < 90.0 {
        Effort::Balanced
    } else {
        Effort::Best
    }
}

#[cfg(test)]
mod tests {
    use webpkit::Effort;

    use super::{lossy_method, lossy_quality, resolve};

    #[test]
    fn defaults_to_balanced() {
        assert_eq!(resolve(None, None, None), Effort::Balanced);
    }

    #[test]
    fn lossy_method_buckets_on_dash_m_only() {
        assert_eq!(lossy_method(None), Effort::Balanced);
        assert_eq!(lossy_method(Some(0)), Effort::Fast);
        assert_eq!(lossy_method(Some(2)), Effort::Fast);
        assert_eq!(lossy_method(Some(4)), Effort::Balanced);
        assert_eq!(lossy_method(Some(5)), Effort::Balanced);
        assert_eq!(lossy_method(Some(6)), Effort::Best);
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
    fn method_flag_buckets() {
        assert_eq!(resolve(Some(0), None, None), Effort::Fast);
        assert_eq!(resolve(Some(4), None, None), Effort::Balanced);
        assert_eq!(resolve(Some(6), None, None), Effort::Best);
    }

    #[test]
    fn level_and_quality_buckets() {
        assert_eq!(resolve(None, Some(1), None), Effort::Fast);
        assert_eq!(resolve(None, Some(9), None), Effort::Best);
        assert_eq!(resolve(None, None, Some(10.0)), Effort::Fast);
        assert_eq!(resolve(None, None, Some(100.0)), Effort::Best);
    }

    #[test]
    fn method_wins_over_level_and_quality() {
        assert_eq!(resolve(Some(0), Some(9), Some(100.0)), Effort::Fast);
        assert_eq!(resolve(None, Some(0), Some(100.0)), Effort::Fast);
    }
}
