// Conventional Commits enforcement.
// Strict on type / non-empty subject / length — relaxed on case so technical
// subjects can mix the lower-case crate name (`webpkit-lossless`) with proper nouns
// (`Rust`, `VP8L`, `WebP`) naturally, and on body / footer length so
// bot-authored commits (Dependabot) with long SHAs / URLs pass.

export default {
    extends: ['@commitlint/config-conventional'],
    rules: {
        'header-max-length': [2, 'always', 100],
        'header-min-length': [2, 'always', 10],
        // subject-case intentionally disabled: technical subjects routinely
        // mix the lower-case crate name and capitalized proper nouns;
        // neither `lower-case` nor `sentence-case` fits naturally.
        'subject-case': [0],
        'subject-empty': [2, 'never'],
        'subject-full-stop': [2, 'never', '.'],
        'type-empty': [2, 'never'],
        'body-max-line-length': [0],
        'footer-max-line-length': [0],
        'type-enum': [
            2,
            'always',
            [
                'feat',
                'fix',
                'docs',
                'style',
                'refactor',
                'perf',
                'test',
                'build',
                'ci',
                'chore',
                'revert',
            ],
        ],
    },
};
