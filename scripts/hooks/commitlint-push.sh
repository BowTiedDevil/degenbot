#!/usr/bin/env bash
#
# pre-push safety net: re-lint every commit message in the outgoing push range.
#
# Invoked by prek's pre-push hook (see prek.toml). prek parses git's push-line
# stdin itself and does not forward it to hooks, so the range is computed from
# the upstream tracking ref instead:
#
#   @{u}          — the upstream tracking branch, last-fetched remote state.
#                   `@{u}..HEAD` is exactly the set of commits about to be pushed.
#   origin/HEAD   — fallback for a brand-new branch with no upstream yet; we lint
#                   back to its merge-base so only this branch's new commits are
#                   checked, not shared history that already shipped.
#   HEAD^         — last resort.
#
# Mirrors `just lint-commits` (default range @{push}..HEAD). @{push} is not yet
# resolvable during a pre-push hook, so @{u} is used here. To lint a range
# manually: `just lint-commits` or `just lint-commits <from>..<to>`.

set -euo pipefail

if ! command -v npx >/dev/null 2>&1; then
  echo "⚠  npx not found; skipping pre-push commit-message lint." >&2
  exit 0
fi

from="$(git rev-parse --symbolic-full-name '@{u}' 2>/dev/null || true)"
if [ -z "$from" ]; then
  from="$(git merge-base origin/HEAD HEAD 2>/dev/null || true)"
fi
if [ -z "$from" ]; then
  from="HEAD^"
fi

# Nothing to lint (e.g. branch already in sync with its upstream).
if [ -z "$(git rev-list "$from..HEAD" 2>/dev/null || true)" ]; then
  exit 0
fi

if ! npx --yes @commitlint/cli --from "$from" --to HEAD >&2; then
  echo "" >&2
  echo "💡 Push blocked: one or more commit messages fail commitlint." >&2
  echo "   Fix with: git commit --amend (tip) or git rebase -i." >&2
  echo "   Bypass locally: git push --no-verify (CI will still reject on PR)." >&2
  exit 1
fi