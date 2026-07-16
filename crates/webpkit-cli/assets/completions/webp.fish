# Print an optspec for argparse to handle cmd's options that are independent of any subcommand.
function __fish_webp_global_optspecs
    string join \n v/verbose q/quiet color= h/help V/version
end

function __fish_webp_needs_command
    # Figure out if the current invocation already has a command.
    set -l cmd (commandline -opc)
    set -e cmd[1]
    argparse -s (__fish_webp_global_optspecs) -- $cmd 2>/dev/null
    or return
    if set -q argv[1]
        # Also print the command, so this can be used to figure out what it is.
        echo $argv[1]
        return 1
    end
    return 0
end

function __fish_webp_using_subcommand
    set -l cmd (__fish_webp_needs_command)
    test -z "$cmd"
    and return 1
    contains -- $cmd[1] $argv
end

complete -c webp -n "__fish_webp_needs_command" -l color -d 'auto, always, or never' -r -f -a "auto\t'Style only when the stream is a terminal that wants it (the default)'
always\t'Style even when the stream is redirected'
never\t'Never style'"
complete -c webp -n "__fish_webp_needs_command" -s v -l verbose -d 'Print per-stage detail on stderr'
complete -c webp -n "__fish_webp_needs_command" -s q -l quiet -d 'Suppress all non-error output'
complete -c webp -n "__fish_webp_needs_command" -s h -l help -d 'Print help (see more with \'--help\')'
complete -c webp -n "__fish_webp_needs_command" -s V -l version -d 'Print version'
complete -c webp -n "__fish_webp_needs_command" -f -a "decode" -d 'Decode a WebP file to PNG (default), PPM/PAM, or raw pixels'
complete -c webp -n "__fish_webp_needs_command" -f -a "encode" -d 'Encode a PNG/PPM/PAM/raw image into a WebP file (lossless, or --lossy)'
complete -c webp -n "__fish_webp_needs_command" -f -a "convert" -d 'Batch-convert many images (or directories) to WebP, in parallel'
complete -c webp -n "__fish_webp_needs_command" -f -a "info" -d 'Print a summary of a WebP file (size, alpha, metadata, animation)'
complete -c webp -n "__fish_webp_needs_command" -f -a "completions" -d 'Print a shell completion script'
complete -c webp -n "__fish_webp_needs_command" -f -a "man" -d 'Print a man page in roff, for `man -l -` or a package\'s man directory'
complete -c webp -n "__fish_webp_needs_command" -f -a "help" -d 'Print this message or the help of the given subcommand(s)'
complete -c webp -n "__fish_webp_using_subcommand decode" -s o -l output -d 'Output path; `-` writes stdout' -r -F
complete -c webp -n "__fish_webp_using_subcommand decode" -l format -d 'Output format; defaults to the `-o` extension, else PNG' -r -f -a "png\t'PNG, RGBA8'
ppm\t'Netpbm binary PPM (`P6`, RGB; alpha dropped)'
pam\t'Netpbm binary PAM (`P7`, RGBA)'
raw\t'Raw row-major pixels in the requested `--layout`'"
complete -c webp -n "__fish_webp_using_subcommand decode" -l layout -d 'Byte order for raw output only' -r -f -a "rgba8\t'`R, G, B, A`'
argb8\t'`A, R, G, B`'
bgra8\t'`B, G, R, A`'"
complete -c webp -n "__fish_webp_using_subcommand decode" -l frames -d 'For animations: which frames to emit' -r -f -a "first\t'Only the first composited frame (the default)'
all\t'Every composited frame, numbered `<stem>-000.<ext>`, ...'"
complete -c webp -n "__fish_webp_using_subcommand decode" -l frame -d 'For animations: emit only this 0-based frame' -r
complete -c webp -n "__fish_webp_using_subcommand decode" -l metadata -d 'Metadata to carry into the output: all,none,icc,exif,xmp (default: all)' -r -f -a "all\t'Keep ICC, Exif, and XMP'
none\t'Strip everything (a bare `VP8L` output)'
icc\t'Keep the ICC color profile'
exif\t'Keep Exif'
xmp\t'Keep XMP'"
complete -c webp -n "__fish_webp_using_subcommand decode" -l color -d 'auto, always, or never' -r -f -a "auto\t'Style only when the stream is a terminal that wants it (the default)'
always\t'Style even when the stream is redirected'
never\t'Never style'"
complete -c webp -n "__fish_webp_using_subcommand decode" -s v -l verbose -d 'Print per-stage detail on stderr'
complete -c webp -n "__fish_webp_using_subcommand decode" -s q -l quiet -d 'Suppress all non-error output'
complete -c webp -n "__fish_webp_using_subcommand decode" -s h -l help -d 'Print help (see more with \'--help\')'
complete -c webp -n "__fish_webp_using_subcommand encode" -s o -l output -d 'Output `.webp` file; `-` writes stdout' -r -F
complete -c webp -n "__fish_webp_using_subcommand encode" -l input-format -d 'Input format; defaults to the extension, else the magic bytes, else raw' -r -f -a "png\t'PNG (any color type; normalized to RGBA8)'
ppm\t'Netpbm binary PPM (`P6`, RGB)'
pam\t'Netpbm binary PAM (`P7`, RGBA)'
raw\t'Raw row-major pixels; requires `--width`/`--height`/`--layout`'"
complete -c webp -n "__fish_webp_using_subcommand encode" -l width -d 'Raw-input width in pixels (required for raw input)' -r
complete -c webp -n "__fish_webp_using_subcommand encode" -l height -d 'Raw-input height in pixels (required for raw input)' -r
complete -c webp -n "__fish_webp_using_subcommand encode" -l layout -d 'Byte order for raw input only' -r -f -a "rgba8\t'`R, G, B, A`'
argb8\t'`A, R, G, B`'
bgra8\t'`B, G, R, A`'"
complete -c webp -n "__fish_webp_using_subcommand encode" -s m -l method -d 'Encoder effort' -r -f -a "fast\t'Fastest: literal + subtract-green only'
balanced\t'Balanced (the default): LZ77 + color cache'
best\t'Smallest: adds Tier 3 forward transforms and meta-Huffman on top of Balanced'"
complete -c webp -n "__fish_webp_using_subcommand encode" -l quality -d 'Lossy quality 0-100 (higher = larger, closer to source); selects --lossy' -r
complete -c webp -n "__fish_webp_using_subcommand encode" -l metadata -d 'Metadata to embed: all,none,icc,exif,xmp (default: all — kinder than cwebp)' -r -f -a "all\t'Keep ICC, Exif, and XMP'
none\t'Strip everything (a bare `VP8L` output)'
icc\t'Keep the ICC color profile'
exif\t'Keep Exif'
xmp\t'Keep XMP'"
complete -c webp -n "__fish_webp_using_subcommand encode" -l color -d 'auto, always, or never' -r -f -a "auto\t'Style only when the stream is a terminal that wants it (the default)'
always\t'Style even when the stream is redirected'
never\t'Never style'"
complete -c webp -n "__fish_webp_using_subcommand encode" -l lossy -d 'Encode lossily (VP8) instead of losslessly (VP8L)'
complete -c webp -n "__fish_webp_using_subcommand encode" -s v -l verbose -d 'Print per-stage detail on stderr'
complete -c webp -n "__fish_webp_using_subcommand encode" -s q -l quiet -d 'Suppress all non-error output'
complete -c webp -n "__fish_webp_using_subcommand encode" -s h -l help -d 'Print help (see more with \'--help\')'
complete -c webp -n "__fish_webp_using_subcommand convert" -s o -l output -d 'Output directory (created outputs are `<stem>.webp`); default: beside input' -r -F
complete -c webp -n "__fish_webp_using_subcommand convert" -s m -l method -d 'Encoder effort (ignored with --optimize)' -r -f -a "fast\t'Fastest: literal + subtract-green only'
balanced\t'Balanced (the default): LZ77 + color cache'
best\t'Smallest: adds Tier 3 forward transforms and meta-Huffman on top of Balanced'"
complete -c webp -n "__fish_webp_using_subcommand convert" -l quality -d 'Lossy quality 0-100 (higher = larger, closer to source); selects --lossy' -r
complete -c webp -n "__fish_webp_using_subcommand convert" -l metadata -d 'Metadata to embed: all,none,icc,exif,xmp (default: all)' -r -f -a "all\t'Keep ICC, Exif, and XMP'
none\t'Strip everything (a bare `VP8L` output)'
icc\t'Keep the ICC color profile'
exif\t'Keep Exif'
xmp\t'Keep XMP'"
complete -c webp -n "__fish_webp_using_subcommand convert" -l color -d 'auto, always, or never' -r -f -a "auto\t'Style only when the stream is a terminal that wants it (the default)'
always\t'Style even when the stream is redirected'
never\t'Never style'"
complete -c webp -n "__fish_webp_using_subcommand convert" -l lossy -d 'Encode lossily (VP8) instead of losslessly (VP8L)'
complete -c webp -n "__fish_webp_using_subcommand convert" -l optimize -d 'Try every lossless effort level and keep the smallest output'
complete -c webp -n "__fish_webp_using_subcommand convert" -s r -l recursive -d 'Recurse into subdirectories'
complete -c webp -n "__fish_webp_using_subcommand convert" -s v -l verbose -d 'Print per-stage detail on stderr'
complete -c webp -n "__fish_webp_using_subcommand convert" -s q -l quiet -d 'Suppress all non-error output'
complete -c webp -n "__fish_webp_using_subcommand convert" -s h -l help -d 'Print help (see more with \'--help\')'
complete -c webp -n "__fish_webp_using_subcommand info" -l color -d 'auto, always, or never' -r -f -a "auto\t'Style only when the stream is a terminal that wants it (the default)'
always\t'Style even when the stream is redirected'
never\t'Never style'"
complete -c webp -n "__fish_webp_using_subcommand info" -s v -l verbose -d 'Print per-stage detail on stderr'
complete -c webp -n "__fish_webp_using_subcommand info" -s q -l quiet -d 'Suppress all non-error output'
complete -c webp -n "__fish_webp_using_subcommand info" -s h -l help -d 'Print help (see more with \'--help\')'
complete -c webp -n "__fish_webp_using_subcommand completions" -l color -d 'auto, always, or never' -r -f -a "auto\t'Style only when the stream is a terminal that wants it (the default)'
always\t'Style even when the stream is redirected'
never\t'Never style'"
complete -c webp -n "__fish_webp_using_subcommand completions" -s v -l verbose -d 'Print per-stage detail on stderr'
complete -c webp -n "__fish_webp_using_subcommand completions" -s q -l quiet -d 'Suppress all non-error output'
complete -c webp -n "__fish_webp_using_subcommand completions" -s h -l help -d 'Print help (see more with \'--help\')'
complete -c webp -n "__fish_webp_using_subcommand man" -l color -d 'auto, always, or never' -r -f -a "auto\t'Style only when the stream is a terminal that wants it (the default)'
always\t'Style even when the stream is redirected'
never\t'Never style'"
complete -c webp -n "__fish_webp_using_subcommand man" -s v -l verbose -d 'Print per-stage detail on stderr'
complete -c webp -n "__fish_webp_using_subcommand man" -s q -l quiet -d 'Suppress all non-error output'
complete -c webp -n "__fish_webp_using_subcommand man" -s h -l help -d 'Print help (see more with \'--help\')'
complete -c webp -n "__fish_webp_using_subcommand help; and not __fish_seen_subcommand_from decode encode convert info completions man help" -f -a "decode" -d 'Decode a WebP file to PNG (default), PPM/PAM, or raw pixels'
complete -c webp -n "__fish_webp_using_subcommand help; and not __fish_seen_subcommand_from decode encode convert info completions man help" -f -a "encode" -d 'Encode a PNG/PPM/PAM/raw image into a WebP file (lossless, or --lossy)'
complete -c webp -n "__fish_webp_using_subcommand help; and not __fish_seen_subcommand_from decode encode convert info completions man help" -f -a "convert" -d 'Batch-convert many images (or directories) to WebP, in parallel'
complete -c webp -n "__fish_webp_using_subcommand help; and not __fish_seen_subcommand_from decode encode convert info completions man help" -f -a "info" -d 'Print a summary of a WebP file (size, alpha, metadata, animation)'
complete -c webp -n "__fish_webp_using_subcommand help; and not __fish_seen_subcommand_from decode encode convert info completions man help" -f -a "completions" -d 'Print a shell completion script'
complete -c webp -n "__fish_webp_using_subcommand help; and not __fish_seen_subcommand_from decode encode convert info completions man help" -f -a "man" -d 'Print a man page in roff, for `man -l -` or a package\'s man directory'
complete -c webp -n "__fish_webp_using_subcommand help; and not __fish_seen_subcommand_from decode encode convert info completions man help" -f -a "help" -d 'Print this message or the help of the given subcommand(s)'
