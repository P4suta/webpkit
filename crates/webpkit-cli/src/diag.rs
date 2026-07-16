//! rustc-class diagnostics: a headline, an optional cause, help and note lines,
//! and an optional caret into the argv that points at the offending token.
//!
//! [`Diagnostic`] is pure data — no styling, no terminal. [`crate::report`]
//! renders it, applying the terminal's color policy. Splitting the two is what
//! keeps this layer free of I/O and lets the rendering live where the color
//! decision already does.

/// A presentable failure: what went wrong, why, and what to do about it.
#[derive(Debug, Clone)]
pub(crate) struct Diagnostic {
    /// The one-line headline, shown after `error:`.
    title: String,
    /// Why it happened, in prose. May span several lines (`\n`-separated).
    cause: Option<String>,
    /// What to do instead; one entry per rendered line.
    help: Vec<String>,
    /// Ancillary remarks, each its own `note:` block.
    notes: Vec<String>,
    /// A caret into the reconstructed command line.
    span: Option<ArgvSpan>,
}

impl Diagnostic {
    /// A diagnostic with just a headline.
    #[must_use]
    pub(crate) fn new(title: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            cause: None,
            help: Vec::new(),
            notes: Vec::new(),
            span: None,
        }
    }

    /// Set the prose cause.
    #[must_use]
    pub(crate) fn with_cause(mut self, cause: impl Into<String>) -> Self {
        self.cause = Some(cause.into());
        self
    }

    /// Append help lines.
    #[must_use]
    pub(crate) fn with_help<I, S>(mut self, lines: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.help.extend(lines.into_iter().map(Into::into));
        self
    }

    /// Append one help line.
    #[must_use]
    pub(crate) fn with_help_line(mut self, line: impl Into<String>) -> Self {
        self.help.push(line.into());
        self
    }

    /// Append a note.
    #[must_use]
    pub(crate) fn with_note(mut self, note: impl Into<String>) -> Self {
        self.notes.push(note.into());
        self
    }

    /// Attach an argv caret.
    #[must_use]
    pub(crate) fn with_span(mut self, span: ArgvSpan) -> Self {
        self.span = Some(span);
        self
    }

    /// The headline.
    pub(crate) fn title(&self) -> &str {
        &self.title
    }

    /// The prose cause, if any.
    pub(crate) fn cause(&self) -> Option<&str> {
        self.cause.as_deref()
    }

    /// The help lines.
    pub(crate) fn help(&self) -> &[String] {
        &self.help
    }

    /// The notes.
    pub(crate) fn notes(&self) -> &[String] {
        &self.notes
    }

    /// The argv caret, if any.
    pub(crate) const fn span(&self) -> Option<&ArgvSpan> {
        self.span.as_ref()
    }
}

/// A caret under one token of a reconstructed command line.
///
/// Our "source" is argv, and the token offsets are known, so we point at them the
/// way rustc points at source. Offsets are counted in [`char`]s, not bytes, so a
/// Unicode path argument still lines the caret up under its token.
#[derive(Debug, Clone)]
pub(crate) struct ArgvSpan {
    /// `program arg0 arg1 ...`, space-joined, as displayed.
    line: String,
    /// The caret's start column (in chars from the left of `line`).
    start: usize,
    /// The caret's width in columns.
    width: usize,
}

impl ArgvSpan {
    /// Point a caret at `args[index]`, prefixing the line with `program`.
    ///
    /// `args` are the arguments as typed (argv without the program name). Returns
    /// `None` if `index` is out of range.
    #[must_use]
    pub(crate) fn at_token(program: &str, args: &[String], index: usize) -> Option<Self> {
        let token = args.get(index)?;
        let mut line = String::from(program);
        for arg in args {
            line.push(' ');
            line.push_str(arg);
        }
        let mut start = program.chars().count();
        for arg in &args[..index] {
            start += 1 + arg.chars().count();
        }
        start += 1; // the space before the token
        Some(Self {
            line,
            start,
            width: token.chars().count().max(1),
        })
    }

    /// The reconstructed command line.
    pub(crate) fn line(&self) -> &str {
        &self.line
    }

    /// The caret's start column.
    pub(crate) const fn start(&self) -> usize {
        self.start
    }

    /// The caret's width.
    pub(crate) const fn width(&self) -> usize {
        self.width
    }
}

/// An "unknown option" diagnostic with a caret and, when one is near enough, a
/// did-you-mean suggestion drawn from `known`.
#[must_use]
pub(crate) fn unknown_flag(
    program: &str,
    args: &[String],
    index: usize,
    flag: &str,
    known: &[&str],
) -> Diagnostic {
    let mut diag = Diagnostic::new(format!("unknown option `{flag}`"));
    if let Some(span) = ArgvSpan::at_token(program, args, index) {
        diag = diag.with_span(span);
    }
    if let Some(suggestion) = closest(flag, known.iter().copied()) {
        diag = diag.with_help_line(format!("a similar option exists: `{suggestion}`"));
    }
    diag
}

/// The candidate closest to `input`, if one is near enough to be worth
/// suggesting. The budget is a third of the input length, so short flags draw no
/// spurious matches (every two-letter flag is one edit from every other, so a
/// 2-char typo gets budget 0 and suggests nothing).
pub(crate) fn closest<'a>(
    input: &str,
    candidates: impl IntoIterator<Item = &'a str>,
) -> Option<&'a str> {
    let budget = input.chars().count() / 3;
    candidates
        .into_iter()
        .map(|candidate| (edit_distance(input, candidate), candidate))
        .filter(|&(distance, _)| distance <= budget)
        .min_by_key(|&(distance, _)| distance)
        .map(|(_, candidate)| candidate)
}

/// The Levenshtein edit distance between two strings, over [`char`]s.
fn edit_distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut curr: Vec<usize> = vec![0; b.len() + 1];
    for (i, &ca) in a.iter().enumerate() {
        curr[0] = i + 1;
        for (j, &cb) in b.iter().enumerate() {
            let cost = usize::from(ca != cb);
            curr[j + 1] = (prev[j] + cost).min(prev[j + 1] + 1).min(curr[j] + 1);
        }
        std::mem::swap(&mut prev, &mut curr);
    }
    prev[b.len()]
}

#[cfg(test)]
mod tests {
    use super::{ArgvSpan, closest, edit_distance};

    #[test]
    fn edit_distance_counts_single_edits() {
        assert_eq!(edit_distance("-lossless", "-lossless"), 0);
        assert_eq!(edit_distance("-lossles", "-lossless"), 1);
        assert_eq!(edit_distance("-nearlossless", "-near_lossless"), 1);
        assert_eq!(edit_distance("kitten", "sitting"), 3);
    }

    #[test]
    fn closest_suggests_a_near_miss_and_ignores_a_far_one() {
        let known = ["-lossless", "-quality", "-metadata"];
        assert_eq!(closest("-lossles", known), Some("-lossless"));
        assert_eq!(closest("-xyzzy", known), None);
    }

    #[test]
    fn a_short_flag_does_not_draw_a_spurious_suggestion() {
        // `-x` is one edit from `-o`, `-m`, `-v`; none is a real match.
        assert_eq!(closest("-x", ["-o", "-m", "-v"]), None);
    }

    #[test]
    fn a_caret_underlines_the_indexed_token() {
        let args: Vec<String> = ["-near_lossless", "60", "photo.png"]
            .into_iter()
            .map(str::to_owned)
            .collect();
        let span = ArgvSpan::at_token("cwebp", &args, 0).expect("token 0 exists");
        assert_eq!(span.line(), "cwebp -near_lossless 60 photo.png");
        // "cwebp " is 6 columns; the flag is 14 wide.
        assert_eq!(span.start(), 6);
        assert_eq!(span.width(), "-near_lossless".len());
    }

    #[test]
    fn a_caret_tracks_a_later_token() {
        let args: Vec<String> = ["-o", "out.webp", "-crop"]
            .into_iter()
            .map(str::to_owned)
            .collect();
        let span = ArgvSpan::at_token("cwebp", &args, 2).expect("token 2 exists");
        // "cwebp -o out.webp " is 18 columns.
        assert_eq!(span.start(), 18);
        assert_eq!(&span.line()[span.start()..], "-crop");
    }

    #[test]
    fn an_out_of_range_token_has_no_span() {
        assert!(ArgvSpan::at_token("cwebp", &["-o".to_owned()], 5).is_none());
    }
}
