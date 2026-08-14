#!/usr/bin/env bash
set -euo pipefail

hours="${1:-${NOIRE_PHASE7_SOAK_HOURS:-}}"
if [[ "$hours" != "8" && "$hours" != "15" ]]; then
    echo "usage: $0 <8|15>" >&2
    exit 2
fi
if [[ -n "$(git status --porcelain --untracked-files=normal)" ]]; then
    echo "Phase-7 acceptance soaks require a clean source tree" >&2
    exit 2
fi

commit=$(git rev-parse HEAD)
started_at=$(date --iso-8601=seconds)
printf 'NOIRE_PHASE7_SOAK_RUN event=start hours=%s commit=%s host=%s started_at=%s\n' \
    "$hours" "$commit" "$(hostname)" "$started_at"

set +e
NOIRE_PHASE7_SOAK_REALTIME=1 NOIRE_PHASE7_SOAK_HOURS="$hours" \
    cargo test --release --package noire-pipewire --test phase5_pipeline \
        release_audio_time_soak_keeps_memory_queues_and_fault_counters_bounded \
        --locked -- --ignored --nocapture
status=$?
set -e

finished_at=$(date --iso-8601=seconds)
printf 'NOIRE_PHASE7_SOAK_RUN event=finish hours=%s commit=%s finished_at=%s exit_status=%s\n' \
    "$hours" "$commit" "$finished_at" "$status"
exit "$status"
