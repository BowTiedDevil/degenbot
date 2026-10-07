#!/usr/bin/env bash
#
# Staged-scope rust format gate for the pre-commit tier. prek passes the
# staged *.rs list; this script checks exactly those files with the per-file
# invocation cargo fmt itself uses (edition from rust/Cargo.toml
# workspace.package — currently 2021; keep the two in sync).
#
# skip_children=true is load-bearing, not cosmetic: given a file, rustfmt by
# default also recurses into the child modules that file declares (verified:
# an unformatted sibling of a passed lib.rs fails the check). That would let
# an unstaged, mid-flight module from another lane fail a commit that does
# not contain it — the whole-tree-instead-of-staged-files bug at file
# granularity. skip_children pins the check to the passed list. The whole-tree
# sweep (`just fmt-check`) lives at pre-push, where a complete tree is a fair
# assumption.
#
# Config note: this repo has no rustfmt.toml today. rustfmt discovers a config
# file from each input file's ancestors (verified), so a future rustfmt.toml
# is honored per-file exactly as cargo fmt would honor it. Do NOT pass
# --config-path here: pointing it at a directory without a config file is a
# hard error.
#
# Empty-list tripwire: same contract as python-fmt-staged.sh — fail closed on
# a zero-arg invocation instead of certifying nothing was examined.
set -euo pipefail
if [ "$#" -eq 0 ]; then
  echo "rust-fmt-staged: empty file list; refusing a vacuous pass" >&2
  exit 1
fi
exec rustfmt --check --edition 2021 --config skip_children=true -- "$@"
