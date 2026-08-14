complete -c noirectl -f
complete -c noirectl -l json -d 'Emit schema-versioned JSON'
complete -c noirectl -s h -l help -d 'Show help'
complete -c noirectl -s V -l version -d 'Show version'
complete -c noirectl -n '__fish_use_subcommand' -a 'status devices start stop set retry diagnostics'
complete -c noirectl -n '__fish_seen_subcommand_from set' -a 'input enabled strength latency-profile fail-mode launch-at-login'
complete -c noirectl -n '__fish_seen_subcommand_from start stop retry set' -l revision -d 'Expected daemon revision'
