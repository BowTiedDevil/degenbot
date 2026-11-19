"""Select the Rust test subjects an outgoing diff invalidates, as `-p` flags.

The pre-push tier certifies the lint contract over the whole tree and the tests
of what is about to be pushed. Which tests is a dependency question, not a
directory question: editing a foundation crate invalidates every package that
depends on it, and editing a workspace-wide build input invalidates all of them.
So the selection is the touched packages closed over the declared dependency
graph, unless the diff touches an input the whole workspace consumes, in which
case every member is selected. A path under `rust/` that no member owns is a
refusal rather than an empty selection, because incomplete knowledge must not
read as "no test needs to run" — except documentation, which no build reads and
which therefore invalidates nothing.

Prints nothing when the outgoing diff touches no Rust test subject, so the
caller runs no test leg at all.
"""

from __future__ import annotations

import json
import pathlib
import subprocess  # ruff: ignore[suspicious-subprocess-import] -- git and cargo plumbing
import sys
from typing import Any

MANIFEST = pathlib.Path("rust") / "Cargo.toml"

# Suffixes of files no build reads. An unowned file under `rust/` carrying one
# invalidates no test, so it is the one refusal case answered by knowledge
# instead of by declining to guess.
DOCUMENTATION_SUFFIXES = frozenset({".md"})

# Inputs every workspace member's build or test behavior reads. Touching one
# invalidates the whole workspace without touching any member's directory.
GLOBAL_INPUTS = frozenset({
    "rust/Cargo.toml",
    "rust/Cargo.lock",
    "rust/.config/nextest.toml",
    "rust-toolchain.toml",
    ".cargo/config.toml",
    "pyproject.toml",
})


class SelectionRefused(Exception):
    """The outgoing diff cannot be mapped to workspace packages."""


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
    """Map each workspace member's directory (repo-relative) to its name.

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


def _dependents(metadata: dict[str, Any], members: set[str]) -> dict[str, set[str]]:
    """Invert every declared dependency edge whose target is a workspace member.

    `cargo metadata --no-deps` reports normal, development, build, optional, and
    target-specific dependencies in one `dependencies` array, distinguished by
    `kind`, `optional`, and `target`; one pass therefore covers every edge kind,
    and the conservatism is deliberate: an edge that only applies to one host or
    feature set still selects its dependent. Each edge names the depended-on
    package, never its in-manifest alias (`rename`), which is what member names
    are keyed by.

    Args:
        metadata: A `cargo metadata --no-deps` payload.
        members: The workspace member names.

    Returns:
        `package -> the members that declare a dependency on it`.
    """
    dependents: dict[str, set[str]] = {member: set() for member in members}
    for package in metadata["packages"]:
        name = package["name"]
        if name not in members:
            continue
        for dependency in package.get("dependencies", []):
            target = dependency.get("name")
            if target in members:
                dependents[target].add(name)
    return dependents


def _specificity(directories: dict[str, str]) -> list[tuple[str, str]]:
    """Order directory/name pairs most-specific first.

    Args:
        directories: One `directory -> package name` entry per member.

    Returns:
        The entries sorted by decreasing directory length, so a nested member
        (a vendored `.../jit/backend` under `.../jit`) that shares a prefix with
        its parent still claims its own files.
    """
    return sorted(directories.items(), key=lambda entry: len(entry[0]), reverse=True)


def _closure(seeds: set[str], dependents: dict[str, set[str]]) -> set[str]:
    """Return the seeds plus every package that transitively depends on them.

    Args:
        seeds: The directly touched packages.
        dependents: The inverted dependency edges.

    Returns:
        The selected package names.
    """
    reached: set[str] = set()
    pending = list(seeds)
    while pending:
        name = pending.pop()
        if name in reached:
            continue
        reached.add(name)
        pending.extend(dependents.get(name, ()))
    return reached


def affected_packages(metadata: dict[str, Any], paths: list[str], root: pathlib.Path) -> list[str]:
    """Return the packages a diff invalidates, sorted and deduplicated.

    Args:
        metadata: A `cargo metadata --no-deps` payload.
        paths: Repo-relative paths, usually the outgoing diff's file list.
        root: The repository root.

    Raises:
        SelectionRefused: A path under `rust/` belongs to no member and is not a
            declared global input.

    Returns:
        The directly touched packages plus their transitive dependents.
    """
    directories = package_directories(metadata, root)
    members = set(directories.values())
    owned = _specificity(directories)
    dependents = _dependents(metadata, members)
    selected: set[str] = set()
    for path in paths:
        if path in GLOBAL_INPUTS:
            selected |= members
            continue
        if not path.startswith("rust/"):
            continue
        for directory, name in owned:
            if path == directory or path.startswith(directory + "/"):
                selected.add(name)
                break
        else:
            if pathlib.Path(path).suffix in DOCUMENTATION_SUFFIXES:
                continue
            raise SelectionRefused(
                f"no workspace package owns {path}; refusing an incomplete test selection"
            )
    return sorted(_closure(selected, dependents))


def _diff_base() -> str:
    """Return the revision the outgoing range starts from.

    Prefers the branch's tracking ref, then the remote's default branch, and
    keeps `origin/main` as the last resort for a clone that records no remote
    HEAD.

    Raises:
        SelectionRefused: No candidate resolves to a commit, so the range (and
            therefore the selection) is unknown.

    Returns:
        A revision name that resolves to a commit.
    """
    candidates: list[str] = []
    upstream = _run(["git", "rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{u}"])
    if upstream.returncode == 0:
        candidates.append(upstream.stdout.strip())
    head = _run(["git", "symbolic-ref", "--short", "refs/remotes/origin/HEAD"])
    if head.returncode == 0:
        candidates.append(head.stdout.strip())
    candidates.append("origin/main")
    for candidate in candidates:
        if not candidate:
            continue
        probe = _run(["git", "rev-parse", "--verify", "--quiet", f"{candidate}^{{commit}}"])
        if probe.returncode == 0:
            return candidate
    reason = (
        "cannot resolve the base of the outgoing range (no tracking branch,"
        " origin/HEAD, or origin/main); refusing an incomplete test selection"
    )
    raise SelectionRefused(reason)


def outgoing_paths() -> list[str]:
    """Return the files in the commits about to be pushed.

    Rename detection is disabled so a move across packages reports both its old
    and new path, letting each side keep its own owner.

    Raises:
        SelectionRefused: The base cannot be resolved or `git diff` fails.

    Returns:
        Repo-relative paths, one per changed side.
    """
    base = _diff_base()
    diff = _run(["git", "diff", "--no-renames", "--name-only", f"{base}...HEAD"])
    if diff.returncode != 0:
        raise SelectionRefused(
            f"git diff {base}...HEAD failed: {diff.stderr.strip() or 'no diagnostic'}"
        )
    return [line for line in diff.stdout.splitlines() if line]


def _metadata() -> dict[str, Any]:
    """Return the offline workspace metadata payload.

    Raises:
        SelectionRefused: Cargo fails or answers with something other than a
            JSON payload.

    Returns:
        The parsed `cargo metadata --no-deps` document.
    """
    metadata = _run([
        "cargo",
        "metadata",
        "--no-deps",
        "--offline",
        "--format-version",
        "1",
        "--manifest-path",
        str(MANIFEST),
    ])
    if metadata.returncode != 0:
        raise SelectionRefused(
            f"cargo metadata --no-deps for {MANIFEST} failed:"
            f" {metadata.stderr.strip() or 'no diagnostic'}"
        )
    try:
        payload: dict[str, Any] = json.loads(metadata.stdout)
    except json.JSONDecodeError as malformed:
        raise SelectionRefused(
            f"cargo metadata --no-deps for {MANIFEST} returned no JSON payload"
        ) from malformed
    return payload


def main(argv: list[str]) -> int:
    """Print the `-p` flags for the outgoing diff, or an override path list.

    Args:
        argv: Positional paths override the diff (a debugging and test seam).

    Returns:
        A process exit status; nonzero when selection is refused.
    """
    root = pathlib.Path.cwd()
    try:
        metadata = _metadata()
        paths = argv or outgoing_paths()
        names = affected_packages(metadata, paths, root)
    except SelectionRefused as refusal:
        sys.stderr.write(f"rust_affected_packages: {refusal}\n")
        return 1
    flags = "".join(f"-p {name} " for name in names)
    if flags:
        sys.stdout.write(f"{flags}\n")
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
