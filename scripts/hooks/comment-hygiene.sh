#!/usr/bin/env bash
# Comment-hygiene instant gate — enforces the AGENTS.md "Comment Hygiene"
# rule (docs/comments.md): code comments never restate work-tracker state.
#
# What it matches (the ERE in "pat" below):
#   - "ergo <6-char ID>" / "ergo epic|task|slice <ID>" citations
#   - "epic|task|slice <ID>" citations
#   - RED/GREEN gate narration co-occurring with a 6-char ID token
# Known blind spot (accepted): bare ID-shaped all-letter tokens with no
# anchor word (e.g. a lone "WEFVGE") — those need a human/agent sweep, not
# a grep. The census doc tracks the full sweep.
#
# Waivers: one repo-relative path per line in
# scripts/hooks/comment-hygiene-waivers.txt. A waiver means "this file
# still carries pre-rule citations" — it NEVER licenses new ones; remove
# the entry in the same commit that strips the file's citations.
# Override the waiver file for testing: WAF=/dev/null <this script>.

set -euo pipefail
cd "$(git rev-parse --show-toplevel)"

waf="${WAF:-scripts/hooks/comment-hygiene-waivers.txt}"
pat='([Ee]rgo[^a-zA-Z0-9]{0,3}[A-Z0-9]{6}\b)|([Ee]rgo[^a-zA-Z0-9]{0,3}(epic|task|slice)[^a-zA-Z0-9]{0,3}[A-Z0-9]{6}\b)|((epic|task|slice)[^a-zA-Z0-9]{0,3}[A-Z0-9]*[0-9][A-Z0-9]{5}\b)|((^|[^a-zA-Z])((RED)|(GREEN))[^a-zA-Z0-9]{0,24}[A-Z0-9]{6}([^A-Z0-9]|$))|([A-Z0-9]{6}([^A-Z0-9]|$)[^a-zA-Z0-9]{0,24}((RED)|(GREEN))([^a-zA-Z0-9]|$))'

hits="$(git grep -nE "$pat" -- '*.py' '*.rs' ':(exclude)executor/**' 2>/dev/null || true)"
[ -z "$hits" ] && exit 0

fail=0
while IFS= read -r line; do
  path="${line%%:*}"
  if ! grep -Fxq "$path" "$waf"; then
    if [ "$fail" -eq 0 ]; then
      echo "comment-hygiene: work-tracker IDs / gate narration are banned in code" >&2
      echo "  (AGENTS.md 'Comment Hygiene', docs/comments.md). Strip the citation," >&2
      echo "  restate the invariant in prose, or anchor the pinning test instead:" >&2
    fi
    echo "  $line" >&2
    fail=1
  fi
done <<< "$hits"
exit "$fail"
