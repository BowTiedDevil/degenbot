#!/usr/bin/env python3
"""The static publication plan: one authority for the workspace's publication facts.

# live-invariant gate (ADR-087 D2): derives every fact from the live workspace
# manifests; not tripwire precedent.

What this module is
-------------------

Every static membership question the publication chain asks is answered by one
planner, so no consumer re-derives the rule. The facts, all read from the
manifests the `cargo metadata --no-deps --offline` invocation reports -- never
from the lockfile's memberships, never from the network: the child cargo runs
`--offline`, so the metadata read cannot reach a registry, and crates.io is
not queried anywhere here (the live audit is the only index reader, and the
verify oracle's registry is the only resolver):

- ELIGIBLE. The members `cargo publish` could upload at all: the metadata
  `publish` value is not `[]`. An unrestricted member carries no `publish`
  key (and `publish = true` normalizes to null); `publish = false`
  normalizes to `[]` and is ABSENT -- cargo skips it at upload, so it is in
  no set below.

- ALLOWLISTED. Eligible members whose `publish` names specific registries.
  They stay in the live audit's classification (an audit may never silently
  omit a member cargo would upload somewhere) and the verify oracle refuses
  them explicitly before any upload: cargo will not carry such a member to
  any other registry name, and the one registry the oracle drives is the
  loopback.

- WITHHELD. The census rules (`scripts/publish-blocked-census.txt`'s gate
  body): the sites a published manifest retains that cannot resolve on
  crates.io -- a version-keyed path or git dependency retained as a version
  requirement naming code not on crates.io -- plus every eligible member
  whose retained requirements reach a withheld or absent member. The one
  dependency shape cargo strips at packaging (a version-less path/git
  dev-dependency) is not a requirement: it is excluded from the retained
  edges, blocks nothing, and can never close a cycle.

- RESOLUTION. Eligible minus withheld: the static set of members whose
  rewritten manifests must resolve for a release to proceed. The verify
  oracle uploads exactly this set to its loopback registry (refusing
  allowlisted members first) and verify-builds the pending part of it.

- CYCLE. A cycle formed by registry-retained normal AND dev dependencies
  among eligible members is a release-graph defect, not a withheld member:
  no upload order exists, so no census row may stand in for one. The plan
  carries the cycle as a fact; the publication consumers (the census gate,
  the verify oracle) fail on it before any packaging. The live audit's
  membership classification does not package, so it proceeds -- exactly as
  before the plan existed.

The interface is one deep read: `load_plan` (or `plan_from_metadata` over
metadata a caller already holds) returns a `PublicationPlan` carrying every
fact above plus the package metadata behind it. A workspace the plan cannot
certify -- a `cargo metadata` failure, no members, no eligible member -- is a
`PublicationRefusal`, never a vacuously green answer.

Consumers and migration
-----------------------

- `scripts/publish_blocked.py` (the census gate) owns census-file
  reconciliation and reports the plan's withheld facts against it; cycle and
  eligibility wording it previously owned itself now come from here, byte
  for byte.

- `scripts/crates_io_pending.sh` (the live audit) is a shell adapter over
  this module's CLI (`--eligible`): the audit never re-states the publish
  rule. Consumer-visible behavior is unchanged.

- `scripts/publish_verify.py` (the verify oracle) consumes the same plan
  object over metadata it already loaded, so the eligibility, withholding,
  and resolution sets it act on cannot drift from the gate's. It still runs
  the census gate for census-file reconciliation (the gate's own exit-code
  contract).

- `.github/workflows/release.yml` provisions this module beside the audit
  script, so a resumed older tag cannot be left with an audit whose planner
  is missing.

Consumer-visible transitions of the migration: none. Every set, message, and
exit code each consumer produced before is produced by the same consumer now;
the only new fact is shared ownership.

The CLI (`--eligible`) prints the eligible set for shell callers -- the live
audit's seam. The withheld, resolution, and allowlisted facts live on the
`PublicationPlan` object for Python consumers. The CLI reports facts and
refuses uncertifiable workspaces; cycle enforcement belongs to the publication
consumers, which fail before any packaging.
"""

from __future__ import annotations

import argparse
import json
import subprocess
import sys
from dataclasses import dataclass
from pathlib import Path
from typing import Any

REPO_ROOT = Path(__file__).resolve().parents[1]
MANIFEST_REL = "rust/Cargo.toml"

VERSION_LESS = "(version-less)"

# The dep payloads are cargo metadata entries read whole and mirrored into the
# census reasons; no field is validated, so they stay Any-typed through the
# helpers below (the same spelling `scripts/publish_verify.py` uses for its
# metadata payloads).
Json = dict[str, Any]


class PublicationRefusal(Exception):
    """The plan declines to certify a workspace it could not fully read."""


@dataclass(frozen=True)
class PublicationPlan:
    """The static publication facts one workspace's manifests imply.

    `members` is every workspace member's cargo metadata package object keyed
    by name; `eligible`, `allowlisted`, and `resolution` (`eligible` minus
    `withheld`) are its publish-relevant sub-populations; `withheld` carries
    the census reason beside each withheld name; `cycle` is the closed name
    list of a retained-edge cycle, or None when the release graph can be
    ordered. `origin` is the label refusal messages name (the manifest path
    that produced the plan).

    """

    origin: str
    members: dict[str, Json]
    eligible: dict[str, Json]
    allowlisted: dict[str, Json]
    withheld: dict[str, str]
    cycle: list[str] | None

    @property
    def resolution(self) -> dict[str, Json]:
        """Eligible minus withheld: the static resolution set.

        Returns:
            The package metadata of every member whose rewritten manifest
            must resolve for a release to proceed.

        """
        return {
            name: package for name, package in self.eligible.items() if name not in self.withheld
        }


def cycle_message(cycle: list[str]) -> str:
    """Render the retained-edge cycle the publication consumers fail on.

    The census gate prints this verbatim (exit 1, before any census work) and
    the verify oracle refuses with it before any packaging; one rendering
    keeps the two from drifting.

    Args:
        cycle: The closed name list `PublicationPlan.cycle` carries.

    Returns:
        The failure message, cycle named.

    """
    return (
        "publishable dependency cycle -- no crates.io publish order can "
        f"exist; repair the manifests:\n  {' -> '.join(cycle)}"
    )


def _metadata(manifest: Path, *, locked: bool) -> Json:
    """Read the workspace's member metadata; refuse anything but a conclusive read.

    Args:
        manifest: The workspace manifest path handed to cargo.
        locked: Pins the read to the tree's own lockfile (the live audit's
            resolution-act guarantee); the census gate reads without it.

    Returns:
        The `cargo metadata --format-version 1 --no-deps --offline` document.

    Raises:
        PublicationRefusal: cargo failed -- a workspace this module cannot
            enumerate is a workspace it must not certify.

    """
    # --offline makes the no-network guarantee structural: the child cargo
    # cannot reach a registry, not merely trusted not to.
    command = [
        "cargo",
        "metadata",
        "--format-version",
        "1",
        "--no-deps",
        "--offline",
        "--manifest-path",
        str(manifest),
    ]
    if locked:
        command.append("--locked")
    proc = subprocess.run(command, check=False, capture_output=True, text=True)
    if proc.returncode != 0:
        msg = f"cargo metadata failed (exit {proc.returncode}):\n{proc.stderr.strip()}"
        raise PublicationRefusal(msg)
    return json.loads(proc.stdout)


def _publish_selection(package: Json) -> str:
    """Classify one member's `publish` value: `eligible`, `allowlisted`, or `absent`.

    Args:
        package: The member's cargo metadata package object.

    Returns:
        `eligible` for an unrestricted member (no key, or the `publish = true`
        null cargo reports), `allowlisted` for a member whose `publish` names
        registries, `absent` for `publish = false` (cargo reports `[]`).

    """
    publish = package.get("publish")
    if publish is None or publish is True:
        return "eligible"
    if publish:
        return "allowlisted"
    return "absent"


def plan_from_metadata(meta: Json, *, origin: str) -> PublicationPlan:
    """Derive the static publication plan from a loaded metadata document.

    Args:
        meta: The `cargo metadata --format-version 1 --no-deps` document (the
            same shape `load_plan` reads and the verify oracle already loads
            under its run lock).
        origin: The label refusal messages name -- the manifest path that
            produced the document.

    Returns:
        The plan: members, eligible, allowlisted, withheld, cycle, and the
        resolution set they compose into.

    Raises:
        PublicationRefusal: the document names no workspace member, or no
            eligible member -- a population the publication chain cannot act
            on is a refusal, not an empty green.

    """
    member_ids = set(meta["workspace_members"])
    members = {
        package["name"]: package for package in meta["packages"] if package["id"] in member_ids
    }
    if not members:
        msg = f"no workspace members under {origin}"
        raise PublicationRefusal(msg)
    eligible: dict[str, Json] = {}
    allowlisted: dict[str, Json] = {}
    for name, package in members.items():
        selection = _publish_selection(package)
        if selection == "absent":
            continue
        eligible[name] = package
        if selection == "allowlisted":
            allowlisted[name] = package
    if not eligible:
        msg = f"no publishable member under {origin}"
        raise PublicationRefusal(msg)
    edges = _retained_edges(eligible, set(members))
    return PublicationPlan(
        origin=origin,
        members=members,
        eligible=eligible,
        allowlisted=allowlisted,
        withheld=_withheld_population(eligible, set(members)),
        cycle=_find_cycle(edges),
    )


def load_plan(manifest: Path, *, locked: bool = False) -> PublicationPlan:
    """Load the static publication plan for the workspace `manifest` describes.

    Args:
        manifest: The workspace manifest path handed to `cargo metadata`.
        locked: Pins the metadata read to the tree's own lockfile.

    Returns:
        The plan. The metadata read failing, or the workspace read not being
        certifiable (no members, no eligible member), surfaces as the
        `PublicationRefusal` the plan raises for either.

    """
    return plan_from_metadata(_metadata(manifest, locked=locked), origin=str(manifest))


def _dep_label(dep: Json) -> str:
    """One dep rendering shared by every reason: `name@req` or `name (version-less)`.

    Args:
        dep: The cargo metadata dependency entry.

    Returns:
        The rendering, shared by every reason.

    """
    if dep["req"] == "*":
        return f"{dep['name']} {VERSION_LESS}"
    return f"{dep['name']}@{dep['req']}"


def _source_kind(dep: Json) -> str | None:
    """`git`, `path`, or None for a plain registry dependency.

    Args:
        dep: The cargo metadata dependency entry.

    Returns:
        The dependency's source kind.

    """
    source = dep.get("source") or ""
    if source.startswith("git+"):
        return "git"
    if not source:
        return "path"
    return None


def _stripped(dep: Json) -> bool:
    """Rule 1: the dependency leaves the published manifest at packaging.

    Args:
        dep: The cargo metadata dependency entry.

    Returns:
        True for a version-less path or git dev-dependency.

    """
    return bool(
        dep.get("kind") == "dev" and dep["req"] == "*" and _source_kind(dep) in {"git", "path"}
    )


def _withheld_population(eligible: dict[str, Json], member_names: set[str]) -> dict[str, str]:
    """Compute the withheld population and its per-member reason (rules 1-4).

    Args:
        eligible: The eligible members' package metadata keyed by name.
        member_names: The names of every workspace member, eligible or not.

    Returns:
        One entry per withheld member: the reason the manifests imply.

    """
    sites: dict[str, list[str]] = {}
    edges: dict[str, list[str]] = {}
    # publish = false, or not a member: a requirement target no upload of
    # this workspace can ever satisfy.
    unavailable = {name for name in member_names if name not in eligible}
    for member in eligible.values():
        site_labels: list[tuple[str, str]] = []
        requires: list[str] = []
        for dep in member["dependencies"]:
            if _stripped(dep):
                continue
            kind = _source_kind(dep)
            if kind == "git" or (kind == "path" and dep["name"] not in member_names):
                # An off-workspace path dependency is rewritten the same way.
                site_labels.append((dep["name"], f"{kind} dep {_dep_label(dep)}"))
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


def _retained_edges(eligible: dict[str, Json], member_names: set[str]) -> dict[str, set[str]]:
    """Collect the upload-ordering edges among eligible members (rules 1 and 3).

    A dependency orders its dependent after its target exactly when the
    published manifest retains it: every non-stripped dependency on another
    workspace member, normal or dev. Stripped dev-dependencies are not
    requirements, so they carry no ordering constraint and no cycle.

    Args:
        eligible: The eligible members' package metadata keyed by name.
        member_names: The names of every workspace member, eligible or not.

    Returns:
        The adjacency: member name -> the members it requires.

    """
    edges: dict[str, set[str]] = {}
    for member in eligible.values():
        name = member["name"]
        targets = {
            dep["name"]
            for dep in member["dependencies"]
            if dep["name"] in member_names and dep["name"] != name and not _stripped(dep)
        }
        if targets:
            edges[name] = targets
    return edges


def _find_cycle(edges: dict[str, set[str]]) -> list[str] | None:
    """One cycle in `edges` as a closed name list, or None when acyclic.

    Iterative colored DFS over the sorted graph, so the named cycle is
    deterministic for a given set of manifests.

    Args:
        edges: The retained-edge adjacency.

    Returns:
        The cycle's closed name list, or None when the graph is acyclic.

    """
    unvisited, in_progress, done = 0, 1, 2
    color: dict[str, int] = dict.fromkeys(edges, unvisited)
    for start in sorted(edges):
        if color[start] != unvisited:
            continue
        color[start] = in_progress
        path = [start]
        iters = [iter(sorted(edges[start]))]
        while path:
            advanced = False
            for nxt in iters[-1]:
                state = color.get(nxt, done)
                if state == in_progress:
                    return [*path[path.index(nxt) :], nxt]
                if state == unvisited:
                    color[nxt] = in_progress
                    path.append(nxt)
                    iters.append(iter(sorted(edges[nxt])))
                    advanced = True
                    break
            if not advanced:
                color[path.pop()] = done
                iters.pop()
    return None


def main(argv: list[str]) -> int:
    """Print the fact view `argv` selects; see the module docstring.

    Args:
        argv: The command-line arguments, without the program name.

    Returns:
        The process exit code: 0 with the facts on stdout, 2 when the
        workspace cannot be certified (the refusal message is on stderr).

    """
    parser = argparse.ArgumentParser(
        description="The static publication plan: Cargo eligibility, census "
        "withholding, and resolution membership in one authority."
    )
    parser.add_argument(
        "--manifest-path",
        type=Path,
        default=REPO_ROOT / MANIFEST_REL,
        help="workspace manifest (default: the repo manifest beside this script)",
    )
    parser.add_argument(
        "--locked",
        action="store_true",
        help="pin the membership metadata read to the tree's own lockfile",
    )
    parser.add_argument("--eligible", action="store_true", help="members cargo could upload")
    args = parser.parse_args(argv)

    try:
        plan = load_plan(args.manifest_path, locked=args.locked)
    except PublicationRefusal as exc:
        print(f"publication-plan: {exc}", file=sys.stderr)
        return 2

    names = sorted(plan.eligible)
    if names:
        print("\n".join(names))
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
