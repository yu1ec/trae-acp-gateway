#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

DRY_RUN=0
BUMP=""

usage() {
  cat >&2 <<'EOF'
Usage: scripts/release.sh <type> [--dry-run]

Types:
  patch / fix / bug / 修复       Bug fix:         0.1.2 -> 0.1.3
  minor / feature / feat / 功能  New feature:     0.1.2 -> 0.2.0
  major / breaking / 大版本      Breaking change: 0.1.2 -> 1.0.0

Examples:
  make release TYPE=patch
  make release TYPE=minor DRY_RUN=1
  ./scripts/release.sh fix --dry-run
EOF
}

for arg in "$@"; do
  case "$arg" in
    --dry-run) DRY_RUN=1 ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      if [[ -n "$BUMP" ]]; then
        echo "Unexpected argument: $arg" >&2
        usage
        exit 1
      fi
      BUMP="$arg"
      ;;
  esac
done

if [[ -z "$BUMP" ]]; then
  usage
  exit 1
fi

current_version() {
  grep '^version' Cargo.toml | head -1 | cut -d'"' -f2
}

bump_label() {
  case "$1" in
    patch|fix|bug|bugfix|修复|修复bug|补丁) echo "patch" ;;
    minor|feature|feat|功能|小版本) echo "minor" ;;
    major|breaking|break|大版本|主版本) echo "major" ;;
    *)
      echo "Unknown release type: $1" >&2
      echo "Use patch, minor, or major (or fix / feature / 大版本)." >&2
      exit 1
      ;;
  esac
}

next_version() {
  local current="$1"
  local bump="$2"
  local base="${current%%[-+]*}"
  local major minor patch

  IFS='.' read -r major minor patch <<< "$base"
  if [[ -z "$major" || -z "$minor" || -z "$patch" ]]; then
    echo "Invalid current version: $current" >&2
    exit 1
  fi

  case "$bump" in
    patch)
      patch=$((patch + 1))
      ;;
    minor)
      minor=$((minor + 1))
      patch=0
      ;;
    major)
      major=$((major + 1))
      minor=0
      patch=0
      ;;
  esac

  echo "${major}.${minor}.${patch}"
}

CURRENT="$(current_version)"
KIND="$(bump_label "$BUMP")"
NEXT="$(next_version "$CURRENT" "$KIND")"

case "$KIND" in
  patch) KIND_DESC="bug fix (patch)" ;;
  minor) KIND_DESC="feature (minor)" ;;
  major) KIND_DESC="breaking change (major)" ;;
esac

echo "Release type: $KIND_DESC"
echo "Version:      $CURRENT -> $NEXT"

if [[ "$DRY_RUN" == "1" ]]; then
  echo ""
  echo "Dry run only — no files changed."
else
  "$(dirname "$0")/bump-version.sh" "$NEXT"
  "$(dirname "$0")/check-version.sh" "$NEXT"
fi

echo ""
echo "Next steps (run manually):"
echo "  git add -A"
echo "  git commit -m \"chore: release v$NEXT\""
echo "  git tag v$NEXT"
echo "  git push origin HEAD --tags"
