//! Select and run conservative ordinary quality work for this checkout.

use std::env;
use std::io::{self, Write};
use std::path::Path;
use std::process::{Command, Output};
use webpkit_ci_core::{AffectedWork, CodeqlScope, QualityWork, Revision, affected_work};

const DEFAULT_BASE: &str = "origin/main";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Operation {
    Check,
    CodeqlMatrix,
}

struct Arguments {
    operation: Operation,
    base: Option<String>,
    head: Option<String>,
}

struct Selection {
    work: AffectedWork,
    reason: &'static str,
}

fn main() -> Result<(), String> {
    run()
}

fn run() -> Result<(), String> {
    let arguments = arguments()?;
    let root = env::current_dir().map_err(|_| "current directory is unavailable".to_owned())?;
    let selection = select(&root, &arguments)?;
    report_selection(&selection)?;
    match arguments.operation {
        Operation::Check => run_quality_gate(&root, selection.work.quality),
        Operation::CodeqlMatrix => write_matrix(selection.work.codeql),
    }
}

fn arguments() -> Result<Arguments, String> {
    let mut values = env::args_os().skip(1);
    let operation = match values
        .next()
        .and_then(|value| value.into_string().ok())
        .as_deref()
    {
        Some("check") => Operation::Check,
        Some("codeql-matrix") => Operation::CodeqlMatrix,
        _ => return Err("expected `check` or `codeql-matrix`".to_owned()),
    };
    let mut base = None;
    let mut head = None;
    while let Some(option) = values.next() {
        let option = option
            .into_string()
            .map_err(|_| "selector option is not valid Unicode".to_owned())?;
        let value = values
            .next()
            .ok_or_else(|| format!("{option} requires a value"))?
            .into_string()
            .map_err(|_| format!("{option} value is not valid Unicode"))?;
        match option.as_str() {
            "--base" if base.is_none() => base = Some(value),
            "--head" if head.is_none() => head = Some(value),
            "--base" | "--head" => return Err(format!("{option} was supplied more than once")),
            _ => return Err(format!("unsupported option `{option}`")),
        }
    }
    Ok(Arguments {
        operation,
        base: configured(base, "CI_BASE_REVISION")?,
        head: configured(head, "CI_HEAD_REVISION")?,
    })
}

fn configured(explicit: Option<String>, name: &str) -> Result<Option<String>, String> {
    if explicit.is_some() {
        return Ok(explicit);
    }
    env::var(name).map(Some).or_else(|error| match error {
        env::VarError::NotPresent => Ok(None),
        env::VarError::NotUnicode(_) => Err(format!("{name} is not valid Unicode")),
    })
}

fn select(root: &Path, arguments: &Arguments) -> Result<Selection, String> {
    let expected_head = arguments
        .head
        .as_deref()
        .map(|head| {
            Revision::parse(head.as_bytes()).map_err(|_| {
                "configured head must be a full 40-character hexadecimal SHA".to_owned()
            })
        })
        .transpose()?;
    let Ok(actual_head) = resolve_commit(root, "HEAD") else {
        return Ok(full("checkout identity unavailable"));
    };
    if expected_head.is_some_and(|expected| expected != actual_head) {
        return Err("configured head does not match the checked-out commit".to_owned());
    }

    let base = arguments.base.as_deref().unwrap_or(DEFAULT_BASE);
    if base.is_empty() {
        return Ok(full("base revision is empty"));
    }
    let Ok(base) = resolve_commit(root, base) else {
        return Ok(full("base revision could not be resolved"));
    };

    match git(
        root,
        [
            "--no-optional-locks",
            "status",
            "--porcelain=v1",
            "-z",
            "--untracked-files=all",
        ],
    ) {
        Ok(status) if status.status.success() && status.stdout.is_empty() => {},
        Ok(status) if status.status.success() => return Ok(full("checkout has uncommitted paths")),
        _ => return Ok(full("checkout status could not be inspected")),
    }

    let base_text = String::from_utf8_lossy(base.bytes()).into_owned();
    let head_text = String::from_utf8_lossy(actual_head.bytes()).into_owned();
    let diff = match git(
        root,
        [
            "--no-optional-locks",
            "diff",
            "--no-ext-diff",
            "--no-textconv",
            "--no-renames",
            "--name-only",
            "-z",
            base_text.as_str(),
            head_text.as_str(),
            "--",
        ],
    ) {
        Ok(diff) if diff.status.success() => diff,
        _ => return Ok(full("path comparison failed")),
    };
    let Some(paths) = changed_paths(&diff.stdout) else {
        return Ok(full("path comparison was empty or malformed"));
    };
    let references = paths.iter().map(String::as_str).collect::<Vec<_>>();
    Ok(Selection {
        work: affected_work(&references),
        reason: "complete path comparison",
    })
}

fn resolve_commit(root: &Path, reference: &str) -> Result<Revision, ()> {
    let expression = format!("{reference}^{{commit}}");
    let output = git(
        root,
        [
            "--no-optional-locks",
            "rev-parse",
            "--verify",
            "--end-of-options",
            expression.as_str(),
        ],
    )
    .map_err(|_| ())?;
    if !output.status.success() {
        return Err(());
    }
    let text = std::str::from_utf8(&output.stdout).map_err(|_| ())?.trim();
    Revision::parse(text.as_bytes()).map_err(|_| ())
}

fn git<const N: usize>(root: &Path, arguments: [&str; N]) -> io::Result<Output> {
    Command::new("git")
        .current_dir(root)
        .args(arguments)
        .output()
}

fn changed_paths(output: &[u8]) -> Option<Vec<String>> {
    if output.is_empty() || output.last() != Some(&0) {
        return None;
    }
    let mut paths = Vec::new();
    for path in output[..output.len() - 1].split(|byte| *byte == 0) {
        if path.is_empty() {
            return None;
        }
        paths.push(std::str::from_utf8(path).ok()?.to_owned());
    }
    Some(paths)
}

const fn full(reason: &'static str) -> Selection {
    Selection {
        work: AffectedWork {
            quality: QualityWork::Full,
            codeql: CodeqlScope::Both,
        },
        reason,
    }
}

fn report_selection(selection: &Selection) -> Result<(), String> {
    let quality = match selection.work.quality {
        QualityWork::Workflows => "workflow-only",
        QualityWork::Full => "full",
    };
    let codeql = match selection.work.codeql {
        CodeqlScope::Rust => "rust",
        CodeqlScope::Actions => "actions",
        CodeqlScope::Both => "both",
    };
    writeln!(
        io::stderr().lock(),
        "ci-select: quality={quality}; codeql={codeql}; reason={}",
        selection.reason
    )
    .map_err(|_| "could not report the selected work".to_owned())
}

fn run_quality_gate(root: &Path, quality: QualityWork) -> Result<(), String> {
    let recipe = match quality {
        QualityWork::Workflows => "check-workflows",
        QualityWork::Full => "check",
    };
    let status = Command::new("just")
        .current_dir(root)
        .arg(recipe)
        .status()
        .map_err(|_| format!("could not start `just {recipe}`"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("`just {recipe}` failed with {status}"))
    }
}

fn write_matrix(scope: CodeqlScope) -> Result<(), String> {
    let json = match scope {
        CodeqlScope::Rust => "{\"include\":[{\"language\":\"rust\",\"build-mode\":\"none\"}]}\n",
        CodeqlScope::Actions => {
            "{\"include\":[{\"language\":\"actions\",\"build-mode\":\"none\"}]}\n"
        },
        CodeqlScope::Both => {
            "{\"include\":[{\"language\":\"rust\",\"build-mode\":\"none\"},{\"language\":\"actions\",\"build-mode\":\"none\"}]}\n"
        },
    };
    io::stdout()
        .lock()
        .write_all(json.as_bytes())
        .map_err(|_| "could not write the CodeQL matrix".to_owned())
}

#[cfg(test)]
mod tests {
    use super::changed_paths;

    #[test]
    fn path_transport_rejects_empty_incomplete_and_undecodable_output() {
        for output in [
            &b""[..],
            &b".github/workflows/release.yml"[..],
            &b".github/workflows/release.yml\0\0"[..],
            &b"\xff\0"[..],
        ] {
            assert!(changed_paths(output).is_none());
        }
    }

    #[test]
    fn path_transport_preserves_every_nul_separated_path() {
        assert_eq!(
            changed_paths(b".github/workflows/release.yml\0notes/file with spaces.md\0"),
            Some(vec![
                ".github/workflows/release.yml".to_owned(),
                "notes/file with spaces.md".to_owned(),
            ])
        );
    }
}
