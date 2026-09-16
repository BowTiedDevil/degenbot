#!/usr/bin/env bash
# user_journey_leak_check.sh — assert no observer-only material is reachable
# by the worker. Run post-sealing, pre-worker-spawn; the observer re-runs its
# broader variant (session-log audit) at scoring time. Non-zero exit = leak.
# Marker list is deliberately generic: knowing this list must reveal nothing.
set -u
cd "$(dirname "$0")/.."
FAIL=0
bad() { printf '[LEAK] %s\n' "$1"; FAIL=1; }
ok()  { printf '[OK]   %s\n' "$1"; }

PKG=docs/autonomous-user-journey
WORKER_VISIBLE="$PKG/HANDOFF.md $PKG/ENVIRONMENT_FINDINGS.md $PKG/RELAYS_AND_GUARDRAILS.md"

# 1. Sealed artifacts must NOT exist in the project tree during a run.
for f in "$PKG/SEALED.md" "$PKG/OBSERVER_BRIEF.md"; do
  [ -f "$f" ] && bad "sealed artifact present in repo: $f" || ok "absent: $f"
done

# 2. Worker-visible docs must not carry sealed markers.
MARKERS='hint ladder|Appendix A|TR-[0-9]|answer key|0x0D6d4c3c|INJECT_EXECUTOR_CODE|EXECUTOR_OWNER_ADDRESS|observer agents'
for f in $WORKER_VISIBLE; do
  if [ -f "$f" ] && grep -Eq "$MARKERS" "$f"; then
    bad "marker found in $f: $(grep -Eo "$MARKERS" "$f" | sort -u | tr '\n' ' ')"
  else
    ok "clean: $f"
  fi
done

# 3. Observer-only docs are marked as such (defense against a curious worker).
for f in "$PKG/PRD.md" "$PKG/ACCEPTANCE_CRITERIA.md" "$PKG/RUNBOOK.md"; do
  grep -q "OBSERVER-ONLY" "$f" && ok "marked: $f" || bad "missing OBSERVER-ONLY header: $f"
done

echo
[ "$FAIL" = "0" ] && { echo "LEAK CHECK PASS"; exit 0; } || { echo "LEAK CHECK FAIL"; exit 1; }
