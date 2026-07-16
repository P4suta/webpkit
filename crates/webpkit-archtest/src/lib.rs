//! Source-scanning primitives shared by the workspace's architecture gates.
//!
//! Both `webpkit` and `webpkit-cli` enforce module rules the compiler cannot
//! state, and both do it by reading their own `src/`. They used to carry
//! byte-identical copies of the scaffolding, and the copies drifted: the CLI's
//! gate learned to flatten `use` trees while the library's — the more
//! load-bearing of the two — kept matching by substring, and so could not see a
//! layering violation written the way rustfmt writes it. One home, so a fix
//! cannot land on one gate and miss the other.
//!
//! These are heuristics, deliberately. Each line is truncated at its first `//`
//! so comments and intra-doc links never count as dependencies, and only real
//! path shapes are read. They catch the accident, not every obfuscation.
#![forbid(unsafe_code)]
#![expect(
    clippy::panic,
    reason = "test scaffolding: a gate that cannot read its own source must fail               loudly, never pass vacuously"
)]

use std::path::{Path, PathBuf};

/// Every `.rs` file under `dir`, recursively.
///
/// # Errors
///
/// Any I/O error from walking `dir`.
pub fn collect_rs(dir: &Path, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_dir() {
            collect_rs(&path, out)?;
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
    Ok(())
}

/// Drop each line's first `//`-comment, so comments and intra-doc links are not
/// mistaken for code.
#[must_use]
pub fn strip_comments(src: &str) -> String {
    src.lines()
        .map(|line| line.split_once("//").map_or(line, |(code, _)| code))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Every `.rs` file under `<manifest_dir>/src`, as (path relative to `src/`,
/// comment-stripped source).
///
/// Pass `env!("CARGO_MANIFEST_DIR")` from the calling test — expanding it here
/// would name this crate instead of the one under test.
///
/// # Panics
///
/// If `<manifest_dir>/src` cannot be read. A gate that cannot find its own
/// source must fail loudly rather than pass vacuously.
#[must_use]
pub fn all_src(manifest_dir: &str) -> Vec<(PathBuf, String)> {
    let src = Path::new(manifest_dir).join("src");
    let mut files = Vec::new();
    collect_rs(&src, &mut files).unwrap_or_else(|err| panic!("read {}: {err}", src.display()));
    assert!(!files.is_empty(), "no sources under {}", src.display());
    files
        .into_iter()
        .map(|path| {
            let rel = path.strip_prefix(&src).unwrap_or(&path).to_path_buf();
            let code = std::fs::read_to_string(&path)
                .unwrap_or_else(|err| panic!("read {}: {err}", path.display()));
            (rel, strip_comments(&code))
        })
        .collect()
}

/// A `src`-relative path as forward-slashed text, so a rule reads the same on
/// Windows as on Unix.
#[must_use]
pub fn slashed(rel: &Path) -> String {
    rel.to_string_lossy().replace('\\', "/")
}

/// Split `body` on commas that are not inside a nested `{...}`.
fn split_top_level(body: &str) -> Vec<&str> {
    let (mut parts, mut depth, mut start) = (Vec::new(), 0_i32, 0);
    for (index, ch) in body.char_indices() {
        match ch {
            '{' => depth += 1,
            '}' => depth -= 1,
            ',' if depth == 0 => {
                parts.push(&body[start..index]);
                start = index + 1;
            },
            _ => {},
        }
    }
    parts.push(&body[start..]);
    parts.into_iter().filter(|p| !p.is_empty()).collect()
}

/// Expand one `use` tree body into full paths under `prefix`.
fn expand_tree(prefix: &str, body: &str, out: &mut Vec<String>) {
    for item in split_top_level(body) {
        match (item.find('{'), item.rfind('}')) {
            (Some(open), Some(close)) if open < close => {
                let head = format!("{prefix}{}", &item[..open]);
                expand_tree(&head, &item[open + 1..close], out);
            },
            _ => out.push(format!("{prefix}{item}")),
        }
    }
}

/// Every path `code` imports from `root`, with nested `use` trees flattened:
/// for `root = "webpkit"`, `use webpkit::{Image, container::reader::chunks};`
/// yields `Image` and `container::reader::chunks`.
///
/// Flattening is the whole point. The flat spelling `use root::a::B;` is a
/// substring match, but rustfmt writes the grouped form — so a check that only
/// sees the flat one passes exactly the code it exists to catch. Matching the
/// grouped form by substring instead over-fires: in
/// `use webpkit::{container::{anim::X}}`, `anim::` is not `webpkit::anim::`.
/// Only real paths answer both.
#[must_use]
pub fn use_paths(code: &str, root: &str) -> Vec<String> {
    let dense: String = code.chars().filter(|c| !c.is_whitespace()).collect();
    let needle = format!("use{root}::");
    let mut paths = Vec::new();
    let mut rest = dense.as_str();
    while let Some(start) = rest.find(&needle) {
        rest = &rest[start + needle.len()..];
        let end = rest.find(';').unwrap_or(rest.len());
        let tree = &rest[..end];
        match (tree.find('{'), tree.rfind('}')) {
            (Some(open), Some(close)) if open < close => {
                expand_tree(&tree[..open], &tree[open + 1..close], &mut paths);
            },
            _ => paths.push(tree.to_owned()),
        }
        rest = &rest[end..];
    }
    paths
}

/// Whether `code` names `root::module` — as an import (however nested) or as an
/// inline path.
#[must_use]
pub fn names_module(code: &str, root: &str, module: &str) -> bool {
    code.contains(&format!("{root}::{module}::"))
        || use_paths(code, root)
            .iter()
            .any(|path| path.split("::").next() == Some(module))
}

/// The first identifier after each `root::module::` occurrence, keeping only
/// those `known` recognizes.
///
/// Reads both spellings: an inline `crate::lossy::idct::foo()` and a grouped
/// `use crate::lossy::{idct, yuv};`. The grouped form is the one a substring
/// scan misses — it stops at the `{` and records nothing at all.
#[must_use]
pub fn module_edges(
    code: &str,
    root: &str,
    module: &str,
    known: &dyn Fn(&str) -> bool,
) -> Vec<String> {
    let mut found: Vec<String> = Vec::new();
    let mut push = |ident: &str| {
        if known(ident) && !found.iter().any(|f| f == ident) {
            found.push(ident.to_owned());
        }
    };

    // Inline paths: `crate::lossy::idct::transform(..)`.
    let needle = format!("{root}::{module}::");
    let mut rest = code;
    while let Some(pos) = rest.find(&needle) {
        rest = &rest[pos + needle.len()..];
        let ident: String = rest
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
            .collect();
        push(&ident);
    }

    // Imports, including `use crate::{lossy::{idct, yuv}};`.
    let prefix = format!("{module}::");
    for path in use_paths(code, root) {
        if let Some(tail) = path.strip_prefix(&prefix) {
            push(tail.split("::").next().unwrap_or(tail));
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::{module_edges, names_module, use_paths};

    fn known(idents: &'static [&'static str]) -> impl Fn(&str) -> bool {
        move |i: &str| idents.contains(&i)
    }

    #[test]
    fn a_flat_import_is_read() {
        assert_eq!(
            use_paths("use webpkit::lossless::Image;", "webpkit"),
            ["lossless::Image"]
        );
    }

    #[test]
    fn a_grouped_import_is_flattened() {
        let code = "use webpkit::{\n    Metadata,\n    lossless::Effort,\n};";
        assert_eq!(use_paths(code, "webpkit"), ["Metadata", "lossless::Effort"]);
    }

    #[test]
    fn nesting_is_flattened_to_full_paths() {
        let code = "use webpkit::{container::{anim::X, reader::{chunks, locate}}, Image};";
        assert_eq!(
            use_paths(code, "webpkit"),
            [
                "container::anim::X",
                "container::reader::chunks",
                "container::reader::locate",
                "Image",
            ]
        );
    }

    #[test]
    fn a_module_is_named_by_import_or_inline_path() {
        assert!(names_module(
            "use webpkit::{lossless::Effort};",
            "webpkit",
            "lossless"
        ));
        assert!(names_module(
            "webpkit::stream::DecodeOptions::new()",
            "webpkit",
            "stream"
        ));
    }

    /// `container::anim` is not `webpkit::anim`: a nested segment must not be
    /// read as a top-level one.
    #[test]
    fn a_nested_segment_is_not_a_top_level_module() {
        let code = "use webpkit::{container::{anim::ANMF_HEADER_LEN}};";
        assert!(names_module(code, "webpkit", "container"));
        assert!(!names_module(code, "webpkit", "anim"));
    }

    /// `webpkit::Error` is the facade type; `webpkit::error` is the module.
    #[test]
    fn a_type_is_not_its_module() {
        assert!(!names_module(
            "webpkit::Error::Truncated",
            "webpkit",
            "error"
        ));
    }

    /// The regression that made the library's gate blind: a grouped import
    /// records no edge at all under a substring scan, because the scan reads the
    /// `{` and stops.
    #[test]
    fn grouped_imports_yield_edges() {
        let code = "use crate::lossy::{idct, yuv};";
        let mut edges = module_edges(code, "crate", "lossy", &known(&["idct", "yuv", "decoder"]));
        edges.sort();
        assert_eq!(edges, ["idct", "yuv"]);
    }

    #[test]
    fn inline_paths_yield_edges() {
        let code = "let x = crate::lossy::idct::transform(a); crate::lossy::yuv::convert(b);";
        let mut edges = module_edges(code, "crate", "lossy", &known(&["idct", "yuv"]));
        edges.sort();
        assert_eq!(edges, ["idct", "yuv"]);
    }

    #[test]
    fn unknown_idents_are_not_edges() {
        let code = "use crate::lossy::{LossyConfig, Quality};";
        assert!(module_edges(code, "crate", "lossy", &known(&["idct"])).is_empty());
    }

    #[test]
    fn an_edge_is_reported_once_however_often_it_appears() {
        let code = "use crate::lossy::{idct}; crate::lossy::idct::a(); crate::lossy::idct::b();";
        assert_eq!(
            module_edges(code, "crate", "lossy", &known(&["idct"])),
            ["idct"]
        );
    }
}
