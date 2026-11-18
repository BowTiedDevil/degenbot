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

# ruff is a dev dependency, so the vintage judging this commit is the one
# uv.lock records and every other python gate in the repo runs. `--no-sync`
# keeps a commit-time hook from re-resolving or reinstalling the environment
# underneath the caller: a venv that has not been synced since ruff entered the
# lock fails here (loudly, from uv) rather than silently checking with a
# different ruff — `uv sync` or `just dev` is the fix.
repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$repo_root"
if ! uv run --no-sync ruff format --check -- "$@"; then

  # Name the repair: ruff lists the files, and this line turns the list into the

  # command that fixes them, so a red commit costs one attempt instead of three.

  printf 'fix: uv run --no-sync ruff format %s\n' "$*" >&2

  exit 1

fi
