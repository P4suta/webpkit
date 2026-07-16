//! Finding and reading the `webp.toml` layers.
//!
//! [`discover`] is pure — it turns a working directory and a config home into the
//! ordered list of candidate paths — so the search order is unit-testable without
//! a filesystem. Only [`file_layers`] touches the disk, and it does so through
//! [`crate::io`], which owns every read.

use std::path::{Path, PathBuf};

use super::Partial;
use crate::error::CliError;

/// The config filename, looked for at every level of the walk-up and in the user
/// config directory.
const CONFIG_FILE: &str = "webp.toml";

/// Candidate config paths, highest priority first: `webp.toml` from `cwd` up to
/// the filesystem root, then `<config_home>/webpkit/webp.toml`.
///
/// Nearer the working directory wins, and a project file always outranks the user
/// one. The paths are candidates; whether each exists is a question for the disk.
pub(crate) fn discover(cwd: Option<&Path>, config_home: Option<&Path>) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if let Some(dir) = cwd {
        paths.extend(dir.ancestors().map(|ancestor| ancestor.join(CONFIG_FILE)));
    }
    if let Some(home) = config_home {
        paths.push(home.join("webpkit").join(CONFIG_FILE));
    }
    paths
}

/// The config files that exist, each parsed into a [`Partial`], highest priority
/// first.
///
/// A missing (or unreadable) candidate is skipped — configuration is optional. A
/// file that *is* read but does not parse is an error: a typo in a real config
/// should be reported, not silently ignored.
///
/// # Errors
///
/// [`CliError::Format`] if a present config file is malformed or names an unknown
/// setting.
pub(crate) fn file_layers() -> Result<Vec<(PathBuf, Partial)>, CliError> {
    let cwd = crate::io::current_dir();
    let config_home = crate::io::config_home();
    let mut layers = Vec::new();
    for path in discover(cwd.as_deref(), config_home.as_deref()) {
        if let Some(text) = crate::io::read_optional_text(&path) {
            let partial = Partial::from_toml(&text, &path)?;
            layers.push((path, partial));
        }
    }
    Ok(layers)
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::discover;

    #[test]
    fn discovery_walks_up_then_falls_back_to_the_config_home() {
        let cwd = Path::new("/a/b/c");
        let home = Path::new("/home/user/.config");
        let paths = discover(Some(cwd), Some(home));
        assert_eq!(
            paths,
            vec![
                PathBuf::from("/a/b/c/webp.toml"),
                PathBuf::from("/a/b/webp.toml"),
                PathBuf::from("/a/webp.toml"),
                PathBuf::from("/webp.toml"),
                PathBuf::from("/home/user/.config/webpkit/webp.toml"),
            ]
        );
    }

    #[test]
    fn discovery_tolerates_a_missing_cwd_or_config_home() {
        assert!(discover(None, None).is_empty());
        assert_eq!(
            discover(None, Some(Path::new("/cfg"))),
            vec![PathBuf::from("/cfg/webpkit/webp.toml")]
        );
    }
}
