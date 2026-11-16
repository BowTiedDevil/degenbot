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
"""

from __future__ import annotations

import importlib.util
import os
import shlex
import shutil
import socket
import subprocess  # ruff: ignore[suspicious-subprocess-import]
import sys
from pathlib import Path
from typing import Any

import pytest

_REPO_ROOT = Path(__file__).resolve().parents[1]
_PLAN = _REPO_ROOT / "scripts" / "publication_plan.py"
_GIT_DEP = 'evm2 = {{ git = "https://example.invalid/evm2.git"{version} }}'
_VERSIONED = ', version = "0.1.0"'


def _plan_module() -> Any:
    """The real script, imported for the focused fact and refusal tests."""
    spec = importlib.util.spec_from_file_location("publication_plan", _PLAN)
    module = importlib.util.module_from_spec(spec)
    assert spec.loader is not None
    spec.loader.exec_module(module)
    return module


def _tree(tmp_path: Path, members: dict[str, str]) -> Path:
    """A throwaway workspace whose members carry `members`' manifest extras."""
    root = tmp_path / "tree"
    names = list(members)
    workspace = root / "rust" / "Cargo.toml"
    workspace.parent.mkdir(parents=True)
    quoted = ", ".join(f'"crates/{name}"' for name in names)
    workspace.write_text(f'[workspace]\nmembers = [{quoted}]\nresolver = "2"\n', encoding="utf-8")
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
