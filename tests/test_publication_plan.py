"""The static publication plan (``scripts/publication_plan.py``).

One planner answers every static membership question the publication chain
asks -- Cargo eligibility, census withholding, retained-edge/cycle validation,
and resolution membership -- so no consumer re-derives the rule. The tests
here pin the plan's own facts, both through the module interface (driven over
synthetic metadata and over throwaway workspaces) and through the CLI the
live audit shells out to. They are the plan-side half of the seams the census
gate (``tests/test_publish_blocked_gate.py``), the live audit
(``tests/test_crates_io_pending.py``), and the verify oracle
(``tests/test_publish_verify_oracle.py``) pin at their CLIs.

  * eligibility -- ``publish != []`` exactly: an unrestricted member and a
    ``publish = true`` member are eligible, both allowlist shapes are eligible
    AND allowlisted, and a ``publish = false`` member is absent from every
    publish-relevant set while still counting as a member (and as a
    requirement target).
  * withholding -- the census rules 1-4: a version-keyed git dependency is a
    site, a version-less git dependency is rejected outright, dependents
    close over the population, and a requirement on a ``publish = false``
    member withholds its carrier as ``depends on non-publishable``.
  * retained edges and cycles -- the version-less path/git dev-dependency is
    the one shape cargo strips: it is no requirement, blocks nothing, and can
    never close a cycle; a retained normal+dev cycle is a fact on the plan
    that names the cycle, and the shared rendering is the message the census
    gate prints and the verify oracle refuses with.
  * resolution -- eligible minus withheld, and the allowlisted members stay
    inside the resolution set (the verify oracle refuses them explicitly
    before upload; the audit keeps classifying them).
  * lockstep -- every internal versioned requirement in
    ``[workspace.dependencies]`` equals ``[workspace.package].version``: the
    rows are identified by their resolved package identity (the alias's
    ``package = "..."`` or the key itself) plus workspace membership and a
    ``path`` source, so the umbrella row and the renamed ``evm2*`` rows are
    compared and third-party registry rows are not. A readable tree with a
    stale literal is a finding on the plan, never a refusal; a workspace
    without a shared version has no invariant to validate, so the
    independent-version fixtures stay valid.
  * refusals -- a metadata read failure, a memberless workspace, and a
    workspace with no eligible member are ``PublicationRefusal``, never a
    vacuously green plan; the CLI translates to exit 2.
  * no network -- the metadata read runs ``cargo metadata --no-deps
    --offline``: the child cargo itself cannot reach a registry, so the
    structural guarantee does not rest on watching the parent process (whose
    sockets are also refused, belt beside braces); the real workspace's plan
    still loads.
  * CLI -- the single ``--eligible`` view prints sorted names (the live
    audit's seam); the withheld, resolution, and allowlisted facts live on
    the ``PublicationPlan`` object and are tested there; ``--offline``,
    ``--no-deps``, and ``--locked`` are forwarded to the membership metadata
    read, the trust anchor the live audit pins (asserted through a recording
    cargo shim, the same seam ``tests/test_crates_io_pending.py`` uses).
  * the live release projection -- ``LivePublicationPlan`` composes the
    static plan with the audit's conclusive ``--classify`` stream (the CLI's
    ``--release-plan`` mode, stdin in, JSON out) into the one contract the
    release workflow consumes: fresh, partial-resume, fully-live, withheld,
    and allowlisted plans; unknown/duplicate/missing/invalid-state
    classification and static cycles refused loudly; exit codes 0/2/3.
"""

from __future__ import annotations

import json
import os
import shlex
import shutil
import socket
import subprocess  # ruff: ignore[suspicious-subprocess-import]
import sys
from pathlib import Path
from typing import Any

import pytest

from tests.repo_script import load_script

_REPO_ROOT = Path(__file__).resolve().parents[1]
_PLAN = _REPO_ROOT / "scripts" / "publication_plan.py"
_GIT_DEP = 'evm2 = {{ git = "https://example.invalid/evm2.git"{version} }}'
_VERSIONED = ', version = "0.1.0"'


def _plan_module() -> Any:
    """The real script, imported for the focused fact and refusal tests."""
    return load_script("publication_plan", _PLAN)


def _tree(tmp_path: Path, members: dict[str, str], *, workspace_extra: str = "") -> Path:
    """A throwaway workspace whose members carry `members`' manifest extras.

    `workspace_extra` is appended to the root manifest, so a fixture can carry
    a shared version and `[workspace.dependencies]` rows -- or omit them, the
    independent-version shape with nothing to validate.
    """
    root = tmp_path / "tree"
    names = list(members)
    workspace = root / "rust" / "Cargo.toml"
    workspace.parent.mkdir(parents=True)
    quoted = ", ".join(f'"crates/{name}"' for name in names)
    workspace.write_text(
        f'[workspace]\nmembers = [{quoted}]\nresolver = "2"\n{workspace_extra}', encoding="utf-8"
    )
    for name, extras in members.items():
        crate = root / "rust" / "crates" / name
        (crate / "src").mkdir(parents=True)
        (crate / "src" / "lib.rs").write_text("", encoding="utf-8")
        manifest = f'[package]\nname = "{name}"\nversion = "0.1.0"\nedition = "2021"\n{extras}'
        (crate / "Cargo.toml").write_text(manifest, encoding="utf-8")
    return root


def _manifest(tree: Path) -> Path:
    return tree / "rust" / "Cargo.toml"


def _deps(body: str) -> str:
    return f"[dependencies]\n{body}\n" if body else ""


def _dev_deps(body: str) -> str:
    return f"[dev-dependencies]\n{body}\n"


def _run_cli(tree: Path, *args: str) -> subprocess.CompletedProcess[str]:
    """Run the real CLI over `tree` -- the seam the live audit shells into."""
    return subprocess.run(  # ruff: ignore[subprocess-without-shell-equals-true]
        [
            sys.executable,
            str(_PLAN),
            "--manifest-path",
            str(_manifest(tree)),
            *args,
        ],
        check=False,
        capture_output=True,
        text=True,
    )


def test_eligible_allowlisted_and_absent_facts(tmp_path: Path) -> None:
    """`publish != []` is eligibility; an allowlist is eligible AND allowlisted;
    `publish = false` is absent from every publish-relevant set but still a
    member (and a requirement target)."""
    publication_plan = _plan_module()
    tree = _tree(
        tmp_path,
        {
            "plan-free": "",
            "plan-true": "publish = true\n",
            "plan-io": 'publish = ["crates-io"]\n',
            "plan-foreign": 'publish = ["private-reg"]\n',
            "plan-priv": "publish = false\n",
        },
    )
    plan = publication_plan.load_plan(_manifest(tree))
    assert set(plan.eligible) == {
        "plan-free",
        "plan-true",
        "plan-io",
        "plan-foreign",
    }
    assert set(plan.allowlisted) == {"plan-io", "plan-foreign"}
    assert set(plan.members) == set(plan.eligible) | {"plan-priv"}
    assert plan.resolution.keys() == plan.eligible.keys()
    assert plan.withheld == {}
    assert plan.cycle is None


def test_withheld_reasons_follow_the_census_rules(tmp_path: Path) -> None:
    """Rules 2-4, with the reason spellings the census rows mirror."""
    publication_plan = _plan_module()
    tree = _tree(
        tmp_path,
        {
            # rule 3: a version-keyed git dependency is a retained site.
            "plan-site": _deps(_GIT_DEP.format(version=_VERSIONED)),
            # rule 2: a version-less git dependency is rejected outright.
            "plan-void": _deps('evm2 = { git = "https://example.invalid/evm2.git" }'),
            # rule 4: a retained requirement closes over the population.
            "plan-dependent": _deps('plan-site = { path = "../plan-site", version = "0.1.0" }'),
            # rule 4: a requirement on a publish = false member withholds its
            # carrier as depends on non-publishable.
            "plan-carrier": _deps('plan-priv = { path = "../plan-priv", version = "0.1.0" }'),
            "plan-priv": "publish = false\n",
        },
    )
    plan = publication_plan.load_plan(_manifest(tree))
    assert plan.withheld == {
        "plan-site": "git dep evm2@^0.1.0",
        "plan-void": "git dep evm2 (version-less)",
        "plan-dependent": "depends on blocked plan-site",
        "plan-carrier": "depends on non-publishable plan-priv",
    }
    # resolution = eligible minus withheld; the absent member is in neither.
    assert set(plan.resolution) == set(plan.members) - set(plan.withheld) - {"plan-priv"}
    # the census row spelling the gate mirrors, straight off the plan object.
    assert [f"{name}\t{plan.withheld[name]}" for name in sorted(plan.withheld)] == [
        "plan-carrier\tdepends on non-publishable plan-priv",
        "plan-dependent\tdepends on blocked plan-site",
        "plan-site\tgit dep evm2@^0.1.0",
        "plan-void\tgit dep evm2 (version-less)",
    ]


def test_a_versionless_dev_dep_is_stripped_from_retained_edges(tmp_path: Path) -> None:
    """Rule 1: the one shape cargo strips is no requirement -- it blocks
    nothing and orders nothing, so a dependent carrying it stays eligible."""
    publication_plan = _plan_module()
    tree = _tree(
        tmp_path,
        {
            "plan-site": _deps(_GIT_DEP.format(version=_VERSIONED)),
            "plan-dev": _dev_deps('plan-site = { path = "../plan-site" }'),
        },
    )
    plan = publication_plan.load_plan(_manifest(tree))
    assert set(plan.withheld) == {"plan-site"}
    assert set(plan.resolution) == {"plan-dev"}
    assert plan.cycle is None


def test_a_retained_cycle_is_a_fact_that_names_the_cycle(tmp_path: Path) -> None:
    """A retained normal+dev cycle among eligible members is the plan's cycle
    fact, not a withheld member, and the shared rendering names it."""
    publication_plan = _plan_module()
    tree = _tree(
        tmp_path,
        {
            "plan-a": _deps('plan-b = { path = "../plan-b", version = "0.1.0" }'),
            "plan-b": _dev_deps('plan-a = { path = "../plan-a", version = "0.1.0" }'),
        },
    )
    plan = publication_plan.load_plan(_manifest(tree))
    assert plan.cycle == ["plan-a", "plan-b", "plan-a"]
    assert plan.withheld == {}
    assert publication_plan.cycle_message(plan.cycle) == (
        "publishable dependency cycle -- no crates.io publish order can "
        "exist; repair the manifests:\n  plan-a -> plan-b -> plan-a"
    )


def test_a_stripped_dev_dep_does_not_close_a_cycle(tmp_path: Path) -> None:
    """The version-less dev-dependency cargo strips cannot close a cycle."""
    publication_plan = _plan_module()
    tree = _tree(
        tmp_path,
        {
            "plan-a": _deps('plan-b = { path = "../plan-b", version = "0.1.0" }'),
            "plan-b": _dev_deps('plan-a = { path = "../plan-a" }'),
        },
    )
    plan = publication_plan.load_plan(_manifest(tree))
    assert plan.cycle is None
    assert plan.withheld == {}


def test_plan_from_metadata_refuses_a_memberless_document() -> None:
    """A document naming no member is a refusal, never an empty green."""
    publication_plan = _plan_module()
    with pytest.raises(publication_plan.PublicationRefusal) as exc:
        publication_plan.plan_from_metadata({"workspace_members": [], "packages": []}, origin="m")
    assert "no workspace members under m" in str(exc.value)


def test_plan_from_metadata_refuses_no_eligible_member() -> None:
    """A workspace where every member declares publish = false cannot be
    certified: the plan refuses instead of reporting empty sets."""
    publication_plan = _plan_module()
    package = {
        "id": "plan-priv",
        "name": "plan-priv",
        "publish": [],
        "dependencies": [],
    }
    meta = {"workspace_members": ["plan-priv"], "packages": [package]}
    with pytest.raises(publication_plan.PublicationRefusal) as exc:
        publication_plan.plan_from_metadata(meta, origin="m")
    assert "no publishable member under m" in str(exc.value)


def test_load_plan_refuses_a_failed_metadata_read(tmp_path: Path) -> None:
    """A manifest cargo cannot read is a refusal naming the failure."""
    publication_plan = _plan_module()
    with pytest.raises(publication_plan.PublicationRefusal) as exc:
        publication_plan.load_plan(tmp_path / "nowhere" / "Cargo.toml")
    assert "cargo metadata failed" in str(exc.value)


# --- The shared-version lockstep invariant --------------------------------
#
# ADR-009's lockstep rule reaches every internal versioned requirement in the
# root manifest's `[workspace.dependencies]`, the umbrella row and the renamed
# `evm2*` rows included: the literal must equal `[workspace.package].version`,
# or cargo packs crates whose manifests require stale siblings and the
# registry rejects the upload. The plan carries a readable tree's mismatch as
# a fact for the census gate to report; it refuses only a manifest it cannot
# read. A workspace with no shared version has no invariant.


def _shared(version: str, rows: str = "") -> str:
    """A root-manifest fragment with the shared version and workspace rows."""
    return f'\n\n[workspace.package]\nversion = "{version}"\n\n[workspace.dependencies]\n{rows}'


def test_lockstep_flags_a_stale_umbrella_requirement(tmp_path: Path) -> None:
    """The umbrella row -- key = resolved package, `path` source, versioned --
    is an internal requirement: a literal behind the shared version is a
    finding on the plan, never a refusal, naming the row and both literals."""
    publication_plan = _plan_module()
    tree = _tree(
        tmp_path,
        {"plan-umbrella": ""},
        workspace_extra=_shared(
            "0.2.0", 'plan-umbrella = { version = "0.1.0", path = "crates/plan-umbrella" }'
        ),
    )
    plan = publication_plan.load_plan(_manifest(tree))
    assert [drift.render() for drift in plan.lockstep] == [
        (
            "lockstep version drift for [workspace.dependencies] 'plan-umbrella' "
            "(package plan-umbrella): requires 0.1.0, [workspace.package].version is 0.2.0"
        )
    ]
    drift = plan.lockstep[0]
    assert (drift.required, drift.shared) == ("0.1.0", "0.2.0")


def test_lockstep_flags_a_stale_aliased_requirement(tmp_path: Path) -> None:
    """A renamed `evm2*` row is identified by its resolved package identity
    (`package = ...`), not by its key, so the alias's literal is compared
    against the shared version too."""
    publication_plan = _plan_module()
    tree = _tree(
        tmp_path,
        {"plan-evm2": "", "plan-user": _deps("evm2 = { workspace = true }")},
        workspace_extra=_shared(
            "0.2.0",
            'evm2 = { package = "plan-evm2", path = "crates/plan-evm2", '
            'version = "0.1.0", default-features = false }',
        ),
    )
    plan = publication_plan.load_plan(_manifest(tree))
    assert [
        (drift.row, drift.package, drift.required, drift.shared) for drift in plan.lockstep
    ] == [("evm2", "plan-evm2", "0.1.0", "0.2.0")]


def test_lockstep_accepts_a_clean_renamed_row(tmp_path: Path) -> None:
    """A renamed row at the shared version is clean; a registry row and a
    version-less path row are not internal versioned requirements, so the
    check keys on resolved identity rather than the table's keys."""
    publication_plan = _plan_module()
    tree = _tree(
        tmp_path,
        {
            "plan-evm2": "",
            "plan-user": _deps("evm2 = { workspace = true }"),
            "plan-raw": "",
        },
        workspace_extra=_shared(
            "0.1.0",
            'evm2 = { package = "plan-evm2", path = "crates/plan-evm2", version = "0.1.0" }\n'
            'third-party = { version = "9.9.9" }\n'
            'plan-raw = { path = "crates/plan-raw" }',
        ),
    )
    plan = publication_plan.load_plan(_manifest(tree))
    assert plan.lockstep == ()


def test_lockstep_has_no_invariant_without_a_shared_version(tmp_path: Path) -> None:
    """A workspace with no `[workspace.package].version` has no shared-version
    invariant: a versioned internal row is not compared, so independent-version
    workspaces stay valid."""
    publication_plan = _plan_module()
    tree = _tree(
        tmp_path,
        {"plan-free": ""},
        workspace_extra=(
            "\n[workspace.dependencies]\n"
            'plan-free = { version = "0.1.0", path = "crates/plan-free" }\n'
        ),
    )
    plan = publication_plan.load_plan(_manifest(tree))
    assert plan.lockstep == ()


@pytest.mark.parametrize(
    ("root_manifest", "expected"),
    [
        (None, "cannot read the workspace root manifest"),
        ("[workspace]\nmembers = []\n\n[workspace.package]\nversion = 1\n", "malformed"),
    ],
)
def test_lockstep_refuses_a_root_manifest_it_cannot_read(
    tmp_path: Path, root_manifest: str | None, expected: str
) -> None:
    """A root manifest the plan cannot read, or whose shared version is not a
    string, is a refusal -- never a silently skipped invariant. The metadata
    document is hand-built because cargo itself refuses these shapes."""
    publication_plan = _plan_module()
    rust = tmp_path / "rust"
    rust.mkdir()
    if root_manifest is not None:
        (rust / "Cargo.toml").write_text(root_manifest, encoding="utf-8")
    meta = {
        "workspace_root": str(rust),
        "workspace_members": ["plan-free"],
        "packages": [{"id": "plan-free", "name": "plan-free", "publish": None, "dependencies": []}],
    }
    with pytest.raises(publication_plan.PublicationRefusal) as exc:
        publication_plan.plan_from_metadata(meta, origin="m")
    assert expected in str(exc.value)


def test_the_real_plan_loads_structurally_offline(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """The metadata read passes `--offline` to the child cargo, so the plan
    cannot touch a registry regardless of what any process intends: with the
    parent's sockets also refused outright, the real workspace's plan still
    loads. The recording-shim test below pins the argv that makes this
    structural."""
    publication_plan = _plan_module()

    def _no_socket(*args: object, **kwargs: object) -> None:
        message = "the static publication plan must not open a socket"
        raise AssertionError(message)

    monkeypatch.setattr(socket, "socket", _no_socket)
    plan = publication_plan.load_plan(_REPO_ROOT / "rust" / "Cargo.toml")
    assert plan.cycle is None
    assert plan.withheld == {}
    assert plan.allowlisted == {}
    assert plan.lockstep == ()
    assert len(plan.eligible) >= 1


def test_the_cli_prints_the_eligible_set(tmp_path: Path) -> None:
    """The one CLI view -- the live audit's seam -- prints the sorted eligible
    set; the withheld, resolution, and allowlisted facts are PublicationPlan
    attributes tested at the module interface, not CLI modes."""
    tree = _tree(
        tmp_path,
        {
            "plan-site": _deps(_GIT_DEP.format(version=_VERSIONED)),
            "plan-free": "",
        },
    )
    eligible = _run_cli(tree, "--eligible")
    assert eligible.returncode == 0, eligible.stdout + eligible.stderr
    assert eligible.stdout.splitlines() == ["plan-free", "plan-site"]


def test_the_cli_refuses_an_uncertifiable_workspace(tmp_path: Path) -> None:
    """No eligible member: exit 2 with the refusal on stderr."""
    tree = _tree(tmp_path, {"plan-priv": "publish = false\n"})
    result = _run_cli(tree, "--eligible")
    assert result.returncode == 2
    assert not result.stdout
    assert "no publishable member" in result.stderr


def _cargo_shim(tmp_path: Path) -> tuple[Path, Path]:
    """A recording cargo shim: logs the argv it forwards, then delegates."""
    real_cargo = shutil.which("cargo")
    assert real_cargo, "the plan tests need a real cargo to delegate to"
    shim_dir = tmp_path / "cargo-shim"
    shim_dir.mkdir()
    log = shim_dir / "invocations.log"
    shim = shim_dir / "cargo"
    shim.write_text(
        "#!/usr/bin/env bash\n"
        + 'printf "%s\\n" "$*" >> '
        + shlex.quote(str(log))
        + "\n"
        + "exec "
        + shlex.quote(real_cargo)
        + ' "$@"\n',
        encoding="utf-8",
    )
    shim.chmod(0o755)
    return shim_dir, log


@pytest.mark.parametrize("locked", [False, True])
def test_the_cli_forwards_the_metadata_read_contract(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, *, locked: bool
) -> None:
    """The membership metadata read is the plan's trust anchor, passed as one
    exact argv: --format-version 1, --no-deps, --offline (the child cargo
    cannot reach a registry), and --locked exactly when the caller pins it --
    the contract the live audit's shim test asserts through this same CLI."""
    tree = _tree(tmp_path, {"plan-free": ""})
    shim_dir, log = _cargo_shim(tmp_path)
    monkeypatch.setenv("PATH", f"{shim_dir}{os.pathsep}{os.environ['PATH']}")
    flags = ("--locked",) if locked else ()
    result = _run_cli(tree, "--eligible", *flags)
    assert result.returncode == 0, result.stdout + result.stderr
    calls = list(log.read_text(encoding="utf-8").splitlines())
    metadata_calls = [call for call in calls if call.startswith("metadata ")]
    assert len(metadata_calls) == 1, calls
    argv = shlex.split(metadata_calls[0])
    assert argv == [
        "metadata",
        "--format-version",
        "1",
        "--no-deps",
        "--offline",
        "--manifest-path",
        str(_manifest(tree)),
        *flags,
    ]


# --- The live release projection (`--release-plan`) -------------------------
#
# The static plan composed with the live audit's conclusive `--classify`
# stream answers every set question the release workflow asks -- live,
# pending, verification exclusions, upload exclusions, and the fully-live
# verdict -- in one projection, so the workflow's shell never re-derives set
# arithmetic. The classification stream is validated against the same static
# plan that produced the audit's membership: every eligible name exactly
# once, live or pending, no unknown names, no missing names, and no static
# cycle.


def _release_cli(tree: Path, classify: str, *args: str) -> subprocess.CompletedProcess[str]:
    """Run the real CLI's `--release-plan` mode over `classify` on stdin."""
    return subprocess.run(  # ruff: ignore[subprocess-without-shell-equals-true]
        [
            sys.executable,
            str(_PLAN),
            "--manifest-path",
            str(_manifest(tree)),
            "--locked",
            "--release-plan",
            *args,
        ],
        input=classify,
        check=False,
        capture_output=True,
        text=True,
    )


def test_release_plan_fresh_workspace(tmp_path: Path) -> None:
    """A fresh release: every resolution member pending, nothing live, nothing
    withheld, nothing excluded, not fully live."""
    publication_plan = _plan_module()
    tree = _tree(tmp_path, {"plan-alpha": "", "plan-beta": ""})
    plan = publication_plan.load_plan(_manifest(tree))
    projection = publication_plan.live_plan_from_classification(
        plan, ["pending plan-alpha", "pending plan-beta"]
    )
    assert projection.to_json() == {
        "live": [],
        "pending": ["plan-alpha", "plan-beta"],
        "withheld": [],
        "verification_excludes": [],
        "publish_excludes": [],
        "fully_live": False,
    }


def test_release_plan_partial_resume(tmp_path: Path) -> None:
    """A resumed release: the live member is excluded from verification and
    upload, the pending member is the verification set, and the withheld
    member is in neither."""
    publication_plan = _plan_module()
    tree = _tree(
        tmp_path,
        {
            "plan-free": "",
            "plan-live": "",
            "plan-site": _deps(_GIT_DEP.format(version=_VERSIONED)),
        },
    )
    plan = publication_plan.load_plan(_manifest(tree))
    projection = publication_plan.live_plan_from_classification(
        plan,
        [
            "live plan-live",
            "pending plan-free",
            "pending plan-site",
        ],
    )
    assert projection.to_json() == {
        "live": ["plan-live"],
        "pending": ["plan-free"],
        "withheld": ["plan-site"],
        "verification_excludes": ["plan-live"],
        "publish_excludes": ["plan-live", "plan-site"],
        "fully_live": False,
    }


def test_release_plan_fully_live(tmp_path: Path) -> None:
    """Every resolution member live: fully live, everything excluded from
    verification and upload, nothing pending to verify or publish."""
    publication_plan = _plan_module()
    tree = _tree(tmp_path, {"plan-a": "", "plan-b": ""})
    plan = publication_plan.load_plan(_manifest(tree))
    projection = publication_plan.live_plan_from_classification(
        plan, ["live plan-a", "live plan-b"]
    )
    assert projection.to_json() == {
        "live": ["plan-a", "plan-b"],
        "pending": [],
        "withheld": [],
        "verification_excludes": ["plan-a", "plan-b"],
        "publish_excludes": ["plan-a", "plan-b"],
        "fully_live": True,
    }


def test_release_plan_withheld_state_is_never_live_or_pending(tmp_path: Path) -> None:
    """A withheld member's index state is not acted on: classified pending or
    live, it lands only in `withheld` -- ineligible for oracle coverage and
    for real upload -- and never contributes to the pending set, so a
    withheld-only release is fully live."""
    publication_plan = _plan_module()
    tree = _tree(
        tmp_path,
        {
            "plan-site1": _deps(_GIT_DEP.format(version=_VERSIONED)),
            "plan-site2": _deps('evm2 = { git = "https://example.invalid/evm2.git" }'),
        },
    )
    plan = publication_plan.load_plan(_manifest(tree))
    assert set(plan.withheld) == {"plan-site1", "plan-site2"}
    pending_withheld = publication_plan.live_plan_from_classification(
        plan, ["pending plan-site1", "live plan-site2"]
    )
    assert pending_withheld.to_json() == {
        "live": [],
        "pending": [],
        "withheld": ["plan-site1", "plan-site2"],
        "verification_excludes": [],
        "publish_excludes": ["plan-site1", "plan-site2"],
        "fully_live": True,
    }


def test_release_plan_allowlisted_resolution_member_stays_pending(tmp_path: Path) -> None:
    """An allowlisted resolution member stays pending: it is never excluded
    from verification, so the verify oracle refuses it loudly before any
    upload instead of the projection covering it."""
    publication_plan = _plan_module()
    tree = _tree(tmp_path, {"plan-io": 'publish = ["crates-io"]\n'})
    plan = publication_plan.load_plan(_manifest(tree))
    projection = publication_plan.live_plan_from_classification(plan, ["pending plan-io"])
    assert projection.to_json() == {
        "live": [],
        "pending": ["plan-io"],
        "withheld": [],
        "verification_excludes": [],
        "publish_excludes": [],
        "fully_live": False,
    }


def test_release_plan_refuses_a_classification_that_does_not_match_the_plan(
    tmp_path: Path,
) -> None:
    """Unknown, duplicate, missing, and invalid-state names all fail loudly,
    naming the defect; nothing is projected from a stream that does not
    conclusively describe the static plan."""
    publication_plan = _plan_module()
    tree = _tree(tmp_path, {"plan-a": "", "plan-b": ""})
    plan = publication_plan.load_plan(_manifest(tree))
    cases = [
        ("unknown member", ["pending plan-a", "pending plan-ghost"], "plan-ghost"),
        ("duplicate member", ["pending plan-a", "live plan-a"], "more than once"),
        ("missing member", ["pending plan-a"], "never classified"),
        ("missing every member", [], "never classified"),
        ("invalid state", ["published plan-a", "pending plan-b"], "invalid"),
        ("state without a member", ["pending", "live plan-b"], "invalid"),
    ]
    for label, lines, expected in cases:
        with pytest.raises(publication_plan.ClassificationError) as exc:
            publication_plan.live_plan_from_classification(plan, lines)
        assert expected in str(exc.value), label


def test_release_plan_refuses_a_static_cycle(tmp_path: Path) -> None:
    """A static release-graph cycle has no upload order, so the projection
    refuses before any classification is acted on, naming the cycle."""
    publication_plan = _plan_module()
    tree = _tree(
        tmp_path,
        {
            "plan-a": _deps('plan-b = { path = "../plan-b", version = "0.1.0" }'),
            "plan-b": _dev_deps('plan-a = { path = "../plan-a", version = "0.1.0" }'),
        },
    )
    plan = publication_plan.load_plan(_manifest(tree))
    assert plan.cycle is not None
    with pytest.raises(publication_plan.PublicationRefusal) as exc:
        publication_plan.live_plan_from_classification(plan, ["pending plan-a", "pending plan-b"])
    assert "publishable dependency cycle" in str(exc.value)
    # the CLI translates the refusal to exit 2, the static-refusal code.
    cycle = _release_cli(tree, "pending plan-a\npending plan-b\n")
    assert cycle.returncode == 2
    assert not cycle.stdout
    assert "publishable dependency cycle" in cycle.stderr


def test_release_plan_json_contract_is_exactly_six_sorted_fields(tmp_path: Path) -> None:
    """The JSON contract: the six fields, lists sorted, `fully_live` a bool --
    the single machine-readable contract the release workflow consumes."""
    publication_plan = _plan_module()
    tree = _tree(tmp_path, {"plan-z": "", "plan-a": ""})
    plan = publication_plan.load_plan(_manifest(tree))
    projection = publication_plan.live_plan_from_classification(
        plan, ["pending plan-z", "live plan-a"]
    )
    document = projection.to_json()
    assert list(document) == [
        "live",
        "pending",
        "withheld",
        "verification_excludes",
        "publish_excludes",
        "fully_live",
    ]
    assert document["live"] == ["plan-a"]
    assert document["pending"] == ["plan-z"]
    assert document["fully_live"] is False


def test_the_release_plan_cli_emits_the_projection(tmp_path: Path) -> None:
    """`--release-plan` reads the classification on stdin and prints the JSON
    projection on stdout; a mismatching stream exits 3 with the defect named
    on stderr and nothing on stdout."""
    tree = _tree(tmp_path, {"plan-a": "", "plan-b": ""})
    ok = _release_cli(tree, "pending plan-a\nlive plan-b\n")
    assert ok.returncode == 0, ok.stdout + ok.stderr
    assert not ok.stderr
    document = json.loads(ok.stdout)
    assert document["live"] == ["plan-b"]
    assert document["pending"] == ["plan-a"]
    assert document["fully_live"] is False

    bad_state = _release_cli(tree, "published plan-a\npending plan-b\n")
    assert bad_state.returncode == 3
    assert not bad_state.stdout
    assert "invalid" in bad_state.stderr
