"""The publish-verify overlay (``scripts/publish_verify.py``).

`cargo publish --dry-run` resolves every member's rewritten manifest against
crates.io. At a fresh lockstep version (ADR-009) the sibling members are not
published yet, so the plain dry run -- verified or ``--no-verify`` alike --
dies on the first member another member requires. This file pins the overlay
mechanism that repairs the non-uploading verification without touching the
real upload rounds: every case drives the real script with ``--root`` pointed
at a throwaway workspace, so no assertion can be satisfied by a re-typed copy
of the script's logic.

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
  * ``test_the_member_entry_matches_the_packed_crate_checksum`` -- the overlay
    indexes members from their own packaged artifacts: the entry's checksum is
    the packed ``.crate``'s checksum, and the entry's dependencies are the
    rewritten ones the member will upload.
  * ``test_a_renamed_dependency_keeps_the_index_name_and_package_fields``,
    ``test_a_renamed_optional_dep_keeps_its_manifest_feature_reference``, and
    ``test_a_renamed_library_dependency_verifies_end_to_end`` -- a dependency
    renamed with `package = ...` keeps Cargo's index rename semantics: the
    entry carries the alias in `name` and the original package name in
    `package` (the documented index spelling), feature references keep the
    manifest's own `dep:` spellings (cargo validates them against the alias),
    and a member importing a library through the alias builds in a verified
    run even when it is compiled from the overlay by a dependent member (with
    a non-renamed control).

The Cargo-fidelity cases pin what the oracle resolves and what cannot
retarget it: packing runs ``--locked`` (a stale lock fails without a
rewrite), ``lock_externals`` mirrors crates.io pins only and fails loud on
path/git/alternate-registry pins, caller-supplied ``--registry``,
``--index``, ``--token``, and ``--config`` are refused in both argument
forms, and a ``publish`` allowlist naming a non-crates.io registry fails
loud instead of being verified against the wrong registry.

The network and lifecycle cases pin the mirror's bounded fetches (explicit
timeout, limited retries with backoff, transient failures only), the
``--no-verify`` index-only mirroring, ephemeral loopback ports, and the
per-run overlay directory.

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
import http.server
import importlib.util
import json
import os
import shutil
import socket
import subprocess
import sys
import threading
import time
import urllib.error
import urllib.request
from pathlib import Path

import pytest

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
    return subprocess.run(
        [sys.executable, str(_ORACLE), "--root", str(tree), *args],
        check=False,
        capture_output=True,
        text=True,
        timeout=600,
    )


class _LoopbackRegistry:
    """A loopback HTTP server over a scratch directory; counts every GET.

    ``fail_remaining`` responses answer with ``failure_status`` before the
    file tree is served (probing the retry policy); ``hang`` accepts the
    connection and never answers (probing the timeout).
    """

    def __init__(
        self,
        directory: Path,
        *,
        fail_remaining: int = 0,
        failure_status: int = 503,
        hang: bool = False,
    ) -> None:
        outer = self

        class Handler(http.server.BaseHTTPRequestHandler):
            def log_message(self, format: str, *args: object) -> None:
                pass

            def do_GET(self) -> None:
                outer.hits.append(self.path)
                if hang:
                    time.sleep(30)
                    return
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
    registry = _LoopbackRegistry(tmp_path / "empty-index")
    try:
        config = tree / "rust" / ".cargo" / "config.toml"
        config.parent.mkdir(parents=True, exist_ok=True)
        config.write_text(
            '[source.crates-io]\nreplace-with = "probe-registry"\n\n'
            "[source.probe-registry]\n"
            f'registry = "sparse+{registry.url}/index/"\n',
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
        registry.stop()
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


def test_a_packing_dev_cycle_still_covers_every_member(tmp_path: Path) -> None:
    """A dev-dependency edge closing a normal cycle has no packing order.

    cargo resolves a member's dev-dependencies at packaging, so `db ->dev
    pools -> uniswap -> db` (the real workspace's shape) cannot be packed in
    any strict order; the oracle seeds provisional entries for the cycle and
    rewrites them from the packaged artifacts, and every member still lands
    covered with the published forms resolving through faithful entries.
    """
    tree = _lockstep_tree(
        tmp_path,
        {
            "pubfix-db": "[dependencies]\npubfix-math.workspace = true\n\n[dev-dependencies]\npubfix-pools.workspace = true\n",
            "pubfix-math": "",
            "pubfix-pools": "[dependencies]\npubfix-uniswap.workspace = true\n",
            "pubfix-uniswap": "[dependencies]\npubfix-db.workspace = true\n",
        },
    )
    _census(tree, [])
    result = _oracle(tree, "--no-verify")
    assert result.returncode == 0, result.stdout + result.stderr
    for name in ("pubfix-db", "pubfix-math", "pubfix-pools", "pubfix-uniswap"):
        assert f"publish-verify covered {name}" in result.stdout


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
    """An allowlist naming another registry is not a crates.io publish shape.

    The overlay certifies the form a crates.io upload round would carry; a
    member publishing to (or through) any other registry would be silently
    verified against the wrong registry's expectations, so it fails loud.
    An allowlist naming crates.io alone stays covered.
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


def test_a_crates_io_publish_allowlist_stays_covered(tmp_path: Path) -> None:
    tree = _lockstep_tree(
        tmp_path,
        {"pubfix-leaf": ""},
        publish_tables={"pubfix-leaf": 'publish = ["crates-io"]'},
    )
    _census(tree, [])

    result = _oracle(tree, "--no-verify")

    assert result.returncode == 0, result.stdout + result.stderr
    assert "publish-verify covered pubfix-leaf" in result.stdout


def test_the_oracle_rejects_caller_registry_targeting(tmp_path: Path) -> None:
    """The overlay owns the registry targeting; the caller cannot retarget it.

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
    registry = _LoopbackRegistry(tmp_path / "served", fail_remaining=2, failure_status=503)
    try:
        raw = module.fetch_bytes(
            f"{registry.url}/pu/bf/pubfix-leaf", timeout=5.0, attempts=3, backoff=0.05
        )
    finally:
        registry.stop()
    assert raw == body
    assert registry.hits == ["/pu/bf/pubfix-leaf"] * 3


def test_the_index_fetch_fails_fast_on_a_definitive_answer(tmp_path: Path) -> None:
    """A 4xx says what the registry serves; retrying it would only stall."""
    module = _oracle_module()
    (tmp_path / "empty").mkdir()
    registry = _LoopbackRegistry(tmp_path / "empty")
    try:
        with pytest.raises(urllib.error.HTTPError):
            module.fetch_bytes(
                f"{registry.url}/pu/bf/pubfix-leaf", timeout=5.0, attempts=3, backoff=0.05
            )
    finally:
        registry.stop()
    assert len(registry.hits) == 1


def test_the_index_fetch_is_bounded_and_names_the_url(tmp_path: Path) -> None:
    """A dead endpoint fails inside its own budget, naming the URL and attempts."""
    module = _oracle_module()
    registry = _LoopbackRegistry(tmp_path / "hanging", hang=True)
    try:
        url = f"{registry.url}/pu/bf/pubfix-leaf"
        start = time.monotonic()
        with pytest.raises(RuntimeError) as raised:
            module.fetch_bytes(url, timeout=0.3, attempts=2, backoff=0.05)
        elapsed = time.monotonic() - start
    finally:
        registry.stop()
    message = str(raised.value)
    assert url in message
    assert "after 2 attempts" in message
    assert len(registry.hits) == 2
    assert elapsed < 10


def test_a_dead_registry_fails_the_mirror_with_a_useful_error(tmp_path: Path) -> None:
    module = _oracle_module()
    tree = _lockstep_tree(tmp_path, {"pubfix-leaf": ""})
    registry = _LoopbackRegistry(tmp_path / "hanging", hang=True)
    try:
        message = module.mirror_external(
            "pubfix-leaf",
            {"0.1.0"},
            "deadbeef",
            tree / "rust" / "target" / "publish-verify-mirror",
            tree / "rust" / "target" / "publish-verify-overlay" / "run-x",
            no_verify=True,
            index_base=registry.url,
            timeout=0.3,
            attempts=2,
            backoff=0.05,
        )
    finally:
        registry.stop()
    assert "pubfix-leaf" in message
    assert "after 2 attempts" in message
    assert "/pu/bf/pubfix-leaf" in message


def test_the_no_verify_mirror_skips_crate_downloads(tmp_path: Path) -> None:
    """``--no-verify`` needs index metadata only; the .crate round-trip is skipped.

    Pinned experimentally: a ``cargo publish/package --no-verify`` run over an
    index-only overlay resolves and succeeds without a single ``/dl`` request,
    while the verified form dies on the missing download.
    """
    module = _oracle_module()
    tree = _lockstep_tree(tmp_path, {"pubfix-leaf": ""})
    served = tmp_path / "served"
    entry = served / "pu" / "bf" / "pubfix-leaf"
    entry.parent.mkdir(parents=True)
    entry.write_text(json.dumps({"name": "pubfix-leaf", "vers": "0.1.0"}) + "\n", encoding="utf-8")
    registry = _LoopbackRegistry(served)
    try:
        message = module.mirror_external(
            "pubfix-leaf",
            {"0.1.0"},
            "deadbeef",
            tree / "rust" / "target" / "publish-verify-mirror",
            tree / "rust" / "target" / "publish-verify-overlay" / "run-x",
            no_verify=True,
            index_base=registry.url,
        )
        hits = list(registry.hits)
    finally:
        registry.stop()
    assert message == ""
    assert hits == ["/pu/bf/pubfix-leaf"]
    overlay = tree / "rust" / "target" / "publish-verify-overlay" / "run-x"
    assert (overlay / "index" / "pu" / "bf" / "pubfix-leaf").is_file()
    assert not (overlay / "dl").exists()


def test_the_verified_mirror_downloads_the_locked_crate(tmp_path: Path) -> None:
    """The verified form still mirrors the exact lock-pinned .crate bytes."""
    module = _oracle_module()
    tree = _lockstep_tree(tmp_path, {"pubfix-leaf": ""})
    served = tmp_path / "served"
    entry = served / "pu" / "bf" / "pubfix-leaf"
    entry.parent.mkdir(parents=True)
    version = "9.9.9"  # nothing local has ever staged this version
    entry.write_text(json.dumps({"name": "pubfix-leaf", "vers": version}) + "\n", encoding="utf-8")
    payload = b"the exact bytes crates.io would serve"
    crate = served / "dl" / "pubfix-leaf" / version / "download"
    crate.parent.mkdir(parents=True)
    crate.write_bytes(payload)
    registry = _LoopbackRegistry(served)
    try:
        message = module.mirror_external(
            "pubfix-leaf",
            {version},
            "deadbeef",
            tree / "rust" / "target" / "publish-verify-mirror",
            tree / "rust" / "target" / "publish-verify-overlay" / "run-x",
            no_verify=False,
            index_base=registry.url,
            dl_base=f"{registry.url}/dl",
        )
    finally:
        registry.stop()
    assert message == ""
    link = (
        tree
        / "rust"
        / "target"
        / "publish-verify-overlay"
        / "run-x"
        / "dl"
        / "pubfix-leaf"
        / version
        / "download"
    )
    assert link.is_symlink()
    assert link.read_bytes() == payload


def test_the_index_server_binds_distinct_ephemeral_ports(tmp_path: Path) -> None:
    """No fixed preferred port: each server binds an unbound port at start.

    A port chosen by the OS at bind time cannot be pre-bound by an attacker
    ahead of the run, and two concurrent runs cannot collide on it.
    """
    module = _oracle_module()
    overlays = []
    for i in range(2):
        overlay = tmp_path / f"overlay-{i}"
        (overlay / "index").mkdir(parents=True)
        (overlay / "index" / "config.json").write_text(f'{{"dl": "run-{i}"}}', encoding="utf-8")
        overlays.append(overlay)
    servers = [module.serve_index(overlay) for overlay in overlays]
    try:
        ports = [port for _, port in servers]
        assert len(set(ports)) == 2
        bodies = [
            urllib.request.urlopen(f"http://127.0.0.1:{port}/index/config.json", timeout=5).read()
            for port in ports
        ]
        assert bodies[0] != bodies[1]
    finally:
        for server, _ in servers:
            server.shutdown()
            server.server_close()


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
    for process, (out, err) in zip(processes, outputs):
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
    for process, (out, err) in zip(processes, outputs):
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
    spec = importlib.util.spec_from_file_location("publish_verify", _ORACLE)
    module = importlib.util.module_from_spec(spec)
    assert spec.loader is not None
    spec.loader.exec_module(module)
    return module


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


def test_a_renamed_dependency_keeps_the_index_name_and_package_fields() -> None:
    """A renamed dependency keeps both of the names Cargo's index carries.

    Cargo's documented index convention puts the manifest's alias in the
    dependency's `name` field and the original package name in its `package`
    field. An entry that loses either name breaks the rename: the real package
    name is what resolves on the registry, the alias is the name the member's
    code imports.
    """
    module = _oracle_module()
    manifest = (
        "[package]\n"
        'name = "pubfix-renamer"\n'
        'version = "0.1.0"\n\n'
        "[dependencies]\n"
        'alias = { package = "pubfix-real", version = "0.1.0" }\n\n'
        "[dev-dependencies]\n"
        'pubfix-plain = { version = "0.2.0" }\n'
    )
    entry = module.entry_from_packaged_manifest(manifest, "0" * 64)
    deps = {dep["name"]: dep for dep in entry["deps"]}
    assert set(deps) == {"alias", "pubfix-plain"}
    assert deps["alias"] == {
        "name": "alias",
        "req": "^0.1.0",
        "features": [],
        "optional": False,
        "default_features": True,
        "target": None,
        "kind": "normal",
        "package": "pubfix-real",
    }
    # a dependency that was not renamed carries no rename field
    assert "package" not in deps["pubfix-plain"]


def test_a_renamed_library_dependency_verifies_end_to_end(tmp_path: Path) -> None:
    """A member importing a library through an alias builds in the verified run.

    `renamed = { package = "pubfix-real", ... }` must resolve to a dependency
    linked under the alias the member's code imports -- both when the member
    is the publish round's own target (from its packaged manifest) and when a
    dependent member resolves it through the overlay index, where the entry's
    dependency list is the only rename carrier. An entry that loses the alias
    leaves the import unresolved exactly as `degenbot-evm2`'s
    `use evm2_macros::instruction` died while a dependent member was verified.
    The non-renamed control proves the run is otherwise sound.
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
    result = _oracle(tree)
    assert result.returncode == 0, result.stdout + result.stderr
    assert "aborting upload due to dry run" in result.stderr


def test_the_member_entry_matches_the_packed_crate_checksum(tmp_path: Path) -> None:
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
    packed = overlay / "dl" / "pubfix-top" / "0.1.0" / "download"
    assert packed.is_file()
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
    assert entry["cksum"] == hashlib.sha256(packed.read_bytes()).hexdigest()
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
