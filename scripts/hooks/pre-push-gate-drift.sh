#!/usr/bin/env bash
#
# Pre-push gate drift brake.
#
# The `just pre-push` recipe is a hand-written transcription of the
# `stages = ["pre-push"]` entries in prek.toml. It drifted once already (four
# gates missing, two test recipes re-pointed at a different entrypoint), and the
# drift direction is the bad one: `just pre-push` went green while `git push`
# was blocked by a gate that recipe never ran. AGENTS.md points every agent at
# `just pre-push` before pushing, so a stale transcription is a false green that
# keeps producing exactly that miss.
#
# So the two lists are compared here and a mismatch fails. It compares the hook
# ENTRIES (the command each gate runs), in declaration order; the `name` fields
# are presentation only.
#
# Exit codes:
#   0   the two lists agree
#   1   drift (prints both lists and their diff)
#   2   a source file or its list could not be produced - fails closed, so a
#       broken extraction can never report a green that examined nothing
#
# Scope and blind spots:
#   - It compares the gate LISTS only. It cannot see whether a gate's body
#     changed meaning underneath an unchanged entry string (e.g. the recipe a
#     `just <name>` alias resolves to changed): the entry string is the whole
#     contract here.
#   - It reads prek.toml with sed, so it sees single-line `{ id = ... }` hook
#     tables only. A pre-push hook written as a multi-line TOML table is
#     invisible to it. `tests/test_pre_push_gate_drift.py` re-derives the same
#     list through a real TOML parser and compares, so that shape is caught by
#     the python-test gate in this same stage instead of here.
#
# Override the sources for the negative probes:
#   JF=/dev/null <this script>              # fails closed (2), no recipe list
#   JF=/tmp/justfile.mutated <this script>  # fails on drift (1)
set -euo pipefail

cd "$(git rev-parse --show-toplevel)"

prek_toml="${PREK:-prek.toml}"
justfile="${JF:-justfile}"
for f in "$prek_toml" "$justfile"; do
  [ -f "$f" ] || { echo "pre-push-gate-drift: missing source file: $f" >&2; exit 2; }
done

prek_list="$(mktemp)"
just_list="$(mktemp)"
trap 'rm -f "$prek_list" "$just_list"' EXIT

# prek.toml: one single-line `{ id = ... }` hook table per gate. Anchored on the
# table opener so the prose in the surrounding comment blocks cannot match.
sed -n 's/^[[:space:]]*{ id = .*entry = "\([^"]*\)".*stages = \["pre-push"\].*/\1/p' \
  "$prek_toml" >"$prek_list"

# justfile: the `pre-push` recipe body, one `run_gate "<label>" <cmd>` per gate.
# Stop at the first column-0 line that is not a comment - the next recipe.
awk '/^pre-push:/{body=1; next} body && /^[^[:space:]#]/{exit} body{print}' "$justfile" \
  | sed -n 's/^[[:space:]]*run_gate "[^"]*"[[:space:]]*//p' >"$just_list"

if [ ! -s "$prek_list" ] || [ ! -s "$just_list" ]; then
  {
    echo "pre-push-gate-drift: could not extract both gate lists; failing closed."
    echo "  $prek_toml pre-push entries: $(wc -l <"$prek_list" | tr -d ' ')"
    echo "  $justfile run_gate lines:    $(wc -l <"$just_list" | tr -d ' ')"
  } >&2
  exit 2
fi

if diff -u \
  --label "$prek_toml (pre-push stage)" \
  --label "$justfile (pre-push recipe)" \
  "$prek_list" "$just_list" >&2
then
  echo "pre-push-gate-drift: ok ($(wc -l <"$prek_list" | tr -d ' ') gates; the recipe matches $prek_toml)."
  exit 0
fi

{
  echo
  echo "pre-push-gate-drift: the 'just pre-push' recipe does not run the same gates as"
  echo "the installed pre-push hook. A 'just pre-push' that runs a shorter list is a false green:"
  echo "the push is then blocked by a gate this recipe never ran."
  echo
  echo "Make the recipe a literal transcription of the 'stages = [\"pre-push\"]' entries in"
  echo "prek.toml: same gates, same order, same entries. Commit both together."
} >&2
exit 1
