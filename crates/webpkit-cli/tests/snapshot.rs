//! Snapshot tests pinning the help text of each binary.
//!
//! One snapshot per binary, not per subcommand: [`dump`] walks the command tree
//! by reading each level's own `Commands:` section, so a subcommand added later
//! shows up in the snapshot diff automatically rather than being silently
//! untested. Three files, however many subcommands there are.
#![expect(
    clippy::expect_used,
    reason = "integration-test helpers live outside #[test] fns; failures are bugs"
)]

use std::fmt::Write as _;

use assert_cmd::Command;

/// Help text for `bin <path...> <flag>`, with the platform `.exe` suffix stripped
/// so the snapshot is identical on Windows and Unix.
fn help(bin: &str, path: &[String], flag: &str) -> String {
    let output = Command::cargo_bin(bin)
        .expect("binary builds")
        .args(path)
        .arg(flag)
        .output()
        .expect("run help");
    String::from_utf8_lossy(&output.stdout).replace(".exe", "")
}

/// The subcommand names listed in a clap `Commands:` section.
///
/// clap renders one indented `  <name>  <about>` line per command, terminated by
/// a blank line. `help` is clap's own and carries nothing worth pinning.
fn subcommands_of(help: &str) -> Vec<String> {
    help.lines()
        .skip_while(|line| line.trim_end() != "Commands:")
        .skip(1)
        .take_while(|line| !line.trim().is_empty())
        .filter_map(|line| line.strip_prefix("  "))
        .filter_map(|line| line.split_whitespace().next())
        .filter(|name| *name != "help")
        .map(str::to_owned)
        .collect()
}

/// Every help page in `bin`'s command tree, depth-first, with a header per page.
fn dump(bin: &str, flag: &str) -> String {
    let mut out = String::new();
    let mut stack = vec![Vec::<String>::new()];
    while let Some(path) = stack.pop() {
        let text = help(bin, &path, flag);
        let title = std::iter::once(bin)
            .chain(path.iter().map(String::as_str))
            .collect::<Vec<_>>()
            .join(" ");
        writeln!(out, "$ {title} {flag}\n{text}").expect("write to String");
        for sub in subcommands_of(&text).into_iter().rev() {
            let mut child = path.clone();
            child.push(sub);
            stack.push(child);
        }
    }
    out
}

#[test]
fn webp_help() {
    insta::assert_snapshot!("webp", dump("webp", "--help"));
}

#[test]
fn cwebp_help() {
    insta::assert_snapshot!("cwebp", dump("cwebp", "-h"));
}

#[test]
fn dwebp_help() {
    insta::assert_snapshot!("dwebp", dump("dwebp", "-h"));
}

#[cfg(test)]
mod tests {
    use super::subcommands_of;

    /// The tree walk is only as good as this parse: if clap restyles its help,
    /// this fails loudly rather than silently snapshotting one page.
    #[test]
    fn subcommands_are_parsed_from_a_commands_section() {
        let help = "Usage: webp [OPTIONS] <COMMAND>\n\
                    \n\
                    Commands:\n\
                    \x20 decode   Decode a WebP file\n\
                    \x20 encode   Encode an image\n\
                    \x20 help     Print this message\n\
                    \n\
                    Options:\n\
                    \x20 -h, --help  Print help\n";
        assert_eq!(subcommands_of(help), ["decode", "encode"]);
    }

    #[test]
    fn a_leaf_command_has_no_subcommands() {
        let help = "Usage: webp encode [OPTIONS]\n\nOptions:\n  -h, --help  Print help\n";
        assert!(subcommands_of(help).is_empty());
    }
}
