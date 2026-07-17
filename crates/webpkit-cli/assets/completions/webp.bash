_webp() {
    local i cur prev opts cmd
    COMPREPLY=()
    if [[ "${BASH_VERSINFO[0]}" -ge 4 ]]; then
        cur="$2"
    else
        cur="${COMP_WORDS[COMP_CWORD]}"
    fi
    prev="$3"
    cmd=""
    opts=""

    for i in "${COMP_WORDS[@]:0:COMP_CWORD}"
    do
        case "${cmd},${i}" in
            ",$1")
                cmd="webp"
                ;;
            webp,completions)
                cmd="webp__subcmd__completions"
                ;;
            webp,config)
                cmd="webp__subcmd__config"
                ;;
            webp,convert)
                cmd="webp__subcmd__convert"
                ;;
            webp,decode)
                cmd="webp__subcmd__decode"
                ;;
            webp,diff)
                cmd="webp__subcmd__diff"
                ;;
            webp,doctor)
                cmd="webp__subcmd__doctor"
                ;;
            webp,encode)
                cmd="webp__subcmd__encode"
                ;;
            webp,explain)
                cmd="webp__subcmd__explain"
                ;;
            webp,help)
                cmd="webp__subcmd__help"
                ;;
            webp,info)
                cmd="webp__subcmd__info"
                ;;
            webp,man)
                cmd="webp__subcmd__man"
                ;;
            webp__subcmd__config,get)
                cmd="webp__subcmd__config__subcmd__get"
                ;;
            webp__subcmd__config,help)
                cmd="webp__subcmd__config__subcmd__help"
                ;;
            webp__subcmd__config__subcmd__help,get)
                cmd="webp__subcmd__config__subcmd__help__subcmd__get"
                ;;
            webp__subcmd__config__subcmd__help,help)
                cmd="webp__subcmd__config__subcmd__help__subcmd__help"
                ;;
            webp__subcmd__help,completions)
                cmd="webp__subcmd__help__subcmd__completions"
                ;;
            webp__subcmd__help,config)
                cmd="webp__subcmd__help__subcmd__config"
                ;;
            webp__subcmd__help,convert)
                cmd="webp__subcmd__help__subcmd__convert"
                ;;
            webp__subcmd__help,decode)
                cmd="webp__subcmd__help__subcmd__decode"
                ;;
            webp__subcmd__help,diff)
                cmd="webp__subcmd__help__subcmd__diff"
                ;;
            webp__subcmd__help,doctor)
                cmd="webp__subcmd__help__subcmd__doctor"
                ;;
            webp__subcmd__help,encode)
                cmd="webp__subcmd__help__subcmd__encode"
                ;;
            webp__subcmd__help,explain)
                cmd="webp__subcmd__help__subcmd__explain"
                ;;
            webp__subcmd__help,help)
                cmd="webp__subcmd__help__subcmd__help"
                ;;
            webp__subcmd__help,info)
                cmd="webp__subcmd__help__subcmd__info"
                ;;
            webp__subcmd__help,man)
                cmd="webp__subcmd__help__subcmd__man"
                ;;
            webp__subcmd__help__subcmd__config,get)
                cmd="webp__subcmd__help__subcmd__config__subcmd__get"
                ;;
            *)
                ;;
        esac
    done

    case "${cmd}" in
        webp)
            opts="-v -o -q -m -r -h -V --verbose --quiet --color --threads --dry-run --output --quality --lossless --lossy --method --metadata --crop --resize --recursive --force --no-clobber --help --version decode encode convert info diff doctor config explain completions man help"
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 1 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                --color)
                    COMPREPLY=($(compgen -W "auto always never" -- "${cur}"))
                    return 0
                    ;;
                --threads)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --output)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                -o)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --quality)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                -q)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --method)
                    COMPREPLY=($(compgen -W "fast balanced best" -- "${cur}"))
                    return 0
                    ;;
                -m)
                    COMPREPLY=($(compgen -W "fast balanced best" -- "${cur}"))
                    return 0
                    ;;
                --metadata)
                    COMPREPLY=($(compgen -W "all none icc exif xmp" -- "${cur}"))
                    return 0
                    ;;
                --crop)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --resize)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        webp__subcmd__completions)
            opts="-v -h --verbose --quiet --color --threads --dry-run --help bash elvish fish powershell zsh"
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 2 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                --color)
                    COMPREPLY=($(compgen -W "auto always never" -- "${cur}"))
                    return 0
                    ;;
                --threads)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        webp__subcmd__config)
            opts="-v -h --json --template --quality --effort --codec --metadata --threads --max-pixels --verbose --quiet --color --dry-run --help get help"
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 2 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                --quality)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --effort)
                    COMPREPLY=($(compgen -W "fast balanced best" -- "${cur}"))
                    return 0
                    ;;
                --codec)
                    COMPREPLY=($(compgen -W "lossless lossy" -- "${cur}"))
                    return 0
                    ;;
                --metadata)
                    COMPREPLY=($(compgen -W "all none icc exif xmp" -- "${cur}"))
                    return 0
                    ;;
                --threads)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --max-pixels)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --color)
                    COMPREPLY=($(compgen -W "auto always never" -- "${cur}"))
                    return 0
                    ;;
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        webp__subcmd__config__subcmd__get)
            opts="-v -h --verbose --quiet --color --dry-run --help"
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 3 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                --color)
                    COMPREPLY=($(compgen -W "auto always never" -- "${cur}"))
                    return 0
                    ;;
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        webp__subcmd__config__subcmd__help)
            opts="get help"
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 3 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        webp__subcmd__config__subcmd__help__subcmd__get)
            opts=""
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 4 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        webp__subcmd__config__subcmd__help__subcmd__help)
            opts=""
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 4 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        webp__subcmd__convert)
            opts="-o -m -q -r -v -h --output --method --lossless --lossy --quality --optimize --recursive --metadata --force --no-clobber --verbose --quiet --color --threads --dry-run --help"
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 2 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                --output)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                -o)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --method)
                    COMPREPLY=($(compgen -W "fast balanced best" -- "${cur}"))
                    return 0
                    ;;
                -m)
                    COMPREPLY=($(compgen -W "fast balanced best" -- "${cur}"))
                    return 0
                    ;;
                --quality)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                -q)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --metadata)
                    COMPREPLY=($(compgen -W "all none icc exif xmp" -- "${cur}"))
                    return 0
                    ;;
                --color)
                    COMPREPLY=($(compgen -W "auto always never" -- "${cur}"))
                    return 0
                    ;;
                --threads)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        webp__subcmd__decode)
            opts="-o -v -h --output --format --layout --frames --frame --metadata --verbose --quiet --color --threads --dry-run --help"
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 2 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                --output)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                -o)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --format)
                    COMPREPLY=($(compgen -W "png ppm pam raw" -- "${cur}"))
                    return 0
                    ;;
                --layout)
                    COMPREPLY=($(compgen -W "rgba8 argb8 bgra8" -- "${cur}"))
                    return 0
                    ;;
                --frames)
                    COMPREPLY=($(compgen -W "first all" -- "${cur}"))
                    return 0
                    ;;
                --frame)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --metadata)
                    COMPREPLY=($(compgen -W "all none icc exif xmp" -- "${cur}"))
                    return 0
                    ;;
                --color)
                    COMPREPLY=($(compgen -W "auto always never" -- "${cur}"))
                    return 0
                    ;;
                --threads)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        webp__subcmd__diff)
            opts="-v -h --min-psnr --json --verbose --quiet --color --threads --dry-run --help"
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 2 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                --min-psnr)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --color)
                    COMPREPLY=($(compgen -W "auto always never" -- "${cur}"))
                    return 0
                    ;;
                --threads)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        webp__subcmd__doctor)
            opts="-v -h --verbose --quiet --color --threads --dry-run --help"
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 2 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                --color)
                    COMPREPLY=($(compgen -W "auto always never" -- "${cur}"))
                    return 0
                    ;;
                --threads)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        webp__subcmd__encode)
            opts="-o -m -q -v -h --output --input-format --width --height --layout --method --lossless --lossy --quality --crop --resize --target-size --min-psnr --metadata --verbose --quiet --color --threads --dry-run --help"
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 2 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                --output)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                -o)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --input-format)
                    COMPREPLY=($(compgen -W "png ppm pam jpeg gif tiff bmp raw" -- "${cur}"))
                    return 0
                    ;;
                --width)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --height)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --layout)
                    COMPREPLY=($(compgen -W "rgba8 argb8 bgra8" -- "${cur}"))
                    return 0
                    ;;
                --method)
                    COMPREPLY=($(compgen -W "fast balanced best" -- "${cur}"))
                    return 0
                    ;;
                -m)
                    COMPREPLY=($(compgen -W "fast balanced best" -- "${cur}"))
                    return 0
                    ;;
                --quality)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                -q)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --crop)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --resize)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --target-size)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --min-psnr)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --metadata)
                    COMPREPLY=($(compgen -W "all none icc exif xmp" -- "${cur}"))
                    return 0
                    ;;
                --color)
                    COMPREPLY=($(compgen -W "auto always never" -- "${cur}"))
                    return 0
                    ;;
                --threads)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        webp__subcmd__explain)
            opts="-v -h --verbose --quiet --color --threads --dry-run --help"
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 2 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                --color)
                    COMPREPLY=($(compgen -W "auto always never" -- "${cur}"))
                    return 0
                    ;;
                --threads)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        webp__subcmd__help)
            opts="decode encode convert info diff doctor config explain completions man help"
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 2 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        webp__subcmd__help__subcmd__completions)
            opts=""
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 3 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        webp__subcmd__help__subcmd__config)
            opts="get"
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 3 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        webp__subcmd__help__subcmd__config__subcmd__get)
            opts=""
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 4 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        webp__subcmd__help__subcmd__convert)
            opts=""
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 3 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        webp__subcmd__help__subcmd__decode)
            opts=""
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 3 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        webp__subcmd__help__subcmd__diff)
            opts=""
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 3 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        webp__subcmd__help__subcmd__doctor)
            opts=""
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 3 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        webp__subcmd__help__subcmd__encode)
            opts=""
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 3 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        webp__subcmd__help__subcmd__explain)
            opts=""
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 3 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        webp__subcmd__help__subcmd__help)
            opts=""
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 3 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        webp__subcmd__help__subcmd__info)
            opts=""
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 3 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        webp__subcmd__help__subcmd__man)
            opts=""
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 3 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        webp__subcmd__info)
            opts="-v -h --json --verbose --quiet --color --threads --dry-run --help"
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 2 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                --color)
                    COMPREPLY=($(compgen -W "auto always never" -- "${cur}"))
                    return 0
                    ;;
                --threads)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        webp__subcmd__man)
            opts="-v -h --verbose --quiet --color --threads --dry-run --help"
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 2 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                --color)
                    COMPREPLY=($(compgen -W "auto always never" -- "${cur}"))
                    return 0
                    ;;
                --threads)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
    esac
}

if [[ "${BASH_VERSINFO[0]}" -eq 4 && "${BASH_VERSINFO[1]}" -ge 4 || "${BASH_VERSINFO[0]}" -gt 4 ]]; then
    complete -F _webp -o nosort -o bashdefault -o default webp
else
    complete -F _webp -o bashdefault -o default webp
fi
