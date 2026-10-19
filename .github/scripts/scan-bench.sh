#!/usr/bin/env bash
# Scan speed on a CI machine's own disk (never a user's): `spaceknife scan --profile` over the same
# folders with different settings, each once with the disk cache purged (macOS: sudo purge) and once
# warm. Prints every run's numbers and a summary table at the end.
#   .github/scripts/scan-bench.sh ROOT...
set -o pipefail

roots=("$@")
py=.venv/bin/python
export SPACEKNIFE_LANGUAGE=en COLUMNS=220
rows=()

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
  rows+=("$(printf '%-24s %-5s %9s items/s  busy %4s%%  folding %4s%%  held %4s%%' "$label" "$cache" \
    "$(num 'Speed [0-9,]+ items/s' "$out")" "$(num 'busy [0-9]+%' "$out")" \
    "$(num 'results [0-9]+%' "$out")" "$(num 'limit [0-9]+%' "$out")")")
}

both() {
  one "$@" && one "$1" warm "${@:3}"
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

one "warm-up" cold # first touch of everything: neither cold nor warm, left out of the comparison
both "default" cold
for t in 4 8 16 32 64; do
  both "threads $t" cold SPACEKNIFE_SCAN_THREADS=$t
done
both "folders via fstatat" cold SPACEKNIFE_NO_BULK=dirs
both "no getattrlistbulk" cold SPACEKNIFE_NO_BULK=1
both "limit balanced" cold --disk-limit balanced

echo
echo "Summary (${roots[*]}):"
printf '%s\n' "${rows[@]}"
