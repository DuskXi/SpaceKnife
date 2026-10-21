#!/usr/bin/env bash
# Scan speed on a CI machine's own disk (never a user's): `spaceknife scan --profile` over the same
# folders with different settings. The settings take turns and each round starts with a different
# one, so drift on a shared runner and the order spread over all of them.
# Prints every run's numbers and, per setting, the mean and range.
#   BENCH_CACHE=warm|cold BENCH_ROUNDS=4 .github/scripts/scan-bench.sh ROOT...
set -o pipefail

roots=("$@")
py=.venv/bin/python
export SPACEKNIFE_LANGUAGE=en COLUMNS=220
data=$(mktemp)

num() { grep -oE "$1" <<<"$2" | head -1 | grep -oE '[0-9][0-9,.]*' | head -1 | tr -d ,; }

one() { # label, cache (cold|warm), then VAR=value settings and extra arguments
  local label=$1 cache=$2
  shift 2
  local envs=() args=() a
  for a in "$@"; do
    case $a in
      *=*) envs+=("$a") ;;
      *) args+=("$a") ;;
    esac
  done
  if [ "$cache" = cold ]; then
    sudo purge 2>/dev/null || true
  fi
  local out
  out=$(env "${envs[@]}" "$py" -X utf8 -m spaceknife scan "${roots[@]}" --profile --no-snapshot --top 1 --depth 1 \
    "${args[@]}" 2>&1)
  echo "::group::$label ($cache)"
  grep -E 'Status|Items|walker|busy|folding|batches|Held|Bulk' <<<"$out" | sed 's/[│┃]//g; s/  */ /g'
  echo "::endgroup::"
  printf '%s|%s|%s|%s|%s|%s\n' "$label" "$cache" "$(num 'Speed [0-9,]+ items/s' "$out")" \
    "$(num 'busy [0-9]+%' "$out")" "$(num 'results [0-9]+%' "$out")" "$(num 'limit [0-9]+%' "$out")" >>"$data"
}

"$py" - <<'EOF'
import os
import sys

from spaceknife.scan.media import media_of
from spaceknife.scan.scanner import native_threads
from spaceknife.scan.volume import volume_for

vol = volume_for("/")
print(f"cpus {os.cpu_count()}, default listing threads {native_threads()}, {sys.platform}")
if vol is not None:
    print(f"/ : {vol.fs_type} on {vol.device}, media {media_of(vol.mount_point, vol.device, vol.is_remote)}")
EOF

cache=${BENCH_CACHE:-warm} # warm: the same folders again; cold: disk cache purged before each run
one "warm-up" cold # first touch of everything, left out of the comparison

if [ "$cache" = warm ]; then
  # what the system's own single-threaded walkers take over the same folders
  "$py" - "${roots[@]}" <<'EOF2'
import subprocess
import sys
import time

roots = sys.argv[1:]
for cmd in (["find", *roots, "-xdev"], ["du", "-skx", *roots]):
    t = time.perf_counter()
    out = subprocess.run(cmd, capture_output=True, check=False)
    dt = time.perf_counter() - t
    paths = out.stdout.count(b"\n")
    rate = f", {paths:,} paths, {paths / dt:,.0f}/s" if cmd[0] == "find" else ""
    print(f"{cmd[0]}: {dt:.1f} s{rate}")
EOF2
fi

cpus=$(getconf _NPROCESSORS_ONLN)
configs=(
  "default|"
  "threads = CPUs (before)|SPACEKNIFE_SCAN_THREADS=$cpus"
  "folders from getattrlistbulk|SPACEKNIFE_BULK_DIRS=1"
)
n=${#configs[@]}
for r in $(seq 0 $((${BENCH_ROUNDS:-4} - 1))); do # each round starts one setting later (no order bias)
  for i in $(seq 0 $((n - 1))); do
    c=${configs[$(((i + r) % n))]}
    # shellcheck disable=SC2086 # the settings split on spaces
    one "${c%%|*}" "$cache" ${c#*|}
  done
done

echo
echo "Summary (${roots[*]}; items/s mean [min–max], listing threads busy, main thread folding, held by the limit):"
awk -F'|' '
  { k = $1 " (" $2 ")"; if (!(k in n)) order[++m] = k
    n[k]++; s[k] += $3; b[k] += $4; f[k] += $5; h[k] += $6
    if (!(k in lo) || $3 < lo[k]) lo[k] = $3
    if (!(k in hi) || $3 > hi[k]) hi[k] = $3 }
  END { for (i = 1; i <= m; i++) { k = order[i]
    printf "%-30s %8.0f [%s–%s]  busy %3.0f%%  folding %3.0f%%  held %3.0f%%  (%d runs)\n",
      k, s[k] / n[k], lo[k], hi[k], b[k] / n[k], f[k] / n[k], h[k] / n[k], n[k] } }' "$data"
