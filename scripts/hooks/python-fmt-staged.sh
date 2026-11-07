#!/usr/bin/env bash
#
# Staged-scope python format gate for the pre-commit tier. prek passes the
# staged *.py list (executor/ is excluded upstream in prek.toml, mirroring the
# vendored tree's own conventions); this script format-checks exactly that
# list, so a green result means "the commit's own python is formatted" and
# nothing more.
#
# Why this is not the old whole-tree sweep: prek's stash parks tracked-unstaged
# changes during hooks but leaves untracked files in place, so
# `ruff format --check src/` at commit time saw other lanes' mid-flight files
# and blocked clean commits. The whole-tree sweep moved to pre-push (prek.toml
# -> `just fmt-check-python`); this is the same detector sliced to the commit.
#
# Empty-list tripwire (load-bearing): prek only invokes file-scoped hooks when
# at least one staged file survives the types/exclude filters, so a zero-arg
# invocation can only mean a broken caller. Fail closed rather than certify a
# list we never looked at — a vacuous green on an unexamined list is exactly
# the failure mode this tier exists to prevent.
set -euo pipefail
if [ "$#" -eq 0 ]; then
  echo "python-fmt-staged: empty file list; refusing a vacuous pass" >&2
  exit 1
fi
# Format-check with the one pinned ruff vintage (the justfile's `ruff_version`):
# a floating `uvx ruff` resolved whatever PyPI published that minute, and ruff
# rewrites files under `format`, so an upstream release could flip -- or
# silently rewrite -- this shared gate's verdict with no diff to review. The
# pin is read, not duplicated; tests/test_python_fmt_staged_gate.py drives this
# script against a recording uvx stub and fails if the gate floats.
repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
ruff_pin="$(grep -m1 '^ruff_version := ' "$repo_root/justfile" | cut -d'"' -f2)"
if [ -z "$ruff_pin" ]; then
  echo "python-fmt-staged: cannot read the ruff_version pin from the justfile" >&2
  exit 1
fi
exec uvx --with "$ruff_pin" ruff format --check -- "$@"
