//! `-metadata` selection: which ICC / Exif / XMP fields to carry.
//!
//! This project preserves metadata by default (kinder than cwebp, which strips
//! it), so an unspecified `--metadata` means "keep everything the source has".

use clap::ValueEnum;
use webpkit::lossless::Metadata;

/// A single choice accepted by `--metadata` (comma-separated).
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum MetadataField {
    /// Keep ICC, Exif, and XMP.
    All,
    /// Strip everything (a bare `VP8L` output).
    None,
    /// Keep the ICC color profile.
    Icc,
    /// Keep Exif.
    Exif,
    /// Keep XMP.
    Xmp,
}

/// Which metadata fields to keep, resolved from the `--metadata` flags.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Selection {
    /// Keep the ICC color profile.
    pub icc: bool,
    /// Keep Exif.
    pub exif: bool,
    /// Keep XMP.
    pub xmp: bool,
}

impl Selection {
    /// Keep all metadata — the default when `--metadata` is unspecified.
    #[must_use]
    pub const fn all() -> Self {
        Self {
            icc: true,
            exif: true,
            xmp: true,
        }
    }

    /// Keep nothing.
    #[must_use]
    pub const fn none() -> Self {
        Self {
            icc: false,
            exif: false,
            xmp: false,
        }
    }

    /// Fold the parsed `--metadata` values into a selection.
    ///
    /// An empty list keeps everything (the preserve-by-default policy). `None`
    /// clears the accumulator; later fields re-enable individual kinds.
    #[must_use]
    pub fn from_fields(fields: &[MetadataField]) -> Self {
        if fields.is_empty() {
            return Self::all();
        }
        let mut sel = Self::none();
        for field in fields {
            match field {
                MetadataField::All => sel = Self::all(),
                MetadataField::None => sel = Self::none(),
                MetadataField::Icc => sel.icc = true,
                MetadataField::Exif => sel.exif = true,
                MetadataField::Xmp => sel.xmp = true,
            }
        }
        sel
    }

    /// Project a source [`Metadata`] down to the selected fields.
    #[must_use]
    pub fn apply(self, source: &Metadata) -> Metadata {
        Metadata {
            icc_profile: self.icc.then(|| source.icc_profile.clone()).flatten(),
            exif: self.exif.then(|| source.exif.clone()).flatten(),
            xmp: self.xmp.then(|| source.xmp.clone()).flatten(),
        }
    }
}

#[cfg(test)]
mod tests {
    use webpkit::lossless::Metadata;

    use super::{MetadataField, Selection};

    fn full() -> Metadata {
        Metadata {
            icc_profile: Some(vec![1]),
            exif: Some(vec![2]),
            xmp: Some(vec![3]),
        }
    }

    #[test]
    fn no_flags_preserves_everything() {
        assert_eq!(Selection::from_fields(&[]), Selection::all());
        assert_eq!(Selection::from_fields(&[]).apply(&full()), full());
    }

    #[test]
    fn none_strips_all() {
        let out = Selection::from_fields(&[MetadataField::None]).apply(&full());
        assert!(out.is_empty());
    }

    #[test]
    fn icc_keeps_only_icc() {
        let out = Selection::from_fields(&[MetadataField::Icc]).apply(&full());
        assert!(out.icc_profile.is_some() && out.exif.is_none() && out.xmp.is_none());
    }

    #[test]
    fn none_then_field_re_enables_that_field() {
        let out =
            Selection::from_fields(&[MetadataField::None, MetadataField::Exif]).apply(&full());
        assert!(out.icc_profile.is_none() && out.exif.is_some() && out.xmp.is_none());
    }
}
