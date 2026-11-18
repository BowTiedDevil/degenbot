#!/usr/bin/env bash
# Map the outgoing diff to the workspace packages it touches, as `-p` flags.
#
# The pre-push tier certifies the lint contract over the whole tree, which is
# cheap, and the tests of what is about to be pushed, which is not cheap enough
# to repeat for fifteen minutes on every push: the full matrix is CI's job, run
# as parallel jobs, and a local serial copy of it only lengthened a session.
#
# Prints nothing when the outgoing diff touches no Rust source, so the caller
# runs no test leg at all.
set -euo pipefail

cd "$(git rev-parse --show-toplevel)"

# @{u} is the last-fetched upstream: exactly the commits about to be pushed.
# A branch with no upstream falls back to the default branch's merge base.
if base="$(git rev-parse --abbrev-ref --symbolic-full-name '@{u}' 2>/dev/null)"; then
    range="${base}...HEAD"
else
    base="$(git symbolic-ref --short refs/remotes/origin/HEAD 2>/dev/null || echo origin/main)"
    range="${base}...HEAD"
fi

changed="$(git diff --name-only "$range" 2>/dev/null || true)"

# Positional paths override the outgoing diff: a debugging and test seam, so the
# mapping can be exercised without a pushable branch.
if [ "$#" -gt 0 ]; then
    changed="$(printf '%s\n' "$@")"
fi

CHANGED="$changed" python3 - <<'PY'
import json
import os
import pathlib
import subprocess
import sys

root = pathlib.Path.cwd()
meta = json.loads(
    subprocess.run(
        [
            "cargo",
            "metadata",
            "--no-deps",
            "--format-version",
            "1",
            "--manifest-path",
            "rust/Cargo.toml",
        ],
        capture_output=True,
        text=True,
        check=True,
    ).stdout
)

dirs = {}
for pkg in meta["packages"]:
    directory = pathlib.Path(pkg["manifest_path"]).parent
    try:
        dirs[directory.relative_to(root).as_posix()] = pkg["name"]
    except ValueError:
        continue

selected = set()
for path in os.environ.get("CHANGED", "").splitlines():
    path = path.strip()
    if not path or not path.startswith("rust/"):
        continue
    for directory, name in dirs.items():
        if path == directory or path.startswith(directory + "/"):
            selected.add(name)
            break

for name in sorted(selected):
    sys.stdout.write(f"-p {name} ")
PY
