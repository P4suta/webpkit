//! The per-type machinery each setting needs: parse from an env/CLI string, parse
//! from a TOML value, render for humans, render as a TOML literal, and serialize
//! to JSON.
//!
//! One [`ConfigValue`] impl per setting type keeps the `super::settings!` table
//! type-driven: the table names a type, and the impl supplies every conversion, so
//! the table stays a list of names rather than a pile of per-field closures.

use serde_json::Value as Json;

use crate::{metadata::Selection, term::ColorChoice};

/// Everything a setting's value type must provide to flow through the config
/// layers. Errors are plain strings; the caller wraps them with the variable name
/// or file location that gives them context.
pub(crate) trait ConfigValue: Sized + Clone {
    /// Parse from an environment-variable or command-line string.
    ///
    /// # Errors
    ///
    /// A human-readable reason the string is not a valid value.
    fn parse(text: &str) -> Result<Self, String>;

    /// Parse from a TOML value.
    ///
    /// # Errors
    ///
    /// A human-readable reason the TOML value is not a valid value.
    fn from_toml(value: &toml::Value) -> Result<Self, String>;

    /// The human-readable rendering, also the `config get` output.
    fn render(&self) -> String;

    /// The TOML right-hand side for the template (quoted where TOML needs it).
    fn toml_literal(&self) -> String;

    /// The JSON representation of the value.
    fn json(&self) -> Json;
}

/// Lossy quality, constrained to 0-100.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Quality(pub(crate) u8);

impl Quality {
    /// The default lossy quality, matching libwebp's `cwebp`.
    pub(crate) const DEFAULT: Self = Self(75);

    /// Build a quality, rejecting anything above 100.
    ///
    /// # Errors
    ///
    /// A message if `value` exceeds 100.
    pub(crate) fn new(value: u8) -> Result<Self, String> {
        if value > 100 {
            Err(format!("quality must be 0-100, got {value}"))
        } else {
            Ok(Self(value))
        }
    }
}

impl ConfigValue for Quality {
    fn parse(text: &str) -> Result<Self, String> {
        let number: u32 = text
            .trim()
            .parse()
            .map_err(|_ignored| format!("quality must be 0-100, got `{}`", text.trim()))?;
        let byte = u8::try_from(number)
            .map_err(|_ignored| format!("quality must be 0-100, got {number}"))?;
        Self::new(byte)
    }

    fn from_toml(value: &toml::Value) -> Result<Self, String> {
        let number = value
            .as_integer()
            .ok_or_else(|| "quality must be an integer 0-100".to_owned())?;
        let byte = u8::try_from(number)
            .map_err(|_ignored| format!("quality must be 0-100, got {number}"))?;
        Self::new(byte)
    }

    fn render(&self) -> String {
        self.0.to_string()
    }

    fn toml_literal(&self) -> String {
        self.0.to_string()
    }

    fn json(&self) -> Json {
        Json::from(self.0)
    }
}

/// Which codec a plain `webp` run defaults to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub(crate) enum Codec {
    /// Lossless (VP8L).
    Lossless,
    /// Lossy (VP8).
    Lossy,
}

impl ConfigValue for Codec {
    fn parse(text: &str) -> Result<Self, String> {
        match text.trim().to_ascii_lowercase().as_str() {
            "lossless" => Ok(Self::Lossless),
            "lossy" => Ok(Self::Lossy),
            other => Err(format!("codec must be lossless or lossy, got `{other}`")),
        }
    }

    fn from_toml(value: &toml::Value) -> Result<Self, String> {
        let text = value
            .as_str()
            .ok_or_else(|| "codec must be a string (\"lossless\" or \"lossy\")".to_owned())?;
        Self::parse(text)
    }

    fn render(&self) -> String {
        match self {
            Self::Lossless => "lossless",
            Self::Lossy => "lossy",
        }
        .to_owned()
    }

    fn toml_literal(&self) -> String {
        format!("\"{}\"", self.render())
    }

    fn json(&self) -> Json {
        Json::from(self.render())
    }
}

/// The decode pixel-count safety cap.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MaxPixels {
    /// A `width * height` ceiling.
    Limited(u64),
    /// No cap (opt out of the safe-by-default limit).
    Unbounded,
}

impl MaxPixels {
    /// Parse `none`, a plain count, or a count with a `K`/`M`/`G` suffix.
    ///
    /// # Errors
    ///
    /// A message if the text is neither `none` nor a (suffixed) count, or if the
    /// count overflows `u64`.
    pub(crate) fn parse(text: &str) -> Result<Self, String> {
        let trimmed = text.trim();
        if trimmed.eq_ignore_ascii_case("none") {
            return Ok(Self::Unbounded);
        }
        let (digits, scale) = match trimmed.chars().last() {
            Some('k' | 'K') => (&trimmed[..trimmed.len() - 1], 1_000),
            Some('m' | 'M') => (&trimmed[..trimmed.len() - 1], 1_000_000),
            Some('g' | 'G') => (&trimmed[..trimmed.len() - 1], 1_000_000_000),
            _ => (trimmed, 1),
        };
        let base: u64 = digits
            .trim()
            .parse()
            .map_err(|_ignored| format!("expected a pixel count or `none`, got `{trimmed}`"))?;
        base.checked_mul(scale)
            .map(Self::Limited)
            .ok_or_else(|| format!("`{trimmed}` is too large"))
    }
}

impl ConfigValue for MaxPixels {
    fn parse(text: &str) -> Result<Self, String> {
        Self::parse(text)
    }

    fn from_toml(value: &toml::Value) -> Result<Self, String> {
        if let Some(number) = value.as_integer() {
            return u64::try_from(number)
                .map(Self::Limited)
                .map_err(|_ignored| "max_pixels must be non-negative".to_owned());
        }
        if let Some(text) = value.as_str() {
            return Self::parse(text);
        }
        Err("max_pixels must be an integer or \"none\"".to_owned())
    }

    fn render(&self) -> String {
        match self {
            Self::Limited(count) => count.to_string(),
            Self::Unbounded => "none".to_owned(),
        }
    }

    fn toml_literal(&self) -> String {
        match self {
            Self::Limited(count) => count.to_string(),
            Self::Unbounded => "\"none\"".to_owned(),
        }
    }

    fn json(&self) -> Json {
        match self {
            Self::Limited(count) => Json::from(*count),
            Self::Unbounded => Json::from("none"),
        }
    }
}

impl ConfigValue for webpkit::Effort {
    fn parse(text: &str) -> Result<Self, String> {
        match text.trim().to_ascii_lowercase().as_str() {
            "fast" => Ok(Self::Fast),
            "balanced" => Ok(Self::Balanced),
            "best" => Ok(Self::Best),
            other => Err(format!(
                "effort must be fast, balanced, or best, got `{other}`"
            )),
        }
    }

    fn from_toml(value: &toml::Value) -> Result<Self, String> {
        let text = value
            .as_str()
            .ok_or_else(|| "effort must be a string (fast, balanced, or best)".to_owned())?;
        Self::parse(text)
    }

    fn render(&self) -> String {
        match self {
            Self::Fast => "fast",
            Self::Balanced => "balanced",
            Self::Best => "best",
        }
        .to_owned()
    }

    fn toml_literal(&self) -> String {
        format!("\"{}\"", self.render())
    }

    fn json(&self) -> Json {
        Json::from(self.render())
    }
}

impl ConfigValue for ColorChoice {
    fn parse(text: &str) -> Result<Self, String> {
        match text.trim().to_ascii_lowercase().as_str() {
            "auto" => Ok(Self::Auto),
            "always" => Ok(Self::Always),
            "never" => Ok(Self::Never),
            other => Err(format!(
                "color must be auto, always, or never, got `{other}`"
            )),
        }
    }

    fn from_toml(value: &toml::Value) -> Result<Self, String> {
        let text = value
            .as_str()
            .ok_or_else(|| "color must be a string (auto, always, or never)".to_owned())?;
        Self::parse(text)
    }

    fn render(&self) -> String {
        match self {
            Self::Auto => "auto",
            Self::Always => "always",
            Self::Never => "never",
        }
        .to_owned()
    }

    fn toml_literal(&self) -> String {
        format!("\"{}\"", self.render())
    }

    fn json(&self) -> Json {
        Json::from(self.render())
    }
}

impl ConfigValue for Selection {
    fn parse(text: &str) -> Result<Self, String> {
        let trimmed = text.trim();
        if trimmed.eq_ignore_ascii_case("all") {
            return Ok(Self::all());
        }
        if trimmed.eq_ignore_ascii_case("none") {
            return Ok(Self::none());
        }
        let mut selection = Self::none();
        for part in trimmed.split(',') {
            match part.trim().to_ascii_lowercase().as_str() {
                "" => {},
                "icc" => selection.icc = true,
                "exif" => selection.exif = true,
                "xmp" => selection.xmp = true,
                other => {
                    return Err(format!(
                        "metadata must be all, none, or a list of icc/exif/xmp, got `{other}`"
                    ));
                },
            }
        }
        Ok(selection)
    }

    fn from_toml(value: &toml::Value) -> Result<Self, String> {
        let text = value
            .as_str()
            .ok_or_else(|| "metadata must be a string (all, none, or a list)".to_owned())?;
        Self::parse(text)
    }

    fn render(&self) -> String {
        if self.icc && self.exif && self.xmp {
            return "all".to_owned();
        }
        let present: Vec<&str> = [("icc", self.icc), ("exif", self.exif), ("xmp", self.xmp)]
            .into_iter()
            .filter_map(|(name, on)| on.then_some(name))
            .collect();
        if present.is_empty() {
            "none".to_owned()
        } else {
            present.join(", ")
        }
    }

    fn toml_literal(&self) -> String {
        format!("\"{}\"", self.render())
    }

    fn json(&self) -> Json {
        Json::from(self.render())
    }
}

impl ConfigValue for u16 {
    fn parse(text: &str) -> Result<Self, String> {
        text.trim()
            .parse()
            .map_err(|_ignored| format!("expected a thread count, got `{}`", text.trim()))
    }

    fn from_toml(value: &toml::Value) -> Result<Self, String> {
        let number = value
            .as_integer()
            .ok_or_else(|| "threads must be an integer".to_owned())?;
        Self::try_from(number).map_err(|_ignored| format!("threads out of range: {number}"))
    }

    fn render(&self) -> String {
        self.to_string()
    }

    fn toml_literal(&self) -> String {
        self.to_string()
    }

    fn json(&self) -> Json {
        Json::from(*self)
    }
}
