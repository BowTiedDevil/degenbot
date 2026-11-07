#!/usr/bin/env python3
"""The publish-blocked census: the workspace members `cargo publish` cannot upload.

# live-invariant gate (ADR-087 D2): watches the live workspace manifests and the
# dependency sources their published forms retain against
# scripts/publish-blocked-census.txt; not tripwire precedent.


What this gate is
-----------------

`cargo publish` rewrites every path and git dependency at packaging: the source
key is stripped and what remains is a version requirement that must resolve on
crates.io. What a member *publishes* is therefore never what it *tests* unless
its external dependencies are declared as registry dependencies, and a member
whose retained requirements cannot resolve on crates.io does not upload at all.


The population of such members is machine-derived from the manifests and
recorded, one row per member with its reason, in the census beside this script.
`just publish-dry-run` withholds exactly that population from the run and keeps
its file-inclusion coverage through `cargo package --list`, so the members the
publish oracle does not attempt are named in the tree rather than dropped by
silence.


The rules
---------

1. A dependency leaves the published manifest only when it is a version-less
   dev-dependency carrying a path or git source (cargo strips it; the
   dev-dependency cannot affect a consumer's build).

2. A version-less path or git dependency in any other slot is rejected outright
   (`all dependencies must have a version requirement specified when
   publishing`), so the member cannot package at all.

3. A path or git dependency carrying a version key is retained as that version
   requirement. For a workspace member the requirement is satisfied by the same
   run's lockstep upload; for any other package it names a crates.io release of
   code developed elsewhere, and the member is withheld until the dependency is
   declared as a registry dependency -- which is also the only shape in which
   what was tested is what was published.

4. A member whose retained dependency requires a withheld member is itself
   withheld: the requirement cannot resolve on crates.io for as long as the
   required member does not upload at this version.


Reconciliation is bidirectional and fails closed: a withheld member with no
census row fails (the population cannot grow silently, and the run cannot
quietly lose coverage), a row with no withheld member fails (a member that can
package again deletes its row in the same change), a row's reason must equal the
reason the manifests imply in both directions, and a missing census, an
unparseable row, a metadata failure, or a workspace with no publishable member
fails instead of certifying a tree it did not examine.

There is no reasonless-high-water line here, unlike the lint-waiver census: a
row's reason is computed from the manifests, so no row can state less than the
site it mirrors. The population's destination is empty -- when the withheld
members' external dependencies become registry dependencies, the rows go with
them.


Exit codes
----------

  0   every withheld member and row reconcile
  1   at least one finding (fresh member, stale row, reason drift)
  2   refused to certify (missing census, unparseable row, no publishable
      member, cargo metadata failure)


Modes
-----

    just check-publish-blocked            # reconcile (the default)
    just check-publish-blocked --rows     # the rows the manifests imply
    just check-publish-blocked --blocked  # the member names to withhold

`--rows` is the regeneration form: paste its output over the census rows
section (the header is maintained by hand).


Scope
-----

Every publishable workspace member of the manifest `--root`/`rust/Cargo.toml`
describes (the packages `cargo metadata --no-deps` reports), read from the
manifests themselves -- never from the lockfile, never from the network. A
member that declares `publish = false` is out of scope except as a requirement
target: cargo skips it at upload, and a retained requirement on it withholds
every member that carries one. Documented blind spots: resolvability is never
queried, so a git dependency carrying a version key stays withheld even after
that version appears on crates.io (the fix is the registry declaration, which is
also the honest one), and a manifest cargo itself would refuse to parse fails
this gate at exit 2 rather than passing over a population it never read.
"""

from __future__ import annotations

import argparse
import json
import subprocess  # ruff: ignore[suspicious-subprocess-import]
import sys
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[1]
CENSUS_REL = "scripts/publish-blocked-census.txt"
MANIFEST_REL = "rust/Cargo.toml"

VERSION_LESS = "(version-less)"


def refuse(message: str) -> None:
    """Exit 2: the gate declines to certify a tree it could not fully read."""
    print(f"publish-blocked: {message} -- refusing to certify", file=sys.stderr)
    raise SystemExit(2)


def load_members(manifest: Path) -> tuple[list[dict], set[str]]:
    """Return the publishable members and the names of every member.

    Refuses (SystemExit 2) on anything but a conclusive read: a workspace this
    gate cannot enumerate is a workspace it must not certify.
    """
    proc = subprocess.run(  # ruff: ignore[subprocess-without-shell-equals-true]
        [
            "cargo",
            "metadata",
            "--format-version",
            "1",
            "--no-deps",
            "--manifest-path",
            str(manifest),
        ],
        check=False,
        capture_output=True,
        text=True,
    )
    if proc.returncode != 0:
        refuse(f"cargo metadata failed (exit {proc.returncode}):\n{proc.stderr.strip()}")
    meta = json.loads(proc.stdout)
    member_ids = set(meta["workspace_members"])
    members = [p for p in meta["packages"] if p["id"] in member_ids]
    if not members:
        refuse(f"no workspace members under {manifest}")
    names = {p["name"] for p in members}
    publishable = [p for p in members if p.get("publish") != []]
    if not publishable:
        refuse(f"no publishable member under {manifest}")
    return publishable, names


def dep_label(dep: dict) -> str:
    """One dep rendering shared by every reason: `name@req` or `name (version-less)`."""
    if dep["req"] == "*":
        return f"{dep['name']} {VERSION_LESS}"
    return f"{dep['name']}@{dep['req']}"


def source_kind(dep: dict) -> str | None:
    """`git`, `path`, or None for a plain registry dependency."""
    source = dep.get("source") or ""
    if source.startswith("git+"):
        return "git"
    if not source:
        return "path"
    return None


def stripped(dep: dict) -> bool:
    """Rule 1: the dependency leaves the published manifest at packaging."""
    return dep.get("kind") == "dev" and dep["req"] == "*" and source_kind(dep) in {"git", "path"}


def blocked_rows(publishable: list[dict], member_names: set[str]) -> dict[str, str]:
    """Compute the withheld population and its per-member reason (rules 1-4)."""
    sites: dict[str, list[str]] = {}
    edges: dict[str, list[str]] = {}
    unavailable = {
        p["name"] for p in publishable
    } ^ member_names  # publish = false, or not a member
    for member in publishable:
        site_labels: list[tuple[str, str]] = []
        requires: list[str] = []
        for dep in member["dependencies"]:
            if stripped(dep):
                continue
            kind = source_kind(dep)
            if kind == "git" or (kind == "path" and dep["name"] not in member_names):
                # An off-workspace path dependency is rewritten the same way.
                site_labels.append((dep["name"], f"{kind} dep {dep_label(dep)}"))
                continue
            if dep["name"] in member_names:
                requires.append(dep["name"])
        if site_labels:
            sites[member["name"]] = [label for _, label in sorted(site_labels)]
        if requires:
            edges[member["name"]] = sorted(set(requires))

    blocked: dict[str, list[str]] = {name: list(labels) for name, labels in sites.items()}
    # Rule 4 closes over dependents until the population is stable.
    changed = True
    while changed:
        changed = False
        for name, targets in edges.items():
            if name in blocked:
                continue
            if any(target in blocked or target in unavailable for target in targets):
                blocked[name] = []
                changed = True

    rows: dict[str, str] = {}
    for name in blocked:
        reasons: list[str] = []
        if name in sites:
            reasons.append(", ".join(sites[name]))
        required_blocked = sorted(t for t in edges.get(name, []) if t in blocked)
        if required_blocked:
            reasons.append(f"depends on blocked {', '.join(required_blocked)}")
        required_unavailable = sorted(t for t in edges.get(name, []) if t in unavailable)
        if required_unavailable:
            reasons.append(f"depends on non-publishable {', '.join(required_unavailable)}")
        rows[name] = "; ".join(reasons)
    return rows


def read_census(path: Path) -> dict[str, str]:
    """Parse `member<TAB>reason` rows; anything else refuses to certify."""
    if not path.is_file():
        refuse(f"missing census {path}")
    rows: dict[str, str] = {}
    for lineno, raw in enumerate(path.read_text(encoding="utf-8").splitlines(), start=1):
        line = raw.rstrip()
        if not line or line.startswith("#"):
            continue
        parts = line.split("\t")
        if len(parts) != 2 or not parts[0] or not parts[1]:
            refuse(f"{path}:{lineno}: row is not '<member>TAB<reason>'")
        if parts[0] in rows:
            refuse(f"{path}:{lineno}: duplicate row for {parts[0]}")
        rows[parts[0]] = parts[1]
    return rows


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description="The publish-blocked census gate.")
    parser.add_argument(
        "--root", type=Path, default=REPO_ROOT, help="tree root (default: this repo)"
    )
    mode = parser.add_mutually_exclusive_group()
    mode.add_argument(
        "--check", action="store_true", help="reconcile the tree against the census (default)"
    )
    mode.add_argument("--rows", action="store_true", help="print the rows the manifests imply")
    mode.add_argument("--blocked", action="store_true", help="print the member names to withhold")
    args = parser.parse_args(argv)

    publishable, member_names = load_members(args.root / MANIFEST_REL)
    rows = blocked_rows(publishable, member_names)

    if args.rows:
        for name in sorted(rows):
            print(f"{name}\t{rows[name]}")
        return 0
    if args.blocked:
        for name in sorted(rows):
            print(name)
        return 0

    census = read_census(args.root / CENSUS_REL)
    findings: list[str] = []
    for name in sorted(rows):
        row = f"{name}\t{rows[name]}"
        if name not in census:
            findings.append(f"publish-blocked member with no census row -- add:\n  {row}")
        elif census[name] != rows[name]:
            findings.append(
                f"reason drift for {name} -- the manifests imply one reason, the census another:\n"
                f"  manifests: {rows[name]}\n  census:    {census[name]}"
            )
    for name in sorted(set(census) - set(rows)):
        findings.append(
            f"census row with no publish-blocked member -- delete:\n  {name}\t{census[name]}"
        )
    if findings:
        print("\n".join(findings))
        return 1
    withheld = ", ".join(sorted(rows)) if rows else "none"
    print(f"publish-blocked census reconciled; withheld from publish-dry-run: {withheld}")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
