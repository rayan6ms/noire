#!/bin/sh
set -eu

repo_dir=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
[ "$#" -le 1 ] || {
    echo "usage: $0 [output.pot]" >&2
    exit 2
}
output=${1:-"$repo_dir/po/noire.pot"}
temporary=$(mktemp)
portable_input=$(mktemp)
trap 'rm -f -- "$temporary" "$portable_input" "$output.tmp"' EXIT HUP INT TERM

# Ubuntu 24.04 predates xgettext's Rust parser. Every translatable call is kept
# as a one-line tr("literal"), allowing its C parser to extract those lines while
# newer hosts use the native Rust parser.
source_file="$repo_dir/crates/noire-ui/src/app.rs"
if xgettext --help | grep -q 'Rust'; then
    language=Rust
    input=$source_file
else
    if grep -nE '(^|[^[:alnum:]_])tr\([^\"]' "$source_file"; then
        echo 'translatable tr() calls must contain a same-line string literal' >&2
        exit 1
    fi
    grep -E '(^|[^[:alnum:]_])tr\("' "$source_file" >"$portable_input"
    language=C
    input=$portable_input
fi

xgettext \
    --language="$language" \
    --from-code=UTF-8 \
    --keyword=tr \
    --sort-by-file \
    --no-location \
    --omit-header \
    --output="$temporary" \
    "$input"

{
    printf '%s\n' \
        '# Noire GTK translation template.' \
        'msgid ""' \
        'msgstr ""' \
        '"Project-Id-Version: Noire 1.0.0\n"' \
        '"Report-Msgid-Bugs-To: https://github.com/rayan6ms/noire/issues\n"' \
        '"POT-Creation-Date: 2026-08-13 00:00+0000\n"' \
        '"PO-Revision-Date: YEAR-MO-DA HO:MI+ZONE\n"' \
        '"Last-Translator: FULL NAME <EMAIL@ADDRESS>\n"' \
        '"Language-Team: LANGUAGE <LL@li.org>\n"' \
        '"Language: \n"' \
        '"MIME-Version: 1.0\n"' \
        '"Content-Type: text/plain; charset=UTF-8\n"' \
        '"Content-Transfer-Encoding: 8bit\n"' \
        ''
    cat "$temporary"
} >"$output.tmp"
mv "$output.tmp" "$output"
