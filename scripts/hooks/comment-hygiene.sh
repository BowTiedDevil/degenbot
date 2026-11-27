#!/usr/bin/env bash
# Comment-hygiene gate — docs/comments.md, docs/gates.md section 1, AGENTS.md.
#
# One anchored detector, zero tolerance: a code comment never restates
# work-tracker state. No census and no waiver stands behind it, so a finding is
# always an edit to the code, never an entry to add here.
#
# Retained shapes (each is anchored by a label word next to the band, which is
# what separates a citation from prose):
#   - tracker citations: "er[g]o <six-character ID>", "er[g]o epic|task|slice
#     <ID>", a bare "epic|task|slice <ID>", and RED / GREEN gate narration within
#     a short band of a six-character ID
#   - migration-work-item labels: a digit immediately after the label word
#     ("sli[ce] <NN>", "Plan <NNN>", and their hyphen forms). A slice is an
#     ephemeral work item exactly like a tracker ID: the rule it introduced
#     belongs in the comment, the history in the ADR.
#   - tracker IDs: a hex-suffixed or parenthesised TODO band
#   - work-item labels, case-insensitive: "Epic <token>" and "sli[ce] <token>"
#     whose token contains a digit, "Phase <letter><digit>", and the pre- and
#     post-[e]pic forms. The digit requirement is what separates a work item
#     from the word in ordinary prose ("an epic feature", "the two-slice
#     intake"). A bare phase number stays out: that prose carries live
#     algorithm semantics (the eager-vs-boundary-tail tick-bitmap phases, the
#     pre-solve gap phases, the GIL phases).
#   - concrete incident labels: a retired gr[a]b, the twin-colla[p]se repair, a
#     gr[e]p compat marker, a cr[a]sh ordinal, and R[E]D placeholders
#
# Review-owned, deliberately NOT scanned (docs/comments.md names both classes):
#   - bare six-character tokens with no anchor word. A tracker citation, an
#     acronym, an EVM opcode and a scream-case static share one surface form, so
#     matching them needs a per-token lexicon of legit identifiers that no
#     reviewer can keep true; the reviewer reading the line can.
#   - broad past-voice narration: a permanence assertion, the value a change
#     replaced, slice-dated deletion and then-future prose, the containing
#     change's play-by-play, the fix's history voice (pre-fix / post-fix / before
#     the fix), past-voiced removal, and the past-habitual "used to <verb>"
#     forms. Live present-tense prose shares every one of those surface forms
#     ("the mode STAYS WatchLive", "is removed when", a runtime refusal
#     string's "used to be" clause), so a lexical matcher would have to misfire
#     on the present to catch the past.
#
# Scope: git's INDEX, not the working tree — the commit being certified contains
# the index, so a brand-new staged source file is examined while another lane's
# untracked work-in-progress cannot fail this commit (prek's stash hides tracked
# dirt but leaves untracked files in place). The scanned classes are the code-ish
# ones (*.py, *.rs, *.pyi, *.toml, *.yml, *.yaml, *.sh, *.sol) plus the
# extensionless justfile; the trigger in scripts/gates.toml names the same set and
# tests/test_comment_hygiene_gate.py reconciles the two. Build recipes (justfile,
# CI workflows, prek.toml, the root Cargo.toml) carry the same prose a code
# comment does, so they are in scope. Excluded: rust/vendor (upstream sources) and
# tier3-oracle/src* (their bytes are sha256-pinned in
# tier3-oracle/artifacts/manifest.json, so a comment edit there fails the
# committed-artifact match tests until a solc/vyper rebuild). Markdown is out:
# docs/ is where tracker discussion legitimately lives.
#
# HCR roots the scan at another tree — the fixture override in
# tests/test_comment_hygiene_gate.py. A fixture root is not a git checkout, so it
# is walked by class instead of enumerated from the index. It is the only seam: a
# gate run always examines the commit's own content.
#
# live-invariant gate (ADR-087 D2): watches the live comment surface; not tripwire precedent.

set -euo pipefail
# HCR points the scan at a fixture tree; unset, it roots at the git top level.
cd "${HCR:-$(git rev-parse --show-toplevel)}"

command -v rg >/dev/null 2>&1 || {
  echo "comment-hygiene: rg not found; refusing to certify a tree it cannot scan" >&2
  exit 1
}

# --- Scope ------------------------------------------------------------------
# One class list builds both enumerations: the fixture walk's globs and the
# index's pathspecs. tests/test_comment_hygiene_gate.py reads this line and
# reconciles it with the registry trigger.
exts=(py rs pyi toml yml yaml sh sol)
globs=()
pathspecs=()
for ext in "${exts[@]}"; do
  globs+=(-g "*.${ext}")
  pathspecs+=("*.${ext}")
done
globs+=(-g 'justfile' -g '!rust/vendor/**' -g '!tier3-oracle/src*/**')
pathspecs+=('justfile' ':(exclude)rust/vendor/**' ':(exclude)tier3-oracle/src*/**')

paths=()
if [ -z "${HCR:-}" ]; then
  mapfile -d '' -t paths < <(git ls-files -z --cached -- "${pathspecs[@]}")
  if [ "${#paths[@]}" -eq 0 ]; then
    echo "comment-hygiene: git's index lists no scannable file; refusing to certify an empty scope" >&2
    exit 1
  fi
fi

# --- Shapes -----------------------------------------------------------------
# Anchored citation shapes, case-sensitive: the uppercase classes are part of the
# shape (a lowercase six-letter word is prose, not an ID). Spelling a band with a
# character class keeps this header from citing itself; the hook is scanned like
# any other file.
cite='([Ee]rgo[^a-zA-Z0-9]{0,3}[A-Z0-9]{6}\b)'
cite+='|([Ee]rgo[^a-zA-Z0-9]{0,3}(epic|task|slice)[^a-zA-Z0-9]{0,3}[A-Z0-9]{6}\b)'
cite+='|((epic|task|slice)[^a-zA-Z0-9]{0,3}[A-Z0-9]*[0-9][A-Z0-9]{5}\b)'
cite+='|((^|[^a-zA-Z])((RED)|(GREEN))[^a-zA-Z0-9]{0,24}[A-Z0-9]{6}([^A-Z0-9]|$))'
cite+='|([A-Z0-9]{6}([^A-Z0-9]|$)[^a-zA-Z0-9]{0,24}((RED)|(GREEN))([^a-zA-Z0-9]|$))'
cite+='|(\b[Ss]lices?([[:space:]]+|-)[0-9]+[a-z]?\b)'
cite+='|(\b[Pp]lan([[:space:]]+|-)[0-9]+\b)'
cite+='|(retired[[:space:]]+grab)'
cite+='|(twin-colla[p]se)'
cite+='|(grep[[:space:]]+compat)'
cite+='|(crash[[:space:]]+#[0-9]+)'
cite+='|(RED[[:space:]]+placeholders)'

# Work-item labels, case-insensitive: a tracker ID can be spelled in lower case.
# The epic and slice shapes want a token containing a digit, so ordinary prose
# ("an epic feature", "the two-slice intake") survives; the phase shape wants
# letter+digit, so a bare algorithm phase number survives.
label='(\btodo[ \t]*[-_][ \t]*[0-9a-f]{4,}\b)'
label+='|(\btodo\([a-z][a-z0-9_-]*\))'
label+='|(\bepic\b[ \t]+[a-z0-9_-]*[0-9][a-z0-9_-]*)'
label+='|(\bslice\b[ \t-]+[a-z0-9_-]*[0-9][a-z0-9_-]*)'
label+='|(\bphase\b[ \t]+[a-z][0-9]\b)'
label+='|(\b(?:pre|post)-epic\b)'

# --- Run --------------------------------------------------------------------
# One detector, two anchored passes: the case-sensitivity of a shape is part of
# the shape, so the label pass runs with -i and the citation pass without. rg
# exits 1 for "no match" and 2 for "could not scan", so a pass that fails to
# examine the tree is a refusal, never a green over nothing.
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
set +e
rg -n -P --no-heading -e "$cite" "${globs[@]}" "${paths[@]}" >"$tmp/cite" 2>"$tmp/cite.err"
cite_status=$?
rg -n -P --no-heading -i -e "$label" "${globs[@]}" "${paths[@]}" >"$tmp/label" 2>"$tmp/label.err"
label_status=$?
set -e
for status in "$cite_status" "$label_status"; do
  if [ "$status" -gt 1 ]; then
    echo "comment-hygiene: rg exited $status; refusing to certify a tree it could not scan" >&2
    cat "$tmp"/*.err >&2
    exit 1
  fi
done

# awk dedupes a line both passes catch; there is no per-token subprocess.
hits="$(awk '!seen[$0]++' "$tmp/cite" "$tmp/label")"

if [ -z "$hits" ]; then
  exit 0
fi

{
  echo "comment-hygiene: a work-tracker citation, a migration-work-item label, or"
  echo "  a change-narration label is banned in code (AGENTS.md 'Comment Hygiene',"
  echo "  docs/comments.md). Code describes the present: state the rule the change"
  echo "  introduced in prose, point at the test that pins it, or delete the sentence."
  echo "  There is no census and no waiver; the repair is always an edit."
  echo "  fix: strip the band from the line above (docs/comments.md)."
  printf '%s\n' "$hits"
} >&2
exit 1
