#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SAURON_BIN="${SAURON_BIN:-$ROOT_DIR/target/release/sauron}"
OUTPUT_FILE="${1:-$ROOT_DIR/benchmarks/benchmark-results-$(date -u +%Y%m%dT%H%M%SZ).json}"
RUNS="${BENCH_RUNS:-5}"

mkdir -p "$(dirname "$OUTPUT_FILE")"

if [[ ! -x "$SAURON_BIN" ]]; then
  echo "sauron binary not found at $SAURON_BIN" >&2
  echo "Build first with: cargo build --release" >&2
  exit 1
fi

run_case() {
  local label="$1"
  shift
  local ok=true
  local status=0
  local total_ms=0
  local run
  for run in $(seq 1 "$RUNS"); do
    local start_ms end_ms elapsed_ms
    start_ms=$(perl -MTime::HiRes=time -e 'printf("%.0f\n", time()*1000)')
    if "$@" >/dev/null 2>&1; then
      status=0
    else
      status=$?
      ok=false
    fi
    end_ms=$(perl -MTime::HiRes=time -e 'printf("%.0f\n", time()*1000)')
    elapsed_ms=$((end_ms - start_ms))
    total_ms=$((total_ms + elapsed_ms))
    if [[ "$status" -ne 0 ]]; then
      break
    fi
  done

  local avg_ms
  if [[ "$RUNS" -gt 0 ]]; then
    avg_ms=$((total_ms / RUNS))
  else
    avg_ms=0
  fi

  printf '{"label":"%s","ok":%s,"status":%d,"runs":%d,"avgMs":%d}' \
    "$label" "$ok" "$status" "$RUNS" "$avg_ms"
}

# Warm runtime for sauron scenarios.
"$SAURON_BIN" runtime start >/dev/null

results=()
results+=("$(run_case sauron_open "$SAURON_BIN" open https://example.com)")
results+=("$(run_case sauron_snapshot "$SAURON_BIN" snapshot -i)")
results+=("$(run_case sauron_get_title "$SAURON_BIN" get title)")
results+=("$(run_case sauron_wait_networkidle "$SAURON_BIN" wait --load networkidle)")

if command -v agent-browser >/dev/null 2>&1; then
  results+=("$(run_case agent_browser_open agent-browser open https://example.com)")
  results+=("$(run_case agent_browser_snapshot agent-browser snapshot -i)")
  results+=("$(run_case agent_browser_wait agent-browser wait --load networkidle)")
fi

"$SAURON_BIN" runtime stop >/dev/null || true

{
  echo '{'
  printf '  "generatedAt": "%s",\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
  printf '  "runs": [\n'
  for i in "${!results[@]}"; do
    sep=','
    if [[ "$i" -eq $((${#results[@]} - 1)) ]]; then
      sep=''
    fi
    printf '    %s%s\n' "${results[$i]}" "$sep"
  done
  echo '  ]'
  echo '}'
} >"$OUTPUT_FILE"

echo "Wrote benchmark report: $OUTPUT_FILE"
