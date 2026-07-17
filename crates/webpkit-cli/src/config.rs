//! Layered configuration with provenance: `args > env > webp.toml > defaults`.
//!
//! Every setting has exactly one resolved value, and [`Settings`] records *where*
//! that value came from — a config file with a line, a `WEBP_*` variable, a
//! command-line flag, or the built-in default. `webp config` prints that table.
//!
//! The fold is a pure function ([`Settings::resolve`]): it takes the layers,
//! highest-priority-first, and for each field keeps the first layer that set it.
//! No I/O, so it is exhaustively unit-testable. Loading the layers *is* I/O and
//! lives in [`load`], which routes every read through [`crate::io`].
//!
//! Settings, their env names, their TOML keys, and their four in-memory
//! representations are declared once in the `settings!` table below; the macro
//! derives the [`Partial`] / [`Settings`] structs, the env and TOML loaders, the
//! `webp config` rows, `config get`, the JSON object, and the template from it, so
//! the representations cannot drift out of step.

mod load;
mod value;

use std::path::PathBuf;

use serde_json::{Map, Value as Json};
pub(crate) use value::{Codec, ConfigValue, MaxPixels, Quality};
use webpkit::{DEFAULT_MAX_PIXELS, Effort};

pub(crate) use self::load::file_layers;
use crate::{error::CliError, metadata::Selection, term::ColorChoice};

/// Where a resolved value came from. The variants are also the precedence order:
/// a higher one, if it set the field, wins.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Origin {
    /// The built-in default (the lowest layer).
    Default,
    /// A `webp.toml`, with the 1-based line the value sits on when it is known.
    ConfigFile {
        /// The file the value was read from.
        path: PathBuf,
        /// The 1-based source line, if it could be located.
        line: Option<u32>,
    },
    /// A `WEBP_*` environment variable, named.
    Env(&'static str),
    /// A command-line argument.
    Args,
}

impl Origin {
    /// A one-line, human-readable rendering for `webp config`.
    fn render(&self) -> String {
        match self {
            Self::Default => "default".to_owned(),
            Self::ConfigFile { path, line } => line.as_ref().map_or_else(
                || path.display().to_string(),
                |line| format!("{}:{line}", path.display()),
            ),
            Self::Env(name) => format!("env {name}"),
            Self::Args => "argument".to_owned(),
        }
    }

    /// A machine-readable JSON object naming the source.
    fn to_json(&self) -> Json {
        let mut map = Map::new();
        let mut set = |key: &str, value: Json| {
            map.insert(key.to_owned(), value);
        };
        match self {
            Self::Default => set("source", Json::from("default")),
            Self::Args => set("source", Json::from("args")),
            Self::Env(name) => {
                set("source", Json::from("env"));
                set("name", Json::from(*name));
            },
            Self::ConfigFile { path, line } => {
                set("source", Json::from("file"));
                set("path", Json::from(path.display().to_string()));
                if let Some(line) = line {
                    set("line", Json::from(*line));
                }
            },
        }
        Json::Object(map)
    }
}

/// A value paired with the [`Origin`] it was resolved from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Sourced<T> {
    /// The resolved value.
    pub(crate) value: T,
    /// Where it came from.
    pub(crate) origin: Origin,
}

impl<T> Sourced<T> {
    /// Pair a value with its origin.
    pub(crate) const fn new(value: T, origin: Origin) -> Self {
        Self { value, origin }
    }
}

/// The `{ "value": ..., "origin": ... }` object one setting serializes to.
fn setting_json<T: ConfigValue>(sourced: &Sourced<T>) -> Json {
    let mut map = Map::new();
    map.insert("value".to_owned(), sourced.value.json());
    map.insert("origin".to_owned(), sourced.origin.to_json());
    Json::Object(map)
}

/// The 1-based line a top-level `key = ...` assignment sits on.
///
/// The config is a flat table, so a leading `key =` locates the value uniquely; a
/// key that appears only inside a value (a string, say) has other characters
/// before the `=`, so it does not match.
fn line_of(text: &str, key: &str) -> Option<u32> {
    text.lines().enumerate().find_map(|(index, line)| {
        let rest = line.trim_start().strip_prefix(key)?.trim_start();
        rest.starts_with('=')
            .then(|| u32::try_from(index + 1).ok())
            .flatten()
    })
}

/// Declare the settings once; derive every representation from this table.
///
/// Each row is `field: Type = default, env "WEBP_*", key "toml-key", desc "..."`.
/// The macro generates the [`Partial`] and [`Settings`] structs, the env and TOML
/// parsers, the `webp config` rows, `config get`, the JSON object, and the
/// template — so adding a setting here adds it everywhere, and forgetting one is a
/// compile error, not a silent gap.
macro_rules! settings {
    ($(
        $(#[$meta:meta])*
        $field:ident : $ty:ty = $default:expr, env $env:literal, key $key:literal, desc $desc:literal;
    )+) => {
        /// One layer's contribution. Each present field carries its own origin, so
        /// a config file can tag its values with different line numbers.
        #[derive(Debug, Default, Clone)]
        pub(crate) struct Partial {
            $( $(#[$meta])* pub(crate) $field: Option<Sourced<$ty>>, )+
        }

        /// Every setting resolved to one value with provenance.
        #[derive(Debug, Clone)]
        pub(crate) struct Settings {
            $( $(#[$meta])* pub(crate) $field: Sourced<$ty>, )+
        }

        impl Settings {
            /// Fold `layers`, highest-priority-first: for each field keep the first
            /// layer that set it, else the built-in default. Pure — no I/O.
            pub(crate) fn resolve(layers: &[Partial]) -> Self {
                Self {
                    $( $field: layers
                        .iter()
                        .find_map(|layer| layer.$field.clone())
                        .unwrap_or_else(|| Sourced::new($default, Origin::Default)), )+
                }
            }

            /// The `(key, rendered value, origin)` rows `webp config` prints.
            pub(crate) fn rows(&self) -> Vec<(&'static str, String, &Origin)> {
                vec![ $( ($key, self.$field.value.render(), &self.$field.origin), )+ ]
            }

            /// One setting's rendered value by key, for `config get <key>`.
            pub(crate) fn get(&self, key: &str) -> Option<String> {
                match key {
                    $( $key => Some(self.$field.value.render()), )+
                    _ => None,
                }
            }

            /// A JSON object; keys are `BTreeMap`-ordered, so the order is stable.
            pub(crate) fn to_json(&self) -> Json {
                let mut map = Map::new();
                $( map.insert($key.to_owned(), setting_json(&self.$field)); )+
                map.insert("schema".to_owned(), Json::from("webpkit.config/v1"));
                Json::Object(map)
            }
        }

        impl Partial {
            /// The env layer: every `WEBP_*` variable that is set, parsed and
            /// tagged [`Origin::Env`].
            ///
            /// # Errors
            ///
            /// [`CliError::Usage`] naming the variable whose value does not parse.
            pub(crate) fn from_env() -> Result<Self, CliError> {
                let mut partial = Self::default();
                $(
                    if let Some(raw) = std::env::var_os($env) {
                        let text = raw.to_string_lossy();
                        let value = <$ty as ConfigValue>::parse(&text).map_err(|why| {
                            CliError::Usage(format!(concat!($env, "={}: {}"), text, why))
                        })?;
                        partial.$field = Some(Sourced::new(value, Origin::Env($env)));
                    }
                )+
                Ok(partial)
            }

            /// A file layer: `text` from `path`, each known key tagged with its
            /// source line.
            ///
            /// # Errors
            ///
            /// [`CliError::Format`] if the TOML is malformed, a value does not
            /// parse, or a key is not a known setting.
            pub(crate) fn from_toml(text: &str, path: &std::path::Path) -> Result<Self, CliError> {
                let table: toml::Table = text
                    .parse()
                    .map_err(|err| CliError::Format(format!("{}: {err}", path.display())))?;
                let mut partial = Self::default();
                for (name, item) in &table {
                    match name.as_str() {
                        $(
                            $key => {
                                let value = <$ty as ConfigValue>::from_toml(item).map_err(|why| {
                                    CliError::Format(format!("{}: `{name}`: {why}", path.display()))
                                })?;
                                let origin = Origin::ConfigFile {
                                    path: path.to_path_buf(),
                                    line: line_of(text, name),
                                };
                                partial.$field = Some(Sourced::new(value, origin));
                            },
                        )+
                        other => {
                            return Err(CliError::Format(format!(
                                "{}: unknown setting `{other}` (known: {})",
                                path.display(),
                                KEYS.join(", "),
                            )));
                        },
                    }
                }
                Ok(partial)
            }
        }

        /// Every setting's TOML key, for `config get` errors and the template.
        pub(crate) const KEYS: &[&str] = &[ $( $key, )+ ];

        /// A commented `webp.toml` with each setting shown at its default value.
        pub(crate) fn template() -> String {
            let mut out = String::from(
                "# webpkit configuration (webp.toml).\n\
                 #\n\
                 # Uncomment a line to set it. Precedence, highest first:\n\
                 #   command-line args > WEBP_* env vars > this file > built-in defaults.\n\
                 # A project file (found by walking up from the working directory) wins\n\
                 # over the one in your user config directory.\n\n",
            );
            $(
                out.push_str(concat!("# ", $desc, "\n"));
                out.push_str(&format!(
                    "# {} = {}\n\n",
                    $key,
                    ConfigValue::toml_literal(&{ $default }),
                ));
            )+
            out
        }
    };
}

settings! {
    /// Lossy quality, 0-100 (higher is larger and closer to the source).
    quality: Quality = Quality::DEFAULT, env "WEBP_QUALITY", key "quality",
        desc "quality: lossy quality 0-100 (higher is larger, closer to the source).";
    /// Encoder effort: `auto` (the default) or a fixed level `0..=9`.
    effort: Effort = Effort::AUTO, env "WEBP_EFFORT", key "effort",
        desc "effort: encoder effort — auto, or a fixed level 0-9.";
    /// Codec: lossless (VP8L) or lossy (VP8).
    codec: Codec = Codec::Lossless, env "WEBP_CODEC", key "codec",
        desc "codec: lossless or lossy.";
    /// Which metadata to carry: all, none, or a list of icc, exif, xmp.
    metadata: Selection = Selection::all(), env "WEBP_METADATA", key "metadata",
        desc "metadata: all, none, or a comma list of icc, exif, xmp.";
    /// When to colorize: auto, always, or never.
    color: ColorChoice = ColorChoice::Auto, env "WEBP_COLOR", key "color",
        desc "color: auto, always, or never.";
    /// Worker threads for bulk work; 0 means one per core.
    threads: u16 = 0, env "WEBP_THREADS", key "threads",
        desc "threads: worker threads for bulk work; 0 = one per core.";
    /// The decode pixel-count safety cap, or none to remove it.
    max_pixels: MaxPixels = MaxPixels::Limited(DEFAULT_MAX_PIXELS), env "WEBP_MAX_PIXELS",
        key "max_pixels",
        desc "max_pixels: decode pixel-count cap (e.g. 100000000, 300M, 2G), or none.";
}

/// The config files that were found, in precedence order, plus the settings they
/// resolve to. Bundled so `webp config` can print both.
pub(crate) struct Resolution {
    /// The resolved settings.
    pub(crate) settings: Settings,
    /// The config files found, highest priority first.
    pub(crate) files: Vec<PathBuf>,
}

/// Resolve the args layer against the env, file, and default layers.
///
/// The args layer is passed in (built from the parsed command line) rather than
/// read here, so this stays close to pure: only [`file_layers`] touches the disk.
///
/// # Errors
///
/// Propagates a malformed env value ([`CliError::Usage`]) or config file
/// ([`CliError::Format`]).
pub(crate) fn resolve(args: Partial) -> Result<Resolution, CliError> {
    let mut layers = vec![args, Partial::from_env()?];
    let found = file_layers()?;
    let files = found.iter().map(|(path, _)| path.clone()).collect();
    layers.extend(found.into_iter().map(|(_, partial)| partial));
    Ok(Resolution {
        settings: Settings::resolve(&layers),
        files,
    })
}

/// The human-readable `webp config` report: one `key = value  (origin)` per
/// setting, then the config files that were consulted.
pub(crate) fn render_report(resolution: &Resolution) -> Vec<String> {
    let rows = resolution.settings.rows();
    let width = rows.iter().map(|(key, ..)| key.len()).max().unwrap_or(0);
    let value_width = rows
        .iter()
        .map(|(_, value, _)| value.len())
        .max()
        .unwrap_or(0);
    let mut lines: Vec<String> = rows
        .iter()
        .map(|(key, value, origin)| {
            format!(
                "{key:<width$}  {value:<value_width$}  ({})",
                origin.render()
            )
        })
        .collect();
    lines.push(String::new());
    if resolution.files.is_empty() {
        lines.push("config files: none found".to_owned());
    } else {
        lines.push("config files (highest priority first):".to_owned());
        for path in &resolution.files {
            lines.push(format!("  {}", path.display()));
        }
    }
    lines
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::{Codec, MaxPixels, Origin, Partial, Quality, Settings, Sourced, line_of};
    use crate::{metadata::Selection, term::ColorChoice};

    fn file_origin(line: u32) -> Origin {
        Origin::ConfigFile {
            path: PathBuf::from("webp.toml"),
            line: Some(line),
        }
    }

    /// A partial that sets only `quality`, with the given origin.
    fn quality_layer(value: u8, origin: Origin) -> Partial {
        Partial {
            quality: Some(Sourced::new(Quality(value), origin)),
            ..Partial::default()
        }
    }

    #[test]
    fn an_empty_stack_resolves_everything_to_defaults() {
        let settings = Settings::resolve(&[]);
        assert_eq!(settings.quality.value, Quality::DEFAULT);
        assert_eq!(settings.quality.origin, Origin::Default);
        assert_eq!(settings.codec.value, Codec::Lossless);
        assert_eq!(settings.metadata.value, Selection::all());
        assert_eq!(settings.color.value, ColorChoice::Auto);
        assert_eq!(settings.threads.value, 0);
    }

    /// The whole precedence ladder on one field, so a mis-ordered fold is caught:
    /// args beats env beats file beats default, and the winner's origin is recorded.
    #[test]
    fn args_beat_env_beat_file_beat_default() {
        let file = quality_layer(70, file_origin(3));
        let env = quality_layer(80, Origin::Env("WEBP_QUALITY"));
        let args = quality_layer(90, Origin::Args);

        // Default only.
        assert_eq!(Settings::resolve(&[]).quality.origin, Origin::Default);
        // File over default.
        let r = Settings::resolve(&[Partial::default(), Partial::default(), file.clone()]);
        assert_eq!(r.quality.value, Quality(70));
        assert_eq!(r.quality.origin, file_origin(3));
        // Env over file.
        let r = Settings::resolve(&[Partial::default(), env.clone(), file.clone()]);
        assert_eq!(r.quality.value, Quality(80));
        assert_eq!(r.quality.origin, Origin::Env("WEBP_QUALITY"));
        // Args over everything.
        let r = Settings::resolve(&[args, env, file]);
        assert_eq!(r.quality.value, Quality(90));
        assert_eq!(r.quality.origin, Origin::Args);
    }

    /// The fold is field-wise: a higher layer that leaves a field unset does not
    /// shadow a lower layer that set it.
    #[test]
    fn a_higher_layer_only_wins_the_fields_it_sets() {
        let env = Partial {
            codec: Some(Sourced::new(Codec::Lossy, Origin::Env("WEBP_CODEC"))),
            ..Partial::default()
        };
        let file = quality_layer(60, file_origin(1));

        let settings = Settings::resolve(&[env, file]);
        // env set codec; file set quality; each keeps its own origin.
        assert_eq!(settings.codec.value, Codec::Lossy);
        assert_eq!(settings.codec.origin, Origin::Env("WEBP_CODEC"));
        assert_eq!(settings.quality.value, Quality(60));
        assert_eq!(settings.quality.origin, file_origin(1));
        // effort was set by no layer, so it defaults.
        assert_eq!(settings.effort.origin, Origin::Default);
    }

    #[test]
    fn from_toml_records_a_line_number_per_key() {
        let text = "quality = 40\n\ncodec = \"lossy\"\n";
        let partial = Partial::from_toml(text, Path::new("webp.toml")).expect("valid toml");
        assert_eq!(partial.quality.expect("quality set").origin, file_origin(1));
        assert_eq!(partial.codec.expect("codec set").origin, file_origin(3));
    }

    #[test]
    fn from_toml_rejects_an_unknown_key() {
        let err = Partial::from_toml("nonsense = 1\n", Path::new("webp.toml"));
        assert!(err.is_err(), "an unknown key must be rejected, not ignored");
    }

    #[test]
    fn from_toml_rejects_an_out_of_range_value() {
        assert!(Partial::from_toml("quality = 200\n", Path::new("webp.toml")).is_err());
    }

    #[test]
    fn line_of_finds_a_flat_key_and_ignores_a_lookalike() {
        let text = "# comment\nquality = 5\nmax_pixels = 10\n";
        assert_eq!(line_of(text, "quality"), Some(2));
        assert_eq!(line_of(text, "max_pixels"), Some(3));
        // A key that never appears as an assignment is absent.
        assert_eq!(line_of(text, "color"), None);
    }

    #[test]
    fn get_returns_a_bare_value_for_a_known_key_and_none_otherwise() {
        let settings = Settings::resolve(&[quality_layer(88, Origin::Args)]);
        assert_eq!(settings.get("quality").as_deref(), Some("88"));
        assert_eq!(settings.get("codec").as_deref(), Some("lossless"));
        assert!(settings.get("nonexistent").is_none());
    }

    /// The JSON carries the value and a machine-readable origin, with the file
    /// line when there is one.
    #[test]
    fn json_reports_value_and_origin() {
        let settings = Settings::resolve(&[quality_layer(64, file_origin(7))]);
        let json = settings.to_json();
        let quality = &json["quality"];
        assert_eq!(quality["value"], serde_json::json!(64));
        assert_eq!(quality["origin"]["source"], serde_json::json!("file"));
        assert_eq!(quality["origin"]["line"], serde_json::json!(7));
        assert_eq!(json["schema"], serde_json::json!("webpkit.config/v1"));
    }

    #[test]
    fn max_pixels_parses_suffixes_and_none() {
        assert_eq!(
            MaxPixels::parse("300M").expect("valid"),
            MaxPixels::Limited(300_000_000)
        );
        assert_eq!(
            MaxPixels::parse("2G").expect("valid"),
            MaxPixels::Limited(2_000_000_000)
        );
        assert_eq!(
            MaxPixels::parse("none").expect("valid"),
            MaxPixels::Unbounded
        );
        assert!(MaxPixels::parse("not-a-number").is_err());
    }

    /// The template shows every key, so it cannot silently omit a setting.
    #[test]
    fn the_template_mentions_every_key() {
        let template = super::template();
        for key in super::KEYS {
            assert!(
                template.contains(&format!("# {key} =")),
                "template is missing `{key}`",
            );
        }
    }
}
