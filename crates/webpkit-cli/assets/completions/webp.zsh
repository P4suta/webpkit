#compdef webp

autoload -U is-at-least

_webp() {
    typeset -A opt_args
    typeset -a _arguments_options
    local ret=1

    if is-at-least 5.2; then
        _arguments_options=(-s -S -C)
    else
        _arguments_options=(-s -C)
    fi

    local context curcontext="$curcontext" state line
    _arguments "${_arguments_options[@]}" : \
'--color=[auto, always, or never]:WHEN:((auto\:"Style only when the stream is a terminal that wants it (the default)"
always\:"Style even when the stream is redirected"
never\:"Never style"))' \
'*-v[Print per-stage detail on stderr]' \
'*--verbose[Print per-stage detail on stderr]' \
'(-v --verbose)-q[Suppress all non-error output]' \
'(-v --verbose)--quiet[Suppress all non-error output]' \
'-h[Print help (see more with '\''--help'\'')]' \
'--help[Print help (see more with '\''--help'\'')]' \
'-V[Print version]' \
'--version[Print version]' \
":: :_webp_commands" \
"*::: :->webp" \
&& ret=0
    case $state in
    (webp)
        words=($line[1] "${words[@]}")
        (( CURRENT += 1 ))
        curcontext="${curcontext%:*:*}:webp-command-$line[1]:"
        case $line[1] in
            (decode)
_arguments "${_arguments_options[@]}" : \
'-o+[Output path; \`-\` writes stdout]:OUTPUT:_files' \
'--output=[Output path; \`-\` writes stdout]:OUTPUT:_files' \
'--format=[Output format; defaults to the \`-o\` extension, else PNG]:FORMAT:((png\:"PNG, RGBA8"
ppm\:"Netpbm binary PPM (\`P6\`, RGB; alpha dropped)"
pam\:"Netpbm binary PAM (\`P7\`, RGBA)"
raw\:"Raw row-major pixels in the requested \`--layout\`"))' \
'--layout=[Byte order for raw output only]:LAYOUT:((rgba8\:"\`R, G, B, A\`"
argb8\:"\`A, R, G, B\`"
bgra8\:"\`B, G, R, A\`"))' \
'--frames=[For animations\: which frames to emit]:FRAMES:((first\:"Only the first composited frame (the default)"
all\:"Every composited frame, numbered \`<stem>-000.<ext>\`, ..."))' \
'(--frames)--frame=[For animations\: emit only this 0-based frame]:FRAME:_default' \
'*--metadata=[Metadata to carry into the output\: all,none,icc,exif,xmp (default\: all)]:METADATA:((all\:"Keep ICC, Exif, and XMP"
none\:"Strip everything (a bare \`VP8L\` output)"
icc\:"Keep the ICC color profile"
exif\:"Keep Exif"
xmp\:"Keep XMP"))' \
'--color=[auto, always, or never]:WHEN:((auto\:"Style only when the stream is a terminal that wants it (the default)"
always\:"Style even when the stream is redirected"
never\:"Never style"))' \
'*-v[Print per-stage detail on stderr]' \
'*--verbose[Print per-stage detail on stderr]' \
'(-v --verbose)-q[Suppress all non-error output]' \
'(-v --verbose)--quiet[Suppress all non-error output]' \
'-h[Print help (see more with '\''--help'\'')]' \
'--help[Print help (see more with '\''--help'\'')]' \
'::input -- Input `.webp` file; `-` (the default) reads stdin:_files' \
&& ret=0
;;
(encode)
_arguments "${_arguments_options[@]}" : \
'-o+[Output \`.webp\` file; \`-\` writes stdout]:OUTPUT:_files' \
'--output=[Output \`.webp\` file; \`-\` writes stdout]:OUTPUT:_files' \
'--input-format=[Input format; defaults to the extension, else the magic bytes, else raw]:INPUT_FORMAT:((png\:"PNG (any color type; normalized to RGBA8)"
ppm\:"Netpbm binary PPM (\`P6\`, RGB)"
pam\:"Netpbm binary PAM (\`P7\`, RGBA)"
raw\:"Raw row-major pixels; requires \`--width\`/\`--height\`/\`--layout\`"))' \
'--width=[Raw-input width in pixels (required for raw input)]:WIDTH:_default' \
'--height=[Raw-input height in pixels (required for raw input)]:HEIGHT:_default' \
'--layout=[Byte order for raw input only]:LAYOUT:((rgba8\:"\`R, G, B, A\`"
argb8\:"\`A, R, G, B\`"
bgra8\:"\`B, G, R, A\`"))' \
'-m+[Encoder effort]:METHOD:((fast\:"Fastest\: literal + subtract-green only"
balanced\:"Balanced (the default)\: LZ77 + color cache"
best\:"Smallest\: adds Tier 3 forward transforms and meta-Huffman on top of Balanced"))' \
'--method=[Encoder effort]:METHOD:((fast\:"Fastest\: literal + subtract-green only"
balanced\:"Balanced (the default)\: LZ77 + color cache"
best\:"Smallest\: adds Tier 3 forward transforms and meta-Huffman on top of Balanced"))' \
'--quality=[Lossy quality 0-100 (higher = larger, closer to source); selects --lossy]:QUALITY:_default' \
'*--metadata=[Metadata to embed\: all,none,icc,exif,xmp (default\: all — kinder than cwebp)]:METADATA:((all\:"Keep ICC, Exif, and XMP"
none\:"Strip everything (a bare \`VP8L\` output)"
icc\:"Keep the ICC color profile"
exif\:"Keep Exif"
xmp\:"Keep XMP"))' \
'--color=[auto, always, or never]:WHEN:((auto\:"Style only when the stream is a terminal that wants it (the default)"
always\:"Style even when the stream is redirected"
never\:"Never style"))' \
'--lossy[Encode lossily (VP8) instead of losslessly (VP8L)]' \
'*-v[Print per-stage detail on stderr]' \
'*--verbose[Print per-stage detail on stderr]' \
'(-v --verbose)-q[Suppress all non-error output]' \
'(-v --verbose)--quiet[Suppress all non-error output]' \
'-h[Print help (see more with '\''--help'\'')]' \
'--help[Print help (see more with '\''--help'\'')]' \
'::input -- Input image (PNG/PPM/PAM/raw); `-` (the default) reads stdin:_files' \
&& ret=0
;;
(convert)
_arguments "${_arguments_options[@]}" : \
'-o+[Output directory (created outputs are \`<stem>.webp\`); default\: beside input]:OUTPUT:_files' \
'--output=[Output directory (created outputs are \`<stem>.webp\`); default\: beside input]:OUTPUT:_files' \
'-m+[Encoder effort (ignored with --optimize)]:METHOD:((fast\:"Fastest\: literal + subtract-green only"
balanced\:"Balanced (the default)\: LZ77 + color cache"
best\:"Smallest\: adds Tier 3 forward transforms and meta-Huffman on top of Balanced"))' \
'--method=[Encoder effort (ignored with --optimize)]:METHOD:((fast\:"Fastest\: literal + subtract-green only"
balanced\:"Balanced (the default)\: LZ77 + color cache"
best\:"Smallest\: adds Tier 3 forward transforms and meta-Huffman on top of Balanced"))' \
'--quality=[Lossy quality 0-100 (higher = larger, closer to source); selects --lossy]:QUALITY:_default' \
'*--metadata=[Metadata to embed\: all,none,icc,exif,xmp (default\: all)]:METADATA:((all\:"Keep ICC, Exif, and XMP"
none\:"Strip everything (a bare \`VP8L\` output)"
icc\:"Keep the ICC color profile"
exif\:"Keep Exif"
xmp\:"Keep XMP"))' \
'--color=[auto, always, or never]:WHEN:((auto\:"Style only when the stream is a terminal that wants it (the default)"
always\:"Style even when the stream is redirected"
never\:"Never style"))' \
'--lossy[Encode lossily (VP8) instead of losslessly (VP8L)]' \
'--optimize[Try every lossless effort level and keep the smallest output]' \
'-r[Recurse into subdirectories]' \
'--recursive[Recurse into subdirectories]' \
'*-v[Print per-stage detail on stderr]' \
'*--verbose[Print per-stage detail on stderr]' \
'(-v --verbose)-q[Suppress all non-error output]' \
'(-v --verbose)--quiet[Suppress all non-error output]' \
'-h[Print help (see more with '\''--help'\'')]' \
'--help[Print help (see more with '\''--help'\'')]' \
'*::inputs -- Input images and/or directories (PNG/PPM/PAM):_files' \
&& ret=0
;;
(info)
_arguments "${_arguments_options[@]}" : \
'--color=[auto, always, or never]:WHEN:((auto\:"Style only when the stream is a terminal that wants it (the default)"
always\:"Style even when the stream is redirected"
never\:"Never style"))' \
'--json[Print the report as JSON instead of text]' \
'*-v[Print per-stage detail on stderr]' \
'*--verbose[Print per-stage detail on stderr]' \
'(-v --verbose)-q[Suppress all non-error output]' \
'(-v --verbose)--quiet[Suppress all non-error output]' \
'-h[Print help (see more with '\''--help'\'')]' \
'--help[Print help (see more with '\''--help'\'')]' \
'::input -- Input `.webp` file; `-` (the default) reads stdin:_files' \
&& ret=0
;;
(completions)
_arguments "${_arguments_options[@]}" : \
'--color=[auto, always, or never]:WHEN:((auto\:"Style only when the stream is a terminal that wants it (the default)"
always\:"Style even when the stream is redirected"
never\:"Never style"))' \
'*-v[Print per-stage detail on stderr]' \
'*--verbose[Print per-stage detail on stderr]' \
'(-v --verbose)-q[Suppress all non-error output]' \
'(-v --verbose)--quiet[Suppress all non-error output]' \
'-h[Print help (see more with '\''--help'\'')]' \
'--help[Print help (see more with '\''--help'\'')]' \
':shell -- The shell to generate for:(bash elvish fish powershell zsh)' \
&& ret=0
;;
(man)
_arguments "${_arguments_options[@]}" : \
'--color=[auto, always, or never]:WHEN:((auto\:"Style only when the stream is a terminal that wants it (the default)"
always\:"Style even when the stream is redirected"
never\:"Never style"))' \
'*-v[Print per-stage detail on stderr]' \
'*--verbose[Print per-stage detail on stderr]' \
'(-v --verbose)-q[Suppress all non-error output]' \
'(-v --verbose)--quiet[Suppress all non-error output]' \
'-h[Print help (see more with '\''--help'\'')]' \
'--help[Print help (see more with '\''--help'\'')]' \
'::command -- Document this subcommand instead of the tool itself:_default' \
&& ret=0
;;
(help)
_arguments "${_arguments_options[@]}" : \
":: :_webp__subcmd__help_commands" \
"*::: :->help" \
&& ret=0

    case $state in
    (help)
        words=($line[1] "${words[@]}")
        (( CURRENT += 1 ))
        curcontext="${curcontext%:*:*}:webp-help-command-$line[1]:"
        case $line[1] in
            (decode)
_arguments "${_arguments_options[@]}" : \
&& ret=0
;;
(encode)
_arguments "${_arguments_options[@]}" : \
&& ret=0
;;
(convert)
_arguments "${_arguments_options[@]}" : \
&& ret=0
;;
(info)
_arguments "${_arguments_options[@]}" : \
&& ret=0
;;
(completions)
_arguments "${_arguments_options[@]}" : \
&& ret=0
;;
(man)
_arguments "${_arguments_options[@]}" : \
&& ret=0
;;
(help)
_arguments "${_arguments_options[@]}" : \
&& ret=0
;;
        esac
    ;;
esac
;;
        esac
    ;;
esac
}

(( $+functions[_webp_commands] )) ||
_webp_commands() {
    local commands; commands=(
'decode:Decode a WebP file to PNG (default), PPM/PAM, or raw pixels' \
'encode:Encode a PNG/PPM/PAM/raw image into a WebP file (lossless, or --lossy)' \
'convert:Batch-convert many images (or directories) to WebP, in parallel' \
'info:Print a summary of a WebP file (size, alpha, metadata, animation)' \
'completions:Print a shell completion script' \
'man:Print a man page in roff, for \`man -l -\` or a package'\''s man directory' \
'help:Print this message or the help of the given subcommand(s)' \
    )
    _describe -t commands 'webp commands' commands "$@"
}
(( $+functions[_webp__subcmd__completions_commands] )) ||
_webp__subcmd__completions_commands() {
    local commands; commands=()
    _describe -t commands 'webp completions commands' commands "$@"
}
(( $+functions[_webp__subcmd__convert_commands] )) ||
_webp__subcmd__convert_commands() {
    local commands; commands=()
    _describe -t commands 'webp convert commands' commands "$@"
}
(( $+functions[_webp__subcmd__decode_commands] )) ||
_webp__subcmd__decode_commands() {
    local commands; commands=()
    _describe -t commands 'webp decode commands' commands "$@"
}
(( $+functions[_webp__subcmd__encode_commands] )) ||
_webp__subcmd__encode_commands() {
    local commands; commands=()
    _describe -t commands 'webp encode commands' commands "$@"
}
(( $+functions[_webp__subcmd__help_commands] )) ||
_webp__subcmd__help_commands() {
    local commands; commands=(
'decode:Decode a WebP file to PNG (default), PPM/PAM, or raw pixels' \
'encode:Encode a PNG/PPM/PAM/raw image into a WebP file (lossless, or --lossy)' \
'convert:Batch-convert many images (or directories) to WebP, in parallel' \
'info:Print a summary of a WebP file (size, alpha, metadata, animation)' \
'completions:Print a shell completion script' \
'man:Print a man page in roff, for \`man -l -\` or a package'\''s man directory' \
'help:Print this message or the help of the given subcommand(s)' \
    )
    _describe -t commands 'webp help commands' commands "$@"
}
(( $+functions[_webp__subcmd__help__subcmd__completions_commands] )) ||
_webp__subcmd__help__subcmd__completions_commands() {
    local commands; commands=()
    _describe -t commands 'webp help completions commands' commands "$@"
}
(( $+functions[_webp__subcmd__help__subcmd__convert_commands] )) ||
_webp__subcmd__help__subcmd__convert_commands() {
    local commands; commands=()
    _describe -t commands 'webp help convert commands' commands "$@"
}
(( $+functions[_webp__subcmd__help__subcmd__decode_commands] )) ||
_webp__subcmd__help__subcmd__decode_commands() {
    local commands; commands=()
    _describe -t commands 'webp help decode commands' commands "$@"
}
(( $+functions[_webp__subcmd__help__subcmd__encode_commands] )) ||
_webp__subcmd__help__subcmd__encode_commands() {
    local commands; commands=()
    _describe -t commands 'webp help encode commands' commands "$@"
}
(( $+functions[_webp__subcmd__help__subcmd__help_commands] )) ||
_webp__subcmd__help__subcmd__help_commands() {
    local commands; commands=()
    _describe -t commands 'webp help help commands' commands "$@"
}
(( $+functions[_webp__subcmd__help__subcmd__info_commands] )) ||
_webp__subcmd__help__subcmd__info_commands() {
    local commands; commands=()
    _describe -t commands 'webp help info commands' commands "$@"
}
(( $+functions[_webp__subcmd__help__subcmd__man_commands] )) ||
_webp__subcmd__help__subcmd__man_commands() {
    local commands; commands=()
    _describe -t commands 'webp help man commands' commands "$@"
}
(( $+functions[_webp__subcmd__info_commands] )) ||
_webp__subcmd__info_commands() {
    local commands; commands=()
    _describe -t commands 'webp info commands' commands "$@"
}
(( $+functions[_webp__subcmd__man_commands] )) ||
_webp__subcmd__man_commands() {
    local commands; commands=()
    _describe -t commands 'webp man commands' commands "$@"
}

if [ "$funcstack[1]" = "_webp" ]; then
    _webp "$@"
else
    compdef _webp webp
fi
