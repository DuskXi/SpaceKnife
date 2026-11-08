#!/usr/bin/env bash
# Which CI jobs a change needs. Reads the changed paths (one per line) on stdin and prints
# rust=true|false, desktop=true|false and electron=true|false, the format of $GITHUB_OUTPUT.
# A path not listed here runs the Rust job: running a spare job is cheaper than skipping a needed one.
set -euo pipefail

rust=false
desktop=false
electron=false
while IFS= read -r path; do
  [ -n "$path" ] || continue
  case "$path" in
    # read by every job: the workflow itself, the design tokens and icons (generated into design/build,
    # which the interface imports and the layout tests compare with), the developer tools and their
    # tests (they read crates/ and design/), and the TypeScript protocol generated from sk-protocol
    .github/workflows/ci.yml | .github/scripts/ci-areas.sh | design/tokens/* | design/icons/* | design/build/* \
      | tools/* | crates/sk-protocol/* | apps/desktop/app/protocol/generated.ts)
      rust=true
      desktop=true
      electron=true
      ;;
    # the Tauri shell is a crate of the workspace (cargo test / clippy); its interface is the desktop job's
    apps/desktop/src-tauri/*)
      rust=true
      ;;
    # the host contract and the modules the Electron shell's main process imports from the interface
    apps/desktop/app/shared/*)
      desktop=true
      electron=true
      ;;
    apps/desktop/*)
      desktop=true
      ;;
    # the Electron shell (main process, preload, packaging configuration)
    apps/electron-shell/*)
      electron=true
      ;;
    # the help pages the interface bundles (@site), and their pictures
    site/src/content/* | site/src/assets/*)
      desktop=true
      ;;
    docs/insights.md) # sk-core's insights tests check the rules library against it
      rust=true
      ;;
    # the Windows console launcher is Rust the Rust job builds and tests (ci.yml)
    packaging/console-launcher/*)
      rust=true
      ;;
    # documentation, the website, artwork, packaging tests and repository meta: nothing in CI builds or
    # tests them (the packaging tests run in the Portable and macOS build workflows)
    docs/* | site/* | design/app-icon/* | design/social/* | design/README.md | packaging/* \
      | README.md | README.zh-CN.md | CONTRIBUTING.md | CODE_OF_CONDUCT.md | SECURITY.md | TODO.md | LICENSE \
      | UNRELEASED.md | .github/ISSUE_TEMPLATE/* | .github/pull_request_template.md | .github/workflows/pages.yml \
      | .github/workflows/macos.yml | .github/workflows/portable.yml | .github/workflows/release.yml \
      | .github/workflows/release-check.yml | release-notes/*) ;;
    *)
      rust=true
      ;;
  esac
done

echo "rust=$rust"
echo "desktop=$desktop"
echo "electron=$electron"
