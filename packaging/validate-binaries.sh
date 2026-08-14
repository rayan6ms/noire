#!/bin/sh
set -eu

usage() {
    echo "usage: $0 <version> <x86_64> <binary-dir>" >&2
    exit 2
}

[ "$#" -eq 3 ] || usage
version=$1
architecture=$2
binary_dir=$3

[ "$architecture" = x86_64 ] || {
    echo "Noire 1.0 package binaries must target x86_64" >&2
    exit 2
}

is_elf() {
    magic=$(dd if="$1" bs=4 count=1 2>/dev/null | od -An -tx1 | tr -d ' \n')
    [ "$magic" = 7f454c46 ]
}

for name in noire noirectl noired; do
    binary="$binary_dir/$name"
    [ -x "$binary" ] || {
        echo "missing executable package binary: $binary" >&2
        exit 1
    }
    "$binary" --version 2>/dev/null | grep -Fx "$name $version" >/dev/null || {
        echo "$name does not report package version $version" >&2
        exit 1
    }
    if ! is_elf "$binary"; then
        if [ "${NOIRE_PACKAGE_ALLOW_TEST_BINARIES:-0}" = 1 ]; then
            continue
        fi
        echo "$name is not an ELF binary; refusing a non-release package payload" >&2
        exit 1
    fi
    command -v readelf >/dev/null 2>&1 || {
        echo "readelf is required to validate release package binaries" >&2
        exit 1
    }
    readelf -h "$binary" | grep -Eq 'Class:[[:space:]]+ELF64' || {
        echo "$name is not a 64-bit ELF binary" >&2
        exit 1
    }
    readelf -h "$binary" | grep -Eq 'Machine:[[:space:]]+Advanced Micro Devices X86-64' || {
        echo "$name is not an x86_64 ELF binary" >&2
        exit 1
    }
done
