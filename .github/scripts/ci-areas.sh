#!/usr/bin/env bash
# Which CI jobs a change needs. Reads the changed paths (one per line) on stdin and prints
# python=true|false and electron=true|false, the format of $GITHUB_OUTPUT.
# A path not listed here runs the Python job: running a spare job is cheaper than skipping a needed one.
set -euo pipefail

python=false
electron=false
while IFS= read -r path; do
  [ -n "$path" ] || continue
  case "$path" in
    # read by both jobs: the workflow itself, the design tokens and icons (generated into design/build,
    # which the renderer imports), the translation checker, and the TS protocol generated from Python
    .github/workflows/ci.yml | .github/scripts/ci-areas.sh | design/tokens/* | design/icons/* | design/build/* \
      | tools/i18n_check.py | tools/i18n_scan_ts.mjs | apps/electron/src/renderer/src/protocol/generated.ts)
      python=true
      electron=true
      ;;
    apps/electron/*)
      electron=true
      ;;
    docs/insights.md) # test_insights checks the rules library against it
      python=true
      ;;
    # documentation, the website, artwork and repository meta: nothing in CI builds or tests them
    docs/* | site/* | design/app-icon/* | design/social/* | design/README.md \
      | README.md | README.zh-CN.md | CONTRIBUTING.md | CODE_OF_CONDUCT.md | SECURITY.md | TODO.md | LICENSE \
      | .github/ISSUE_TEMPLATE/* | .github/pull_request_template.md | .github/workflows/pages.yml \
      | .github/workflows/macos.yml | .github/workflows/portable.yml) ;;
    *)
      python=true
      ;;
  esac
done

echo "python=$python"
echo "electron=$electron"
