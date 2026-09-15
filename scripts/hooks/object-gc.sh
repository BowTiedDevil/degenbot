#!/usr/bin/env bash
#
# pre-push brake: keep the loose object database below the size where git fires
# a repack that grinds the machine to a halt.
#
# The failure this exists to stop (2026-09-15): a run staged rust/target build
# output and never committed it. 139,329 loose objects / 44.5 GiB accumulated in
# .git/objects, all unreachable, and .git grew to 45 GB. The next `git push`
# crossed gc.auto's 6,700-object threshold, so git's detached maintenance ran
# `git repack -d -l --geometric=2` -> `pack-objects` over the pile. Delta
# compression against 100 MB ELF binaries with pack.windowMemory unbounded by
# default reached 663% CPU / 21.4 GB RSS with no ETA.
#
# Two deliberate choices:
#
#   1. `git prune --expire=now`, never `git gc`/`git repack`. Prune only
#      unlinks loose objects that no ref, index, or reflog can reach — it does
#      not inflate or delta-compress them, so that 44.5 GiB took 40 s. A repack
#      would reproduce the blow-up this gate exists to prevent.
#   2. Reflogs are NOT expired. Objects still named by a reflog entry stay
#      reachable, so commits from an interrupted amend/rebase remain recoverable
#      — the guarantee `--expire=now` on the reflogs would have discarded.
#
# Thresholds (overridable for tests or a deliberate large import):
#   DEGENBOT_OBJECT_GC_MAX_COUNT       soft: loose object count that triggers a prune
#   DEGENBOT_OBJECT_GC_MAX_KIB         soft: loose KiB that triggers a prune
#   DEGENBOT_OBJECT_GC_HARD_MAX_COUNT  hard: still above after pruning => block
#   DEGENBOT_OBJECT_GC_HARD_MAX_KIB    hard: still above after pruning => block
#
# Exit 1 only in the hard case: pruning cannot help, so the object DB needs a
# deliberate, memory-capped repack by a human rather than a silent push.

set -euo pipefail

max_count=${DEGENBOT_OBJECT_GC_MAX_COUNT:-2000}
max_kib=${DEGENBOT_OBJECT_GC_MAX_KIB:-204800}
hard_max_count=${DEGENBOT_OBJECT_GC_HARD_MAX_COUNT:-50000}
hard_max_kib=${DEGENBOT_OBJECT_GC_HARD_MAX_KIB:-2097152}

if ! git rev-parse --git-dir >/dev/null 2>&1; then
  echo "object-gc: not a git work tree; skipping." >&2
  exit 0
fi

# count-objects -v reports "count: N", "size: N" (KiB), "garbage: N".
read_stats() {
  local stats
  stats="$(git count-objects -v)"
  stat_count="$(awk -F': ' '$1=="count"{print $2}' <<<"$stats")"
  stat_kib="$(awk -F': ' '$1=="size"{print $2}' <<<"$stats")"
  stat_garbage="$(awk -F': ' '$1=="garbage"{print $2}' <<<"$stats")"
}
stat_count=0
stat_kib=0
stat_garbage=0
read_stats

if [ "$stat_count" -le "$max_count" ] && [ "$stat_kib" -le "$max_kib" ]; then
  echo "object-gc: object database is healthy ($stat_count loose objects, $stat_kib KiB, $stat_garbage garbage); nothing to do."
  exit 0
fi

echo "object-gc: loose object database is oversized; removing what nothing can reach." >&2
echo "object-gc:   before: $stat_count loose objects, $stat_kib KiB (threshold $max_count / $max_kib KiB)" >&2
before_count="$stat_count"
before_kib="$stat_kib"
if ! git prune --expire=now >&2; then
  echo "object-gc: 'git prune --expire=now' failed; blocking the push to avoid an unbounded repack." >&2
  echo "object-gc:   inspect with: git count-objects -vH" >&2
  exit 1
fi
read_stats
echo "object-gc:   after:  $stat_count loose objects, $stat_kib KiB (reclaimed $((before_count - stat_count)) objects, $((before_kib - stat_kib)) KiB)" >&2

if [ "$stat_count" -gt "$hard_max_count" ] || [ "$stat_kib" -gt "$hard_max_kib" ]; then
  {
    echo ""
    echo "⛔ object-gc: push blocked — the object database is oversized and pruning could not fix it."
    echo "   remaining: $stat_count loose objects, $stat_kib KiB (hard limit $hard_max_count / $hard_max_kib KiB)"
    echo ""
    echo "   Everything left is reachable or reflog-protected, so 'git prune' must not"
    echo "   remove it. Do NOT reach for a plain 'git gc' here: that is the unbounded"
    echo "   repack this gate exists to prevent (see the header of scripts/hooks/object-gc.sh)."
    echo "   Repack deliberately, with a memory ceiling and a coarse delta window:"
    echo ""
    echo "     git -c pack.windowMemory=256m -c pack.deltaCacheSize=64m \\"
    echo "         -c core.bigFileThreshold=32m repack -ad"
    echo ""
    echo "   If a stray 'git add' of build output (e.g. rust/target) put the objects"
    echo "   there and you accept losing them:"
    echo ""
    echo "     git reflog expire --expire=now --all && git prune --expire=now"
  } >&2
  exit 1
fi

echo "object-gc: ok."
exit 0
