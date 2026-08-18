#!/bin/bash
set -euo pipefail

# ── Autoresearch checks script ──
# Runs the full test suite to verify correctness.
# Only shows errors/summary to keep context lean.

cd /home/ralph/code/executor

uv run ape test -q 2>&1 | tail -1
