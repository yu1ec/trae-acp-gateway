#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

VERSION="${1:-}"
if [[ -z "$VERSION" ]]; then
  echo "Usage: scripts/bump-version.sh <version>" >&2
  echo "Example: scripts/bump-version.sh 0.1.2" >&2
  exit 1
fi

VERSION="${VERSION#v}"

if ! [[ "$VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?(\+[0-9A-Za-z.-]+)?$ ]]; then
  echo "Invalid version: $VERSION (expected semver like 0.1.2)" >&2
  exit 1
fi

for f in Cargo.toml app/Cargo.toml; do
  perl -pi -e 's/^version = ".*"/version = "'"$VERSION"'"/' "$f"
done

tmp="$(mktemp)"
jq --arg v "$VERSION" '.version = $v' app/tauri.conf.json > "$tmp"
mv "$tmp" app/tauri.conf.json

(
  cd app/frontend
  npm version "$VERSION" --no-git-tag-version --allow-same-version
)

echo "Bumped to $VERSION:"
echo "  Cargo.toml"
echo "  app/Cargo.toml"
echo "  app/tauri.conf.json"
echo "  app/frontend/package.json (+ package-lock.json)"
if cargo check; then
  echo "  Cargo.lock (updated)"
else
  echo "  Cargo.lock (skipped — run 'cargo check' manually)" >&2
fi
