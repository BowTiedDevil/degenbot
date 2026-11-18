"""Map the outgoing diff to the workspace packages it touches, as `-p` flags.

The pre-push tier certifies the lint contract over the whole tree and the tests
of what is about to be pushed. Repeating the whole workspace suite serially
before every push is what made a session long, and CI owns that matrix, where
its legs run as parallel jobs.

Prints nothing when the outgoing diff touches no Rust source, so the caller runs
no test leg at all.
"""

from __future__ import annotations

import json
import pathlib
import subprocess  # ruff: ignore[suspicious-subprocess-import] -- git and cargo plumbing
import sys
from typing import Any

MANIFEST = pathlib.Path("rust") / "Cargo.toml"


def _run(argv: list[str], *, check: bool = False) -> subprocess.CompletedProcess[str]:
    """Run one tool by PATH and capture its text output.

    The single place this module shells out, so the security exemptions for
    invoking the operator's own `git` and `cargo` are stated once.

    Args:
        argv: The program and its arguments.
        check: Raise on a nonzero exit when true.

    Returns:
        The completed process.
    """
    # ruff: ignore[subprocess-without-shell-equals-true] -- fixed argv, no shell, tools by PATH
    return subprocess.run(argv, capture_output=True, text=True, check=check)


def package_directories(metadata: dict[str, Any], root: pathlib.Path) -> dict[str, str]:
    """Map each workspace package's directory (repo-relative) to its name.

    Args:
        metadata: A `cargo metadata --no-deps` payload.
        root: The repository root the directories are made relative to.

    Returns:
        One `directory -> package name` entry per workspace member inside root.
    """
    directories: dict[str, str] = {}
    for package in metadata["packages"]:
        directory = pathlib.Path(package["manifest_path"]).parent
        try:
            directories[directory.relative_to(root).as_posix()] = package["name"]
        except ValueError:
            continue
    return directories


def affected_packages(metadata: dict[str, Any], paths: list[str], root: pathlib.Path) -> list[str]:
    """Return the packages whose directory contains one of `paths`, sorted.

    Args:
        metadata: A `cargo metadata --no-deps` payload.
        paths: Repo-relative paths, usually the outgoing diff's file list.
        root: The repository root.

    Returns:
        The package names, sorted, without duplicates.
    """
    directories = package_directories(metadata, root)
    selected: set[str] = set()
    for path in paths:
        if not path.startswith("rust/"):
            continue
        for directory, name in directories.items():
            if path == directory or path.startswith(directory + "/"):
                selected.add(name)
                break
    return sorted(selected)


def outgoing_paths() -> list[str]:
    """Return the files in the commits about to be pushed.

    Returns:
        Repo-relative paths, or an empty list when the range cannot be resolved.
    """
    upstream = _run(["git", "rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{u}"])
    if upstream.returncode == 0 and upstream.stdout.strip():
        base = upstream.stdout.strip()
    else:
        head = _run(["git", "symbolic-ref", "--short", "refs/remotes/origin/HEAD"])
        base = head.stdout.strip() or "origin/main"
    diff = _run(["git", "diff", "--name-only", f"{base}...HEAD"])
    return [line for line in diff.stdout.splitlines() if line]


def main(argv: list[str]) -> int:
    """Print the `-p` flags for the outgoing diff, or an override path list.

    Args:
        argv: Positional paths override the diff (a debugging and test seam).

    Returns:
        A process exit status.
    """
    root = pathlib.Path.cwd()
    metadata = json.loads(
        _run(
            [
                "cargo",
                "metadata",
                "--no-deps",
                "--format-version",
                "1",
                "--manifest-path",
                str(MANIFEST),
            ],
            check=True,
        ).stdout
    )
    paths = argv or outgoing_paths()
    flags = "".join(f"-p {name} " for name in affected_packages(metadata, paths, root))
    if flags:
        sys.stdout.write(f"{flags}\n")
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
