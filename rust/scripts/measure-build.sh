#!/usr/bin/env bash
set -Eeuo pipefail

if [[ $# -eq 0 ]]; then
  echo "usage: $0 <command> [args...]" >&2
  exit 2
fi

limit_seconds="${BUILD_TIME_LIMIT_SECONDS:-1200}"
limit_mib="${BUILD_MEMORY_LIMIT_MIB:-7168}"
report="${BUILD_METRICS_FILE:-target/build-metrics/wasm-build-time.txt}"
mkdir -p "$(dirname "$report")"

if [[ ! -x /usr/bin/time ]]; then
  echo "GNU /usr/bin/time is required to measure peak RSS." >&2
  exit 2
fi

start="$(date +%s)"
set +e
/usr/bin/time -v -o "$report" "$@"
status=$?
set -e
elapsed=$(( $(date +%s) - start ))

cat "$report" >&2
peak_kib="$(awk -F': ' '/Maximum resident set size/ { print $2; exit }' "$report")"
if [[ -z "${peak_kib:-}" ]]; then
  echo "Could not read GNU time peak RSS from $report" >&2
  exit 3
fi
peak_mib=$(( (peak_kib + 1023) / 1024 ))

echo "BUILD_METRICS elapsed_seconds=$elapsed peak_rss_mib=$peak_mib command_status=$status"
if [[ $status -ne 0 ]]; then
  exit "$status"
fi
if (( elapsed > limit_seconds )); then
  echo "Build exceeded time ceiling: ${elapsed}s > ${limit_seconds}s" >&2
  exit 4
fi
if (( peak_mib > limit_mib )); then
  echo "Build exceeded memory ceiling: ${peak_mib} MiB > ${limit_mib} MiB" >&2
  exit 5
fi
