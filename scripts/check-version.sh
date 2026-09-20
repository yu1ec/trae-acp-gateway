#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

TAG_VERSION="${1:-}"
if [[ -z "$TAG_VERSION" ]]; then
  if ! TAG_VERSION="$(git describe --tags --exact-match 2>/dev/null)"; then
    echo "Usage: scripts/check-version.sh <version-or-tag>" >&2
    echo "       or run from a tagged commit" >&2
    exit 1
  fi
fi

TAG_VERSION="${TAG_VERSION#v}"
CARGO_VERSION="$(grep '^version' Cargo.toml | head -1 | cut -d'"' -f2)"
TAURI_VERSION="$(jq -r .version app/tauri.conf.json)"

if [[ "$TAG_VERSION" != "$CARGO_VERSION" || "$TAG_VERSION" != "$TAURI_VERSION" ]]; then
  echo "Version mismatch:"
  echo "  tag:   $TAG_VERSION"
  echo "  cargo: $CARGO_VERSION"
  echo "  tauri: $TAURI_VERSION"
  exit 1
fi

echo "Version OK: $TAG_VERSION"
