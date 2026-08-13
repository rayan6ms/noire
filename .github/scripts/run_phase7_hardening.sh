#!/usr/bin/env bash
set -euo pipefail

nightly="${NOIRE_NIGHTLY_TOOLCHAIN:-nightly-2026-08-10}"

PROPTEST_CASES=4096 cargo test --release --package noire-config --package noire-model \
    --package noire-dsp --package noired --lib --locked

MIRIFLAGS="-Zmiri-strict-provenance -Zmiri-disable-isolation" \
    cargo "+$nightly" miri test --package noire-config --package noire-core \
        --package noire-dsp --package noire-model --lib --locked -- \
        --skip arbitrary_config_documents_never_panic_or_escape_validation \
        --skip arbitrary_chunking_matches_a_reference_delay \
        --skip arbitrary_fault_and_clock_sequences_remain_bounded \
        --skip arbitrary_manifest_numbers_are_rejected_or_bounded \
        --skip every_bit_pattern_becomes_finite_and_normal_or_zero \
        --skip legacy_migration_preserves_valid_finite_strength \
        --skip randomized_chunks_conserve_every_sample

RUSTFLAGS="-Zsanitizer=address" PROPTEST_CASES=1024 \
    cargo "+$nightly" test --target x86_64-unknown-linux-gnu \
    --package noire-config --package noire-dsp --package noire-model \
    --package noired --lib --locked

unsafe_pattern='(^|[^[:alnum:]_])unsafe[[:space:]]*(\{|fn|impl|trait|extern|\()'
if rg --line-number "$unsafe_pattern" crates --glob '*.rs'; then
    echo "unsafe Rust requires an explicit audited allowlist entry" >&2
    exit 1
fi

echo "NOIRE_PHASE7_HARDENING fuzz_regressions=pass miri=pass address_sanitizer=pass unsafe_findings=0"
