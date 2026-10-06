#!/usr/bin/env bash
# Comment-hygiene instant gate — enforces the AGENTS.md "Comment Hygiene"
# rule (docs/comments.md): code comments never restate work-tracker state.
#
# Detector 1 — anchored citations (the regex in "pat" below):
#   - "ergo <6-char ID>" / "ergo epic|task|slice <ID>" citations
#   - "epic|task|slice <ID>" citations
#   - RED/GREEN gate narration co-occurring with a 6-char ID token
#   Scope: the working tree, not the index. The search runs rg (same tool
#   and ignore rules as Detector 2) over the whole repo for *.py and *.rs,
#   excluding executor/** (a self-contained sub-workspace with its own
#   conventions), so a brand-new untracked source file is examined. An
#   index-bound search can certify a tree it never looked at — a green
#   that examined nothing — which is the failure this gate exists to
#   prevent. One rg pass; no per-token subprocesses.
#   Deliberately out of scope here: shell and markdown. *.sh is covered
#   by Detector 2's bare-token ratchet below, and markdown is where
#   tracker discussion legitimately lives (docs/, this gate's own
#   census/waiver files).
#   Waivers: one repo-relative path per line in
#   scripts/hooks/comment-hygiene-waivers.txt. A waiver means "this file
#   still carries pre-rule citations" — it NEVER licenses new ones; remove
#   the entry in the same commit that strips the file's citations.
#   Override the waiver file for testing: WAF=/dev/null <this script>.
#
# Detector 2 — the bare-ID census ratchet (the formerly accepted blind
# spot): bare six-char [A-Z0-9]{6} tracker tokens with no anchor word.
# Any such token NOT accounted for in
# scripts/hooks/comment-hygiene-census.txt fails the gate, so new citations
# cannot land while existing ones are grandfathered until swept (shrink
# protocol in the census header). Exclusions reuse the sweep commits'
# classification vocabulary:
#   - census lexicon entries (legit identifiers: SIGINT, BEFORE, STATIC,
#     OUTPUT, INPUT, STABLE, WETH, USDC, EVM opcodes, linter codes, ...)
#   - pure-number tokens (tick values, block numbers)
#   - hex-shaped tokens ([0-9A-F]{6} selector/byte data); a census citation
#     entry is checked first, so a deliberate hex-spelled citation stays
#     ratcheted
#   - anything inside a longer word or identifier: the word-boundary rule
#     excludes SCREAMING_SNAKE members and 0x-prefixed hex fragments
#   Scope: src/ tests/ examples/ (all lines), scripts/ and run_bot.sh
#   (all lines), and rust/crates/*.rs comment lines only, excluding rust
#   tests/benches/examples and the gate's own census/hook/waiver files.
#   Keep it fast (it runs in pre-commit): one rg pass + one awk pass, no
#   per-token subprocesses.
#   Override the census for testing: CHF=/dev/null <this script> (fails on
#   every bare token).

set -euo pipefail
cd "$(git rev-parse --show-toplevel)"

waf="${WAF:-scripts/hooks/comment-hygiene-waivers.txt}"
census="${CHF:-scripts/hooks/comment-hygiene-census.txt}"
pat='([Ee]rgo[^a-zA-Z0-9]{0,3}[A-Z0-9]{6}\b)|([Ee]rgo[^a-zA-Z0-9]{0,3}(epic|task|slice)[^a-zA-Z0-9]{0,3}[A-Z0-9]{6}\b)|((epic|task|slice)[^a-zA-Z0-9]{0,3}[A-Z0-9]*[0-9][A-Z0-9]{5}\b)|((^|[^a-zA-Z])((RED)|(GREEN))[^a-zA-Z0-9]{0,24}[A-Z0-9]{6}([^A-Z0-9]|$))|([A-Z0-9]{6}([^A-Z0-9]|$)[^a-zA-Z0-9]{0,24}((RED)|(GREEN))([^a-zA-Z0-9]|$))'

fail=0
# rg, not git grep: git grep only sees files the index knows about, so a
# brand-new untracked source file gets no scan at all. rg walks the
# working tree (gitignore-aware), matching Detector 2's view.
command -v rg >/dev/null 2>&1 || {
  echo "comment-hygiene: rg not found; refusing to run both detectors blind" >&2
  exit 1
}
hits="$(rg -n --no-heading -e "$pat" \
  -g '*.py' -g '*.rs' \
  -g '!executor/**' \
  2>/dev/null || true)"
if [ -n "$hits" ]; then
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
fi

# --- Detector 2: bare-ID census ratchet ---
# -e, not -f: an empty census (the documented CHF=/dev/null probe) is a
# character device, so -f would wrongly take the missing-file exit. An
# empty/unlistable census must reach the ratchet and fail every bare token.
if [ ! -e "$census" ]; then
  echo "comment-hygiene: census file missing: $census" >&2
  exit 1
fi
tmp="$(mktemp)"
trap 'rm -f "$tmp"' EXIT
rg -n --no-heading -P \
  -g '*.py' -g '*.rs' -g '*.sh' \
  -g '!rust/crates/**/tests/**' \
  -g '!rust/crates/**/benches/**' \
  -g '!rust/crates/**/examples/**' \
  -g '!scripts/hooks/comment-hygiene*' \
  '(?<![A-Za-z0-9_])[A-Z0-9]{6}(?![A-Za-z0-9_])' \
  src tests rust/crates run_bot.sh scripts examples >"$tmp" 2>/dev/null || true

bare_fail="$(awk -v CENSUS="$census" '
  BEGIN {
    while ((getline line < CENSUS) > 0) {
      if (line == "" || line ~ /^#/) continue
      # a line with a "/" is a path:token citation; a bare token is lexicon
      if (line ~ /\//) allowed[line] = 1
      else legit[line] = 1
    }
  }
  {
    line = $0
    c1 = index(line, ":")
    if (c1 == 0) next
    path = substr(line, 1, c1 - 1)
    rest = substr(line, c1 + 1)
    c2 = index(rest, ":")
    lineno = substr(rest, 1, c2 - 1)
    content = substr(rest, c2 + 1)
    # rust/crates slice: comment lines only (//, /*, block-comment continuation)
    if (path ~ /^rust\// && content !~ /^[ \t]*(\/\/|\/\*|\*)/) next
    s = content
    prev_alnum = 0  # was the char just before s[1] a letter/digit/underscore?
    while (match(s, /[A-Z0-9][A-Z0-9][A-Z0-9][A-Z0-9][A-Z0-9][A-Z0-9]/)) {
      tok = substr(s, RSTART, RLENGTH)
      post = (RSTART + RLENGTH <= length(s)) ? substr(s, RSTART + RLENGTH, 1) : ""
      pre_bad = (RSTART > 1) ? (substr(s, RSTART - 1, 1) ~ /[A-Za-z0-9_]/) : prev_alnum
      s = substr(s, RSTART + RLENGTH)
      prev_alnum = 1  # the char before the next scan position ends this token
      # word-boundary rule: no letter/digit/underscore may touch the token;
      # prev_alnum keeps the boundary honest across a rejected match (the
      # tail of a longer uppercase run, e.g. STRUCT in SELFDESTRUCT)
      if (!pre_bad && post !~ /[A-Za-z0-9_]/) {
        key = path ":" tok
        if (key in allowed) continue          # census: grandfathered citation
        if (tok in legit) continue            # lexicon: legit identifier
        if (tok ~ /^[0-9]+$/) continue        # pure-number token
        if (tok ~ /^[0-9A-F]+$/) continue     # hex-shaped token
        if (!(key in reported)) {
          reported[key] = 1
          print path ":" lineno ":" content
        }
      }
    }
  }
' "$tmp")" || true
if [ -n "$bare_fail" ]; then
  echo "comment-hygiene: bare six-char tracker token is not in the census" >&2
  echo "  (scripts/hooks/comment-hygiene-census.txt; docs/comments.md). Strip the" >&2
  echo "  citation, or — only for a real word/identifier — add it to the census" >&2
  echo "  lexicon section:" >&2
  printf '%s\n' "$bare_fail" >&2
  fail=1
fi
exit "$fail"
