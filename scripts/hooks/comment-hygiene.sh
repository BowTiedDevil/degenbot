#!/usr/bin/env bash
# Comment-hygiene instant gate — enforces the AGENTS.md "Comment Hygiene"
# rule (docs/comments.md): code comments never restate work-tracker state.
#
# Detector 1 — anchored citations (the regex in "pat" below):
#   - "ergo <6-char ID>" / "ergo epic|task|slice <ID>" citations
#   - "epic|task|slice <ID>" citations
#   - RED/GREEN gate narration co-occurring with a 6-char ID token
#   - migration-work-item labels: "slice <NN>[x]" / "slice-<NN>" and
#     "Plan <NNN>" / "plan-<NNN>" (this header spells them with angle
#     brackets so documenting the pattern does not cite one). A migration
#     slice is an ephemeral work item exactly like a tracker ID: the rule it
#     introduced belongs in the comment, the history in the ADR. A digit
#     must follow the label word across whitespace or one hyphen, so
#     ordinary prose about a data slice ("the two-slice intake") and the
#     call/index forms ("plan(1.5)", "plan[0]") are untouched.
#   Scope: the working tree, not the index. The search runs rg (same tool
#   and ignore rules as Detector 2) over the whole repo for the code-ish
#   file classes (*.py, *.rs, *.pyi, *.toml, *.yml, *.yaml, *.sh, *.sol,
#   and the extensionless `justfile`), excluding executor/** (a
#   self-contained sub-workspace with its own conventions), the
#   tier3-oracle/src* harness sources (their bytes are sha256-pinned in
#   artifacts/manifest.json — a comment edit there fails the
#   committed-artifact match tests until a solc/vyper rebuild, so they stay
#   out of scope), and generated lockfiles, so a brand-new untracked source
#   file is examined. Build recipes (justfile, CI workflows, prek.toml, the
#   root Cargo.toml) and the tier3-oracle build scripts carry the same
#   prose a code comment does, so they are in scope. An index-bound search can certify a tree
#   it never looked at — a green that examined nothing — which is the
#   failure this gate exists to prevent. One rg pass; no per-token
#   subprocesses.
#   Deliberately out of scope here: markdown. It is where tracker
#   discussion legitimately lives (docs/, this gate's own census/waiver
#   files).
#   Waivers: one repo-relative path per line in
#   scripts/hooks/comment-hygiene-waivers.txt. A waiver means "this file
#   still carries pre-rule citations" — it NEVER licenses new ones; remove
#   the entry in the same commit that strips the file's citations.
#   Override the waiver file for testing: WAF=/dev/null <this script>.
#
# live-invariant gate (ADR-087 D2): watches the live comment surface, ratcheted against the lexicon census; not tripwire precedent.
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
#   (all lines), rust/crates/*.rs comment lines only, plus the build
#   surface — justfile, *.toml, *.yml/*.yaml, *.pyi, *.sol, .github/**,
#   tier3-oracle/** — as all-lines files (their comments are '#' or '///',
#   not Rust comments). Excluding rust tests/benches/examples, generated
#   artifacts, and the gate's own census/hook/waiver files.
#   Keep it fast (it runs in pre-commit): one rg pass + one awk pass, no
#   per-token subprocesses.
#   Override the census for testing: CHF=/dev/null <this script> (fails on
#   every bare token).
#
# Detector 3 — the tracker-citation census ratchet: the citation shapes
# Detector 1 (which needs an anchor word) and Detector 2 (which needs an
# all-uppercase token) cannot see, and the residue a per-file waiver cannot
# express. Case-insensitive, word-boundary anchored:
#   - TODO-<hex> (any case) and TODO(<word>) tracker IDs
#   - Epic <token> and slice <token> work-item labels
#   - Phase <letter><digit> labels
#   - pre-/post-epic narration
# Bare "phase <N>" is deliberately out of scope: that prose carries live
# algorithm semantics (the eager-vs-boundary-tail phases in tick_bitmap.rs,
# the pre-solve gap phases in instruments.rs, the GIL phases in
# result_channel.rs), so this shape wants a letter+digit token after the
# label word. The slice shape likewise wants a token CONTAINING a digit, so
# data-slicing prose ("the two-slice intake") survives.
# Scope: the working tree, over Detector 1's file classes, minus this gate's
# own files (the hook and its censuses carry the shapes by construction), so
# brand-new untracked sources are examined and markdown — where tracker
# discussion legitimately lives — stays out. The engine's algorithm-phase
# prose is a blind spot the design accepts; the sweep that shrank this
# census left it alone.
# Census: scripts/hooks/comment-hygiene-citation-census.txt, one row per
# grandfathered site, formatted "<repo-relative-path>:<matched text>" (the
# band as rg -o prints it, whitespace collapsed, so the row survives a line
# reflow). The check is bidirectional and fails closed: a citation with no
# row fails (a NEW citation cannot land), a row with no citation fails (a
# swept row must be deleted in the same commit), and a missing census file
# fails instead of passing over an empty census. The only rows left are
# reviewed non-citations, each with the reason written above it.
# Override for testing: CCF=/dev/null <this script> (fails on every matched
# band; the negative probe in tests/test_comment_hygiene_gate.py drives
# this and the HCR root override below).

set -euo pipefail
# HCR points the scan at another tree (the negative probe's fixture); unset,
# it roots at the git top level. The overrides (HCR, WAF, CHF, CCF) are test
# seams only: a gate run has to examine the whole working tree.
cd "${HCR:-$(git rev-parse --show-toplevel)}"

waf="${WAF:-scripts/hooks/comment-hygiene-waivers.txt}"
census="${CHF:-scripts/hooks/comment-hygiene-census.txt}"
# Migration-work-item labels ("ADR-005 slice <NN>[x]", "Plan <NNN>")
# join the anchored citation class: a digit must follow the label word, so
# data-slice prose and the plan(..)/plan[..] call and index forms survive.
label='(\b[Ss]lices?([[:space:]]+|-)[0-9]+[a-z]?\b)|(\b[Pp]lan([[:space:]]+|-)[0-9]+\b)'
pat="(${label})|([Ee]rgo[^a-zA-Z0-9]{0,3}[A-Z0-9]{6}\b)|([Ee]rgo[^a-zA-Z0-9]{0,3}(epic|task|slice)[^a-zA-Z0-9]{0,3}[A-Z0-9]{6}\b)|((epic|task|slice)[^a-zA-Z0-9]{0,3}[A-Z0-9]*[0-9][A-Z0-9]{5}\b)|((^|[^a-zA-Z])((RED)|(GREEN))[^a-zA-Z0-9]{0,24}[A-Z0-9]{6}([^A-Z0-9]|$))|([A-Z0-9]{6}([^A-Z0-9]|$)[^a-zA-Z0-9]{0,24}((RED)|(GREEN))([^a-zA-Z0-9]|$))"

fail=0
# rg, not git grep: git grep only sees files the index knows about, so a
# brand-new untracked source file gets no scan at all. rg walks the
# working tree (gitignore-aware), matching Detector 2's view.
command -v rg >/dev/null 2>&1 || {
  echo "comment-hygiene: rg not found; refusing to run both detectors blind" >&2
  exit 1
}
hits="$(rg -n --no-heading -e "$pat" \
  -g '*.py' -g '*.rs' -g '*.pyi' -g '*.toml' \
  -g '*.yml' -g '*.yaml' -g '*.sh' -g '*.sol' \
  -g 'justfile' \
  -g '!executor/**' \
  -g '!rust/vendor/**' \
  -g '!tier3-oracle/src*/**' \
  2>/dev/null || true)"
if [ -n "$hits" ]; then
  while IFS= read -r line; do
    path="${line%%:*}"
    if ! grep -Fxq "$path" "$waf"; then
      if [ "$fail" -eq 0 ]; then
        echo "comment-hygiene: work-tracker IDs / migration labels / gate narration" >&2
        echo "  are banned in code (AGENTS.md 'Comment Hygiene', docs/comments.md)." >&2
        echo "  Strip the citation, restate the rule it introduced in prose, or anchor" >&2
        echo "  the pinning test instead:" >&2
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
  -g '*.pyi' -g '*.toml' -g '*.yml' -g '*.yaml' -g '*.sol' \
  -g 'justfile' \
  -g '!rust/crates/**/tests/**' \
  -g '!rust/crates/**/benches/**' \
  -g '!rust/crates/**/examples/**' \
  -g '!tier3-oracle/artifacts/**' \
  -g '!tier3-oracle/src*/**' \
  -g '!scripts/hooks/comment-hygiene*' \
  '(?<![A-Za-z0-9_])[A-Z0-9]{6}(?![A-Za-z0-9_])' \
  src tests rust/crates run_bot.sh scripts examples \
  justfile prek.toml rust/Cargo.toml .github tier3-oracle >"$tmp" 2>/dev/null || true

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
    # Rust source: comment lines only (//, /*, block-comment continuation);
    # non-.rs files under rust/ (e.g. rust/Cargo.toml) are '#'-commented and
    # are scanned on all lines like every other build file.
    if (path ~ /\.rs$/ && content !~ /^[ \t]*(\/\/|\/\*|\*)/) next
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
  echo "  (scripts/hooks/comment-hygiene-census.txt; docs/comments.md)." >&2
  echo "  Line-start-deref false positive? A Rust line whose first non-space char is" >&2
  echo "  '*' (a deref, e.g. '    *LOADED') is read as a block-comment continuation," >&2
  echo "  so a six-char UPPERCASE static name on it trips this ratchet with no" >&2
  echo "  comment involved; that name is a real identifier — add it to the census" >&2
  echo "  lexicon (see the census header's line-start-deref note)." >&2
  echo "  Otherwise strip the citation, or — only for a real word/identifier — add it" >&2
  echo "  to the census lexicon section:" >&2
  printf '%s\n' "$bare_fail" >&2
  fail=1
fi

# --- Detector 3: tracker-citation census ratchet ---
# -o emits the matched band alone, so a census row is keyed on the path and
# the text instead of a line number: the row survives a reflow and disappears
# only when the citation itself does. One rg pass; the awk pass classifies the
# bands and reconciles them against the census in both directions.
ccf="${CCF:-scripts/hooks/comment-hygiene-citation-census.txt}"
if [ ! -e "$ccf" ]; then
  echo "comment-hygiene: citation census missing: $ccf" >&2
  exit 1
fi
d3_tmp="$(mktemp)"
trap 'rm -f "$tmp" "$d3_tmp"' EXIT
d3='(\btodo[ \t]*[-_][ \t]*[0-9a-f]{4,}\b)|(\btodo\([a-z][a-z0-9_-]*\))|(\bepic\b[ \t]+[a-z0-9][a-z0-9_-]*)|(\bslice\b[ \t-]+[a-z0-9_-]*[0-9][a-z0-9_-]*)|(\bphase\b[ \t]+[a-z][0-9]\b)|(\b(?:pre|post)-epic\b)'
rg -n -o --no-heading -i -P \
  -g '*.py' -g '*.rs' -g '*.pyi' -g '*.toml' \
  -g '*.yml' -g '*.yaml' -g '*.sh' -g '*.sol' \
  -g 'justfile' \
  -g '!executor/**' \
  -g '!rust/vendor/**' \
  -g '!tier3-oracle/src*/**' \
  -g '!scripts/hooks/comment-hygiene*' \
  -e "$d3" >"$d3_tmp" 2>/dev/null || true
d3_out="$(awk -v CENSUS="$ccf" '
  BEGIN {
    while ((getline row < CENSUS) > 0) {
      sub(/[ \t]+$/, "", row)
      if (row == "" || row ~ /^#/) continue
      listed[row] = 1
    }
  }
  {
    c1 = index($0, ":")
    if (c1 == 0) next
    path = substr($0, 1, c1 - 1)
    rest = substr($0, c1 + 1)
    c2 = index(rest, ":")
    if (c2 == 0) next
    lineno = substr(rest, 1, c2 - 1)
    hit = substr(rest, c2 + 1)
    gsub(/[ \t]+/, " ", hit)
    key = path ":" hit
    seen[key] = 1
    if (key in listed) next
    if (key in reported) next
    reported[key] = 1
    printf "  fresh: %s:%s: %s -> census row: %s\n", path, lineno, hit, key
  }
  END {
    for (row in listed) if (!(row in seen)) printf "  stale: %s -> delete this row\n", row
  }
' "$d3_tmp" | sort || true)"
if [ -n "$d3_out" ]; then
  echo "comment-hygiene: Detector 3 — tracker citations / work-item labels are out" >&2
  echo "  of sync with scripts/hooks/comment-hygiene-citation-census.txt" >&2
  echo "  (docs/gates.md section 1). 'fresh' is a citation the census does not" >&2
  echo "  grandfather: strip it (docs/comments.md), never list a new one. 'stale'" >&2
  echo "  is a swept citation whose row has to go in the same change." >&2
  printf '%s\n' "$d3_out" >&2
  fail=1
fi
exit "$fail"
