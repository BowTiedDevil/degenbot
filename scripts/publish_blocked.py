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

The facts this gate reconciles against the census -- eligibility, the withheld
population and its reasons, the retained edges and their cycles -- are the
static publication plan's (`scripts/publication_plan.py`): the gate owns
census-file reconciliation, the plan owns the rules. The rules below describe
what the plan derives; this file no longer computes them.

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

5. A cycle formed by registry-retained normal AND dev dependencies among
   publishable members fails the gate outright, with the cycle named. Every
   retained requirement is an upload-ordering constraint: `cargo publish
   --workspace` uploads in dependency order, and a member's retained
   dev-dependencies are requirements too -- cargo resolves a member's
   versioned dev-dependencies while preparing its package, including under
   `--no-verify`, so the fresh sibling version must already be resolvable at
   packaging time -- so a normal edge in one direction plus a retained dev
   edge back cannot be ordered and no
   crates.io version of the workspace can satisfy it. The one dependency
   form cargo strips at packaging (the version-less path/git
   dev-dependency) is not a requirement and cannot close a cycle. A cycle
   is never recorded as a census row: it is a release-graph defect to
   repair in the manifests, not a member to withhold, and no census
   exclusion may hide it.

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
  1   at least one finding (fresh member, stale row, reason drift), or a
      retained-edge cycle (named, before any census work)
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
describes (the packages the plan's `cargo metadata --no-deps --offline`
reports), read from the manifests themselves -- never from the lockfile,
never from the network. A
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
import sys
from pathlib import Path
from typing import NoReturn

import publication_plan

REPO_ROOT = Path(__file__).resolve().parents[1]
CENSUS_REL = "scripts/publish-blocked-census.txt"
MANIFEST_REL = publication_plan.MANIFEST_REL

CENSUS_ROW_FIELDS = 2


def refuse(message: str) -> NoReturn:
    """Exit 2: the gate declines to certify a tree it could not fully read.

    Raises:
        SystemExit: always, with the refusal exit code.

    """
    print(f"publish-blocked: {message} -- refusing to certify", file=sys.stderr)
    raise SystemExit(2)


def read_census(path: Path) -> dict[str, str]:
    """Parse `member<TAB>reason` rows; anything else refuses to certify.

    Args:
        path: The census file's path.

    Returns:
        The rows: member name -> reason.

    """
    if not path.is_file():
        refuse(f"missing census {path}")
    rows: dict[str, str] = {}
    for lineno, raw in enumerate(path.read_text(encoding="utf-8").splitlines(), start=1):
        line = raw.rstrip()
        if not line or line.startswith("#"):
            continue
        parts = line.split("\t")
        if len(parts) != CENSUS_ROW_FIELDS or not parts[0] or not parts[1]:
            refuse(f"{path}:{lineno}: row is not '<member>TAB<reason>'")
        if parts[0] in rows:
            refuse(f"{path}:{lineno}: duplicate row for {parts[0]}")
        rows[parts[0]] = parts[1]
    return rows


def main(argv: list[str]) -> int:
    """Run the gate in the mode `argv` selects; see the module docstring.

    Args:
        argv: The command-line arguments, without the program name.

    Returns:
        The process exit code.

    """
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

    try:
        plan = publication_plan.load_plan(args.root / MANIFEST_REL)
    except publication_plan.PublicationRefusal as exc:
        refuse(str(exc))

    # Rule 5 before any census work: a cycle means no member upload order
    # exists, so reconciliation is moot until the manifests are repaired.
    if plan.cycle is not None:
        print(publication_plan.cycle_message(plan.cycle))
        return 1

    rows = dict(plan.withheld)

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
    findings.extend(
        f"census row with no publish-blocked member -- delete:\n  {name}\t{census[name]}"
        for name in sorted(set(census) - set(rows))
    )
    if findings:
        print("\n".join(findings))
        return 1
    withheld = ", ".join(sorted(rows)) if rows else "none"
    print(f"publish-blocked census reconciled; withheld from publish-dry-run: {withheld}")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
