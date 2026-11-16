"""The crates.io live audit (``scripts/crates_io_pending.sh``).

Every case drives the real script against a throwaway workspace and a loopback
sparse index, so no assertion can be satisfied by a re-typed copy of the
script's logic, and the default suite never queries the real crates.io index.

The membership rule is the one the publish chain shares: a member is
publish-eligible exactly when its ``cargo metadata`` ``publish`` value is not
``[]`` (``scripts/publish_blocked.py`` applies the same rule). Concretely:

  * an unrestricted member (no ``publish`` key, or ``publish = true``, which
    cargo normalizes to ``null``) is audited;
  * a ``publish = false`` member stays absent -- cargo skips it at upload, so
    a fully-live audit that reported it would never terminate;
  * a ``publish = ["crates-io"]`` allowlist and an allowlist naming another
    registry both reach classification: the audit must never silently omit a
    member, and the verify oracle (``scripts/publish_verify.py``) fails loud
    on exactly these members -- real Cargo can drive a crates.io allowlist,
    but the loopback oracle cannot verify any allowlist today, so an
    allowlisted member must surface as a failure, not vanish.
"""

from __future__ import annotations

import http.server
import json
import os
import shlex
import shutil
import subprocess
import sys
import threading
from pathlib import Path

_REPO_ROOT = Path(__file__).resolve().parents[1]
_AUDIT = _REPO_ROOT / "scripts" / "crates_io_pending.sh"
_ORACLE = _REPO_ROOT / "scripts" / "publish_verify.py"


def _workspace(tmp_path: Path, members: dict[str, str], workspace_version: str = "0.1.0") -> Path:
    """A throwaway workspace: `members` maps crate name to manifest extras.

    The extras carry whatever the case needs after the ``[package]`` table --
    a ``publish`` line, nothing else. Every member is dependency-free, so
    ``cargo metadata`` resolves offline.
    """
    tree = tmp_path / "tree"
    rust = tree / "rust"
    rust.mkdir(parents=True)
    quoted = ", ".join(f'"crates/{name}"' for name in members)
    (rust / "Cargo.toml").write_text(
        f"""[workspace]
members = [{quoted}]
resolver = "2"

[workspace.package]
version = "{workspace_version}"
edition = "2021"
""",
        encoding="utf-8",
    )
    for name, extras in members.items():
        crate = rust / "crates" / name
        (crate / "src").mkdir(parents=True)
        (crate / "src" / "lib.rs").write_text("pub fn f() {}\n", encoding="utf-8")
        (crate / "Cargo.toml").write_text(
            f'[package]\nname = "{name}"\nversion.workspace = true\n'
            f"edition.workspace = true\n{extras}",
            encoding="utf-8",
        )
    return tree


def _census(tree: Path, rows: list[str]) -> None:
    path = tree / "scripts" / "publish-blocked-census.txt"
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text("".join(f"{row}\n" for row in rows), encoding="utf-8")


def _index_path(name: str) -> str:
    """The sparse-index layout, spelled the way the audit itself maps it."""
    lower = name.lower()
    length = len(lower)
    if length <= 2:
        return f"{length}/{lower}"
    if length == 3:
        return f"3/{lower[0]}/{lower}"
    return f"{lower[:2]}/{lower[2:4]}/{lower}"


class _SparseIndex:
    """A loopback sparse index over a dict of crate entries; counts every GET.

    Keyed by crate name; each value is the list of index entry dicts served at
    the crate's sparse path. ``config.json`` always answers 200, so the
    audit's reachability probe passes and per-member shapes are what vary.
    ``status_overrides`` forces a status code for a path (the fail-closed
    shapes); everything else 404s.
    """

    def __init__(self, entries: dict[str, list[dict[str, object]]] | None = None) -> None:
        outer = self
        self.entries = entries or {}
        self.status_overrides: dict[str, int] = {}
        self.hits: list[str] = []

        class Handler(http.server.BaseHTTPRequestHandler):
            def log_message(self, format_: str, *args: object) -> None:
                pass

            def do_GET(self) -> None:
                outer.hits.append(self.path)
                override = outer.status_overrides.get(self.path)
                if override is not None:
                    self.send_error(override)
                    return
                if self.path == "/config.json":
                    body = b'{"dl": "dl", "api": "api"}'
                else:
                    name = self.path.rsplit("/", 1)[-1]
                    lines = outer.entries.get(name)
                    if lines is None:
                        self.send_error(404)
                        return
                    body = "".join(json.dumps(entry) + "\n" for entry in lines).encode("utf-8")
                self.send_response(200)
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)

        self.server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self.server.daemon_threads = True
        threading.Thread(target=self.server.serve_forever, daemon=True).start()

    @property
    def url(self) -> str:
        return f"http://127.0.0.1:{self.server.server_address[1]}"

    def stop(self) -> None:
        self.server.shutdown()
        self.server.server_close()


def _audit(
    index_url: str,
    *args: str,
    manifest: Path | None = None,
    env_extra: dict[str, str] | None = None,
) -> subprocess.CompletedProcess[str]:
    command = [
        "bash",
        str(_AUDIT),
        "--index-url",
        index_url,
        "--manifest-path",
        str(manifest) if manifest else str(_REPO_ROOT / "rust" / "Cargo.toml"),
        *args,
    ]
    env = {**os.environ, **(env_extra or {})}
    return subprocess.run(
        command, check=False, capture_output=True, text=True, timeout=300, env=env
    )


def test_unrestricted_members_are_audited_and_disabled_members_stay_absent(
    tmp_path: Path,
) -> None:
    """The census's own membership rule: `publish != []`, not `publish is None`.

    An unrestricted member is classified; a `publish = false` member never
    appears in any mode's output -- cargo skips it at upload, so counting it
    live or pending would both be wrong.
    """
    tree = _workspace(
        tmp_path,
        {"pubfix-free": "", "pubfix-priv": "publish = false\n"},
    )
    index = _SparseIndex({"pubfix-free": [{"name": "pubfix-free", "vers": "0.1.0"}]})
    try:
        result = _audit(index.url, "--classify", manifest=tree / "rust" / "Cargo.toml")
    finally:
        index.stop()
    assert result.returncode == 0, result.stdout + result.stderr
    assert result.stdout.splitlines() == ["live pubfix-free"]
    assert "pubfix-priv" not in result.stdout


def test_a_crates_io_allowlisted_member_reaches_classification(tmp_path: Path) -> None:
    """`publish = ["crates-io"]` is cargo-publishable here, so it is audited.

    Under the old `publish is None` filter this member dropped out of the
    audit silently while the census still counted it publishable; the audit
    must see everyone cargo would upload.
    """
    tree = _workspace(
        tmp_path,
        {"pubfix-io": 'publish = ["crates-io"]\n'},
    )
    index = _SparseIndex()
    try:
        result = _audit(index.url, "--classify", manifest=tree / "rust" / "Cargo.toml")
    finally:
        index.stop()
    assert result.returncode == 0, result.stdout + result.stderr
    assert result.stdout.splitlines() == ["pending pubfix-io"]


def test_a_foreign_allowlisted_member_reaches_classification(tmp_path: Path) -> None:
    """An allowlist naming another registry still reaches classification.

    Cargo will never upload such a member to crates.io, but the audit may not
    decide that on its own: it classifies, and the verify oracle turns the
    member into a loud failure (pinned next). Silent omission is the one
    forbidden outcome.
    """
    tree = _workspace(
        tmp_path,
        {"pubfix-foreign": 'publish = ["private-reg"]\n'},
    )
    index = _SparseIndex()
    try:
        result = _audit(index.url, "--classify", manifest=tree / "rust" / "Cargo.toml")
    finally:
        index.stop()
    assert result.returncode == 0, result.stdout + result.stderr
    assert result.stdout.splitlines() == ["pending pubfix-foreign"]


def test_an_allowlisted_member_cannot_bypass_the_verify_oracle(tmp_path: Path) -> None:
    """An allowlisted member the audit classifies dies in the oracle, loud.

    The audit now hands allowlisted members to the release pipeline; the
    oracle's existing fail-loud path is what stops them: an allowlist
    (crates.io or otherwise) leaves no registry the loopback run may drive,
    so the run fails naming member and registry instead of covering it.
    """
    tree = _workspace(
        tmp_path,
        {
            "pubfix-io": 'publish = ["crates-io"]\n',
            "pubfix-foreign": 'publish = ["private-reg"]\n',
        },
    )
    _census(tree, [])
    result = subprocess.run(
        [sys.executable, str(_ORACLE), "--root", str(tree), "--no-verify"],
        check=False,
        capture_output=True,
        text=True,
        timeout=600,
    )
    assert result.returncode != 0
    output = result.stdout + result.stderr
    # the refusal names the offending member and the registry its allowlist
    # permits -- whichever allowlisted member the run reaches first -- and no
    # member is covered past it
    refusals = [line for line in output.splitlines() if "publishes only to" in line]
    assert refusals, output
    assert any(name in refusals[0] for name in ("pubfix-io", "pubfix-foreign")), refusals
    assert any(registry in refusals[0] for registry in ("crates-io", "private-reg")), refusals
    assert "publish-verify covered" not in output


def test_pending_live_and_classify_views_are_preserved(tmp_path: Path) -> None:
    """The three output modes and their exit code are unchanged.

    One member live at the workspace version, one pending: default mode
    prints the pending member, ``--live`` the live one, ``--classify`` one
    ``"<state> <member>"`` line per member, all exiting 0.
    """
    tree = _workspace(
        tmp_path,
        {"pubfix-live": "", "pubfix-new": ""},
    )
    index = _SparseIndex({"pubfix-live": [{"name": "pubfix-live", "vers": "0.1.0"}]})
    manifest = tree / "rust" / "Cargo.toml"
    try:
        pending = _audit(index.url, manifest=manifest)
        live = _audit(index.url, "--live", manifest=manifest)
        classify = _audit(index.url, "--classify", manifest=manifest)
    finally:
        index.stop()
    for result in (pending, live, classify):
        assert result.returncode == 0, result.stdout + result.stderr
    assert pending.stdout.splitlines() == ["pubfix-new"]
    assert live.stdout.splitlines() == ["pubfix-live"]
    assert sorted(classify.stdout.splitlines()) == [
        "live pubfix-live",
        "pending pubfix-new",
    ]


def test_the_version_match_is_exact_across_prereleases(tmp_path: Path) -> None:
    """A prerelease entry sharing the release prefix must not read as live.

    A bare substring test would accept ``0.1.0-alpha.1`` as ``0.1.0``, or
    read a workspace at ``0.1.0-alpha.15`` as live from an index holding
    only ``0.1.0-alpha.1``; the audit's quoted, dot-escaped match keeps
    the member pending.
    """
    tree = _workspace(tmp_path, {"pubfix-pre": ""}, workspace_version="0.1.0-alpha.15")
    index = _SparseIndex({"pubfix-pre": [{"name": "pubfix-pre", "vers": "0.1.0-alpha.1"}]})
    try:
        result = _audit(index.url, manifest=tree / "rust" / "Cargo.toml")
    finally:
        index.stop()
    assert result.returncode == 0, result.stdout + result.stderr
    assert result.stdout.splitlines() == ["pubfix-pre"]


def test_an_unreachable_index_fails_closed(tmp_path: Path) -> None:
    """No index, no answer: exit 2, the state-unknown message, nothing on stdout.

    Callers must treat an unreadable index as unknown state, never as
    "nothing is live".
    """
    tree = _workspace(tmp_path, {"pubfix-free": ""})
    import socket

    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        dead_port = sock.getsockname()[1]
    result = _audit(f"http://127.0.0.1:{dead_port}", manifest=tree / "rust" / "Cargo.toml")
    assert result.returncode == 2
    assert result.stdout == ""
    assert "index unreachable" in result.stderr
    assert "state unknown" in result.stderr


def test_an_unreadable_member_fails_closed_and_names_it(tmp_path: Path) -> None:
    """A per-member read failure fails the whole audit with exit 2.

    One member answering 500 means the audit cannot claim every member's
    state was read conclusively: the member is named on stderr and the
    run exits 2, so no caller can read the output as green. The
    reachable member's own answer still prints -- pubfix-ok is pending
    at the workspace version -- and the pinned stdout holds exactly
    that one line beside the failing exit code.
    """
    tree = _workspace(tmp_path, {"pubfix-ok": "", "pubfix-broken": ""})
    index = _SparseIndex({"pubfix-ok": [{"name": "pubfix-ok", "vers": "0.2.0"}]})
    index.status_overrides[f"/{_index_path('pubfix-broken')}"] = 500
    try:
        result = _audit(index.url, manifest=tree / "rust" / "Cargo.toml")
    finally:
        index.stop()
    assert result.returncode == 2
    assert result.stdout == "pubfix-ok\n"
    assert "pubfix-broken" in result.stderr
    assert "state unknown" in result.stderr


def test_a_usage_error_exits_64(tmp_path: Path) -> None:
    tree = _workspace(tmp_path, {"pubfix-free": ""})
    result = _audit("http://127.0.0.1:1", "--bogus", manifest=tree / "rust" / "Cargo.toml")
    assert result.returncode == 64
    assert "unknown argument" in result.stderr


def test_a_workspace_with_no_publishable_member_fails_the_membership_read(
    tmp_path: Path,
) -> None:
    """Empty membership is a loud exit 1, not a vacuous green audit."""
    tree = _workspace(tmp_path, {"pubfix-priv": "publish = false\n"})
    index = _SparseIndex()
    try:
        result = _audit(index.url, manifest=tree / "rust" / "Cargo.toml")
    finally:
        index.stop()
    assert result.returncode == 1
    assert "could not read publishable members" in result.stderr


def test_the_metadata_read_is_resolved_locked(tmp_path: Path) -> None:
    """The audit's `cargo metadata` invocation carries `--locked`.

    The membership read is the audit's trust anchor: if cargo ever starts
    consulting (or refreshing) the lockfile during metadata, a drifted tree
    must fail the read, not answer from a resolution the release would never
    see. A recording cargo shim captures the argv the script actually passes.
    """
    tree = _workspace(tmp_path, {"pubfix-free": ""})
    real_cargo = shutil.which("cargo")
    assert real_cargo, "the audit tests need a real cargo to delegate to"
    shim_dir = tmp_path / "cargo-shim"
    shim_dir.mkdir()
    log = shim_dir / "invocations.log"
    shim = shim_dir / "cargo"
    shim.write_text(
        "#!/usr/bin/env bash\n"
        f'printf "%s\\n" "$*" >> {shlex.quote(str(log))}\n'
        f'exec {shlex.quote(real_cargo)} "$@"\n',
        encoding="utf-8",
    )
    shim.chmod(0o755)
    env_extra = {"PATH": f"{shim_dir}{os.pathsep}{os.environ['PATH']}"}
    index = _SparseIndex()
    try:
        result = _audit(index.url, manifest=tree / "rust" / "Cargo.toml", env_extra=env_extra)
    finally:
        index.stop()
    assert result.returncode == 0, result.stdout + result.stderr
    invocations = log.read_text(encoding="utf-8").splitlines()
    metadata_calls = [line for line in invocations if line.startswith("metadata ")]
    assert len(metadata_calls) == 1, invocations
    call = metadata_calls[0]
    assert "--locked" in call
    assert "--no-deps" in call
    assert "--format-version 1" in call
