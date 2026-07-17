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
'--color=[auto, always, or never \[default\: auto, or from env/config\]]:WHEN:((auto\:"Style only when the stream is a terminal that wants it (the default)"
always\:"Style even when the stream is redirected"
never\:"Never style"))' \
'--threads=[Worker threads for parallel work; 0 (the default) uses one per core]:N:_default' \
'-o+[Output file, or a directory for many inputs; default\: beside each input]:OUTPUT:_files' \
'--output=[Output file, or a directory for many inputs; default\: beside each input]:OUTPUT:_files' \
'-q+[Lossy quality 0-100 (higher = larger, closer to source); selects lossy]:QUALITY:_default' \
'--quality=[Lossy quality 0-100 (higher = larger, closer to source); selects lossy]:QUALITY:_default' \
'(--lossy)--near-lossless=[Near-lossless preprocessing 0-100 (lower = stronger; implies lossless)]:N:_default' \
'-m+[Encoder effort \[default\: auto, or from env/config\]]:METHOD:((auto\:"Adapt the search depth to the image'\''s content and size (the default)"
fast\:"Fastest\: the shallowest fixed search"
best\:"Smallest\: the deepest fixed search"))' \
'--method=[Encoder effort \[default\: auto, or from env/config\]]:METHOD:((auto\:"Adapt the search depth to the image'\''s content and size (the default)"
fast\:"Fastest\: the shallowest fixed search"
best\:"Smallest\: the deepest fixed search"))' \
'*--metadata=[Metadata to embed\: all,none,icc,exif,xmp (default\: all)]:METADATA:((all\:"Keep ICC, Exif, and XMP"
none\:"Strip everything (a bare \`VP8L\` output)"
icc\:"Keep the ICC color profile"
exif\:"Keep Exif"
xmp\:"Keep XMP"))' \
'--crop=[Crop before encoding\: \`x,y,width,height\` in pixels (applied before --resize)]:X,Y,W,H:_default' \
'--resize=[Resize before encoding\: \`WxH\` (use 0 on one axis to keep aspect)]:WxH:_default' \
'*-v[Print per-stage detail on stderr]' \
'*--verbose[Print per-stage detail on stderr]' \
'(-v --verbose)--quiet[Suppress all non-error output]' \
'--dry-run[Report what would be written, without encoding or writing anything]' \
'(--lossy)--lossless[Force lossless (VP8L) encoding]' \
'--lossy[Force lossy (VP8) encoding]' \
'-r[Recurse into subdirectories]' \
'--recursive[Recurse into subdirectories]' \
'--force[Overwrite an existing derived output]' \
'(--force)--no-clobber[Skip an existing derived output instead of failing (still exits 0)]' \
'-h[Print help (see more with '\''--help'\'')]' \
'--help[Print help (see more with '\''--help'\'')]' \
'-V[Print version]' \
'--version[Print version]' \
'::inputs -- Images or directories. A WebP is decoded to PNG; anything else is encoded:_files' \
":: :_webp_commands" \
"*::: :->webp" \
&& ret=0
    case $state in
    (webp)
        words=($line[2] "${words[@]}")
        (( CURRENT += 1 ))
        curcontext="${curcontext%:*:*}:webp-command-$line[2]:"
        case $line[2] in
            (decode)
_arguments "${_arguments_options[@]}" : \
'-o+[Output path; \`-\` writes stdout]:OUTPUT:_files' \
'--output=[Output path; \`-\` writes stdout]:OUTPUT:_files' \
'--format=[Output format; defaults to the \`-o\` extension, else PNG]:FORMAT:((png\:"PNG, RGBA8"
ppm\:"Netpbm binary PPM (\`P6\`, RGB; alpha dropped)"
pam\:"Netpbm binary PAM (\`P7\`, RGBA)"
bmp\:"BMP (RGBA8; needs the \`formats\` feature)"
tiff\:"TIFF (RGBA8; needs the \`formats\` feature)"
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
'--color=[auto, always, or never \[default\: auto, or from env/config\]]:WHEN:((auto\:"Style only when the stream is a terminal that wants it (the default)"
always\:"Style even when the stream is redirected"
never\:"Never style"))' \
'--threads=[Worker threads for parallel work; 0 (the default) uses one per core]:N:_default' \
'*-v[Print per-stage detail on stderr]' \
'*--verbose[Print per-stage detail on stderr]' \
'(-v --verbose)--quiet[Suppress all non-error output]' \
'--dry-run[Report what would be written, without encoding or writing anything]' \
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
jpeg\:"JPEG (decoded to RGBA8; needs the \`formats\` feature)"
gif\:"GIF (first frame as a still; whole-file animation is a separate path)"
tiff\:"TIFF (decoded to RGBA8; needs the \`formats\` feature)"
bmp\:"BMP (decoded to RGBA8; needs the \`formats\` feature)"
raw\:"Raw row-major pixels; requires \`--width\`/\`--height\`/\`--layout\`"))' \
'--width=[Raw-input width in pixels (required for raw input)]:WIDTH:_default' \
'--height=[Raw-input height in pixels (required for raw input)]:HEIGHT:_default' \
'--layout=[Byte order for raw input only]:LAYOUT:((rgba8\:"\`R, G, B, A\`"
argb8\:"\`A, R, G, B\`"
bgra8\:"\`B, G, R, A\`"))' \
'-m+[Encoder effort \[default\: auto, or from env/config\]]:METHOD:((auto\:"Adapt the search depth to the image'\''s content and size (the default)"
fast\:"Fastest\: the shallowest fixed search"
best\:"Smallest\: the deepest fixed search"))' \
'--method=[Encoder effort \[default\: auto, or from env/config\]]:METHOD:((auto\:"Adapt the search depth to the image'\''s content and size (the default)"
fast\:"Fastest\: the shallowest fixed search"
best\:"Smallest\: the deepest fixed search"))' \
'-q+[Lossy quality 0-100 (higher = larger, closer to source); selects --lossy]:QUALITY:_default' \
'--quality=[Lossy quality 0-100 (higher = larger, closer to source); selects --lossy]:QUALITY:_default' \
'(--lossy)--near-lossless=[Near-lossless preprocessing 0-100 (lower = stronger; implies lossless)]:N:_default' \
'--crop=[Crop before encoding\: \`x,y,width,height\` in pixels (applied before --resize)]:X,Y,W,H:_default' \
'--resize=[Resize before encoding\: \`WxH\` (use 0 on one axis to keep aspect)]:WxH:_default' \
'--target-size=[Target output size, e.g. \`200k\` or \`2M\`, found by searching lossy quality]:SIZE:_default' \
'--min-psnr=[Target reconstruction PSNR floor in dB (lossy only; pairs with --target-size)]:DB:_default' \
'*--metadata=[Metadata to embed\: all,none,icc,exif,xmp (default\: all — kinder than cwebp)]:METADATA:((all\:"Keep ICC, Exif, and XMP"
none\:"Strip everything (a bare \`VP8L\` output)"
icc\:"Keep the ICC color profile"
exif\:"Keep Exif"
xmp\:"Keep XMP"))' \
'--kmax=[With --optimize\: force a keyframe at least every N frames (gif2webp -kmax; 0 = only the first)]:N:_default' \
'--kmin=[With --optimize\: never place keyframes closer than N frames apart (gif2webp -kmin)]:N:_default' \
'--color=[auto, always, or never \[default\: auto, or from env/config\]]:WHEN:((auto\:"Style only when the stream is a terminal that wants it (the default)"
always\:"Style even when the stream is redirected"
never\:"Never style"))' \
'--threads=[Worker threads for parallel work; 0 (the default) uses one per core]:N:_default' \
'(--lossy)--lossless[Force lossless (VP8L). The default is source-derived\: JPEG → lossy, else lossless]' \
'--lossy[Encode lossily (VP8) instead of losslessly (VP8L)]' \
'--optimize[Inter-frame optimize a GIF animation\: encode each frame as a minimal delta]' \
'--mixed[With --optimize\: trial each frame lossy and lossless, keep the smaller (gif2webp -mixed)]' \
'--min-size[With --optimize\: exhaustively search each frame'\''s rect/blend/dispose/codec (gif2webp \`-min_size\`)]' \
'*-v[Print per-stage detail on stderr]' \
'*--verbose[Print per-stage detail on stderr]' \
'(-v --verbose)--quiet[Suppress all non-error output]' \
'--dry-run[Report what would be written, without encoding or writing anything]' \
'-h[Print help (see more with '\''--help'\'')]' \
'--help[Print help (see more with '\''--help'\'')]' \
'::input -- Input image (PNG/JPEG/GIF/TIFF/BMP/PPM/PAM/raw); `-` (default) reads stdin:_files' \
&& ret=0
;;
(convert)
_arguments "${_arguments_options[@]}" : \
'-o+[Output directory (created outputs are \`<stem>.webp\`); default\: beside input]:OUTPUT:_files' \
'--output=[Output directory (created outputs are \`<stem>.webp\`); default\: beside input]:OUTPUT:_files' \
'-m+[Encoder effort (ignored with --optimize) \[default\: auto, or from env/config\]]:METHOD:((auto\:"Adapt the search depth to the image'\''s content and size (the default)"
fast\:"Fastest\: the shallowest fixed search"
best\:"Smallest\: the deepest fixed search"))' \
'--method=[Encoder effort (ignored with --optimize) \[default\: auto, or from env/config\]]:METHOD:((auto\:"Adapt the search depth to the image'\''s content and size (the default)"
fast\:"Fastest\: the shallowest fixed search"
best\:"Smallest\: the deepest fixed search"))' \
'-q+[Lossy quality 0-100 (higher = larger, closer to source); selects --lossy]:QUALITY:_default' \
'--quality=[Lossy quality 0-100 (higher = larger, closer to source); selects --lossy]:QUALITY:_default' \
'(--lossy)--near-lossless=[Near-lossless preprocessing 0-100 (lower = stronger; implies lossless)]:N:_default' \
'*--metadata=[Metadata to embed\: all,none,icc,exif,xmp (default\: all)]:METADATA:((all\:"Keep ICC, Exif, and XMP"
none\:"Strip everything (a bare \`VP8L\` output)"
icc\:"Keep the ICC color profile"
exif\:"Keep Exif"
xmp\:"Keep XMP"))' \
'--color=[auto, always, or never \[default\: auto, or from env/config\]]:WHEN:((auto\:"Style only when the stream is a terminal that wants it (the default)"
always\:"Style even when the stream is redirected"
never\:"Never style"))' \
'--threads=[Worker threads for parallel work; 0 (the default) uses one per core]:N:_default' \
'(--lossy)--lossless[Force lossless (VP8L). The default is source-derived\: JPEG → lossy, else lossless]' \
'--lossy[Encode lossily (VP8) instead of losslessly (VP8L)]' \
'--optimize[Try every lossless effort level and keep the smallest output]' \
'-r[Recurse into subdirectories]' \
'--recursive[Recurse into subdirectories]' \
'--force[Overwrite an existing output]' \
'(--force)--no-clobber[Skip an input whose \`.webp\` output exists (still exits 0)]' \
'*-v[Print per-stage detail on stderr]' \
'*--verbose[Print per-stage detail on stderr]' \
'(-v --verbose)--quiet[Suppress all non-error output]' \
'--dry-run[Report what would be written, without encoding or writing anything]' \
'-h[Print help (see more with '\''--help'\'')]' \
'--help[Print help (see more with '\''--help'\'')]' \
'*::inputs -- Input images and/or directories (PNG/JPEG/GIF/TIFF/BMP/PPM/PAM):_files' \
&& ret=0
;;
(animate)
_arguments "${_arguments_options[@]}" : \
'-o+[Output \`.webp\` file; \`-\` writes stdout]:OUTPUT:_files' \
'--output=[Output \`.webp\` file; \`-\` writes stdout]:OUTPUT:_files' \
'--delay=[Per-frame delay in ms\: one value for every frame, or a comma list (\`40,40,80\`)]:MS:_default' \
'--loop=[Loop count; \`0\` (the default) loops forever]:N:_default' \
'--bgcolor=[Advisory background color as \`RRGGBBAA\` hex (e.g. \`ffffffff\`)]:RRGGBBAA:_default' \
'--dispose=[Disposal method applied to every frame]:DISPOSE:((keep\:"Leave the canvas as-is for the next frame (the default)"
background\:"Clear the frame'\''s rectangle to transparent before the next frame"))' \
'--blend=[Blend method applied to every frame]:BLEND:((blend\:"Alpha-blend the frame over the canvas (the default)"
overwrite\:"Overwrite the frame'\''s rectangle, ignoring what is underneath"))' \
'--lossy=[Encode frames lossily (VP8) at this quality 0-100; the default is lossless]:Q:_default' \
'--canvas=[Canvas size as \`WxH\`; defaults to the largest frame]:WxH:_default' \
'-m+[Encoder effort \[default\: auto, or from env/config\]]:METHOD:((auto\:"Adapt the search depth to the image'\''s content and size (the default)"
fast\:"Fastest\: the shallowest fixed search"
best\:"Smallest\: the deepest fixed search"))' \
'--method=[Encoder effort \[default\: auto, or from env/config\]]:METHOD:((auto\:"Adapt the search depth to the image'\''s content and size (the default)"
fast\:"Fastest\: the shallowest fixed search"
best\:"Smallest\: the deepest fixed search"))' \
'--input-format=[Force the input format instead of sniffing each file]:INPUT_FORMAT:((png\:"PNG (any color type; normalized to RGBA8)"
ppm\:"Netpbm binary PPM (\`P6\`, RGB)"
pam\:"Netpbm binary PAM (\`P7\`, RGBA)"
jpeg\:"JPEG (decoded to RGBA8; needs the \`formats\` feature)"
gif\:"GIF (first frame as a still; whole-file animation is a separate path)"
tiff\:"TIFF (decoded to RGBA8; needs the \`formats\` feature)"
bmp\:"BMP (decoded to RGBA8; needs the \`formats\` feature)"
raw\:"Raw row-major pixels; requires \`--width\`/\`--height\`/\`--layout\`"))' \
'--kmax=[With --optimize\: force a keyframe at least every N frames (gif2webp -kmax; 0 = only the first)]:N:_default' \
'--kmin=[With --optimize\: never place keyframes closer than N frames apart (gif2webp -kmin)]:N:_default' \
'--color=[auto, always, or never \[default\: auto, or from env/config\]]:WHEN:((auto\:"Style only when the stream is a terminal that wants it (the default)"
always\:"Style even when the stream is redirected"
never\:"Never style"))' \
'--threads=[Worker threads for parallel work; 0 (the default) uses one per core]:N:_default' \
'--optimize[Inter-frame optimize\: encode each frame as a minimal delta against the canvas]' \
'--mixed[With --optimize\: trial each frame lossy and lossless, keep the smaller (gif2webp -mixed)]' \
'--min-size[With --optimize\: exhaustively search each frame'\''s rect/blend/dispose/codec (gif2webp \`-min_size\`)]' \
'*-v[Print per-stage detail on stderr]' \
'*--verbose[Print per-stage detail on stderr]' \
'(-v --verbose)--quiet[Suppress all non-error output]' \
'--dry-run[Report what would be written, without encoding or writing anything]' \
'-h[Print help (see more with '\''--help'\'')]' \
'--help[Print help (see more with '\''--help'\'')]' \
'*::inputs -- Still images, one per frame, in display order (PNG/JPEG/GIF/TIFF/BMP/PPM/PAM):_files' \
&& ret=0
;;
(mux)
_arguments "${_arguments_options[@]}" : \
'--color=[auto, always, or never \[default\: auto, or from env/config\]]:WHEN:((auto\:"Style only when the stream is a terminal that wants it (the default)"
always\:"Style even when the stream is redirected"
never\:"Never style"))' \
'--threads=[Worker threads for parallel work; 0 (the default) uses one per core]:N:_default' \
'*-v[Print per-stage detail on stderr]' \
'*--verbose[Print per-stage detail on stderr]' \
'(-v --verbose)--quiet[Suppress all non-error output]' \
'--dry-run[Report what would be written, without encoding or writing anything]' \
'-h[Print help (see more with '\''--help'\'')]' \
'--help[Print help (see more with '\''--help'\'')]' \
":: :_webp__subcmd__mux_commands" \
"*::: :->mux" \
&& ret=0

    case $state in
    (mux)
        words=($line[1] "${words[@]}")
        (( CURRENT += 1 ))
        curcontext="${curcontext%:*:*}:webp-mux-command-$line[1]:"
        case $line[1] in
            (get-frame)
_arguments "${_arguments_options[@]}" : \
'-o+[Output \`.webp\` file; \`-\` writes stdout]:OUTPUT:_files' \
'--output=[Output \`.webp\` file; \`-\` writes stdout]:OUTPUT:_files' \
'--color=[auto, always, or never \[default\: auto, or from env/config\]]:WHEN:((auto\:"Style only when the stream is a terminal that wants it (the default)"
always\:"Style even when the stream is redirected"
never\:"Never style"))' \
'--threads=[Worker threads for parallel work; 0 (the default) uses one per core]:N:_default' \
'*-v[Print per-stage detail on stderr]' \
'*--verbose[Print per-stage detail on stderr]' \
'(-v --verbose)--quiet[Suppress all non-error output]' \
'--dry-run[Report what would be written, without encoding or writing anything]' \
'-h[Print help (see more with '\''--help'\'')]' \
'--help[Print help (see more with '\''--help'\'')]' \
':input -- Input animated `.webp` file:_files' \
':index -- 0-based frame index to extract:_default' \
&& ret=0
;;
(set)
_arguments "${_arguments_options[@]}" : \
'-o+[Output \`.webp\` file; \`-\` writes stdout]:OUTPUT:_files' \
'--output=[Output \`.webp\` file; \`-\` writes stdout]:OUTPUT:_files' \
'--loop=[New loop count (\`0\` loops forever)]:N:_default' \
'--bgcolor=[New background color as \`RRGGBBAA\` hex]:RRGGBBAA:_default' \
'--color=[auto, always, or never \[default\: auto, or from env/config\]]:WHEN:((auto\:"Style only when the stream is a terminal that wants it (the default)"
always\:"Style even when the stream is redirected"
never\:"Never style"))' \
'--threads=[Worker threads for parallel work; 0 (the default) uses one per core]:N:_default' \
'*-v[Print per-stage detail on stderr]' \
'*--verbose[Print per-stage detail on stderr]' \
'(-v --verbose)--quiet[Suppress all non-error output]' \
'--dry-run[Report what would be written, without encoding or writing anything]' \
'-h[Print help (see more with '\''--help'\'')]' \
'--help[Print help (see more with '\''--help'\'')]' \
':input -- Input animated `.webp` file:_files' \
&& ret=0
;;
(remove)
_arguments "${_arguments_options[@]}" : \
'-o+[Output \`.webp\` file; \`-\` writes stdout]:OUTPUT:_files' \
'--output=[Output \`.webp\` file; \`-\` writes stdout]:OUTPUT:_files' \
'--color=[auto, always, or never \[default\: auto, or from env/config\]]:WHEN:((auto\:"Style only when the stream is a terminal that wants it (the default)"
always\:"Style even when the stream is redirected"
never\:"Never style"))' \
'--threads=[Worker threads for parallel work; 0 (the default) uses one per core]:N:_default' \
'*-v[Print per-stage detail on stderr]' \
'*--verbose[Print per-stage detail on stderr]' \
'(-v --verbose)--quiet[Suppress all non-error output]' \
'--dry-run[Report what would be written, without encoding or writing anything]' \
'-h[Print help (see more with '\''--help'\'')]' \
'--help[Print help (see more with '\''--help'\'')]' \
':input -- Input animated `.webp` file:_files' \
':index -- 0-based frame index to remove:_default' \
&& ret=0
;;
(insert)
_arguments "${_arguments_options[@]}" : \
'-o+[Output \`.webp\` file; \`-\` writes stdout]:OUTPUT:_files' \
'--output=[Output \`.webp\` file; \`-\` writes stdout]:OUTPUT:_files' \
'--at=[0-based index to insert at; defaults to appending at the end]:N:_default' \
'--delay=[The new frame'\''s display duration in ms]:MS:_default' \
'--blend=[The new frame'\''s blend method]:BLEND:((blend\:"Alpha-blend the frame over the canvas (the default)"
overwrite\:"Overwrite the frame'\''s rectangle, ignoring what is underneath"))' \
'--dispose=[The new frame'\''s disposal method]:DISPOSE:((keep\:"Leave the canvas as-is for the next frame (the default)"
background\:"Clear the frame'\''s rectangle to transparent before the next frame"))' \
'--color=[auto, always, or never \[default\: auto, or from env/config\]]:WHEN:((auto\:"Style only when the stream is a terminal that wants it (the default)"
always\:"Style even when the stream is redirected"
never\:"Never style"))' \
'--threads=[Worker threads for parallel work; 0 (the default) uses one per core]:N:_default' \
'*-v[Print per-stage detail on stderr]' \
'*--verbose[Print per-stage detail on stderr]' \
'(-v --verbose)--quiet[Suppress all non-error output]' \
'--dry-run[Report what would be written, without encoding or writing anything]' \
'-h[Print help (see more with '\''--help'\'')]' \
'--help[Print help (see more with '\''--help'\'')]' \
':input -- Input animated `.webp` file:_files' \
':frame -- The still `.webp` to insert as a new frame:_files' \
&& ret=0
;;
(replace)
_arguments "${_arguments_options[@]}" : \
'-o+[Output \`.webp\` file; \`-\` writes stdout]:OUTPUT:_files' \
'--output=[Output \`.webp\` file; \`-\` writes stdout]:OUTPUT:_files' \
'--at=[0-based index of the frame to replace]:N:_default' \
'--color=[auto, always, or never \[default\: auto, or from env/config\]]:WHEN:((auto\:"Style only when the stream is a terminal that wants it (the default)"
always\:"Style even when the stream is redirected"
never\:"Never style"))' \
'--threads=[Worker threads for parallel work; 0 (the default) uses one per core]:N:_default' \
'*-v[Print per-stage detail on stderr]' \
'*--verbose[Print per-stage detail on stderr]' \
'(-v --verbose)--quiet[Suppress all non-error output]' \
'--dry-run[Report what would be written, without encoding or writing anything]' \
'-h[Print help (see more with '\''--help'\'')]' \
'--help[Print help (see more with '\''--help'\'')]' \
':input -- Input animated `.webp` file:_files' \
':frame -- The still `.webp` whose image replaces the frame:_files' \
&& ret=0
;;
(help)
_arguments "${_arguments_options[@]}" : \
":: :_webp__subcmd__mux__subcmd__help_commands" \
"*::: :->help" \
&& ret=0

    case $state in
    (help)
        words=($line[1] "${words[@]}")
        (( CURRENT += 1 ))
        curcontext="${curcontext%:*:*}:webp-mux-help-command-$line[1]:"
        case $line[1] in
            (get-frame)
_arguments "${_arguments_options[@]}" : \
&& ret=0
;;
(set)
_arguments "${_arguments_options[@]}" : \
&& ret=0
;;
(remove)
_arguments "${_arguments_options[@]}" : \
&& ret=0
;;
(insert)
_arguments "${_arguments_options[@]}" : \
&& ret=0
;;
(replace)
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
;;
(info)
_arguments "${_arguments_options[@]}" : \
'--color=[auto, always, or never \[default\: auto, or from env/config\]]:WHEN:((auto\:"Style only when the stream is a terminal that wants it (the default)"
always\:"Style even when the stream is redirected"
never\:"Never style"))' \
'--threads=[Worker threads for parallel work; 0 (the default) uses one per core]:N:_default' \
'--json[Print the report as JSON instead of text]' \
'*-v[Print per-stage detail on stderr]' \
'*--verbose[Print per-stage detail on stderr]' \
'(-v --verbose)--quiet[Suppress all non-error output]' \
'--dry-run[Report what would be written, without encoding or writing anything]' \
'-h[Print help (see more with '\''--help'\'')]' \
'--help[Print help (see more with '\''--help'\'')]' \
'::input -- Input `.webp` file; `-` (the default) reads stdin:_files' \
&& ret=0
;;
(meta)
_arguments "${_arguments_options[@]}" : \
'--color=[auto, always, or never \[default\: auto, or from env/config\]]:WHEN:((auto\:"Style only when the stream is a terminal that wants it (the default)"
always\:"Style even when the stream is redirected"
never\:"Never style"))' \
'--threads=[Worker threads for parallel work; 0 (the default) uses one per core]:N:_default' \
'*-v[Print per-stage detail on stderr]' \
'*--verbose[Print per-stage detail on stderr]' \
'(-v --verbose)--quiet[Suppress all non-error output]' \
'--dry-run[Report what would be written, without encoding or writing anything]' \
'-h[Print help (see more with '\''--help'\'')]' \
'--help[Print help (see more with '\''--help'\'')]' \
":: :_webp__subcmd__meta_commands" \
"*::: :->meta" \
&& ret=0

    case $state in
    (meta)
        words=($line[1] "${words[@]}")
        (( CURRENT += 1 ))
        curcontext="${curcontext%:*:*}:webp-meta-command-$line[1]:"
        case $line[1] in
            (show)
_arguments "${_arguments_options[@]}" : \
'--color=[auto, always, or never \[default\: auto, or from env/config\]]:WHEN:((auto\:"Style only when the stream is a terminal that wants it (the default)"
always\:"Style even when the stream is redirected"
never\:"Never style"))' \
'--threads=[Worker threads for parallel work; 0 (the default) uses one per core]:N:_default' \
'--json[Print the metadata as JSON instead of text]' \
'*-v[Print per-stage detail on stderr]' \
'*--verbose[Print per-stage detail on stderr]' \
'(-v --verbose)--quiet[Suppress all non-error output]' \
'--dry-run[Report what would be written, without encoding or writing anything]' \
'-h[Print help (see more with '\''--help'\'')]' \
'--help[Print help (see more with '\''--help'\'')]' \
'::input -- Input `.webp` file; `-` (the default) reads stdin:_files' \
&& ret=0
;;
(set)
_arguments "${_arguments_options[@]}" : \
'-o+[Output \`.webp\` file; \`-\` writes stdout]:OUTPUT:_files' \
'--output=[Output \`.webp\` file; \`-\` writes stdout]:OUTPUT:_files' \
'--icc=[Set the ICC color profile from this file]:FILE:_files' \
'--exif=[Set the Exif metadata from this file]:FILE:_files' \
'--xmp=[Set the XMP metadata from this file]:FILE:_files' \
'--color=[auto, always, or never \[default\: auto, or from env/config\]]:WHEN:((auto\:"Style only when the stream is a terminal that wants it (the default)"
always\:"Style even when the stream is redirected"
never\:"Never style"))' \
'--threads=[Worker threads for parallel work; 0 (the default) uses one per core]:N:_default' \
'*-v[Print per-stage detail on stderr]' \
'*--verbose[Print per-stage detail on stderr]' \
'(-v --verbose)--quiet[Suppress all non-error output]' \
'--dry-run[Report what would be written, without encoding or writing anything]' \
'-h[Print help (see more with '\''--help'\'')]' \
'--help[Print help (see more with '\''--help'\'')]' \
':input -- Input `.webp` file; `-` reads stdin:_files' \
&& ret=0
;;
(strip)
_arguments "${_arguments_options[@]}" : \
'-o+[Output \`.webp\` file; \`-\` writes stdout]:OUTPUT:_files' \
'--output=[Output \`.webp\` file; \`-\` writes stdout]:OUTPUT:_files' \
'--color=[auto, always, or never \[default\: auto, or from env/config\]]:WHEN:((auto\:"Style only when the stream is a terminal that wants it (the default)"
always\:"Style even when the stream is redirected"
never\:"Never style"))' \
'--threads=[Worker threads for parallel work; 0 (the default) uses one per core]:N:_default' \
'*-v[Print per-stage detail on stderr]' \
'*--verbose[Print per-stage detail on stderr]' \
'(-v --verbose)--quiet[Suppress all non-error output]' \
'--dry-run[Report what would be written, without encoding or writing anything]' \
'-h[Print help (see more with '\''--help'\'')]' \
'--help[Print help (see more with '\''--help'\'')]' \
':input -- Input `.webp` file; `-` reads stdin:_files' \
&& ret=0
;;
(help)
_arguments "${_arguments_options[@]}" : \
":: :_webp__subcmd__meta__subcmd__help_commands" \
"*::: :->help" \
&& ret=0

    case $state in
    (help)
        words=($line[1] "${words[@]}")
        (( CURRENT += 1 ))
        curcontext="${curcontext%:*:*}:webp-meta-help-command-$line[1]:"
        case $line[1] in
            (show)
_arguments "${_arguments_options[@]}" : \
&& ret=0
;;
(set)
_arguments "${_arguments_options[@]}" : \
&& ret=0
;;
(strip)
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
;;
(diff)
_arguments "${_arguments_options[@]}" : \
'--min-psnr=[Fail (exit 1) if the RGB PSNR is below this many decibels]:DB:_default' \
'--color=[auto, always, or never \[default\: auto, or from env/config\]]:WHEN:((auto\:"Style only when the stream is a terminal that wants it (the default)"
always\:"Style even when the stream is redirected"
never\:"Never style"))' \
'--threads=[Worker threads for parallel work; 0 (the default) uses one per core]:N:_default' \
'--json[Print the comparison as JSON instead of text]' \
'*-v[Print per-stage detail on stderr]' \
'*--verbose[Print per-stage detail on stderr]' \
'(-v --verbose)--quiet[Suppress all non-error output]' \
'--dry-run[Report what would be written, without encoding or writing anything]' \
'-h[Print help (see more with '\''--help'\'')]' \
'--help[Print help (see more with '\''--help'\'')]' \
':a -- The first image (a WebP, or any readable format):_files' \
':b -- The second image, compared against the first (same dimensions required):_files' \
&& ret=0
;;
(doctor)
_arguments "${_arguments_options[@]}" : \
'--color=[auto, always, or never \[default\: auto, or from env/config\]]:WHEN:((auto\:"Style only when the stream is a terminal that wants it (the default)"
always\:"Style even when the stream is redirected"
never\:"Never style"))' \
'--threads=[Worker threads for parallel work; 0 (the default) uses one per core]:N:_default' \
'*-v[Print per-stage detail on stderr]' \
'*--verbose[Print per-stage detail on stderr]' \
'(-v --verbose)--quiet[Suppress all non-error output]' \
'--dry-run[Report what would be written, without encoding or writing anything]' \
'-h[Print help (see more with '\''--help'\'')]' \
'--help[Print help (see more with '\''--help'\'')]' \
&& ret=0
;;
(config)
_arguments "${_arguments_options[@]}" : \
'--quality=[Override\: lossy quality 0-100]:0-100:_default' \
'--effort=[Override\: encoder effort]:EFFORT:((auto\:"Adapt the search depth to the image'\''s content and size (the default)"
fast\:"Fastest\: the shallowest fixed search"
best\:"Smallest\: the deepest fixed search"))' \
'--codec=[Override\: lossless or lossy]:CODEC:((lossless\:"Lossless (VP8L)"
lossy\:"Lossy (VP8)"))' \
'*--metadata=[Override\: metadata to carry (all,none,icc,exif,xmp)]:METADATA:((all\:"Keep ICC, Exif, and XMP"
none\:"Strip everything (a bare \`VP8L\` output)"
icc\:"Keep the ICC color profile"
exif\:"Keep Exif"
xmp\:"Keep XMP"))' \
'--threads=[Override\: worker threads (0 = one per core)]:THREADS:_default' \
'--max-pixels=[Override\: decode pixel cap (N, 300M, 2G, or none)]:N|none:_default' \
'--color=[auto, always, or never \[default\: auto, or from env/config\]]:WHEN:((auto\:"Style only when the stream is a terminal that wants it (the default)"
always\:"Style even when the stream is redirected"
never\:"Never style"))' \
'--json[Print the resolved settings as JSON (stable key order)]' \
'(--json)--template[Print a commented \`webp.toml\` template to stdout]' \
'*-v[Print per-stage detail on stderr]' \
'*--verbose[Print per-stage detail on stderr]' \
'(-v --verbose)--quiet[Suppress all non-error output]' \
'--dry-run[Report what would be written, without encoding or writing anything]' \
'-h[Print help (see more with '\''--help'\'')]' \
'--help[Print help (see more with '\''--help'\'')]' \
":: :_webp__subcmd__config_commands" \
"*::: :->config" \
&& ret=0

    case $state in
    (config)
        words=($line[1] "${words[@]}")
        (( CURRENT += 1 ))
        curcontext="${curcontext%:*:*}:webp-config-command-$line[1]:"
        case $line[1] in
            (get)
_arguments "${_arguments_options[@]}" : \
'--color=[auto, always, or never \[default\: auto, or from env/config\]]:WHEN:((auto\:"Style only when the stream is a terminal that wants it (the default)"
always\:"Style even when the stream is redirected"
never\:"Never style"))' \
'*-v[Print per-stage detail on stderr]' \
'*--verbose[Print per-stage detail on stderr]' \
'(-v --verbose)--quiet[Suppress all non-error output]' \
'--dry-run[Report what would be written, without encoding or writing anything]' \
'-h[Print help (see more with '\''--help'\'')]' \
'--help[Print help (see more with '\''--help'\'')]' \
':key -- The setting to print, e.g. `quality`:_default' \
&& ret=0
;;
(help)
_arguments "${_arguments_options[@]}" : \
":: :_webp__subcmd__config__subcmd__help_commands" \
"*::: :->help" \
&& ret=0

    case $state in
    (help)
        words=($line[1] "${words[@]}")
        (( CURRENT += 1 ))
        curcontext="${curcontext%:*:*}:webp-config-help-command-$line[1]:"
        case $line[1] in
            (get)
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
;;
(explain)
_arguments "${_arguments_options[@]}" : \
'--color=[auto, always, or never \[default\: auto, or from env/config\]]:WHEN:((auto\:"Style only when the stream is a terminal that wants it (the default)"
always\:"Style even when the stream is redirected"
never\:"Never style"))' \
'--threads=[Worker threads for parallel work; 0 (the default) uses one per core]:N:_default' \
'*-v[Print per-stage detail on stderr]' \
'*--verbose[Print per-stage detail on stderr]' \
'(-v --verbose)--quiet[Suppress all non-error output]' \
'--dry-run[Report what would be written, without encoding or writing anything]' \
'-h[Print help (see more with '\''--help'\'')]' \
'--help[Print help (see more with '\''--help'\'')]' \
':code -- An exit code (`0`..`9`) or its short name (`usage`, `limit`, ...):_default' \
&& ret=0
;;
(completions)
_arguments "${_arguments_options[@]}" : \
'--color=[auto, always, or never \[default\: auto, or from env/config\]]:WHEN:((auto\:"Style only when the stream is a terminal that wants it (the default)"
always\:"Style even when the stream is redirected"
never\:"Never style"))' \
'--threads=[Worker threads for parallel work; 0 (the default) uses one per core]:N:_default' \
'*-v[Print per-stage detail on stderr]' \
'*--verbose[Print per-stage detail on stderr]' \
'(-v --verbose)--quiet[Suppress all non-error output]' \
'--dry-run[Report what would be written, without encoding or writing anything]' \
'-h[Print help (see more with '\''--help'\'')]' \
'--help[Print help (see more with '\''--help'\'')]' \
':shell -- The shell to generate for:(bash elvish fish powershell zsh)' \
&& ret=0
;;
(man)
_arguments "${_arguments_options[@]}" : \
'--color=[auto, always, or never \[default\: auto, or from env/config\]]:WHEN:((auto\:"Style only when the stream is a terminal that wants it (the default)"
always\:"Style even when the stream is redirected"
never\:"Never style"))' \
'--threads=[Worker threads for parallel work; 0 (the default) uses one per core]:N:_default' \
'*-v[Print per-stage detail on stderr]' \
'*--verbose[Print per-stage detail on stderr]' \
'(-v --verbose)--quiet[Suppress all non-error output]' \
'--dry-run[Report what would be written, without encoding or writing anything]' \
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
(animate)
_arguments "${_arguments_options[@]}" : \
&& ret=0
;;
(mux)
_arguments "${_arguments_options[@]}" : \
":: :_webp__subcmd__help__subcmd__mux_commands" \
"*::: :->mux" \
&& ret=0

    case $state in
    (mux)
        words=($line[1] "${words[@]}")
        (( CURRENT += 1 ))
        curcontext="${curcontext%:*:*}:webp-help-mux-command-$line[1]:"
        case $line[1] in
            (get-frame)
_arguments "${_arguments_options[@]}" : \
&& ret=0
;;
(set)
_arguments "${_arguments_options[@]}" : \
&& ret=0
;;
(remove)
_arguments "${_arguments_options[@]}" : \
&& ret=0
;;
(insert)
_arguments "${_arguments_options[@]}" : \
&& ret=0
;;
(replace)
_arguments "${_arguments_options[@]}" : \
&& ret=0
;;
        esac
    ;;
esac
;;
(info)
_arguments "${_arguments_options[@]}" : \
&& ret=0
;;
(meta)
_arguments "${_arguments_options[@]}" : \
":: :_webp__subcmd__help__subcmd__meta_commands" \
"*::: :->meta" \
&& ret=0

    case $state in
    (meta)
        words=($line[1] "${words[@]}")
        (( CURRENT += 1 ))
        curcontext="${curcontext%:*:*}:webp-help-meta-command-$line[1]:"
        case $line[1] in
            (show)
_arguments "${_arguments_options[@]}" : \
&& ret=0
;;
(set)
_arguments "${_arguments_options[@]}" : \
&& ret=0
;;
(strip)
_arguments "${_arguments_options[@]}" : \
&& ret=0
;;
        esac
    ;;
esac
;;
(diff)
_arguments "${_arguments_options[@]}" : \
&& ret=0
;;
(doctor)
_arguments "${_arguments_options[@]}" : \
&& ret=0
;;
(config)
_arguments "${_arguments_options[@]}" : \
":: :_webp__subcmd__help__subcmd__config_commands" \
"*::: :->config" \
&& ret=0

    case $state in
    (config)
        words=($line[1] "${words[@]}")
        (( CURRENT += 1 ))
        curcontext="${curcontext%:*:*}:webp-help-config-command-$line[1]:"
        case $line[1] in
            (get)
_arguments "${_arguments_options[@]}" : \
&& ret=0
;;
        esac
    ;;
esac
;;
(explain)
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
'encode:Encode an image (PNG/JPEG/GIF/TIFF/BMP/PPM/PAM/raw) into a WebP file' \
'convert:Batch-convert many images (or directories) to WebP, in parallel' \
'animate:Assemble still images into an animated WebP (surpasses img2webp/gif2webp)' \
'mux:Edit an animated WebP without re-encoding frames (webpmux-parity muxing)' \
'info:Print a summary of a WebP file (size, alpha, metadata, animation)' \
'meta:Read, set, or strip a WebP file'\''s metadata (ICC/Exif/XMP), without re-encoding the image' \
'diff:Compare two images\: report PSNR and the largest per-channel difference' \
'doctor:Diagnose the environment\: PATH drop-in shadows, config, terminal, threads' \
'config:Show resolved settings and where each came from (args, env, file, default)' \
'explain:Explain an exit code\: what a failing run'\''s status number means' \
'completions:Print a shell completion script' \
'man:Print a man page in roff, for \`man -l -\` or a package'\''s man directory' \
'help:Print this message or the help of the given subcommand(s)' \
    )
    _describe -t commands 'webp commands' commands "$@"
}
(( $+functions[_webp__subcmd__animate_commands] )) ||
_webp__subcmd__animate_commands() {
    local commands; commands=()
    _describe -t commands 'webp animate commands' commands "$@"
}
(( $+functions[_webp__subcmd__completions_commands] )) ||
_webp__subcmd__completions_commands() {
    local commands; commands=()
    _describe -t commands 'webp completions commands' commands "$@"
}
(( $+functions[_webp__subcmd__config_commands] )) ||
_webp__subcmd__config_commands() {
    local commands; commands=(
'get:Print a single setting'\''s resolved value, with nothing else' \
'help:Print this message or the help of the given subcommand(s)' \
    )
    _describe -t commands 'webp config commands' commands "$@"
}
(( $+functions[_webp__subcmd__config__subcmd__get_commands] )) ||
_webp__subcmd__config__subcmd__get_commands() {
    local commands; commands=()
    _describe -t commands 'webp config get commands' commands "$@"
}
(( $+functions[_webp__subcmd__config__subcmd__help_commands] )) ||
_webp__subcmd__config__subcmd__help_commands() {
    local commands; commands=(
'get:Print a single setting'\''s resolved value, with nothing else' \
'help:Print this message or the help of the given subcommand(s)' \
    )
    _describe -t commands 'webp config help commands' commands "$@"
}
(( $+functions[_webp__subcmd__config__subcmd__help__subcmd__get_commands] )) ||
_webp__subcmd__config__subcmd__help__subcmd__get_commands() {
    local commands; commands=()
    _describe -t commands 'webp config help get commands' commands "$@"
}
(( $+functions[_webp__subcmd__config__subcmd__help__subcmd__help_commands] )) ||
_webp__subcmd__config__subcmd__help__subcmd__help_commands() {
    local commands; commands=()
    _describe -t commands 'webp config help help commands' commands "$@"
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
(( $+functions[_webp__subcmd__diff_commands] )) ||
_webp__subcmd__diff_commands() {
    local commands; commands=()
    _describe -t commands 'webp diff commands' commands "$@"
}
(( $+functions[_webp__subcmd__doctor_commands] )) ||
_webp__subcmd__doctor_commands() {
    local commands; commands=()
    _describe -t commands 'webp doctor commands' commands "$@"
}
(( $+functions[_webp__subcmd__encode_commands] )) ||
_webp__subcmd__encode_commands() {
    local commands; commands=()
    _describe -t commands 'webp encode commands' commands "$@"
}
(( $+functions[_webp__subcmd__explain_commands] )) ||
_webp__subcmd__explain_commands() {
    local commands; commands=()
    _describe -t commands 'webp explain commands' commands "$@"
}
(( $+functions[_webp__subcmd__help_commands] )) ||
_webp__subcmd__help_commands() {
    local commands; commands=(
'decode:Decode a WebP file to PNG (default), PPM/PAM, or raw pixels' \
'encode:Encode an image (PNG/JPEG/GIF/TIFF/BMP/PPM/PAM/raw) into a WebP file' \
'convert:Batch-convert many images (or directories) to WebP, in parallel' \
'animate:Assemble still images into an animated WebP (surpasses img2webp/gif2webp)' \
'mux:Edit an animated WebP without re-encoding frames (webpmux-parity muxing)' \
'info:Print a summary of a WebP file (size, alpha, metadata, animation)' \
'meta:Read, set, or strip a WebP file'\''s metadata (ICC/Exif/XMP), without re-encoding the image' \
'diff:Compare two images\: report PSNR and the largest per-channel difference' \
'doctor:Diagnose the environment\: PATH drop-in shadows, config, terminal, threads' \
'config:Show resolved settings and where each came from (args, env, file, default)' \
'explain:Explain an exit code\: what a failing run'\''s status number means' \
'completions:Print a shell completion script' \
'man:Print a man page in roff, for \`man -l -\` or a package'\''s man directory' \
'help:Print this message or the help of the given subcommand(s)' \
    )
    _describe -t commands 'webp help commands' commands "$@"
}
(( $+functions[_webp__subcmd__help__subcmd__animate_commands] )) ||
_webp__subcmd__help__subcmd__animate_commands() {
    local commands; commands=()
    _describe -t commands 'webp help animate commands' commands "$@"
}
(( $+functions[_webp__subcmd__help__subcmd__completions_commands] )) ||
_webp__subcmd__help__subcmd__completions_commands() {
    local commands; commands=()
    _describe -t commands 'webp help completions commands' commands "$@"
}
(( $+functions[_webp__subcmd__help__subcmd__config_commands] )) ||
_webp__subcmd__help__subcmd__config_commands() {
    local commands; commands=(
'get:Print a single setting'\''s resolved value, with nothing else' \
    )
    _describe -t commands 'webp help config commands' commands "$@"
}
(( $+functions[_webp__subcmd__help__subcmd__config__subcmd__get_commands] )) ||
_webp__subcmd__help__subcmd__config__subcmd__get_commands() {
    local commands; commands=()
    _describe -t commands 'webp help config get commands' commands "$@"
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
(( $+functions[_webp__subcmd__help__subcmd__diff_commands] )) ||
_webp__subcmd__help__subcmd__diff_commands() {
    local commands; commands=()
    _describe -t commands 'webp help diff commands' commands "$@"
}
(( $+functions[_webp__subcmd__help__subcmd__doctor_commands] )) ||
_webp__subcmd__help__subcmd__doctor_commands() {
    local commands; commands=()
    _describe -t commands 'webp help doctor commands' commands "$@"
}
(( $+functions[_webp__subcmd__help__subcmd__encode_commands] )) ||
_webp__subcmd__help__subcmd__encode_commands() {
    local commands; commands=()
    _describe -t commands 'webp help encode commands' commands "$@"
}
(( $+functions[_webp__subcmd__help__subcmd__explain_commands] )) ||
_webp__subcmd__help__subcmd__explain_commands() {
    local commands; commands=()
    _describe -t commands 'webp help explain commands' commands "$@"
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
(( $+functions[_webp__subcmd__help__subcmd__meta_commands] )) ||
_webp__subcmd__help__subcmd__meta_commands() {
    local commands; commands=(
'show:Show the ICC/Exif/XMP a WebP carries (kinds and byte sizes)' \
'set:Write a copy with ICC/Exif/XMP set from files (unspecified kinds are kept)' \
'strip:Write a copy with all sidecar metadata removed' \
    )
    _describe -t commands 'webp help meta commands' commands "$@"
}
(( $+functions[_webp__subcmd__help__subcmd__meta__subcmd__set_commands] )) ||
_webp__subcmd__help__subcmd__meta__subcmd__set_commands() {
    local commands; commands=()
    _describe -t commands 'webp help meta set commands' commands "$@"
}
(( $+functions[_webp__subcmd__help__subcmd__meta__subcmd__show_commands] )) ||
_webp__subcmd__help__subcmd__meta__subcmd__show_commands() {
    local commands; commands=()
    _describe -t commands 'webp help meta show commands' commands "$@"
}
(( $+functions[_webp__subcmd__help__subcmd__meta__subcmd__strip_commands] )) ||
_webp__subcmd__help__subcmd__meta__subcmd__strip_commands() {
    local commands; commands=()
    _describe -t commands 'webp help meta strip commands' commands "$@"
}
(( $+functions[_webp__subcmd__help__subcmd__mux_commands] )) ||
_webp__subcmd__help__subcmd__mux_commands() {
    local commands; commands=(
'get-frame:Extract one frame as a standalone still WebP (bytes copied verbatim)' \
'set:Rewrite the loop count and/or background color' \
'remove:Remove one frame, rebuilding the frame list' \
'insert:Insert a still WebP as a new frame (its image bytes copied verbatim)' \
'replace:Replace one frame'\''s image with a still WebP, keeping its placement/timing' \
    )
    _describe -t commands 'webp help mux commands' commands "$@"
}
(( $+functions[_webp__subcmd__help__subcmd__mux__subcmd__get-frame_commands] )) ||
_webp__subcmd__help__subcmd__mux__subcmd__get-frame_commands() {
    local commands; commands=()
    _describe -t commands 'webp help mux get-frame commands' commands "$@"
}
(( $+functions[_webp__subcmd__help__subcmd__mux__subcmd__insert_commands] )) ||
_webp__subcmd__help__subcmd__mux__subcmd__insert_commands() {
    local commands; commands=()
    _describe -t commands 'webp help mux insert commands' commands "$@"
}
(( $+functions[_webp__subcmd__help__subcmd__mux__subcmd__remove_commands] )) ||
_webp__subcmd__help__subcmd__mux__subcmd__remove_commands() {
    local commands; commands=()
    _describe -t commands 'webp help mux remove commands' commands "$@"
}
(( $+functions[_webp__subcmd__help__subcmd__mux__subcmd__replace_commands] )) ||
_webp__subcmd__help__subcmd__mux__subcmd__replace_commands() {
    local commands; commands=()
    _describe -t commands 'webp help mux replace commands' commands "$@"
}
(( $+functions[_webp__subcmd__help__subcmd__mux__subcmd__set_commands] )) ||
_webp__subcmd__help__subcmd__mux__subcmd__set_commands() {
    local commands; commands=()
    _describe -t commands 'webp help mux set commands' commands "$@"
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
(( $+functions[_webp__subcmd__meta_commands] )) ||
_webp__subcmd__meta_commands() {
    local commands; commands=(
'show:Show the ICC/Exif/XMP a WebP carries (kinds and byte sizes)' \
'set:Write a copy with ICC/Exif/XMP set from files (unspecified kinds are kept)' \
'strip:Write a copy with all sidecar metadata removed' \
'help:Print this message or the help of the given subcommand(s)' \
    )
    _describe -t commands 'webp meta commands' commands "$@"
}
(( $+functions[_webp__subcmd__meta__subcmd__help_commands] )) ||
_webp__subcmd__meta__subcmd__help_commands() {
    local commands; commands=(
'show:Show the ICC/Exif/XMP a WebP carries (kinds and byte sizes)' \
'set:Write a copy with ICC/Exif/XMP set from files (unspecified kinds are kept)' \
'strip:Write a copy with all sidecar metadata removed' \
'help:Print this message or the help of the given subcommand(s)' \
    )
    _describe -t commands 'webp meta help commands' commands "$@"
}
(( $+functions[_webp__subcmd__meta__subcmd__help__subcmd__help_commands] )) ||
_webp__subcmd__meta__subcmd__help__subcmd__help_commands() {
    local commands; commands=()
    _describe -t commands 'webp meta help help commands' commands "$@"
}
(( $+functions[_webp__subcmd__meta__subcmd__help__subcmd__set_commands] )) ||
_webp__subcmd__meta__subcmd__help__subcmd__set_commands() {
    local commands; commands=()
    _describe -t commands 'webp meta help set commands' commands "$@"
}
(( $+functions[_webp__subcmd__meta__subcmd__help__subcmd__show_commands] )) ||
_webp__subcmd__meta__subcmd__help__subcmd__show_commands() {
    local commands; commands=()
    _describe -t commands 'webp meta help show commands' commands "$@"
}
(( $+functions[_webp__subcmd__meta__subcmd__help__subcmd__strip_commands] )) ||
_webp__subcmd__meta__subcmd__help__subcmd__strip_commands() {
    local commands; commands=()
    _describe -t commands 'webp meta help strip commands' commands "$@"
}
(( $+functions[_webp__subcmd__meta__subcmd__set_commands] )) ||
_webp__subcmd__meta__subcmd__set_commands() {
    local commands; commands=()
    _describe -t commands 'webp meta set commands' commands "$@"
}
(( $+functions[_webp__subcmd__meta__subcmd__show_commands] )) ||
_webp__subcmd__meta__subcmd__show_commands() {
    local commands; commands=()
    _describe -t commands 'webp meta show commands' commands "$@"
}
(( $+functions[_webp__subcmd__meta__subcmd__strip_commands] )) ||
_webp__subcmd__meta__subcmd__strip_commands() {
    local commands; commands=()
    _describe -t commands 'webp meta strip commands' commands "$@"
}
(( $+functions[_webp__subcmd__mux_commands] )) ||
_webp__subcmd__mux_commands() {
    local commands; commands=(
'get-frame:Extract one frame as a standalone still WebP (bytes copied verbatim)' \
'set:Rewrite the loop count and/or background color' \
'remove:Remove one frame, rebuilding the frame list' \
'insert:Insert a still WebP as a new frame (its image bytes copied verbatim)' \
'replace:Replace one frame'\''s image with a still WebP, keeping its placement/timing' \
'help:Print this message or the help of the given subcommand(s)' \
    )
    _describe -t commands 'webp mux commands' commands "$@"
}
(( $+functions[_webp__subcmd__mux__subcmd__get-frame_commands] )) ||
_webp__subcmd__mux__subcmd__get-frame_commands() {
    local commands; commands=()
    _describe -t commands 'webp mux get-frame commands' commands "$@"
}
(( $+functions[_webp__subcmd__mux__subcmd__help_commands] )) ||
_webp__subcmd__mux__subcmd__help_commands() {
    local commands; commands=(
'get-frame:Extract one frame as a standalone still WebP (bytes copied verbatim)' \
'set:Rewrite the loop count and/or background color' \
'remove:Remove one frame, rebuilding the frame list' \
'insert:Insert a still WebP as a new frame (its image bytes copied verbatim)' \
'replace:Replace one frame'\''s image with a still WebP, keeping its placement/timing' \
'help:Print this message or the help of the given subcommand(s)' \
    )
    _describe -t commands 'webp mux help commands' commands "$@"
}
(( $+functions[_webp__subcmd__mux__subcmd__help__subcmd__get-frame_commands] )) ||
_webp__subcmd__mux__subcmd__help__subcmd__get-frame_commands() {
    local commands; commands=()
    _describe -t commands 'webp mux help get-frame commands' commands "$@"
}
(( $+functions[_webp__subcmd__mux__subcmd__help__subcmd__help_commands] )) ||
_webp__subcmd__mux__subcmd__help__subcmd__help_commands() {
    local commands; commands=()
    _describe -t commands 'webp mux help help commands' commands "$@"
}
(( $+functions[_webp__subcmd__mux__subcmd__help__subcmd__insert_commands] )) ||
_webp__subcmd__mux__subcmd__help__subcmd__insert_commands() {
    local commands; commands=()
    _describe -t commands 'webp mux help insert commands' commands "$@"
}
(( $+functions[_webp__subcmd__mux__subcmd__help__subcmd__remove_commands] )) ||
_webp__subcmd__mux__subcmd__help__subcmd__remove_commands() {
    local commands; commands=()
    _describe -t commands 'webp mux help remove commands' commands "$@"
}
(( $+functions[_webp__subcmd__mux__subcmd__help__subcmd__replace_commands] )) ||
_webp__subcmd__mux__subcmd__help__subcmd__replace_commands() {
    local commands; commands=()
    _describe -t commands 'webp mux help replace commands' commands "$@"
}
(( $+functions[_webp__subcmd__mux__subcmd__help__subcmd__set_commands] )) ||
_webp__subcmd__mux__subcmd__help__subcmd__set_commands() {
    local commands; commands=()
    _describe -t commands 'webp mux help set commands' commands "$@"
}
(( $+functions[_webp__subcmd__mux__subcmd__insert_commands] )) ||
_webp__subcmd__mux__subcmd__insert_commands() {
    local commands; commands=()
    _describe -t commands 'webp mux insert commands' commands "$@"
}
(( $+functions[_webp__subcmd__mux__subcmd__remove_commands] )) ||
_webp__subcmd__mux__subcmd__remove_commands() {
    local commands; commands=()
    _describe -t commands 'webp mux remove commands' commands "$@"
}
(( $+functions[_webp__subcmd__mux__subcmd__replace_commands] )) ||
_webp__subcmd__mux__subcmd__replace_commands() {
    local commands; commands=()
    _describe -t commands 'webp mux replace commands' commands "$@"
}
(( $+functions[_webp__subcmd__mux__subcmd__set_commands] )) ||
_webp__subcmd__mux__subcmd__set_commands() {
    local commands; commands=()
    _describe -t commands 'webp mux set commands' commands "$@"
}

if [ "$funcstack[1]" = "_webp" ]; then
    _webp "$@"
else
    compdef _webp webp
fi
