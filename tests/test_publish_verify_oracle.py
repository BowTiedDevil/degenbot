"""The publish-verify loopback registry (``scripts/publish_verify.py``).

`cargo publish --dry-run` resolves every member's rewritten manifest against
crates.io. At a fresh lockstep version (ADR-009) the sibling members are not
published yet, so the plain dry run -- verified or ``--no-verify`` alike --
dies on the first member another member requires. This file pins the
Cargo-owned loopback registry that repairs the non-uploading verification
without touching the real upload rounds: every case drives the real script
with ``--root`` pointed at a throwaway workspace, so no assertion can be
satisfied by a re-typed copy of the script's logic.

  * ``test_the_plain_publish_fails_against_a_deterministic_local_registry`` --
    the red shape: cargo itself cannot resolve the unpublished sibling, so the
    oracle must actually work rather than the shape being absent. The probe
    replaces crates.io with a loopback sparse index serving nothing, so the
    default suite never queries the real index nor depends on whether the
    ``pubfix-*`` fixture names are registered on it.
  * ``test_the_oracle_covers_a_fresh_lockstep_workspace_dry_run`` -- the
    ``--no-verify`` PR form passes and names every member covered.
  * ``test_the_oracle_verifies_a_fresh_lockstep_workspace`` -- the verified
    release form passes and builds the packaged form.
  * ``test_a_census_withheld_member_is_withheld_by_name`` -- the
    publish-blocked census stays the owner of genuinely unresolvable shapes:
    a versioned git dep is excluded by name while the rest stay covered.
  * ``test_an_unrecorded_block_fails_the_oracle_loud`` -- a withheld member
    with no census row fails the run before any cargo invocation.
  * ``test_a_fresh_run_verifies_every_pending_member``,
    ``test_a_resumed_run_verifies_only_pending_members_against_live_siblings``,
    and ``test_a_fully_live_run_skips_every_verification_round`` -- the three
    audit shapes release.yml's pre-publish step drives. The RESOLUTION set
    (every member uploaded to the loopback registry) is independent of the
    VERIFICATION set (the members whose verify build the ``--dry-run`` round
    performs): a live member forwarded as cargo's own ``--exclude`` keeps
    resolving its dependents through the loopback index while skipping its
    own verify build, a fresh run verifies every pending member, a fully live
    call runs no verification, and an unknown exclusion fails loud before any
    cargo work (the ``--exclude=<name>`` form parses like the separated one).
  * ``test_the_loopback_entry_is_cargo_generated_and_matches_the_uploaded_bytes``
    -- the loopback index entry is cargo's own upload metadata mapped onto the
    sparse-index spelling: the entry's checksum is the uploaded ``.crate``'s
    checksum, and the entry's dependencies are the rewritten ones the member
    will upload. No handwritten metadata exists to test.
  * ``test_uploads_land_only_on_the_loopback_under_the_dummy_token`` -- the
    registry server's own upload receipts prove every upload hit the loopback
    Publish API endpoint under the run's fixed dummy token, and the run's
    cargo config names no real crates.io endpoint or credential.
  * ``test_a_renamed_optional_dep_keeps_its_manifest_feature_reference`` and
    ``test_a_renamed_library_dependency_verifies_end_to_end`` -- a dependency
    renamed with `package = ...` keeps Cargo's index rename semantics in the
    Cargo-generated entry: the entry carries the alias in `name` and the
    original package name in `package` (the documented index spelling),
    feature references keep the manifest's own `dep:` spellings (cargo
    validates them against the alias), and a member importing a library
    through the alias builds in a verified run even when it is compiled from
    the registry by a dependent member (with a non-renamed control).
  * ``test_a_member_that_fails_to_compile_fails_the_verified_run`` -- the
    verification round actually builds the packaged form: a member whose code
    does not compile fails the run, so a green run is a build fact, not a
    bookkeeping one.
  * ``test_the_loopback_entry_carries_rust_version_and_links_from_the_upload``
    -- the entry carries the Publish payload's ``rust_version`` and ``links``
    when present (real-Cargo upload evidence: the workspace MSRV 1.97 and a
    ``links``-bearing member with a build script, as cargo itself frames them).
  * ``test_a_dirty_staging_directory_does_not_poison_a_rerun`` and
    ``test_clear_cargo_staging_removes_only_the_tmp_dirs`` -- Cargo's own
    ``rust/target/package/tmp-crate`` and ``tmp-registry`` staging from a
    previous round are removed under the run lock before each publish round,
    and nothing else under ``target/package`` (packaged artifacts) is touched.
  * ``test_a_publish_false_member_is_out_of_the_eligible_covered_set``,
    ``test_upload_receipts_must_exactly_match_the_eligible_covered_set``, and
    ``test_a_failed_population_round_prints_no_covered_lines`` -- coverage
    fails closed: ``publish = false`` members are not eligible for the
    covered set, covered lines print only after the population round
    succeeded, and the receipt names must equal the exact eligible set
    (missing and unexpected receipts both fail).
  * ``test_the_loopback_registry_rejects_traversal_outside_its_root`` --
    ``_serve_file``/``do_GET`` reject raw and percent-encoded ``..`` traversal
    outside the overlay root with 403.
  * ``test_cargo_environment_strips_token_overrides`` -- real/default
    (``CARGO_TOKEN``) and named-registry (``CARGO_REGISTRIES_*_TOKEN``,
    ``CARGO_REGISTRY_*``) token environment overrides are stripped from every
    cargo child environment, so the config's fixed dummy token always wins
    (the receipt test runs a real upload under hostile token variables).

The Cargo-fidelity cases pin what the oracle resolves and what cannot
retarget it: the rounds run ``--locked`` (a stale lock fails without a
rewrite), ``lock_externals`` mirrors crates.io pins only and fails loud on
path/git/alternate-registry pins, caller-supplied ``--registry``,
``--index``, ``--token``, and ``--config`` are refused in both argument
forms, and a ``publish`` allowlist naming registries fails loud -- cargo
refuses to carry such a member to any other registry name, and the one
registry this run drives is the loopback.

The network and lifecycle cases pin the mirror's bounded fetches (explicit
timeout, limited retries with backoff, transient failures only), the eager
mirror copying sparse-index metadata only, and the lazy download handler
behind the one ``LoopbackRegistry`` interface: a locked artifact fetched the
first time cargo asks and reused from the persistent lock-hash cache after,
unused and unlocked artifacts never fetched, concurrent requests for one
artifact coalesced onto a single fetch, a failed miss answered loudly on the
response, real ``cargo fetch`` resolving and downloading through the handler,
and the registry's own checksum prevalidation over every byte source --
persistent cache, real crates.io cache, and upstream fetch -- against the
mirrored index entry's sha256 (corrupt or wrong-source bytes are repaired,
ignored, or refused before they are served or cached, and cargo still
verifies independently); downloads-disabled mode refusing every ``/dl``
request without an upstream hit -- plus the ephemeral loopback ports and
per-run overlay directories.

The process-state cases pin the quarantine of everything a run would
otherwise leave in the caller's Cargo home: every cargo invocation executes
with ``CARGO_HOME`` pointed at the run's own directory inside its overlay, so
a sentinel real ``CARGO_HOME`` is byte-for-byte unchanged after a run. The
same-workspace cases pin the interprocess lock that serializes runs sharing
one tree's ``rust/target/package`` staging: two same-tree runs both complete
serialized, a run facing a held lock fails bounded with an error naming the
lock and its recorded owner, and a failed run cleans up only its own overlay
directory.
"""

from __future__ import annotations

import fcntl
import hashlib
import http.client
import http.server
import io
import json
import os
import shutil
import socket
import subprocess
import sys
import tarfile
import threading
import time
import urllib.error
from pathlib import Path
from typing import Any

import pytest

from tests.repo_script import load_script

_REPO_ROOT = Path(__file__).resolve().parents[1]
_ORACLE = _REPO_ROOT / "scripts" / "publish_verify.py"
_CENSUS_PARTS = ("scripts", "publish-blocked-census.txt")

# The withheld shape the census owns and that still resolves as a workspace:
# a publishable member requiring a `publish = false` member by versioned path.
_PRIV_DEP = '[dependencies]\npriv = { path = "../priv", version = "0.1.0" }\n'


def _lockstep_tree(
    tmp_path: Path,
    members: dict[str, str],
    publishable: tuple[str, ...] | None = None,
    publish_tables: dict[str, str] | None = None,
) -> Path:
    """A fresh-lockstep workspace: siblings required by version, unpublished.

    `members` maps crate name to its dependency table; sibling dependencies
    are declared through `[workspace.dependencies]` with an explicit version,
    the ADR-009 shape whose published forms cannot resolve on a fresh release.
    """
    root = tmp_path / "tree"
    names = list(members)
    publishable = names if publishable is None else list(publishable)
    workspace = root / "rust" / "Cargo.toml"
    workspace.parent.mkdir(parents=True)
    quoted = ", ".join(f'"crates/{name}"' for name in names)
    dep_entries = "".join(
        f'{name} = {{ version = "0.1.0", path = "crates/{name}" }}\n' for name in names
    )
    workspace.write_text(
        f"""[workspace]
members = [{quoted}]
resolver = "2"

[workspace.package]
version = "0.1.0"
edition = "2021"

[workspace.dependencies]
{dep_entries}""",
        encoding="utf-8",
    )
    for name, dependency_table in members.items():
        crate = root / "rust" / "crates" / name
        (crate / "src").mkdir(parents=True)
        (crate / "src" / "lib.rs").write_text("pub fn f() {}\n", encoding="utf-8")
        if name in (publish_tables or {}):
            publish = f"\n{(publish_tables or {})[name]}\n"
        else:
            publish = "" if name in publishable else "\npublish = false\n"
        manifest = (
            f'[package]\nname = "{name}"\nversion.workspace = true\n'
            f"edition.workspace = true\n{publish}\n{dependency_table}"
        )
        (crate / "Cargo.toml").write_text(manifest, encoding="utf-8")
    return root


def _census(tree: Path, rows: list[str]) -> None:
    path = tree.joinpath(*_CENSUS_PARTS)
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text("".join(f"{row}\n" for row in rows), encoding="utf-8")


def _oracle(tree: Path, *args: str) -> subprocess.CompletedProcess[str]:
    """Run the real oracle, with cargo's presentation pinned to plain text.

    The assertions read cargo's status lines out of the oracle's streams, so the
    ambient `CARGO_TERM_COLOR` (CI sets it to `always`) must not decide whether
    ``Verifying`` arrives wrapped in escapes.
    """
    return subprocess.run(
        [sys.executable, str(_ORACLE), "--root", str(tree), *args],
        check=False,
        capture_output=True,
        text=True,
        timeout=600,
        env={**os.environ, "CARGO_TERM_COLOR": "never"},
    )


def _loopback_get(port: int, path: str) -> tuple[int, bytes]:
    """One GET against a loopback registry server, driven through `http.client`."""
    conn = http.client.HTTPConnection("127.0.0.1", port, timeout=5)
    try:
        conn.request("GET", path)
        response = conn.getresponse()
        return response.status, response.read()
    finally:
        conn.close()


class _Upstream:
    """A loopback HTTP server over a scratch directory; counts every GET.

    ``fail_remaining`` responses answer with ``failure_status`` before the
    file tree is served (probing the retry policy); ``hang`` accepts the
    connection and never answers (probing the timeout); ``delay`` seconds are
    slept before every answer (widening the race window the coalescing probe
    needs).
    """

    def __init__(
        self,
        directory: Path,
        *,
        fail_remaining: int = 0,
        failure_status: int = 503,
        hang: bool = False,
        delay: float = 0.0,
    ) -> None:
        outer = self

        class Handler(http.server.BaseHTTPRequestHandler):
            def log_message(self, format_: str, *args: object) -> None:
                pass

            def do_GET(self) -> None:
                outer.hits.append(self.path)
                if hang:
                    time.sleep(30)
                    return
                if delay:
                    time.sleep(delay)
                if outer.fail_remaining > 0:
                    outer.fail_remaining -= 1
                    self.send_error(failure_status)
                    return
                target = directory.joinpath(*self.path.lstrip("/").split("/"))
                if not target.is_file():
                    self.send_error(404)
                    return
                body = target.read_bytes()
                self.send_response(200)
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)

        self.hits: list[str] = []
        self.fail_remaining = fail_remaining
        self.server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self.server.daemon_threads = True
        threading.Thread(target=self.server.serve_forever, daemon=True).start()

    @property
    def url(self) -> str:
        return f"http://127.0.0.1:{self.server.server_address[1]}"

    def stop(self) -> None:
        self.server.shutdown()
        self.server.server_close()


def test_the_plain_publish_fails_against_a_deterministic_local_registry(
    tmp_path: Path,
) -> None:
    """The red shape, resolved against a controlled registry, not crates.io.

    cargo itself cannot resolve the unpublished sibling, so the oracle must
    actually work rather than the shape being absent. The workspace replaces
    the crates.io source with a loopback sparse index that serves nothing, so
    the failure is deterministic and offline: the default suite neither
    queries the real index nor depends on the ``pubfix-*`` names staying
    unregistered on it.
    """
    tree = _lockstep_tree(
        tmp_path,
        {
            "pubfix-leaf": "",
            "pubfix-mid": "[dependencies]\npubfix-leaf.workspace = true\n",
        },
    )
    upstream = _Upstream(tmp_path / "empty-index")
    try:
        config = tree / "rust" / ".cargo" / "config.toml"
        config.parent.mkdir(parents=True, exist_ok=True)
        config.write_text(
            '[source.crates-io]\nreplace-with = "probe-registry"\n\n'
            "[source.probe-registry]\n"
            f'registry = "sparse+{upstream.url}/index/"\n',
            encoding="utf-8",
        )
        result = subprocess.run(
            [
                "cargo",
                "publish",
                "--registry",
                "crates-io",
                "--dry-run",
                "--no-verify",
                "--allow-dirty",
                "-p",
                "pubfix-mid",
            ],
            cwd=tree / "rust",
            check=False,
            capture_output=True,
            text=True,
            timeout=300,
        )
    finally:
        upstream.stop()
    assert result.returncode != 0
    assert "no matching package named `pubfix-leaf` found" in result.stderr
    assert "replacing registry `crates-io`" in result.stderr


def test_the_oracle_covers_a_fresh_lockstep_workspace_dry_run(tmp_path: Path) -> None:
    tree = _lockstep_tree(
        tmp_path,
        {
            "pubfix-leaf": "",
            "pubfix-mid": "[dependencies]\npubfix-leaf.workspace = true\n",
            "pubfix-top": "[dependencies]\npubfix-mid.workspace = true\n",
        },
    )
    _census(tree, [])
    result = _oracle(tree, "--no-verify")
    assert result.returncode == 0, result.stdout + result.stderr
    for name in ("pubfix-leaf", "pubfix-mid", "pubfix-top"):
        assert f"publish-verify covered {name}" in result.stdout
    assert "aborting upload due to dry run" not in result.stdout
    # the run cleaned up its own overlay directory
    assert list((tree / "rust" / "target" / "publish-verify-overlay").iterdir()) == []


def test_the_oracle_verifies_a_fresh_lockstep_workspace(tmp_path: Path) -> None:
    tree = _lockstep_tree(
        tmp_path,
        {
            "pubfix-leaf": "",
            "pubfix-mid": "[dependencies]\npubfix-leaf.workspace = true\n",
            "pubfix-top": "[dependencies]\npubfix-mid.workspace = true\n",
        },
    )
    _census(tree, [])
    result = _oracle(tree)
    assert result.returncode == 0, result.stdout + result.stderr
    # Cargo's status lines (Verifying/Compiling/Uploading) stream on stderr.
    assert "Verifying" in result.stderr
    assert "aborting upload due to dry run" in result.stderr


def test_a_fresh_run_verifies_every_pending_member(tmp_path: Path) -> None:
    """Fresh tag shape: nothing is live, so every pending member is verified.

    release.yml's pre-publish step forwards no ``--exclude`` when the audit
    reports no live members; the run verifies all of them.
    """
    tree = _lockstep_tree(
        tmp_path,
        {
            "pubfix-leaf": "",
            "pubfix-mid": "[dependencies]\npubfix-leaf.workspace = true\n",
            "pubfix-top": "[dependencies]\npubfix-mid.workspace = true\n",
        },
    )
    _census(tree, [])
    result = _oracle(tree)
    assert result.returncode == 0, result.stdout + result.stderr
    verified = sorted(
        line.split()[1]
        for line in result.stderr.splitlines()
        if line.strip().startswith("Verifying pubfix-")
    )
    assert verified == ["pubfix-leaf", "pubfix-mid", "pubfix-top"]


def test_a_resumed_run_verifies_only_pending_members_against_live_siblings(
    tmp_path: Path,
) -> None:
    """Resumed tag shape: live siblings resolve, only pending members verify.

    The release step forwards the audit's live members as cargo's own
    ``--exclude`` spelling. The run keeps the excluded member in the
    resolution set -- it is still packed and indexed, so the pending member's
    rewritten manifest (which requires it at the lockstep version crates.io
    actually holds live) resolves through the overlay -- but its own verify
    round is skipped: an already-live member cannot be re-uploaded, and
    re-verifying it is exactly what makes a resumed re-run expensive. The run
    passing while pubfix-mid (whose published manifest requires pubfix-leaf)
    is the proof that resolution against a live sibling survives exclusion.
    """
    tree = _lockstep_tree(
        tmp_path,
        {
            "pubfix-leaf": "",
            "pubfix-mid": "[dependencies]\npubfix-leaf.workspace = true\n",
            "pubfix-top": "[dependencies]\npubfix-mid.workspace = true\n",
        },
    )
    _census(tree, [])
    result = _oracle(tree, "--exclude", "pubfix-leaf")
    assert result.returncode == 0, result.stdout + result.stderr
    # resolution set: the live sibling is still packed and indexed
    for name in ("pubfix-leaf", "pubfix-mid", "pubfix-top"):
        assert f"publish-verify covered {name}" in result.stdout
    assert "publish-verify resolution-only pubfix-leaf v0.1.0" in result.stdout
    # verification set: only the pending members get a publish round
    assert "Verifying pubfix-leaf" not in result.stderr
    assert "Verifying pubfix-mid" in result.stderr
    assert "Verifying pubfix-top" in result.stderr
    assert result.stderr.count("aborting upload due to dry run") == 2


def test_a_fully_live_run_skips_every_verification_round(tmp_path: Path) -> None:
    """Fully-live tag shape: nothing verifies; nothing uploads; the run passes.

    Every publishable member is already live at this version, so the release
    step normally exits before invoking the oracle at all; this pins the
    oracle's own contract should it still see an all-excluded call: all
    members stay in the resolution set (packed and indexed) and no
    verification round runs.
    """
    tree = _lockstep_tree(
        tmp_path,
        {
            "pubfix-leaf": "",
            "pubfix-mid": "[dependencies]\npubfix-leaf.workspace = true\n",
            "pubfix-top": "[dependencies]\npubfix-mid.workspace = true\n",
        },
    )
    _census(tree, [])
    result = _oracle(
        tree,
        "--exclude",
        "pubfix-leaf",
        "--exclude",
        "pubfix-mid",
        "--exclude",
        "pubfix-top",
    )
    assert result.returncode == 0, result.stdout + result.stderr
    for name in ("pubfix-leaf", "pubfix-mid", "pubfix-top"):
        assert f"publish-verify covered {name}" in result.stdout
        assert f"publish-verify resolution-only {name} v0.1.0" in result.stdout
    assert "Verifying" not in result.stderr
    assert "aborting upload due to dry run" not in result.stderr


def test_an_unknown_exclusion_fails_loud_before_packing(tmp_path: Path) -> None:
    """``--exclude`` of a name outside the workspace fails before any packing.

    Mirrors cargo's own package-selection behavior: an exclusion that matches
    no member would silently verify-or-skip the wrong set, so the run names
    the offender and dies before the first ``cargo package``.
    """
    tree = _lockstep_tree(
        tmp_path,
        {
            "pubfix-leaf": "",
            "pubfix-mid": "[dependencies]\npubfix-leaf.workspace = true\n",
        },
    )
    _census(tree, [])
    result = _oracle(tree, "--no-verify", "--exclude", "pubfix-ghost")
    assert result.returncode != 0
    output = result.stdout + result.stderr
    assert "pubfix-ghost" in output
    # refused before any cargo packaging, not after a partial run
    assert "publish-verify covered" not in output


def test_an_exclusion_accepts_the_equals_form(tmp_path: Path) -> None:
    """``--exclude=<name>`` parses like the separated form (and like the
    retargeting-flag refusal does for its own spellings)."""
    tree = _lockstep_tree(
        tmp_path,
        {
            "pubfix-leaf": "",
            "pubfix-mid": "[dependencies]\npubfix-leaf.workspace = true\n",
        },
    )
    _census(tree, [])
    result = _oracle(tree, "--no-verify", "--exclude=pubfix-leaf")
    assert result.returncode == 0, result.stdout + result.stderr
    assert "publish-verify resolution-only pubfix-leaf v0.1.0" in result.stdout
    assert "publish-verify resolution-only pubfix-mid v0.1.0" not in result.stdout


def test_a_stale_lock_fails_the_pack_without_rewriting_it(tmp_path: Path) -> None:
    """Packing asserts the lockfile it resolves through, as the real upload does.

    A real `cargo publish --locked` refuses to run against a lockfile the
    manifests have outgrown, and it never rewrites one. Packing is the same
    resolution act, so it must refuse the same way: a stale lock fails the run
    loudly instead of being silently refreshed into a resolution the release
    would never see.
    """
    tree = _lockstep_tree(
        tmp_path,
        {
            "pubfix-leaf": "",
            "pubfix-mid": "[dependencies]\npubfix-leaf.workspace = true\n",
        },
    )
    generated = subprocess.run(
        ["cargo", "metadata", "--format-version", "1"],
        cwd=tree / "rust",
        check=False,
        capture_output=True,
        text=True,
        timeout=300,
    )
    assert generated.returncode == 0, generated.stderr
    lock = tree / "rust" / "Cargo.lock"
    stale = lock.read_text(encoding="utf-8").replace(
        'name = "pubfix-leaf"\nversion = "0.1.0"', 'name = "pubfix-leaf"\nversion = "9.9.9"'
    )
    assert stale != lock.read_text(encoding="utf-8")
    lock.write_text(stale, encoding="utf-8")
    _census(tree, [])

    result = _oracle(tree, "--no-verify")

    assert result.returncode != 0
    assert "because --locked was passed" in result.stderr
    # `--locked` refuses to run without rewriting the lockfile it refused.
    assert lock.read_text(encoding="utf-8") == stale


def test_a_retained_dev_cycle_fails_the_oracle_before_packaging(
    tmp_path: Path,
) -> None:
    """A dev-dependency edge closing a normal cycle is a release-graph defect.

    Every retained requirement -- normal or dev -- is an upload-ordering
    constraint, so `db -> pools -> uniswap -> db` (the real workspace's
    shape, with the dev edge retained by its explicit version) leaves
    `cargo publish --workspace` no upload order. The census gate rejects the
    cycle outright with the cycle named before this oracle packs anything,
    so no member is covered and no census exclusion may hide the defect.
    """
    tree = _lockstep_tree(
        tmp_path,
        {
            "pubfix-db": (
                "[dependencies]\npubfix-math.workspace = true\n\n"
                "[dev-dependencies]\npubfix-pools.workspace = true\n"
            ),
            "pubfix-math": "",
            "pubfix-pools": "[dependencies]\npubfix-uniswap.workspace = true\n",
            "pubfix-uniswap": "[dependencies]\npubfix-db.workspace = true\n",
        },
    )
    _census(tree, [])
    result = _oracle(tree, "--no-verify")
    assert result.returncode == 1
    assert "publishable dependency cycle" in result.stdout
    assert "pubfix-db -> pubfix-pools -> pubfix-uniswap -> pubfix-db" in result.stdout
    # refused before any packaging: no member was covered
    assert "publish-verify covered" not in result.stdout


def test_a_renamed_optional_dep_keeps_its_manifest_feature_reference(
    tmp_path: Path,
) -> None:
    """Feature references keep the manifest's spelling, which names the alias.

    `alias = { package = "real", ... }` indexes the dependency under the alias
    name with the real package in `package` (Cargo's index convention), and
    cargo validates the member's own `dep:alias` feature against that alias --
    an entry that rewrites the reference to the real package name is rejected
    as an invalid index entry, and the run dies resolving the first member
    that requires this one.
    """
    tree = _lockstep_tree(
        tmp_path,
        {
            "pubfix-real": "",
            "pubfix-renamer": (
                "[dependencies]\n"
                'alias = { package = "pubfix-real", version = "0.1.0", '
                'optional = true, path = "../pubfix-real" }\n\n'
                '[features]\nrealfeat = ["dep:alias"]\n'
            ),
        },
    )
    _census(tree, [])
    result = _oracle(tree, "--no-verify", "--keep-overlay")
    assert result.returncode == 0, result.stdout + result.stderr
    overlay = _only_overlay_run(tree)
    entry = json.loads(
        (overlay / "index" / "pu" / "bf" / "pubfix-renamer").read_text(encoding="utf-8")
    )
    assert [(dep["name"], dep["package"]) for dep in entry["deps"]] == [("alias", "pubfix-real")]
    assert entry["features2"]["realfeat"] == ["dep:alias"]


def test_a_census_withheld_member_is_withheld_by_name(tmp_path: Path) -> None:
    tree = _lockstep_tree(
        tmp_path,
        {
            "pubfix-leaf": "",
            "pubfix-mid": "[dependencies]\npubfix-leaf.workspace = true\n",
            "pubfix-blocked": _PRIV_DEP,
            "priv": "",
        },
        publishable=("pubfix-leaf", "pubfix-mid", "pubfix-blocked"),
    )
    _census(tree, ["pubfix-blocked\tdepends on non-publishable priv"])
    result = _oracle(tree, "--no-verify")
    assert result.returncode == 0, result.stdout + result.stderr
    assert "publish-blocked pubfix-blocked:" in result.stdout
    assert "publish-verify covered pubfix-blocked" not in result.stdout
    for name in ("pubfix-leaf", "pubfix-mid"):
        assert f"publish-verify covered {name}" in result.stdout


def test_an_allowlisted_member_the_census_withholds_keeps_its_withheld_coverage(
    tmp_path: Path,
) -> None:
    """Census withholding outranks the allowlist refusal.

    A member that is both allowlisted and census-withheld has no upload path
    by the census's own verdict already, so it never reaches the allowlist
    refusal: the run keeps its withheld package-list coverage and certifies
    the rest of the resolution set.
    """
    tree = _lockstep_tree(
        tmp_path,
        {
            "pubfix-leaf": "",
            "pubfix-blocked": _PRIV_DEP,
            "priv": "",
        },
        publishable=("pubfix-leaf", "pubfix-blocked"),
        publish_tables={"pubfix-blocked": 'publish = ["crates-io"]'},
    )
    _census(tree, ["pubfix-blocked\tdepends on non-publishable priv"])

    result = _oracle(tree, "--no-verify")

    assert result.returncode == 0, result.stdout + result.stderr
    output = result.stdout + result.stderr
    assert "publish-blocked pubfix-blocked:" in output
    assert "publishes only to" not in output
    assert "publish-verify covered pubfix-blocked" not in output


def test_an_unrecorded_block_fails_the_oracle_loud(tmp_path: Path) -> None:
    tree = _lockstep_tree(
        tmp_path,
        {
            "pubfix-leaf": "",
            "pubfix-mid": "[dependencies]\npubfix-leaf.workspace = true\n",
            "pubfix-blocked": _PRIV_DEP,
            "priv": "",
        },
        publishable=("pubfix-leaf", "pubfix-mid", "pubfix-blocked"),
    )
    _census(tree, [])
    result = _oracle(tree, "--no-verify")
    assert result.returncode == 1
    assert "no census row" in result.stdout


def test_a_non_crates_io_publish_allowlist_fails_loud(tmp_path: Path) -> None:
    """An allowlist naming another registry cannot reach the loopback registry.

    Cargo refuses to publish a member to any registry its `publish` allowlist
    does not name, and the one registry this run drives is the loopback -- so
    an allowlisted member has no upload path the oracle can drive and fails
    loud with both names, before any round runs.
    """
    tree = _lockstep_tree(
        tmp_path,
        {"pubfix-leaf": ""},
        publish_tables={"pubfix-leaf": 'publish = ["private-registry"]'},
    )
    _census(tree, [])

    result = _oracle(tree, "--no-verify")

    assert result.returncode == 1
    output = result.stdout + result.stderr
    assert "pubfix-leaf" in output
    assert "private-registry" in output
    assert "publish-verify covered pubfix-leaf" not in output


def test_a_crates_io_publish_allowlist_fails_loud(tmp_path: Path) -> None:
    """Even a crates.io-named allowlist cannot be driven to the loopback.

    `cargo publish --registry <name>` refuses a member whose allowlist names
    only `crates-io` (measured: "the registry `pubfix` is not listed in the
    `package.publish` value"), and `--registry crates-io` hardcodes the real
    crates.io upload API, which the run must never touch. The oracle refuses
    up front instead of failing mid-round on cargo's own refusal.
    """
    tree = _lockstep_tree(
        tmp_path,
        {"pubfix-leaf": ""},
        publish_tables={"pubfix-leaf": 'publish = ["crates-io"]'},
    )
    _census(tree, [])

    result = _oracle(tree, "--no-verify")

    assert result.returncode != 0
    output = result.stdout + result.stderr
    assert "pubfix-leaf" in output
    assert "crates-io" in output
    assert "publish-verify covered pubfix-leaf" not in output


def test_the_oracle_rejects_caller_registry_targeting(tmp_path: Path) -> None:
    """The run owns the registry targeting; the caller cannot retarget it.

    `--registry`, `--index`, `--token`, and `--config` would redirect the very
    resolution the oracle certifies (a different registry, a foreign index, a
    token-bearing run, an overridden source replacement), in both the separated
    and the `=` form. Everything else keeps forwarding to the publish rounds.
    """
    tree = _lockstep_tree(tmp_path, {"pubfix-leaf": ""})
    _census(tree, [])
    for forwarded in (
        ("--registry", "evil"),
        ("--registry=evil",),
        ("--index", "sparse+https://evil.example.com/index/"),
        ("--index=sparse+https://evil.example.com/index/",),
        ("--token", "hunter2"),
        ("--token=hunter2",),
        ("--config", 'source.crates-io.replace-with="evil"'),
        ('--config=source.crates-io.replace-with="evil"',),
    ):
        result = _oracle(tree, "--no-verify", *forwarded)

        assert result.returncode == 1, forwarded
        output = result.stdout + result.stderr
        assert forwarded[0].split("=")[0] in output, forwarded
        # Refused before any cargo work, not after a partial run.
        assert "publish-blocked census reconciled" not in result.stdout, forwarded


def _only_overlay_run(tree: Path) -> Path:
    """The single overlay directory a run kept behind ``--keep-overlay``."""
    runs = list((tree / "rust" / "target" / "publish-verify-overlay").iterdir())
    assert len(runs) == 1, runs
    return runs[0]


def test_the_index_fetch_retries_transient_failures_then_succeeds(tmp_path: Path) -> None:
    """A 5xx is transient: bounded retries with backoff, then the body."""
    module = _oracle_module()
    served = tmp_path / "served" / "pu" / "bf"
    served.mkdir(parents=True)
    body = b'{"name": "pubfix-leaf"}\n'
    (served / "pubfix-leaf").write_bytes(body)
    upstream = _Upstream(tmp_path / "served", fail_remaining=2, failure_status=503)
    try:
        raw = module.fetch_bytes(
            f"{upstream.url}/pu/bf/pubfix-leaf", timeout=5.0, attempts=3, backoff=0.05
        )
    finally:
        upstream.stop()
    assert raw == body
    assert upstream.hits == ["/pu/bf/pubfix-leaf"] * 3


def test_the_index_fetch_fails_fast_on_a_definitive_answer(tmp_path: Path) -> None:
    """A 4xx says what the registry serves; retrying it would only stall."""
    module = _oracle_module()
    (tmp_path / "empty").mkdir()
    upstream = _Upstream(tmp_path / "empty")
    try:
        with pytest.raises(urllib.error.HTTPError):
            module.fetch_bytes(
                f"{upstream.url}/pu/bf/pubfix-leaf", timeout=5.0, attempts=3, backoff=0.05
            )
    finally:
        upstream.stop()
    assert len(upstream.hits) == 1


def test_the_index_fetch_is_bounded_and_names_the_url(tmp_path: Path) -> None:
    """A dead endpoint fails inside its own budget, naming the URL and attempts."""
    module = _oracle_module()
    upstream = _Upstream(tmp_path / "hanging", hang=True)
    try:
        url = f"{upstream.url}/pu/bf/pubfix-leaf"
        start = time.monotonic()
        with pytest.raises(RuntimeError) as raised:
            module.fetch_bytes(url, timeout=0.3, attempts=2, backoff=0.05)
        elapsed = time.monotonic() - start
    finally:
        upstream.stop()
    message = str(raised.value)
    assert url in message
    assert "after 2 attempts" in message
    assert len(upstream.hits) == 2
    assert elapsed < 10


def test_a_dead_registry_fails_the_mirror_with_a_useful_error(tmp_path: Path) -> None:
    module = _oracle_module()
    tree = _lockstep_tree(tmp_path, {"pubfix-leaf": ""})
    upstream = _Upstream(tmp_path / "hanging", hang=True)
    try:
        message = module.mirror_index(
            "pubfix-leaf",
            {"0.1.0"},
            "deadbeef",
            tree / "rust" / "target" / "publish-verify-mirror",
            tree / "rust" / "target" / "publish-verify-overlay" / "run-x",
            index_base=upstream.url,
            timeout=0.3,
            attempts=2,
            backoff=0.05,
        )
    finally:
        upstream.stop()
    assert "pubfix-leaf" in message
    assert "after 2 attempts" in message
    assert "/pu/bf/pubfix-leaf" in message


def test_the_mirror_is_index_only_and_never_downloads(tmp_path: Path) -> None:
    """The eager mirror copies sparse-index metadata only; `.crate` bytes are lazy.

    Resolution needs the index entries before any cargo round, and that is all
    the mirror fetches: the bytes are served by the registry's download
    handler the first time cargo requests them. A `--no-verify` run -- which
    resolves from the index alone -- therefore touches the network only for
    metadata and downloads no artifacts (pinned experimentally: a
    ``cargo publish/package --no-verify`` round over an index-only loopback
    makes no ``/dl`` request, while the verified form dies on the missing
    download -- which is now fetched on demand instead of pre-materialized).
    """
    module = _oracle_module()
    tree = _lockstep_tree(tmp_path, {"pubfix-leaf": ""})
    served = tmp_path / "served"
    entry = served / "pu" / "bf" / "pubfix-leaf"
    entry.parent.mkdir(parents=True)
    entry.write_text(json.dumps({"name": "pubfix-leaf", "vers": "0.1.0"}) + "\n", encoding="utf-8")
    upstream = _Upstream(served)
    try:
        message = module.mirror_index(
            "pubfix-leaf",
            {"0.1.0"},
            "deadbeef",
            tree / "rust" / "target" / "publish-verify-mirror",
            tree / "rust" / "target" / "publish-verify-overlay" / "run-x",
            index_base=upstream.url,
        )
        hits = list(upstream.hits)
    finally:
        upstream.stop()
    assert not message
    assert hits == ["/pu/bf/pubfix-leaf"]
    overlay = tree / "rust" / "target" / "publish-verify-overlay" / "run-x"
    assert (overlay / "index" / "pu" / "bf" / "pubfix-leaf").is_file()
    assert not (overlay / "dl").exists()


def _lazy_registry(
    tmp_path: Path,
    *,
    externals: dict[str, set[str]],
    dl_base: str | None = None,
    timeout: float = 5.0,
    attempts: int = 2,
    backoff: float = 0.05,
    downloads_enabled: bool = True,
    caller_cache: Path | None = None,
    cksum: str | None = None,
) -> tuple[Any, Path, Path]:
    """A started lazy-download registry over a fresh overlay.

    The caller cache is injected (default: a nonexistent scratch path, so no
    ambient ``CARGO_HOME`` leaks in); when `cksum` is given, the overlay index
    carries entries naming it for every external, as the production mirror
    would. Returns the registry, its overlay directory, and its persistent
    lock-hash download cache.
    """
    module = _oracle_module()
    overlay = tmp_path / "overlay"
    if cksum is not None:
        for name, versions in sorted(externals.items()):
            index = overlay / "index" / module.index_rel(name)
            index.parent.mkdir(parents=True, exist_ok=True)
            for version in sorted(versions):
                index.write_text(
                    json.dumps({"name": name, "vers": version, "cksum": cksum}) + "\n",
                    encoding="utf-8",
                )
    dl_cache = tmp_path / "dl-cache"
    registry = module.LoopbackRegistry(
        overlay,
        token=module.DUMMY_TOKEN,
        externals=externals,
        dl_cache=dl_cache,
        dl_base=dl_base or module.CRATES_IO_DL,
        timeout=timeout,
        attempts=attempts,
        backoff=backoff,
        downloads_enabled=downloads_enabled,
        caller_cache=caller_cache or tmp_path / "caller-cache",
    )
    registry.start()
    return registry, overlay, dl_cache


def _registry_port(registry: Any) -> int:
    """The bound port, read back through the interface the run itself uses."""
    return int(registry.index_url.removeprefix("sparse+http://127.0.0.1:").removesuffix("/index/"))


def test_a_locked_artifact_is_fetched_once_and_reused(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """The download handler fetches a locked artifact on demand, exactly once.

    The first `/dl` request triggers the bounded fetch into the persistent
    lock-hash cache and serves the exact fetched bytes; the second request is
    answered from that cache without touching the upstream again.
    """
    monkeypatch.setenv("HOME", str(tmp_path / "home"))
    served = tmp_path / "served"
    payload = b"the exact bytes crates.io would serve"
    crate = served / "dl" / "pubfix-leaf" / "0.1.0" / "download"
    crate.parent.mkdir(parents=True)
    crate.write_bytes(payload)
    upstream = _Upstream(served)
    registry, _, dl_cache = _lazy_registry(
        tmp_path / "run",
        externals={"pubfix-leaf": {"0.1.0"}},
        dl_base=f"{upstream.url}/dl",
        cksum=hashlib.sha256(payload).hexdigest(),
    )
    try:
        port = _registry_port(registry)
        status, body = _loopback_get(port, "/dl/pubfix-leaf/0.1.0/download")
        again, reused = _loopback_get(port, "/dl/pubfix-leaf/0.1.0/download")
    finally:
        registry.close()
        upstream.stop()
    assert status == 200
    assert body == payload
    assert again == 200
    assert reused == payload
    assert [hit for hit in upstream.hits if hit.startswith("/dl/")] == [
        "/dl/pubfix-leaf/0.1.0/download"
    ]
    assert (dl_cache / "pubfix-leaf-0.1.0.crate").read_bytes() == payload


def test_unused_and_unlocked_artifacts_are_never_fetched(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """Only a locked, requested artifact is ever fetched; nothing else is.

    The handler fetches nothing until cargo asks; a request naming an
    artifact outside the locked external set is refused without a single
    upstream hit, and a locked-but-unrequested artifact stays unfetched --
    the run's network touch is exactly the artifacts the build graph needs.
    """
    monkeypatch.setenv("HOME", str(tmp_path / "home"))
    served = tmp_path / "served"
    payload = b"locked bytes"
    crate = served / "dl" / "pubfix-leaf" / "0.1.0" / "download"
    crate.parent.mkdir(parents=True)
    crate.write_bytes(payload)
    stranger = served / "dl" / "stranger" / "9.9.9" / "download"
    stranger.parent.mkdir(parents=True)
    stranger.write_bytes(b"stranger bytes")
    upstream = _Upstream(served)
    registry, _, _ = _lazy_registry(
        tmp_path / "run",
        externals={"pubfix-leaf": {"0.1.0"}},
        dl_base=f"{upstream.url}/dl",
        cksum=hashlib.sha256(payload).hexdigest(),
    )
    try:
        port = _registry_port(registry)
        assert upstream.hits == []  # nothing is fetched before cargo asks
        refused, _ = _loopback_get(port, "/dl/stranger/9.9.9/download")
        status, body = _loopback_get(port, "/dl/pubfix-leaf/0.1.0/download")
    finally:
        registry.close()
        upstream.stop()
    assert refused == 404
    assert status == 200
    assert body == payload
    assert not any("stranger" in hit for hit in upstream.hits)


def test_concurrent_requests_for_one_artifact_coalesce(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """Simultaneous downloads of one artifact produce exactly one fetch.

    The upstream delays every answer, so all eight requests overlap inside
    the handler; coalescing turns them into one upstream fetch whose result
    every requester shares.
    """
    monkeypatch.setenv("HOME", str(tmp_path / "home"))
    served = tmp_path / "served"
    payload = b"shared bytes"
    crate = served / "dl" / "pubfix-leaf" / "0.1.0" / "download"
    crate.parent.mkdir(parents=True)
    crate.write_bytes(payload)
    upstream = _Upstream(served, delay=0.3)
    registry, _, _ = _lazy_registry(
        tmp_path / "run",
        externals={"pubfix-leaf": {"0.1.0"}},
        dl_base=f"{upstream.url}/dl",
        cksum=hashlib.sha256(payload).hexdigest(),
    )
    results: list[tuple[int, bytes]] = []
    results_lock = threading.Lock()

    def request() -> None:
        result = _loopback_get(_registry_port(registry), "/dl/pubfix-leaf/0.1.0/download")
        with results_lock:
            results.append(result)

    try:
        threads = [threading.Thread(target=request) for _ in range(8)]
        for thread in threads:
            thread.start()
        for thread in threads:
            thread.join()
    finally:
        registry.close()
        upstream.stop()
    assert len(results) == 8
    assert all(status == 200 and body == payload for status, body in results)
    assert [hit for hit in upstream.hits if hit.startswith("/dl/")] == [
        "/dl/pubfix-leaf/0.1.0/download"
    ]


def test_a_failed_download_fails_the_request_loudly(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """A miss the upstream cannot serve answers 500, naming the crate and URL.

    The download handler reuses the bounded fetch policy: a dead upstream
    exhausts the retries inside its own budget and the failure surfaces on
    the response -- never a hang, never a silent empty body, and no cached
    partial bytes.
    """
    monkeypatch.setenv("HOME", str(tmp_path / "home"))
    upstream = _Upstream(tmp_path / "hanging", hang=True)
    registry, _, dl_cache = _lazy_registry(
        tmp_path / "run",
        externals={"pubfix-leaf": {"0.1.0"}},
        dl_base=f"{upstream.url}/dl",
        timeout=0.3,
        cksum=hashlib.sha256(b"never served").hexdigest(),
    )
    try:
        start = time.monotonic()
        status, body = _loopback_get(_registry_port(registry), "/dl/pubfix-leaf/0.1.0/download")
        elapsed = time.monotonic() - start
    finally:
        registry.close()
        upstream.stop()
    assert status == 500
    assert b"pubfix-leaf" in body
    assert b"after 2 attempts" in body
    assert elapsed < 10
    assert not (dl_cache / "pubfix-leaf-0.1.0.crate").exists()


def _crate_bytes(name: str, version: str) -> bytes:
    """A minimal valid `.crate` archive: a gzipped tar with a package root."""
    buffer = io.BytesIO()
    with tarfile.open(fileobj=buffer, mode="w:gz") as archive:
        manifest = (
            f'[package]\nname = "{name}"\nversion = "{version}"\nedition = "2021"\n'
        ).encode()
        library = b"pub fn f() {}\n"
        for member, payload in (
            (f"{name}-{version}/Cargo.toml", manifest),
            (f"{name}-{version}/src/lib.rs", library),
        ):
            info = tarfile.TarInfo(member)
            info.size = len(payload)
            archive.addfile(info, io.BytesIO(payload))
    return buffer.getvalue()


def _external_dependency_tree(tmp_path: Path) -> Path:
    """A workspace member requiring an external crate, resolved via the loopback.

    The tree's ``.cargo/config.toml`` -- written by the caller once the
    registry's port is known -- replaces crates.io with the loopback registry,
    so resolution and download never touch the real crates.io.
    """
    tree = tmp_path / "tree"
    rust = tree / "rust"
    member = rust / "crates" / "pubfix-user"
    (member / "src").mkdir(parents=True)
    (member / "src" / "lib.rs").write_text("pub fn f() {}\n", encoding="utf-8")
    (rust / "Cargo.toml").write_text(
        '[workspace]\nmembers = ["crates/pubfix-user"]\nresolver = "2"\n\n'
        '[workspace.package]\nversion = "0.1.0"\nedition = "2021"\n',
        encoding="utf-8",
    )
    (member / "Cargo.toml").write_text(
        '[package]\nname = "pubfix-user"\nversion.workspace = true\n'
        'edition.workspace = true\n\n[dependencies]\npubfix-ext = "0.1.0"\n',
        encoding="utf-8",
    )
    return tree


def _start_external_registry(tmp_path: Path, index_cksum: str, upstream_url: str) -> Any:
    """A started registry whose overlay indexes pubfix-ext 0.1.0 at `cksum`."""
    module = _oracle_module()
    overlay = tmp_path / "overlay"
    entry = {
        "name": "pubfix-ext",
        "vers": "0.1.0",
        "deps": [],
        "cksum": index_cksum,
        "features": {},
        "yanked": False,
    }
    index = overlay / "index" / "pu" / "bf" / "pubfix-ext"
    index.parent.mkdir(parents=True)
    index.write_text(json.dumps(entry) + "\n", encoding="utf-8")
    registry = module.LoopbackRegistry(
        overlay,
        token=module.DUMMY_TOKEN,
        externals={"pubfix-ext": {"0.1.0"}},
        dl_cache=tmp_path / "dl-cache",
        dl_base=f"{upstream_url}/dl",
        timeout=5.0,
        attempts=2,
        backoff=0.05,
        caller_cache=tmp_path / "caller-cache",
    )
    registry.start()
    return registry


def _cargo_fetch(tree: Path, index_url: str, cargo_home: Path) -> subprocess.CompletedProcess[str]:
    """Resolve and download the external dependency through the loopback."""
    config = tree / "rust" / ".cargo" / "config.toml"
    config.parent.mkdir(parents=True, exist_ok=True)
    config.write_text(
        '[source.crates-io]\nreplace-with = "probe-registry"\n\n'
        "[source.probe-registry]\n"
        f'registry = "{index_url}"\n',
        encoding="utf-8",
    )
    return subprocess.run(
        ["cargo", "fetch"],
        cwd=tree / "rust",
        check=False,
        capture_output=True,
        text=True,
        timeout=300,
        env={**os.environ, "CARGO_HOME": str(cargo_home)},
    )


def test_a_resolved_external_builds_through_the_lazy_handler(tmp_path: Path) -> None:
    """The lazy download serves real cargo resolution, end to end, once.

    cargo resolves pubfix-ext through the loopback index and downloads its
    artifact through the lazy handler; the upstream is hit exactly once and
    the served bytes verify against the index checksum.
    """
    good = _crate_bytes("pubfix-ext", "0.1.0")
    served = tmp_path / "served"
    crate = served / "dl" / "pubfix-ext" / "0.1.0" / "download"
    crate.parent.mkdir(parents=True)
    crate.write_bytes(good)
    upstream = _Upstream(served)
    tree = _external_dependency_tree(tmp_path)
    registry = _start_external_registry(tmp_path, hashlib.sha256(good).hexdigest(), upstream.url)
    try:
        result = _cargo_fetch(tree, registry.index_url, tmp_path / "cargo-home")
    finally:
        registry.close()
        upstream.stop()
    assert result.returncode == 0, result.stdout + result.stderr
    assert [hit for hit in upstream.hits if hit.startswith("/dl/")] == [
        "/dl/pubfix-ext/0.1.0/download"
    ]


def test_corrupt_bytes_fail_the_run_loudly(tmp_path: Path) -> None:
    """Corrupt upstream bytes fail loudly with checksum evidence, no poison.

    The upstream serves bytes that do not match the checksum of the served
    index entry; the registry validates what it fetched before serving or
    caching it, so the download is refused with a checksum error, the cargo
    run fails loudly instead of certifying corrupt bytes, and no corrupt
    bytes reach the persistent cache. Cargo remains the independent checksum
    authority over anything the registry does serve.
    """
    good = _crate_bytes("pubfix-ext", "0.1.0")
    served = tmp_path / "served"
    crate = served / "dl" / "pubfix-ext" / "0.1.0" / "download"
    crate.parent.mkdir(parents=True)
    crate.write_bytes(b"corrupt bytes under the same name")
    upstream = _Upstream(served)
    tree = _external_dependency_tree(tmp_path)
    registry = _start_external_registry(tmp_path, hashlib.sha256(good).hexdigest(), upstream.url)
    try:
        result = _cargo_fetch(tree, registry.index_url, tmp_path / "cargo-home")
        status, body = _loopback_get(_registry_port(registry), "/dl/pubfix-ext/0.1.0/download")
    finally:
        registry.close()
        upstream.stop()
    assert result.returncode != 0
    assert "checksum" in (result.stdout + result.stderr).lower()
    assert status == 500
    assert b"failed its checksum" in body
    assert b"not cached" in body
    # Nothing is cached, so every retry re-fetches; what matters is that the
    # only artifact ever requested is the locked one and the corrupt bytes
    # never reached the persistent cache.
    hits = [hit for hit in upstream.hits if hit.startswith("/dl/")]
    assert hits
    assert set(hits) == {"/dl/pubfix-ext/0.1.0/download"}
    assert not (tmp_path / "dl-cache" / "pubfix-ext-0.1.0.crate").exists()


def test_a_no_verify_registry_refuses_every_download_without_an_upstream_hit(
    tmp_path: Path,
) -> None:
    """Downloads disabled: every /dl request fails loudly, nothing is fetched.

    A `--no-verify` run must download no artifacts: with downloads disabled
    the handler answers every `/dl` request with a 503 naming the
    `--no-verify` mode -- before the uploaded-member lookup, the locked
    external validation, or any upstream fetch. Pinned against a live
    countable upstream recording zero hits, for both a locked-name request
    and a path that would otherwise hit the uploaded-member tree.
    """
    served = tmp_path / "served"
    payload = b"locked bytes"
    crate = served / "dl" / "pubfix-leaf" / "0.1.0" / "download"
    crate.parent.mkdir(parents=True)
    crate.write_bytes(payload)
    upstream = _Upstream(served)
    registry, _, dl_cache = _lazy_registry(
        tmp_path / "run",
        externals={"pubfix-leaf": {"0.1.0"}},
        dl_base=f"{upstream.url}/dl",
        downloads_enabled=False,
        cksum=hashlib.sha256(payload).hexdigest(),
    )
    try:
        port = _registry_port(registry)
        status, body = _loopback_get(port, "/dl/pubfix-leaf/0.1.0/download")
        uploaded_status, uploaded_body = _loopback_get(port, "/dl/pubfix-leaf/9.9.9/download")
    finally:
        registry.close()
        upstream.stop()
    assert status == 503
    assert b"--no-verify" in body
    assert uploaded_status == 503
    assert b"--no-verify" in uploaded_body
    assert upstream.hits == []
    assert not dl_cache.exists()


def test_a_corrupt_persistent_cache_is_repaired_from_the_upstream(tmp_path: Path) -> None:
    """A cached artifact that fails the index checksum is repaired, not trusted.

    The persistent lock-hash cache is validated against the mirrored index
    checksum at request time: corrupt cached bytes are discarded, a good
    upstream fetch repairs the cache, and the repaired bytes are served.
    """
    good = _crate_bytes("pubfix-leaf", "0.1.0")
    served = tmp_path / "served"
    crate = served / "dl" / "pubfix-leaf" / "0.1.0" / "download"
    crate.parent.mkdir(parents=True)
    crate.write_bytes(good)
    upstream = _Upstream(served)
    registry, _, dl_cache = _lazy_registry(
        tmp_path / "run",
        externals={"pubfix-leaf": {"0.1.0"}},
        dl_base=f"{upstream.url}/dl",
        cksum=hashlib.sha256(good).hexdigest(),
    )
    poisoned = dl_cache / "pubfix-leaf-0.1.0.crate"
    poisoned.parent.mkdir(parents=True)
    poisoned.write_bytes(b"corrupt cached bytes")
    try:
        status, body = _loopback_get(_registry_port(registry), "/dl/pubfix-leaf/0.1.0/download")
    finally:
        registry.close()
        upstream.stop()
    assert status == 200
    assert body == good
    assert [hit for hit in upstream.hits if hit.startswith("/dl/")] == [
        "/dl/pubfix-leaf/0.1.0/download"
    ]
    assert poisoned.read_bytes() == good


def test_wrong_source_and_corrupt_real_cache_candidates_are_ignored(tmp_path: Path) -> None:
    """Only a valid crates.io cache artifact is ever served from the real cache.

    A same-named artifact left by an alternate registry's cache directory and
    a corrupt artifact under a crates.io cache directory both fail the
    mirrored index checksum; both are ignored, the good upstream fetch serves
    instead, and neither candidate's bytes reach the persistent cache.
    """
    good = _crate_bytes("pubfix-leaf", "0.1.0")
    served = tmp_path / "served"
    crate = served / "dl" / "pubfix-leaf" / "0.1.0" / "download"
    crate.parent.mkdir(parents=True)
    crate.write_bytes(good)
    upstream = _Upstream(served)
    caller = tmp_path / "caller-home" / "registry" / "cache"
    stranger = caller / "alternate-registry-0000" / "pubfix-leaf-0.1.0.crate"
    stranger.parent.mkdir(parents=True)
    stranger.write_bytes(b"stranger bytes from another registry")
    corrupt = caller / "index.crates.io-1234" / "pubfix-leaf-0.1.0.crate"
    corrupt.parent.mkdir(parents=True)
    corrupt.write_bytes(b"corrupt same-name bytes")
    registry, _, dl_cache = _lazy_registry(
        tmp_path / "run",
        externals={"pubfix-leaf": {"0.1.0"}},
        dl_base=f"{upstream.url}/dl",
        caller_cache=caller,
        cksum=hashlib.sha256(good).hexdigest(),
    )
    try:
        status, body = _loopback_get(_registry_port(registry), "/dl/pubfix-leaf/0.1.0/download")
    finally:
        registry.close()
        upstream.stop()
    assert status == 200
    assert body == good
    assert [hit for hit in upstream.hits if hit.startswith("/dl/")] == [
        "/dl/pubfix-leaf/0.1.0/download"
    ]
    assert (dl_cache / "pubfix-leaf-0.1.0.crate").read_bytes() == good


def test_a_valid_real_cache_candidate_is_reused_with_zero_upstream(tmp_path: Path) -> None:
    """Valid crates.io cache bytes are reused; a fresh instance reuses again.

    The caller's real crates.io cache holds bytes matching the mirrored index
    checksum: the handler serves them, copies them into the persistent
    lock-hash cache, and never touches the upstream; a fresh registry
    instance over the same persistent cache serves again with zero upstream
    hits.
    """
    module = _oracle_module()
    good = _crate_bytes("pubfix-leaf", "0.1.0")
    cksum = hashlib.sha256(good).hexdigest()
    overlay = tmp_path / "overlay"
    index = overlay / "index" / module.index_rel("pubfix-leaf")
    index.parent.mkdir(parents=True)
    index.write_text(
        json.dumps({"name": "pubfix-leaf", "vers": "0.1.0", "cksum": cksum}) + "\n",
        encoding="utf-8",
    )
    caller = tmp_path / "caller-home" / "registry" / "cache"
    cached = caller / "index.crates.io-1dcc56b5f7" / "pubfix-leaf-0.1.0.crate"
    cached.parent.mkdir(parents=True)
    cached.write_bytes(good)

    def fresh_registry(upstream: _Upstream) -> Any:
        registry = module.LoopbackRegistry(
            overlay,
            token=module.DUMMY_TOKEN,
            externals={"pubfix-leaf": {"0.1.0"}},
            dl_cache=tmp_path / "dl-cache",
            dl_base=f"{upstream.url}/dl",
            timeout=5.0,
            attempts=2,
            backoff=0.05,
            caller_cache=caller,
        )
        registry.start()
        return registry

    (tmp_path / "unreachable").mkdir()
    upstream = _Upstream(tmp_path / "unreachable")
    try:
        registry = fresh_registry(upstream)
        try:
            status, body = _loopback_get(_registry_port(registry), "/dl/pubfix-leaf/0.1.0/download")
        finally:
            registry.close()
        assert status == 200
        assert body == good
        assert upstream.hits == []
        assert (tmp_path / "dl-cache" / "pubfix-leaf-0.1.0.crate").read_bytes() == good
    finally:
        upstream.stop()
    (tmp_path / "unreachable-again").mkdir()
    again = _Upstream(tmp_path / "unreachable-again")
    try:
        registry = fresh_registry(again)
        try:
            status, body = _loopback_get(_registry_port(registry), "/dl/pubfix-leaf/0.1.0/download")
        finally:
            registry.close()
        assert status == 200
        assert body == good
        assert again.hits == []
    finally:
        again.stop()


def test_the_real_cache_source_resolves_the_caller_s_cargo_home(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """The real-cache source derives from CARGO_HOME at registry construction.

    With `CARGO_HOME` naming an explicit home, a registry built without
    injection resolves its real-cache source there and serves a valid cached
    artifact from it -- neither the ambient environment's cache nor a
    hardcoded home path is consulted.
    """
    module = _oracle_module()
    monkeypatch.setenv("HOME", str(tmp_path / "wrong-home"))
    monkeypatch.setenv("CARGO_HOME", str(tmp_path / "real-home"))
    good = _crate_bytes("pubfix-leaf", "0.1.0")
    cksum = hashlib.sha256(good).hexdigest()
    overlay = tmp_path / "overlay"
    index = overlay / "index" / module.index_rel("pubfix-leaf")
    index.parent.mkdir(parents=True)
    index.write_text(
        json.dumps({"name": "pubfix-leaf", "vers": "0.1.0", "cksum": cksum}) + "\n",
        encoding="utf-8",
    )
    cached = (
        tmp_path
        / "real-home"
        / "registry"
        / "cache"
        / "index.crates.io-1dcc56b5f7"
        / "pubfix-leaf-0.1.0.crate"
    )
    cached.parent.mkdir(parents=True)
    cached.write_bytes(good)
    (tmp_path / "unreachable").mkdir()
    upstream = _Upstream(tmp_path / "unreachable")
    registry = module.LoopbackRegistry(
        overlay,
        token=module.DUMMY_TOKEN,
        externals={"pubfix-leaf": {"0.1.0"}},
        dl_cache=tmp_path / "dl-cache",
        dl_base=f"{upstream.url}/dl",
        timeout=5.0,
        attempts=2,
        backoff=0.05,
    )
    registry.start()
    try:
        status, body = _loopback_get(_registry_port(registry), "/dl/pubfix-leaf/0.1.0/download")
    finally:
        registry.close()
        upstream.stop()
    assert status == 200
    assert body == good
    assert upstream.hits == []


def test_the_real_cache_source_falls_back_to_the_default_home(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """Without CARGO_HOME the real-cache source resolves under HOME/.cargo."""
    module = _oracle_module()
    monkeypatch.delenv("CARGO_HOME", raising=False)
    monkeypatch.setenv("HOME", str(tmp_path / "home"))
    good = _crate_bytes("pubfix-leaf", "0.1.0")
    cksum = hashlib.sha256(good).hexdigest()
    overlay = tmp_path / "overlay"
    index = overlay / "index" / module.index_rel("pubfix-leaf")
    index.parent.mkdir(parents=True)
    index.write_text(
        json.dumps({"name": "pubfix-leaf", "vers": "0.1.0", "cksum": cksum}) + "\n",
        encoding="utf-8",
    )
    cached = (
        tmp_path
        / "home"
        / ".cargo"
        / "registry"
        / "cache"
        / "index.crates.io-1dcc56b5f7"
        / "pubfix-leaf-0.1.0.crate"
    )
    cached.parent.mkdir(parents=True)
    cached.write_bytes(good)
    (tmp_path / "unreachable").mkdir()
    upstream = _Upstream(tmp_path / "unreachable")
    registry = module.LoopbackRegistry(
        overlay,
        token=module.DUMMY_TOKEN,
        externals={"pubfix-leaf": {"0.1.0"}},
        dl_cache=tmp_path / "dl-cache",
        dl_base=f"{upstream.url}/dl",
        timeout=5.0,
        attempts=2,
        backoff=0.05,
    )
    registry.start()
    try:
        status, body = _loopback_get(_registry_port(registry), "/dl/pubfix-leaf/0.1.0/download")
    finally:
        registry.close()
        upstream.stop()
    assert status == 200
    assert body == good
    assert upstream.hits == []


def test_the_registry_server_binds_distinct_ephemeral_ports(tmp_path: Path) -> None:
    """No fixed preferred port: each server binds an unbound port at start.

    A port chosen by the OS at bind time cannot be pre-bound by an attacker
    ahead of the run, and two concurrent runs cannot collide on it.
    """
    module = _oracle_module()
    registries = []
    for i in range(2):
        overlay = tmp_path / f"overlay-{i}"
        (overlay / "index").mkdir(parents=True)
        registries.append(
            module.LoopbackRegistry(
                overlay,
                token=module.DUMMY_TOKEN,
                externals={},
                dl_cache=tmp_path / f"dl-cache-{i}",
            )
        )
    try:
        for registry in registries:
            registry.start()
        ports = [_registry_port(registry) for registry in registries]
        assert len(set(ports)) == 2
        bodies = [_loopback_get(port, "/index/config.json")[1] for port in ports]
        assert bodies[0] != bodies[1]
    finally:
        for registry in registries:
            registry.close()


def test_each_run_gets_a_distinct_overlay_directory(tmp_path: Path) -> None:
    """The shared-overlay deletion race needs a shared directory; runs get their own."""
    module = _oracle_module()
    first = module.overlay_run_dir(tmp_path)
    second = module.overlay_run_dir(tmp_path)
    assert first != second
    assert first.parent == tmp_path / "rust" / "target" / "publish-verify-overlay"
    first.mkdir(parents=True)
    (first / "index").write_text("entry", encoding="utf-8")
    # another run's cleanup removes only its own directory
    shutil.rmtree(second, ignore_errors=True)
    assert (first / "index").is_file()


def test_a_run_survives_a_squatter_on_the_retired_fixed_port(tmp_path: Path) -> None:
    """The overlay once preferred 41946; a squatter there must not matter.

    With ephemeral ports the run never tries the fixed port, so a pre-bound
    listener cannot starve it and cannot serve a pre-planted index.
    """
    tree = _lockstep_tree(
        tmp_path,
        {
            "pubfix-leaf": "",
            "pubfix-mid": "[dependencies]\npubfix-leaf.workspace = true\n",
            "pubfix-top": "[dependencies]\npubfix-mid.workspace = true\n",
        },
    )
    _census(tree, [])
    with socket.socket() as squatter:
        try:
            squatter.bind(("127.0.0.1", 41946))
            squatter.listen(1)
        except OSError:
            # the retired port is already occupied (a sibling worker's own
            # squatter) -- which is the condition under test
            pass
        result = _oracle(tree, "--no-verify")
    assert result.returncode == 0, result.stdout + result.stderr


def test_concurrent_runs_on_distinct_trees_stay_independent(tmp_path: Path) -> None:
    """Two runs at once share no port and no overlay directory."""
    members = {
        "pubfix-leaf": "",
        "pubfix-mid": "[dependencies]\npubfix-leaf.workspace = true\n",
        "pubfix-top": "[dependencies]\npubfix-mid.workspace = true\n",
    }
    trees = [_lockstep_tree(tmp_path / f"tree-{i}", members) for i in range(2)]
    for tree in trees:
        _census(tree, [])
    processes = [
        subprocess.Popen(
            [sys.executable, str(_ORACLE), "--root", str(tree), "--no-verify"],
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        )
        for tree in trees
    ]
    outputs = [process.communicate(timeout=290) for process in processes]
    for process, (out, err) in zip(processes, outputs, strict=True):
        assert process.returncode == 0, out + err
        for name in ("pubfix-leaf", "pubfix-mid", "pubfix-top"):
            assert f"publish-verify covered {name}" in out


def test_two_concurrent_same_tree_runs_serialize_and_both_cover(
    tmp_path: Path,
) -> None:
    """Same-tree runs serialize on an interprocess lock; distinct trees do not.

    Two invocations in one throwaway workspace share `rust/target/package`
    staging and the lockfile, so their package and publish phases cannot
    interleave: both complete with every member covered. The lock path is
    derived from the tree root, so this waits only same-workspace runs --
    runs in different trees take different locks and proceed independently
    (pinned by `test_concurrent_runs_on_distinct_trees_stay_independent`).
    """
    members = {
        "pubfix-leaf": "",
        "pubfix-mid": "[dependencies]\npubfix-leaf.workspace = true\n",
        "pubfix-top": "[dependencies]\npubfix-mid.workspace = true\n",
    }
    tree = _lockstep_tree(tmp_path / "tree", members)
    _census(tree, [])
    processes = [
        subprocess.Popen(
            [sys.executable, str(_ORACLE), "--root", str(tree), "--no-verify"],
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        )
        for _ in range(2)
    ]
    outputs = [process.communicate(timeout=580) for process in processes]
    for process, (out, err) in zip(processes, outputs, strict=True):
        assert process.returncode == 0, out + err
        for name in members:
            assert f"publish-verify covered {name}" in out


def test_a_held_same_tree_lock_fails_bounded_and_names_the_owner(
    tmp_path: Path,
) -> None:
    """A run facing a held workspace lock waits bounded, then fails loud.

    The error names the lock file and the recorded owner pid, so a stuck
    holder is diagnosable instead of a silent hang or a concurrent run over
    shared staging. The wait bound is a caller-overridable environment
    setting (`PUBLISH_VERIFY_LOCK_WAIT_SECONDS`), not a fixed constant.
    """
    tree = _lockstep_tree(
        tmp_path,
        {
            "pubfix-leaf": "",
            "pubfix-mid": "[dependencies]\npubfix-leaf.workspace = true\n",
        },
    )
    _census(tree, [])
    lock = tree / "rust" / "target" / "publish-verify.lock"
    lock.parent.mkdir(parents=True, exist_ok=True)
    holder_pid = os.getpid()
    fd = os.open(lock, os.O_RDWR | os.O_CREAT, 0o644)
    try:
        fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
        os.ftruncate(fd, 0)
        os.write(fd, json.dumps({"pid": holder_pid}).encode("utf-8"))
        env = {**os.environ, "PUBLISH_VERIFY_LOCK_WAIT_SECONDS": "1"}
        result = subprocess.run(
            [sys.executable, str(_ORACLE), "--root", str(tree), "--no-verify"],
            env=env,
            check=False,
            capture_output=True,
            text=True,
            timeout=300,
        )
    finally:
        fcntl.flock(fd, fcntl.LOCK_UN)
        os.close(fd)
    assert result.returncode != 0
    output = result.stdout + result.stderr
    assert str(lock) in output
    assert str(holder_pid) in output


@pytest.mark.parametrize("raw", ["nan", "NaN", "inf", "-inf", "Infinity", "-Infinity"])
def test_a_non_finite_lock_wait_fails_loud(raw: str, capsys: pytest.CaptureFixture[str]) -> None:
    """NaN and infinities fail loud like the other invalid bounds.

    NaN would make the wait's deadline comparison permanently false (a wait
    that never times out and never reports) and an infinity would make the
    supposedly bounded wait unbounded, so a non-finite
    `PUBLISH_VERIFY_LOCK_WAIT_SECONDS` is rejected with the variable and
    value named, never accepted as a number of seconds.
    """
    module = _oracle_module()
    with pytest.MonkeyPatch.context() as monkeypatch:
        monkeypatch.setenv("PUBLISH_VERIFY_LOCK_WAIT_SECONDS", raw)
        with pytest.raises(SystemExit):
            module.lock_wait_seconds()
    assert raw in capsys.readouterr().err


def test_a_failed_run_cleans_up_without_deleting_another_runs_overlay(
    tmp_path: Path,
) -> None:
    """A run removes only its own per-run directory, in failure and success.

    A failing run leaves no overlay residue of its own and never deletes the
    directory an earlier `--keep-overlay` run kept for inspection.
    """
    tree = _lockstep_tree(
        tmp_path,
        {
            "pubfix-leaf": "",
            "pubfix-mid": "[dependencies]\npubfix-leaf.workspace = true\n",
            "pubfix-top": "[dependencies]\npubfix-mid.workspace = true\n",
        },
    )
    _census(tree, [])
    good = _oracle(tree, "--no-verify", "--keep-overlay")
    assert good.returncode == 0, good.stdout + good.stderr
    overlay_root = tree / "rust" / "target" / "publish-verify-overlay"
    kept = list(overlay_root.iterdir())
    assert len(kept) == 1

    # a stale lock fails the second run during packing, after its cleanup
    # path is live
    lock = tree / "rust" / "Cargo.lock"
    stale = lock.read_text(encoding="utf-8").replace(
        'name = "pubfix-leaf"\nversion = "0.1.0"',
        'name = "pubfix-leaf"\nversion = "9.9.9"',
    )
    lock.write_text(stale, encoding="utf-8")
    bad = _oracle(tree, "--no-verify")
    assert bad.returncode != 0
    assert list(overlay_root.iterdir()) == kept


def test_a_sentinel_real_cargo_home_stays_untouched(tmp_path: Path) -> None:
    """Ephemeral Cargo state is quarantined in the run's own CARGO_HOME.

    Packing and the dry-run rounds make cargo write loopback-registry caches
    (`127.0.0.1-*` residue) under CARGO_HOME; a run points CARGO_HOME at its
    own overlay directory, so a sentinel real CARGO_HOME handed to the run is
    byte-for-byte unchanged afterwards and the residue lives only under the
    kept per-run overlay.
    """
    tree = _lockstep_tree(
        tmp_path,
        {
            "pubfix-leaf": "",
            "pubfix-mid": "[dependencies]\npubfix-leaf.workspace = true\n",
        },
    )
    _census(tree, [])
    sentinel = tmp_path / "sentinel-cargo-home"
    # Pre-existing nested content: the pinned postcondition is byte-for-byte
    # equality of what the caller's real Cargo home already held, not just
    # emptiness.
    config = sentinel / "config.toml"
    config.parent.mkdir(parents=True, exist_ok=True)
    config.write_bytes(b'[registry]\nindex = "https://example.invalid"\n')
    cache = sentinel / "registry" / "cache" / "ex" / "am"
    cache.mkdir(parents=True)
    (cache / "example-1.0.0.crate").write_bytes(b"pre-existing crate bytes")
    (sentinel / ".package-cache").mkdir()

    def snapshot() -> list[tuple[str, str]]:
        """(relative path, sha256) per file, plus `/`-marked directories."""
        entries = []
        for path in sorted(sentinel.rglob("*")):
            rel = str(path.relative_to(sentinel))
            if path.is_dir():
                entries.append((rel + "/", ""))
            else:
                entries.append((rel, hashlib.sha256(path.read_bytes()).hexdigest()))
        return entries

    before = snapshot()
    env = {**os.environ, "CARGO_HOME": str(sentinel)}
    result = subprocess.run(
        [sys.executable, str(_ORACLE), "--root", str(tree), "--no-verify", "--keep-overlay"],
        env=env,
        check=False,
        capture_output=True,
        text=True,
        timeout=580,
    )
    assert result.returncode == 0, result.stdout + result.stderr
    assert snapshot() == before
    overlay = _only_overlay_run(tree)
    quarantined = sorted(str(f.relative_to(overlay)) for f in overlay.rglob("*"))
    assert any("cargo-home" in path for path in quarantined)
    assert any("127.0.0.1-" in path for path in quarantined)


def _oracle_module() -> object:
    """The real script, imported for the focused lock-source unit tests."""
    return load_script("publish_verify", _ORACLE)


def _lock_entry(name: str, version: str, source: str | None) -> str:
    source_line = "" if source is None else f'source = "{source}"\n'
    return f'[[package]]\nname = "{name}"\nversion = "{version}"\n{source_line}'


def _lock_file(tree: Path, entries: list[str]) -> Path:
    lock = tree / "rust" / "Cargo.lock"
    lock.write_text("version = 4\n\n" + "\n\n".join(entries) + "\n", encoding="utf-8")
    return lock


def test_lock_externals_mirrors_only_crates_io_pins(tmp_path: Path) -> None:
    """The overlay can only serve what crates.io serves, so only a pin whose
    locked source is crates.io is mirrored; workspace members never are."""
    module = _oracle_module()
    tree = _lockstep_tree(tmp_path, {"pubfix-leaf": ""})
    _lock_file(
        tree,
        [
            _lock_entry("pubfix-leaf", "0.1.0", None),
            _lock_entry(
                "serde", "1.0.200", "registry+https://github.com/rust-lang/crates.io-index"
            ),
        ],
    )

    externals, problems = module.lock_externals(tree / "rust" / "Cargo.lock", {"pubfix-leaf"})

    assert externals == {"serde": {"1.0.200"}}
    assert problems == []


def test_a_path_locked_non_member_fails_the_source_filter(tmp_path: Path) -> None:
    module = _oracle_module()
    tree = _lockstep_tree(tmp_path, {"pubfix-leaf": ""})
    _lock_file(tree, [_lock_entry("pubfix-outsider", "0.1.0", None)])

    externals, problems = module.lock_externals(tree / "rust" / "Cargo.lock", set())

    assert externals == {}
    assert len(problems) == 1
    assert "pubfix-outsider" in problems[0]
    assert "path" in problems[0]


def test_a_git_locked_non_member_fails_the_source_filter(tmp_path: Path) -> None:
    module = _oracle_module()
    tree = _lockstep_tree(tmp_path, {"pubfix-leaf": ""})
    source = "git+https://github.com/example/gitdep?rev=abc123#abc123"
    _lock_file(tree, [_lock_entry("gitdep", "0.1.0", source)])

    externals, problems = module.lock_externals(tree / "rust" / "Cargo.lock", set())

    assert externals == {}
    assert len(problems) == 1
    assert "gitdep" in problems[0]
    assert source in problems[0]


def test_an_alternate_registry_locked_non_member_fails_the_source_filter(
    tmp_path: Path,
) -> None:
    module = _oracle_module()
    tree = _lockstep_tree(tmp_path, {"pubfix-leaf": ""})
    source = "sparse+https://alt.example.com/index/"
    _lock_file(tree, [_lock_entry("altreg", "0.2.0", source)])

    externals, problems = module.lock_externals(tree / "rust" / "Cargo.lock", set())

    assert externals == {}
    assert len(problems) == 1
    assert "altreg" in problems[0]
    assert source in problems[0]


def test_a_non_member_path_dependency_fails_the_run_loud(tmp_path: Path) -> None:
    """A path-sourced non-member must never resolve by name on crates.io.

    The lock pins the outsider by path; a mirror that ignored sources would
    fetch whatever crates.io happens to publish under the same name -- a
    stranger's code -- so the run refuses before any mirror fetch.
    """
    dev_table = '[dev-dependencies]\npubfix-outsider = { path = "../../../outsider" }\n'
    tree = _lockstep_tree(tmp_path, {"pubfix-leaf": dev_table})
    outsider = tree / "outsider"
    (outsider / "src").mkdir(parents=True)
    (outsider / "src" / "lib.rs").write_text("pub fn f() {}\n", encoding="utf-8")
    (outsider / "Cargo.toml").write_text(
        '[package]\nname = "pubfix-outsider"\nversion = "0.1.0"\nedition = "2021"\n',
        encoding="utf-8",
    )
    _census(tree, [])

    result = _oracle(tree, "--no-verify")

    assert result.returncode != 0
    assert "pubfix-outsider" in result.stdout + result.stderr
    assert "locked to a path source" in result.stdout + result.stderr


def test_a_renamed_library_dependency_verifies_end_to_end(tmp_path: Path) -> None:
    """A member importing a library through an alias builds in the verified run.

    `renamed = { package = "pubfix-real", ... }` must resolve to a dependency
    linked under the alias the member's code imports -- both when the member
    is the round's own target and when a dependent member resolves it through
    the loopback index, where the Cargo-generated entry's dependency list is
    the only rename carrier: the alias in the entry dependency's `name`, the
    real package name in `package` (the documented index spelling; an entry
    that loses either name breaks the rename exactly as `degenbot-evm2`'s
    `use evm2_macros::instruction` died while a dependent member was
    verified). The non-renamed control proves the run is otherwise sound.
    """
    tree = _lockstep_tree(
        tmp_path,
        {
            "pubfix-real": "",
            "pubfix-plain": "",
            "pubfix-user": (
                "[dependencies]\n"
                'renamed = { package = "pubfix-real", version = "0.1.0", '
                'path = "../pubfix-real" }\n'
                'pubfix-plain = { version = "0.1.0", path = "../pubfix-plain" }\n'
            ),
            "pubfix-top": "[dependencies]\npubfix-user.workspace = true\n",
        },
    )
    (tree / "rust" / "crates" / "pubfix-user" / "src" / "lib.rs").write_text(
        "pub fn use_both() {\n    renamed::f();\n    pubfix_plain::f();\n}\n",
        encoding="utf-8",
    )
    (tree / "rust" / "crates" / "pubfix-top" / "src" / "lib.rs").write_text(
        "pub fn use_top() {\n    pubfix_user::use_both();\n}\n",
        encoding="utf-8",
    )
    _census(tree, [])
    result = _oracle(tree, "--keep-overlay")
    assert result.returncode == 0, result.stdout + result.stderr
    assert "aborting upload due to dry run" in result.stderr
    # the Cargo-generated entry keeps both rename names
    overlay = _only_overlay_run(tree)
    entry = json.loads(
        (overlay / "index" / "pu" / "bf" / "pubfix-user").read_text(encoding="utf-8")
    )
    deps = {dep["name"]: dep for dep in entry["deps"]}
    assert deps["renamed"]["package"] == "pubfix-real"
    # a dependency that was not renamed carries no rename field
    assert "package" not in deps["pubfix-plain"]


def test_the_loopback_entry_is_cargo_generated_and_matches_the_uploaded_bytes(
    tmp_path: Path,
) -> None:
    """The loopback index entry is cargo's upload metadata, byte-exact.

    The entry's checksum is the uploaded ``.crate``'s checksum (the receipt
    and the stored artifact agree), the dependency list is the rewritten form
    the member will upload, and there is exactly one entry per member -- the
    registry never appended a second, divergent line for the same version.
    """
    tree = _lockstep_tree(
        tmp_path,
        {
            "pubfix-leaf": "",
            "pubfix-mid": "[dependencies]\npubfix-leaf.workspace = true\n",
            "pubfix-top": "[dependencies]\npubfix-mid.workspace = true\n",
        },
    )
    _census(tree, [])
    result = _oracle(tree, "--no-verify", "--keep-overlay")
    assert result.returncode == 0, result.stdout + result.stderr
    overlay = _only_overlay_run(tree)
    stored = overlay / "dl" / "pubfix-top" / "0.1.0" / "download"
    assert stored.is_file()
    index_lines = [
        line
        for line in (overlay / "index" / "pu" / "bf" / "pubfix-top")
        .read_text(encoding="utf-8")
        .splitlines()
        if line.strip()
    ]
    assert len(index_lines) == 1
    entry = json.loads(index_lines[0])
    assert entry["name"] == "pubfix-top"
    assert entry["vers"] == "0.1.0"
    assert entry["cksum"] == hashlib.sha256(stored.read_bytes()).hexdigest()
    receipts = [
        json.loads(line)
        for line in (overlay / "uploads.jsonl").read_text(encoding="utf-8").splitlines()
        if line.strip()
    ]
    assert [r["cksum"] for r in receipts if r["name"] == "pubfix-top"] == [entry["cksum"]]
    assert entry["deps"] == [
        {
            "name": "pubfix-mid",
            "req": "^0.1.0",
            "features": [],
            "optional": False,
            "default_features": True,
            "target": None,
            "kind": "normal",
        }
    ]


def test_uploads_land_only_on_the_loopback_under_the_dummy_token(tmp_path: Path) -> None:
    """Every upload receipt proves the loopback endpoint and the dummy auth.

    The registry server records what actually arrived: a loopback Publish API
    endpoint and the run's fixed dummy token -- never a caller credential.
    The run's cargo config names the loopback index twice (resolution and
    upload) and no real crates.io endpoint or token at all.
    """
    tree = _lockstep_tree(
        tmp_path,
        {
            "pubfix-leaf": "",
            "pubfix-mid": "[dependencies]\npubfix-leaf.workspace = true\n",
        },
    )
    _census(tree, [])
    result = _oracle(tree, "--keep-overlay")
    assert result.returncode == 0, result.stdout + result.stderr
    overlay = _only_overlay_run(tree)
    receipts = [
        json.loads(line)
        for line in (overlay / "uploads.jsonl").read_text(encoding="utf-8").splitlines()
        if line.strip()
    ]
    assert {r["name"] for r in receipts} == {"pubfix-leaf", "pubfix-mid"}
    for receipt in receipts:
        assert receipt["endpoint"].startswith("http://127.0.0.1:")
        assert receipt["endpoint"].endswith("/api/v1/crates/new")
        assert receipt["auth"] == _oracle_module().DUMMY_TOKEN
    config = (overlay / "cargo-home" / "config.toml").read_text(encoding="utf-8")
    assert _oracle_module().DUMMY_TOKEN in config
    assert "https://crates.io" not in config
    assert "index.crates.io" not in config


def test_uploads_land_on_the_loopback_under_hostile_token_env(tmp_path: Path) -> None:
    """Token env overrides cannot retarget the upload auth; the dummy wins.

    cargo prefers environment variables over config tables, so a real
    ``CARGO_TOKEN`` or a named-registry ``CARGO_REGISTRIES_*_TOKEN`` in the
    caller's environment would override the config's fixed dummy token and
    put a real credential on the wire. The run strips every such variable
    from the cargo child environment, so even a run started with all of them
    hostile uploads under the dummy -- the receipts record what arrived.
    """
    tree = _lockstep_tree(
        tmp_path,
        {
            "pubfix-leaf": "",
            "pubfix-mid": "[dependencies]\npubfix-leaf.workspace = true\n",
        },
    )
    _census(tree, [])
    with pytest.MonkeyPatch.context() as monkeypatch:
        monkeypatch.setenv("CARGO_TOKEN", "real-default-registry-token")
        monkeypatch.setenv("CARGO_REGISTRIES_CRATES_IO_TOKEN", "real-crates-io-token")
        monkeypatch.setenv("CARGO_REGISTRIES_PUBVERIFY_LOOPBACK_TOKEN", "hostile-named-token")
        monkeypatch.setenv("CARGO_REGISTRY_TOKEN", "hostile-legacy-token")
        result = _oracle(tree, "--keep-overlay")
    assert result.returncode == 0, result.stdout + result.stderr
    overlay = _only_overlay_run(tree)
    receipts = [
        json.loads(line)
        for line in (overlay / "uploads.jsonl").read_text(encoding="utf-8").splitlines()
        if line.strip()
    ]
    assert {r["name"] for r in receipts} == {"pubfix-leaf", "pubfix-mid"}
    for receipt in receipts:
        assert receipt["auth"] == _oracle_module().DUMMY_TOKEN


def test_cargo_environment_strips_token_overrides(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """Every token override is stripped from the cargo child environment.

    The default-registry token (``CARGO_TOKEN``), the legacy
    ``CARGO_REGISTRY_*`` spellings, and every named-registry
    ``CARGO_REGISTRIES_*`` variable are removed; an unrelated variable and
    the redirected ``CARGO_HOME`` survive.
    """
    module = _oracle_module()
    monkeypatch.setenv("CARGO_TOKEN", "real-default-token")
    monkeypatch.setenv("CARGO_REGISTRY_TOKEN", "legacy-token")
    monkeypatch.setenv("CARGO_REGISTRIES_CRATES_IO_TOKEN", "crates-io-token")
    monkeypatch.setenv("CARGO_REGISTRIES_PUBVERIFY_LOOPBACK_TOKEN", "named-token")
    monkeypatch.setenv("KEEP_ME", "value")

    env = module.cargo_environment(tmp_path / "cargo-home")

    for hostile in (
        "CARGO_TOKEN",
        "CARGO_REGISTRY_TOKEN",
        "CARGO_REGISTRIES_CRATES_IO_TOKEN",
        "CARGO_REGISTRIES_PUBVERIFY_LOOPBACK_TOKEN",
    ):
        assert hostile not in env, hostile
    assert env["KEEP_ME"] == "value"
    assert env["CARGO_HOME"] == str(tmp_path / "cargo-home")


def test_a_member_that_fails_to_compile_fails_the_verified_run(tmp_path: Path) -> None:
    """The verification round builds the packaged form, so a green run builds.

    A member whose code does not compile still uploads in the population
    round (`--no-verify` stages without building) and then fails the
    verification round's build -- the run refuses to certify what cannot
    compile, exactly the failure the real upload round would hit.
    """
    tree = _lockstep_tree(
        tmp_path,
        {
            "pubfix-leaf": "",
            "pubfix-mid": "[dependencies]\npubfix-leaf.workspace = true\n",
        },
    )
    (tree / "rust" / "crates" / "pubfix-leaf" / "src" / "lib.rs").write_text(
        "this is not rust\n",
        encoding="utf-8",
    )
    _census(tree, [])
    result = _oracle(tree)
    assert result.returncode != 0
    output = result.stdout + result.stderr
    assert "Verifying pubfix-leaf" in output
    assert "aborting upload due to dry run" not in output


def test_the_loopback_entry_carries_rust_version_and_links_from_the_upload(
    tmp_path: Path,
) -> None:
    """The entry carries the Publish payload's rust_version and links when present.

    Cargo frames both fields in the Publish API payload it uploads; the
    mapping onto the sparse-index spelling must carry them when present.
    The evidence is a real upload: the workspace MSRV (rust-version = "1.97")
    and a `links`-bearing member with a build script, each visible in the
    Cargo-generated entry exactly as cargo sent it. A member with neither
    field carries neither key.
    """
    tree = tmp_path / "tree"
    rust = tree / "rust"
    rust.mkdir(parents=True)
    (rust / "Cargo.toml").write_text(
        """[workspace]
members = ["crates/pubfix-plain", "crates/pubfix-linked"]
resolver = "2"

[workspace.package]
version = "0.1.0"
edition = "2021"
rust-version = "1.97"
"""
    )
    for name, links in (("pubfix-plain", ""), ("pubfix-linked", 'links = "pubfix-native"\n')):
        crate = rust / "crates" / name
        (crate / "src").mkdir(parents=True)
        (crate / "src" / "lib.rs").write_text("pub fn f() {}\n", encoding="utf-8")
        (crate / "build.rs").write_text("fn main() {}\n", encoding="utf-8")
        (crate / "Cargo.toml").write_text(
            f'[package]\nname = "{name}"\nversion.workspace = true\n'
            f"edition.workspace = true\nrust-version.workspace = true\n{links}",
            encoding="utf-8",
        )
    _census(tree, [])
    result = _oracle(tree, "--no-verify", "--keep-overlay")
    assert result.returncode == 0, result.stdout + result.stderr
    overlay = _only_overlay_run(tree)
    plain = json.loads(
        (overlay / "index" / "pu" / "bf" / "pubfix-plain").read_text(encoding="utf-8")
    )
    linked = json.loads(
        (overlay / "index" / "pu" / "bf" / "pubfix-linked").read_text(encoding="utf-8")
    )
    assert plain["rust_version"] == "1.97"
    assert "links" not in plain
    assert linked["rust_version"] == "1.97"
    assert linked["links"] == "pubfix-native"


def test_clear_cargo_staging_removes_only_the_tmp_dirs(tmp_path: Path) -> None:
    """The staging cleanup removes Cargo's tmp-crate/tmp-registry and nothing else.

    `target/package` also holds packaged artifacts; deleting it wholesale
    would destroy them. Only the two temporary staging directories Cargo
    creates for a publish round are removed.
    """
    module = _oracle_module()
    package = tmp_path / "rust" / "target" / "package"
    (package / "tmp-crate").mkdir(parents=True)
    (package / "tmp-crate" / "pubfix-leaf-0.1.0.crate").write_bytes(b"stale")
    (package / "tmp-registry" / "index").mkdir(parents=True)
    (package / "tmp-registry" / "index" / "garbage").write_text("{}", encoding="utf-8")
    artifact = package / "pubfix-leaf-0.1.0.crate"
    artifact.write_bytes(b"packaged artifact bytes")
    other = package / "pubfix-leaf-0.1.0"
    other.mkdir()

    module.clear_cargo_staging(tmp_path / "rust")

    assert not (package / "tmp-crate").exists()
    assert not (package / "tmp-registry").exists()
    assert artifact.read_bytes() == b"packaged artifact bytes"
    assert other.is_dir()


def test_a_dirty_staging_directory_does_not_poison_a_rerun(tmp_path: Path) -> None:
    """Residue from a previous round is cleared under the lock before a rerun.

    A killed run leaves `target/package/tmp-registry` (a stale index Cargo
    may reuse) and `tmp-crate` (stale packed bytes) behind; a rerun that
    trusted them would certify against the previous round's leftovers. The
    rerun removes exactly those directories before its publish round,
    succeeds, and leaves packaged artifacts outside them untouched.
    """
    tree = _lockstep_tree(
        tmp_path,
        {
            "pubfix-leaf": "",
            "pubfix-mid": "[dependencies]\npubfix-leaf.workspace = true\n",
        },
    )
    _census(tree, [])
    first = _oracle(tree, "--no-verify")
    assert first.returncode == 0, first.stdout + first.stderr
    staging = tree / "rust" / "target" / "package"
    poisoned = staging / "tmp-registry" / "index" / "pu" / "bf" / "pubfix-leaf"
    assert poisoned.is_file(), "the first round must leave staging residue to poison"
    poisoned.write_text('{"garbage": true}\n', encoding="utf-8")
    (staging / "tmp-crate" / "sentinel-stale.crate").write_bytes(b"stale bytes")
    artifact = staging / "pubfix-mid-0.1.0.crate"
    artifact.write_bytes(b"packaged artifact from an earlier packaging")

    second = _oracle(tree, "--no-verify", "--keep-overlay")

    assert second.returncode == 0, second.stdout + second.stderr
    # the poisoned residue was removed before the round, not reused
    assert not (staging / "tmp-crate" / "sentinel-stale.crate").exists()
    fresh = json.loads(
        (staging / "tmp-registry" / "index" / "pu" / "bf" / "pubfix-leaf").read_text(
            encoding="utf-8"
        )
    )
    assert "garbage" not in fresh
    overlay = _only_overlay_run(tree)
    assert (
        fresh["cksum"]
        == hashlib.sha256(
            (overlay / "dl" / "pubfix-leaf" / "0.1.0" / "download").read_bytes()
        ).hexdigest()
    )
    # packaged artifacts outside the two staging directories survive
    assert artifact.read_bytes() == b"packaged artifact from an earlier packaging"


def test_a_publish_false_member_is_out_of_the_eligible_covered_set(tmp_path: Path) -> None:
    """`publish = false` members are cargo-skipped, so they are not eligible.

    An eligible member is one cargo will actually upload in the population
    round; a `publish = false` member is skipped by `cargo publish
    --workspace` and must therefore stay out of the covered set entirely --
    no covered line and no receipt expectation.
    """
    tree = _lockstep_tree(
        tmp_path,
        {"pubfix-leaf": "", "priv": ""},
        publishable=("pubfix-leaf",),
    )
    _census(tree, [])
    result = _oracle(tree, "--no-verify")
    assert result.returncode == 0, result.stdout + result.stderr
    assert "publish-verify covered pubfix-leaf" in result.stdout
    assert "publish-verify covered priv" not in result.stdout


def test_upload_receipts_must_exactly_match_the_eligible_covered_set() -> None:
    """Missing and unexpected receipts both fail the coverage check.

    A receipt for a member outside the eligible set, or an eligible member
    without a receipt, means the round uploaded something other than what
    the run certified; the check names both directions and stays silent only
    on an exact match.
    """
    module = _oracle_module()
    assert module.receipt_problems({"a", "b"}, {"a", "b"}) is None
    missing = module.receipt_problems({"a", "b"}, {"a"})
    assert missing is not None
    assert "b" in missing
    unexpected = module.receipt_problems({"a"}, {"a", "ghost"})
    assert unexpected is not None
    assert "ghost" in unexpected
    both = module.receipt_problems({"a", "b"}, {"a", "ghost"})
    assert both is not None
    assert "b" in both
    assert "ghost" in both


def test_a_failed_population_round_prints_no_covered_lines(tmp_path: Path) -> None:
    """Covered lines print only after the population round has succeeded.

    A population round that fails (here: two members linking the same native
    library, which cargo cannot resolve) must print no coverage -- lines that
    name covered members ahead of the round's outcome would certify uploads
    the round never completed.
    """
    tree = tmp_path / "tree"
    rust = tree / "rust"
    rust.mkdir(parents=True)
    (rust / "Cargo.toml").write_text(
        """[workspace]
members = ["crates/pubfix-a", "crates/pubfix-b"]
resolver = "2"

[workspace.package]
version = "0.1.0"
edition = "2021"
"""
    )
    for name in ("pubfix-a", "pubfix-b"):
        crate = rust / "crates" / name
        (crate / "src").mkdir(parents=True)
        (crate / "src" / "lib.rs").write_text("pub fn f() {}\n", encoding="utf-8")
        (crate / "build.rs").write_text("fn main() {}\n", encoding="utf-8")
        (crate / "Cargo.toml").write_text(
            f'[package]\nname = "{name}"\nversion.workspace = true\n'
            'edition.workspace = true\nlinks = "pubfix-native"\n',
            encoding="utf-8",
        )
    _census(tree, [])
    result = _oracle(tree, "--no-verify")
    assert result.returncode != 0
    assert "publish-verify covered" not in result.stdout


def test_the_loopback_registry_rejects_traversal_outside_its_root(tmp_path: Path) -> None:
    """`_serve_file`/`do_GET` never serve a path outside the overlay root.

    Raw and percent-encoded `..` traversal (and any path that would escape
    the overlay root) is answered 403, so a hostile URL cannot read the
    caller's files through the loopback server. The in-root control still
    serves.
    """
    module = _oracle_module()
    overlay = tmp_path / "overlay"
    index = overlay / "index" / "pu" / "bf"
    index.mkdir(parents=True)
    (index / "pubfix-leaf").write_text('{"name": "pubfix-leaf"}\n', encoding="utf-8")
    secret = tmp_path / "secret.txt"
    secret.write_text("caller's own bytes", encoding="utf-8")
    assert (overlay / ".." / "secret.txt").resolve() == secret
    registry = module.LoopbackRegistry(
        overlay,
        token=module.DUMMY_TOKEN,
        externals={},
        dl_cache=tmp_path / "dl-cache",
    )
    registry.start()
    try:
        port = _registry_port(registry)
        status, _ = _loopback_get(port, "/index/pu/bf/pubfix-leaf")
        assert status == 200
        for hostile in (
            "/dl/../../secret.txt",
            "/dl/%2e%2e/%2e%2e/secret.txt",
            "/index/%2e%2e/%2e%2e/secret.txt",
            "/dl/..%2f..%2fsecret.txt",
            "/index/../..%2fsecret.txt",
        ):
            status, _ = _loopback_get(port, hostile)
            assert status == 403, hostile
    finally:
        registry.close()
