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
            webp,convert)
                cmd="webp__subcmd__convert"
                ;;
            webp,decode)
                cmd="webp__subcmd__decode"
                ;;
            webp,encode)
                cmd="webp__subcmd__encode"
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
            webp__subcmd__help,completions)
                cmd="webp__subcmd__help__subcmd__completions"
                ;;
            webp__subcmd__help,convert)
                cmd="webp__subcmd__help__subcmd__convert"
                ;;
            webp__subcmd__help,decode)
                cmd="webp__subcmd__help__subcmd__decode"
                ;;
            webp__subcmd__help,encode)
                cmd="webp__subcmd__help__subcmd__encode"
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
            *)
                ;;
        esac
    done

    case "${cmd}" in
        webp)
            opts="-v -q -h -V --verbose --quiet --color --help --version decode encode convert info completions man help"
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 1 ]] ; then
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
        webp__subcmd__completions)
            opts="-v -q -h --verbose --quiet --color --help bash elvish fish powershell zsh"
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 2 ]] ; then
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
        webp__subcmd__convert)
            opts="-o -m -r -v -q -h --output --method --lossy --quality --optimize --recursive --metadata --verbose --quiet --color --help"
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
                --metadata)
                    COMPREPLY=($(compgen -W "all none icc exif xmp" -- "${cur}"))
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
        webp__subcmd__decode)
            opts="-o -v -q -h --output --format --layout --frames --frame --metadata --verbose --quiet --color --help"
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
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        webp__subcmd__encode)
            opts="-o -m -v -q -h --output --input-format --width --height --layout --method --lossy --quality --metadata --verbose --quiet --color --help"
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
                    COMPREPLY=($(compgen -W "png ppm pam raw" -- "${cur}"))
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
                --metadata)
                    COMPREPLY=($(compgen -W "all none icc exif xmp" -- "${cur}"))
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
        webp__subcmd__help)
            opts="decode encode convert info completions man help"
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
            opts="-v -q -h --json --verbose --quiet --color --help"
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 2 ]] ; then
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
        webp__subcmd__man)
            opts="-v -q -h --verbose --quiet --color --help"
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 2 ]] ; then
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
    esac
}

if [[ "${BASH_VERSINFO[0]}" -eq 4 && "${BASH_VERSINFO[1]}" -ge 4 || "${BASH_VERSINFO[0]}" -gt 4 ]]; then
    complete -F _webp -o nosort -o bashdefault -o default webp
else
    complete -F _webp -o bashdefault -o default webp
fi
