#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

VERSION=""
FROM_TAG=""
TO_REF="HEAD"
OUT=""
USE_AI=1
PUBLISH=0
DRY_RUN=0
MODEL="${AI_MODEL:-}"
# Keep release-note generation cheap: default to `high` regardless of the
# interactive session's effort, and only for this script's `claude` call.
EFFORT="${AI_EFFORT:-high}"

usage() {
  cat >&2 <<'EOF'
Usage: scripts/release-notes.sh [version] [options]

Generate Markdown release notes for the current changes and optionally publish
them to GitHub Releases. Commits since the previous tag and uncommitted or
untracked worktree changes are both included.

Arguments:
  version            Target version (default: version in Cargo.toml)
                     Accepts both 0.8.0 and v0.8.0.

Options:
  --from <tag>       Previous release tag (default: latest reachable tag)
  --to <ref>         End ref (default: HEAD)
  --out <file>       Output file (default: release-notes/v<version>.md)
  --model <name>     Model passed to the AI CLI
  --effort <level>   Effort passed to the AI CLI (low|medium|high|xhigh|max,
                     default: high). Use `none` to keep the CLI default.
  --no-ai            Skip the AI call and build notes from commit subjects
  --publish          Run `gh release edit v<version> --notes-file <file>`
  --dry-run          Print notes to stdout instead of writing the file
  -h, --help         Show this help

Environment:
  AI_CMD             Custom command that reads the prompt from stdin and
                     writes Markdown to stdout. Defaults to `claude -p`.
  AI_MODEL           Same as --model.
  AI_EFFORT          Same as --effort.

Examples:
  scripts/release-notes.sh 0.8.0
  scripts/release-notes.sh 0.8.0 --publish
  make notes VERSION=0.8.0
  make notes VERSION=0.8.0 NO_AI=1
EOF
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --from)
      FROM_TAG="${2:-}"
      [[ -n "$FROM_TAG" ]] || { echo "--from requires a tag" >&2; exit 1; }
      shift 2
      ;;
    --to)
      TO_REF="${2:-}"
      [[ -n "$TO_REF" ]] || { echo "--to requires a ref" >&2; exit 1; }
      shift 2
      ;;
    --out)
      OUT="${2:-}"
      [[ -n "$OUT" ]] || { echo "--out requires a file" >&2; exit 1; }
      shift 2
      ;;
    --model)
      MODEL="${2:-}"
      [[ -n "$MODEL" ]] || { echo "--model requires a name" >&2; exit 1; }
      shift 2
      ;;
    --effort)
      EFFORT="${2:-}"
      [[ -n "$EFFORT" ]] || { echo "--effort requires a level" >&2; exit 1; }
      shift 2
      ;;
    --no-ai) USE_AI=0; shift ;;
    --publish) PUBLISH=1; shift ;;
    --dry-run) DRY_RUN=1; shift ;;
    -h|--help) usage; exit 0 ;;
    -*)
      echo "Unknown option: $1" >&2
      usage
      exit 1
      ;;
    *)
      if [[ -n "$VERSION" ]]; then
        echo "Unexpected argument: $1" >&2
        usage
        exit 1
      fi
      VERSION="$1"
      shift
      ;;
  esac
done

if ! git rev-parse --is-inside-work-tree >/dev/null 2>&1; then
  echo "Not a git repository." >&2
  exit 1
fi

if [[ -z "$VERSION" ]]; then
  VERSION="$(grep '^version' Cargo.toml | head -1 | cut -d'"' -f2)"
fi
VERSION="${VERSION#v}"

if ! [[ "$VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?(\+[0-9A-Za-z.-]+)?$ ]]; then
  echo "Invalid version: $VERSION (expected semver like 0.8.0)" >&2
  exit 1
fi

case "$EFFORT" in
  ""|none|low|medium|high|xhigh|max) ;;
  *)
    echo "Invalid effort: $EFFORT (expected low|medium|high|xhigh|max|none)" >&2
    exit 1
    ;;
esac

if ! git rev-parse --verify --quiet "$TO_REF^{commit}" >/dev/null; then
  echo "Unknown ref: $TO_REF" >&2
  exit 1
fi

if [[ -z "$FROM_TAG" ]]; then
  FROM_TAG="$(git describe --tags --abbrev=0 "$TO_REF" 2>/dev/null || true)"
  # If the current version is already tagged, compare against the tag before it
  # so notes can still be regenerated after tagging.
  if [[ -n "$FROM_TAG" && "${FROM_TAG#v}" == "$VERSION" ]]; then
    FROM_TAG="$(git describe --tags --abbrev=0 "${FROM_TAG}^" 2>/dev/null || true)"
  fi
fi

if [[ -n "$FROM_TAG" ]]; then
  if ! git rev-parse --verify --quiet "$FROM_TAG^{commit}" >/dev/null; then
    echo "Unknown previous tag: $FROM_TAG" >&2
    exit 1
  fi
  RANGE="$FROM_TAG..$TO_REF"
else
  FROM_TAG="$(git rev-list --max-parents=0 "$TO_REF" | tail -1)"
  RANGE="$FROM_TAG..$TO_REF"
  FROM_TAG=""
fi

COMMIT_COUNT="$(git rev-list --count "$RANGE")"

# Release work is often still uncommitted when notes are generated right after
# `make release`. Include the working tree diff, minus version/lock files that
# the bump itself touches.
EXCLUDES=(
  ':(exclude)Cargo.toml'
  ':(exclude)Cargo.lock'
  ':(exclude)app/Cargo.toml'
  ':(exclude)app/tauri.conf.json'
  ':(exclude)app/frontend/package.json'
  ':(exclude)app/frontend/package-lock.json'
  ':(exclude)release-notes'
)
UNCOMMITTED_STAT="$(git diff --shortstat HEAD -- . "${EXCLUDES[@]}" 2>/dev/null || true)"
UNCOMMITTED_FILES_RAW="$(git diff --name-status HEAD -- . "${EXCLUDES[@]}" 2>/dev/null || true)"
UNTRACKED_FILES_RAW="$(git ls-files --others --exclude-standard -- . "${EXCLUDES[@]}" 2>/dev/null || true)"
UNCOMMITTED=0
if [[ -n "$UNCOMMITTED_FILES_RAW" || -n "$UNTRACKED_FILES_RAW" ]]; then
  UNCOMMITTED=1
fi

if [[ "$COMMIT_COUNT" == "0" && "$UNCOMMITTED" == "0" ]]; then
  echo "No commits in range $RANGE and no uncommitted changes; nothing to release." >&2
  exit 1
fi

if [[ -z "$OUT" ]]; then
  OUT="release-notes/v$VERSION.md"
fi

TMP_DIR="$(mktemp -d "${TMPDIR:-/tmp}/trae-release-notes.XXXXXX")"
trap 'rm -rf "$TMP_DIR"' EXIT

COMMIT_LOG="$TMP_DIR/commits.txt"
CHANGED_FILES="$TMP_DIR/files.txt"
UNCOMMITTED_FILES="$TMP_DIR/uncommitted-files.txt"
UNCOMMITTED_DIFF="$TMP_DIR/uncommitted-diff.txt"
PROMPT_FILE="$TMP_DIR/prompt.md"
AI_OUTPUT_FILE="$TMP_DIR/ai.md"

git log --no-merges --pretty=format:'- %s%n%b' "$RANGE" | sed -n '1,400p' > "$COMMIT_LOG"
git diff --name-status "$RANGE" | sed -n '1,200p' >> "$CHANGED_FILES"
if [[ "$UNCOMMITTED" == "1" ]]; then
  {
    printf '%s\n' "$UNCOMMITTED_FILES_RAW"
    if [[ -n "$UNTRACKED_FILES_RAW" ]]; then
      printf '%s\n' "$UNTRACKED_FILES_RAW" | sed 's/^/? /'
    fi
  } | sed '/^$/d' | sed -n '1,200p' > "$UNCOMMITTED_FILES"
  git diff HEAD -- . "${EXCLUDES[@]}" | sed -n '1,800p' > "$UNCOMMITTED_DIFF"
fi

DIFF_STAT="$(git diff --shortstat "$RANGE" 2>/dev/null || true)"

{
  cat <<'EOF'
You are the release manager for Trae ACP Gateway, a local desktop gateway that
exposes OpenAI / Anthropic / Gemini compatible HTTP APIs and talks to the Trae
CLI agent over ACP. The product users download is a Tauri 2 tray app
(macOS / Linux / Windows). CLI binaries are no longer published.

Write user-facing release notes in Simplified Chinese for the new version.

## Release context
EOF

  printf -- '- New version: v%s\n' "$VERSION"
  if [[ -n "$FROM_TAG" ]]; then
    printf -- '- Previous version: %s\n' "$FROM_TAG"
  else
    printf -- '- Previous version: none (first release)\n'
  fi
  printf -- '- Commits in range: %s\n' "$COMMIT_COUNT"
  if [[ "$UNCOMMITTED" == "1" ]]; then
    printf -- '- Uncommitted worktree changes: yes (part of this release)\n'
    if [[ -n "$UNCOMMITTED_STAT" ]]; then
      printf -- '- Uncommitted diff summary: %s\n' "$UNCOMMITTED_STAT"
    fi
  fi
  if [[ -n "$DIFF_STAT" ]]; then
    printf -- '- Diff summary: %s\n' "$DIFF_STAT"
  fi

  cat <<'EOF'

## Source layout
Map changed files to user-visible modules before writing:
- app/frontend/src/pages/SettingsPage.tsx, app/src/main.rs: tray app, settings, Trae CLI install status
- app/frontend/src/pages/ThoughtsPage.tsx, src/thought.rs: floating thought panel
- app/frontend/src/pages/LogsPage.tsx: gateway log viewer
- app/src/update.rs, src/update.rs: in-app update check and install
- app/src/permissions.rs: macOS system permissions
- app/frontend/src/lib/theme.ts, app/frontend/src/styles: theme and visual styling
- src/http.rs, src/agent.rs, src/acp.rs: local HTTP gateway and ACP session
- src/openai.rs, src/openai_responses.rs: OpenAI Chat Completions / Responses
- src/anthropic.rs: Anthropic Messages
- src/gemini.rs: Gemini generateContent
- src/prompt.rs, src/config.rs, src/trae_cli.rs: prompt conversion, workdir, Trae CLI
- .github/workflows, Makefile, scripts: build, signing, GitHub Releases

## Commits
EOF

  cat "$COMMIT_LOG"

  cat <<'EOF'

## Changed files
EOF

  cat "$CHANGED_FILES"

  if [[ "$UNCOMMITTED" == "1" ]]; then
    cat <<'EOF'

## Uncommitted changes (part of this release)
The worktree still has changes that are not committed yet. Treat them as part
of this release together with the commits above.

### Files
EOF
    cat "$UNCOMMITTED_FILES"
    cat <<'EOF'

### Diff (truncated)
```
EOF
    cat "$UNCOMMITTED_DIFF"
    cat <<'EOF'
```
EOF
  fi

  cat <<'EOF'

## Output requirements
1. Output Markdown body only. Do NOT wrap it in a code fence and do NOT add a
   top-level version heading; the caller adds the version header.
2. Use these sections, only when they have content:
   ### 新增功能
   ### 体验优化
   ### 问题修复
   ### 其他变更
3. Each item starts with "- " and describes the change the user can notice.
   Never mention file paths, function names, commit hashes, or internal types.
4. Merge duplicate or trivial commits. Put tests, CI, dependency bumps and
   refactors under 其他变更; omit them if users cannot notice a difference.
5. Never invent features that are not supported by the commits. Prefer a
   conservative description when unsure.
6. Keep it concise and factual; avoid marketing language.
7. Do not mention the discontinued CLI binary unless this release actually
   changes CLI behavior that existing local users would notice.
EOF
} > "$PROMPT_FILE"

ai_available() {
  if [[ -n "${AI_CMD:-}" ]]; then
    return 0
  fi
  command -v claude >/dev/null 2>&1
}

run_ai() {
  if [[ -n "${AI_CMD:-}" ]]; then
    # shellcheck disable=SC2086
    eval "$AI_CMD" < "$PROMPT_FILE"
    return
  fi

  local args=(-p --tools "" --output-format text)
  if [[ -n "$MODEL" ]]; then
    args+=(--model "$MODEL")
  fi
  if [[ -n "$EFFORT" && "$EFFORT" != "none" ]]; then
    args+=(--effort "$EFFORT")
  fi
  claude "${args[@]}" < "$PROMPT_FILE"
}

fallback_notes() {
  # Classify plain commit subjects when no AI CLI is available.
  local added="$TMP_DIR/added.txt"
  local fixed="$TMP_DIR/fixed.txt"
  local improved="$TMP_DIR/improved.txt"
  local other="$TMP_DIR/other.txt"
  : > "$added"; : > "$fixed"; : > "$improved"; : > "$other"

  local subject lower
  while IFS= read -r subject || [[ -n "$subject" ]]; do
    [[ -n "$subject" ]] || continue
    subject="$(printf '%s' "$subject" | sed -E 's/^[a-zA-Z]+(\([^)]*\))?!?:[[:space:]]*//')"
    lower="$(printf '%s' "$subject" | tr '[:upper:]' '[:lower:]')"
    case "$lower" in
      *feat*|*add*|*support*|*新增*|*添加*|*支持*) printf -- '- %s\n' "$subject" >> "$added" ;;
      *fix*|*bug*|*修复*|*修正*) printf -- '- %s\n' "$subject" >> "$fixed" ;;
      *perf*|*optimi*|*improve*|*refactor*|*优化*|*改进*|*提升*|*重构*) printf -- '- %s\n' "$subject" >> "$improved" ;;
      *) printf -- '- %s\n' "$subject" >> "$other" ;;
    esac
  done < <(git log --no-merges --pretty=format:'%s' "$RANGE")

  if [[ -s "$added" ]]; then echo "### 新增功能"; sort -u "$added"; echo; fi
  if [[ -s "$improved" ]]; then echo "### 体验优化"; sort -u "$improved"; echo; fi
  if [[ -s "$fixed" ]]; then echo "### 问题修复"; sort -u "$fixed"; echo; fi
  if [[ -s "$other" ]]; then echo "### 其他变更"; sort -u "$other"; echo; fi
}

BODY=""
if [[ "$USE_AI" == "1" ]] && ai_available; then
  echo "Generating release notes with AI..." >&2
  if run_ai > "$AI_OUTPUT_FILE" 2>"$TMP_DIR/ai.err"; then
    BODY="$(sed -E -e '/^[[:space:]]*```/d' -e '/^##[[:space:]]+Trae ACP Gateway/d' "$AI_OUTPUT_FILE")"
  else
    echo "AI generation failed; falling back to commit subjects." >&2
    sed 's/^/  /' "$TMP_DIR/ai.err" >&2 || true
  fi
else
  if [[ "$USE_AI" == "1" ]]; then
    echo "No AI CLI found (set AI_CMD or install claude); falling back to commit subjects." >&2
  fi
fi

if [[ -z "${BODY//[[:space:]]/}" ]]; then
  BODY="$(fallback_notes)"
fi

if [[ -z "${BODY//[[:space:]]/}" ]]; then
  BODY="### 其他变更
- 本次发布包含若干功能与体验调整。"
fi

{
  printf '## Trae ACP Gateway v%s\n\n' "$VERSION"
  if [[ -n "$FROM_TAG" ]]; then
    if [[ "$UNCOMMITTED" == "1" && "$COMMIT_COUNT" == "0" ]]; then
      printf '> 相对 `%s`，本次发布包含未提交的工作区改动。\n\n' "$FROM_TAG"
    elif [[ "$UNCOMMITTED" == "1" ]]; then
      printf '> 相对 `%s`，本次共 %s 个提交，并包含未提交的工作区改动。\n\n' "$FROM_TAG" "$COMMIT_COUNT"
    else
      printf '> 相对 `%s`，本次共 %s 个提交。\n\n' "$FROM_TAG" "$COMMIT_COUNT"
    fi
  else
    if [[ "$UNCOMMITTED" == "1" && "$COMMIT_COUNT" == "0" ]]; then
      printf '> 首个版本，包含未提交的工作区改动。\n\n'
    elif [[ "$UNCOMMITTED" == "1" ]]; then
      printf '> 首个版本，共 %s 个提交，并包含未提交的工作区改动。\n\n' "$COMMIT_COUNT"
    else
      printf '> 首个版本，共 %s 个提交。\n\n' "$COMMIT_COUNT"
    fi
  fi

  printf '%s\n\n' "$BODY"

  cat <<'EOF'
---

### 下载
- `Trae-ACP-Gateway-macos-universal.dmg` — macOS（Universal）
- `Trae-ACP-Gateway-linux-x86_64.AppImage` — Linux
- `Trae-ACP-Gateway-windows-x86_64.msi` — Windows
- `Trae-ACP-Gateway-windows-x86_64-setup.exe` — Windows NSIS 安装器

> CLI 形态已停止后续开发，Release 不再提供 CLI 二进制；请使用桌面 App。
>
> macOS 安装包使用固定自签名证书签名（未公证）。首次打开如提示
> “无法验证开发者”，请到「系统设置 → 隐私与安全性」点击「仍要打开」。
> 签名身份跨版本固定，已授权的完全磁盘访问等权限升级后不会重置。
>
> 应用内更新会下载 updater 产物、原地替换并自动重启。首次从旧版本升级
> 仍需手动安装一次。
>
> 首次运行如弹出「想访问 Apple Music…」，请点击「不允许」，网关不需要该权限。
EOF
} > "$TMP_DIR/notes.md"

if [[ "$DRY_RUN" == "1" ]]; then
  cat "$TMP_DIR/notes.md"
  exit 0
fi

mkdir -p "$(dirname "$OUT")"
cp "$TMP_DIR/notes.md" "$OUT"
echo "Release notes written to $OUT"

if [[ "$PUBLISH" == "1" ]]; then
  TAG="v$VERSION"
  command -v gh >/dev/null 2>&1 || {
    echo "gh CLI is not installed; cannot publish." >&2
    exit 1
  }
  if ! gh release view "$TAG" >/dev/null 2>&1; then
    echo "GitHub Release $TAG does not exist yet; create and push the tag first, then run:" >&2
    echo "  gh release edit $TAG --notes-file $OUT" >&2
    exit 1
  fi
  gh release edit "$TAG" --notes-file "$OUT"
  echo "Published release notes to GitHub Release $TAG"
fi
