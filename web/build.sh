#!/usr/bin/env bash
# Build the page bundle from web/src and put it behind every published page.
#
# Needs the toolchain the lockfile was written for: Node 14 (react-scripts
# 3.4.3 and node-sass 4.14.1 do not install on a current Node).
#
# The build runs in a copy of web/, because react-scripts empties its output
# directory first and web/build also holds every published page and its
# results.json. scripts/stage_site.py then copies the bundle
# in and rewrites each page to point at it, so the bundle's file names (which
# vary with the directory it was built in) never need to be predicted.
set -euo pipefail

WEB="$(cd "$(dirname "$0")" && pwd)"
REPO="$(dirname "$WEB")"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

cp -R "$WEB/package.json" "$WEB/package-lock.json" "$WEB/src" "$WEB/public" "$WORK/"
(cd "$WORK" && npm ci --no-audit --no-fund && CI=true npm run build)
python3 "$REPO/scripts/stage_site.py" --build "$WORK/build" --site-root "$WEB/build"
