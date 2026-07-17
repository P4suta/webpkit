# Print an optspec for argparse to handle cmd's options that are independent of any subcommand.
function __fish_webp_global_optspecs
    string join \n v/verbose quiet color= threads= dry-run o/output= q/quality= lossless lossy near-lossless= m/method= metadata= crop= resize= r/recursive force no-clobber h/help V/version
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

complete -c webp -n "__fish_webp_needs_command" -l color -d 'auto, always, or never [default: auto, or from env/config]' -r -f -a "auto\t'Style only when the stream is a terminal that wants it (the default)'
always\t'Style even when the stream is redirected'
never\t'Never style'"
complete -c webp -n "__fish_webp_needs_command" -l threads -d 'Worker threads for parallel work; 0 (the default) uses one per core' -r
complete -c webp -n "__fish_webp_needs_command" -s o -l output -d 'Output file, or a directory for many inputs; default: beside each input' -r -F
complete -c webp -n "__fish_webp_needs_command" -s q -l quality -d 'Lossy quality 0-100 (higher = larger, closer to source); selects lossy' -r
complete -c webp -n "__fish_webp_needs_command" -l near-lossless -d 'Near-lossless preprocessing 0-100 (lower = stronger; implies lossless)' -r
complete -c webp -n "__fish_webp_needs_command" -s m -l method -d 'Encoder effort [default: auto, or from env/config]' -r -f -a "auto\t'Adapt the search depth to the image\'s content and size (the default)'
fast\t'Fastest: the shallowest fixed search'
best\t'Smallest: the deepest fixed search'"
complete -c webp -n "__fish_webp_needs_command" -l metadata -d 'Metadata to embed: all,none,icc,exif,xmp (default: all)' -r -f -a "all\t'Keep ICC, Exif, and XMP'
none\t'Strip everything (a bare `VP8L` output)'
icc\t'Keep the ICC color profile'
exif\t'Keep Exif'
xmp\t'Keep XMP'"
complete -c webp -n "__fish_webp_needs_command" -l crop -d 'Crop before encoding: `x,y,width,height` in pixels (applied before --resize)' -r
complete -c webp -n "__fish_webp_needs_command" -l resize -d 'Resize before encoding: `WxH` (use 0 on one axis to keep aspect)' -r
complete -c webp -n "__fish_webp_needs_command" -s v -l verbose -d 'Print per-stage detail on stderr'
complete -c webp -n "__fish_webp_needs_command" -l quiet -d 'Suppress all non-error output'
complete -c webp -n "__fish_webp_needs_command" -l dry-run -d 'Report what would be written, without encoding or writing anything'
complete -c webp -n "__fish_webp_needs_command" -l lossless -d 'Force lossless (VP8L) encoding'
complete -c webp -n "__fish_webp_needs_command" -l lossy -d 'Force lossy (VP8) encoding'
complete -c webp -n "__fish_webp_needs_command" -s r -l recursive -d 'Recurse into subdirectories'
complete -c webp -n "__fish_webp_needs_command" -l force -d 'Overwrite an existing derived output'
complete -c webp -n "__fish_webp_needs_command" -l no-clobber -d 'Skip an existing derived output instead of failing (still exits 0)'
complete -c webp -n "__fish_webp_needs_command" -s h -l help -d 'Print help (see more with \'--help\')'
complete -c webp -n "__fish_webp_needs_command" -s V -l version -d 'Print version'
complete -c webp -n "__fish_webp_needs_command" -a "decode" -d 'Decode a WebP file to PNG (default), PPM/PAM, or raw pixels'
complete -c webp -n "__fish_webp_needs_command" -a "encode" -d 'Encode an image (PNG/JPEG/GIF/TIFF/BMP/PPM/PAM/raw) into a WebP file'
complete -c webp -n "__fish_webp_needs_command" -a "convert" -d 'Batch-convert many images (or directories) to WebP, in parallel'
complete -c webp -n "__fish_webp_needs_command" -a "animate" -d 'Assemble still images into an animated WebP (surpasses img2webp/gif2webp)'
complete -c webp -n "__fish_webp_needs_command" -a "mux" -d 'Edit an animated WebP without re-encoding frames (webpmux-parity muxing)'
complete -c webp -n "__fish_webp_needs_command" -a "info" -d 'Print a summary of a WebP file (size, alpha, metadata, animation)'
complete -c webp -n "__fish_webp_needs_command" -a "meta" -d 'Read, set, or strip a WebP file\'s metadata (ICC/Exif/XMP), without re-encoding the image'
complete -c webp -n "__fish_webp_needs_command" -a "diff" -d 'Compare two images: report PSNR and the largest per-channel difference'
complete -c webp -n "__fish_webp_needs_command" -a "doctor" -d 'Diagnose the environment: PATH drop-in shadows, config, terminal, threads'
complete -c webp -n "__fish_webp_needs_command" -a "config" -d 'Show resolved settings and where each came from (args, env, file, default)'
complete -c webp -n "__fish_webp_needs_command" -a "explain" -d 'Explain an exit code: what a failing run\'s status number means'
complete -c webp -n "__fish_webp_needs_command" -a "completions" -d 'Print a shell completion script'
complete -c webp -n "__fish_webp_needs_command" -a "man" -d 'Print a man page in roff, for `man -l -` or a package\'s man directory'
complete -c webp -n "__fish_webp_needs_command" -a "help" -d 'Print this message or the help of the given subcommand(s)'
complete -c webp -n "__fish_webp_using_subcommand decode" -s o -l output -d 'Output path; `-` writes stdout' -r -F
complete -c webp -n "__fish_webp_using_subcommand decode" -l format -d 'Output format; defaults to the `-o` extension, else PNG' -r -f -a "png\t'PNG, RGBA8'
ppm\t'Netpbm binary PPM (`P6`, RGB; alpha dropped)'
pam\t'Netpbm binary PAM (`P7`, RGBA)'
bmp\t'BMP (RGBA8; needs the `formats` feature)'
tiff\t'TIFF (RGBA8; needs the `formats` feature)'
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
complete -c webp -n "__fish_webp_using_subcommand decode" -l color -d 'auto, always, or never [default: auto, or from env/config]' -r -f -a "auto\t'Style only when the stream is a terminal that wants it (the default)'
always\t'Style even when the stream is redirected'
never\t'Never style'"
complete -c webp -n "__fish_webp_using_subcommand decode" -l threads -d 'Worker threads for parallel work; 0 (the default) uses one per core' -r
complete -c webp -n "__fish_webp_using_subcommand decode" -s v -l verbose -d 'Print per-stage detail on stderr'
complete -c webp -n "__fish_webp_using_subcommand decode" -l quiet -d 'Suppress all non-error output'
complete -c webp -n "__fish_webp_using_subcommand decode" -l dry-run -d 'Report what would be written, without encoding or writing anything'
complete -c webp -n "__fish_webp_using_subcommand decode" -s h -l help -d 'Print help (see more with \'--help\')'
complete -c webp -n "__fish_webp_using_subcommand encode" -s o -l output -d 'Output `.webp` file; `-` writes stdout' -r -F
complete -c webp -n "__fish_webp_using_subcommand encode" -l input-format -d 'Input format; defaults to the extension, else the magic bytes, else raw' -r -f -a "png\t'PNG (any color type; normalized to RGBA8)'
ppm\t'Netpbm binary PPM (`P6`, RGB)'
pam\t'Netpbm binary PAM (`P7`, RGBA)'
jpeg\t'JPEG (decoded to RGBA8; needs the `formats` feature)'
gif\t'GIF (first frame as a still; whole-file animation is a separate path)'
tiff\t'TIFF (decoded to RGBA8; needs the `formats` feature)'
bmp\t'BMP (decoded to RGBA8; needs the `formats` feature)'
raw\t'Raw row-major pixels; requires `--width`/`--height`/`--layout`'"
complete -c webp -n "__fish_webp_using_subcommand encode" -l width -d 'Raw-input width in pixels (required for raw input)' -r
complete -c webp -n "__fish_webp_using_subcommand encode" -l height -d 'Raw-input height in pixels (required for raw input)' -r
complete -c webp -n "__fish_webp_using_subcommand encode" -l layout -d 'Byte order for raw input only' -r -f -a "rgba8\t'`R, G, B, A`'
argb8\t'`A, R, G, B`'
bgra8\t'`B, G, R, A`'"
complete -c webp -n "__fish_webp_using_subcommand encode" -s m -l method -d 'Encoder effort [default: auto, or from env/config]' -r -f -a "auto\t'Adapt the search depth to the image\'s content and size (the default)'
fast\t'Fastest: the shallowest fixed search'
best\t'Smallest: the deepest fixed search'"
complete -c webp -n "__fish_webp_using_subcommand encode" -s q -l quality -d 'Lossy quality 0-100 (higher = larger, closer to source); selects --lossy' -r
complete -c webp -n "__fish_webp_using_subcommand encode" -l near-lossless -d 'Near-lossless preprocessing 0-100 (lower = stronger; implies lossless)' -r
complete -c webp -n "__fish_webp_using_subcommand encode" -l crop -d 'Crop before encoding: `x,y,width,height` in pixels (applied before --resize)' -r
complete -c webp -n "__fish_webp_using_subcommand encode" -l resize -d 'Resize before encoding: `WxH` (use 0 on one axis to keep aspect)' -r
complete -c webp -n "__fish_webp_using_subcommand encode" -l target-size -d 'Target output size, e.g. `200k` or `2M`, found by searching lossy quality' -r
complete -c webp -n "__fish_webp_using_subcommand encode" -l min-psnr -d 'Target reconstruction PSNR floor in dB (lossy only; pairs with --target-size)' -r
complete -c webp -n "__fish_webp_using_subcommand encode" -l metadata -d 'Metadata to embed: all,none,icc,exif,xmp (default: all — kinder than cwebp)' -r -f -a "all\t'Keep ICC, Exif, and XMP'
none\t'Strip everything (a bare `VP8L` output)'
icc\t'Keep the ICC color profile'
exif\t'Keep Exif'
xmp\t'Keep XMP'"
complete -c webp -n "__fish_webp_using_subcommand encode" -l kmax -d 'With --optimize: force a keyframe at least every N frames (gif2webp -kmax; 0 = only the first)' -r
complete -c webp -n "__fish_webp_using_subcommand encode" -l kmin -d 'With --optimize: never place keyframes closer than N frames apart (gif2webp -kmin)' -r
complete -c webp -n "__fish_webp_using_subcommand encode" -l color -d 'auto, always, or never [default: auto, or from env/config]' -r -f -a "auto\t'Style only when the stream is a terminal that wants it (the default)'
always\t'Style even when the stream is redirected'
never\t'Never style'"
complete -c webp -n "__fish_webp_using_subcommand encode" -l threads -d 'Worker threads for parallel work; 0 (the default) uses one per core' -r
complete -c webp -n "__fish_webp_using_subcommand encode" -l lossless -d 'Force lossless (VP8L). The default is source-derived: JPEG → lossy, else lossless'
complete -c webp -n "__fish_webp_using_subcommand encode" -l lossy -d 'Encode lossily (VP8) instead of losslessly (VP8L)'
complete -c webp -n "__fish_webp_using_subcommand encode" -l optimize -d 'Inter-frame optimize a GIF animation: encode each frame as a minimal delta'
complete -c webp -n "__fish_webp_using_subcommand encode" -l mixed -d 'With --optimize: trial each frame lossy and lossless, keep the smaller (gif2webp -mixed)'
complete -c webp -n "__fish_webp_using_subcommand encode" -l min-size -d 'With --optimize: exhaustively search each frame\'s rect/blend/dispose/codec (gif2webp `-min_size`)'
complete -c webp -n "__fish_webp_using_subcommand encode" -s v -l verbose -d 'Print per-stage detail on stderr'
complete -c webp -n "__fish_webp_using_subcommand encode" -l quiet -d 'Suppress all non-error output'
complete -c webp -n "__fish_webp_using_subcommand encode" -l dry-run -d 'Report what would be written, without encoding or writing anything'
complete -c webp -n "__fish_webp_using_subcommand encode" -s h -l help -d 'Print help (see more with \'--help\')'
complete -c webp -n "__fish_webp_using_subcommand convert" -s o -l output -d 'Output directory (created outputs are `<stem>.webp`); default: beside input' -r -F
complete -c webp -n "__fish_webp_using_subcommand convert" -s m -l method -d 'Encoder effort (ignored with --optimize) [default: auto, or from env/config]' -r -f -a "auto\t'Adapt the search depth to the image\'s content and size (the default)'
fast\t'Fastest: the shallowest fixed search'
best\t'Smallest: the deepest fixed search'"
complete -c webp -n "__fish_webp_using_subcommand convert" -s q -l quality -d 'Lossy quality 0-100 (higher = larger, closer to source); selects --lossy' -r
complete -c webp -n "__fish_webp_using_subcommand convert" -l near-lossless -d 'Near-lossless preprocessing 0-100 (lower = stronger; implies lossless)' -r
complete -c webp -n "__fish_webp_using_subcommand convert" -l metadata -d 'Metadata to embed: all,none,icc,exif,xmp (default: all)' -r -f -a "all\t'Keep ICC, Exif, and XMP'
none\t'Strip everything (a bare `VP8L` output)'
icc\t'Keep the ICC color profile'
exif\t'Keep Exif'
xmp\t'Keep XMP'"
complete -c webp -n "__fish_webp_using_subcommand convert" -l color -d 'auto, always, or never [default: auto, or from env/config]' -r -f -a "auto\t'Style only when the stream is a terminal that wants it (the default)'
always\t'Style even when the stream is redirected'
never\t'Never style'"
complete -c webp -n "__fish_webp_using_subcommand convert" -l threads -d 'Worker threads for parallel work; 0 (the default) uses one per core' -r
complete -c webp -n "__fish_webp_using_subcommand convert" -l lossless -d 'Force lossless (VP8L). The default is source-derived: JPEG → lossy, else lossless'
complete -c webp -n "__fish_webp_using_subcommand convert" -l lossy -d 'Encode lossily (VP8) instead of losslessly (VP8L)'
complete -c webp -n "__fish_webp_using_subcommand convert" -l optimize -d 'Try every lossless effort level and keep the smallest output'
complete -c webp -n "__fish_webp_using_subcommand convert" -s r -l recursive -d 'Recurse into subdirectories'
complete -c webp -n "__fish_webp_using_subcommand convert" -l force -d 'Overwrite an existing output'
complete -c webp -n "__fish_webp_using_subcommand convert" -l no-clobber -d 'Skip an input whose `.webp` output exists (still exits 0)'
complete -c webp -n "__fish_webp_using_subcommand convert" -s v -l verbose -d 'Print per-stage detail on stderr'
complete -c webp -n "__fish_webp_using_subcommand convert" -l quiet -d 'Suppress all non-error output'
complete -c webp -n "__fish_webp_using_subcommand convert" -l dry-run -d 'Report what would be written, without encoding or writing anything'
complete -c webp -n "__fish_webp_using_subcommand convert" -s h -l help -d 'Print help (see more with \'--help\')'
complete -c webp -n "__fish_webp_using_subcommand animate" -s o -l output -d 'Output `.webp` file; `-` writes stdout' -r -F
complete -c webp -n "__fish_webp_using_subcommand animate" -l delay -d 'Per-frame delay in ms: one value for every frame, or a comma list (`40,40,80`)' -r
complete -c webp -n "__fish_webp_using_subcommand animate" -l loop -d 'Loop count; `0` (the default) loops forever' -r
complete -c webp -n "__fish_webp_using_subcommand animate" -l bgcolor -d 'Advisory background color as `RRGGBBAA` hex (e.g. `ffffffff`)' -r
complete -c webp -n "__fish_webp_using_subcommand animate" -l dispose -d 'Disposal method applied to every frame' -r -f -a "keep\t'Leave the canvas as-is for the next frame (the default)'
background\t'Clear the frame\'s rectangle to transparent before the next frame'"
complete -c webp -n "__fish_webp_using_subcommand animate" -l blend -d 'Blend method applied to every frame' -r -f -a "blend\t'Alpha-blend the frame over the canvas (the default)'
overwrite\t'Overwrite the frame\'s rectangle, ignoring what is underneath'"
complete -c webp -n "__fish_webp_using_subcommand animate" -l lossy -d 'Encode frames lossily (VP8) at this quality 0-100; the default is lossless' -r
complete -c webp -n "__fish_webp_using_subcommand animate" -l canvas -d 'Canvas size as `WxH`; defaults to the largest frame' -r
complete -c webp -n "__fish_webp_using_subcommand animate" -s m -l method -d 'Encoder effort [default: auto, or from env/config]' -r -f -a "auto\t'Adapt the search depth to the image\'s content and size (the default)'
fast\t'Fastest: the shallowest fixed search'
best\t'Smallest: the deepest fixed search'"
complete -c webp -n "__fish_webp_using_subcommand animate" -l input-format -d 'Force the input format instead of sniffing each file' -r -f -a "png\t'PNG (any color type; normalized to RGBA8)'
ppm\t'Netpbm binary PPM (`P6`, RGB)'
pam\t'Netpbm binary PAM (`P7`, RGBA)'
jpeg\t'JPEG (decoded to RGBA8; needs the `formats` feature)'
gif\t'GIF (first frame as a still; whole-file animation is a separate path)'
tiff\t'TIFF (decoded to RGBA8; needs the `formats` feature)'
bmp\t'BMP (decoded to RGBA8; needs the `formats` feature)'
raw\t'Raw row-major pixels; requires `--width`/`--height`/`--layout`'"
complete -c webp -n "__fish_webp_using_subcommand animate" -l kmax -d 'With --optimize: force a keyframe at least every N frames (gif2webp -kmax; 0 = only the first)' -r
complete -c webp -n "__fish_webp_using_subcommand animate" -l kmin -d 'With --optimize: never place keyframes closer than N frames apart (gif2webp -kmin)' -r
complete -c webp -n "__fish_webp_using_subcommand animate" -l color -d 'auto, always, or never [default: auto, or from env/config]' -r -f -a "auto\t'Style only when the stream is a terminal that wants it (the default)'
always\t'Style even when the stream is redirected'
never\t'Never style'"
complete -c webp -n "__fish_webp_using_subcommand animate" -l threads -d 'Worker threads for parallel work; 0 (the default) uses one per core' -r
complete -c webp -n "__fish_webp_using_subcommand animate" -l optimize -d 'Inter-frame optimize: encode each frame as a minimal delta against the canvas'
complete -c webp -n "__fish_webp_using_subcommand animate" -l mixed -d 'With --optimize: trial each frame lossy and lossless, keep the smaller (gif2webp -mixed)'
complete -c webp -n "__fish_webp_using_subcommand animate" -l min-size -d 'With --optimize: exhaustively search each frame\'s rect/blend/dispose/codec (gif2webp `-min_size`)'
complete -c webp -n "__fish_webp_using_subcommand animate" -s v -l verbose -d 'Print per-stage detail on stderr'
complete -c webp -n "__fish_webp_using_subcommand animate" -l quiet -d 'Suppress all non-error output'
complete -c webp -n "__fish_webp_using_subcommand animate" -l dry-run -d 'Report what would be written, without encoding or writing anything'
complete -c webp -n "__fish_webp_using_subcommand animate" -s h -l help -d 'Print help (see more with \'--help\')'
complete -c webp -n "__fish_webp_using_subcommand mux; and not __fish_seen_subcommand_from get-frame set remove insert replace help" -l color -d 'auto, always, or never [default: auto, or from env/config]' -r -f -a "auto\t'Style only when the stream is a terminal that wants it (the default)'
always\t'Style even when the stream is redirected'
never\t'Never style'"
complete -c webp -n "__fish_webp_using_subcommand mux; and not __fish_seen_subcommand_from get-frame set remove insert replace help" -l threads -d 'Worker threads for parallel work; 0 (the default) uses one per core' -r
complete -c webp -n "__fish_webp_using_subcommand mux; and not __fish_seen_subcommand_from get-frame set remove insert replace help" -s v -l verbose -d 'Print per-stage detail on stderr'
complete -c webp -n "__fish_webp_using_subcommand mux; and not __fish_seen_subcommand_from get-frame set remove insert replace help" -l quiet -d 'Suppress all non-error output'
complete -c webp -n "__fish_webp_using_subcommand mux; and not __fish_seen_subcommand_from get-frame set remove insert replace help" -l dry-run -d 'Report what would be written, without encoding or writing anything'
complete -c webp -n "__fish_webp_using_subcommand mux; and not __fish_seen_subcommand_from get-frame set remove insert replace help" -s h -l help -d 'Print help (see more with \'--help\')'
complete -c webp -n "__fish_webp_using_subcommand mux; and not __fish_seen_subcommand_from get-frame set remove insert replace help" -f -a "get-frame" -d 'Extract one frame as a standalone still WebP (bytes copied verbatim)'
complete -c webp -n "__fish_webp_using_subcommand mux; and not __fish_seen_subcommand_from get-frame set remove insert replace help" -f -a "set" -d 'Rewrite the loop count and/or background color'
complete -c webp -n "__fish_webp_using_subcommand mux; and not __fish_seen_subcommand_from get-frame set remove insert replace help" -f -a "remove" -d 'Remove one frame, rebuilding the frame list'
complete -c webp -n "__fish_webp_using_subcommand mux; and not __fish_seen_subcommand_from get-frame set remove insert replace help" -f -a "insert" -d 'Insert a still WebP as a new frame (its image bytes copied verbatim)'
complete -c webp -n "__fish_webp_using_subcommand mux; and not __fish_seen_subcommand_from get-frame set remove insert replace help" -f -a "replace" -d 'Replace one frame\'s image with a still WebP, keeping its placement/timing'
complete -c webp -n "__fish_webp_using_subcommand mux; and not __fish_seen_subcommand_from get-frame set remove insert replace help" -f -a "help" -d 'Print this message or the help of the given subcommand(s)'
complete -c webp -n "__fish_webp_using_subcommand mux; and __fish_seen_subcommand_from get-frame" -s o -l output -d 'Output `.webp` file; `-` writes stdout' -r -F
complete -c webp -n "__fish_webp_using_subcommand mux; and __fish_seen_subcommand_from get-frame" -l color -d 'auto, always, or never [default: auto, or from env/config]' -r -f -a "auto\t'Style only when the stream is a terminal that wants it (the default)'
always\t'Style even when the stream is redirected'
never\t'Never style'"
complete -c webp -n "__fish_webp_using_subcommand mux; and __fish_seen_subcommand_from get-frame" -l threads -d 'Worker threads for parallel work; 0 (the default) uses one per core' -r
complete -c webp -n "__fish_webp_using_subcommand mux; and __fish_seen_subcommand_from get-frame" -s v -l verbose -d 'Print per-stage detail on stderr'
complete -c webp -n "__fish_webp_using_subcommand mux; and __fish_seen_subcommand_from get-frame" -l quiet -d 'Suppress all non-error output'
complete -c webp -n "__fish_webp_using_subcommand mux; and __fish_seen_subcommand_from get-frame" -l dry-run -d 'Report what would be written, without encoding or writing anything'
complete -c webp -n "__fish_webp_using_subcommand mux; and __fish_seen_subcommand_from get-frame" -s h -l help -d 'Print help (see more with \'--help\')'
complete -c webp -n "__fish_webp_using_subcommand mux; and __fish_seen_subcommand_from set" -s o -l output -d 'Output `.webp` file; `-` writes stdout' -r -F
complete -c webp -n "__fish_webp_using_subcommand mux; and __fish_seen_subcommand_from set" -l loop -d 'New loop count (`0` loops forever)' -r
complete -c webp -n "__fish_webp_using_subcommand mux; and __fish_seen_subcommand_from set" -l bgcolor -d 'New background color as `RRGGBBAA` hex' -r
complete -c webp -n "__fish_webp_using_subcommand mux; and __fish_seen_subcommand_from set" -l color -d 'auto, always, or never [default: auto, or from env/config]' -r -f -a "auto\t'Style only when the stream is a terminal that wants it (the default)'
always\t'Style even when the stream is redirected'
never\t'Never style'"
complete -c webp -n "__fish_webp_using_subcommand mux; and __fish_seen_subcommand_from set" -l threads -d 'Worker threads for parallel work; 0 (the default) uses one per core' -r
complete -c webp -n "__fish_webp_using_subcommand mux; and __fish_seen_subcommand_from set" -s v -l verbose -d 'Print per-stage detail on stderr'
complete -c webp -n "__fish_webp_using_subcommand mux; and __fish_seen_subcommand_from set" -l quiet -d 'Suppress all non-error output'
complete -c webp -n "__fish_webp_using_subcommand mux; and __fish_seen_subcommand_from set" -l dry-run -d 'Report what would be written, without encoding or writing anything'
complete -c webp -n "__fish_webp_using_subcommand mux; and __fish_seen_subcommand_from set" -s h -l help -d 'Print help (see more with \'--help\')'
complete -c webp -n "__fish_webp_using_subcommand mux; and __fish_seen_subcommand_from remove" -s o -l output -d 'Output `.webp` file; `-` writes stdout' -r -F
complete -c webp -n "__fish_webp_using_subcommand mux; and __fish_seen_subcommand_from remove" -l color -d 'auto, always, or never [default: auto, or from env/config]' -r -f -a "auto\t'Style only when the stream is a terminal that wants it (the default)'
always\t'Style even when the stream is redirected'
never\t'Never style'"
complete -c webp -n "__fish_webp_using_subcommand mux; and __fish_seen_subcommand_from remove" -l threads -d 'Worker threads for parallel work; 0 (the default) uses one per core' -r
complete -c webp -n "__fish_webp_using_subcommand mux; and __fish_seen_subcommand_from remove" -s v -l verbose -d 'Print per-stage detail on stderr'
complete -c webp -n "__fish_webp_using_subcommand mux; and __fish_seen_subcommand_from remove" -l quiet -d 'Suppress all non-error output'
complete -c webp -n "__fish_webp_using_subcommand mux; and __fish_seen_subcommand_from remove" -l dry-run -d 'Report what would be written, without encoding or writing anything'
complete -c webp -n "__fish_webp_using_subcommand mux; and __fish_seen_subcommand_from remove" -s h -l help -d 'Print help (see more with \'--help\')'
complete -c webp -n "__fish_webp_using_subcommand mux; and __fish_seen_subcommand_from insert" -s o -l output -d 'Output `.webp` file; `-` writes stdout' -r -F
complete -c webp -n "__fish_webp_using_subcommand mux; and __fish_seen_subcommand_from insert" -l at -d '0-based index to insert at; defaults to appending at the end' -r
complete -c webp -n "__fish_webp_using_subcommand mux; and __fish_seen_subcommand_from insert" -l delay -d 'The new frame\'s display duration in ms' -r
complete -c webp -n "__fish_webp_using_subcommand mux; and __fish_seen_subcommand_from insert" -l blend -d 'The new frame\'s blend method' -r -f -a "blend\t'Alpha-blend the frame over the canvas (the default)'
overwrite\t'Overwrite the frame\'s rectangle, ignoring what is underneath'"
complete -c webp -n "__fish_webp_using_subcommand mux; and __fish_seen_subcommand_from insert" -l dispose -d 'The new frame\'s disposal method' -r -f -a "keep\t'Leave the canvas as-is for the next frame (the default)'
background\t'Clear the frame\'s rectangle to transparent before the next frame'"
complete -c webp -n "__fish_webp_using_subcommand mux; and __fish_seen_subcommand_from insert" -l color -d 'auto, always, or never [default: auto, or from env/config]' -r -f -a "auto\t'Style only when the stream is a terminal that wants it (the default)'
always\t'Style even when the stream is redirected'
never\t'Never style'"
complete -c webp -n "__fish_webp_using_subcommand mux; and __fish_seen_subcommand_from insert" -l threads -d 'Worker threads for parallel work; 0 (the default) uses one per core' -r
complete -c webp -n "__fish_webp_using_subcommand mux; and __fish_seen_subcommand_from insert" -s v -l verbose -d 'Print per-stage detail on stderr'
complete -c webp -n "__fish_webp_using_subcommand mux; and __fish_seen_subcommand_from insert" -l quiet -d 'Suppress all non-error output'
complete -c webp -n "__fish_webp_using_subcommand mux; and __fish_seen_subcommand_from insert" -l dry-run -d 'Report what would be written, without encoding or writing anything'
complete -c webp -n "__fish_webp_using_subcommand mux; and __fish_seen_subcommand_from insert" -s h -l help -d 'Print help (see more with \'--help\')'
complete -c webp -n "__fish_webp_using_subcommand mux; and __fish_seen_subcommand_from replace" -s o -l output -d 'Output `.webp` file; `-` writes stdout' -r -F
complete -c webp -n "__fish_webp_using_subcommand mux; and __fish_seen_subcommand_from replace" -l at -d '0-based index of the frame to replace' -r
complete -c webp -n "__fish_webp_using_subcommand mux; and __fish_seen_subcommand_from replace" -l color -d 'auto, always, or never [default: auto, or from env/config]' -r -f -a "auto\t'Style only when the stream is a terminal that wants it (the default)'
always\t'Style even when the stream is redirected'
never\t'Never style'"
complete -c webp -n "__fish_webp_using_subcommand mux; and __fish_seen_subcommand_from replace" -l threads -d 'Worker threads for parallel work; 0 (the default) uses one per core' -r
complete -c webp -n "__fish_webp_using_subcommand mux; and __fish_seen_subcommand_from replace" -s v -l verbose -d 'Print per-stage detail on stderr'
complete -c webp -n "__fish_webp_using_subcommand mux; and __fish_seen_subcommand_from replace" -l quiet -d 'Suppress all non-error output'
complete -c webp -n "__fish_webp_using_subcommand mux; and __fish_seen_subcommand_from replace" -l dry-run -d 'Report what would be written, without encoding or writing anything'
complete -c webp -n "__fish_webp_using_subcommand mux; and __fish_seen_subcommand_from replace" -s h -l help -d 'Print help (see more with \'--help\')'
complete -c webp -n "__fish_webp_using_subcommand mux; and __fish_seen_subcommand_from help" -f -a "get-frame" -d 'Extract one frame as a standalone still WebP (bytes copied verbatim)'
complete -c webp -n "__fish_webp_using_subcommand mux; and __fish_seen_subcommand_from help" -f -a "set" -d 'Rewrite the loop count and/or background color'
complete -c webp -n "__fish_webp_using_subcommand mux; and __fish_seen_subcommand_from help" -f -a "remove" -d 'Remove one frame, rebuilding the frame list'
complete -c webp -n "__fish_webp_using_subcommand mux; and __fish_seen_subcommand_from help" -f -a "insert" -d 'Insert a still WebP as a new frame (its image bytes copied verbatim)'
complete -c webp -n "__fish_webp_using_subcommand mux; and __fish_seen_subcommand_from help" -f -a "replace" -d 'Replace one frame\'s image with a still WebP, keeping its placement/timing'
complete -c webp -n "__fish_webp_using_subcommand mux; and __fish_seen_subcommand_from help" -f -a "help" -d 'Print this message or the help of the given subcommand(s)'
complete -c webp -n "__fish_webp_using_subcommand info" -l color -d 'auto, always, or never [default: auto, or from env/config]' -r -f -a "auto\t'Style only when the stream is a terminal that wants it (the default)'
always\t'Style even when the stream is redirected'
never\t'Never style'"
complete -c webp -n "__fish_webp_using_subcommand info" -l threads -d 'Worker threads for parallel work; 0 (the default) uses one per core' -r
complete -c webp -n "__fish_webp_using_subcommand info" -l json -d 'Print the report as JSON instead of text'
complete -c webp -n "__fish_webp_using_subcommand info" -s v -l verbose -d 'Print per-stage detail on stderr'
complete -c webp -n "__fish_webp_using_subcommand info" -l quiet -d 'Suppress all non-error output'
complete -c webp -n "__fish_webp_using_subcommand info" -l dry-run -d 'Report what would be written, without encoding or writing anything'
complete -c webp -n "__fish_webp_using_subcommand info" -s h -l help -d 'Print help (see more with \'--help\')'
complete -c webp -n "__fish_webp_using_subcommand meta; and not __fish_seen_subcommand_from show set strip help" -l color -d 'auto, always, or never [default: auto, or from env/config]' -r -f -a "auto\t'Style only when the stream is a terminal that wants it (the default)'
always\t'Style even when the stream is redirected'
never\t'Never style'"
complete -c webp -n "__fish_webp_using_subcommand meta; and not __fish_seen_subcommand_from show set strip help" -l threads -d 'Worker threads for parallel work; 0 (the default) uses one per core' -r
complete -c webp -n "__fish_webp_using_subcommand meta; and not __fish_seen_subcommand_from show set strip help" -s v -l verbose -d 'Print per-stage detail on stderr'
complete -c webp -n "__fish_webp_using_subcommand meta; and not __fish_seen_subcommand_from show set strip help" -l quiet -d 'Suppress all non-error output'
complete -c webp -n "__fish_webp_using_subcommand meta; and not __fish_seen_subcommand_from show set strip help" -l dry-run -d 'Report what would be written, without encoding or writing anything'
complete -c webp -n "__fish_webp_using_subcommand meta; and not __fish_seen_subcommand_from show set strip help" -s h -l help -d 'Print help (see more with \'--help\')'
complete -c webp -n "__fish_webp_using_subcommand meta; and not __fish_seen_subcommand_from show set strip help" -f -a "show" -d 'Show the ICC/Exif/XMP a WebP carries (kinds and byte sizes)'
complete -c webp -n "__fish_webp_using_subcommand meta; and not __fish_seen_subcommand_from show set strip help" -f -a "set" -d 'Write a copy with ICC/Exif/XMP set from files (unspecified kinds are kept)'
complete -c webp -n "__fish_webp_using_subcommand meta; and not __fish_seen_subcommand_from show set strip help" -f -a "strip" -d 'Write a copy with all sidecar metadata removed'
complete -c webp -n "__fish_webp_using_subcommand meta; and not __fish_seen_subcommand_from show set strip help" -f -a "help" -d 'Print this message or the help of the given subcommand(s)'
complete -c webp -n "__fish_webp_using_subcommand meta; and __fish_seen_subcommand_from show" -l color -d 'auto, always, or never [default: auto, or from env/config]' -r -f -a "auto\t'Style only when the stream is a terminal that wants it (the default)'
always\t'Style even when the stream is redirected'
never\t'Never style'"
complete -c webp -n "__fish_webp_using_subcommand meta; and __fish_seen_subcommand_from show" -l threads -d 'Worker threads for parallel work; 0 (the default) uses one per core' -r
complete -c webp -n "__fish_webp_using_subcommand meta; and __fish_seen_subcommand_from show" -l json -d 'Print the metadata as JSON instead of text'
complete -c webp -n "__fish_webp_using_subcommand meta; and __fish_seen_subcommand_from show" -s v -l verbose -d 'Print per-stage detail on stderr'
complete -c webp -n "__fish_webp_using_subcommand meta; and __fish_seen_subcommand_from show" -l quiet -d 'Suppress all non-error output'
complete -c webp -n "__fish_webp_using_subcommand meta; and __fish_seen_subcommand_from show" -l dry-run -d 'Report what would be written, without encoding or writing anything'
complete -c webp -n "__fish_webp_using_subcommand meta; and __fish_seen_subcommand_from show" -s h -l help -d 'Print help (see more with \'--help\')'
complete -c webp -n "__fish_webp_using_subcommand meta; and __fish_seen_subcommand_from set" -s o -l output -d 'Output `.webp` file; `-` writes stdout' -r -F
complete -c webp -n "__fish_webp_using_subcommand meta; and __fish_seen_subcommand_from set" -l icc -d 'Set the ICC color profile from this file' -r -F
complete -c webp -n "__fish_webp_using_subcommand meta; and __fish_seen_subcommand_from set" -l exif -d 'Set the Exif metadata from this file' -r -F
complete -c webp -n "__fish_webp_using_subcommand meta; and __fish_seen_subcommand_from set" -l xmp -d 'Set the XMP metadata from this file' -r -F
complete -c webp -n "__fish_webp_using_subcommand meta; and __fish_seen_subcommand_from set" -l color -d 'auto, always, or never [default: auto, or from env/config]' -r -f -a "auto\t'Style only when the stream is a terminal that wants it (the default)'
always\t'Style even when the stream is redirected'
never\t'Never style'"
complete -c webp -n "__fish_webp_using_subcommand meta; and __fish_seen_subcommand_from set" -l threads -d 'Worker threads for parallel work; 0 (the default) uses one per core' -r
complete -c webp -n "__fish_webp_using_subcommand meta; and __fish_seen_subcommand_from set" -s v -l verbose -d 'Print per-stage detail on stderr'
complete -c webp -n "__fish_webp_using_subcommand meta; and __fish_seen_subcommand_from set" -l quiet -d 'Suppress all non-error output'
complete -c webp -n "__fish_webp_using_subcommand meta; and __fish_seen_subcommand_from set" -l dry-run -d 'Report what would be written, without encoding or writing anything'
complete -c webp -n "__fish_webp_using_subcommand meta; and __fish_seen_subcommand_from set" -s h -l help -d 'Print help (see more with \'--help\')'
complete -c webp -n "__fish_webp_using_subcommand meta; and __fish_seen_subcommand_from strip" -s o -l output -d 'Output `.webp` file; `-` writes stdout' -r -F
complete -c webp -n "__fish_webp_using_subcommand meta; and __fish_seen_subcommand_from strip" -l color -d 'auto, always, or never [default: auto, or from env/config]' -r -f -a "auto\t'Style only when the stream is a terminal that wants it (the default)'
always\t'Style even when the stream is redirected'
never\t'Never style'"
complete -c webp -n "__fish_webp_using_subcommand meta; and __fish_seen_subcommand_from strip" -l threads -d 'Worker threads for parallel work; 0 (the default) uses one per core' -r
complete -c webp -n "__fish_webp_using_subcommand meta; and __fish_seen_subcommand_from strip" -s v -l verbose -d 'Print per-stage detail on stderr'
complete -c webp -n "__fish_webp_using_subcommand meta; and __fish_seen_subcommand_from strip" -l quiet -d 'Suppress all non-error output'
complete -c webp -n "__fish_webp_using_subcommand meta; and __fish_seen_subcommand_from strip" -l dry-run -d 'Report what would be written, without encoding or writing anything'
complete -c webp -n "__fish_webp_using_subcommand meta; and __fish_seen_subcommand_from strip" -s h -l help -d 'Print help (see more with \'--help\')'
complete -c webp -n "__fish_webp_using_subcommand meta; and __fish_seen_subcommand_from help" -f -a "show" -d 'Show the ICC/Exif/XMP a WebP carries (kinds and byte sizes)'
complete -c webp -n "__fish_webp_using_subcommand meta; and __fish_seen_subcommand_from help" -f -a "set" -d 'Write a copy with ICC/Exif/XMP set from files (unspecified kinds are kept)'
complete -c webp -n "__fish_webp_using_subcommand meta; and __fish_seen_subcommand_from help" -f -a "strip" -d 'Write a copy with all sidecar metadata removed'
complete -c webp -n "__fish_webp_using_subcommand meta; and __fish_seen_subcommand_from help" -f -a "help" -d 'Print this message or the help of the given subcommand(s)'
complete -c webp -n "__fish_webp_using_subcommand diff" -l min-psnr -d 'Fail (exit 1) if the RGB PSNR is below this many decibels' -r
complete -c webp -n "__fish_webp_using_subcommand diff" -l color -d 'auto, always, or never [default: auto, or from env/config]' -r -f -a "auto\t'Style only when the stream is a terminal that wants it (the default)'
always\t'Style even when the stream is redirected'
never\t'Never style'"
complete -c webp -n "__fish_webp_using_subcommand diff" -l threads -d 'Worker threads for parallel work; 0 (the default) uses one per core' -r
complete -c webp -n "__fish_webp_using_subcommand diff" -l json -d 'Print the comparison as JSON instead of text'
complete -c webp -n "__fish_webp_using_subcommand diff" -s v -l verbose -d 'Print per-stage detail on stderr'
complete -c webp -n "__fish_webp_using_subcommand diff" -l quiet -d 'Suppress all non-error output'
complete -c webp -n "__fish_webp_using_subcommand diff" -l dry-run -d 'Report what would be written, without encoding or writing anything'
complete -c webp -n "__fish_webp_using_subcommand diff" -s h -l help -d 'Print help (see more with \'--help\')'
complete -c webp -n "__fish_webp_using_subcommand doctor" -l color -d 'auto, always, or never [default: auto, or from env/config]' -r -f -a "auto\t'Style only when the stream is a terminal that wants it (the default)'
always\t'Style even when the stream is redirected'
never\t'Never style'"
complete -c webp -n "__fish_webp_using_subcommand doctor" -l threads -d 'Worker threads for parallel work; 0 (the default) uses one per core' -r
complete -c webp -n "__fish_webp_using_subcommand doctor" -s v -l verbose -d 'Print per-stage detail on stderr'
complete -c webp -n "__fish_webp_using_subcommand doctor" -l quiet -d 'Suppress all non-error output'
complete -c webp -n "__fish_webp_using_subcommand doctor" -l dry-run -d 'Report what would be written, without encoding or writing anything'
complete -c webp -n "__fish_webp_using_subcommand doctor" -s h -l help -d 'Print help (see more with \'--help\')'
complete -c webp -n "__fish_webp_using_subcommand config; and not __fish_seen_subcommand_from get help" -l quality -d 'Override: lossy quality 0-100' -r
complete -c webp -n "__fish_webp_using_subcommand config; and not __fish_seen_subcommand_from get help" -l effort -d 'Override: encoder effort' -r -f -a "auto\t'Adapt the search depth to the image\'s content and size (the default)'
fast\t'Fastest: the shallowest fixed search'
best\t'Smallest: the deepest fixed search'"
complete -c webp -n "__fish_webp_using_subcommand config; and not __fish_seen_subcommand_from get help" -l codec -d 'Override: lossless or lossy' -r -f -a "lossless\t'Lossless (VP8L)'
lossy\t'Lossy (VP8)'"
complete -c webp -n "__fish_webp_using_subcommand config; and not __fish_seen_subcommand_from get help" -l metadata -d 'Override: metadata to carry (all,none,icc,exif,xmp)' -r -f -a "all\t'Keep ICC, Exif, and XMP'
none\t'Strip everything (a bare `VP8L` output)'
icc\t'Keep the ICC color profile'
exif\t'Keep Exif'
xmp\t'Keep XMP'"
complete -c webp -n "__fish_webp_using_subcommand config; and not __fish_seen_subcommand_from get help" -l threads -d 'Override: worker threads (0 = one per core)' -r
complete -c webp -n "__fish_webp_using_subcommand config; and not __fish_seen_subcommand_from get help" -l max-pixels -d 'Override: decode pixel cap (N, 300M, 2G, or none)' -r
complete -c webp -n "__fish_webp_using_subcommand config; and not __fish_seen_subcommand_from get help" -l color -d 'auto, always, or never [default: auto, or from env/config]' -r -f -a "auto\t'Style only when the stream is a terminal that wants it (the default)'
always\t'Style even when the stream is redirected'
never\t'Never style'"
complete -c webp -n "__fish_webp_using_subcommand config; and not __fish_seen_subcommand_from get help" -l json -d 'Print the resolved settings as JSON (stable key order)'
complete -c webp -n "__fish_webp_using_subcommand config; and not __fish_seen_subcommand_from get help" -l template -d 'Print a commented `webp.toml` template to stdout'
complete -c webp -n "__fish_webp_using_subcommand config; and not __fish_seen_subcommand_from get help" -s v -l verbose -d 'Print per-stage detail on stderr'
complete -c webp -n "__fish_webp_using_subcommand config; and not __fish_seen_subcommand_from get help" -l quiet -d 'Suppress all non-error output'
complete -c webp -n "__fish_webp_using_subcommand config; and not __fish_seen_subcommand_from get help" -l dry-run -d 'Report what would be written, without encoding or writing anything'
complete -c webp -n "__fish_webp_using_subcommand config; and not __fish_seen_subcommand_from get help" -s h -l help -d 'Print help (see more with \'--help\')'
complete -c webp -n "__fish_webp_using_subcommand config; and not __fish_seen_subcommand_from get help" -f -a "get" -d 'Print a single setting\'s resolved value, with nothing else'
complete -c webp -n "__fish_webp_using_subcommand config; and not __fish_seen_subcommand_from get help" -f -a "help" -d 'Print this message or the help of the given subcommand(s)'
complete -c webp -n "__fish_webp_using_subcommand config; and __fish_seen_subcommand_from get" -l color -d 'auto, always, or never [default: auto, or from env/config]' -r -f -a "auto\t'Style only when the stream is a terminal that wants it (the default)'
always\t'Style even when the stream is redirected'
never\t'Never style'"
complete -c webp -n "__fish_webp_using_subcommand config; and __fish_seen_subcommand_from get" -s v -l verbose -d 'Print per-stage detail on stderr'
complete -c webp -n "__fish_webp_using_subcommand config; and __fish_seen_subcommand_from get" -l quiet -d 'Suppress all non-error output'
complete -c webp -n "__fish_webp_using_subcommand config; and __fish_seen_subcommand_from get" -l dry-run -d 'Report what would be written, without encoding or writing anything'
complete -c webp -n "__fish_webp_using_subcommand config; and __fish_seen_subcommand_from get" -s h -l help -d 'Print help (see more with \'--help\')'
complete -c webp -n "__fish_webp_using_subcommand config; and __fish_seen_subcommand_from help" -f -a "get" -d 'Print a single setting\'s resolved value, with nothing else'
complete -c webp -n "__fish_webp_using_subcommand config; and __fish_seen_subcommand_from help" -f -a "help" -d 'Print this message or the help of the given subcommand(s)'
complete -c webp -n "__fish_webp_using_subcommand explain" -l color -d 'auto, always, or never [default: auto, or from env/config]' -r -f -a "auto\t'Style only when the stream is a terminal that wants it (the default)'
always\t'Style even when the stream is redirected'
never\t'Never style'"
complete -c webp -n "__fish_webp_using_subcommand explain" -l threads -d 'Worker threads for parallel work; 0 (the default) uses one per core' -r
complete -c webp -n "__fish_webp_using_subcommand explain" -s v -l verbose -d 'Print per-stage detail on stderr'
complete -c webp -n "__fish_webp_using_subcommand explain" -l quiet -d 'Suppress all non-error output'
complete -c webp -n "__fish_webp_using_subcommand explain" -l dry-run -d 'Report what would be written, without encoding or writing anything'
complete -c webp -n "__fish_webp_using_subcommand explain" -s h -l help -d 'Print help (see more with \'--help\')'
complete -c webp -n "__fish_webp_using_subcommand completions" -l color -d 'auto, always, or never [default: auto, or from env/config]' -r -f -a "auto\t'Style only when the stream is a terminal that wants it (the default)'
always\t'Style even when the stream is redirected'
never\t'Never style'"
complete -c webp -n "__fish_webp_using_subcommand completions" -l threads -d 'Worker threads for parallel work; 0 (the default) uses one per core' -r
complete -c webp -n "__fish_webp_using_subcommand completions" -s v -l verbose -d 'Print per-stage detail on stderr'
complete -c webp -n "__fish_webp_using_subcommand completions" -l quiet -d 'Suppress all non-error output'
complete -c webp -n "__fish_webp_using_subcommand completions" -l dry-run -d 'Report what would be written, without encoding or writing anything'
complete -c webp -n "__fish_webp_using_subcommand completions" -s h -l help -d 'Print help (see more with \'--help\')'
complete -c webp -n "__fish_webp_using_subcommand man" -l color -d 'auto, always, or never [default: auto, or from env/config]' -r -f -a "auto\t'Style only when the stream is a terminal that wants it (the default)'
always\t'Style even when the stream is redirected'
never\t'Never style'"
complete -c webp -n "__fish_webp_using_subcommand man" -l threads -d 'Worker threads for parallel work; 0 (the default) uses one per core' -r
complete -c webp -n "__fish_webp_using_subcommand man" -s v -l verbose -d 'Print per-stage detail on stderr'
complete -c webp -n "__fish_webp_using_subcommand man" -l quiet -d 'Suppress all non-error output'
complete -c webp -n "__fish_webp_using_subcommand man" -l dry-run -d 'Report what would be written, without encoding or writing anything'
complete -c webp -n "__fish_webp_using_subcommand man" -s h -l help -d 'Print help (see more with \'--help\')'
complete -c webp -n "__fish_webp_using_subcommand help; and not __fish_seen_subcommand_from decode encode convert animate mux info meta diff doctor config explain completions man help" -f -a "decode" -d 'Decode a WebP file to PNG (default), PPM/PAM, or raw pixels'
complete -c webp -n "__fish_webp_using_subcommand help; and not __fish_seen_subcommand_from decode encode convert animate mux info meta diff doctor config explain completions man help" -f -a "encode" -d 'Encode an image (PNG/JPEG/GIF/TIFF/BMP/PPM/PAM/raw) into a WebP file'
complete -c webp -n "__fish_webp_using_subcommand help; and not __fish_seen_subcommand_from decode encode convert animate mux info meta diff doctor config explain completions man help" -f -a "convert" -d 'Batch-convert many images (or directories) to WebP, in parallel'
complete -c webp -n "__fish_webp_using_subcommand help; and not __fish_seen_subcommand_from decode encode convert animate mux info meta diff doctor config explain completions man help" -f -a "animate" -d 'Assemble still images into an animated WebP (surpasses img2webp/gif2webp)'
complete -c webp -n "__fish_webp_using_subcommand help; and not __fish_seen_subcommand_from decode encode convert animate mux info meta diff doctor config explain completions man help" -f -a "mux" -d 'Edit an animated WebP without re-encoding frames (webpmux-parity muxing)'
complete -c webp -n "__fish_webp_using_subcommand help; and not __fish_seen_subcommand_from decode encode convert animate mux info meta diff doctor config explain completions man help" -f -a "info" -d 'Print a summary of a WebP file (size, alpha, metadata, animation)'
complete -c webp -n "__fish_webp_using_subcommand help; and not __fish_seen_subcommand_from decode encode convert animate mux info meta diff doctor config explain completions man help" -f -a "meta" -d 'Read, set, or strip a WebP file\'s metadata (ICC/Exif/XMP), without re-encoding the image'
complete -c webp -n "__fish_webp_using_subcommand help; and not __fish_seen_subcommand_from decode encode convert animate mux info meta diff doctor config explain completions man help" -f -a "diff" -d 'Compare two images: report PSNR and the largest per-channel difference'
complete -c webp -n "__fish_webp_using_subcommand help; and not __fish_seen_subcommand_from decode encode convert animate mux info meta diff doctor config explain completions man help" -f -a "doctor" -d 'Diagnose the environment: PATH drop-in shadows, config, terminal, threads'
complete -c webp -n "__fish_webp_using_subcommand help; and not __fish_seen_subcommand_from decode encode convert animate mux info meta diff doctor config explain completions man help" -f -a "config" -d 'Show resolved settings and where each came from (args, env, file, default)'
complete -c webp -n "__fish_webp_using_subcommand help; and not __fish_seen_subcommand_from decode encode convert animate mux info meta diff doctor config explain completions man help" -f -a "explain" -d 'Explain an exit code: what a failing run\'s status number means'
complete -c webp -n "__fish_webp_using_subcommand help; and not __fish_seen_subcommand_from decode encode convert animate mux info meta diff doctor config explain completions man help" -f -a "completions" -d 'Print a shell completion script'
complete -c webp -n "__fish_webp_using_subcommand help; and not __fish_seen_subcommand_from decode encode convert animate mux info meta diff doctor config explain completions man help" -f -a "man" -d 'Print a man page in roff, for `man -l -` or a package\'s man directory'
complete -c webp -n "__fish_webp_using_subcommand help; and not __fish_seen_subcommand_from decode encode convert animate mux info meta diff doctor config explain completions man help" -f -a "help" -d 'Print this message or the help of the given subcommand(s)'
complete -c webp -n "__fish_webp_using_subcommand help; and __fish_seen_subcommand_from mux" -f -a "get-frame" -d 'Extract one frame as a standalone still WebP (bytes copied verbatim)'
complete -c webp -n "__fish_webp_using_subcommand help; and __fish_seen_subcommand_from mux" -f -a "set" -d 'Rewrite the loop count and/or background color'
complete -c webp -n "__fish_webp_using_subcommand help; and __fish_seen_subcommand_from mux" -f -a "remove" -d 'Remove one frame, rebuilding the frame list'
complete -c webp -n "__fish_webp_using_subcommand help; and __fish_seen_subcommand_from mux" -f -a "insert" -d 'Insert a still WebP as a new frame (its image bytes copied verbatim)'
complete -c webp -n "__fish_webp_using_subcommand help; and __fish_seen_subcommand_from mux" -f -a "replace" -d 'Replace one frame\'s image with a still WebP, keeping its placement/timing'
complete -c webp -n "__fish_webp_using_subcommand help; and __fish_seen_subcommand_from meta" -f -a "show" -d 'Show the ICC/Exif/XMP a WebP carries (kinds and byte sizes)'
complete -c webp -n "__fish_webp_using_subcommand help; and __fish_seen_subcommand_from meta" -f -a "set" -d 'Write a copy with ICC/Exif/XMP set from files (unspecified kinds are kept)'
complete -c webp -n "__fish_webp_using_subcommand help; and __fish_seen_subcommand_from meta" -f -a "strip" -d 'Write a copy with all sidecar metadata removed'
complete -c webp -n "__fish_webp_using_subcommand help; and __fish_seen_subcommand_from config" -f -a "get" -d 'Print a single setting\'s resolved value, with nothing else'
