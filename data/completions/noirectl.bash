_noirectl() {
    local current="${COMP_WORDS[COMP_CWORD]}"
    local commands="status devices start stop set retry diagnostics"
    local settings="input enabled strength latency-profile fail-mode launch-at-login"
    if [[ ${COMP_CWORD} -eq 1 ]]; then
        COMPREPLY=( $(compgen -W "--json --help --version ${commands}" -- "${current}") )
    elif [[ ${COMP_CWORD} -eq 2 && ${COMP_WORDS[1]} == set ]]; then
        COMPREPLY=( $(compgen -W "--revision ${settings}" -- "${current}") )
    else
        COMPREPLY=( $(compgen -W "--revision --help" -- "${current}") )
    fi
}
complete -F _noirectl noirectl
